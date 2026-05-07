//! MCP tool handler implementations — core tools.
//!
//! Each handler has the uniform signature: `fn(&Value, &Tier, &mut McpState) -> Result<Value, String>`
//! Each tool also has a `_def()` function returning its `ToolDef` (schema + tier + handler co-located).
//!
//! Tier-aware truncation: detail lists are limited to `tier.detail_limit()` items.
//! Free users see top-3 + total counts. Pro users see everything.

use crate::analysis::scanner;
use crate::core::snapshot::Snapshot;
use crate::license::Tier;
use crate::metrics::arch;
use crate::metrics;
use super::McpState;
use super::registry::ToolDef;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ── Scan helper (shared by scan, rescan, session_end) ──

pub(crate) fn do_scan_with_options(
    root: &Path,
    options: scanner::ScanOptions,
) -> Result<(Snapshot, metrics::HealthReport, arch::ArchReport), String> {
    let root_str = root.to_str().ok_or("Invalid path encoding")?;
    let s = crate::core::settings::Settings::default();
    let result = scanner::scan_directory_with_options(
        root_str,
        None,
        None,
        &scanner::common::ScanLimits {
            max_file_size_kb: s.max_file_size_kb,
            max_parse_size_kb: s.max_parse_size_kb,
            max_call_targets: s.max_call_targets,
        },
        None, // MCP scans are not cancellable
        options,
    ).map_err(|e| format!("Scan failed: {e}"))?;
    let arch_report = arch::compute_arch(&result.snapshot);
    let health = metrics::compute_health(&result.snapshot);
    Ok((result.snapshot, health, arch_report))
}


// ══════════════════════════════════════════════════════════════════
//  SCAN
// ══════════════════════════════════════════════════════════════════

pub fn scan_def() -> ToolDef {
    ToolDef {
        name: "scan",
        description: "Scan a directory and compute all metrics. Must be called before other tools. Returns quality_signal.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the directory to scan" },
                "include_untracked": {
                    "type": "boolean",
                    "description": "Include untracked, non-ignored Git worktree files in the scan",
                    "default": false
                }
            },
            "required": ["path"]
        }),
        min_tier: Tier::Free,
        handler: handle_scan,
        invalidates_evolution: true,
    }
}

fn handle_scan(args: &Value, _tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let path = args.get("path").and_then(|p| p.as_str())
        .ok_or("Missing 'path' argument")?;

    let root = PathBuf::from(path);
    if !root.is_dir() {
        return Err(format!("Not a directory: {path}"));
    }

    let include_untracked = args.get("include_untracked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let (snapshot, health, arch_report) = do_scan_with_options(
        &root,
        scanner::ScanOptions { include_untracked },
    )?;

    let result = json!({
        "scanned": path,
        "include_untracked": include_untracked,
        "quality_signal": (health.quality_signal * 10000.0).round() as u32,
        "files": snapshot.total_files,
        "lines": snapshot.total_lines,
        "import_edges": snapshot.import_graph.len()
    });

    state.scan_root = Some(root);
    state.cached_snapshot = Some(Arc::new(snapshot));
    state.cached_health = Some(health);
    state.cached_arch = Some(arch_report);

    Ok(result)
}

// ══════════════════════════════════════════════════════════════════
//  HEALTH (tier-aware truncation)
// ══════════════════════════════════════════════════════════════════

pub fn health_def() -> ToolDef {
    ToolDef {
        name: "health",
        description: "Get quality signal (0-1) with root cause breakdown (modularity, acyclicity, depth, equality, redundancy). Quality signal = geometric mean — maximize this ONE number.",
        input_schema: json!({ "type": "object", "properties": {} }),
        min_tier: Tier::Free,
        handler: handle_health,
        invalidates_evolution: false,
    }
}

