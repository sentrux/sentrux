//! Path utilities for module boundary detection.
//!
//! Provides adaptive module name extraction from file paths using directory
//! depth heuristics. Handles depth-2 and depth-3 boundaries, dominant
//! directory stripping (src/, lib/), and extension removal. Used throughout
//! the metrics and analysis layers to assign files to logical modules.
//! Public functions: `module_of`, `module_of_at_depth`, `is_same_module`.

/// Adaptive module boundary detection.
///
/// Extract module name from a path using directory depth:
///   - Depth-3 when ≥3 directory levels exist (fine-grained sub-modules)
///   - Depth-2 when exactly 2 directory levels exist
///   - Parent directory for files under non-dominant dirs
///   - File stem for root-level files or files under dominant dirs (src, lib)
///
/// This works regardless of scan root:
///   Scanned from project root:
///     "src/analysis/scanner.rs"       → "src/analysis"
///     "src/analysis/parser/mod.rs"    → "src/analysis/parser"
///     "src/settings.rs"               → "src/settings"
///   Scanned from src/:
///     "analysis/scanner.rs"           → "analysis"
///     "analysis/parser/mod.rs"        → "analysis/parser"
///     "settings.rs"                   → "settings"
///
/// Full examples:
///   "src/layout/types.rs"             → "src/layout"
///   "src/layout/algo/squarify.rs"     → "src/layout/algo"  (depth-3)
///   "src/metrics/arch/graph.rs"       → "src/metrics/arch"  (depth-3)
///   "analysis/scanner.rs"             → "analysis"
///   "frontend/components/btn.js"      → "frontend/components"
///   "frontend/components/atoms/x.js"  → "frontend/components/atoms" (depth-3)
///   "frontend/index.js"               → "frontend"
///   "src/settings.rs"                 → "src/settings"
///   "db.rs"                           → "db"
/// Strip extension from a path, returning the stem.
/// Ensures the dot is after the last '/' to avoid stripping directory dots.
fn strip_extension(path: &str) -> &str {
    let last_sep = path.rfind('/').map_or(0, |i| i + 1);
    match path[last_sep..].rfind('.') {
        Some(dot) => &path[..last_sep + dot],
        None => path,
    }
}

/// Handle root-level files (no directory component).
fn module_of_root_file(path: &str) -> &str {
    strip_extension(path)
}

/// Handle files at >=2 directory levels.
/// Returns depth-3 module if 3+ levels (and max_depth >= 3), depth-2 module if exactly 2 levels
/// or if max_depth <= 2.
fn module_of_deep(path: &str, _first_slash: usize, depth2_end: usize, max_depth: usize) -> &str {
    if max_depth <= 2 {
        // Cap at depth-2: never look for a third slash
        return &path[..depth2_end];
    }
    let after_depth2 = &path[depth2_end + 1..];
    match after_depth2.find('/') {
        Some(j) => &path[..depth2_end + 1 + j],
        None => &path[..depth2_end],
    }
}

/// Directories that are "dominant" — flat files underneath get per-file modules.
fn is_dominant_dir(parent: &str) -> bool {
    matches!(parent, "src" | "lib" | "packages" | "cmd" | "pkg" | "internal")
}

/// Handle files directly under one directory level.
fn module_of_single_dir(path: &str, first_slash: usize) -> &str {
    let parent = &path[..first_slash];
    if is_dominant_dir(parent) {
        // "src/app.rs" → "src/app"
        match path.rfind('.') {
            Some(dot) if dot > first_slash => &path[..dot],
            _ => path,
        }
    } else {
        // "analysis/scanner.rs" → "analysis"
        parent
    }
}

/// Extract the module name from a file path using adaptive directory depth,
/// capped at `max_depth` directory levels.
///
/// When `max_depth <= 2`, the result is always a depth-2 prefix (no depth-3
/// sub-module detection). This is useful for monorepos where package boundaries
/// are at depth-2 and depth-3 produces overly fine-grained groupings.
///
/// When `max_depth >= 3`, behavior is identical to `module_of`.
pub fn module_of_at_depth(path: &str, max_depth: usize) -> &str {
    let first_slash = match path.find('/') {
        Some(i) => i,
        None => return module_of_root_file(path),
    };

    let rest = &path[first_slash + 1..];
    match rest.find('/') {
        Some(i) => module_of_deep(path, first_slash, first_slash + 1 + i, max_depth),
        None => module_of_single_dir(path, first_slash),
    }
}

/// Extract the module name from a file path using adaptive directory depth.
pub fn module_of(path: &str) -> &str {
    module_of_at_depth(path, 3)
}

