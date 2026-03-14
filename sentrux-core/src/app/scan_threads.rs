//! Background thread implementations for scanner and layout workers.
//!
//! Each thread runs in a loop receiving commands from the main thread,
//! processing them, and sending results back. Handles panic recovery and
//! generation-based stale result rejection.

use super::channels::{LayoutMsg, LayoutRequest, ScanCommand, ScanMsg};
use crossbeam_channel::{Receiver, Sender};
use std::sync::Arc;

/// Re-export ScanLimits from scanner::common — single source of truth.
pub(crate) use crate::analysis::scanner::common::ScanLimits;

/// Format a thread panic payload into a user-visible error string.
pub(crate) fn format_panic(thread_name: &str, payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        format!("Error: {} thread panicked: {}", thread_name, s)
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("Error: {} thread panicked: {}", thread_name, s)
    } else {
        format!("Error: {} thread panicked (unknown payload)", thread_name)
    }
}

/// Scanner background thread — handles both full scan and incremental rescan.
/// Generation is embedded in each ScanCommand, so no shared atomic is needed. [ref:13696c9c]
pub(crate) fn scanner_thread(
    rx: Receiver<ScanCommand>,
    tx: Sender<ScanMsg>,
) {
    while let Ok(cmd) = rx.recv() {
        let cmd = drain_to_latest(cmd, &rx);
        match cmd {
            ScanCommand::FullScan { ref root, limits, gen } => {
                handle_full_scan(&tx, root, &limits, gen);
            }
            ScanCommand::Rescan { ref root, ref changed, ref old_snap, limits, gen } => {
                handle_rescan(&tx, root, changed, old_snap, &limits, gen);
            }
        }
    }
}

fn handle_full_scan(
    tx: &Sender<ScanMsg>,
    root_path: &str,
    limits: &ScanLimits,
    gen: u64,
) {
    crate::analysis::parser::clear_cache();
    crate::analysis::git::clear_cache();

    let tx_progress = tx.clone();
    let tx_tree = tx.clone();
    let gen_for_tree = gen;

    let result = crate::analysis::scanner::scan_directory(
        root_path,
        Some(&move |p| {
            if let Err(crossbeam_channel::TrySendError::Disconnected(_)) = tx_progress.try_send(ScanMsg::Progress(p)) {
                eprintln!("[scanner] progress channel disconnected");
            }
        }),
        Some(&move |snap| {
            if let Err(e) = tx_tree.send(ScanMsg::TreeReady(Arc::new(snap), gen_for_tree)) {
                eprintln!("[scanner] failed to send TreeReady: {}", e);
            }
        }),
        limits,
    );

    send_scan_result(tx, result, gen, root_path);
}

fn handle_rescan(
    tx: &Sender<ScanMsg>,
    root: &str,
    changed: &[String],
    old_snap: &Arc<crate::core::snapshot::Snapshot>,
    limits: &ScanLimits,
    gen: u64,
) {
    let result = crate::analysis::scanner::rescan::rescan_changed(
        root,
        old_snap,
        changed,
        None,
        limits,
    );

    send_scan_result(tx, result, gen, root);
}

fn send_scan_result(
    tx: &Sender<ScanMsg>,
    result: Result<crate::analysis::scanner::ScanResult, crate::core::types::AppError>,
    gen: u64,
    root_path: &str,
) {
    match result {
        Ok(scan_result) => {
            let snap = Arc::new(scan_result.snapshot);
            let rules_cfg = crate::metrics::rules::RulesConfig::try_load(
                std::path::Path::new(root_path)
            );
            let mcfg = crate::metrics::MetricsConfig {
                module_depth: rules_cfg.as_ref().and_then(|c| c.constraints.module_depth),
                cohesion_exclude: rules_cfg.as_ref().map(|c| c.constraints.cohesion_exclude.clone()).unwrap_or_default(),
            };
            let report = crate::metrics::compute_health_with_config(&snap, &mcfg);
            let arch = crate::metrics::arch::compute_arch(&snap);

            // Build complexity map once and share between evolution and test gap analysis.
            // Previously computed twice — O(N) duplicated work.
            let complexity_map = build_complexity_map(&snap);

            // Compute evolution metrics (git log walking) — may take a few seconds
            let evolution = compute_evolution_report_with_map(root_path, &snap, &complexity_map);

            // Compute test gap analysis (fast — graph traversal only)
            let test_gaps = compute_test_gap_report_with_map(&snap, &complexity_map);

            // Compute rules check if .sentrux/rules.toml exists
            let rules = compute_rules_check(root_path, &snap, &report, &arch);

            let reports = crate::app::channels::ScanReports {
                health: Some(report),
                arch: Some(arch),
                evolution,
                test_gaps: Some(test_gaps),
                rules,
            };

            if tx.send(ScanMsg::Complete(Arc::clone(&snap), gen, Box::new(reports))).is_err() {
                eprintln!("[scanner] failed to send Complete — receiver dropped");
            }
        }
        Err(e) => {
            if tx.send(ScanMsg::Error(e.to_string(), gen)).is_err() {
                eprintln!("[scanner] failed to send Error — receiver dropped");
            }
        }
    }
}

