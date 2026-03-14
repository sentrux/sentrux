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

// ── Internal helpers ──

/// Depth-aware module path extraction for arch metrics.
fn module_of_path(path: &str, module_depth: Option<usize>) -> String {
    match module_depth {
        Some(d) => crate::core::path_utils::module_of_at_depth(path, d).to_string(),
        None => crate::core::path_utils::module_of(path).to_string(),
    }
}

// ── Named constants [ref:736ae249] ──

/// Maximum allowed upward-dependency ratio before grading as F.
const UPWARD_RATIO_THRESHOLD_A: f64 = 0.0;
const UPWARD_RATIO_THRESHOLD_B: f64 = 0.02;
const UPWARD_RATIO_THRESHOLD_C: f64 = 0.05;
const UPWARD_RATIO_THRESHOLD_D: f64 = 0.10;

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
    /// Grade for levelization quality (A = no violations)
    pub levelization_grade: char,

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
    /// Grade for distance from main sequence
    pub distance_grade: char,

    // ── Sub-grades (pre-computed for UI display) ──
    /// Grade for blast radius severity
    pub blast_grade: char,
    /// Grade for attack surface exposure
    pub surface_grade: char,

    // ── Overall architecture grade ──
    /// Composite grade from all architecture dimensions
    pub arch_grade: char,
}

/// Baseline snapshot for session diff / structural regression gate.
/// Captured at session start; subsequent scans compare against this
/// to detect regressions (e.g., new cycles, grade drops).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArchBaseline {
    /// When the baseline was captured (Unix epoch seconds)
    pub timestamp: f64,
    /// Overall structure grade at baseline
    pub structure_grade: char,
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
}

/// Diff between two snapshots (baseline vs current).
#[derive(Debug, Clone)]
pub struct ArchDiff {
    /// Structure grade from the baseline snapshot
    pub structure_grade_before: char,
    /// Structure grade from the current snapshot
    pub structure_grade_after: char,
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
    /// True if any metric degraded beyond tolerance
    pub degraded: bool,
    /// Human-readable violation descriptions
    pub violations: Vec<String>,
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
            structure_grade: report.grade,
            coupling_score: report.coupling_score,
            cycle_count: report.circular_dep_count,
            god_file_count: report.god_files.len(),
            hotspot_count: report.hotspot_files.len(),
            complex_fn_count: report.complex_functions.len(),
            max_depth: report.max_depth,
            total_import_edges: report.total_import_edges,
            cross_module_edges: report.cross_module_edges,
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
    pub fn diff(&self, current: &crate::metrics::HealthReport) -> ArchDiff {
        let mut violations = Vec::new();

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
        if current.complex_functions.len() > self.complex_fn_count {
            violations.push(format!(
                "Complex functions increased: {} → {}",
                self.complex_fn_count,
                current.complex_functions.len()
            ));
        }

        let degraded = current.grade > self.structure_grade || !violations.is_empty();

        ArchDiff {
            structure_grade_before: self.structure_grade,
            structure_grade_after: current.grade,
            coupling_before: self.coupling_score,
            coupling_after: current.coupling_score,
            cycles_before: self.cycle_count,
            cycles_after: current.circular_dep_count,
            god_files_before: self.god_file_count,
            god_files_after: current.god_files.len(),
            degraded,
            violations,
        }
    }
}

// ── Grading ──

/// Grade blast radius concentration: max blast of non-foundation files / total files.
///
/// A file belongs to a "foundation module" when its MODULE (depth-2 directory)
/// is stable — low instability at the module level AND sufficient module-level
/// incoming coupling. Internal implementation files (helpers, sub-modules)
/// inherit their module's stability: if the module is stable, all its files are
/// considered foundation regardless of their individual fan-in.
///
/// This is architecturally correct: blast risk comes from volatile business
/// logic modules, not from internal helpers of stable infrastructure modules.
/// [ref:28b7bc6f]
pub fn grade_blast_concentration(blast_radius: &HashMap<String, u32>, edges: &[ImportEdge]) -> char {
    grade_blast_concentration_depth(blast_radius, edges, None)
}

