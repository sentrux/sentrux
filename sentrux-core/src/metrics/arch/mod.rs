//! Architecture-level metrics — beyond structural hygiene.
//!
//! Based on:
//! - Lakos 1996: levelization, upward dependency violations
//! - Robert C. Martin 2003: distance from main sequence, dependency direction
//! - Baldwin & Clark 2000: Design Structure Matrix (data only, rendering elsewhere)
//!
//! All metrics operate on the existing import graph — no additional parsing.
//!
//! Graph algorithms (SCC, levelization, blast radius, attack surface) are in
//! `arch_graph`. Re-exported here for backward compatibility.

use crate::core::types::ImportEdge;
use crate::core::snapshot::Snapshot;
use self::distance::{self as distance_mod, ModuleDistance};
use std::collections::{HashMap, HashSet};

pub mod distance;
pub mod graph;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;

// Re-export graph algorithms for backward compatibility.
pub use self::graph::{
    compute_attack_surface, compute_blast_radius, compute_levels, find_upward_violations,
    UpwardViolation,
};
pub(crate) use self::graph::{
    compute_levels_with_sccs, compute_sccs, find_upward_violations_with_sccs,
};

// ── Trait: ArchAnalyzer ──

/// Interface for computing architecture-level metrics from a snapshot.
///
/// Abstracts the architecture analysis so that:
/// - Tests can inject synthetic snapshots and verify grading logic
/// - Alternative analysis strategies (e.g., package-level vs file-level) can be swapped
/// - Pre-computed reports can be cached and returned directly
pub trait ArchAnalyzer {
    /// Compute the full architecture report from a snapshot.
    fn analyze(&self, snapshot: &Snapshot) -> ArchReport;

    /// Compute file levels from import edges.
    fn levels(&self, edges: &[ImportEdge]) -> (HashMap<String, u32>, u32);

    /// Compute blast radius from import edges.
    fn blast_radius(&self, edges: &[ImportEdge]) -> HashMap<String, u32>;
}

/// Default implementation using Lakos levelization and Martin distance metrics.
pub struct DefaultArchAnalyzer;

impl ArchAnalyzer for DefaultArchAnalyzer {
    fn analyze(&self, snapshot: &Snapshot) -> ArchReport {
        compute_arch(snapshot)
    }

    fn levels(&self, edges: &[ImportEdge]) -> (HashMap<String, u32>, u32) {
        compute_levels(edges)
    }

    fn blast_radius(&self, edges: &[ImportEdge]) -> HashMap<String, u32> {
        compute_blast_radius(edges)
    }
}

// ── Named constants [ref:736ae249] ──

// Upward-dependency ratio thresholds removed — continuous [0,1] scores replace letter grades.

// ── Public types ──

/// Complete architecture report — aggregates all arch-level metrics.
/// Produced by `compute_arch()` from a Snapshot's import graph.
#[derive(Debug, Clone)]
pub struct ArchReport {
    // ── Lakos 1996 — Levelization ──
    /// Per-file level in the DAG (0 = leaf, higher = more dependencies below)
    pub levels: HashMap<String, u32>,
    /// Maximum level across all files
    pub max_level: u32,
    /// Edges that violate levelization (from lower level to higher level)
    pub upward_violations: Vec<UpwardViolation>,
    /// Ratio of upward violations to total edges
    pub upward_ratio: f64,
    // (levelization_score removed — proxy metric, captured by root cause acyclicity+depth)

    // ── Blast radius (transitive reach from each file) ──
    /// Per-file transitive dependent count
    pub blast_radius: HashMap<String, u32>,
    /// Highest blast radius in the codebase
    pub max_blast_radius: u32,
    /// File with the highest blast radius
    pub max_blast_file: String,

    // ── Attack surface (transitive reach from entry points) ──
    /// Number of files reachable from any entry point
    pub attack_surface_files: u32,
    /// Ratio of reachable files to total graph files
    pub attack_surface_ratio: f64,
    /// Total files in the dependency graph
    pub total_graph_files: u32,

