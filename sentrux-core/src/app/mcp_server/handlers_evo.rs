//! MCP tool handlers for evolution, churn, bus factor, coupling history,
//! what-if simulation, DSM, and test gap analysis.
//!
//! Same uniform signature as handlers.rs: `fn(&Value, &Tier, &mut McpState) -> Result<Value, String>`

use crate::core::snapshot::Snapshot;
use crate::core::types::FileNode;
use crate::metrics::evolution;
use super::McpState;
use super::registry::ToolDef;
use crate::license::Tier;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

// ── Helpers (unchanged) ──

pub(crate) fn build_complexity_map(snapshot: &Snapshot) -> HashMap<String, u32> {
    let mut map = HashMap::new();
    collect_complexity(&snapshot.root, &mut map);
    map
}

fn extract_max_cc(node: &FileNode) -> Option<u32> {
    let funcs = node.sa.as_ref()?.functions.as_ref()?;
    Some(funcs.iter().filter_map(|f| f.cc).max().unwrap_or(1))
}

fn collect_complexity(node: &FileNode, map: &mut HashMap<String, u32>) {
    if !node.is_dir {
        if let Some(max_cc) = extract_max_cc(node) {
            map.insert(node.path.clone(), max_cc);
        }
    }
    if let Some(children) = &node.children {
        for child in children {
            collect_complexity(child, map);
        }
    }
}

pub(crate) fn build_known_files(snapshot: &Snapshot) -> HashSet<String> {
    let mut set = HashSet::new();
    collect_files(&snapshot.root, &mut set);
    set
}

fn collect_files(node: &FileNode, set: &mut HashSet<String>) {
    if !node.is_dir {
        set.insert(node.path.clone());
    }
    if let Some(children) = &node.children {
        for child in children {
            collect_files(child, set);
        }
    }
}

// ══════════════════════════════════════════════════════════════════
//  GIT STATS (churn, hotspots, bus factor, change coupling)
// ══════════════════════════════════════════════════════════════════

pub fn evolution_def() -> ToolDef {
    ToolDef {
        name: "git_stats",
        description: "Git history analysis: code churn, hotspots (churn x complexity), bus factor, change coupling. Enriches top_files (churn/risk metrics), suggest_refactoring (merge suggestions), impact_analysis (change coupling), and file_info (per-file git history). Requires a git repository.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "days": { "type": "integer", "description": "Lookback window in days (default 90)" }
            }
        }),
        min_tier: Tier::Free,
        handler: handle_evolution,
        invalidates_evolution: false,
    }
}

fn handle_evolution(args: &Value, tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let root = state.scan_root.as_ref().ok_or("No scan root. Call 'scan' first.")?;
    let snap = state.cached_snapshot.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let days = args.get("days").and_then(|d| d.as_u64()).map(|d| d as u32);

    let known = build_known_files(snap);
    let complexity = build_complexity_map(snap);

    let report = evolution::compute_evolution(root, &known, &complexity, days)
        .map_err(|e| format!("Evolution analysis failed: {e}"))?;

    let mut result = json!({
        "lookback_days": report.lookback_days,
        "commits_analyzed": report.commits_analyzed,
        "files_with_churn": report.churn.len(),
        "single_author_ratio": report.single_author_ratio,
        "coupling_pairs_found": report.coupling_pairs.len(),
        "hotspot_count": report.hotspots.len(),
        "bus_factor_solo_files": (report.single_author_ratio * report.churn.len() as f64).round() as u32
    });

    // Pro: file-level hotspot details. Free: scores + counts only.
    if tier.is_pro() {
        result["top_hotspots"] = json!(report.hotspots.iter().take(10).map(|h| json!({
            "file": h.file,
            "risk_score": h.risk_score,
            "churn": h.churn_count,
            "complexity": h.max_complexity
        })).collect::<Vec<_>>());
    }

    state.cached_evolution = Some(report);

    Ok(result)
}

// ══════════════════════════════════════════════════════════════════
//  DSM
// ══════════════════════════════════════════════════════════════════