/// Compute evolution report with a pre-built complexity map.
/// Returns None if git log walking fails (e.g., not a git repo).
fn compute_evolution_report_with_map(
    root: &str,
    snap: &crate::core::snapshot::Snapshot,
    complexity_map: &std::collections::HashMap<String, u32>,
) -> Option<crate::metrics::evo::EvolutionReport> {
    let root_path = std::path::Path::new(root);
    let known_files: std::collections::HashSet<String> = crate::core::snapshot::flatten_files_ref(&snap.root)
        .iter()
        .filter(|f| !f.is_dir)
        .map(|f| f.path.clone())
        .collect();
    match crate::metrics::evo::compute_evolution(root_path, &known_files, complexity_map, None) {
        Ok(report) => Some(report),
        Err(e) => {
            eprintln!("[scanner] evolution metrics skipped: {}", e);
            None
        }
    }
}

/// Build complexity map: file → max cyclomatic complexity.
fn build_complexity_map(snap: &crate::core::snapshot::Snapshot) -> std::collections::HashMap<String, u32> {
    let mut map = std::collections::HashMap::new();
    fn collect(node: &crate::core::types::FileNode, map: &mut std::collections::HashMap<String, u32>) {
        if !node.is_dir {
            if let Some(sa) = &node.sa {
                if let Some(funcs) = &sa.functions {
                    let max_cc = funcs.iter().filter_map(|f| f.cc).max().unwrap_or(1);
                    map.insert(node.path.clone(), max_cc);
                }
            }
        }
        if let Some(children) = &node.children {
            for child in children {
                collect(child, map);
            }
        }
    }
    collect(&snap.root, &mut map);
    map
}

/// Compute test gap report with a pre-built complexity map.
fn compute_test_gap_report_with_map(
    snap: &crate::core::snapshot::Snapshot,
    complexity_map: &std::collections::HashMap<String, u32>,
) -> crate::metrics::testgap::TestGapReport {
    crate::metrics::testgap::compute_test_gaps(snap, complexity_map)
}

/// Compute rules check if .sentrux/rules.toml exists.
fn compute_rules_check(
    root: &str,
    snap: &crate::core::snapshot::Snapshot,
    health: &crate::metrics::HealthReport,
    arch: &crate::metrics::arch::ArchReport,
) -> Option<crate::metrics::rules::checks::RuleCheckResult> {
    let root_path = std::path::Path::new(root);
    let config = crate::metrics::rules::RulesConfig::try_load(root_path)?;
    Some(crate::metrics::rules::check_rules(&config, health, arch, &snap.import_graph))
}

/// Drain any queued commands, keeping only the latest. [ref:b9f45231]
pub(crate) fn drain_to_latest(first: ScanCommand, rx: &Receiver<ScanCommand>) -> ScanCommand {
    let mut commands = vec![first];
    while let Ok(cmd) = rx.try_recv() {
        commands.push(cmd);
    }

    let last_full_idx = commands
        .iter()
        .rposition(|c| matches!(c, ScanCommand::FullScan { .. }));

    if let Some(idx) = last_full_idx {
        return commands.swap_remove(idx);
    }

    if commands.len() == 1 {
        return commands.swap_remove(0);
    }

    let mut merged_changes: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cmd in &commands {
        if let ScanCommand::Rescan { changed, .. } = cmd {
            merged_changes.extend(changed.iter().cloned());
        }
    }

    let last_idx = commands.len() - 1;
    let mut result = commands.swap_remove(last_idx);
    if let ScanCommand::Rescan { ref mut changed, .. } = result {
        *changed = merged_changes.into_iter().collect();
    }
    result
}

/// Layout background thread
pub(crate) fn layout_thread(rx: Receiver<LayoutRequest>, tx: Sender<LayoutMsg>) {
    while let Ok(req) = rx.recv() {
        // Drain any stale requests — only process the latest
        let mut latest = req;
        while let Ok(newer) = rx.try_recv() {
            latest = newer;
        }

        let version = latest.version;
        let cfg = crate::layout::LayoutConfig {
            size_mode: latest.size_mode,
            scale_mode: latest.scale_mode,
            layout_mode: latest.layout_mode,
            heat_map: latest.heat_map.as_ref(),
            settings: &latest.settings,
            focus_mode: &latest.focus_mode,
            entry_point_files: &*latest.entry_point_files,
            hidden_paths: &*latest.hidden_paths,
            impact_files: latest.impact_files.as_deref(),
        };
        let rd = crate::layout::compute_layout_from_snapshot(
            &latest.snapshot,
            latest.viewport_w,
            latest.viewport_h,
            latest.drill_path.as_deref(),
            &cfg,
        );
        match tx.try_send(LayoutMsg::Ready(rd, version)) {
            Ok(()) => {}
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                // Stale result dropped — main thread will re-request layout
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                eprintln!("[layout] failed to send Ready — receiver dropped");
                break;
            }
        }
    }
}
