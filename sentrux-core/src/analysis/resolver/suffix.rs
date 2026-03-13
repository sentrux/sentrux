//! Suffix-index import resolver — Tier 2 resolution for non-JS/TS languages.
//!
//! Resolves import specifiers by matching against a suffix index of all known
//! file paths. Handles relative imports, language-specific conventions (Rust mod,
//! Python package, Go package), and monorepo project boundaries.

use crate::core::types::ImportEdge;
use crate::core::types::FileNode;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::helpers::{
    resolve_relative, try_resolve_name,
    try_suffix_resolve, file_to_module_path, SuffixIndex, ResolveEnv,
};
// Re-export normalize_path so existing callers (tests, graph) still find it here.
pub(crate) use super::helpers::normalize_path;

/// Source file context for import resolution.
pub(crate) struct SourceContext<'a> {
    /// The import specifier string to resolve
    pub specifier: &'a str,
    /// The file containing this import statement
    pub file: &'a FileNode,
    /// Parent directory of the importing file
    pub file_dir: &'a Path,
    /// Project root this file belongs to (for boundary filtering)
    pub src_project: &'a str,
}

/// Shared indexes used for resolution lookups.
pub(crate) struct ResolutionIndex<'a> {
    /// Map from file path to its project root
    pub project_map: &'a HashMap<String, String>,
    #[allow(dead_code)]
    /// Set of all known file paths in the scan (reserved for future resolution strategies)
    pub known_files: &'a HashSet<&'a str>,
    #[allow(dead_code)]
    /// Module-path suffix index for fuzzy file matching (reserved for future resolution strategies)
    pub suffix_index: &'a SuffixIndex<'a>,
}

/// Atomic counters for resolution statistics.
pub(crate) struct ResolutionStats {
    /// Number of imports successfully resolved to a file
    pub resolved_count: std::sync::atomic::AtomicUsize,
    /// Number of imports that could not be resolved
    pub unresolved_count: std::sync::atomic::AtomicUsize,
    /// Number of imports filtered out by project boundary
    pub cross_project_count: std::sync::atomic::AtomicUsize,
}