fn grade_blast_concentration_depth(blast_radius: &HashMap<String, u32>, edges: &[ImportEdge], module_depth: Option<usize>) -> char {
    if blast_radius.is_empty() || edges.is_empty() {
        return 'A';
    }
    let total_files = blast_radius.len();
    if total_files == 0 {
        return 'A';
    }

    let (mod_fan_out, mod_fan_in) = compute_blast_module_coupling(edges, module_depth);
    let file_fan_in = compute_blast_file_fan_in(edges);

    let max_non_foundation = find_max_non_foundation_blast(
        blast_radius, &mod_fan_out, &mod_fan_in, &file_fan_in, module_depth,
    );

    let ratio = max_non_foundation as f64 / total_files as f64;
    if ratio <= 0.10 { 'A' }
    else if ratio <= 0.20 { 'B' }
    else if ratio <= 0.35 { 'C' }
    else if ratio <= 0.50 { 'D' }
    else { 'F' }
}

/// Compute MODULE-level coupling, excluding mod-declaration edges.
/// Rust `pub mod foo;` creates structural containment edges that inflate
/// parent module fan-out without representing functional dependencies.
fn compute_blast_module_coupling(
    edges: &[ImportEdge],
    module_depth: Option<usize>,
) -> (HashMap<String, HashSet<String>>, HashMap<String, HashSet<String>>) {
    let mut mod_fan_out: HashMap<String, HashSet<String>> = HashMap::new();
    let mut mod_fan_in: HashMap<String, HashSet<String>> = HashMap::new();
    for edge in edges {
        if crate::metrics::types::is_mod_declaration_edge(edge) {
            continue;
        }
        let from_mod = module_of_path(&edge.from_file, module_depth);
        let to_mod = module_of_path(&edge.to_file, module_depth);
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
    module_depth: Option<usize>,
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
        let module = module_of_path(path, module_depth);
        let ca = file_fan_in.get(path.as_str()).copied().unwrap_or(0);
        // Package-index files (__init__.py, index.js, mod.rs, etc.) are barrel
        // re-exporters — their high blast radius reflects re-exports, not genuine
        // change risk. Treat them as foundation regardless of instability.
        let is_barrel = super::is_package_index_file(path);
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

/// Grade attack surface: what fraction of the codebase is reachable from entry points.
/// Original thresholds — unchanged.
pub fn grade_attack_surface(ratio: f64) -> char {
    if ratio <= 0.30 { 'A' }
    else if ratio <= 0.50 { 'B' }
    else if ratio <= 0.70 { 'C' }
    else if ratio <= 0.85 { 'D' }
    else { 'F' }
}

/// Check if a project is an application (has main entry points) vs a library.
/// Applications naturally have ~100% reachable code — grading attack surface
/// penalizes correct architecture. Libraries benefit from encapsulation.
pub fn is_application(snapshot: &Snapshot) -> bool {
    snapshot.entry_points.iter().any(|ep| ep.func == "main")
}

/// Composite architecture grade: floor_mean capped by worst + 1.
/// Same aggregation as health grade for consistency.
fn composite_grade(a: char, b: char, c: char, d: char) -> char {
    let grades = [a, b, c, d];
    let sum: u32 = grades.iter().map(|&g| match g { 'A'=>4, 'B'=>3, 'C'=>2, 'D'=>1, _=>0 }).sum();
    let floor_mean = match sum / 4 { 4=>'A', 3=>'B', 2=>'C', 1=>'D', _=>'F' };
    let worst = *grades.iter().max().unwrap(); // 'F' > 'A' in ASCII
    let worst_val = match worst { 'A'=>4u32, 'B'=>3, 'C'=>2, 'D'=>1, _=>0 };
    let cap = match if worst_val < 4 { worst_val + 1 } else { 4 } { 4=>'A', 3=>'B', 2=>'C', 1=>'D', _=>'F' };
    if floor_mean > cap { floor_mean } else { cap }
}

pub(crate) fn grade_levelization(upward_ratio: f64) -> char {
    if upward_ratio <= UPWARD_RATIO_THRESHOLD_A {
        'A'
    } else if upward_ratio <= UPWARD_RATIO_THRESHOLD_B {
        'B'
    } else if upward_ratio <= UPWARD_RATIO_THRESHOLD_C {
        'C'
    } else if upward_ratio <= UPWARD_RATIO_THRESHOLD_D {
        'D'
    } else {
        'F'
    }
}

// ── Main entry point ──

/// Compute architecture report from a snapshot.
pub fn compute_arch(snapshot: &Snapshot) -> ArchReport {
    compute_arch_with_depth(snapshot, None)
}

/// Compute architecture report from a snapshot with configurable module depth.
///
/// `module_depth` controls how deep the module boundary detection goes:
/// - `None` (default): depth-3 (fine-grained sub-modules)
/// - `Some(2)`: depth-2 (coarser grouping, better for monorepos)
pub fn compute_arch_with_depth(snapshot: &Snapshot, module_depth: Option<usize>) -> ArchReport {
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
    let levelization_grade = grade_levelization(upward_ratio);

    // Blast radius (already filters mod-declaration edges internally)
    let blast_radius = compute_blast_radius(edges);
    let (max_blast_file, max_blast_radius) = blast_radius
        .iter()
        .max_by_key(|(_, &v)| v)
        .map(|(k, &v)| (k.clone(), v))
        .unwrap_or_default();

    // Attack surface + distance + grading — pass pre-filtered dep_edges to avoid redundant filtering
    let (attack_surface_files, total_graph_files, attack_surface_ratio,
         distance_metrics, avg_distance, distance_grade,
         blast_grade, surface_grade, arch_grade) =
        compute_arch_secondary(snapshot, edges, &dep_edges, &blast_radius, levelization_grade, module_depth);

    ArchReport {
        levels,
        max_level,
        upward_violations,
        upward_ratio,
        levelization_grade,
        blast_radius,
        max_blast_radius,
        max_blast_file,
        attack_surface_files,
        attack_surface_ratio,
        total_graph_files,
        distance_metrics,
        avg_distance,
        distance_grade,
        blast_grade,
        surface_grade,
        arch_grade,
    }
}

/// Compute attack surface, distance metrics, and final grading for compute_arch.
/// Takes pre-filtered `dep_edges` (mod-declaration edges removed) to avoid redundant O(E) filtering.
fn compute_arch_secondary(
    snapshot: &Snapshot,
    edges: &[ImportEdge],
    dep_edges: &[ImportEdge],
    blast_radius: &HashMap<String, u32>,
    levelization_grade: char,
    module_depth: Option<usize>,
) -> (u32, u32, f64, Vec<ModuleDistance>, f64, char, char, char, char) {
    let (attack_surface_files, total_graph_files) =
        compute_attack_surface(dep_edges, &snapshot.entry_points);
    let attack_surface_ratio = if total_graph_files > 0 {
        attack_surface_files as f64 / total_graph_files as f64
    } else {
        0.0
    };

    // Distance from Main Sequence (Martin 2003)
    let distance_metrics = distance_mod::compute_distance_from_main_seq(snapshot, edges, module_depth);
    let (avg_distance, distance_grade) = compute_distance_grade(&distance_metrics);

    // Overall architecture grade — composite conformance score.
    let (blast_grade, surface_grade, arch_grade) =
        compute_arch_grades(snapshot, edges, blast_radius, levelization_grade,
                            attack_surface_ratio, distance_grade, module_depth);

    (attack_surface_files, total_graph_files, attack_surface_ratio,
     distance_metrics, avg_distance, distance_grade, blast_grade, surface_grade, arch_grade)
}

/// Compute average distance and grade from module distance metrics.
/// Excludes foundation modules (I <= threshold) — their high D is expected.
fn compute_distance_grade(distance_metrics: &[ModuleDistance]) -> (f64, char) {
    let non_foundation: Vec<&ModuleDistance> = distance_metrics.iter()
        .filter(|m| !m.is_foundation)
        .collect();
    let avg_distance = if non_foundation.is_empty() {
        0.0
    } else {
        non_foundation.iter().map(|m| m.distance).sum::<f64>() / non_foundation.len() as f64
    };
    // Only grade distance if non-foundation modules have abstract types.
    // With A=0, D always equals 1-I regardless of architecture quality.
    let has_graded_abstract = non_foundation.iter().any(|m| m.abstract_count > 0);
    let distance_grade = if has_graded_abstract {
        distance_mod::grade_distance(avg_distance)
    } else {
        'A' // Unmeasurable — no abstract types in graded modules
    };
    (avg_distance, distance_grade)
}

/// Compute blast, surface, and composite architecture grades.
fn compute_arch_grades(
    snapshot: &Snapshot,
    edges: &[ImportEdge],
    blast_radius: &HashMap<String, u32>,
    levelization_grade: char,
    attack_surface_ratio: f64,
    distance_grade: char,
    module_depth: Option<usize>,
) -> (char, char, char) {
    let blast_grade = grade_blast_concentration_depth(blast_radius, edges, module_depth);
    // Applications (with main()) naturally have ~100% reachable code.
    // Grading attack surface for apps penalizes correct architecture.
    let surface_grade = if is_application(snapshot) {
        'A'
    } else {
        grade_attack_surface(attack_surface_ratio)
    };
    let arch_grade = composite_grade(levelization_grade, blast_grade, surface_grade, distance_grade);
    (blast_grade, surface_grade, arch_grade)
}