    // ── Distance from Main Sequence (Martin 2003) ──
    /// Per-module distance metrics
    pub distance_metrics: Vec<ModuleDistance>,
    /// Average distance across all modules
    pub avg_distance: f64,
    // (distance_score, blast_score, surface_score, arch_score removed
    //  — proxy metrics, all captured by root cause modularity Q)
}

/// One complex function entry stored in a baseline for gate/session diffs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplexFnSnapshot {
    pub file: String,
    pub func: String,
    pub cc: u32,
}

impl ComplexFnSnapshot {
    fn from_func_metric(m: &crate::metrics::FuncMetric) -> Self {
        Self {
            file: m.file.clone(),
            func: m.func.clone(),
            cc: m.value,
        }
    }

    fn key(&self) -> String {
        format!("{}::{}", self.file, self.func)
    }
}

fn complex_fn_snapshots(report: &crate::metrics::HealthReport) -> Vec<ComplexFnSnapshot> {
    report
        .complex_functions
        .iter()
        .map(ComplexFnSnapshot::from_func_metric)
        .collect()
}

/// Baseline snapshot for session diff / structural regression gate.
/// Captured at session start; subsequent scans compare against this
/// to detect regressions (e.g., quality_signal drop, new cycles).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArchBaseline {
    /// When the baseline was captured (Unix epoch seconds)
    pub timestamp: f64,
    /// Quality signal at baseline (geometric mean of 3 category scores)
    pub quality_signal: f64,
    /// Coupling score at baseline
    pub coupling_score: f64,
    /// Number of circular dependency cycles at baseline
    pub cycle_count: usize,
    /// Number of god files (fan-out > threshold) at baseline
    pub god_file_count: usize,
    /// Number of hotspot files (fan-in > threshold) at baseline
    pub hotspot_count: usize,
    /// Number of complex functions (CC > threshold) at baseline
    pub complex_fn_count: usize,
    /// Maximum dependency depth at baseline
    pub max_depth: u32,
    /// Total import edges at baseline
    pub total_import_edges: usize,
    /// Cross-module import edges at baseline
    pub cross_module_edges: usize,
    /// Complex functions (CC > language threshold) at baseline — optional for backward compat.
    #[serde(default)]
    pub complex_functions: Vec<ComplexFnSnapshot>,
}

/// Diff between two snapshots (baseline vs current).
#[derive(Debug, Clone)]
pub struct ArchDiff {
    /// Quality signal from the baseline
    pub signal_before: f64,
    /// Quality signal from the current snapshot
    pub signal_after: f64,
    /// Coupling score from the baseline
    pub coupling_before: f64,
    /// Coupling score from the current snapshot
    pub coupling_after: f64,
    /// Cycle count from the baseline
    pub cycles_before: usize,
    /// Cycle count from the current snapshot
    pub cycles_after: usize,
    /// God file count from the baseline
    pub god_files_before: usize,
    /// God file count from the current snapshot
    pub god_files_after: usize,
    /// True if quality_signal dropped or any metric degraded
    pub degraded: bool,
    /// Human-readable violation descriptions
    pub violations: Vec<String>,
    /// Complex functions present in current scan but not in baseline
    pub new_complex_functions: Vec<ComplexFnSnapshot>,
    /// Complex functions present in baseline but no longer complex in current scan
    pub removed_complex_functions: Vec<ComplexFnSnapshot>,
}

// ── Baseline Save/Load ──

impl ArchBaseline {
    /// Create baseline from current health report.
    pub fn from_health(report: &crate::metrics::HealthReport) -> Self {
        Self {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
            quality_signal: report.quality_signal,
            coupling_score: report.coupling_score,
            cycle_count: report.circular_dep_count,
            god_file_count: report.god_files.len(),
            hotspot_count: report.hotspot_files.len(),
            complex_fn_count: report.complex_functions.len(),
            max_depth: report.max_depth,
            total_import_edges: report.total_import_edges,
            cross_module_edges: report.cross_module_edges,
            complex_functions: complex_fn_snapshots(report),
        }
    }

