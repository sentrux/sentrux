//! Code health metrics (Constantine & Yourdon 1979, McCabe 1976, Martin).
//!
//! Top-level module that orchestrates all metric computations: structural
//! coupling, cyclic dependencies, god-file detection, cyclomatic complexity,
//! and overall quality signal scoring. Sub-modules provide architecture
//! analysis, DSM construction, evolutionary metrics, rule enforcement,
//! test-gap analysis, and what-if scenario simulation.
//! Key function: `compute_health` produces a `HealthReport` from a `Snapshot`.

// ── Sub-modules (directory modules with internal cohesion) ──
pub mod arch;     // arch/mod.rs + graph.rs + distance.rs
pub mod evo;      // evo/mod.rs + git_walker.rs
pub mod rules;    // rules/mod.rs + checks.rs

// ── Flat modules (remain at metrics root) ──
pub mod cross_validation; // FREE: compression-based quality cross-check
pub mod dsm;
pub mod root_causes;
pub mod stability;
pub mod testgap;
pub mod types;
pub mod whatif;

pub use types::*;

// ── Re-exports for backward compatibility ──
// External code (app/mcp_handlers_evo.rs) imports crate::metrics::evolution.
// After restructure, evolution lives in crate::metrics::evo.
pub use evo as evolution;

#[cfg(test)]
pub(crate) mod test_helpers;
#[cfg(test)]
mod mod_tests;
#[cfg(test)]
mod mod_tests2;

use stability::{
    compute_avg_cohesion, compute_coupling_score, compute_shannon_entropy,
    compute_stable_modules,
};
// Threshold constants are no longer imported — thresholds are now per-language,
// read from the LanguageProfile at runtime via lang_registry::profile().
use crate::core::types::{EntryPoint, ImportEdge};
use crate::core::snapshot::Snapshot;
use crate::core::types::FileNode;
use std::collections::{HashMap, HashSet};

/// Check if a file is a package-index / barrel file via its language profile.
/// Now reads from plugin.toml [semantics] package_index_files per language,
/// instead of a single hardcoded list.
pub(crate) fn is_package_index_for_path(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("");
    let lang = crate::analysis::lang_registry::detect_lang_from_ext(ext);
    crate::analysis::lang_registry::profile(&lang).is_package_index_file(path)
}

/// Compute per-file fan-out and fan-in counts from import + call edges.
/// Both represent real dependencies — import is explicit, call is implicit.
/// Deduplicated: A→B via import AND call counts as 1 edge.
fn compute_fan_maps(import_edges: &[ImportEdge], call_edges: &[crate::core::types::CallEdge]) -> (HashMap<String, usize>, HashMap<String, usize>) {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut fan_out: HashMap<String, usize> = HashMap::new();
    let mut fan_in: HashMap<String, usize> = HashMap::new();
    for edge in import_edges {
        if seen.insert((edge.from_file.clone(), edge.to_file.clone())) {
            *fan_out.entry(edge.from_file.clone()).or_default() += 1;
            *fan_in.entry(edge.to_file.clone()).or_default() += 1;
        }
    }
    for edge in call_edges {
        if seen.insert((edge.from_file.clone(), edge.to_file.clone())) {
            *fan_out.entry(edge.from_file.clone()).or_default() += 1;
            *fan_in.entry(edge.to_file.clone()).or_default() += 1;
        }
    }
    (fan_out, fan_in)
}

/// Resolve file path to its language profile's fan-out threshold.
fn fan_out_threshold_for_path(path: &str) -> usize {
    let ext = path.rsplit('.').next().unwrap_or("");
    let lang = crate::analysis::lang_registry::detect_lang_from_ext(ext);
    crate::analysis::lang_registry::profile(&lang).thresholds.fan_out
}

/// Resolve file path to its language profile's fan-in threshold.
fn fan_in_threshold_for_path(path: &str) -> usize {
    let ext = path.rsplit('.').next().unwrap_or("");
    let lang = crate::analysis::lang_registry::detect_lang_from_ext(ext);
    crate::analysis::lang_registry::profile(&lang).thresholds.fan_in
}

/// Detect god files: files with fan-out exceeding per-language FAN_OUT threshold.
/// Entry-point files are excluded (they legitimately import many modules).
fn detect_god_files(
    fan_out: &HashMap<String, usize>,
    entry_points: &[EntryPoint],
) -> Vec<FileMetric> {
    let entry_file_set: HashSet<&str> = entry_points
        .iter()
        .map(|ep| ep.file.as_str())
        .collect();
    let mut v: Vec<FileMetric> = fan_out
        .iter()
        .filter(|(path, &count)| {
            let threshold = fan_out_threshold_for_path(path);
            count > threshold
                && !entry_file_set.contains(path.as_str())
                && !is_package_index_for_path(path)
        })
        .map(|(path, &count)| FileMetric { path: path.clone(), value: count })
        .collect();
    v.sort_unstable_by(|a, b| b.value.cmp(&a.value));
    v
}

