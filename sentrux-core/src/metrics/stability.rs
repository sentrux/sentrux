//! Coupling, entropy, cohesion, and stability computations.
//!
//! Extracted from metrics/mod.rs — these functions compute module-level
//! structural metrics from the import graph:
//!   - Coupling score (Constantine & Yourdon 1979, Martin SDP)
//!   - Stable module detection (Martin's Stable Dependencies Principle)
//!   - Shannon entropy of cross-module edge distribution
//!   - Average module cohesion
//!   - Module boundary helpers (is_same_module, module_of)

use super::testgap;
use crate::core::types::ImportEdge;
use std::collections::{HashMap, HashSet};

/// Check if two files belong to the same module.
/// BUG FIX: removed asymmetric root-level-file exception. Previously,
/// `src/app.rs` (module "src") was treated as intra-module with ALL subdirs
/// (src/layout, src/renderer, etc.), making it invisible to the coupling
/// metric regardless of how many modules it touches. This masked real
/// coupling — a file importing 10 different sub-modules showed 0% coupling.
/// Now: module equality is strict. Root-level files form their own module
/// ("src") and importing from "src/layout" IS cross-module, which is
/// factually correct. Entry-point exclusion already handles main.rs for
/// god-file detection separately.
#[allow(dead_code)] // Used by test modules (conditionally compiled)
pub(crate) fn is_same_module(path_a: &str, path_b: &str) -> bool {
    crate::core::path_utils::module_of(path_a) == crate::core::path_utils::module_of(path_b)
}

/// Re-export module_of from core::path_utils for backward compatibility.
/// Used by test modules for path-based assertions.
#[allow(dead_code)] // Used by test modules (conditionally compiled)
pub(crate) fn module_of(path: &str) -> &str {
    crate::core::path_utils::module_of(path)
}

/// Depth-aware module extraction. Uses `module_of_at_depth` when depth is
/// provided, falling back to the default `module_of` (depth-3) otherwise.
pub(crate) fn module_of_depth<'a>(path: &'a str, depth: Option<usize>) -> &'a str {
    match depth {
        Some(d) => crate::core::path_utils::module_of_at_depth(path, d),
        None => crate::core::path_utils::module_of(path),
    }
}

/// Depth-aware same-module check.
pub(crate) fn is_same_module_depth(path_a: &str, path_b: &str, depth: Option<usize>) -> bool {
    module_of_depth(path_a, depth) == module_of_depth(path_b, depth)
}

/// Coupling score: ratio of import edges that cross module boundaries,
/// excluding edges to stable foundations (Martin's Stable Dependencies Principle).
///
/// Depending on stable modules (instability ≈ 0, e.g., types.rs, error.rs) is
/// GOOD architecture — these are foundations everyone should depend on.
/// Only cross-module edges to UNSTABLE modules count as problematic coupling.
///
/// 0.0 = all imports within same module or to stable foundations.
/// 1.0 = all imports cross modules toward unstable targets (spaghetti).
pub(crate) fn compute_coupling_score(edges: &[ImportEdge], stable_modules: &HashSet<&str>, module_depth: Option<usize>) -> (f64, usize, usize) {
    if edges.is_empty() {
        return (0.0, 0, 0);
    }

    let mut cross = 0usize;
    let mut cross_unstable = 0usize;
    for edge in edges {
        if !is_same_module_depth(&edge.from_file, &edge.to_file, module_depth) {
            cross += 1;
            let to_mod = module_of_depth(&edge.to_file, module_depth);
            if !stable_modules.contains(to_mod) {
                cross_unstable += 1;
            }
        }
    }

    // Score based on cross-module edges to unstable targets only.
    // Total denominator is still all edges (not just cross-module).
    let score = cross_unstable as f64 / edges.len() as f64;
    (score, cross, cross_unstable)
}

/// Check if a module qualifies as a stable foundation given current stable set.
/// A module is stable if:
///   - ce == 0 (all outgoing go to stable) AND ca >= 2, OR
///   - instability <= threshold AND ca >= min_fan_in
fn is_module_stable(
    m: &str,
    mod_fan_out: &HashMap<&str, HashSet<&str>>,
    mod_fan_in: &HashMap<&str, HashSet<&str>>,
    stable: &HashSet<&str>,
    stability_threshold: f64,
    min_fan_in: usize,
) -> bool {
    let ce = mod_fan_out.get(m).map_or(0, |targets| {
        targets.iter().filter(|t| !stable.contains(**t)).count()
    });
    let ca = mod_fan_in.get(m).map_or(0, |s| s.len());
    let total = ca + ce;
    if total == 0 {
        return false;
    }
    if ce == 0 {
        ca >= 2
    } else {
        let instability = ce as f64 / total as f64;
        instability <= stability_threshold && ca >= min_fan_in
    }
}