fn handle_health(_args: &Value, tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let h = state.cached_health.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let rc = &h.root_cause_scores;
    let raw = &h.root_cause_raw;
    // Identify the weakest root cause — this is where improvement effort should focus
    let scores_arr = [
        ("modularity", rc.modularity),
        ("acyclicity", rc.acyclicity),
        ("depth", rc.depth),
        ("equality", rc.equality),
        ("redundancy", rc.redundancy),
    ];
    let bottleneck = scores_arr.iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .map(|(name, _)| *name)
        .unwrap_or("none");

    let s = |v: f64| -> u32 { (v * 10000.0).round() as u32 };
    let mut result = json!({
        "quality_signal": s(h.quality_signal),
        "bottleneck": bottleneck,
        "root_causes": {
            "modularity":  {"score": s(rc.modularity),  "raw": raw.modularity_q},
            "acyclicity":  {"score": s(rc.acyclicity),  "raw": raw.cycle_count},
            "depth":       {"score": s(rc.depth),       "raw": raw.max_depth},
            "equality":    {"score": s(rc.equality),    "raw": raw.complexity_gini},
            "redundancy":  {"score": s(rc.redundancy),  "raw": raw.redundancy_ratio}
        },
        "total_import_edges": h.total_import_edges,
        "cross_module_edges": h.cross_module_edges
    });

    // Pro: root-cause-organized diagnostics. Tells AI WHERE to focus for each root cause.
    if crate::pro_registry::has(crate::pro_registry::ProFeature::McpDiagnostics) {
        result["diagnostics"] = json!({
            "modularity": {
                "god_files": h.god_files.iter().map(|f| json!({"path": f.path, "fan_out": f.value})).collect::<Vec<_>>(),
                "hotspot_files": h.hotspot_files.iter().map(|f| json!({"path": f.path, "fan_in": f.value})).collect::<Vec<_>>(),
                "most_unstable": h.most_unstable.iter().take(10).map(|m| json!({"path": m.path, "instability": m.instability, "fan_in": m.fan_in, "fan_out": m.fan_out})).collect::<Vec<_>>(),
            },
            "acyclicity": {
                "cycles": h.circular_dep_files.iter().collect::<Vec<_>>(),
            },
            "depth": {
                "max_depth": h.max_depth,
            },
            "equality": {
                "complex_functions": h.complex_functions.iter().take(20).map(|f| json!({"file": f.file, "func": f.func, "cc": f.value})).collect::<Vec<_>>(),
                "cog_complex_functions": h.cog_complex_functions.iter().take(20).map(|f| json!({"file": f.file, "func": f.func, "cog": f.value})).collect::<Vec<_>>(),
                "long_functions": h.long_functions.iter().take(20).map(|f| json!({"file": f.file, "func": f.func, "lines": f.value})).collect::<Vec<_>>(),
                "large_files": h.long_files.iter().take(10).map(|f| json!({"path": f.path, "lines": f.value})).collect::<Vec<_>>(),
                "high_param_functions": h.high_param_functions.iter().take(20).map(|f| json!({"file": f.file, "func": f.func, "params": f.value})).collect::<Vec<_>>(),
            },
            "redundancy": {
                "dead_functions": h.dead_functions.iter().take(50).map(|f| json!({"file": f.file, "func": f.func, "lines": f.value})).collect::<Vec<_>>(),
                "duplicate_groups": h.duplicate_groups.iter().take(20).map(|g| json!({"instances": g.instances.iter().map(|(file, func, lines)| json!({"file": file, "func": func, "lines": lines})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
            },
        });
    } else {
        result["upgrade"] = json!({
            "message": "Upgrade to Pro for root-cause diagnostics: https://github.com/sentrux/sentrux"
        });
    }

    Ok(result)
}

// Redundant tools removed: coupling, cycles, architecture, blast_radius, hottest, level.
// All diagnostics are grouped by root cause in the `health` tool's `diagnostics` field.
// See quality-signal-design.md — one true score, root-cause-organized diagnostics.

// ══════════════════════════════════════════════════════════════════
//  SESSION START
// ══════════════════════════════════════════════════════════════════

pub fn session_start_def() -> ToolDef {
    ToolDef {
        name: "session_start",
        description: "Save current health metrics as baseline for later comparison via 'gate' or 'session_end'.",
        input_schema: json!({ "type": "object", "properties": {} }),
        min_tier: Tier::Free,
        handler: handle_session_start,
        invalidates_evolution: false,
    }
}

fn handle_session_start(_args: &Value, _tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let h = state.cached_health.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let b = arch::ArchBaseline::from_health(h);
    let signal = b.quality_signal;
    state.baseline = Some(b);
    Ok(json!({
        "status": "Baseline saved",
        "quality_signal": (signal * 10000.0).round() as u32,
        "message": "Call 'session_end' after making changes to see the diff"
    }))
}

// ══════════════════════════════════════════════════════════════════
//  SESSION END
// ══════════════════════════════════════════════════════════════════

pub fn session_end_def() -> ToolDef {
    ToolDef {
        name: "session_end",
        description: "Re-scan and compare current state against session baseline. Returns diff showing what degraded.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "include_untracked": {
                    "type": "boolean",
                    "description": "Include untracked, non-ignored Git worktree files in the scan",
                    "default": false
                }
            }
        }),
        min_tier: Tier::Free,
        handler: handle_session_end,
        invalidates_evolution: true,
    }
}