/// Detect hotspot files: high fan-in files that are also unstable (I >= 0.15).
/// Stable foundations (high fan-in + low fan-out) are excluded per Martin's SDP.
/// Package-index files (__init__.py, index.js, mod.rs, etc.) are excluded because
/// their fan-in reflects barrel re-exports, not genuine coupling hotspots.
fn detect_hotspot_files(fan_in: &HashMap<String, usize>, fan_out: &HashMap<String, usize>) -> Vec<FileMetric> {
    let mut v: Vec<FileMetric> = fan_in
        .iter()
        .filter(|(path, &count)| {
            let threshold = fan_in_threshold_for_path(path);
            if count <= threshold { return false; }
            // Exclude package-index / barrel files — their high fan-in is an
            // artifact of re-exporting, not a design flaw.
            if is_package_index_for_path(path) { return false; }
            // Exclude stable foundations (I < 0.15): high fan-in + low fan-out
            // is GOOD architecture (Martin's SDP). Only flag unstable hotspots.
            let fo = *fan_out.get(path.as_str()).unwrap_or(&0);
            let instability = fo as f64 / (count + fo) as f64;
            instability >= 0.15
        })
        .map(|(path, &count)| FileMetric { path: path.clone(), value: count })
        .collect();
    v.sort_unstable_by(|a, b| b.value.cmp(&a.value));
    v
}

/// Compute per-file instability I = Ce/(Ca+Ce). Returns top 10 most unstable files.
fn compute_instability(
    import_edges: &[ImportEdge],
    fan_out: &HashMap<String, usize>,
    fan_in: &HashMap<String, usize>,
) -> Vec<InstabilityMetric> {
    let mut all_files: HashSet<&str> = HashSet::new();
    for edge in import_edges {
        all_files.insert(edge.from_file.as_str());
        all_files.insert(edge.to_file.as_str());
    }
    let mut v: Vec<InstabilityMetric> = all_files
        .iter()
        .filter(|&&path| !testgap::is_test_file(path))
        .map(|&path| {
            let ce = *fan_out.get(path).unwrap_or(&0);
            let ca = *fan_in.get(path).unwrap_or(&0);
            let total = ca + ce;
            // Simple ratio: 0.5 when no data (neutral).
            let instability = if total == 0 { 0.5 } else { ce as f64 / total as f64 };
            InstabilityMetric {
                path: path.to_string(),
                instability,
                fan_in: ca,
                fan_out: ce,
            }
        })
        .collect();
    v.sort_unstable_by(|a, b| b.instability.partial_cmp(&a.instability).unwrap_or(std::cmp::Ordering::Equal));
    v.truncate(10);
    v
}

/// Collect functions exceeding a threshold on a given metric.
/// `extract_value` receives the file (for language-aware thresholds) and each function.
/// Returns Some(value) if the function exceeds the threshold.
fn collect_functions_exceeding(
    files: &[&FileNode],
    extract_value: impl Fn(&FileNode, &crate::core::types::FuncInfo) -> Option<u32>,
) -> Vec<FuncMetric> {
    let mut result = Vec::new();
    for file in files {
        let funcs = match file.sa.as_ref().and_then(|sa| sa.functions.as_ref()) {
            Some(f) => f,
            None => continue,
        };
        for f in funcs {
            if let Some(value) = extract_value(file, f) {
                result.push(FuncMetric {
                    file: file.path.clone(),
                    func: f.n.clone(),
                    value,
                });
            }
        }
    }
    result.sort_unstable_by(|a, b| b.value.cmp(&a.value));
    result
}

/// Collect complex functions and long functions from all files.
/// Thresholds are per-language: reads from the file's language profile.
fn collect_per_function_metrics(
    files: &[&FileNode],
) -> (Vec<FuncMetric>, Vec<FuncMetric>) {
    let complex_functions = collect_functions_exceeding(files, |file, f| {
        let threshold = crate::analysis::lang_registry::profile(&file.lang).thresholds.cc_high;
        f.cc.filter(|&cc| cc > threshold)
    });
    let long_functions = collect_functions_exceeding(files, |file, f| {
        let threshold = crate::analysis::lang_registry::profile(&file.lang).thresholds.func_length;
        if f.ln > threshold { Some(f.ln) } else { None }
    });
    (complex_functions, long_functions)
}

/// Collect ALL function cyclomatic complexities (unfiltered, for rules engine).
fn collect_all_function_ccs(files: &[&FileNode]) -> Vec<FuncMetric> {
    collect_functions_exceeding(files, |_file, f| f.cc)
}

/// Collect ALL function line counts (unfiltered, for rules engine).
fn collect_all_function_lines(files: &[&FileNode]) -> Vec<FuncMetric> {
    collect_functions_exceeding(files, |_file, f| Some(f.ln))
}

