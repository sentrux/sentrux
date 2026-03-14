//! JS/TS import resolution via oxc_resolver — accurate Node.js module resolution.
//!
//! Uses oxc_resolver (Rust port of enhanced-resolve) for Tier 1 resolution of
//! JavaScript and TypeScript imports. Handles tsconfig paths, package.json exports,
//! and Node.js resolution algorithm. Falls back gracefully to suffix resolver.

use crate::core::types::ImportEdge;
use crate::core::types::FileNode;
use oxc_resolver::{ResolveOptions, Resolver, TsconfigDiscovery};
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Build a set of file stems + parent dir names from known_files.
/// Used to skip bare specifiers that can't possibly match any project file.
/// E.g., "react" has no matching file stem → definitely npm → skip resolver call.
/// But "utils" might match "src/utils.ts" (stem) or "utils/index.ts" (parent dir).
fn build_known_identifiers<'a>(known_files: &'a HashSet<&str>) -> HashSet<&'a str> {
    let mut ids: HashSet<&str> = HashSet::with_capacity(known_files.len() * 2);
    for f in known_files.iter() {
        let p = Path::new(f);
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            ids.insert(stem);
        }
        if let Some(parent_name) = p.parent().and_then(|pp| pp.file_name()).and_then(|n| n.to_str()) {
            ids.insert(parent_name);
        }
    }
    ids
}

/// Collect JS/TS files that have imports, sorted by path for directory locality.
fn collect_js_ts_with_imports<'a>(files: &'a [&FileNode]) -> Vec<&'a &'a FileNode> {
    let mut js_ts_files: Vec<&&FileNode> = files
        .iter()
        .filter(|f| {
            !f.is_dir
                && matches!(f.lang.as_str(), "javascript" | "typescript" | "jsx" | "tsx")
                && f.sa
                    .as_ref()
                    .and_then(|sa| sa.imp.as_ref())
                    .is_some_and(|imp| !imp.is_empty())
        })
        .collect();
    // Sort by path → rayon chunks contain files from the same directory,
    // improving Resolver internal cache hits (package.json, tsconfig).
    js_ts_files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    js_ts_files
}

/// Build ResolveOptions with standard JS/TS extensions and conditions.
fn build_resolve_options(scan_root: &Path) -> (ResolveOptions, bool) {
    let has_tsconfig = scan_root.join("tsconfig.json").exists();
    let tsconfig = if has_tsconfig {
        Some(TsconfigDiscovery::Auto)
    } else {
        None
    };
    let opts = ResolveOptions {
        extensions: vec![
            ".ts".into(), ".tsx".into(), ".js".into(), ".jsx".into(),
            ".mjs".into(), ".mts".into(), ".json".into(),
        ],
        condition_names: vec![
            "import".into(), "require".into(), "node".into(), "default".into(),
        ],
        main_fields: vec!["module".into(), "main".into()],
        main_files: vec!["index".into()],
        tsconfig,
        ..ResolveOptions::default()
    };
    (opts, has_tsconfig)
}

/// FNV-1a hash of scan_root + tsconfig presence for thread-local cache key.
fn compute_root_key(scan_root: &Path, has_tsconfig: bool) -> u64 {
    let bytes = scan_root.as_os_str().as_encoded_bytes();
    let mut h: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h ^= has_tsconfig as u64;
    h = h.wrapping_mul(0x100000001b3);
    h
}

/// Returns true if this bare specifier should be skipped (definitely npm, not a project file).
fn should_skip_bare_specifier(specifier: &str, known_identifiers: &HashSet<&str>) -> bool {
    if specifier.starts_with('.')
        || specifier.starts_with('/')
        || specifier.starts_with('#')
    {
        return false;
    }
    let is_scoped = specifier.starts_with('@');
    let slash_count = specifier.matches('/').count();
    if (!is_scoped && slash_count == 0) || (is_scoped && slash_count <= 1) {
        let check_name = specifier.rsplit('/').next().unwrap_or(specifier);
        return !known_identifiers.contains(check_name);
    }
    false
}

