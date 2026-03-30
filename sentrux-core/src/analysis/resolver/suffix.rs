//! Unified import resolver — suffix-index for ALL languages.
//!
//! Resolves import specifiers by matching against a suffix index of all known
//! file paths. Handles relative imports, path aliases (from plugin-declared
//! config files like tsconfig.json), and monorepo project boundaries.

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
}

impl ResolutionStats {
    /// Create a new zeroed ResolutionStats.
    pub fn new() -> Self {
        Self {
            resolved_count: std::sync::atomic::AtomicUsize::new(0),
            unresolved_count: std::sync::atomic::AtomicUsize::new(0),
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
/// Manifest files aggregated from all loaded plugins. Cached at first access.
static MANIFEST_FILES: std::sync::LazyLock<Vec<String>> =
    std::sync::LazyLock::new(|| {
        crate::analysis::lang_registry::all_manifest_files()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    });

/// Unified import resolution for ALL languages via suffix-index.
/// No tier split — JS/TS goes through the same resolver with path alias support.
pub(crate) fn resolve_path_imports_ref(files: &[&FileNode], scan_root: Option<&Path>) -> Vec<ImportEdge> {
    let t0 = std::time::Instant::now();
    let scan_root = match scan_root {
        Some(r) => r,
        None => return Vec::new(),
    };

    let known_files: HashSet<&str> = files
        .iter()
        .filter(|f| !f.is_dir)
        .map(|f| f.path.as_str())
        .collect();

    let mut exts = crate::analysis::lang_registry::all_extensions();
    exts.sort_unstable();

    let project_map = build_project_map(files, scan_root);
    let t_project_map = t0.elapsed();

    let suffix_index = build_module_suffix_index(&known_files, scan_root, &project_map);

    // Load path aliases from two sources:
    // 1. Config files (tsconfig.json paths) — declared in plugin.toml
    // 2. Manifest names (package.json "name", Cargo.toml "package.name") — auto-discovered
    let mut path_aliases = load_path_aliases(&project_map, scan_root);
    let manifest_aliases = collect_manifest_path_aliases(&project_map, scan_root);
    if !manifest_aliases.is_empty() {
        path_aliases.entry(String::new()).or_default().extend(manifest_aliases);
    }
    let t_suffix = t0.elapsed();

    let edges = resolve_tier2_imports(files, &known_files, &project_map, &suffix_index, &exts, &path_aliases);
    let t_total = t0.elapsed();

    eprintln!(
        "[resolve_imports] project_map {:.1}ms, suffix_idx {:.1}ms, suffix_resolve {:.1}ms, total {:.1}ms",
        t_project_map.as_secs_f64() * 1000.0,
        (t_suffix - t_project_map).as_secs_f64() * 1000.0,
        (t_total - t_suffix).as_secs_f64() * 1000.0,
        t_total.as_secs_f64() * 1000.0,
    );

    edges
}

/// Resolve a single import specifier for a file and classify the result.
/// Returns Some(ImportEdge) if resolved within the same project, None otherwise.
fn resolve_single_specifier(
    src: &SourceContext<'_>,
    _idx: &ResolutionIndex<'_>,
    env: &ResolveEnv<'_>,
    stats: &ResolutionStats,
) -> Option<ImportEdge> {
    if src.specifier.starts_with('<') {
        return None;
    }
    let resolved = resolve_module_import(src.specifier, src.file_dir, env, &src.file.lang);
    match resolved {
        Some(target) if target != src.file.path => {
            // Accept ALL resolved edges. The user chose to scan this directory —
            // everything in it is their project. Cross-sub-project imports are
            // real dependencies that the tool should show, not hide.
            stats.resolved_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(ImportEdge { from_file: src.file.path.clone(), to_file: target })
        }
        None => {
            stats.unresolved_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        }
        _ => None,
    }
}

/// Resolve non-JS/TS imports in parallel using suffix-index and relative-path strategies.
fn resolve_tier2_imports(
    files: &[&FileNode],
    known_files: &HashSet<&str>,
    project_map: &HashMap<String, String>,
    suffix_index: &SuffixIndex<'_>,
    exts: &[&str],
    path_aliases: &HashMap<String, Vec<PathAlias>>,
) -> Vec<ImportEdge> {
    let stats = ResolutionStats::new();
    let idx = ResolutionIndex { known_files, project_map, suffix_index };
    let edges: Vec<ImportEdge> = files
        .par_iter()
        .filter(|f| !f.is_dir)
        .flat_map_iter(|file| {
            let imports = match file.sa.as_ref().and_then(|sa| sa.imp.as_ref()) {
                Some(imp) => imp,
                None => return Vec::new(),
            };
            // Per-file env: directory_is_package comes from the file's language profile (TOML)
            let profile = crate::analysis::lang_registry::profile(&file.lang);
            let env = ResolveEnv {
                suffix_index, known_files, exts,
                directory_is_package: profile.semantics.project.directory_is_package,
            };
            let file_dir = Path::new(&file.path).parent().unwrap_or(Path::new(""));
            let src_project = project_map.get(&file.path).map(|s| s.as_str()).unwrap_or("");

            // Get path aliases for this file's project
            let project_aliases = path_aliases.get(src_project).map(|v| v.as_slice()).unwrap_or(&[]);

            // Also try root-level aliases (from manifest name discovery)
            let root_aliases = path_aliases.get("").map(|v| v.as_slice()).unwrap_or(&[]);

            imports.iter()
                .filter_map(|specifier| {
                    // Try alias-substituted specifier first, fall back to original.
                    // Aliases are hints, not absolute — if the substituted path
                    // doesn't exist (source not in src/), the original may still resolve.
                    let alias_specs = [
                        apply_path_alias(specifier, project_aliases),
                        apply_path_alias(specifier, root_aliases),
                    ];
                    for aliased in &alias_specs {
                        if let Some(ref spec) = aliased {
                            let src = SourceContext { specifier: spec, file, file_dir, src_project };
                            if let Some(edge) = resolve_single_specifier(&src, &idx, &env, &stats) {
                                return Some(edge);
                            }
                        }
                    }
                    // Fall back to original specifier
                    let src = SourceContext { specifier, file, file_dir, src_project };
                    resolve_single_specifier(&src, &idx, &env, &stats)
                })
                .collect()
        })
        .collect();

    let unresolved = stats.unresolved_count.load(std::sync::atomic::Ordering::Relaxed);
    let resolved = stats.resolved_count.load(std::sync::atomic::Ordering::Relaxed);
    let total_specs = resolved + unresolved;
    if total_specs > 0 {
        eprintln!(
            "[resolve] {} resolved, {} unresolved (of {} total specs)",
            resolved, unresolved, total_specs
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

    // Collect extensions for languages where directories are packages
    // (e.g., Go: any .go file in a dir is part of that package).
    // These files get parent-directory suffixes so package imports resolve.
    let dir_pkg_exts: Vec<String> = crate::analysis::lang_registry::all_profiles()
        .filter(|p| p.semantics.project.directory_is_package)
        .flat_map(|p| {
            crate::analysis::lang_registry::get(&p.name)
                .map(|c| c.extensions.clone())
                .unwrap_or_default()
        })
        .collect();

    for &file_path in known_files {
        let module_path = file_to_module_path(file_path);
        if module_path.is_empty() {
            continue;
        }

        add_module_suffixes(&mut index, module_path, file_path);

        // For directory-is-package languages: also add parent dir as module path.
        // e.g., Go `import "internal/config"` references the directory, not a file.
        if !dir_pkg_exts.is_empty() {
            let has_dir_pkg_ext = file_path.rsplit('.').next()
                .map_or(false, |ext| dir_pkg_exts.iter().any(|e| e == ext));
            if has_dir_pkg_ext {
                if let Some((parent, _)) = module_path.rsplit_once('/') {
                    if !parent.is_empty() {
                        add_module_suffixes(&mut index, parent, file_path);
                    }
                }
            }
        }
    }

    // Module prefix files: parse files like go.mod to map module paths to project dirs.
    // Reads module_prefix_file and module_prefix_directive from plugin TOML.
    let module_prefixes = collect_module_prefixes(project_map, scan_root);

    // Manifest name → entry point (separate map for safe single-segment lookup)
    let mut manifest_name_aliases: HashMap<String, Vec<&'a str>> = HashMap::new();
    inject_manifest_name_aliases(&mut manifest_name_aliases, known_files, scan_root);

    SuffixIndex { index, manifest_name_aliases, module_prefixes }
}

/// Extract a module path from a file content using a directive keyword.
/// Generic version: reads `<directive> <path>` from the first matching line.
/// Works for Go (`module github.com/user/repo` in go.mod) and any similar format.
fn extract_module_name_generic<'a>(content: &'a str, directive: &str) -> Option<&'a str> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(directive) {
            let rest = rest.trim();
            if rest.is_empty() { continue; }
            return Some(rest.split_whitespace().next().unwrap_or(rest));
        }
    }
    None
}

/// Extract prefix->directory mappings from a JSON manifest file.
/// Navigates to each json_path and reads the object's key-value pairs.
/// Keys are namespace prefixes (with separator), values are directory paths.
/// Used for PSR-4 (composer.json), TypeScript paths (tsconfig.json), etc.
fn extract_json_prefix_map(
    content: &str,
    json_paths: &[String],
    namespace_sep: &str,
) -> Vec<(String, String)> {
    let mut prefixes = Vec::new();
    let parsed: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return prefixes,
    };
    for json_path in json_paths {
        // Navigate to the path (e.g., "autoload.psr-4")
        // Use a custom split that handles hyphenated keys:
        // "autoload.psr-4" splits on the first '.' -> ["autoload", "psr-4"]
        let mut current = &parsed;
        let keys = split_json_path(json_path);
        for key in &keys {
            match current.get(key.as_str()) {
                Some(v) => current = v,
                None => { current = &serde_json::Value::Null; break; }
            }
        }
        // Read the object as prefix->directory map
        if let Some(obj) = current.as_object() {
            for (ns_prefix, dir_value) in obj {
                // Normalize the namespace prefix: convert separator to /
                let mut normalized = ns_prefix.clone();
                if !namespace_sep.is_empty() {
                    normalized = normalized.replace(namespace_sep, "/");
                }
                // Remove trailing slash
                let normalized = normalized.trim_end_matches('/').to_string();

                // Directory can be a string or array of strings
                let dirs: Vec<String> = match dir_value {
                    serde_json::Value::String(s) => vec![s.trim_end_matches('/').to_string()],
                    serde_json::Value::Array(arr) => arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.trim_end_matches('/').to_string())
                        .collect(),
                    _ => continue,
                };
                for dir in dirs {
                    if !normalized.is_empty() {
                        prefixes.push((normalized.clone(), dir));
                    }
                }
            }
        }
    }
    prefixes
}

