//! Types and constants for code health metrics.
//!
//! Defines thresholds (cyclomatic complexity, function length, fan-out),
//! report structures (`HealthReport`, `GodFile`, `HotspotFile`), and score
//! thresholds used throughout the metrics layer. All magic numbers are named
//! constants with literature references (McCabe 1976, Myers 1977, Martin).
//! Key types: `HealthReport`, `GodFile`, `HotspotFile`, `ComplexFuncInfo`.

use std::collections::HashMap;

// ── Thresholds are now per-language, read from LanguageProfile ──
// See analysis/plugin/profile.rs for defaults (LanguageThresholds::default()).
// Per-language overrides come from plugin.toml [thresholds].
// Project-level overrides come from .sentrux/rules.toml.

/// Complete health report for a codebase snapshot.
/// Aggregates all health dimensions into a single quality signal [0,1].
#[derive(Debug, Clone)]
pub struct HealthReport {
    // ── Graph-level metrics ──
    /// Coupling score: ratio of cross-module edges to unstable targets (0.0-1.0)
    pub coupling_score: f64,
    /// Number of strongly connected components (circular dependency cycles)
    pub circular_dep_count: usize,
    /// Files involved in each circular dependency cycle
    pub circular_dep_files: Vec<Vec<String>>,
    /// Shortest cycle path for each SCC (e.g. ["A", "B", "C", "A"])
    pub circular_dep_paths: Vec<Vec<String>>,
    /// Total import edges in the dependency graph
    pub total_import_edges: usize,
    /// Import edges that cross module boundaries
    pub cross_module_edges: usize,

    // ── Entropy & structure metrics ──
    /// Normalized Shannon entropy of cross-module edge distribution (0.0-1.0)
    pub entropy: f64,
    /// Raw Shannon entropy in bits (before normalization)
    #[allow(dead_code)] // Exposed via MCP health endpoint
    pub entropy_bits: f64,
    /// Average intra-module cohesion (None if no modules with 2+ files)
    pub avg_cohesion: Option<f64>,
    /// Maximum dependency depth in the DAG
    pub max_depth: u32,

    // ── Per-file metrics ──
    /// Files with fan-out exceeding threshold (god files)
    pub god_files: Vec<FileMetric>,
    /// Files with high fan-in that are also unstable (hotspots)
    pub hotspot_files: Vec<FileMetric>,
    /// Top 10 most unstable files by Martin's instability metric
    pub most_unstable: Vec<InstabilityMetric>,

    // ── Per-function metrics ──
    /// Functions with cyclomatic complexity > 15
    pub complex_functions: Vec<FuncMetric>,
    /// Functions with length > 50 lines
    pub long_functions: Vec<FuncMetric>,
    /// Functions with cognitive complexity > 15
    pub cog_complex_functions: Vec<FuncMetric>,
    /// Functions with > 4 parameters
    pub high_param_functions: Vec<FuncMetric>,
    /// Groups of functions with identical body hashes
    pub duplicate_groups: Vec<DuplicateGroup>,
    /// Functions not referenced by any call site
    pub dead_functions: Vec<FuncMetric>,

    // ── Per-file line counts (for rules enforcement) ──
    /// Files exceeding the large file threshold (> 500 lines)
    pub long_files: Vec<FileMetric>,

    // ── Raw data for rules engine (unfiltered, all functions/files) ──
    /// ALL function cyclomatic complexities: (file, func_name, cc)
    pub all_function_ccs: Vec<FuncMetric>,
    /// ALL function line counts: (file, func_name, lines)
    pub all_function_lines: Vec<FuncMetric>,
    /// ALL file line counts: (path, lines)
    pub all_file_lines: Vec<FileMetric>,

    // ── Ratios (used by grading + UI) ──
    /// God files / total files
    pub god_file_ratio: f64,
    /// Hotspot files / total files
    pub hotspot_ratio: f64,
    /// Complex functions / total functions
    pub complex_fn_ratio: f64,
    /// Long functions / total functions
    pub long_fn_ratio: f64,
    /// Comments / total lines (None if no code files)
    pub comment_ratio: Option<f64>,
    /// Number of files exceeding the large file threshold
    pub large_file_count: usize,
    /// Large files / total files
    pub large_file_ratio: f64,
    /// Duplicate functions / total functions
    pub duplication_ratio: f64,
    /// Dead functions / total functions
    pub dead_code_ratio: f64,
    /// High-param functions / total functions
    pub high_param_ratio: f64,
    /// Cognitively complex functions / total functions
    pub cog_complex_ratio: f64,

    // ── Root Cause Scores (6 fundamental metrics) ──
    /// Quality signal ∈ [0,1]: geometric mean of 6 root cause scores.
    /// THE one number AI agents maximize. Higher = better architecture.
    pub quality_signal: f64,
    /// Raw root cause values (un-normalized, for display)
    pub root_cause_raw: super::root_causes::RootCauseRaw,
    /// Normalized root cause scores ∈ [0,1] (for signal computation)
    pub root_cause_scores: super::root_causes::RootCauseScores,

}

/// A file-level metric: path + numeric value (e.g., fan-out count, line count).
#[derive(Debug, Clone)]
pub struct FileMetric {
    /// Relative file path
    pub path: String,
    /// Metric value (fan-out, fan-in, line count, etc.)
    pub value: usize,
}

