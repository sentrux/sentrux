//! MCP tool handlers for evolution, churn, bus factor, coupling history,
//! what-if simulation, DSM, and test gap analysis.
//!
//! Same uniform signature as handlers.rs: `fn(&Value, &Tier, &mut McpState) -> Result<Value, String>`

use crate::core::snapshot::{self, Snapshot};
use crate::core::types::{FileNode, ImportEdge};
use crate::metrics::evolution;
use crate::metrics::testgap;
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

// ══════════════════════════════════════════════════════════════════
//  FILE INFO (Pro: per-file detailed analysis)
// ══════════════════════════════════════════════════════════════════

pub fn file_info_def() -> ToolDef {
    ToolDef {
        name: "file_info",
        description: "Per-file deep dive — the most granular view of a single file. Shows: function-level complexity, coupling (fan-in/out, instability), architecture position (level, blast radius), git history (churn, authors, code age), and all detected issues. Git history fields are available when git_stats has been called.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative file path as shown by scan (e.g. 'src/app.rs')" }
            },
            "required": ["path"]
        }),
        min_tier: Tier::Pro,
        handler: handle_file_info,
        invalidates_evolution: false,
    }
}

fn handle_file_info(args: &Value, _tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let snap = state.cached_snapshot.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let h = state.cached_health.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let a = state.cached_arch.as_ref().ok_or("No scan data. Call 'scan' first.")?;

    let raw_path = args.get("path").and_then(|p| p.as_str())
        .ok_or("Missing 'path' argument")?;
    let path = raw_path.trim().trim_start_matches("./");

    // ── 1. Find the FileNode ──
    let all_files = snapshot::flatten_files_ref(&snap.root);
    let node = all_files.iter()
        .find(|n| n.path == path || n.path.ends_with(&format!("/{path}")))
        .ok_or_else(|| format!("File '{path}' not found in scan. Check the path is relative to the scanned root."))?;

    // ── 2. Functions with per-function metrics ──
    let mut functions_json: Vec<Value> = Vec::new();
    if let Some(sa) = &node.sa {
        if let Some(funcs) = &sa.functions {
            let mut sorted: Vec<&crate::core::types::FuncInfo> = funcs.iter().collect();
            // Sort by CC descending so worst functions come first
            sorted.sort_by(|a, b| {
                b.cc.unwrap_or(0).cmp(&a.cc.unwrap_or(0))
            });
            for f in &sorted {
                functions_json.push(json!({
                    "name": f.n,
                    "lines": f.ln,
                    "cc": f.cc,
                    "cognitive": f.cog,
                    "params": f.pc,
                    "is_public": f.is_public
                }));
            }
        }
    }

    // ── 3. Coupling from import_graph + call_graph ──
    // Filter mod-declaration edges and deduplicate import+call edges,
    // consistent with compute_fan_maps() in metrics/mod.rs.
    let mut imports: Vec<&str> = Vec::new();
    let mut imported_by: Vec<&str> = Vec::new();
    let mut seen_edges: HashSet<(&str, &str)> = HashSet::new();
    for edge in snap.import_graph.iter()
        .filter(|e| !crate::metrics::types::is_mod_declaration_edge(e))
    {
        if !seen_edges.insert((edge.from_file.as_str(), edge.to_file.as_str())) {
            continue;
        }
        if edge.from_file == path {
            imports.push(&edge.to_file);
        }
        if edge.to_file == path {
            imported_by.push(&edge.from_file);
        }
    }
    for edge in &snap.call_graph {
        if !seen_edges.insert((edge.from_file.as_str(), edge.to_file.as_str())) {
            continue;
        }
        if edge.from_file == path {
            imports.push(&edge.to_file);
        }
        if edge.to_file == path {
            imported_by.push(&edge.from_file);
        }
    }
    let fan_in = imported_by.len();
    let fan_out = imports.len();
    let instability = if fan_in + fan_out > 0 {
        fan_out as f64 / (fan_in + fan_out) as f64
    } else {
        0.0
    };

    // ── 4. Issues from HealthReport ──
    let is_god_file = h.god_files.iter().any(|f| f.path == path);
    let is_hotspot = h.hotspot_files.iter().any(|f| f.path == path);
    let in_cycle = h.circular_dep_files.iter().any(|cycle| cycle.contains(&path.to_string()));
    let is_large_file = h.long_files.iter().any(|f| f.path == path);
    let complex_fns = h.complex_functions.iter().filter(|f| f.file == path).count();
    let long_fns = h.long_functions.iter().filter(|f| f.file == path).count();
    let cog_complex_fns = h.cog_complex_functions.iter().filter(|f| f.file == path).count();
    let high_param_fns = h.high_param_functions.iter().filter(|f| f.file == path).count();
    let dead_fns = h.dead_functions.iter().filter(|f| f.file == path).count();
    let duplicate_fns = h.duplicate_groups.iter()
        .flat_map(|g| &g.instances)
        .filter(|(f, _, _)| f == path)
        .count();

    // ── 5. Build issues summary ──
    let mut issue_parts: Vec<String> = Vec::new();
    if is_god_file { issue_parts.push("god file (high fan-out)".into()); }
    if is_hotspot { issue_parts.push("hotspot (high fan-in + unstable)".into()); }
    if is_large_file { issue_parts.push(format!("large file (>{} lines)", node.lines)); }
    if in_cycle { issue_parts.push("in circular dependency".into()); }
    if complex_fns > 0 { issue_parts.push(format!("{complex_fns} complex function(s)")); }
    if long_fns > 0 { issue_parts.push(format!("{long_fns} long function(s)")); }
    if cog_complex_fns > 0 { issue_parts.push(format!("{cog_complex_fns} cognitively complex function(s)")); }
    if high_param_fns > 0 { issue_parts.push(format!("{high_param_fns} high-param function(s)")); }
    if dead_fns > 0 { issue_parts.push(format!("{dead_fns} dead function(s)")); }
    if duplicate_fns > 0 { issue_parts.push(format!("{duplicate_fns} duplicate function(s)")); }

    let summary = if issue_parts.is_empty() {
        "No issues detected.".to_string()
    } else {
        format!("{} issue(s): {}", issue_parts.len(), issue_parts.join(", "))
    };

    // ── 6. Assemble result ──
    let is_test = testgap::is_test_file(path);
    let is_entry_point = snap.entry_points.iter().any(|ep| ep.file == path);

    let mut result = json!({
        "path": path,
        "lang": node.lang,
        "is_test": is_test,
        "is_entry_point": is_entry_point,
        "lines": {
            "total": node.lines,
            "logic": node.logic,
            "comments": node.comments,
            "blanks": node.blanks
        },
        "functions": {
            "count": node.funcs,
            "details": functions_json
        },
        "dependencies": {
            "fan_in": fan_in,
            "fan_out": fan_out,
            "instability": (instability * 10000.0).round() as u32,
            "imports": imports,
            "imported_by": imported_by
        },
        "architecture": {
            "level": a.levels.get(path),
            "max_level": a.max_level,
            "blast_radius": a.blast_radius.get(path),
            "exec_depth": snap.exec_depth.get(path)
        },
        "issues": {
            "is_god_file": is_god_file,
            "is_hotspot": is_hotspot,
            "is_large_file": is_large_file,
            "in_cycle": in_cycle,
            "complex_functions": complex_fns,
            "long_functions": long_fns,
            "cog_complex_functions": cog_complex_fns,
            "high_param_functions": high_param_fns,
            "dead_functions": dead_fns,
            "duplicate_functions": duplicate_fns
        },
        "summary": summary
    });

    // ── 7. Git history (if evolution data was computed via git_stats) ──
    if let Some(evo) = state.cached_evolution.as_ref() {
        let mut git = json!({});
        if let Some(churn) = evo.churn.get(path) {
            git["churn"] = json!({
                "commits": churn.commit_count,
                "lines_added": churn.lines_added,
                "lines_removed": churn.lines_removed,
                "total_churn": churn.total_churn
            });
        }
        if let Some(&age) = evo.code_age.get(path) {
            git["code_age_days"] = json!(age);
        }
        if let Some(author) = evo.authors.get(path) {
            git["authors"] = json!({
                "count": author.author_count,
                "primary": author.primary_author,
                "primary_ratio": (author.primary_ratio * 10000.0).round() as u32,
                "all": author.authors
            });
        }
        if let Some(hotspot) = evo.hotspots.iter().find(|h| h.file == path) {
            git["temporal_hotspot"] = json!({
                "risk_score": hotspot.risk_score,
                "churn": hotspot.churn_count,
                "complexity": hotspot.max_complexity
            });
        }
        // Coupling pairs involving this file
        let coupling: Vec<Value> = evo.coupling_pairs.iter()
            .filter(|cp| cp.file_a == path || cp.file_b == path)
            .map(|cp| {
                let other = if cp.file_a == path { &cp.file_b } else { &cp.file_a };
                json!({
                    "coupled_with": other,
                    "co_changes": cp.co_change_count,
                    "strength": (cp.coupling_strength * 10000.0).round() as u32
                })
            })
            .collect();
        if !coupling.is_empty() {
            git["change_coupling"] = json!(coupling);
        }
        result["git_history"] = git;
    }

    Ok(result)
}

