//! Channel polling and scan/layout message handling.
//!
//! The main event loop calls `poll_channels()` each frame to drain messages
//! from the scanner, layout, and watcher threads. Handles scan results,
//! layout completion, file changes, and dead-thread recovery.

use super::channels::{LayoutMsg, LayoutRequest, ScanCommand, ScanMsg};
use super::scan_threads::{format_panic, layout_thread, scanner_thread};
use super::watcher::WatcherHandle;
use crate::layout::spatial_index::SpatialIndex;
use crate::layout::types::SizeMode;
use crossbeam_channel::bounded;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::SentruxApp;

impl SentruxApp {
    /// Poll all channels (non-blocking) and update state.
    pub(crate) fn poll_channels(&mut self, ctx: &egui::Context) {
        self.poll_scan_messages(ctx);
        self.poll_layout_messages(ctx);
        self.poll_watcher_messages(ctx);
        self.flush_pending_changes();
        self.poll_watcher_setup(ctx);
        self.poll_folder_picker();
        self.retry_layout_if_needed();
        self.poll_dead_scanner(ctx);
        self.poll_dead_layout(ctx);
        self.tick_heat_and_animation(ctx);
    }

    fn poll_scan_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.scan_rx.try_recv() {
            match msg {
                ScanMsg::Progress(p) => {
                    self.state.scan_step = p.step;
                    self.state.scan_pct = p.pct;
                    ctx.request_repaint();
                }
                ScanMsg::TreeReady(snap, gen) => {
                    self.handle_tree_ready(snap, gen, ctx);
                }
                ScanMsg::Complete(snap, gen, reports) => {
                    self.handle_scan_complete(snap, gen, *reports, ctx);
                }
                ScanMsg::Error(e, gen) => {
                    if gen == self.scan_generation {
                        self.state.scanning = false;
                        self.state.scan_pct = 0;
                        self.state.scan_step = format!("Error: {}", e);
                        // Reset last_scanned_root so the user can retry the same directory
                        self.last_scanned_root = None;
                        ctx.request_repaint();
                    }
                }
            }
        }
    }

    /// Process TreeReady message: early snapshot with file tree but no graphs yet.
    /// Enables rendering file blocks before graph computation finishes.
    fn handle_tree_ready(&mut self, snap: Arc<crate::core::snapshot::Snapshot>, gen: u64, ctx: &egui::Context) {
        if gen == self.scan_generation {
            self.state.snapshot = Some(snap);
            self.state.rebuild_file_index();
            self.request_layout();
            ctx.request_repaint();
        }
    }

    /// Apply a completed scan's reports and snapshot to app state.
    fn apply_scan_reports(
        &mut self,
        snap: Arc<crate::core::snapshot::Snapshot>,
        reports: crate::app::channels::ScanReports,
        ctx: &egui::Context,
    ) {
        let report = reports.health.unwrap_or_else(|| crate::metrics::compute_health(&snap));
        let arch = reports.arch.unwrap_or_else(|| crate::metrics::arch::compute_arch(&snap));
        self.check_arch_degradation(&arch);
        // Record scan for telemetry
        crate::app::update_check::record_scan(snap.total_files, report.grade);
        self.state.health_report = Some(report);
        self.state.arch_report = Some(arch);
        self.state.evolution_report = reports.evolution;
        self.state.test_gap_report = reports.test_gaps;
        self.state.rule_check_result = reports.rules;
        self.state.snapshot = Some(snap);
        self.state.scanning = false;
        self.state.rebuild_file_index();
        self.request_layout();
        if self.watcher_handle.is_none() || self.state.root_path != self.last_scanned_root {
            self.start_watcher();
        }
        ctx.request_repaint();
    }

    /// Process Complete message: full snapshot with all analysis reports.
    /// Updates all state, starts watcher if needed, and requests re-layout.
    fn handle_scan_complete(
        &mut self,
        snap: Arc<crate::core::snapshot::Snapshot>,
        gen: u64,
        reports: crate::app::channels::ScanReports,
        ctx: &egui::Context,
    ) {
        if gen == self.scan_generation {
            self.apply_scan_reports(snap, reports, ctx);
        } else if gen > 0 && self.state.scanning {
            // Stale generation result: clear scanning flag if no fresh scan is
            // in flight (the scanner thread is idle or finished).  Previously
            // this only cleared when root_path diverged from last_scanned_root,
            // leaving the flag stuck when the roots matched but a superseded
            // generation completed.
            let scanner_idle = self.scanner_handle.as_ref().is_none_or(|h| h.is_finished());
            if self.state.root_path != self.last_scanned_root || scanner_idle {
                self.state.scanning = false;
                ctx.request_repaint();
            }
        }
    }

    /// Log specific architecture regressions between previous and current reports.
    fn log_arch_regressions(prev: &crate::metrics::arch::ArchReport, current: &crate::metrics::arch::ArchReport) {
        if current.upward_violations.len() > prev.upward_violations.len() {
            eprintln!(
                "[arch-diff] Upward violations increased: {} -> {}",
                prev.upward_violations.len(), current.upward_violations.len()
            );
        }
        if current.max_blast_radius > prev.max_blast_radius + 2 {
            eprintln!(
                "[arch-diff] Max blast radius increased: {} -> {} ({})",
                prev.max_blast_radius, current.max_blast_radius, current.max_blast_file
            );
        }
    }

    /// Compare current arch report against previous one and log regressions.
    fn check_arch_degradation(&mut self, arch: &crate::metrics::arch::ArchReport) {
        let prev_arch = match &self.state.arch_report {
            Some(p) => p,
            None => return,
        };
        Self::log_arch_regressions(prev_arch, arch);
        if arch.arch_grade > prev_arch.arch_grade {
            self.state.record_activity(
                format!("Architecture degraded: {} -> {}", prev_arch.arch_grade, arch.arch_grade),
                "arch_degraded".to_string(),
            );
        }
    }

    /// Process a single layout-ready message: build spatial index, fit viewport, update state.
    fn apply_layout_ready(&mut self, rd: crate::layout::types::RenderData, version: u64, ctx: &egui::Context) {
        let si = SpatialIndex::build(&rd.rects, rd.content_width, rd.content_height);
        if self.state.rendered_version == 0 {
            self.state.viewport.fit_content(
                rd.content_width,
                rd.content_height,
                self.state.settings.fit_content_padding,
            );
        }
        self.state.render_data = Some(rd);
        self.state.spatial_index = Some(si);
        self.state.rendered_version = version;
        self.state.layout_pending = version < self.state.layout_version;
        ctx.request_repaint();
    }

    fn poll_layout_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.layout_rx.try_recv() {
            match msg {
                LayoutMsg::Ready(rd, version) => {
                    if version < self.state.layout_version {
                        continue;
                    }
                    if self.state.snapshot.is_none() {
                        self.state.layout_pending = true;
                        continue;
                    }
                    self.apply_layout_ready(rd, version, ctx);
                }
            }
        }
    }

    fn poll_watcher_messages(&mut self, ctx: &egui::Context) {
        let mut had_watch_events = false;
        while let Ok(fe) = self.watch_rx.try_recv() {
            self.state.heat.record_change(&fe.path, &self.state.settings.heat_config());
            if !fe.is_dir {
                self.state.record_activity(fe.path.clone(), fe.kind.clone());
            }
            self.state.pending_changes.insert(fe.path);
            // Reset debounce timer on every event so rapid changes (e.g. a build)
            // settle before triggering a rescan, instead of firing 500ms after
            // the first event while the build is still running.
            self.state.pending_since = Some(Instant::now());
            had_watch_events = true;
        }
        if had_watch_events {
            ctx.request_repaint();
        }
    }

    fn flush_pending_changes(&mut self) {
        let since = match self.state.pending_since {
            Some(s) => s,
            None => return,
        };
        let debounce_elapsed = since.elapsed() > Duration::from_millis(self.state.settings.file_change_debounce_ms);
        if !debounce_elapsed || self.state.scanning {
            return;
        }
        if self.state.root_path.is_none() || self.state.snapshot.is_none() {
            // Don't re-arm the timer — leave pending_since as-is so changes
            // are flushed as soon as a snapshot becomes available. [H10 fix]
            return;
        }
        let changed: Vec<String> = std::mem::take(&mut self.state.pending_changes).into_iter().collect();
        self.state.pending_since = None;
        if !changed.is_empty() {
            self.start_rescan(changed);
        }
    }

    fn poll_watcher_setup(&mut self, ctx: &egui::Context) {
        let rx = match &self.watcher_setup_rx {
            Some(rx) => rx,
            None => return,
        };
        let maybe_handle = match rx.try_recv() {
            Ok(h) => h,
            Err(_) => return,
        };
        match maybe_handle {
            Some(handle) => { self.watcher_handle = Some(handle); }
            None => {
                self.state.scan_step = "Warning: file watcher failed — live updates disabled".into();
                ctx.request_repaint();
            }
        }
        self.watcher_setup_rx = None;
    }

    fn poll_folder_picker(&mut self) {
        let rx = match &self.folder_picker_rx {
            Some(rx) => rx,
            None => return,
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(_) => return,
        };
        if let Some(path) = result {
            self.state.root_path = Some(path);
        }
        self.folder_picker_rx = None;
    }

    fn retry_layout_if_needed(&mut self) {
        let needs_layout = !self.state.scanning && self.state.snapshot.is_some() && (
            self.state.layout_request_dropped
            || self.state.render_data.is_none()
        );
        if needs_layout
            && self.state.layout_retry_at.is_none_or(|t| t.elapsed() >= Duration::from_millis(50)) {
                self.request_layout();
                self.state.layout_retry_at = Some(Instant::now());
            }
    }

    fn poll_dead_scanner(&mut self, ctx: &egui::Context) {
        let is_finished = self.scanner_handle.as_ref().is_some_and(|h| h.is_finished());
        if !is_finished {
            return;
        }
        let handle = self.scanner_handle.take().unwrap();
        match handle.join() {
            Ok(()) => {
                self.state.scan_step = "Error: scanner thread exited unexpectedly".into();
            }
            Err(panic_payload) => {
                let msg = format_panic("scanner", &panic_payload);
                eprintln!("[app] {}", msg);
                self.state.scan_step = msg;
            }
        }
        self.state.scanning = false;
        self.respawn_scanner_thread();
        self.last_scanned_root = None;
        ctx.request_repaint();
    }

    /// Handle a dead layout thread: check if it panicked and recover.
    fn handle_dead_layout_result(&mut self, handle: std::thread::JoinHandle<()>, ctx: &egui::Context) {
        let was_panic = match handle.join() {
            Err(panic_payload) => {
                let msg = format_panic("layout", &panic_payload);
                eprintln!("[app] {}", msg);
                self.state.scan_step = msg;
                ctx.request_repaint();
                true
            }
            Ok(()) => false,
        };
        // Always respawn on thread death — both panic and normal exit
        // (normal exit means channel disconnected, layout is permanently dead)
        self.respawn_layout_thread();
        if self.state.snapshot.is_some() {
            self.request_layout();
        }
        if !was_panic {
            eprintln!("[app] layout thread exited normally (channel disconnected) — respawned");
        }
    }

    fn poll_dead_layout(&mut self, ctx: &egui::Context) {
        let is_finished = self.layout_handle.as_ref().is_some_and(|h| h.is_finished());
        if !is_finished {
            return;
        }
        let handle = self.layout_handle.take().unwrap();
        self.handle_dead_layout_result(handle, ctx);
    }

    /// Get current epoch seconds, logging a warning on system clock errors.
    fn current_epoch_secs(&mut self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_else(|e| {
                eprintln!("[app] system clock before epoch: {}", e);
                if self.state.scan_step.is_empty() || !self.state.scan_step.starts_with("Warning:") {
                    self.state.scan_step = "Warning: system clock error — Age/Heat colors unreliable".into();
                }
                std::time::Duration::ZERO
            })
            .as_secs_f64()
    }

    fn tick_heat_and_animation(&mut self, ctx: &egui::Context) {
        self.state.heat.tick(&self.state.settings.heat_config());
        self.state.anim_time = self.state.anim_start.elapsed().as_secs_f64();
        self.state.frame_instant = Instant::now();
        self.state.frame_now_secs = self.current_epoch_secs();
        let has_visible_edges = self.state.selected_path.is_some()
            || self.state.hovered_path.is_some()
            || self.state.show_all_edges;
        let needs_anim = self.state.heat.is_active() || has_visible_edges;
        if needs_anim {
            ctx.request_repaint_after(Duration::from_millis(self.state.settings.heat_repaint_ms));
        }
    }

    /// Snapshot heat values for the layout thread when Heat size mode is active.
    fn snapshot_heat_map(&self) -> Option<std::collections::HashMap<String, f64>> {
        if self.state.size_mode == SizeMode::Heat {
            Some(self.state.heat.hot_files(0.0, self.state.frame_instant, self.state.settings.heat_half_life)
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect())
        } else {
            None
        }
    }

    /// Build a layout request from current state.
    fn build_layout_request(&self, snap: &Arc<crate::core::snapshot::Snapshot>, version: u64) -> LayoutRequest {
        LayoutRequest {
            snapshot: Arc::clone(snap),
            size_mode: self.state.size_mode,
            scale_mode: self.state.scale_mode,
            layout_mode: self.state.layout_mode,
            viewport_w: self.state.viewport.canvas_w,
            viewport_h: self.state.viewport.canvas_h,
            drill_path: self.state.drill_stack.last().cloned(),
            version,
            heat_map: self.snapshot_heat_map(),
            settings: self.state.settings.clone(),
            focus_mode: self.state.focus_mode.clone(),
            entry_point_files: Arc::clone(&self.state.entry_point_files),
            hidden_paths: Arc::clone(&self.state.hidden_paths),
            impact_files: self.state.impact_files.clone(),
        }
    }

    /// Send a layout request to the background layout thread.
    pub(crate) fn request_layout(&mut self) {
        let snap = match &self.state.snapshot {
            Some(s) => s.clone(),
            None => return,
        };
        let next_version = self.state.layout_version + 1;
        let req = self.build_layout_request(&snap, next_version);
        match self.layout_tx.try_send(req) {
            Ok(()) => {
                self.state.layout_version = next_version;
                self.state.layout_pending = true;
                self.state.layout_request_dropped = false;
            }
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                self.state.layout_pending = true;
                self.state.layout_request_dropped = true;
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                eprintln!("[app] layout channel disconnected — respawning layout thread");
                self.state.scan_step = "Warning: layout thread restarted".into();
                self.respawn_layout_thread();
                self.state.layout_pending = true;
                self.state.layout_request_dropped = true;
            }
        }
    }

    /// Respawn the layout background thread with fresh channels.
    /// Joins the old thread on a detached background thread to avoid blocking the UI.
    fn respawn_layout_thread(&mut self) {
        if let Some(old_handle) = self.layout_handle.take() {
            drop(std::mem::replace(&mut self.layout_tx, bounded::<LayoutRequest>(0).0));
            // Join on a background thread to avoid blocking the UI event loop
            std::thread::Builder::new()
                .name("layout-join".into())
                .spawn(move || { let _ = old_handle.join(); })
                .unwrap_or_else(|_| {
                    // If we can't spawn, just detach (the old thread will exit on its own)
                    std::thread::spawn(|| {})
                });
        }
        let (layout_req_tx, layout_req_rx) = bounded::<LayoutRequest>(2);
        let (layout_msg_tx, layout_msg_rx) = bounded::<LayoutMsg>(2);
        match std::thread::Builder::new()
            .name("layout".into())
            .spawn(move || {
                layout_thread(layout_req_rx, layout_msg_tx);
            })
        {
            Ok(handle) => {
                self.layout_tx = layout_req_tx;
                self.layout_rx = layout_msg_rx;
                self.layout_handle = Some(handle);
                self.state.layout_pending = false;
                self.state.layout_request_dropped = false;
            }
            Err(e) => {
                eprintln!("[app] CRITICAL: failed to respawn layout thread: {}", e);
                self.state.scan_step = format!("Error: layout thread spawn failed: {}", e);
                // Keep old channels — layout is dead but app won't panic
            }
        }
    }

    /// Respawn the scanner background thread with fresh channels.
    /// Joins the old thread on a detached background thread to avoid blocking the UI.
    fn respawn_scanner_thread(&mut self) {
        if let Some(old_handle) = self.scanner_handle.take() {
            drop(std::mem::replace(&mut self.scan_tx, bounded::<ScanCommand>(0).0));
            // Join on a background thread to avoid blocking the UI event loop
            std::thread::Builder::new()
                .name("scanner-join".into())
                .spawn(move || { let _ = old_handle.join(); })
                .unwrap_or_else(|_| {
                    std::thread::spawn(|| {})
                });
        }
        let (scan_cmd_tx, scan_cmd_rx) = bounded::<ScanCommand>(1);
        let (scan_msg_tx, scan_msg_rx) = bounded::<ScanMsg>(64);
        match std::thread::Builder::new()
            .name("scanner".into())
            .spawn(move || {
                scanner_thread(scan_cmd_rx, scan_msg_tx);
            })
        {
            Ok(handle) => {
                self.scan_tx = scan_cmd_tx;
                self.scan_rx = scan_msg_rx;
                self.scanner_handle = Some(handle);
                eprintln!("[app] scanner thread respawned");
            }
            Err(e) => {
                eprintln!("[app] CRITICAL: failed to respawn scanner thread: {}", e);
                self.state.scan_step = format!("Error: scanner thread spawn failed: {}", e);
                // Keep old channels — scanner is dead but app won't panic
            }
        }
    }

    /// Clear all stale state from the previous scan/directory after a new scan starts.
    fn clear_stale_state(&mut self) {
        // Drop old watcher before scanning new directory [ref:b9f45231]
        self.watcher_handle = None;
        self.watcher_setup_rx = None;

        // Clear ALL stale state from previous scan/directory
        self.state.render_data = None;
        self.state.spatial_index = None;
        self.state.snapshot = None;
        self.state.health_report = None;
        self.state.arch_report = None;
        self.state.evolution_report = None;
        self.state.test_gap_report = None;
        self.state.rule_check_result = None;
        self.state.whatif_cache = None;
        self.state.impact_files = None;
        self.state.dsm_cache = None;
        self.state.top_connections_cache = None;
        self.state.drill_stack.clear();
        self.state.selected_path = None;
        self.state.hovered_path = None;
        self.state.file_index.clear();

        // Reset scan progress state
        self.state.scanning = true;
        self.state.scan_step = "Starting scan...".into();
        self.state.scan_pct = 0;
        self.state.rendered_version = 0;
        self.state.pending_changes.clear();
        self.state.pending_since = None;

        // Drain stale watcher events from the old directory
        while self.watch_rx.try_recv().is_ok() {}

        // Clear heat state — old project's heat data causes stale repaints
        self.state.heat = crate::core::heat::HeatTracker::new();
    }

    /// Try to start a full scan of the current root_path.
    /// Returns `true` if the scan command was successfully sent.
    pub(crate) fn start_scan(&mut self) -> bool {
        let root = match &self.state.root_path {
            Some(r) => r.clone(),
            None => return false,
        };
        let next_gen = self.scan_generation + 1;
        match self.scan_tx.try_send(ScanCommand::FullScan {
            root,
            limits: crate::app::scan_threads::ScanLimits {
                max_file_size_kb: self.state.settings.max_file_size_kb,
                max_parse_size_kb: self.state.settings.max_parse_size_kb,
                max_call_targets: self.state.settings.max_call_targets,
            },
            gen: next_gen,
        }) {
            Ok(()) => {
                self.scan_generation = next_gen;
                self.clear_stale_state();
                true
            }
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                self.state.scan_step = "Scanner busy, retrying...".into();
                false
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                self.state.scan_step = "Error: scanner thread crashed".into();
                false
            }
        }
    }

    /// Re-queue changed paths for a later debounce cycle (scanner was busy).
    fn requeue_changes(&mut self, changed: Vec<String>) {
        for p in changed {
            self.state.pending_changes.insert(p);
        }
        if self.state.pending_since.is_none() {
            self.state.pending_since = Some(Instant::now());
        }
    }

    /// Start an incremental rescan for the given changed file paths.
    pub(crate) fn start_rescan(&mut self, changed: Vec<String>) {
        let (root, snap) = match (&self.state.root_path, &self.state.snapshot) {
            (Some(r), Some(s)) => (r.clone(), Arc::clone(s)),
            _ => return,
        };
        let next_gen = self.scan_generation + 1;
        let changed_count = changed.len();
        match self.scan_tx.try_send(ScanCommand::Rescan {
            root,
            changed: changed.clone(),
            old_snap: snap,
            limits: crate::app::scan_threads::ScanLimits {
                max_file_size_kb: self.state.settings.max_file_size_kb,
                max_parse_size_kb: self.state.settings.max_parse_size_kb,
                max_call_targets: self.state.settings.max_call_targets,
            },
            gen: next_gen,
        }) {
            Ok(()) => {
                self.scan_generation = next_gen;
                self.state.scanning = true;
                self.state.scan_pct = 0;
                self.state.scan_step = format!("Updating {} files...", changed_count);
            }
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                self.requeue_changes(changed);
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                self.state.scan_step = "Error: scanner thread crashed".into();
            }
        }
    }

    /// Start file watcher on current root. [ref:b9f45231]
    pub(crate) fn start_watcher(&mut self) {
        // Drop any existing watcher first
        self.watcher_handle = None;
        // Drain stale events from old watcher to prevent unnecessary rescans
        while self.watch_rx.try_recv().is_ok() {}
        if let Some(root) = &self.state.root_path {
            let root_owned = root.clone();
            let tx = self.watch_tx.clone();
            let debounce_ms = self.state.settings.watcher_debounce_ms;
            let (handle_tx, handle_rx) = crossbeam_channel::bounded::<Option<WatcherHandle>>(1);
            if let Err(e) = std::thread::Builder::new()
                .name("watcher-setup".into())
                .spawn(move || {
                    match crate::app::watcher::start_watcher(&root_owned, tx, debounce_ms) {
                        Ok(handle) => { let _ = handle_tx.send(Some(handle)); }
                        Err(e) => {
                            eprintln!("Failed to start watcher: {}", e);
                            let _ = handle_tx.send(None);
                        }
                    }
                })
            {
                eprintln!("Failed to spawn watcher setup thread: {}", e);
                return;
            }
            self.watcher_setup_rx = Some(handle_rx);
        }
    }
}