/// Collect ALL file line counts (unfiltered, for rules engine).
fn collect_all_file_lines(files: &[&FileNode]) -> Vec<FileMetric> {
    files.iter()
        .filter(|f| !f.lang.is_empty() && f.lang != "unknown")
        .map(|f| FileMetric { path: f.path.clone(), value: f.lines as usize })
        .collect()
}

/// Compute comment-to-total-lines ratio across all code files.
/// Returns None if there are no code files (no language detected).
fn compute_comment_ratio(files: &[&FileNode]) -> Option<f64> {
    let (total_comments, total_lines): (u64, u64) = files.iter()
        .filter(|f| !f.lang.is_empty() && f.lang != "unknown")
        .fold((0u64, 0u64), |(c, l), f| (c + f.comments as u64, l + f.lines as u64));
    if total_lines > 0 {
        Some(total_comments as f64 / total_lines as f64)
    } else { None }
}

/// Compute large file statistics: files exceeding per-language large_file_lines threshold.
/// Returns (long_files_list, count, ratio_vs_total_code_files).
fn compute_large_file_stats(
    files: &[&FileNode],
) -> (Vec<FileMetric>, usize, f64) {
    let long_files: Vec<FileMetric> = files.iter()
        .filter(|f| {
            if f.lang.is_empty() || f.lang == "unknown" { return false; }
            let threshold = crate::analysis::lang_registry::profile(&f.lang).thresholds.large_file_lines;
            f.lines > threshold
        })
        .map(|f| FileMetric { path: f.path.clone(), value: f.lines as usize })
        .collect();
    let large_file_count = long_files.len();
    let code_file_count = files.iter().filter(|f| !f.lang.is_empty() && f.lang != "unknown").count();
    let large_file_ratio = if code_file_count == 0 || large_file_count == 0 { 0.0 } else {
        large_file_count as f64 / code_file_count as f64
    };
    (long_files, large_file_count, large_file_ratio)
}

/// Collect functions with cognitive complexity > threshold (per-language).
fn collect_cog_complex_functions(files: &[&FileNode]) -> Vec<FuncMetric> {
    collect_functions_exceeding(files, |file, f| {
        let threshold = crate::analysis::lang_registry::profile(&file.lang).thresholds.cog_high;
        f.cog.filter(|&cog| cog > threshold)
    })
}

/// Collect functions with parameter count > threshold (per-language).
fn collect_high_param_functions(files: &[&FileNode]) -> Vec<FuncMetric> {
    collect_functions_exceeding(files, |file, f| {
        let threshold = crate::analysis::lang_registry::profile(&file.lang).thresholds.param_high;
        f.pc.filter(|&pc| pc > threshold)
    })
}

/// Collect body-hashed functions from a single file into the hash map.
fn collect_file_body_hashes(file: &FileNode, hash_map: &mut HashMap<u64, Vec<(String, String, u32)>>) {
    let funcs = match file.sa.as_ref().and_then(|sa| sa.functions.as_ref()) {
        Some(f) => f,
        None => return,
    };
    for f in funcs {
        if let Some(bh) = f.bh {
            if bh != 0 {
                hash_map.entry(bh).or_default().push((file.path.clone(), f.n.clone(), f.ln));
            }
        }
    }
}

/// Build a map from body hash to list of (file, func_name, line_count).
fn build_body_hash_map(files: &[&FileNode]) -> HashMap<u64, Vec<(String, String, u32)>> {
    let mut hash_map: HashMap<u64, Vec<(String, String, u32)>> = HashMap::new();
    for file in files {
        collect_file_body_hashes(file, &mut hash_map);
    }
    hash_map
}

/// Collect groups of functions with identical body hashes (duplicates).
fn collect_duplicate_groups(files: &[&FileNode]) -> Vec<DuplicateGroup> {
    let hash_map = build_body_hash_map(files);
    let mut groups: Vec<DuplicateGroup> = hash_map
        .into_iter()
        .filter(|(_, instances)| instances.len() > 1)
        .map(|(hash, instances)| DuplicateGroup { hash, instances })
        .collect();
    groups.sort_unstable_by(|a, b| b.instances.len().cmp(&a.instances.len()));
    groups
}

/// Insert a call name and its base name (after last `::`) into the call set.
fn insert_call_with_base(all_calls: &mut HashSet<String>, call: &str) {
    all_calls.insert(call.to_string());
    if let Some(base) = call.rsplit("::").next() {
        all_calls.insert(base.to_string());
    }
}

/// Insert all calls from a call list into the call set.
fn insert_calls_from_list(all_calls: &mut HashSet<String>, calls: &[String]) {
    for c in calls {
        insert_call_with_base(all_calls, c);
    }
}

/// Collect all calls from a file's structural analysis into the call set.
fn collect_file_calls(all_calls: &mut HashSet<String>, sa: &crate::core::types::StructuralAnalysis) {
    if let Some(co) = &sa.co {
        insert_calls_from_list(all_calls, co);
    }
    if let Some(funcs) = &sa.functions {
        for f in funcs {
            if let Some(co) = &f.co {
                insert_calls_from_list(all_calls, co);
            }
        }
    }
}