/// Split a JSON path like "autoload.psr-4" into segments.
/// Splits on '.' but only at the top level — each segment can contain hyphens.
fn split_json_path(path: &str) -> Vec<String> {
    path.split('.').map(|s| s.to_string()).collect()
}

/// Plugin resolver config snapshot for prefix collection.
/// Holds references to the fields needed from each plugin's ResolverConfig.
struct PrefixPluginConfig<'a> {
    prefix_file: &'a str,
    directive: &'a str,
    format: &'a str,
    json_paths: &'a [String],
    namespace_sep: &'a str,
}

/// Scan project roots for module prefix files and build a map of module paths to project dirs.
/// Reads module_prefix_file from ALL loaded plugin profiles.
/// Supports two formats:
///   - "line" (default): reads `<directive> <path>` from a text file (e.g., Go go.mod).
///   - "json_map": reads prefix->directory mappings from a JSON file (e.g., PHP composer.json).
/// Sorted longest-first so more specific module paths match before shorter ones.
fn collect_module_prefixes(project_map: &HashMap<String, String>, scan_root: &Path) -> Vec<(String, String)> {
    // Collect all plugin configs that have a module_prefix_file
    let prefix_configs: Vec<PrefixPluginConfig<'_>> = crate::analysis::lang_registry::all_profiles()
        .filter(|p| !p.semantics.resolver.module_prefix_file.is_empty())
        .filter(|p| {
            // Must have either a directive (line format) or json_paths (json_map format)
            !p.semantics.resolver.module_prefix_directive.is_empty()
                || (p.semantics.resolver.module_prefix_format == "json_map"
                    && !p.semantics.resolver.module_prefix_json_paths.is_empty())
        })
        .map(|p| PrefixPluginConfig {
            prefix_file: p.semantics.resolver.module_prefix_file.as_str(),
            directive: p.semantics.resolver.module_prefix_directive.as_str(),
            format: p.semantics.resolver.module_prefix_format.as_str(),
            json_paths: &p.semantics.resolver.module_prefix_json_paths,
            namespace_sep: p.semantics.resolver.namespace_separator.as_str(),
        })
        .collect();

    if prefix_configs.is_empty() {
        return Vec::new();
    }

    let unique_roots: HashSet<&str> = project_map.values().map(|s| s.as_str()).collect();
    let mut prefixes = Vec::new();

    for &project_dir in &unique_roots {
        for cfg in &prefix_configs {
            let path = if project_dir.is_empty() {
                scan_root.join(cfg.prefix_file)
            } else {
                scan_root.join(project_dir).join(cfg.prefix_file)
            };

            if let Ok(content) = std::fs::read_to_string(&path) {
                if cfg.format == "json_map" && !cfg.json_paths.is_empty() {
                    // JSON map format: read prefix->directory mappings
                    let maps = extract_json_prefix_map(
                        &content,
                        cfg.json_paths,
                        cfg.namespace_sep,
                    );
                    for (prefix, dir) in maps {
                        // Build the full project-relative path for the directory.
                        // The dir from JSON is relative to the manifest file location.
                        let full_dir = if project_dir.is_empty() {
                            dir
                        } else {
                            format!("{}/{}", project_dir, dir)
                        };
                        prefixes.push((prefix, full_dir));
                    }
                } else if !cfg.directive.is_empty() {
                    // Line format (existing): read "directive value" from text file
                    if let Some(module_name) = extract_module_name_generic(&content, cfg.directive) {
                        prefixes.push((module_name.to_string(), project_dir.to_string()));
                    }
                }
            }
        }
    }
    // Sort longest module path first for greedy matching
    prefixes.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    prefixes
}