    /// Save baseline to a JSON file.
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize baseline: {e}"))?;
        std::fs::write(path, json)
            .map_err(|e| format!("Failed to write baseline to {}: {e}", path.display()))?;
        Ok(())
    }

    /// Load baseline from a JSON file.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read baseline from {}: {e}", path.display()))?;
        serde_json::from_str(&json)
            .map_err(|e| format!("Failed to parse baseline: {e}"))
    }

    /// Compare current health report against this baseline.
    /// Degradation = quality_signal dropped OR any specific metric worsened.
    pub fn diff(&self, current: &crate::metrics::HealthReport) -> ArchDiff {
        let mut violations = Vec::new();

        // Quality signal is the primary indicator
        let signal_delta = current.quality_signal - self.quality_signal;
        if signal_delta < -0.02 {
            violations.push(format!(
                "Quality signal dropped: {:.2} → {:.2} ({:+.2})",
                self.quality_signal, current.quality_signal, signal_delta
            ));
        }

        if current.coupling_score > self.coupling_score + 0.05 {
            violations.push(format!(
                "Coupling degraded: {:.2} → {:.2}",
                self.coupling_score, current.coupling_score
            ));
        }
        if current.circular_dep_count > self.cycle_count {
            violations.push(format!(
                "Cycles increased: {} → {}",
                self.cycle_count, current.circular_dep_count
            ));
        }
        if current.god_files.len() > self.god_file_count {
            violations.push(format!(
                "God files increased: {} → {}",
                self.god_file_count,
                current.god_files.len()
            ));
        }
        let current_complex = complex_fn_snapshots(current);
        let legacy_baseline = self.complex_functions.is_empty() && self.complex_fn_count > 0;
        let (new_complex_functions, removed_complex_functions) = if legacy_baseline {
            (Vec::new(), Vec::new())
        } else {
            diff_complex_functions(&self.complex_functions, &current_complex)
        };

        if current.complex_functions.len() > self.complex_fn_count {
            violations.push(format!(
                "Complex functions increased: {} → {}",
                self.complex_fn_count,
                current.complex_functions.len()
            ));
        }
        if !legacy_baseline {
            for f in &new_complex_functions {
                violations.push(format!(
                    "New complex function: {}::{} (cc={})",
                    f.file, f.func, f.cc
                ));
            }
        }

        let degraded = current.quality_signal < self.quality_signal - 0.02
            || !violations.is_empty();

        ArchDiff {
            signal_before: self.quality_signal,
            signal_after: current.quality_signal,
            coupling_before: self.coupling_score,
            coupling_after: current.coupling_score,
            cycles_before: self.cycle_count,
            cycles_after: current.circular_dep_count,
            god_files_before: self.god_file_count,
            god_files_after: current.god_files.len(),
            degraded,
            violations,
            new_complex_functions,
            removed_complex_functions,
        }
    }
}

/// Set-diff complex function snapshots by `file::func` key.
fn diff_complex_functions(
    before: &[ComplexFnSnapshot],
    after: &[ComplexFnSnapshot],
) -> (Vec<ComplexFnSnapshot>, Vec<ComplexFnSnapshot>) {
    use std::collections::HashSet;

    let before_keys: HashSet<String> = before.iter().map(ComplexFnSnapshot::key).collect();
    let after_keys: HashSet<String> = after.iter().map(ComplexFnSnapshot::key).collect();

    let new_fns: Vec<ComplexFnSnapshot> = after
        .iter()
        .filter(|f| !before_keys.contains(&f.key()))
        .cloned()
        .collect();
    let removed_fns: Vec<ComplexFnSnapshot> = before
        .iter()
        .filter(|f| !after_keys.contains(&f.key()))
        .cloned()
        .collect();
    (new_fns, removed_fns)
}

// ── Grading ──