/// Build the set of all call targets across all files (both full and base names).
fn build_call_target_set(files: &[&FileNode]) -> HashSet<String> {
    let mut all_calls: HashSet<String> = HashSet::new();
    for file in files {
        if let Some(sa) = &file.sa {
            collect_file_calls(&mut all_calls, sa);
        }
    }
    all_calls
}

/// Default implicit entry points (lifecycle, framework, common patterns).
/// Used when plugin TOML doesn't specify language-specific ones.
const DEFAULT_IMPLICIT_ENTRY_POINTS: &[&str] = &[
    "main", "new", "default", "init", "setup", "teardown",
    "run", "start", "stop", "build", "configure", "register",
    "update", "draw", "render", "serialize", "deserialize",
];

/// Default test function prefixes.
const DEFAULT_TEST_PREFIXES: &[&str] = &["test_"];

/// Collect implicit entry points from all loaded language profiles.
/// Merges language-specific lists with universal defaults.
fn implicit_entry_points() -> HashSet<String> {
    let mut set: HashSet<String> = DEFAULT_IMPLICIT_ENTRY_POINTS.iter().map(|s| s.to_string()).collect();
    for profile in crate::analysis::lang_registry::all_profiles() {
        for ep in &profile.semantics.implicit_entry_points {
            set.insert(ep.clone());
        }
    }
    set
}

/// Check if a file should be skipped for dead-code analysis (test files).
/// Uses the language profile's test detection — no hardcoded suffixes.
fn is_dead_code_skip_file(file: &FileNode) -> bool {
    let profile = crate::analysis::lang_registry::profile(&file.lang);
    if profile.is_test_file(&file.path) {
        return true;
    }
    // Generic fallback: path contains "test" somewhere
    if file.path.contains("test") || file.path.contains("/tests/") {
        return true;
    }
    if let Some(sa) = &file.sa {
        if let Some(tags) = &sa.tags {
            if tags.iter().any(|t| t.contains("test")) {
                return true;
            }
        }
    }
    false
}

/// Check if a function should be excluded from dead-code detection.
/// Uses test_function_prefixes and qualified_name_separator from plugin TOML.
fn is_excluded_function(func_name: &str, implicit: &HashSet<String>, lang: &str) -> bool {
    let profile = crate::analysis::lang_registry::profile(lang);
    let sem = &profile.semantics;

    // Skip test/bench functions using configured prefixes
    let test_prefixes = if sem.test_function_prefixes.is_empty() {
        DEFAULT_TEST_PREFIXES.iter().map(|s| s.to_string()).collect::<Vec<_>>()
    } else {
        sem.test_function_prefixes.clone()
    };
    for prefix in &test_prefixes {
        if func_name.starts_with(prefix.as_str()) {
            return true;
        }
    }

    // Skip qualified names (trait impl methods like Foo::bar or obj.method)
    if !sem.qualified_name_separator.is_empty() && func_name.contains(&sem.qualified_name_separator) {
        return true;
    }

    // Skip implicit entry points
    let sep = if sem.qualified_name_separator.is_empty() { "::" } else { &sem.qualified_name_separator };
    let base_name = func_name.rsplit(sep).next().unwrap_or(func_name);
    implicit.contains(base_name)
}

/// Check if a function is referenced by any call site.
fn is_called(func_name: &str, all_calls: &HashSet<String>) -> bool {
    let base_name = func_name.rsplit("::").next().unwrap_or(func_name);
    all_calls.contains(func_name) || all_calls.contains(base_name)
}

/// Collect functions not referenced by any call site (dead code candidates).
fn collect_dead_functions(files: &[&FileNode]) -> Vec<FuncMetric> {
    let all_calls = build_call_target_set(files);
    let implicit = implicit_entry_points();

    let mut result = Vec::new();
    for file in files {
        if is_dead_code_skip_file(file) { continue; }
        let funcs = match file.sa.as_ref().and_then(|sa| sa.functions.as_ref()) {
            Some(f) => f,
            None => continue,
        };
        for f in funcs {
            // Public/exported functions are NOT dead code — they're API surface.
            // Methods (self/this) are called via object dispatch — can't trace statically.
            if f.is_public || f.is_method { continue; }
            if is_excluded_function(&f.n, &implicit, &file.lang) { continue; }
            if !is_called(&f.n, &all_calls) {
                result.push(FuncMetric {
                    file: file.path.clone(),
                    func: f.n.clone(),
                    value: f.ln,
                });
            }
        }
    }
    result.sort_unstable_by(|a, b| b.value.cmp(&a.value));
    result
}

/// Simple ratio: count / total, or 0.0 if total == 0 or count == 0.
fn simple_ratio(count: usize, total: usize) -> f64 {
    if total == 0 || count == 0 { return 0.0; }
    count as f64 / total as f64
}