impl ResolutionStats {
    /// Create a new zeroed ResolutionStats.
    pub fn new() -> Self {
        Self {
            resolved_count: std::sync::atomic::AtomicUsize::new(0),
            unresolved_count: std::sync::atomic::AtomicUsize::new(0),
            cross_project_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

/// Manifest files that mark a project boundary.
/// When the scan root contains multiple projects (monorepo), each manifest
/// defines a separate project. Imports only resolve within the same project.
/// Only manifests that truly define a project boundary. Makefile and
/// CMakeLists.txt are excluded: they routinely appear at multiple directory
/// levels within a single project (CMake per-directory, recursive Make),
/// causing the boundary gate to silently drop valid cross-directory imports.
const MANIFEST_FILES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "mix.exs",
];

/// Tier 1 + Tier 2 synchronous import resolution (zero-copy: accepts &[&FileNode]).
/// Tier 1: oxc_resolver for JS/TS/JSX/TSX -- accurate module resolution.
/// Tier 2: suffix-index + file-path join for everything else.
/// Returns empty if scan_root is None.
pub(crate) fn resolve_path_imports_ref(files: &[&FileNode], scan_root: Option<&Path>) -> Vec<ImportEdge> {
    let t0 = std::time::Instant::now();
    let scan_root = match scan_root {
        Some(r) => r,
        None => return Vec::new(),
    };

    // Build known_files ONCE -- shared by oxc resolver and suffix resolver
    let known_files: HashSet<&str> = files
        .iter()
        .filter(|f| !f.is_dir)
        .map(|f| f.path.as_str())
        .collect();

    // Compute extensions ONCE -- sorted for deterministic resolution
    let mut exts = crate::analysis::lang_registry::all_extensions();
    exts.sort_unstable();
    let exts = exts;

    // Detect project boundaries -- imports only resolve within the same project
    let project_map = build_project_map(files, scan_root);
    let t_project_map = t0.elapsed();

    // JS/TS: dedicated oxc_resolver
    let mut edges = resolve_js_ts_with_boundary(scan_root, files, &known_files, &project_map);
    let t_oxc = t0.elapsed();

    // Tier 2: suffix-index resolution for non-JS/TS languages
    let suffix_index = build_module_suffix_index(&known_files, scan_root, &project_map);
    let t_suffix = t0.elapsed();
    let tier2_edges = resolve_tier2_imports(files, &known_files, &project_map, &suffix_index, &exts);
    edges.extend(tier2_edges);

    let t_total = t0.elapsed();
    eprintln!(
        "[resolve_imports] project_map {:.1}ms, oxc {:.1}ms, suffix_idx {:.1}ms, suffix_resolve {:.1}ms, total {:.1}ms",
        t_project_map.as_secs_f64() * 1000.0,
        (t_oxc - t_project_map).as_secs_f64() * 1000.0,
        (t_suffix - t_oxc).as_secs_f64() * 1000.0,
        (t_total - t_suffix).as_secs_f64() * 1000.0,
        t_total.as_secs_f64() * 1000.0,
    );

    edges
}

/// Resolve JS/TS imports via oxc_resolver, then filter by project boundary.
fn resolve_js_ts_with_boundary(
    scan_root: &Path,
    files: &[&FileNode],
    known_files: &HashSet<&str>,
    project_map: &HashMap<String, String>,
) -> Vec<ImportEdge> {
    let mut edges = super::oxc::resolve_js_ts_imports(scan_root, files, known_files);
    // Allow imports into the root project (empty string) from any sub-project.
    edges.retain(|e| {
        let from_proj = project_map.get(&e.from_file).map(|s| s.as_str()).unwrap_or("");
        let to_proj = project_map.get(&e.to_file).map(|s| s.as_str()).unwrap_or("");
        from_proj == to_proj || to_proj.is_empty()
    });
    edges
}

/// Resolve a single import specifier for a file and classify the result.
/// Returns Some(ImportEdge) if resolved within the same project, None otherwise.
fn resolve_single_specifier(
    src: &SourceContext<'_>,
    idx: &ResolutionIndex<'_>,
    env: &ResolveEnv<'_>,
    stats: &ResolutionStats,
) -> Option<ImportEdge> {
    if src.specifier.starts_with('<') {
        return None;
    }
    let resolved = resolve_module_import(src.specifier, src.file_dir, env, &src.file.lang);
    match resolved {
        Some(target) if target != src.file.path => {
            let dst_project = idx.project_map.get(&target).map(|s| s.as_str()).unwrap_or("");
            if src.src_project == dst_project || dst_project.is_empty() {
                stats.resolved_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Some(ImportEdge { from_file: src.file.path.clone(), to_file: target })
            } else {
                stats.cross_project_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                None
            }
        }
        None => {
            stats.unresolved_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        }
        _ => None, // self-import, skip silently
    }
}

/// Resolve non-JS/TS imports in parallel using suffix-index and relative-path strategies.
fn resolve_tier2_imports(
    files: &[&FileNode],
    known_files: &HashSet<&str>,
    project_map: &HashMap<String, String>,
    suffix_index: &SuffixIndex<'_>,
    exts: &[&str],
) -> Vec<ImportEdge> {
    let js_ts = ["javascript", "typescript", "jsx", "tsx"];
    let stats = ResolutionStats::new();
    let idx = ResolutionIndex { known_files, project_map, suffix_index };
    let env = ResolveEnv { suffix_index, known_files, exts };
    let edges: Vec<ImportEdge> = files
        .par_iter()
        .filter(|f| !f.is_dir && !js_ts.contains(&f.lang.as_str()))
        .flat_map_iter(|file| {
            let imports = match file.sa.as_ref().and_then(|sa| sa.imp.as_ref()) {
                Some(imp) => imp,
                None => return Vec::new(),
            };
            let file_dir = Path::new(&file.path).parent().unwrap_or(Path::new(""));
            let src_project = project_map.get(&file.path).map(|s| s.as_str()).unwrap_or("");

            imports.iter()
                .filter_map(|specifier| {
                    let src = SourceContext { specifier, file, file_dir, src_project };
                    resolve_single_specifier(&src, &idx, &env, &stats)
                })
                .collect()
        })
        .collect();

    let unresolved = stats.unresolved_count.load(std::sync::atomic::Ordering::Relaxed);
    let cross_proj = stats.cross_project_count.load(std::sync::atomic::Ordering::Relaxed);
    let resolved = stats.resolved_count.load(std::sync::atomic::Ordering::Relaxed);
    if unresolved > 0 || cross_proj > 0 {
        eprintln!(
            "[resolve_tier2] {} resolved, {} unresolved, {} cross-project filtered",
            resolved, unresolved, cross_proj
        );
    }
    edges
}

/// Backfill all visited directories with the found project root.
fn backfill_cache(cache: &mut HashMap<String, String>, visited: &[String], result: &str) {
    for v in visited {
        cache.insert(v.clone(), result.to_string());
    }
}

/// Check if any manifest file exists in the given directory.
fn has_manifest(dir: &Path) -> bool {
    MANIFEST_FILES.iter().any(|manifest| dir.join(manifest).exists())
}

/// Detect which project a file belongs to by walking up from its directory
/// to find the nearest manifest file. Caches ALL intermediate directories
/// visited during the walk so sibling files sharing ancestor dirs skip the
/// filesystem entirely (previous code only cached the leaf dir).
fn detect_project_root_cached(
    file_rel_path: &str,
    scan_root: &Path,
    cache: &mut HashMap<String, String>,
) -> String {
    let abs = scan_root.join(file_rel_path);
    let mut dir = abs.parent().unwrap_or(scan_root).to_path_buf();
    let mut visited: Vec<String> = Vec::new();

    while dir.starts_with(scan_root) {
        let rel = dir.strip_prefix(scan_root)
            .unwrap_or(&dir)
            .to_string_lossy()
            .to_string();

        // Cache hit on intermediate dir -> backfill all visited dirs
        if let Some(cached) = cache.get(&rel) {
            let result = cached.clone();
            backfill_cache(cache, &visited, &result);
            return result;
        }

        if has_manifest(&dir) {
            cache.insert(rel.clone(), rel.clone());
            backfill_cache(cache, &visited, &rel);
            return rel;
        }

        visited.push(rel);
        if dir == *scan_root || !dir.pop() {
            break;
        }
    }

    // No manifest found -- treat everything as one project
    backfill_cache(cache, &visited, "");
    String::new()
}

/// Build project membership map: file_path -> project_root.
/// Computed once per scan for all files. Caches intermediate directories
/// to avoid redundant filesystem walks up shared ancestor paths.
fn build_project_map(files: &[&FileNode], scan_root: &Path) -> HashMap<String, String> {
    let t0 = std::time::Instant::now();
    let mut dir_cache: HashMap<String, String> = HashMap::new();
    let mut project_map = HashMap::new();
    let mut cache_misses = 0usize;

    for file in files {
        if file.is_dir { continue; }
        let dir = Path::new(&file.path)
            .parent()
            .unwrap_or(Path::new(""))
            .to_string_lossy()
            .to_string();
        let project_root = if let Some(cached) = dir_cache.get(&dir) {
            cached.clone()
        } else {
            cache_misses += 1;
            detect_project_root_cached(&file.path, scan_root, &mut dir_cache)
        };
        project_map.insert(file.path.clone(), project_root);
    }
    eprintln!(
        "[build_project_map] {} files, {} unique dirs, {} cache misses, {:.1}ms",
        files.len(), dir_cache.len(), cache_misses, t0.elapsed().as_secs_f64() * 1000.0
    );
    project_map
}

/// Add all suffixes of a module path to the index, pointing to the given file.
/// e.g. "a/b/c" generates suffixes ["a/b/c", "b/c", "c"].
fn add_module_suffixes<'a>(index: &mut HashMap<String, Vec<&'a str>>, module_path: &str, file_path: &'a str) {
    let mut pos = 0;
    loop {
        let suffix = &module_path[pos..];
        if !suffix.is_empty() {
            index.entry(suffix.to_string()).or_default().push(file_path);
        }
        match module_path[pos..].find('/') {
            Some(slash) => pos += slash + 1,
            None => break,
        }
    }
}

/// Map every suffix of every file's module path to that file.
/// e.g. "a/b/c.py" -> suffixes ["c", "b/c", "a/b/c"] all point to "a/b/c.py".
///
/// Package index files use their parent directory as the module path:
///   __init__.py, mod.rs, index.js, index.ts, etc.
/// This is detected from the filename -- no language knowledge needed.
fn build_module_suffix_index<'a>(known_files: &HashSet<&'a str>, scan_root: &Path, project_map: &HashMap<String, String>) -> SuffixIndex<'a> {
    let mut index: HashMap<String, Vec<&'a str>> = HashMap::new();
    for &file_path in known_files {
        let module_path = file_to_module_path(file_path);
        if module_path.is_empty() {
            continue;
        }

        add_module_suffixes(&mut index, module_path, file_path);

        // Go imports reference packages (directories), not individual files.
        // e.g. `import "internal/config"` means the package in internal/config/.
        // Unlike Python (__init__.py) or Rust (mod.rs), Go has no package index
        // file — any .go file in a directory is part of the package.
        // Add parent-directory suffixes so Go package imports can resolve.
        if file_path.ends_with(".go") {
            if let Some((parent, _)) = module_path.rsplit_once('/') {
                if !parent.is_empty() {
                    add_module_suffixes(&mut index, parent, file_path);
                }
            }
        }
    }

    // Manifest-derived aliases: project name -> entry point file.
    let mut manifest_aliases: HashMap<String, Vec<&'a str>> = HashMap::new();
    inject_manifest_aliases(&mut manifest_aliases, known_files, scan_root);

    // Go module prefixes: parse go.mod files to map module paths to project dirs.
    let go_module_prefixes = collect_go_module_prefixes(project_map, scan_root);

    SuffixIndex { index, manifest_aliases, go_module_prefixes }
}