/// Add exact package name → entry file to the manifest_name_aliases map.
/// For exact imports: `use sentrux_core` → `src/lib.rs`, `import '@company/shared'` → `src/index.ts`.
/// Uses alias_entry_point from plugin profile to find entry files, then reads manifest for name.
/// Try to read a manifest and extract the transformed package name for an entry-point file.
fn extract_manifest_alias(
    file_path: &str,
    resolver: &crate::analysis::plugin::profile::ResolverConfig,
    scan_root: &Path,
) -> Option<String> {
    let project_dir = file_path
        .strip_suffix(&resolver.alias_entry_point)
        .unwrap_or("")
        .trim_end_matches('/');
    let manifest_path = if project_dir.is_empty() {
        scan_root.join(&resolver.alias_file)
    } else {
        scan_root.join(project_dir).join(&resolver.alias_file)
    };
    let content = std::fs::read_to_string(&manifest_path).ok()?;
    let manifest_name = manifest_path.file_name()
        .and_then(|n| n.to_str()).unwrap_or(&resolver.alias_file);
    let name = extract_name_from_manifest(&content, &resolver.alias_field, manifest_name)?;
    let transformed = apply_alias_transform(&name, &resolver.alias_transform);
    if transformed.is_empty() { None } else { Some(transformed) }
}