/// Count total functions across all files.
fn count_total_funcs(files: &[&FileNode]) -> usize {
    files.iter()
        .filter_map(|f| f.sa.as_ref())
        .filter_map(|sa| sa.functions.as_ref())
        .map(|fns| fns.len())
        .sum()
}

/// Aggregate all per-file metrics into a FileMetrics result.
/// Combines fan maps, god/hotspot detection, instability, complexity, and ratios.
fn compute_file_metrics(
    files: &[&FileNode],
    import_edges: &[ImportEdge],
    call_edges: &[crate::core::types::CallEdge],
    entry_points: &[EntryPoint],
) -> FileMetrics {
    let (fan_out, fan_in) = compute_fan_maps(import_edges, call_edges);
    let god_files = detect_god_files(&fan_out, entry_points);
    let hotspot_files = detect_hotspot_files(&fan_in, &fan_out);
    let most_unstable = compute_instability(import_edges, &fan_out, &fan_in);
    let (complex_functions, long_functions) = collect_per_function_metrics(files);
    let cog_complex_functions = collect_cog_complex_functions(files);
    let high_param_functions = collect_high_param_functions(files);
    let duplicate_groups = collect_duplicate_groups(files);
    let dead_functions = collect_dead_functions(files);

    let total_funcs = count_total_funcs(files);
    let complex_fn_ratio = simple_ratio(complex_functions.len(), total_funcs);
    let long_fn_ratio = simple_ratio(long_functions.len(), total_funcs);
    let cog_complex_ratio = simple_ratio(cog_complex_functions.len(), total_funcs);
    let high_param_ratio = simple_ratio(high_param_functions.len(), total_funcs);
    let dup_func_count: usize = duplicate_groups.iter().map(|g| g.instances.len()).sum();
    let duplication_ratio = simple_ratio(dup_func_count, total_funcs);
    let dead_code_ratio = simple_ratio(dead_functions.len(), total_funcs);

    let comment_ratio = compute_comment_ratio(files);
    let (long_files, large_file_count, large_file_ratio) = compute_large_file_stats(files);

    let code_file_count = files.iter().filter(|f| !f.lang.is_empty() && f.lang != "unknown").count();
    let god_ratio = simple_ratio(god_files.len(), code_file_count);
    let hotspot_ratio = simple_ratio(hotspot_files.len(), code_file_count);

    FileMetrics {
        fan_out, fan_in, god_files, hotspot_files, most_unstable,
        complex_functions, long_functions, long_files,
        complex_fn_ratio, long_fn_ratio, comment_ratio,
        large_file_count, large_file_ratio, god_ratio, hotspot_ratio,
        cog_complex_functions, high_param_functions, duplicate_groups, dead_functions,
        duplication_ratio, dead_code_ratio, high_param_ratio, cog_complex_ratio,
    }
}

/// Module-level structural metrics: coupling, entropy, cohesion, depth, cycles.
fn compute_module_metrics(
    files: &[&FileNode],
    import_edges: &[ImportEdge],
    call_edges: &[crate::core::types::CallEdge],
    entry_points: &[EntryPoint],
) -> ModuleMetrics {
    let dep_edges = import_edges;

    let stable_modules = compute_stable_modules(dep_edges);
    let (coupling_score, cross_module_edges, _) =
        compute_coupling_score(dep_edges, &stable_modules);
    let (entropy_raw, entropy_bits, entropy_num_pairs) = compute_shannon_entropy(dep_edges, &stable_modules);
    // Scale entropy by coupling: low coupling means few cross-module edges,
    // so entropy of their distribution is less meaningful. Use B-threshold (0.35)
    // as denominator for gradual dampening instead of binary cutoff at A-threshold.
    let magnitude = (coupling_score / 0.35).min(1.0);
    let entropy = entropy_raw * magnitude;

    let avg_cohesion = compute_avg_cohesion(dep_edges, call_edges, files);
    let max_depth = compute_max_depth(dep_edges, entry_points);
    let circular_dep_files = detect_cycles(dep_edges);
    let circular_dep_paths = extract_cycle_paths(&circular_dep_files, dep_edges);
    let circular_dep_count = circular_dep_files.len();

    ModuleMetrics {
        coupling_score, cross_module_edges, entropy, entropy_bits, entropy_num_pairs,
        avg_cohesion, max_depth, circular_dep_files, circular_dep_paths, circular_dep_count,
    }
}

