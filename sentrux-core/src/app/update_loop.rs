//! Application initialization and eframe update loop.
//!
//! Creates the `SentruxApp`, spawns scanner and layout threads, sets up
//! channels, and implements `eframe::App::update()` to orchestrate the
//! per-frame rendering pipeline.

use super::channels::{LayoutMsg, LayoutRequest, ScanCommand, ScanMsg};
use crate::renderer;
use super::state::AppState;
use crate::core::snapshot::FileEvent;
use crossbeam_channel::bounded;
use std::time::{Duration, Instant};

use super::SentruxApp;
use super::draw_panels;

impl SentruxApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_path: Option<String>) -> Self {
        let ctx = &cc.egui_ctx;

        let mut state = AppState::new();
        if let Some(storage) = cc.storage {
            let prefs = crate::app::prefs::UserPrefs::load(storage);
            prefs.apply_to(&mut state);
        }
        // CLI path argument overrides saved preference
        if let Some(path) = initial_path {
            state.root_path = Some(path);
        }

        // Capture native ppp before any scaling so we can recompute correctly when
        // the ui_scale slider changes at runtime.
        let native_ppp = ctx.pixels_per_point();

        // Load fonts after prefs so load_cjk_fonts setting is respected.
        // Also check SENTRUX_NO_CJK env var as an override.
        let load_cjk = state.settings.load_cjk_fonts
            && std::env::var("SENTRUX_NO_CJK").is_err();
        setup_fonts(ctx, load_cjk);
        setup_style(ctx, native_ppp, state.settings.ui_scale);

        let (scan_cmd_tx, scan_cmd_rx) = bounded::<ScanCommand>(1);
        let (scan_msg_tx, scan_msg_rx) = bounded::<ScanMsg>(64);
        let (layout_req_tx, layout_req_rx) = bounded::<LayoutRequest>(2);
        let (layout_msg_tx, layout_msg_rx) = bounded::<LayoutMsg>(2);
        let (watch_tx, watch_rx) = bounded::<FileEvent>(256);

        log_failed_languages();

        let scanner_handle = spawn_scanner_thread(scan_cmd_rx, scan_msg_tx.clone());
        let layout_handle = spawn_layout_thread(layout_req_rx, layout_msg_tx);

        Self {
            state,
            scan_tx: scan_cmd_tx,
            scan_rx: scan_msg_rx,
            layout_tx: layout_req_tx,
            layout_rx: layout_msg_rx,
            watch_rx,
            watch_tx,
            last_scanned_root: None,
            watcher_handle: None,
            scan_generation: 0,
            scan_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            watcher_setup_rx: None,
            scanner_handle: Some(scanner_handle),
            layout_handle: Some(layout_handle),
            folder_picker_rx: None,
            native_ppp,
        }
    }
}

/// Surface failed plugin loads at startup.
fn log_failed_languages() {
    let failed = crate::analysis::lang_registry::failed_plugins();
    for err in failed {
        crate::debug_log!("[app] WARNING: plugin failed: {}", err);
    }
}

/// Spawn the scanner worker thread. [ref:13696c9c]
fn spawn_scanner_thread(
    cmd_rx: crossbeam_channel::Receiver<ScanCommand>,
    msg_tx: crossbeam_channel::Sender<ScanMsg>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("scanner".into())
        .spawn(move || {
            super::scan_threads::scanner_thread(cmd_rx, msg_tx);
        })
        .expect("failed to spawn scanner thread")
}

/// Spawn the layout worker thread.
fn spawn_layout_thread(
    req_rx: crossbeam_channel::Receiver<LayoutRequest>,
    msg_tx: crossbeam_channel::Sender<LayoutMsg>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("layout".into())
        .spawn(move || {
            super::scan_threads::layout_thread(req_rx, msg_tx);
        })
        .expect("failed to spawn layout thread")
}

impl eframe::App for SentruxApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        crate::app::prefs::UserPrefs::from_state(&self.state).save(storage);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Re-apply ui_scale every frame so the settings slider takes effect immediately.
        ctx.set_pixels_per_point(self.native_ppp * self.state.settings.ui_scale);

        // Global keyboard shortcuts
        ctx.input(|i| {
            // Cmd+R: rescan
            if i.modifiers.command && i.key_pressed(egui::Key::R) {
                self.state.rescan_requested = true;
            }
            // Escape: clear selection and search
            if i.key_pressed(egui::Key::Escape) {
                self.state.selected_path = None;
                self.state.search_query.clear();
            }
        });

        self.poll_channels(ctx);
        self.maybe_start_scan();
        let panels = draw_panels::draw_all_panels(self, ctx);
        self.apply_panel_changes(ctx, &panels);
        self.maybe_launch_folder_picker();
        self.draw_central_canvas(ctx);
    }
}