// ══════════════════════════════════════════════════════════════════
//  TOP FILES — Ranked file listing by metric
// ══════════════════════════════════════════════════════════════════

pub fn top_files_def() -> ToolDef {
    ToolDef {
        name: "top_files",
        description: "Ranked file listing by a chosen metric — the middle layer between 'health' (project-level) and 'file_info' (single file). Unlike health's per-root-cause lists, this provides a unified cross-metric ranking. Metrics: 'coupling' (fan-in + fan-out), 'fan_in', 'fan_out', 'complexity' (max cyclomatic), 'cognitive' (max cognitive), 'instability', 'blast_radius', 'churn', 'risk' (churn × complexity × coupling composite). Churn/risk metrics are more accurate when git_stats has been called.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "metric": {
                    "type": "string",
                    "description": "Metric to rank by: coupling, fan_in, fan_out, complexity, cognitive, instability, blast_radius, churn, risk (default: risk)",
                    "enum": ["coupling", "fan_in", "fan_out", "complexity", "cognitive", "instability", "blast_radius", "churn", "risk"]
                },
                "limit": { "type": "integer", "description": "Number of files to return (default 10, max 50)" }
            }
        }),
        min_tier: Tier::Pro,
        handler: handle_top_files,
        invalidates_evolution: false,
    }
}