/// Compute a comprehensive code health report from a scan snapshot.
/// Evaluates coupling, complexity, dead code, duplication, and more.
/// Quality signal is derived from root causes (modularity, cycles, depth,
/// complexity equality, redundancy).
pub fn compute_health(snapshot: &Snapshot) -> HealthReport {
    let files = crate::core::snapshot::flatten_files_ref(&snapshot.root);
    // Filter mod-declaration edges once at the top. `pub mod foo;` is structural
    // containment, not a functional dependency — consistent across ALL metrics.
    let dep_edges: Vec<ImportEdge> = snapshot.import_graph.iter()
        .filter(|e| !is_mod_declaration_edge(e))
        .cloned()
        .collect();

    let fm = compute_file_metrics(&files, &dep_edges, &snapshot.call_graph, &snapshot.entry_points);
    let mm = compute_module_metrics(&files, &dep_edges, &snapshot.call_graph, &snapshot.entry_points);

    // Raw unfiltered data for rules engine (user thresholds may be stricter than hardcoded ones)
    let all_function_ccs = collect_all_function_ccs(&files);
    let all_function_lines = collect_all_function_lines(&files);
    let all_file_lines = collect_all_file_lines(&files);

    // ── Root cause metrics (6 fundamental structural properties) ──
    let modularity_q = root_causes::compute_modularity_q(&dep_edges, &snapshot.call_graph, &files);
    let complexity_gini = root_causes::compute_complexity_gini(&files);
    let dup_func_count: usize = fm.duplicate_groups.iter().map(|g| g.instances.len()).sum();
    let total_funcs = count_total_funcs(&files);
    let redundancy_ratio = root_causes::compute_redundancy_ratio(
        fm.dead_functions.len(), dup_func_count, total_funcs,
    );

    let root_cause_raw = root_causes::RootCauseRaw {
        modularity_q,
        cycle_count: mm.circular_dep_count,
        max_depth: mm.max_depth,
        complexity_gini,
        redundancy_ratio,
    };
    let (root_cause_scores, quality_signal) = root_causes::compute_root_cause_scores(&root_cause_raw);

    HealthReport {
        coupling_score: mm.coupling_score,
        circular_dep_count: mm.circular_dep_count,
        circular_dep_files: mm.circular_dep_files,
        circular_dep_paths: mm.circular_dep_paths,
        total_import_edges: dep_edges.len(),
        cross_module_edges: mm.cross_module_edges,
        entropy: mm.entropy,
        entropy_bits: mm.entropy_bits,
        avg_cohesion: mm.avg_cohesion,
        max_depth: mm.max_depth,
        god_files: fm.god_files,
        hotspot_files: fm.hotspot_files,
        most_unstable: fm.most_unstable,
        complex_functions: fm.complex_functions,
        long_functions: fm.long_functions,
        cog_complex_functions: fm.cog_complex_functions,
        high_param_functions: fm.high_param_functions,
        duplicate_groups: fm.duplicate_groups,
        dead_functions: fm.dead_functions,
        long_files: fm.long_files,
        all_function_ccs,
        all_function_lines,
        all_file_lines,
        god_file_ratio: fm.god_ratio,
        hotspot_ratio: fm.hotspot_ratio,
        complex_fn_ratio: fm.complex_fn_ratio,
        long_fn_ratio: fm.long_fn_ratio,
        comment_ratio: fm.comment_ratio,
        large_file_count: fm.large_file_count,
        large_file_ratio: fm.large_file_ratio,
        duplication_ratio: fm.duplication_ratio,
        dead_code_ratio: fm.dead_code_ratio,
        high_param_ratio: fm.high_param_ratio,
        cog_complex_ratio: fm.cog_complex_ratio,
        quality_signal,
        root_cause_raw,
        root_cause_scores,
    }
}

/// Build forward adjacency list from import edges. Returns (nodes, adjacency_map).
fn build_adjacency_list(edges: &[ImportEdge]) -> (HashSet<&str>, HashMap<&str, Vec<&str>>) {
    let mut nodes: HashSet<&str> = HashSet::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        nodes.insert(edge.from_file.as_str());
        nodes.insert(edge.to_file.as_str());
        adj.entry(edge.from_file.as_str())
            .or_default()
            .push(edge.to_file.as_str());
    }
    (nodes, adj)
}

/// State for iterative Tarjan's SCC algorithm.
struct TarjanState<'a> {
    index_counter: u32,
    stack: Vec<&'a str>,
    on_stack: HashSet<&'a str>,
    index_map: HashMap<&'a str, u32>,
    lowlink: HashMap<&'a str, u32>,
    sccs: Vec<Vec<String>>,
}

impl<'a> TarjanState<'a> {
    fn new() -> Self {
        Self {
            index_counter: 0,
            stack: Vec::new(),
            on_stack: HashSet::new(),
            index_map: HashMap::new(),
            lowlink: HashMap::new(),
            sccs: Vec::new(),
        }
    }

    /// Initialize a new node: assign index, push onto stack.
    fn visit(&mut self, node: &'a str) {
        self.index_map.insert(node, self.index_counter);
        self.lowlink.insert(node, self.index_counter);
        self.index_counter += 1;
        self.stack.push(node);
        self.on_stack.insert(node);
    }