/// Private helpers extracted from `update()` to keep each method ≤50 lines.
impl SentruxApp {
    /// Check if root_path changed or rescan requested. [ref:bf6756ac] [ref:93cf32d4]
    fn maybe_start_scan(&mut self) {
        // Manual rescan: force re-scan of current project
        if self.state.rescan_requested {
            self.state.rescan_requested = false;
            self.last_scanned_root = None; // Force re-scan
        }
        if self.state.root_path == self.last_scanned_root || self.state.root_path.is_none() {
            return;
        }
        let should_retry = self.state.scan_retry_at.is_none_or(|t| {
            t.elapsed() >= Duration::from_millis(500)
        });
        if !should_retry {
            return;
        }
        let root_clone = self.state.root_path.clone();
        let sent = self.start_scan();
        if sent {
            self.last_scanned_root = root_clone;
            self.state.scan_retry_at = None;
        } else {
            self.state.scan_retry_at = Some(Instant::now());
        }
    }

    /// Apply layout/visual changes signaled by UI panels.
    fn apply_panel_changes(&mut self, ctx: &egui::Context, panels: &draw_panels::PanelResult) {
        if panels.layout_changed && self.state.snapshot.is_some() {
            if panels.layout_mode_changed {
                self.state.rendered_version = 0;
            }
            self.request_layout();
        } else if panels.visual_changed {
            ctx.request_repaint();
        }
        if panels.breadcrumb_changed && self.state.snapshot.is_some() {
            self.state.rendered_version = 0;
            self.request_layout();
        }
    }

    /// Launch async folder picker dialog if requested. [ref:b9f45231]
    fn maybe_launch_folder_picker(&mut self) {
        if !self.state.folder_picker_requested {
            return;
        }
        self.state.folder_picker_requested = false;
        if self.folder_picker_rx.is_some() {
            return;
        }
        let (tx, rx) = bounded::<Option<String>>(1);
        match std::thread::Builder::new()
            .name("folder-picker".into())
            .spawn(move || {
                let result = rfd::FileDialog::new().pick_folder();
                let _ = tx.send(result.map(|p| p.to_string_lossy().to_string()));
            })
        {
            Ok(_) => {
                self.folder_picker_rx = Some(rx);
            }
            Err(e) => {
                crate::debug_log!("[app] failed to spawn folder picker thread: {}", e);
                self.state.scan_step = format!("Error: folder picker failed: {}", e);
            }
        }
    }

    /// Check whether render data is absent or empty.
    fn should_show_placeholder(&self) -> (bool, bool) {
        let is_empty = self.state.render_data.as_ref()
            .is_some_and(|rd| rd.rects.is_empty());
        let no_data = self.state.render_data.is_none();
        (no_data || is_empty, is_empty)
    }

    /// Draw central canvas panel with treemap or placeholder.
    fn draw_central_canvas(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(self.state.theme_config.canvas_bg))
            .show(ctx, |ui| {
                let canvas_rect = ui.available_rect_before_wrap();
                let new_w = canvas_rect.width() as f64;
                let new_h = canvas_rect.height() as f64;

                // Detect significant canvas resize → re-fit viewport to new dimensions.
                // Threshold of 2.0 logical pixels avoids jitter from sub-pixel rounding.
                let resized = (new_w - self.state.viewport.canvas_w).abs() > 2.0
                    || (new_h - self.state.viewport.canvas_h).abs() > 2.0;
                self.state.viewport.canvas_w = new_w;
                self.state.viewport.canvas_h = new_h;
                if resized {
                    if let Some(rd) = &self.state.render_data {
                        self.state.viewport.fit_content(
                            rd.content_width,
                            rd.content_height,
                            self.state.settings.fit_content_padding,
                        );
                    }
                }

                let (show_placeholder, is_empty) = self.should_show_placeholder();
                if show_placeholder {
                    self.draw_placeholder(ui, is_empty);
                    return;
                }
                let response = ui.allocate_rect(canvas_rect, egui::Sense::click_and_drag());
                self.handle_canvas_interaction(&response, canvas_rect);
                self.paint_render_frame(ui, canvas_rect);
            });
    }

    /// Draw placeholder text when no render data is available.
    fn draw_placeholder(&self, ui: &mut egui::Ui, is_empty_render: bool) {
        if self.state.scanning {
            draw_panels::draw_progress(ui, &self.state, false);
        } else if is_empty_render && self.state.root_path.is_some() {
            let folder_name = self.state.root_path.as_ref()
                .and_then(|p| p.rsplit('/').next())
                .unwrap_or("folder");
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new(format!("'{}' is empty — no source files found", folder_name))
                        .size(18.0)
                        .color(self.state.theme_config.text_secondary),
                );
            });
        } else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Click 'Open Folder' to scan a project")
                        .size(18.0)
                        .color(self.state.theme_config.text_secondary),
                );
            });
        }
    }

    /// Paint the treemap render frame and optional progress overlay.
    fn paint_render_frame(&mut self, ui: &mut egui::Ui, canvas_rect: egui::Rect) {
        let painter = ui.painter_at(canvas_rect);
        let render_ctx = renderer::RenderContext {
            render_data: self.state.render_data.as_ref(),
            viewport: &self.state.viewport,
            theme_config: &self.state.theme_config,
            settings: &self.state.settings,
            file_index: &self.state.file_index,
            color_mode: self.state.color_mode,
            selected_path: self.state.selected_path.as_deref(),
            hovered_path: self.state.hovered_path.as_deref(),
            edge_filter: self.state.edge_filter,
            show_all_edges: self.state.show_all_edges,
            snapshot: self.state.snapshot.as_ref(),
            arch_report: self.state.arch_report.as_ref(),
            heat: &self.state.heat,
            frame_instant: self.state.frame_instant,
            frame_now_secs: self.state.frame_now_secs,
            anim_time: self.state.anim_time,
            interacting: self.state.interacting,
            root_path: self.state.root_path.as_deref(),
            search_query: &self.state.search_query,
        };
        renderer::render_frame(&painter, canvas_rect, &render_ctx);

        if self.state.scanning {
            draw_panels::draw_progress(ui, &self.state, true);
        }
    }
}