/// Compute which modules are "stable foundations" (instability ≤ threshold
/// AND high enough fan-in to be genuinely foundational).
///
/// A module is a stable foundation when:
///   1. Instability I = Ce/(Ca+Ce) ≤ 0.15 (mostly depended-on, little outgoing)
///   2. Fan-in ≥ 3 (at least 3 other modules depend on it)
///
/// The fan-in floor prevents leaf nodes in small graphs from being falsely
/// classified as "foundations." A module with fan-in=1 is just a leaf, not
/// a foundational type file that everything depends on.
pub(crate) fn compute_stable_modules<'a>(edges: &'a [ImportEdge], module_depth: Option<usize>) -> HashSet<&'a str> {
    const STABILITY_THRESHOLD: f64 = 0.15;
    const MIN_FAN_IN: usize = 3;

    // Aggregate fan-in / fan-out per MODULE (not per file)
    let mut mod_fan_out: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut mod_fan_in: HashMap<&str, HashSet<&str>> = HashMap::new();

    for edge in edges {
        let from_mod = module_of_depth(&edge.from_file, module_depth);
        let to_mod = module_of_depth(&edge.to_file, module_depth);
        if from_mod != to_mod {
            mod_fan_out.entry(from_mod).or_default().insert(to_mod);
            mod_fan_in.entry(to_mod).or_default().insert(from_mod);
        }
    }

    let mut all_mods: HashSet<&str> = HashSet::new();
    all_mods.extend(mod_fan_out.keys());
    all_mods.extend(mod_fan_in.keys());

    // Cascading stability: dependencies on stable modules don't count as
    // fan-out. A module that only depends on stable foundations is itself
    // stable (Martin's Stable Dependencies Principle). Iterate until no
    // new stable modules are found.
    //
    // Special case: if effective Ce = 0 (all outgoing edges go to stable
    // modules), the module is a foundation regardless of fan-in. The fan-in
    // floor only applies to modules with non-zero effective outgoing edges
    // to prevent misclassifying unstable leaves.
    // Batch-update cascading: collect all newly-stable modules per pass,
    // then insert after the pass completes. This makes the result
    // deterministic regardless of HashSet iteration order.
    let mut stable = HashSet::new();
    loop {
        let batch: Vec<&str> = all_mods.iter()
            .filter(|&&m| !stable.contains(m))
            .filter(|&&m| is_module_stable(m, &mod_fan_out, &mod_fan_in, &stable, STABILITY_THRESHOLD, MIN_FAN_IN))
            .copied()
            .collect();
        if batch.is_empty() {
            break;
        }
        for m in batch {
            stable.insert(m);
        }
    }
    stable
}

/// Shannon entropy of the cross-module edge distribution, excluding edges
/// to stable foundations (Martin's Stable Dependencies Principle).
///
/// H = -Σ p(i) * log2(p(i)), normalized to [0,1] by log2(N).
///
/// Only cross-module edges to UNSTABLE targets are counted. Edges to stable
/// modules (types, error, config) are healthy hub-and-spoke dependencies
/// and should not inflate entropy. Without this, a project with shared
/// foundational types always scores entropy ≈ 1.0 (F grade).
pub(crate) fn compute_shannon_entropy(edges: &[ImportEdge], stable_modules: &HashSet<&str>, module_depth: Option<usize>) -> (f64, f64, usize) {
    if edges.is_empty() {
        return (0.0, 0.0, 0);
    }

    // Count only cross-module edges to unstable targets
    let mut pair_counts: HashMap<(&str, &str), usize> = HashMap::new();
    let mut cross_count: usize = 0;
    for edge in edges {
        if !is_same_module_depth(&edge.from_file, &edge.to_file, module_depth) {
            let from_mod = module_of_depth(&edge.from_file, module_depth);
            let to_mod = module_of_depth(&edge.to_file, module_depth);
            // Skip edges to stable foundations
            if stable_modules.contains(to_mod) {
                continue;
            }
            *pair_counts.entry((from_mod, to_mod)).or_default() += 1;
            cross_count += 1;
        }
    }

    // No problematic cross-module edges = zero entropy
    if cross_count == 0 {
        return (0.0, 0.0, 0);
    }

    let num_pairs = pair_counts.len();
    if num_pairs <= 1 {
        return (0.0, 0.0, num_pairs);
    }


    let n = cross_count as f64;
    let mut h: f64 = 0.0;
    for &count in pair_counts.values() {
        let p = count as f64 / n;
        if p > 0.0 {
            h -= p * p.log2();
        }
    }

    let max_h = (num_pairs as f64).log2();
    let normalized = if max_h > 0.0 { h / max_h } else { 0.0 };

    (normalized, h, num_pairs)
}