    /// Update lowlink for v when neighbor w is already on stack.
    fn update_lowlink(&mut self, v: &'a str, w: &'a str) {
        if self.on_stack.contains(w) {
            let w_idx = self.index_map[w];
            let v_low = self.lowlink.get_mut(v).unwrap();
            if w_idx < *v_low {
                *v_low = w_idx;
            }
        }
    }

    /// Pop an SCC rooted at `root` from the stack. Only keeps cycles (len > 1).
    fn pop_scc(&mut self, root: &str) {
        let mut scc = Vec::new();
        loop {
            let w = self.stack.pop().unwrap();
            self.on_stack.remove(w);
            scc.push(w.to_string());
            if w == root { break; }
        }
        if scc.len() > 1 {
            scc.sort_unstable();
            self.sccs.push(scc);
        }
    }

    /// Propagate lowlink from child to parent after DFS backtrack.
    fn propagate_lowlink(&mut self, parent: &'a str, child_low: u32) {
        let parent_low = self.lowlink.get_mut(parent).unwrap();
        if child_low < *parent_low {
            *parent_low = child_low;
        }
    }
}

/// Iterative Tarjan's SCC — returns only cycles (SCCs with >1 member).
fn tarjan_sccs<'a>(
    nodes: &HashSet<&'a str>,
    adj: &HashMap<&'a str, Vec<&'a str>>,
) -> Vec<Vec<String>> {
    let mut state = TarjanState::new();

    for &start in nodes {
        if state.index_map.contains_key(start) {
            continue;
        }

        state.visit(start);
        let mut dfs_stack: Vec<(&str, usize)> = vec![(start, 0)];

        while let Some((v, ni)) = dfs_stack.last_mut() {
            let neighbors = adj.get(*v).map(|n| n.as_slice()).unwrap_or(&[]);
            if *ni < neighbors.len() {
                let w = neighbors[*ni];
                *ni += 1;

                if !state.index_map.contains_key(w) {
                    state.visit(w);
                    dfs_stack.push((w, 0));
                } else {
                    state.update_lowlink(v, w);
                }
            } else {
                let v_node = *v;
                let v_low = state.lowlink[v_node];
                let v_idx = state.index_map[v_node];

                if v_low == v_idx {
                    state.pop_scc(v_node);
                }

                dfs_stack.pop();

                if let Some((parent, _)) = dfs_stack.last() {
                    state.propagate_lowlink(parent, v_low);
                }
            }
        }
    }

    state.sccs
}

fn detect_cycles(edges: &[ImportEdge]) -> Vec<Vec<String>> {
    let (nodes, adj) = build_adjacency_list(edges);
    tarjan_sccs(&nodes, &adj)
}

/// Extract one concrete cycle path from each SCC using DFS within the subgraph.
/// For each SCC, finds a path like ["A", "B", "C", "A"] showing the actual dependency chain.
fn extract_cycle_paths(sccs: &[Vec<String>], edges: &[ImportEdge]) -> Vec<Vec<String>> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        adj.entry(edge.from_file.as_str())
            .or_default()
            .push(edge.to_file.as_str());
    }

    let mut paths = Vec::new();
    for scc in sccs {
        if scc.len() < 2 {
            continue;
        }
        let scc_set: HashSet<&str> = scc.iter().map(|s| s.as_str()).collect();
        if let Some(path) = find_cycle_in_subgraph(&scc_set, &adj) {
            paths.push(path);
        }
    }
    paths
}

/// DFS within an SCC subgraph to find one concrete cycle, returned as
/// [start, ..., start] so the caller can see the full loop.
fn find_cycle_in_subgraph(
    scc: &HashSet<&str>,
    adj: &HashMap<&str, Vec<&str>>,
) -> Option<Vec<String>> {
    // Pick an arbitrary start node
    let &start = scc.iter().next()?;

    // DFS with parent tracking
    let mut stack: Vec<&str> = vec![start];
    let mut visited: HashSet<&str> = HashSet::new();
    let mut parent: HashMap<&str, &str> = HashMap::new();
    visited.insert(start);

    while let Some(node) = stack.pop() {
        let neighbors = adj.get(node).map(|v| v.as_slice()).unwrap_or(&[]);
        for &next in neighbors {
            if !scc.contains(next) {
                continue; // Stay within the SCC
            }
            if next == start && visited.len() > 1 {
                // Found a cycle back to start — reconstruct path
                // Build chain: node → parent[node] → ... → start
                let mut chain = Vec::new();
                let mut cur = node;
                while cur != start {
                    chain.push(cur.to_string());
                    cur = parent[cur];
                }
                chain.push(start.to_string());
                chain.reverse();
                // chain = [start, ..., node], append start to close the loop
                chain.push(start.to_string());
                return Some(chain);
            }
            if !visited.contains(next) {
                visited.insert(next);
                parent.insert(next, node);
                stack.push(next);
            }
        }
    }
    None
}