/// Tier 1: Resolve JS/TS/JSX/TSX imports using oxc_resolver (webpack-compatible).
/// Returns edges only for targets present in `known_files` (project files).
/// node_modules targets are excluded by membership check — no explicit filtering needed.
/// Accepts `&[&FileNode]` (zero-copy from flatten_files_ref) to avoid cloning the tree.
///
/// Performance optimizations:
///   - Parallel via rayon par_iter
///   - thread_local Resolver per rayon thread (invalidated on scan_root change)
///   - Files sorted by path for directory locality (Resolver cache hits)
///   - Bare npm specifiers pre-filtered via known_identifiers set
pub fn resolve_js_ts_imports(scan_root: &Path, files: &[&FileNode], known_files: &HashSet<&str>) -> Vec<ImportEdge> {
    let t0 = std::time::Instant::now();

    // Canonicalize scan_root to handle symlinks (e.g. /tmp -> /private/tmp on macOS).
    let scan_root = scan_root.canonicalize().unwrap_or_else(|_| scan_root.to_path_buf());
    let scan_root = scan_root.as_path();

    let known_identifiers = build_known_identifiers(known_files);
    let js_ts_files = collect_js_ts_with_imports(files);
    if js_ts_files.is_empty() {
        return Vec::new();
    }

    let (resolve_opts, has_tsconfig) = build_resolve_options(scan_root);
    let root_key = compute_root_key(scan_root, has_tsconfig);
    let t_setup = t0.elapsed();

    let setup = OxcSetup {
        scan_root,
        known_files,
        known_identifiers: &known_identifiers,
        resolve_opts: &resolve_opts,
        root_key,
    };
    let (edges, resolved_count, skipped) = resolve_all_parallel(&js_ts_files, &setup);

    log_oxc_timing(&OxcTimingStats {
        t0, t_setup, file_count: js_ts_files.len(), resolved_count, skipped, edge_count: edges.len(),
    });
    edges
}

/// Setup parameters for parallel oxc resolution — groups the immutable
/// scan-wide configuration that every file in the batch needs.
struct OxcSetup<'a> {
    scan_root: &'a Path,
    known_files: &'a HashSet<&'a str>,
    known_identifiers: &'a HashSet<&'a str>,
    resolve_opts: &'a ResolveOptions,
    root_key: u64,
}

/// Resolve imports in parallel across all JS/TS files using thread-local Resolvers.
fn resolve_all_parallel(
    js_ts_files: &[&&FileNode],
    setup: &OxcSetup<'_>,
) -> (Vec<ImportEdge>, usize, usize) {
    std::thread_local! {
        static TL_RESOLVER: std::cell::RefCell<(u64, Option<Resolver>)> = const {
            std::cell::RefCell::new((0, None))
        };
    }
    let skipped = AtomicUsize::new(0);
    let resolved_count = AtomicUsize::new(0);
    let ctx = ResolveContext {
        scan_root: setup.scan_root,
        known_files: setup.known_files,
        known_identifiers: setup.known_identifiers,
        skipped: &skipped,
        resolved_count: &resolved_count,
    };

    let edges: Vec<ImportEdge> = js_ts_files
        .par_iter()
        .flat_map_iter(|file| {
            let abs_dir = ctx.scan_root.join(&file.path);
            let dir = match abs_dir.parent() {
                Some(d) => d.to_path_buf(),
                None => return Vec::new(),
            };
            let imports = match file.sa.as_ref().and_then(|sa| sa.imp.as_ref()) {
                Some(imp) => imp,
                None => return Vec::new(),
            };
            TL_RESOLVER.with(|cell| {
                let mut borrow = cell.borrow_mut();
                if borrow.0 != setup.root_key || borrow.1.is_none() {
                    borrow.0 = setup.root_key;
                    borrow.1 = Some(Resolver::new(setup.resolve_opts.clone()));
                }
                let resolver = borrow.1.as_ref().unwrap();
                resolve_file_imports(resolver, &dir, imports, file, &ctx)
            })
        })
        .collect();

    (edges, resolved_count.load(Ordering::Relaxed), skipped.load(Ordering::Relaxed))
}

/// Shared context for resolving imports within a single parallel batch.
/// Groups the immutable references that are the same for every file in the batch,
/// plus the shared atomic counters for statistics.
struct ResolveContext<'a> {
    scan_root: &'a Path,
    known_files: &'a HashSet<&'a str>,
    known_identifiers: &'a HashSet<&'a str>,
    skipped: &'a AtomicUsize,
    resolved_count: &'a AtomicUsize,
}

/// Try to resolve a single specifier to a known project file, returning an edge if found.
fn resolve_single_specifier(
    resolver: &Resolver,
    dir: &Path,
    specifier: &str,
    from_path: &str,
    ctx: &ResolveContext<'_>,
) -> Option<ImportEdge> {
    ctx.resolved_count.fetch_add(1, Ordering::Relaxed);
    let resolution = resolver.resolve(dir, specifier).ok()?;
    // Canonicalize resolved path to match canonicalized scan_root.
    // On macOS, /tmp → /private/tmp symlink causes strip_prefix to fail
    // if only one side is canonicalized.
    let full_path = resolution.full_path().canonicalize()
        .unwrap_or_else(|_| resolution.full_path().to_path_buf());
    let rel = full_path.strip_prefix(ctx.scan_root).ok()?;
    let rel_str = rel.to_string_lossy();
    if ctx.known_files.contains(rel_str.as_ref()) && rel_str.as_ref() != from_path {
        Some(ImportEdge { from_file: from_path.to_string(), to_file: rel_str.to_string() })
    } else {
        None
    }
}