fn inject_manifest_name_aliases<'a>(
    index: &mut HashMap<String, Vec<&'a str>>,
    known_files: &HashSet<&'a str>,
    scan_root: &Path,
) {
    for profile in crate::analysis::lang_registry::all_profiles() {
        let resolver = &profile.semantics.resolver;
        if resolver.alias_file.is_empty() || resolver.alias_field.is_empty()
            || resolver.alias_entry_point.is_empty()
        {
            continue;
        }

        let entry_filename = resolver.alias_entry_point.rsplit('/').next()
            .unwrap_or(&resolver.alias_entry_point);

        for &file_path in known_files {
            let filename = file_path.rsplit('/').next().unwrap_or(file_path);
            if filename != entry_filename || !file_path.ends_with(&resolver.alias_entry_point) {
                continue;
            }
            if let Some(transformed) = extract_manifest_alias(file_path, resolver, scan_root) {
                index.entry(transformed).or_default().push(file_path);
            }
        }
    }
}

/// Scan all manifest files and build package name → directory path aliases.
///
/// First-principle approach: the manifest's DIRECTORY is the package.
/// When @company/shared is imported, it means "files in the directory
/// containing a package.json with name: @company/shared".
///
/// No entry point guessing. No src/ assumptions. The directory IS the truth.
/// Normal resolution (package_index_files, extension probing) handles the rest.
///
/// Data-driven from plugin.toml [semantics.resolver] alias_file + alias_field.
/// Try to resolve a single project directory into a PathAlias from its manifest.
fn resolve_project_alias(
    project_dir: &str,
    scan_root: &Path,
    resolver: &crate::analysis::plugin::profile::ResolverConfig,
) -> Option<PathAlias> {
    let manifest_dir = if project_dir.is_empty() {
        scan_root.to_path_buf()
    } else {
        scan_root.join(project_dir)
    };
    let manifest_path = resolve_manifest_path(&manifest_dir, &resolver.alias_file)?;
    let content = std::fs::read_to_string(&manifest_path).ok()?;
    let name = extract_name_from_manifest(&content, &resolver.alias_field, &resolver.alias_file)
        .filter(|n| !n.is_empty())?;
    let transformed = apply_alias_transform(&name, &resolver.alias_transform);
    let dir_replacement = build_dir_replacement(project_dir, &resolver.source_root);
    Some(PathAlias {
        prefix: format!("{}/", transformed),
        replacements: vec![dir_replacement],
    })
}