/// Blast radius concentration score [0,1].
/// 1.0 = no concentrated blast, 0.0 = catastrophic concentration.
/// [ref:28b7bc6f]
pub fn score_blast_concentration(blast_radius: &HashMap<String, u32>, edges: &[ImportEdge]) -> f64 {
    if blast_radius.is_empty() || edges.is_empty() {
        return 1.0;
    }
    let total_files = blast_radius.len();
    if total_files == 0 {
        return 1.0;
    }

    let (mod_fan_out, mod_fan_in) = compute_blast_module_coupling(edges);
    let file_fan_in = compute_blast_file_fan_in(edges);

    let max_non_foundation = find_max_non_foundation_blast(
        blast_radius, &mod_fan_out, &mod_fan_in, &file_fan_in,
    );

    let ratio = max_non_foundation as f64 / total_files as f64;
    (1.0 - ratio).clamp(0.0, 1.0)
}

/// Compute MODULE-level coupling, excluding mod-declaration edges.
/// Rust `pub mod foo;` creates structural containment edges that inflate
/// parent module fan-out without representing functional dependencies.
fn compute_blast_module_coupling(
    edges: &[ImportEdge],
) -> (HashMap<String, HashSet<String>>, HashMap<String, HashSet<String>>) {
    let mut mod_fan_out: HashMap<String, HashSet<String>> = HashMap::new();
    let mut mod_fan_in: HashMap<String, HashSet<String>> = HashMap::new();
    for edge in edges {
        if crate::metrics::types::is_mod_declaration_edge(edge) {
            continue;
        }
        let from_mod = crate::core::path_utils::module_of(&edge.from_file).to_string();
        let to_mod = crate::core::path_utils::module_of(&edge.to_file).to_string();
        if from_mod != to_mod {
            mod_fan_out.entry(from_mod.clone()).or_default().insert(to_mod.clone());
            mod_fan_in.entry(to_mod).or_default().insert(from_mod);
        }
    }
    (mod_fan_out, mod_fan_in)
}

/// Compute file-level fan-in for foundation file detection.
fn compute_blast_file_fan_in(edges: &[ImportEdge]) -> HashMap<&str, usize> {
    let mut file_fan_in: HashMap<&str, usize> = HashMap::new();
    for edge in edges {
        *file_fan_in.entry(edge.to_file.as_str()).or_default() += 1;
    }
    file_fan_in
}

/// Find the maximum blast radius among non-foundation files.
/// A file is foundation if its MODULE is stable OR the FILE itself has high fan-in.
fn find_max_non_foundation_blast(
    blast_radius: &HashMap<String, u32>,
    mod_fan_out: &HashMap<String, HashSet<String>>,
    mod_fan_in: &HashMap<String, HashSet<String>>,
    file_fan_in: &HashMap<&str, usize>,
) -> u32 {
    const MOD_STABILITY_THRESHOLD: f64 = 0.25;
    const MIN_MOD_FAN_IN: usize = 2;
    /// File-level foundation: a file with enough direct dependents is "too
    /// important to change casually" regardless of its fan-out.
    const MIN_FILE_FAN_IN_FOUNDATION: usize = 5;

    let is_foundation_module = |module: &str| -> bool {
        let ce = mod_fan_out.get(module).map_or(0, |s| s.len());
        let ca = mod_fan_in.get(module).map_or(0, |s| s.len());
        let total = ca + ce;
        if total == 0 { return false; }
        let instability = ce as f64 / total as f64;
        instability <= MOD_STABILITY_THRESHOLD && ca >= MIN_MOD_FAN_IN
    };

    let mut max_non_foundation: u32 = 0;
    for (path, &blast) in blast_radius {
        let module = crate::core::path_utils::module_of(path).to_string();
        let ca = file_fan_in.get(path.as_str()).copied().unwrap_or(0);
        // Package-index files (__init__.py, index.js, mod.rs, etc.) are barrel
        // re-exporters — their high blast radius reflects re-exports, not genuine
        // change risk. Treat them as foundation regardless of instability.
        let is_barrel = super::is_package_index_for_path(path);
        let is_foundation = is_barrel
            || is_foundation_module(&module)
            || ca >= MIN_FILE_FAN_IN_FOUNDATION;
        if !is_foundation && blast > max_non_foundation {
            max_non_foundation = blast;
        }
    }

    // If ALL files are in foundation modules, blast radius is architecturally
    // expected (stable foundations naturally have high reach). Return 0 so that
    // the grade computes as 'A' — penalizing stable-only codebases is wrong.
    max_non_foundation
}