/// Check if two file paths belong to the same module boundary.
#[allow(dead_code)] // Used by stability module for module boundary checks
pub fn is_same_module(path_a: &str, path_b: &str) -> bool {
    module_of(path_a) == module_of(path_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_2_grouping() {
        // Files at exactly 2 dir levels still group at depth-2
        assert_eq!(module_of("src/layout/types.rs"), "src/layout");
        assert_eq!(module_of("frontend/components/btn.js"), "frontend/components");
    }

    #[test]
    fn depth_3_grouping() {
        // Files at ≥3 dir levels get finer-grained depth-3 modules
        assert_eq!(module_of("src/layout/algo/squarify.rs"), "src/layout/algo");
        assert_eq!(module_of("src/metrics/arch/graph.rs"), "src/metrics/arch");
        assert_eq!(module_of("src/metrics/arch/tests.rs"), "src/metrics/arch");
        assert_eq!(module_of("frontend/components/atoms/btn.js"), "frontend/components/atoms");
        // Deeper nesting still caps at depth-3
        assert_eq!(module_of("a/b/c/d/e.rs"), "a/b/c");
    }

    #[test]
    fn depth_3_same_module() {
        // Files in the same depth-3 directory are the same module
        assert_eq!(
            module_of("src/metrics/arch/mod.rs"),
            module_of("src/metrics/arch/graph.rs")
        );
        assert_ne!(
            module_of("src/metrics/arch/graph.rs"),
            module_of("src/metrics/evo/evolution.rs")
        );
    }

    #[test]
    fn depth_2_and_3_are_different_modules() {
        // Flat files at depth-2 vs files in subdirs are different modules
        assert_ne!(
            module_of("src/metrics/types.rs"),       // "src/metrics"
            module_of("src/metrics/arch/graph.rs")   // "src/metrics/arch"
        );
    }

    #[test]
    fn dominant_dir_flat_files() {
        // Under dominant dirs, flat files get their own module
        assert_eq!(module_of("src/app.rs"), "src/app");
        assert_eq!(module_of("src/settings.rs"), "src/settings");
        assert_eq!(module_of("lib/utils.rs"), "lib/utils");
    }

    #[test]
    fn non_dominant_dir_groups_by_parent() {
        // Under non-dominant dirs, files group by parent directory
        assert_eq!(module_of("analysis/scanner.rs"), "analysis");
        assert_eq!(module_of("analysis/parser.rs"), "analysis");
        assert_eq!(module_of("metrics/arch.rs"), "metrics");
        assert_eq!(module_of("metrics/mod.rs"), "metrics");
        assert_eq!(module_of("core/types.rs"), "core");
        assert_eq!(module_of("app/mod.rs"), "app");
        assert_eq!(module_of("app/state.rs"), "app");
        assert_eq!(module_of("app/canvas.rs"), "app");
    }

    #[test]
    fn root_level_files() {
        assert_eq!(module_of("db.rs"), "db");
        assert_eq!(module_of("main.rs"), "main");
    }

    #[test]
    fn scan_from_project_root() {
        // When scanned from project root, src/ is dominant → depth-2 for flat files
        assert_eq!(module_of("src/analysis/scanner.rs"), "src/analysis");
        assert_eq!(module_of("src/analysis/parser.rs"), "src/analysis");
        assert_eq!(module_of("src/metrics/arch.rs"), "src/metrics");
        assert_eq!(module_of("src/app/state.rs"), "src/app");
        // Depth-3 subdirs get their own module
        assert_eq!(module_of("src/metrics/arch/graph.rs"), "src/metrics/arch");
        assert_eq!(module_of("src/analysis/parser/mod.rs"), "src/analysis/parser");
    }

    #[test]
    fn symmetry() {
        // Same module regardless of file
        assert_eq!(module_of("analysis/scanner.rs"), module_of("analysis/parser.rs"));
        assert_eq!(module_of("src/layout/types.rs"), module_of("src/layout/routing.rs"));
        // Different modules
        assert_ne!(module_of("analysis/scanner.rs"), module_of("metrics/arch.rs"));
        assert_ne!(module_of("src/app.rs"), module_of("src/settings.rs"));
    }

    #[test]
    fn module_of_at_depth_caps_at_depth2() {
        // depth-2: never go deeper than the second directory level
        assert_eq!(module_of_at_depth("backend/src/plugins/admin.ts", 2), "backend/src");
        // depth-3 (default): goes to third level
        assert_eq!(module_of_at_depth("backend/src/plugins/admin.ts", 3), "backend/src/plugins");
        // depth-2 on a 2-level path: same as depth-3
        assert_eq!(module_of_at_depth("backend/src/admin.ts", 2), "backend/src");
        // depth-2 on a root file: unchanged
        assert_eq!(module_of_at_depth("main.rs", 2), "main");
        // depth-2 on deeply nested path: still capped at depth-2
        assert_eq!(module_of_at_depth("a/b/c/d/e.rs", 2), "a/b");
        // module_of delegates to depth-3
        assert_eq!(module_of("backend/src/plugins/admin.ts"), module_of_at_depth("backend/src/plugins/admin.ts", 3));
    }
}