/// Apply alias_transform (e.g., hyphen_to_underscore) to a manifest name.
fn apply_alias_transform(name: &str, transform: &str) -> String {
    match transform {
        "hyphen_to_underscore" => name.replace('-', "_"),
        _ => name.to_string(),
    }
}

/// Build the directory replacement path from project_dir and source_root.
fn build_dir_replacement(project_dir: &str, source_root: &str) -> String {
    let base = if project_dir.is_empty() {
        String::new()
    } else {
        format!("{}/", project_dir)
    };
    if source_root.is_empty() { base } else { format!("{}{}/", base, source_root) }
}

fn collect_manifest_path_aliases(
    project_map: &HashMap<String, String>,
    scan_root: &Path,
) -> Vec<PathAlias> {
    let mut aliases = Vec::new();
    let mut seen_dirs: HashSet<String> = HashSet::new();
    let unique_roots: HashSet<&str> = project_map.values().map(|s| s.as_str()).collect();

    for profile in crate::analysis::lang_registry::all_profiles() {
        let resolver = &profile.semantics.resolver;
        if resolver.alias_file.is_empty() || resolver.alias_field.is_empty() {
            continue;
        }
        for &project_dir in &unique_roots {
            if seen_dirs.contains(project_dir) {
                continue;
            }
            if let Some(alias) = resolve_project_alias(project_dir, scan_root, resolver) {
                aliases.push(alias);
                seen_dirs.insert(project_dir.to_string());
            }
        }
    }

    aliases.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));
    aliases
}

/// Resolve a manifest filename to an actual path.
/// Supports exact names ("Cargo.toml") and glob patterns ("*.csproj").
fn resolve_manifest_path(dir: &Path, filename: &str) -> Option<std::path::PathBuf> {
    if filename.starts_with('*') {
        // Glob: find first file matching the extension
        let ext = filename.trim_start_matches('*');
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.ends_with(ext) && entry.path().is_file() {
                    return Some(entry.path());
                }
            }
        }
        None
    } else {
        let path = dir.join(filename);
        if path.exists() { Some(path) } else { None }
    }
}

/// Extract a name field from a manifest file.
/// Supports 5 strategies, auto-detected from filename extension:
///   JSON (.json) — serde_json
///   TOML (.toml) — toml crate
///   XML  (.xml, .csproj, .fsproj, .vbproj) — quick-xml
///   YAML (.yaml, .yml) — serde_yaml
///   Line match (everything else) — scan for `field` as a line prefix, extract quoted string
fn extract_name_from_manifest(content: &str, field: &str, filename: &str) -> Option<String> {
    if filename.ends_with(".json") {
        extract_json_field(content, field)
    } else if filename.ends_with(".toml") {
        extract_toml_field(content, field)
    } else if filename.ends_with(".xml") || filename.ends_with("proj") {
        extract_xml_field(content, field)
    } else if filename.ends_with(".yaml") || filename.ends_with(".yml") {
        extract_yaml_field(content, field)
    } else {
        // Line match: scan for field as prefix, extract quoted string
        // Works for: build.sbt (name := "x"), *.gemspec (spec.name = "x"),
        // mix.exs (app: :x), Package.swift (name: "x")
        extract_line_match(content, field)
    }
}

/// Extract a value by scanning lines for a prefix and extracting the string/symbol after it.
/// Handles: `prefix "value"`, `prefix 'value'`, `prefix :value` (Elixir atoms).
fn extract_line_match(content: &str, prefix: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let rest = rest.trim().trim_start_matches(|c: char| c == '=' || c == ':').trim();
            // Extract quoted string: "value" or 'value'
            if rest.starts_with('"') {
                return rest[1..].find('"').map(|i| rest[1..1 + i].to_string());
            }
            if rest.starts_with('\'') {
                return rest[1..].find('\'').map(|i| rest[1..1 + i].to_string());
            }
            // Elixir atom: :my_app
            if rest.starts_with(':') {
                let atom = rest[1..].split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next().unwrap_or("");
                if !atom.is_empty() {
                    return Some(atom.to_string());
                }
            }
            // Bare word (no quotes)
            let word = rest.split(|c: char| c.is_whitespace() || c == ',')
                .next().unwrap_or("").trim_matches('"');
            if !word.is_empty() {
                return Some(word.to_string());
            }
        }
    }
    None
}