/// Extract the module path from a go.mod file content.
/// Parses `module github.com/user/repo` from the first `module` directive.
fn extract_go_module_name(content: &str) -> Option<&str> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("module") {
            let rest = rest.trim();
            if rest.is_empty() { continue; }
            // Module path is the first token (no quotes in go.mod)
            return Some(rest.split_whitespace().next().unwrap_or(rest));
        }
    }
    None
}

/// Scan project roots for go.mod files and build a map of Go module paths to project directories.
/// Uses the project_map to find unique project roots, then checks each for go.mod.
/// Sorted longest-first so more specific module paths match before shorter ones.
fn collect_go_module_prefixes(project_map: &HashMap<String, String>, scan_root: &Path) -> Vec<(String, String)> {
    let unique_roots: HashSet<&str> = project_map.values().map(|s| s.as_str()).collect();
    let mut prefixes = Vec::new();

    for &project_dir in &unique_roots {
        let go_mod_path = if project_dir.is_empty() {
            scan_root.join("go.mod")
        } else {
            scan_root.join(project_dir).join("go.mod")
        };

        if let Ok(content) = std::fs::read_to_string(&go_mod_path) {
            if let Some(module_name) = extract_go_module_name(&content) {
                prefixes.push((module_name.to_string(), project_dir.to_string()));
            }
        }
    }
    // Sort longest module path first for greedy matching
    prefixes.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    prefixes
}