fn setup_fonts(ctx: &egui::Context, load_cjk: bool) {
    let mut fonts = egui::FontDefinitions::default();

    // Embed JetBrains Mono — clean, readable monospace font.
    let jbmono_data = include_bytes!("../../assets/fonts/jetbrains-mono.ttf");
    let jbmono_bold_data = include_bytes!("../../assets/fonts/jetbrains-mono-bold.ttf");

    fonts.font_data.insert(
        "jetbrains_mono".to_string(),
        egui::FontData::from_static(jbmono_data).into(),
    );
    fonts.font_data.insert(
        "jetbrains_mono_bold".to_string(),
        egui::FontData::from_static(jbmono_bold_data).into(),
    );

    // JetBrains Mono as PRIMARY monospace font (before egui defaults)
    fonts.families.entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "jetbrains_mono".to_string());

    // Also use for proportional (everything should be monospace in our app)
    fonts.families.entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "jetbrains_mono".to_string());

    if load_cjk {
        let cjk_paths = [
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\simsun.ttc",
        ];
        let mut cjk_loaded = false;
        for path in &cjk_paths {
            if let Ok(data) = std::fs::read(path) {
                fonts.font_data.insert(
                    "cjk_fallback".to_string(),
                    egui::FontData::from_owned(data).into(),
                );
                fonts.families.entry(egui::FontFamily::Monospace)
                    .or_default()
                    .push("cjk_fallback".to_string());
                fonts.families.entry(egui::FontFamily::Proportional)
                    .or_default()
                    .push("cjk_fallback".to_string());
                cjk_loaded = true;
                break;
            }
        }
        if !cjk_loaded {
            crate::debug_log!("[app] WARNING: no CJK font found — CJK characters will render as missing glyphs");
        }
    } else {
        crate::debug_log!("[app] CJK font loading disabled — saving 10-30MB memory");
    }

    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context, native_ppp: f32, ui_scale: f32) {
    // Scale ALL UI (text + widgets) uniformly via pixels_per_point.
    // Use native_ppp (pre-scaling baseline) so we multiply once, not repeatedly.
    ctx.set_pixels_per_point(native_ppp * ui_scale);

    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(egui::TextStyle::Body, egui::FontId::new(13.0, egui::FontFamily::Monospace));
    style.text_styles.insert(egui::TextStyle::Button, egui::FontId::new(12.0, egui::FontFamily::Monospace));
    style.text_styles.insert(egui::TextStyle::Small, egui::FontId::new(11.0, egui::FontFamily::Monospace));
    style.text_styles.insert(egui::TextStyle::Heading, egui::FontId::new(15.0, egui::FontFamily::Monospace));
    style.visuals.window_corner_radius = egui::CornerRadius::ZERO;
    style.visuals.menu_corner_radius = egui::CornerRadius::ZERO;
    style.visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::ZERO;
    style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::ZERO;
    style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::ZERO;
    style.visuals.widgets.active.corner_radius = egui::CornerRadius::ZERO;
    style.visuals.widgets.open.corner_radius = egui::CornerRadius::ZERO;
    style.visuals.popup_shadow = egui::epaint::Shadow::NONE;
    style.visuals.window_shadow = egui::epaint::Shadow::NONE;
    style.visuals.window_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(44, 46, 58));
    ctx.set_style(style);
}