/// Average module cohesion: for each module, what fraction of a spanning tree's
/// edges actually exist? Baseline is n-1 (minimum edges for full connectivity).
/// Uses `module_of()` with adaptive depth — same boundary as coupling score,
/// so the two metrics measure complementary aspects of the same structure.
/// Returns None if no modules with ≥2 files exist (nothing to measure).
/// Returns Some(0.0) if modules exist but have zero internal connectivity.
///
/// Test files are excluded from the file count (but their edges still count).
/// Tests inflate module size N without contributing incoming edges — production
/// code never imports from test files. This is the same principle as excluding
/// entry points from god-file detection: known one-way consumers should not
/// penalize the metric they can't contribute to.
pub(crate) fn compute_avg_cohesion(edges: &[ImportEdge], files: &[&crate::core::types::FileNode], module_depth: Option<usize>, cohesion_exclude: &[String]) -> Option<f64> {
    // Group files by module (same boundary as coupling), excluding test files
    // and user-specified cohesion_exclude patterns (e.g., migrations, seeds).
    let mut mod_files: HashMap<&str, Vec<&str>> = HashMap::new();
    for f in files {
        if f.is_dir || f.lang.is_empty() {
            continue;
        }
        // Skip files that weren't successfully parsed — they have a detected
        // language but no grammar or exceeded parse limits. Including them
        // inflates module size N without contributing any import edges.
        if f.sa.is_none() {
            continue;
        }
        if testgap::is_test_file(&f.path) {
            continue;
        }
        if !cohesion_exclude.is_empty() && cohesion_exclude.iter().any(|pat| crate::metrics::rules::glob_match(pat, &f.path)) {
            continue;
        }
        let m = module_of_depth(&f.path, module_depth);
        mod_files.entry(m).or_default().push(f.path.as_str());
    }

    // Count intra-module edges per module.
    // is_same_module_depth is strict equality of module_of_depth(), so both endpoints
    // always map to the same module key — attribute to that module.
    let mut mod_edge_count: HashMap<&str, usize> = HashMap::new();
    for edge in edges {
        if is_same_module_depth(&edge.from_file, &edge.to_file, module_depth) {
            let m = module_of_depth(&edge.from_file, module_depth);
            *mod_edge_count.entry(m).or_default() += 1;
        }
    }

    // Cohesion = actual_intra_edges / expected_edges per module.
    //
    // Baseline: n-1 (spanning tree = minimum edges for full connectivity).
    //
    // RATIONALE: The previous baseline n*(n-1)/2 (half of all pairs) was far
    // too aggressive. A well-structured 15-file module with a spanning tree
    // (14 edges) scored 14/105 = 0.13 (grade D), even though every file was
    // reachable. In practice, files import what they need — not every sibling.
    // A spanning tree IS the minimal connected graph, and scoring it as "poor"
    // penalizes principled, minimal-dependency design.
    //
    // With n-1 as baseline:
    //   - cohesion = 1.0: at least spanning-tree connectivity (all files reachable)
    //   - cohesion = 0.5: half the minimum edges present (some files disconnected)
    //   - cohesion = 0.0: no internal edges (files are unrelated → should split module)
    //
    // Values > 1.0 are capped: extra edges beyond a spanning tree don't improve
    // the score — we reward connectivity, not over-coupling.
    //
    // Examples:
    //   10-file module with 9 edges (spanning tree)  → 9/9  = 1.0 (A)
    //   10-file module with 5 edges (partially linked)→ 5/9  = 0.56 (B)
    //   10-file module with 2 edges (mostly isolated) → 2/9  = 0.22 (D)
    //   10-file module with 0 edges (no connectivity) → 0/9  = 0.0  (F)
    let mut total_cohesion = 0.0;
    let mut module_count = 0u32;

    for (m, files_in_mod) in &mod_files {
        let n = files_in_mod.len();
        if n < 2 {
            continue;
        }
        // n-1 = spanning tree (minimum edges for a connected graph)
        let expected_edges = n - 1;
        let actual = *mod_edge_count.get(m).unwrap_or(&0);
        let cohesion = (actual as f64 / expected_edges as f64).min(1.0);
        total_cohesion += cohesion;
        module_count += 1;
    }

    if module_count == 0 {
        return None;
    }
    Some(total_cohesion / module_count as f64)
}