/// Per-file aggregated data for ranking.
struct FileRankData {
    path: String,
    fan_in: usize,
    fan_out: usize,
    max_cc: u32,
    max_cog: u32,
    instability: f64,
    blast_radius: u32,
    churn: u32,
    lines: u32,
    risk_score: f64,
}

fn handle_top_files(args: &Value, _tier: &Tier, state: &mut McpState) -> Result<Value, String> {
    let snap = state.cached_snapshot.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let health = state.cached_health.as_ref().ok_or("No scan data. Call 'scan' first.")?;
    let arch = state.cached_arch.as_ref().ok_or("No scan data. Call 'scan' first.")?;

    let metric = args.get("metric").and_then(|m| m.as_str()).unwrap_or("risk");
    let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10).min(50) as usize;

    // Reuse the canonical fan-map computation from metrics (filters mod-declaration
    // edges, deduplicates import+call edges). Keyed by owned String.
    let dep_edges: Vec<ImportEdge> = snap.import_graph.iter()
        .filter(|e| !crate::metrics::types::is_mod_declaration_edge(e))
        .cloned()
        .collect();
    let (fan_out_map, fan_in_map) = crate::metrics::compute_fan_maps(&dep_edges, &snap.call_graph);

    // Build per-file data
    let all_files = snapshot::flatten_files_ref(&snap.root);
    let churn_map = state.cached_evolution.as_ref().map(|e| &e.churn);

    let mut ranked: Vec<FileRankData> = all_files.iter()
        .filter(|f| !f.lang.is_empty() && f.lang != "unknown")
        .filter(|f| !testgap::is_test_file(&f.path))
        .map(|f| {
            let fi = fan_in_map.get(f.path.as_str()).copied().unwrap_or(0);
            let fo = fan_out_map.get(f.path.as_str()).copied().unwrap_or(0);
            let total = fi + fo;
            let inst = if total > 0 { fo as f64 / total as f64 } else { 0.0 };
            let br = arch.blast_radius.get(&f.path).copied().unwrap_or(0);

            let (max_cc, max_cog) = f.sa.as_ref()
                .and_then(|sa| sa.functions.as_ref())
                .map(|funcs| {
                    let cc = funcs.iter().filter_map(|func| func.cc).max().unwrap_or(1);
                    let cog = funcs.iter().filter_map(|func| func.cog).max().unwrap_or(0);
                    (cc, cog)
                })
                .unwrap_or((1, 0));

            let churn = churn_map
                .and_then(|cm| cm.get(&f.path))
                .map(|c| c.commit_count)
                .unwrap_or(0);

            // Risk composite: structural reach × complexity × change frequency.
            // Uses max(blast_radius, coupling) so high-reach files rank even with zero churn.
            // Churn floor of 1.0 ensures structural-only risk is never zeroed out.
            let reach = (br as f64).max((fi + fo) as f64);
            let complexity_factor = max_cc as f64;
            let churn_factor = (churn as f64).max(1.0);
            let risk_score = churn_factor * complexity_factor * reach;

            FileRankData {
                path: f.path.clone(),
                fan_in: fi,
                fan_out: fo,
                max_cc,
                max_cog,
                instability: inst,
                blast_radius: br,
                churn,
                lines: f.lines,
                risk_score,
            }
        })
        .collect();

    // Sort by selected metric (descending = worst first), break ties alphabetically
    // for deterministic output.
    let tiebreak = |a: &FileRankData, b: &FileRankData| a.path.cmp(&b.path);
    match metric {
        "coupling" => ranked.sort_by(|a, b| (b.fan_in + b.fan_out).cmp(&(a.fan_in + a.fan_out)).then_with(|| tiebreak(a, b))),
        "fan_in" => ranked.sort_by(|a, b| b.fan_in.cmp(&a.fan_in).then_with(|| tiebreak(a, b))),
        "fan_out" => ranked.sort_by(|a, b| b.fan_out.cmp(&a.fan_out).then_with(|| tiebreak(a, b))),
        "complexity" => ranked.sort_by(|a, b| b.max_cc.cmp(&a.max_cc).then_with(|| tiebreak(a, b))),
        "cognitive" => ranked.sort_by(|a, b| b.max_cog.cmp(&a.max_cog).then_with(|| tiebreak(a, b))),
        "instability" => ranked.sort_by(|a, b| b.instability.partial_cmp(&a.instability).unwrap_or(std::cmp::Ordering::Equal).then_with(|| tiebreak(a, b))),
        "blast_radius" => ranked.sort_by(|a, b| b.blast_radius.cmp(&a.blast_radius).then_with(|| tiebreak(a, b))),
        "churn" => ranked.sort_by(|a, b| b.churn.cmp(&a.churn).then_with(|| tiebreak(a, b))),
        "risk" | _ => ranked.sort_by(|a, b| b.risk_score.partial_cmp(&a.risk_score).unwrap_or(std::cmp::Ordering::Equal).then_with(|| tiebreak(a, b))),
    }

    ranked.truncate(limit);

    let has_churn = churn_map.is_some();

    let files_json: Vec<Value> = ranked.iter().enumerate().map(|(i, f)| {
        let mut entry = json!({
            "rank": i + 1,
            "path": f.path,
            "fan_in": f.fan_in,
            "fan_out": f.fan_out,
            "coupling": f.fan_in + f.fan_out,
            "max_cc": f.max_cc,
            "max_cognitive": f.max_cog,
            "instability": (f.instability * 10000.0).round() as u32,
            "blast_radius": f.blast_radius,
            "lines": f.lines
        });
        if has_churn {
            entry["churn"] = json!(f.churn);
            entry["risk_score"] = json!((f.risk_score * 100.0).round() as u64);
        }
        entry
    }).collect();

    let mut result = json!({
        "metric": metric,
        "total_source_files": all_files.iter().filter(|f| !f.lang.is_empty() && f.lang != "unknown" && !testgap::is_test_file(&f.path)).count(),
        "showing": ranked.len(),
        "files": files_json
    });

    if metric == "risk" && !has_churn {
        result["note"] = json!("Risk scores use churn=1 (default). Run git_stats first for accurate churn-weighted risk.");
    }
    if metric == "churn" && !has_churn {
        result["note"] = json!("No git history loaded. Run git_stats first to get churn data.");
    }

    // Cross-reference with health report issues
    let issues_in_top: usize = ranked.iter().filter(|f| {
        health.god_files.iter().any(|g| g.path == f.path)
        || health.hotspot_files.iter().any(|h| h.path == f.path)
        || health.circular_dep_files.iter().any(|cycle| cycle.contains(&f.path))
    }).count();
    result["issues_overlap"] = json!({
        "files_with_health_issues": issues_in_top,
        "out_of": ranked.len()
    });

    Ok(result)
}