/// Resolve all import specifiers for a single file, returning edges to known project files.
fn resolve_file_imports(
    resolver: &Resolver,
    dir: &Path,
    imports: &[String],
    file: &FileNode,
    ctx: &ResolveContext<'_>,
) -> Vec<ImportEdge> {
    let mut file_edges = Vec::new();
    for specifier in imports {
        if should_skip_bare_specifier(specifier, ctx.known_identifiers) {
            ctx.skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        if let Some(edge) = resolve_single_specifier(resolver, dir, specifier, &file.path, ctx) {
            file_edges.push(edge);
        }
    }
    file_edges
}

/// Timing and statistics for oxc resolution — logged after resolution completes.
struct OxcTimingStats {
    t0: std::time::Instant,
    t_setup: std::time::Duration,
    file_count: usize,
    resolved_count: usize,
    skipped: usize,
    edge_count: usize,
}

/// Log oxc_resolver timing breakdown.
fn log_oxc_timing(stats: &OxcTimingStats) {
    let t_total = stats.t0.elapsed();
    eprintln!(
        "[oxc_resolver] {} files, {} resolved, {} skipped (npm), {} edges | setup {:.1}ms, resolve {:.1}ms, total {:.1}ms",
        stats.file_count,
        stats.resolved_count,
        stats.skipped,
        stats.edge_count,
        stats.t_setup.as_secs_f64() * 1000.0,
        (t_total - stats.t_setup).as_secs_f64() * 1000.0,
        t_total.as_secs_f64() * 1000.0,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::test_helpers::make_file;
    use crate::core::types::{FileNode, StructuralAnalysis};
    use std::fs;

    #[test]
    fn resolves_relative_ts_import() {
        // Create temp directory with real files for oxc_resolver
        let tmp = std::env::temp_dir().join("sentrux_test_oxc_resolve");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("src")).unwrap();
        fs::write(tmp.join("src/main.ts"), "import { foo } from './utils';").unwrap();
        fs::write(tmp.join("src/utils.ts"), "export const foo = 1;").unwrap();

        let files = vec![
            make_file(
                "main.ts",
                "src/main.ts",
                "typescript",
                Some(StructuralAnalysis {
                    functions: None,
                    cls: None,
                    imp: Some(vec!["./utils".to_string()]),
                    co: None,
                    tags: None,
                }),
            ),
            make_file("utils.ts", "src/utils.ts", "typescript", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let known: HashSet<&str> = refs.iter().filter(|f| !f.is_dir).map(|f| f.path.as_str()).collect();
        let edges = resolve_js_ts_imports(&tmp, &refs, &known);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from_file, "src/main.ts");
        assert_eq!(edges[0].to_file, "src/utils.ts");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn skips_node_modules() {
        let tmp = std::env::temp_dir().join("sentrux_test_oxc_node_modules");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("src")).unwrap();
        fs::create_dir_all(tmp.join("node_modules/react")).unwrap();
        fs::write(tmp.join("src/app.tsx"), "import React from 'react';").unwrap();
        fs::write(
            tmp.join("node_modules/react/index.js"),
            "module.exports = {};",
        )
        .unwrap();
        fs::write(
            tmp.join("node_modules/react/package.json"),
            r#"{"name":"react","main":"index.js"}"#,
        )
        .unwrap();

        let files = vec![make_file(
            "app.tsx",
            "src/app.tsx",
            "tsx",
            Some(StructuralAnalysis {
                functions: None,
                cls: None,
                imp: Some(vec!["react".to_string()]),
                co: None,
                tags: None,
            }),
        )];

        let refs: Vec<&FileNode> = files.iter().collect();
        let known: HashSet<&str> = refs.iter().filter(|f| !f.is_dir).map(|f| f.path.as_str()).collect();
        let edges = resolve_js_ts_imports(&tmp, &refs, &known);
        // react resolves to node_modules — not in known_files → no edge
        assert!(edges.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_files_returns_empty() {
        let tmp = std::env::temp_dir().join("sentrux_test_oxc_empty");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let known: HashSet<&str> = HashSet::new();
        let edges = resolve_js_ts_imports(&tmp, &[], &known);
        assert!(edges.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn no_self_edges() {
        let tmp = std::env::temp_dir().join("sentrux_test_oxc_self");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("src")).unwrap();
        fs::write(tmp.join("src/a.ts"), "import './a';").unwrap();

        let files = vec![make_file(
            "a.ts",
            "src/a.ts",
            "typescript",
            Some(StructuralAnalysis {
                functions: None,
                cls: None,
                imp: Some(vec!["./a".to_string()]),
                co: None,
                tags: None,
            }),
        )];

        let refs: Vec<&FileNode> = files.iter().collect();
        let known: HashSet<&str> = refs.iter().filter(|f| !f.is_dir).map(|f| f.path.as_str()).collect();
        let edges = resolve_js_ts_imports(&tmp, &refs, &known);
        assert!(edges.is_empty(), "Self-imports should not produce edges");

        let _ = fs::remove_dir_all(&tmp);
    }
}