/// Derive the project directory from a lib.rs file path.
/// Returns None if the path doesn't end with /lib.rs or isn't exactly "lib.rs".
fn lib_rs_project_dir(file_path: &str) -> Option<&str> {
    if file_path == "lib.rs" {
        return Some("");
    }
    let src_dir = file_path.strip_suffix("/lib.rs")?;
    if src_dir == "src" {
        Some("")
    } else {
        Some(src_dir.strip_suffix("/src").unwrap_or(src_dir))
    }
}

/// Try to read a crate name from Cargo.toml and insert it as an alias.
fn try_insert_cargo_alias<'a>(
    index: &mut HashMap<String, Vec<&'a str>>,
    file_path: &'a str,
    cargo_toml: &Path,
) {
    match std::fs::read_to_string(cargo_toml) {
        Ok(content) => {
            if let Some(name) = extract_cargo_name(&content) {
                let crate_name = name.replace('-', "_");
                index.entry(crate_name).or_default().push(file_path);
            }
        }
        Err(e) => {
            if cargo_toml.exists() {
                eprintln!("[graph] failed to read {}: {} (crate alias skipped)", cargo_toml.display(), e);
            }
        }
    }
}

/// Read manifest files and add project-name -> entry-point aliases to the suffix index.
fn inject_manifest_aliases<'a>(
    index: &mut HashMap<String, Vec<&'a str>>,
    known_files: &HashSet<&'a str>,
    scan_root: &Path,
) {
    for &file_path in known_files {
        let filename = file_path.rsplit('/').next().unwrap_or(file_path);
        if filename != "lib.rs" {
            continue;
        }
        let project_dir = match lib_rs_project_dir(file_path) {
            Some(d) => d,
            None => continue,
        };
        let cargo_toml = scan_root.join(project_dir).join("Cargo.toml");
        try_insert_cargo_alias(index, file_path, &cargo_toml);
    }
}

