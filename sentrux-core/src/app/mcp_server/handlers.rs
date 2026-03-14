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

pub(crate) fn do_scan(root: &Path) -> Result<(Snapshot, metrics::HealthReport, arch::ArchReport), String> {
    let root_str = root.to_str().ok_or("Invalid path encoding")?;
    let s = crate::core::settings::Settings::default();
    let result = scanner::scan_directory(
        root_str,
        None,
        None,
        &scanner::common::ScanLimits {
            max_file_size_kb: s.max_file_size_kb,
            max_parse_size_kb: s.max_parse_size_kb,
            max_call_targets: s.max_call_targets,
        },
    ).map_err(|e| format!("Scan failed: {e}"))?;
    let rules_cfg = crate::metrics::rules::RulesConfig::try_load(root);
    let mcfg = metrics::MetricsConfig {
        module_depth: rules_cfg.as_ref().and_then(|c| c.constraints.module_depth),
        cohesion_exclude: rules_cfg.as_ref().map(|c| c.constraints.cohesion_exclude.clone()).unwrap_or_default(),
    };
    let health = metrics::compute_health_with_config(&result.snapshot, &mcfg);
    let arch_report = arch::compute_arch(&result.snapshot);
    Ok((result.snapshot, health, arch_report))
}


// ══════════════════════════════════════════════════════════════════
//  SCAN
// ══════════════════════════════════════════════════════════════════