/// Extract a dot-separated field from JSON (e.g., "name" or "compilerOptions.paths").
fn extract_json_field(content: &str, field: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(content).ok()?;
    navigate_json(&json, field)?.as_str().map(|s| s.to_string())
}

/// Extract a dot-separated field from TOML (e.g., "package.name").
fn extract_toml_field(content: &str, field: &str) -> Option<String> {
    let val: toml::Value = content.parse().ok()?;
    let mut current = &val;
    for key in field.split('.') {
        current = current.get(key)?;
    }
    current.as_str().map(|s| s.to_string())
}

/// Extract a dot-separated field from XML (e.g., "project.artifactId").
/// Navigates nested elements: "project.artifactId" finds <project><artifactId>value</artifactId></project>.
fn extract_xml_field(content: &str, field: &str) -> Option<String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let path_parts: Vec<&str> = field.split('.').collect();
    let mut reader = Reader::from_str(content);
    let mut depth_matched = 0usize;
    let mut capture_text = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if depth_matched < path_parts.len() && tag == path_parts[depth_matched] {
                    depth_matched += 1;
                    if depth_matched == path_parts.len() {
                        capture_text = true;
                    }
                }
            }
            Ok(Event::Text(e)) if capture_text => {
                let text = e.unescape().ok()?.trim().to_string();
                if !text.is_empty() {
                    return Some(text);
                }
            }
            Ok(Event::End(_)) if capture_text => {
                return None; // Empty element
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    None
}

/// Extract a dot-separated field from YAML (e.g., "name" or "project.name").
fn extract_yaml_field(content: &str, field: &str) -> Option<String> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(content).ok()?;
    let mut current = &yaml;
    for key in field.split('.') {
        current = current.get(key)?;
    }
    current.as_str().map(|s| s.to_string())
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
    // Finally: manifest name aliases (crate names, package names)
    // These are high-confidence (from actual manifest files), safe for single-segment lookup.
    if let Some(candidates) = env.suffix_index.manifest_name_aliases.get(specifier) {
        if candidates.len() == 1 {
            return Some(candidates[0].to_string());
        }
    }
    None
}

// ── Path alias resolution (data-driven from plugin.toml) ──────────────

/// A single path alias mapping: prefix → replacement paths.
pub(crate) struct PathAlias {
    prefix: String,
    replacements: Vec<String>,
}

/// Apply path alias substitution to a specifier.
fn apply_path_alias(specifier: &str, aliases: &[PathAlias]) -> Option<String> {
    for alias in aliases {
        // Prefix match: @company/shared/utils → packages/shared/utils
        if specifier.starts_with(&alias.prefix) {
            let remainder = &specifier[alias.prefix.len()..];
            if let Some(replacement) = alias.replacements.first() {
                return Some(format!("{}{}", replacement, remainder));
            }
        }
        // Exact match: @company/shared → packages/shared (directory)
        // The caller's normal resolution will find index files via package_index_files
        let exact = alias.prefix.trim_end_matches('/');
        if specifier == exact {
            if let Some(replacement) = alias.replacements.first() {
                let dir = replacement.trim_end_matches('/');
                if dir.is_empty() {
                    // Root project: exact import of own package name
                    // Can't return empty — return None, let suffix-index handle it
                    continue;
                }
                return Some(dir.to_string());
            }
        }
    }
    None
}

/// Load path aliases + workspace package aliases from plugin-declared config files.
/// Try loading path aliases for a single project directory from a resolver config.
fn try_load_project_aliases(
    project_dir: &str,
    scan_root: &Path,
    resolver: &crate::analysis::plugin::profile::ResolverConfig,
) -> Option<Vec<PathAlias>> {
    let config_path = if project_dir.is_empty() {
        scan_root.join(&resolver.path_alias_file)
    } else {
        scan_root.join(project_dir).join(&resolver.path_alias_file)
    };
    if !config_path.exists() { return None; }
    parse_path_alias_config(&config_path, &resolver.path_alias_field, &resolver.path_alias_base_url)
}

fn load_path_aliases(
    project_map: &HashMap<String, String>,
    scan_root: &Path,
) -> HashMap<String, Vec<PathAlias>> {
    let mut result: HashMap<String, Vec<PathAlias>> = HashMap::new();
    let unique_roots: HashSet<&str> = project_map.values().map(|s| s.as_str()).collect();

    for profile in crate::analysis::lang_registry::all_profiles() {
        let resolver = &profile.semantics.resolver;
        if resolver.path_alias_file.is_empty() || resolver.path_alias_field.is_empty() {
            continue;
        }
        for &project_dir in &unique_roots {
            if result.contains_key(project_dir) { continue; }
            if let Some(aliases) = try_load_project_aliases(project_dir, scan_root, resolver) {
                result.entry(project_dir.to_string()).or_default().extend(aliases);
            }
        }
    }
    result
}