/// A function-level metric: file + function name + numeric value.
#[derive(Debug, Clone)]
pub struct FuncMetric {
    /// File containing the function
    pub file: String,
    /// Function name
    pub func: String,
    /// Metric value (cyclomatic complexity or line count)
    pub value: u32,
}

/// Robert C. Martin's Instability metric: I = Ce / (Ca + Ce)
/// Ce = efferent coupling (fan-out), Ca = afferent coupling (fan-in).
/// 0.0 = maximally stable (only depended on, never depends out).
/// 1.0 = maximally unstable (depends on everything, nothing depends on it).
#[derive(Debug, Clone)]
pub struct InstabilityMetric {
    /// File path
    pub path: String,
    /// Instability value (0.0-1.0)
    pub instability: f64,
    /// Afferent coupling: number of files that depend on this file
    pub fan_in: usize,
    /// Efferent coupling: number of files this file depends on
    pub fan_out: usize,
}

/// A group of functions with identical body hashes (copy-paste duplicates).
#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    /// Body hash shared by all instances in this group
    #[allow(dead_code)]
    pub hash: u64,
    /// Duplicate instances: (file_path, func_name, line_count)
    pub instances: Vec<(String, String, u32)>,
}

/// Intermediate results from per-file analysis: fan-in/fan-out, god files,
/// hotspots, instability, per-function complexity/length, and derived ratios.
pub(crate) struct FileMetrics {
    #[allow(dead_code)]
    pub(crate) fan_out: HashMap<String, usize>,
    #[allow(dead_code)]
    pub(crate) fan_in: HashMap<String, usize>,
    pub(crate) god_files: Vec<FileMetric>,
    pub(crate) hotspot_files: Vec<FileMetric>,
    pub(crate) most_unstable: Vec<InstabilityMetric>,
    pub(crate) complex_functions: Vec<FuncMetric>,
    pub(crate) long_functions: Vec<FuncMetric>,
    pub(crate) long_files: Vec<FileMetric>,
    pub(crate) complex_fn_ratio: f64,
    pub(crate) long_fn_ratio: f64,
    pub(crate) comment_ratio: Option<f64>,
    pub(crate) large_file_count: usize,
    pub(crate) large_file_ratio: f64,
    pub(crate) god_ratio: f64,
    pub(crate) hotspot_ratio: f64,
    pub(crate) cog_complex_functions: Vec<FuncMetric>,
    pub(crate) high_param_functions: Vec<FuncMetric>,
    pub(crate) duplicate_groups: Vec<DuplicateGroup>,
    pub(crate) dead_functions: Vec<FuncMetric>,
    pub(crate) duplication_ratio: f64,
    pub(crate) dead_code_ratio: f64,
    pub(crate) high_param_ratio: f64,
    pub(crate) cog_complex_ratio: f64,
}

/// Intermediate results from module/dependency-level analysis.
pub(crate) struct ModuleMetrics {
    pub(crate) coupling_score: f64,
    pub(crate) cross_module_edges: usize,
    pub(crate) entropy: f64,
    pub(crate) entropy_bits: f64,
    pub(crate) entropy_num_pairs: usize,
    pub(crate) avg_cohesion: Option<f64>,
    pub(crate) max_depth: u32,
    pub(crate) circular_dep_files: Vec<Vec<String>>,
    pub(crate) circular_dep_paths: Vec<Vec<String>>,
    pub(crate) circular_dep_count: usize,
}

/// Mod declaration files aggregated from all plugins. Cached at first access.
static MOD_DECL_FILES: std::sync::LazyLock<std::collections::HashSet<String>> =
    std::sync::LazyLock::new(|| {
        crate::analysis::lang_registry::all_mod_declaration_files()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    });

/// Is this edge a module declaration (structural containment, not a real dependency)?
/// Examples: Rust `mod xxx;`, Python `__init__.py` re-exports.
/// Kept for coupling/cohesion but excluded from cycle/god_file/depth metrics.
pub(crate) fn is_mod_declaration_edge(edge: &crate::core::types::ImportEdge) -> bool {
    let from_name = edge.from_file.rsplit('/').next().unwrap_or(&edge.from_file);
    if !MOD_DECL_FILES.contains(from_name) {
        return false;
    }
    let from_dir = edge.from_file.rfind('/').map(|i| &edge.from_file[..i]).unwrap_or("");
    let to_dir = edge.to_file.rfind('/').map(|i| &edge.to_file[..i]).unwrap_or("");
    // Same directory: "src/app/mod.rs" → "src/app/state.rs"
    // Guard: both dirs must be non-empty to prevent false positives when files are at root level
    // (e.g., "lib.rs" → "foo.rs" both have from_dir="" and to_dir="", but this is NOT a mod declaration)
    if from_dir == to_dir && !from_dir.is_empty() {
        return true;
    }
    // Parent→child: "src/app/mod.rs" → "src/app/mcp_server/mod.rs"
    // from_dir is a prefix of to_dir (the target is in a subdirectory)
    // Guard: from_dir must not be empty (prevents matching across workspace crates
    // where e.g. "crates/foo/src/lib.rs" has from_dir="" matching any to_dir)
    if !from_dir.is_empty()
        && to_dir.starts_with(from_dir)
        && to_dir.as_bytes().get(from_dir.len()) == Some(&b'/')
    {
        // Only count as mod-declaration if to_file is a direct child subdir's mod.rs
        // or a file directly one level deeper
        let remainder = &to_dir[from_dir.len() + 1..];
        // No further slashes = direct child subdir
        if !remainder.contains('/') {
            return true;
        }
    }
    false
}