pub fn scan_def() -> ToolDef {
    ToolDef {
        name: "scan",
        description: "Scan a directory and compute all metrics. Must be called before other tools. Returns structure grade.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the directory to scan" }
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

    let (snapshot, health, arch_report) = do_scan(&root)?;

    let result = json!({
        "scanned": path,
        "structure_grade": health.grade.to_string(),
        "architecture_grade": arch_report.arch_grade.to_string(),
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
        description: "Get full health report with 11-dimension A-F grades for coupling, cycles, complexity, cohesion, entropy, god files, hotspots, function length, comment ratio, and file size.",
        input_schema: json!({ "type": "object", "properties": {} }),
        min_tier: Tier::Free,
        handler: handle_health,
        invalidates_evolution: false,
    }
}

fn handle_health(_args: &Value, tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let h = state.cached_health.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let d = &h.dimensions;

    let mut result = json!({
        "grade": h.grade.to_string(),
        "dimensions": {
            "cycles":      {"grade": d.cycles.to_string(),      "value": h.circular_dep_count},
            "complex_fn":  {"grade": d.complex_fn.to_string(),  "value": format!("{:.1}%", h.complex_fn_ratio * 100.0)},
            "coupling":    {"grade": d.coupling.to_string(),    "value": format!("{:.2}", h.coupling_score)},
            "entropy":     {"grade": d.entropy.to_string(),     "value": format!("{:.2}", h.entropy)},
            "cohesion":    {"grade": d.cohesion.map(|g| g.to_string()).unwrap_or("-".into()), "value": h.avg_cohesion.map(|c| format!("{:.2}", c)).unwrap_or("n/a".into())},
            "depth":       {"grade": d.depth.to_string(),       "value": h.max_depth},
            "god_files":   {"grade": d.god_files.to_string(),   "value": format!("{} ({:.1}%)", h.god_files.len(), h.god_file_ratio * 100.0)},
            "hotspots":    {"grade": d.hotspots.to_string(),    "value": format!("{} ({:.1}%)", h.hotspot_files.len(), h.hotspot_ratio * 100.0)},
            "long_fn":     {"grade": d.long_fn.to_string(),     "value": format!("{} ({:.1}%)", h.long_functions.len(), h.long_fn_ratio * 100.0)},
            "comment":     {"grade": d.comment.map(|g| g.to_string()).unwrap_or("-".into()), "value": h.comment_ratio.map(|c| format!("{:.1}%", c * 100.0)).unwrap_or("n/a".into())},
            "file_size":   {"grade": d.file_size.to_string(),   "value": format!("{} ({:.1}%)", h.large_file_count, h.large_file_ratio * 100.0)},
            "cog_complex": {"grade": d.cog_complex.to_string(), "value": format!("{} ({:.1}%)", h.cog_complex_functions.len(), h.cog_complex_ratio * 100.0)},
            "duplication":  {"grade": d.duplication.to_string(),  "value": format!("{} groups ({:.1}%)", h.duplicate_groups.len(), h.duplication_ratio * 100.0)},
            "dead_code":   {"grade": d.dead_code.to_string(),   "value": format!("{} ({:.1}%)", h.dead_functions.len(), h.dead_code_ratio * 100.0)},
            "high_params": {"grade": d.high_params.to_string(), "value": format!("{} ({:.1}%)", h.high_param_functions.len(), h.high_param_ratio * 100.0)}
        },
        "total_import_edges": h.total_import_edges,
        "cross_module_edges": h.cross_module_edges
    });

    // Pro: full file-level detail lists. Free: grades + counts only, no file paths.
    if tier.is_pro() {
        result["details"] = json!({
            "complex_functions": h.complex_functions.iter().take(100).map(|f| json!({"file": f.file, "func": f.func, "cc": f.value})).collect::<Vec<_>>(),
            "long_functions": h.long_functions.iter().take(100).map(|f| json!({"file": f.file, "func": f.func, "lines": f.value})).collect::<Vec<_>>(),
            "cog_complex_functions": h.cog_complex_functions.iter().take(100).map(|f| json!({"file": f.file, "func": f.func, "cog": f.value})).collect::<Vec<_>>(),
            "high_param_functions": h.high_param_functions.iter().take(100).map(|f| json!({"file": f.file, "func": f.func, "params": f.value})).collect::<Vec<_>>(),
            "duplicate_groups": h.duplicate_groups.iter().take(100).map(|g| json!({"instances": g.instances.iter().map(|(file, func, lines)| json!({"file": file, "func": func, "lines": lines})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
            "dead_functions": h.dead_functions.iter().take(100).map(|f| json!({"file": f.file, "func": f.func, "lines": f.value})).collect::<Vec<_>>(),
            "god_files": h.god_files.iter().map(|f| json!({"path": f.path, "fan_out": f.value})).collect::<Vec<_>>(),
            "hotspot_files": h.hotspot_files.iter().map(|f| json!({"path": f.path, "fan_in": f.value})).collect::<Vec<_>>(),
            "long_files": h.long_files.iter().take(10).map(|f| json!({"path": f.path, "lines": f.value})).collect::<Vec<_>>(),
            "cycles": h.circular_dep_files.iter().collect::<Vec<_>>()
        });
    } else {
        result["upgrade"] = json!({
            "message": "Upgrade to Pro for file-level details: https://github.com/sentrux/sentrux"
        });
    }

    Ok(result)
}

// ══════════════════════════════════════════════════════════════════
//  COUPLING
// ══════════════════════════════════════════════════════════════════

pub fn coupling_def() -> ToolDef {
    ToolDef {
        name: "coupling",
        description: "Get coupling details: score, cross-module edges, and god files (high fan-out).",
        input_schema: json!({ "type": "object", "properties": {} }),
        min_tier: Tier::Free,
        handler: handle_coupling,
        invalidates_evolution: false,
    }
}

fn handle_coupling(_args: &Value, tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let h = state.cached_health.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let mut result = json!({
        "coupling_score": h.coupling_score,
        "grade": h.dimensions.coupling.to_string(),
        "cross_module_edges": h.cross_module_edges,
        "total_edges": h.total_import_edges,
        "god_files_count": h.god_files.len(),
        "hotspot_files_count": h.hotspot_files.len()
    });
    if tier.is_pro() {
        result["god_files"] = json!(h.god_files.iter().map(|f| json!({"path": f.path, "fan_out": f.value})).collect::<Vec<_>>());
        result["hotspot_files"] = json!(h.hotspot_files.iter().map(|f| json!({"path": f.path, "fan_in": f.value})).collect::<Vec<_>>());
    }
    Ok(result)
}

// ══════════════════════════════════════════════════════════════════
//  CYCLES
// ══════════════════════════════════════════════════════════════════

pub fn cycles_def() -> ToolDef {
    ToolDef {
        name: "cycles",
        description: "Get circular dependency details: count and list of files in each cycle.",
        input_schema: json!({ "type": "object", "properties": {} }),
        min_tier: Tier::Free,
        handler: handle_cycles,
        invalidates_evolution: false,
    }
}

fn handle_cycles(_args: &Value, tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let h = state.cached_health.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let mut result = json!({
        "cycle_count": h.circular_dep_count,
        "grade": h.dimensions.cycles.to_string()
    });
    if tier.is_pro() {
        result["cycles"] = json!(h.circular_dep_files);
    }
    Ok(result)
}

// ══════════════════════════════════════════════════════════════════
//  ARCHITECTURE
// ══════════════════════════════════════════════════════════════════

pub fn architecture_def() -> ToolDef {
    ToolDef {
        name: "architecture",
        description: "Get architecture-level metrics: levelization, upward dependency violations, blast radius, attack surface.",
        input_schema: json!({ "type": "object", "properties": {} }),
        min_tier: Tier::Free,
        handler: handle_architecture,
        invalidates_evolution: false,
    }
}

fn handle_architecture(_args: &Value, tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let a = state.cached_arch.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let mut result = json!({
        "arch_grade": a.arch_grade.to_string(),
        "levelization_grade": a.levelization_grade.to_string(),
        "blast_grade": a.blast_grade.to_string(),
        "surface_grade": a.surface_grade.to_string(),
        "distance_grade": a.distance_grade.to_string(),
        "avg_distance_from_main_seq": format!("{:.3}", a.avg_distance),
        "max_level": a.max_level,
        "upward_violations_count": a.upward_violations.len(),
        "upward_ratio": format!("{:.2}%", a.upward_ratio * 100.0),
        "max_blast_radius": a.max_blast_radius,
        "attack_surface_files": a.attack_surface_files,
        "attack_surface_ratio": format!("{:.1}%", a.attack_surface_ratio * 100.0),
        "total_graph_files": a.total_graph_files
    });
    // Pro: file-level details (violation files, distance per module, blast file name)
    if tier.is_pro() {
        result["max_blast_file"] = json!(a.max_blast_file);
        result["top_violations"] = json!(a.upward_violations.iter().take(5).map(|v| json!({
            "from": v.from_file, "from_level": v.from_level,
            "to": v.to_file, "to_level": v.to_level
        })).collect::<Vec<_>>());
        result["distance_from_main_sequence"] = json!(a.distance_metrics.iter().take(10).map(|m| json!({
            "module": m.module,
            "abstractness": format!("{:.2}", m.abstractness),
            "instability": format!("{:.2}", m.instability),
            "distance": format!("{:.3}", m.distance),
            "abstract_types": m.abstract_count, "total_types": m.total_types,
            "fan_in": m.fan_in, "fan_out": m.fan_out
        })).collect::<Vec<_>>());
    }
    Ok(result)
}

// ══════════════════════════════════════════════════════════════════
//  BLAST RADIUS
// ══════════════════════════════════════════════════════════════════

pub fn blast_radius_def() -> ToolDef {
    ToolDef {
        name: "blast_radius",
        description: "Get the blast radius for a specific file: how many files are transitively affected if this file changes. (Pro)",
        input_schema: json!({
            "type": "object",
            "properties": {
                "file": { "type": "string", "description": "Relative path to the file (e.g., 'src/app.rs')" }
            },
            "required": ["file"]
        }),
        min_tier: Tier::Pro,
        handler: handle_blast_radius,
        invalidates_evolution: false,
    }
}

fn handle_blast_radius(args: &Value, _tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let a = state.cached_arch.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let file = args.get("file").and_then(|f| f.as_str())
        .ok_or("Missing 'file' argument")?;
    let radius = a.blast_radius.get(file).copied().unwrap_or(0);
    let level = a.levels.get(file).copied().unwrap_or(0);
    Ok(json!({
        "file": file,
        "blast_radius": radius,
        "level": level,
        "interpretation": if radius > 20 { "HIGH RISK: changing this file affects many others" }
            else if radius > 5 { "MODERATE: changing this file has significant impact" }
            else { "LOW: safe to modify, limited impact" }
    }))
}

// ══════════════════════════════════════════════════════════════════
//  HOTTEST
// ══════════════════════════════════════════════════════════════════

pub fn hottest_def() -> ToolDef {
    ToolDef {
        name: "hottest",
        description: "Get the files with highest blast radius (most dangerous to change).",
        input_schema: json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "Number of files to return (default 10)" }
            }
        }),
        min_tier: Tier::Free,
        handler: handle_hottest,
        invalidates_evolution: false,
    }
}

fn handle_hottest(args: &Value, tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let a = state.cached_arch.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let mut result = json!({
        "max_blast_radius": a.max_blast_radius,
        "total_files_in_graph": a.total_graph_files
    });
    // Pro: file-level list. Free: max blast radius only.
    if tier.is_pro() {
        let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize;
        let mut files: Vec<(&String, &u32)> = a.blast_radius.iter().collect();
        files.sort_unstable_by(|a, b| b.1.cmp(a.1));
        files.truncate(limit);
        result["hottest_files"] = json!(files.iter().map(|(path, &radius)| json!({
            "path": path, "blast_radius": radius,
            "level": a.levels.get(*path).copied().unwrap_or(0)
        })).collect::<Vec<_>>());
    }
    Ok(result)
}

// ══════════════════════════════════════════════════════════════════
//  LEVEL
// ══════════════════════════════════════════════════════════════════

pub fn level_def() -> ToolDef {
    ToolDef {
        name: "level",
        description: "Get the dependency level of a specific file. Level 0 = leaf (depends on nothing), higher = depends on more layers. (Pro)",
        input_schema: json!({
            "type": "object",
            "properties": {
                "file": { "type": "string", "description": "Relative path to the file" }
            },
            "required": ["file"]
        }),
        min_tier: Tier::Pro,
        handler: handle_level,
        invalidates_evolution: false,
    }
}

fn handle_level(args: &Value, _tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let a = state.cached_arch.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let file = args.get("file").and_then(|f| f.as_str())
        .ok_or("Missing 'file' argument")?;
    match a.levels.get(file).copied() {
        Some(l) => Ok(json!({
            "file": file, "level": l, "max_level": a.max_level,
            "interpretation": if l == 0 { "Leaf node: depends on nothing. Safest to modify." }
                else if l == a.max_level { "Top-level: depends on everything. Most complex." }
                else { "Mid-level: depends on lower layers." }
        })),
        None => Err(format!("File '{file}' not found in import graph")),
    }
}

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
    let grade = b.structure_grade;
    state.baseline = Some(b);
    Ok(json!({
        "status": "Baseline saved",
        "grade": grade.to_string(),
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
        input_schema: json!({ "type": "object", "properties": {} }),
        min_tier: Tier::Free,
        handler: handle_session_end,
        invalidates_evolution: true,
    }
}

fn handle_session_end(_args: &Value, _tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    // Clone to avoid borrow conflict: we read root+baseline, then mutate state.
    let root = state.scan_root.clone().ok_or("No scan root. Call 'scan' first.")?;
    let baseline = state.baseline.clone().ok_or("No baseline saved. Call 'session_start' first.")?;

    let (snapshot, health, arch_report) = do_scan(&root)?;
    let diff = baseline.diff(&health);

    let result = json!({
        "pass": !diff.degraded,
        "grade_before": diff.structure_grade_before.to_string(),
        "grade_after": diff.structure_grade_after.to_string(),
        "coupling_change": format!("{:.2} → {:.2}", diff.coupling_before, diff.coupling_after),
        "cycles_change": format!("{} → {}", diff.cycles_before, diff.cycles_after),
        "god_files_change": format!("{} → {}", diff.god_files_before, diff.god_files_after),
        "violations": diff.violations,
        "summary": if diff.degraded { "⚠ Architecture degraded during this session" }
            else { "✓ Architecture stable or improved" }
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
        input_schema: json!({ "type": "object", "properties": {} }),
        min_tier: Tier::Free,
        handler: handle_rescan,
        invalidates_evolution: true,
    }
}

fn handle_rescan(_args: &Value, _tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    // Clone root to avoid borrow conflict
    let root = state.scan_root.clone().ok_or("No scan root. Call 'scan' first.")?;
    let (snapshot, health, arch_report) = do_scan(&root)?;

    let result = json!({
        "status": "Rescanned",
        "structure_grade": health.grade.to_string(),
        "architecture_grade": arch_report.arch_grade.to_string(),
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
    let truncated = if !tier.is_pro() && total_rules > 3 {
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
            "message": "Free tier checks up to 3 rules. Upgrade to Pro for unlimited: https://github.com/sentrux/sentrux"
        });
    }
    Ok(response)
}