/// Attack surface score [0,1]: 1.0 = minimal exposure, 0.0 = everything reachable.
pub fn score_attack_surface(ratio: f64) -> f64 {
    (1.0 - ratio).clamp(0.0, 1.0)
}

/// Check if a project is an application (has main entry points) vs a library.
/// Applications naturally have ~100% reachable code — grading attack surface
/// penalizes correct architecture. Libraries benefit from encapsulation.
pub fn is_application(snapshot: &Snapshot) -> bool {
    snapshot.entry_points.iter().any(|ep| ep.func == "main")
}

/// Levelization score [0,1]: 1.0 = no upward violations.
pub(crate) fn score_levelization(upward_ratio: f64) -> f64 {
    (1.0 - upward_ratio * 10.0).clamp(0.0, 1.0)
}

// ── Main entry point ──

/// Compute architecture report from a snapshot.
pub fn compute_arch(snapshot: &Snapshot) -> ArchReport {
    let edges = &snapshot.import_graph;

    // Filter mod-declaration edges (Rust `pub mod foo;`) from levelization.
    // Mod declarations are structural containment — NOT functional dependencies.
    // Without this filter, parent→child + child→parent(facade) creates false cycles.
    // Health metrics already filter these for coupling/depth/cycles; arch must too.
    let dep_edges: Vec<ImportEdge> = edges.iter()
        .filter(|e| !crate::metrics::types::is_mod_declaration_edge(e))
        .cloned()
        .collect();

    // Compute SCCs once and share between levelization + violation detection.
    let sccs = compute_sccs(&dep_edges);
    let (levels, max_level) = compute_levels_with_sccs(&dep_edges, &sccs);
    let upward_violations = find_upward_violations_with_sccs(&dep_edges, &levels, &sccs);
    let upward_ratio = if dep_edges.is_empty() {
        0.0
    } else {
        upward_violations.len() as f64 / dep_edges.len() as f64
    };
    // Blast radius (already filters mod-declaration edges internally)
    let blast_radius = compute_blast_radius(edges);
    let (max_blast_file, max_blast_radius) = blast_radius
        .iter()
        .max_by_key(|(_, &v)| v)
        .map(|(k, &v)| (k.clone(), v))
        .unwrap_or_default();

    // Attack surface + distance — diagnostic data only, no scoring
    let (attack_surface_files, total_graph_files, attack_surface_ratio,
         distance_metrics, avg_distance) =
        compute_arch_diagnostics(snapshot, &dep_edges);

    ArchReport {
        levels,
        max_level,
        upward_violations,
        upward_ratio,
        blast_radius,
        max_blast_radius,
        max_blast_file,
        attack_surface_files,
        attack_surface_ratio,
        total_graph_files,
        distance_metrics,
        avg_distance,
    }
}

/// Compute attack surface and distance diagnostics for compute_arch.
/// No scoring — diagnostic data only. The one true score is quality_signal.
fn compute_arch_diagnostics(
    snapshot: &Snapshot,
    dep_edges: &[ImportEdge],
) -> (u32, u32, f64, Vec<ModuleDistance>, f64) {
    let (attack_surface_files, total_graph_files) =
        compute_attack_surface(dep_edges, &snapshot.entry_points);
    let attack_surface_ratio = if total_graph_files > 0 {
        attack_surface_files as f64 / total_graph_files as f64
    } else {
        0.0
    };

    // Distance from Main Sequence (Martin 2003) — diagnostic only
    let distance_metrics = distance_mod::compute_distance_from_main_seq(snapshot, &snapshot.import_graph);
    let avg_distance = {
        let non_foundation: Vec<&ModuleDistance> = distance_metrics.iter()
            .filter(|m| !m.is_foundation)
            .collect();
        if non_foundation.is_empty() {
            0.0
        } else {
            non_foundation.iter().map(|m| m.distance).sum::<f64>() / non_foundation.len() as f64
        }
    };

    (attack_surface_files, total_graph_files, attack_surface_ratio,
     distance_metrics, avg_distance)
}