/// Seed nodes for depth: entry points, or root nodes (fan-in = 0).
fn find_depth_seeds<'a>(
    edges: &'a [ImportEdge],
    entry_points: &'a [EntryPoint],
) -> (Vec<&'a str>, HashMap<&'a str, Vec<&'a str>>, HashSet<&'a str>) {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut has_incoming: HashSet<&str> = HashSet::new();
    let mut all_nodes: HashSet<&str> = HashSet::new();
    for edge in edges {
        adj.entry(edge.from_file.as_str())
            .or_default()
            .push(edge.to_file.as_str());
        has_incoming.insert(edge.to_file.as_str());
        all_nodes.insert(edge.from_file.as_str());
        all_nodes.insert(edge.to_file.as_str());
    }

    let mut seeds: Vec<&str> = Vec::new();
    if !entry_points.is_empty() {
        for ep in entry_points {
            if all_nodes.contains(ep.file.as_str()) {
                seeds.push(ep.file.as_str());
            }
        }
    }
    if seeds.is_empty() {
        for &node in &all_nodes {
            if !has_incoming.contains(node) {
                seeds.push(node);
            }
        }
    }
    (seeds, adj, all_nodes)
}

/// Process a neighbor during longest-path DFS: either use memoized value,
/// Propagate a completed node's result up to its parent in the DFS stack.
fn dfs_propagate_to_parent(stack: &mut [(&str, usize, u32)], result: u32, node_count: usize) {
    if let Some((_pnode, _pidx, pmax)) = stack.last_mut() {
        let candidate = result.saturating_add(1).min(node_count as u32);
        if candidate > *pmax {
            *pmax = candidate;
        }
    }
}

/// Iterative longest-path DFS. Skips back-edges and caps at node_count.
fn longest_path_dfs<'a>(
    seeds: &[&'a str],
    adj: &HashMap<&'a str, Vec<&'a str>>,
    node_count: usize,
) -> HashMap<&'a str, u32> {
    let mut memo: HashMap<&str, u32> = HashMap::new();
    let mut on_stack: HashSet<&str> = HashSet::new();

    for &start in seeds {
        if memo.contains_key(start) {
            continue;
        }

        let mut stack: Vec<(&str, usize, u32)> = vec![(start, 0, 0)];
        on_stack.insert(start);

        while !stack.is_empty() {
            let (node, idx, max_child) = stack.last_mut().unwrap();
            let neighbors = adj.get(*node).map(|v| v.as_slice()).unwrap_or(&[]);

            if *idx < neighbors.len() {
                let neighbor = neighbors[*idx];
                *idx += 1;
                // Inline neighbor processing to avoid double mutable borrow of stack.
                if let Some(&d) = memo.get(neighbor) {
                    let candidate = d.saturating_add(1).min(node_count as u32);
                    if candidate > *max_child {
                        *max_child = candidate;
                    }
                } else if !on_stack.contains(neighbor) {
                    on_stack.insert(neighbor);
                    stack.push((neighbor, 0, 0));
                }
            } else {
                let node = *node;
                let result = *max_child;
                stack.pop();
                on_stack.remove(node);
                memo.insert(node, result);
                dfs_propagate_to_parent(&mut stack, result, node_count);
            }
        }
    }
    memo
}

/// Maximum dependency depth via iterative longest-path DFS. [ref:4e8f1175]
fn compute_max_depth(edges: &[ImportEdge], entry_points: &[EntryPoint]) -> u32 {
    if edges.is_empty() {
        return 0;
    }

    let (seeds, adj, all_nodes) = find_depth_seeds(edges, entry_points);
    let memo = longest_path_dfs(&seeds, &adj, all_nodes.len());

    // Only max from seed nodes (non-seed memos are for correctness only).
    seeds.iter()
        .filter_map(|s| memo.get(s))
        .copied()
        .max()
        .unwrap_or(0)
}

// ── Pro metrics extension point ──

/// Trait for injecting additional metrics from Pro crate.
/// Pro implements this to add Type Coupling, LCOM, etc.
pub trait MetricsExtension: Send + Sync {
    /// Compute additional metrics and return as JSON value.
    /// Called after the standard health report is computed.
    fn compute(&self, snapshot: &crate::core::snapshot::Snapshot) -> serde_json::Value;
    
    /// Name of the metric (for display in health panel).
    fn name(&self) -> &str;
}

/// Global registry of Pro metrics extensions.
static METRICS_EXTENSIONS: std::sync::OnceLock<Vec<Box<dyn MetricsExtension>>> = std::sync::OnceLock::new();

/// Register Pro metrics extensions (called by sentrux-pro at startup).
pub fn register_extensions(extensions: Vec<Box<dyn MetricsExtension>>) {
    let _ = METRICS_EXTENSIONS.set(extensions);
}

/// Get registered extensions (returns empty slice if no Pro).
pub fn extensions() -> &'static [Box<dyn MetricsExtension>] {
    METRICS_EXTENSIONS.get().map(|v| v.as_slice()).unwrap_or(&[])
}