fn handle_session_end(args: &Value, _tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    // Clone to avoid borrow conflict: we read root+baseline, then mutate state.
    let root = state.scan_root.clone().ok_or("No scan root. Call 'scan' first.")?;
    let baseline = state.baseline.clone().ok_or("No baseline saved. Call 'session_start' first.")?;

    let include_untracked = args.get("include_untracked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let (snapshot, health, arch_report) = do_scan_with_options(
        &root,
        scanner::ScanOptions { include_untracked },
    )?;
    let diff = baseline.diff(&health);

    let result = json!({
        "pass": !diff.degraded,
        "include_untracked": include_untracked,
        "signal_before": (diff.signal_before * 10000.0).round() as i32,
        "signal_after": (diff.signal_after * 10000.0).round() as i32,
        "signal_delta": ((diff.signal_after - diff.signal_before) * 10000.0).round() as i32,
        "coupling_change": [diff.coupling_before, diff.coupling_after],
        "cycles_change": [diff.cycles_before, diff.cycles_after],
        "violations": diff.violations,
        "summary": if diff.degraded { "Quality degraded" } else { "Quality stable or improved" }
    });

    state.cached_snapshot = Some(Arc::new(snapshot));
    state.cached_health = Some(health);
    state.cached_arch = Some(arch_report);

    Ok(result)
}

// ══════════════════════════════════════════════════════════════════
//  RESCAN
// ══════════════════════════════════════════════════════════════════

pub fn rescan_def() -> ToolDef {
    ToolDef {
        name: "rescan",
        description: "Re-scan the current directory to pick up file changes since last scan.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "include_untracked": {
                    "type": "boolean",
                    "description": "Include untracked, non-ignored Git worktree files in the scan",
                    "default": false
                }
            }
        }),
        min_tier: Tier::Free,
        handler: handle_rescan,
        invalidates_evolution: true,
    }
}

fn handle_rescan(args: &Value, _tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    // Clone root to avoid borrow conflict
    let root = state.scan_root.clone().ok_or("No scan root. Call 'scan' first.")?;
    let include_untracked = args.get("include_untracked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let (snapshot, health, arch_report) = do_scan_with_options(
        &root,
        scanner::ScanOptions { include_untracked },
    )?;

    let result = json!({
        "status": "Rescanned",
        "include_untracked": include_untracked,
        "quality_signal": (health.quality_signal * 10000.0).round() as u32,
        "files": snapshot.total_files
    });

    state.cached_snapshot = Some(Arc::new(snapshot));
    state.cached_health = Some(health);
    state.cached_arch = Some(arch_report);

    Ok(result)
}

// ══════════════════════════════════════════════════════════════════
//  CHECK RULES
// ══════════════════════════════════════════════════════════════════

pub fn check_rules_def() -> ToolDef {
    ToolDef {
        name: "check_rules",
        description: "Check .sentrux/rules.toml architectural constraints. Returns pass/fail with specific violations.",
        input_schema: json!({ "type": "object", "properties": {} }),
        min_tier: Tier::Free,
        handler: handle_check_rules,
        invalidates_evolution: false,
    }
}

fn handle_check_rules(_args: &Value, tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let root = state.scan_root.as_ref().ok_or("No scan root. Call 'scan' first.")?;
    let h = state.cached_health.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let a = state.cached_arch.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let snap = state.cached_snapshot.as_ref().ok_or("No scan data. Call 'scan' first.")?;

    let mut config = crate::metrics::rules::RulesConfig::try_load(root)
        .ok_or_else(|| format!(
            "No rules file found at {}/.sentrux/rules.toml. Create one to define architectural constraints.",
            root.display()
        ))?;

    // Free tier: max 3 rules (constraints count as 1 if any thresholds set,
    // plus layers and boundaries each count as 1 rule).
    let total_rules = config.constraints.count_active()
        + config.layers.len()
        + config.boundaries.len();
    let truncated = if !crate::pro_registry::has(crate::pro_registry::ProFeature::UnlimitedRules) && total_rules > 3 {
        // Keep constraints (1 rule) + first 2 of layers/boundaries
        let mut remaining = 3usize.saturating_sub(if config.constraints.count_active() > 0 { 1 } else { 0 });
        config.layers.truncate(remaining.min(config.layers.len()));
        remaining = remaining.saturating_sub(config.layers.len());
        config.boundaries.truncate(remaining.min(config.boundaries.len()));
        true
    } else {
        false
    };

    let result = crate::metrics::rules::check_rules(&config, h, a, &snap.import_graph);

    let mut response = json!({
        "pass": result.passed,
        "rules_checked": result.rules_checked,
        "violation_count": result.violations.len(),
        "violations": result.violations.iter().map(|v| json!({
            "rule": v.rule,
            "severity": format!("{:?}", v.severity),
            "message": v.message,
            "files": v.files
        })).collect::<Vec<_>>(),
        "summary": if result.passed { "✓ All architectural rules pass" }
            else { "✗ Architectural rule violations detected" }
    });
    if truncated {
        response["truncated"] = json!({
            "total_rules_defined": total_rules,
            "rules_checked": result.rules_checked,
            "message": "Checking up to 3 rules. More available with sentrux Pro: https://github.com/sentrux/sentrux"
        });
    }
    Ok(response)
}