/// Parse a TOML string value: handles double-quoted, single-quoted, and bare values.
fn parse_toml_string_value(rest: &str) -> Option<&str> {
    if let Some(inner) = rest.strip_prefix('"') {
        inner.find('"').map(|i| &inner[..i])
    } else if let Some(inner) = rest.strip_prefix('\'') {
        inner.find('\'').map(|i| &inner[..i])
    } else {
        Some(rest.split_whitespace().next().unwrap_or(""))
    }
}

/// Extract `name = "..."` from Cargo.toml [package] section.
fn extract_cargo_name(content: &str) -> Option<String> {
    let mut in_package = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package { continue; }
        let rest = match trimmed.strip_prefix("name") {
            Some(r) => r,
            None => continue,
        };
        let rest = match rest.trim_start().strip_prefix('=') {
            Some(r) => r.trim(),
            None => continue,
        };
        if let Some(n) = parse_toml_string_value(rest) {
            if !n.is_empty() {
                return Some(n.to_string());
            }
        }
    }
    None
}

/// Language-agnostic module resolver.
///
/// Resolution strategy (tried in order):
///   1. Relative (leading '.') -> resolve from file dir
///   2. Multi-segment absolute -> suffix-index with progressive prefix stripping
///   3. Single-segment -> dir-relative, then root-relative
///   4. Package index files -> try __init__.py, mod.rs, index.{js,ts,...} for dirs
///
/// Key design rule: single-segment absolute imports never use suffix-index.
fn resolve_module_import(
    specifier: &str,
    file_dir: &Path,
    env: &ResolveEnv<'_>,
    _lang: &str,
) -> Option<String> {
    if specifier.is_empty() {
        return None;
    }

    // 1. Relative imports (leading dots)
    if specifier.starts_with('.') {
        return resolve_relative(specifier, file_dir, env.known_files, env.exts);
    }

    // 2. Direct file path check
    {
        let cleaned = specifier.trim_start_matches("./").trim_start_matches('/');
        let joined = file_dir.join(cleaned);
        let normalized = normalize_path(&joined);
        if env.known_files.contains(normalized.as_str()) {
            return Some(normalized);
        }
        let from_root = normalize_path(Path::new(cleaned));
        if env.known_files.contains(from_root.as_str()) {
            return Some(from_root);
        }
    }

    // 3+4. Module-name resolution
    let file_dir_str = file_dir.to_str().unwrap_or("");

    if specifier.contains('/') {
        if let Some(found) = try_suffix_resolve(specifier, env, file_dir_str, file_dir) {
            return Some(found);
        }

        // Previously fell back to parent module when submodule didn't resolve,
        // creating false-positive import edges. Removed: if the exact specifier
        // doesn't resolve, return None rather than silently return the wrong file.
        // [ref:4540215f]
    }

    // Single-segment: try dir-relative first (handles `mod foo` -> foo.rs)
    if let Some(found) = try_resolve_name(specifier, file_dir, env.known_files, env.exts) {
        return Some(found);
    }
    // Then root-relative
    if let Some(found) = try_resolve_name(specifier, Path::new(""), env.known_files, env.exts) {
        return Some(found);
    }
    // Finally, manifest-derived aliases
    if let Some(candidates) = env.suffix_index.manifest_aliases.get(specifier) {
        if candidates.len() == 1 {
            return Some(candidates[0].to_string());
        }
    }
    None
}