/// Parse a JSON config file and extract path alias mappings.
fn parse_path_alias_config(
    config_path: &Path,
    field_path: &str,
    base_url_path: &str,
) -> Option<Vec<PathAlias>> {
    let content = std::fs::read_to_string(config_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let base_url = if !base_url_path.is_empty() {
        navigate_json(&json, base_url_path)
            .and_then(|v| v.as_str())
            .unwrap_or(".")
    } else { "." };

    let paths_obj = navigate_json(&json, field_path)?.as_object()?;
    let mut aliases = Vec::new();

    for (pattern, mapped) in paths_obj {
        let prefix = pattern.trim_end_matches('*');
        if prefix.is_empty() { continue; }
        let replacements: Vec<String> = match mapped {
            serde_json::Value::Array(arr) => arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| {
                    let stripped = s.trim_end_matches('*');
                    if base_url == "." { stripped.to_string() }
                    else { format!("{}/{}", base_url.trim_end_matches('/'), stripped.trim_start_matches("./")) }
                })
                .collect(),
            _ => continue,
        };
        if !replacements.is_empty() {
            aliases.push(PathAlias { prefix: prefix.to_string(), replacements });
        }
    }

    aliases.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));
    if aliases.is_empty() { None } else { Some(aliases) }
}

/// Navigate a JSON value by dot-separated path.
fn navigate_json<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for key in path.split('.') { current = current.get(key)?; }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_prefix_map_psr4_basic() {
        // Typical composer.json PSR-4 autoload
        let content = r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#;
        let paths = vec!["autoload.psr-4".to_string()];
        let result = extract_json_prefix_map(content, &paths, "\\");
        assert_eq!(result, vec![("App".to_string(), "src".to_string())]);
    }

    #[test]
    fn json_prefix_map_multiple_namespaces() {
        let content = r#"{
            "autoload": {
                "psr-4": {
                    "App\\": "src/",
                    "Database\\Factories\\": "database/factories/",
                    "Database\\Seeders\\": "database/seeders/"
                }
            }
        }"#;
        let paths = vec!["autoload.psr-4".to_string()];
        let result = extract_json_prefix_map(content, &paths, "\\");
        assert_eq!(result.len(), 3);
        assert!(result.contains(&("App".to_string(), "src".to_string())));
        assert!(result.contains(&("Database/Factories".to_string(), "database/factories".to_string())));
        assert!(result.contains(&("Database/Seeders".to_string(), "database/seeders".to_string())));
    }

    #[test]
    fn json_prefix_map_with_dev_autoload() {
        let content = r#"{
            "autoload": {"psr-4": {"App\\": "src/"}},
            "autoload-dev": {"psr-4": {"Tests\\": "tests/"}}
        }"#;
        let paths = vec!["autoload.psr-4".to_string(), "autoload-dev.psr-4".to_string()];
        let result = extract_json_prefix_map(content, &paths, "\\");
        assert_eq!(result.len(), 2);
        assert!(result.contains(&("App".to_string(), "src".to_string())));
        assert!(result.contains(&("Tests".to_string(), "tests".to_string())));
    }

    #[test]
    fn json_prefix_map_array_dirs() {
        // PSR-4 allows array of directories for a single namespace
        let content = r#"{"autoload":{"psr-4":{"App\\":["src/","lib/"]}}}"#;
        let paths = vec!["autoload.psr-4".to_string()];
        let result = extract_json_prefix_map(content, &paths, "\\");
        assert_eq!(result.len(), 2);
        assert!(result.contains(&("App".to_string(), "src".to_string())));
        assert!(result.contains(&("App".to_string(), "lib".to_string())));
    }

    #[test]
    fn json_prefix_map_invalid_json() {
        let content = "not json at all";
        let paths = vec!["autoload.psr-4".to_string()];
        let result = extract_json_prefix_map(content, &paths, "\\");
        assert!(result.is_empty());
    }

    #[test]
    fn json_prefix_map_missing_path() {
        let content = r#"{"autoload":{}}"#;
        let paths = vec!["autoload.psr-4".to_string()];
        let result = extract_json_prefix_map(content, &paths, "\\");
        assert!(result.is_empty());
    }

    #[test]
    fn json_prefix_map_empty_namespace_sep() {
        // No separator conversion — namespace prefix kept as-is
        let content = r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#;
        let paths = vec!["autoload.psr-4".to_string()];
        let result = extract_json_prefix_map(content, &paths, "");
        // With empty sep, the backslash stays in the prefix
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "App\\");
        assert_eq!(result[0].1, "src");
    }

    #[test]
    fn split_json_path_basic() {
        assert_eq!(split_json_path("autoload.psr-4"), vec!["autoload", "psr-4"]);
        assert_eq!(split_json_path("compilerOptions.paths"), vec!["compilerOptions", "paths"]);
        assert_eq!(split_json_path("name"), vec!["name"]);
    }

    // ── Helper: build a minimal SuffixIndex + ResolveEnv for resolver tests ──

    use crate::analysis::resolver::helpers::{
        SuffixIndex, ResolveEnv, try_suffix_resolve,
    };

    fn make_suffix_index<'a>(
        files: &[&'a str],
        module_prefixes: Vec<(String, String)>,
        dir_pkg_exts: &[&str],
    ) -> SuffixIndex<'a> {
        let mut index = HashMap::new();
        for &file_path in files {
            let module_path = crate::analysis::resolver::helpers::file_to_module_path(file_path);
            if module_path.is_empty() { continue; }
            add_module_suffixes(&mut index, module_path, file_path);
            if !dir_pkg_exts.is_empty() {
                let has_ext = file_path.rsplit('.').next()
                    .map_or(false, |ext| dir_pkg_exts.iter().any(|e| *e == ext));
                if has_ext {
                    if let Some((parent, _)) = module_path.rsplit_once('/') {
                        if !parent.is_empty() {
                            add_module_suffixes(&mut index, parent, file_path);
                        }
                    }
                }
            }
        }
        SuffixIndex { index, manifest_name_aliases: HashMap::new(), module_prefixes }
    }

    // ── Resolver: stdlib imports should NOT match project files when module prefixes exist ──

    #[test]
    fn stdlib_import_not_resolved_with_module_prefix() {
        let files: Vec<&str> = vec![
            "library/json/json.go",
            "library/json/decode.go",
            "library/log/log.go",
            "app/handler/handler.go",
        ];
        let known: HashSet<&str> = files.iter().copied().collect();
        let module_prefixes = vec![
            ("gitlab.example.com/org/repo".to_string(), String::new()),
        ];
        let suffix_index = make_suffix_index(&files, module_prefixes, &["go"]);
        let env = ResolveEnv {
            suffix_index: &suffix_index,
            known_files: &known,
            exts: &["go"],
            directory_is_package: true,
        };
        assert_eq!(try_suffix_resolve("encoding/json", &env, "app/handler", Path::new("app/handler")), None);
        assert_eq!(try_suffix_resolve("net/http", &env, "app/handler", Path::new("app/handler")), None);
        assert_eq!(try_suffix_resolve("path/filepath", &env, "app/handler", Path::new("app/handler")), None);
    }

    #[test]
    fn project_import_resolves_with_module_prefix() {
        let files: Vec<&str> = vec![
            "library/json/json.go",
            "library/log/log.go",
            "app/handler/handler.go",
        ];
        let known: HashSet<&str> = files.iter().copied().collect();
        let module_prefixes = vec![
            ("gitlab.example.com/org/repo".to_string(), String::new()),
        ];
        let suffix_index = make_suffix_index(&files, module_prefixes, &["go"]);
        let env = ResolveEnv {
            suffix_index: &suffix_index,
            known_files: &known,
            exts: &["go"],
            directory_is_package: true,
        };
        let result = try_suffix_resolve(
            "gitlab.example.com/org/repo/library/json", &env,
            "app/handler", Path::new("app/handler"),
        );
        assert!(result.is_some(), "project-internal import should resolve");
        assert!(result.unwrap().starts_with("library/json/"),
            "should resolve to a file in library/json/");
    }

    #[test]
    fn no_module_prefix_still_uses_suffix_stripping() {
        let files: Vec<&str> = vec!["src/utils/json.py"];
        let known: HashSet<&str> = files.iter().copied().collect();
        let suffix_index = make_suffix_index(&files, vec![], &[]);
        let env = ResolveEnv {
            suffix_index: &suffix_index,
            known_files: &known,
            exts: &["py"],
            directory_is_package: false,
        };
        let result = try_suffix_resolve("utils/json", &env, "src/app", Path::new("src/app"));
        assert!(result.is_some(), "without module prefixes, suffix stripping should work");
    }
}