pub fn dsm_def() -> ToolDef {
    ToolDef {
        name: "dsm",
        description: "Get the Design Structure Matrix: NxN dependency matrix showing file relationships, clusters, and inversions.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "format": { "type": "string", "description": "Output format: 'text' for ASCII matrix, 'stats' for summary statistics (default: stats)" }
            }
        }),
        min_tier: Tier::Free,
        handler: handle_dsm,
        invalidates_evolution: false,
    }
}

fn handle_dsm(args: &Value, tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let snap = state.cached_snapshot.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let dsm = crate::metrics::dsm::build_dsm(&snap.import_graph);
    let stats = crate::metrics::dsm::compute_stats(&dsm);

    let mut result = json!({
        "size": stats.size,
        "edge_count": stats.edge_count,
        "density": (stats.density * 10000.0).round() as u32,
        "above_diagonal": stats.above_diagonal,
        "below_diagonal": stats.below_diagonal,
        "same_level": stats.same_level,
        "propagation_cost": (stats.propagation_cost * 10000.0).round() as u32,
        "level_breaks": dsm.level_breaks.len(),
        "interpretation": if stats.above_diagonal == 0 {
            "Clean layering: all dependencies flow downward"
        } else if stats.above_diagonal as f64 / stats.edge_count.max(1) as f64 > 0.2 {
            "Significant architectural inversions detected"
        } else {
            "Mostly clean layering with minor inversions"
        }
    });

    // Pro: full matrix text and cluster file lists. Free: summary stats only.
    if tier.is_pro() {
        let format = args.get("format").and_then(|f| f.as_str()).unwrap_or("stats");
        if format == "text" {
            result["matrix"] = json!(crate::metrics::dsm::render_text(&dsm, 30));
        }
        result["clusters"] = json!(stats.clusters.iter().take(5).map(|c| json!({
            "level": c.level, "files": c.files.len(),
            "internal_edges": c.internal_edges,
            "file_list": c.files.iter().take(10).collect::<Vec<_>>()
        })).collect::<Vec<_>>());
    } else {
        result["clusters"] = json!(stats.clusters.iter().take(5).map(|c| json!({
            "level": c.level, "files_count": c.files.len(),
            "internal_edges": c.internal_edges
        })).collect::<Vec<_>>());
    }

    Ok(result)
}

// ══════════════════════════════════════════════════════════════════
//  TEST GAPS (free: top-3, pro: full)
// ══════════════════════════════════════════════════════════════════

pub fn test_gaps_def() -> ToolDef {
    ToolDef {
        name: "test_gaps",
        description: "Find high-risk source files with zero test coverage. Cross-references test file detection with import graph and complexity.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "Top-N riskiest untested files (default 20)" }
            }
        }),
        min_tier: Tier::Free,
        handler: handle_test_gaps,
        invalidates_evolution: false,
    }
}

fn handle_test_gaps(args: &Value, tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let snap = state.cached_snapshot.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let complexity = build_complexity_map(snap);
    let report = crate::metrics::testgap::compute_test_gaps(snap, &complexity);

    let mut result = json!({
        "coverage_score": report.coverage_score,
        "source_files": report.source_files,
        "test_files": report.test_files,
        "tested": report.tested_source_files,
        "untested": report.untested_source_files,
        "coverage_ratio": (report.coverage_ratio * 10000.0).round() as u32
    });

    // Pro: file-level gap details. Free: scores + counts only.
    if tier.is_pro() {
        let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize;
        result["riskiest_untested"] = json!(report.gaps.iter().take(limit).map(|g| json!({
            "file": g.file, "risk_score": g.risk_score,
            "complexity": g.max_complexity, "fan_in": g.fan_in, "lang": g.lang
        })).collect::<Vec<_>>());
        result["test_files_detail"] = json!(report.test_coverage.iter().take(10).map(|tc| json!({
            "test": tc.test_file, "covers": tc.covers
        })).collect::<Vec<_>>());
    }

    Ok(result)
}
