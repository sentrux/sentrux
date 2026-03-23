//! Full directory scanner — walks filesystem, counts lines, parses structure, builds graphs.
//!
//! Uses `ignore` crate for gitignore-aware walking, `tokei` for line counting,
//! and `rayon` for parallel file processing. Produces a complete `Snapshot`.
//! Reports progress via callback for UI progress bars.

pub mod common;
mod tree;
pub mod rescan;

use self::common::{
    MAX_FILES, ScanLimits, count_lines_from_bytes, detect_lang,
    should_ignore_dir, should_ignore_file,
};
use self::tree::build_tree;
use crate::core::types::AppError;
use crate::core::snapshot::{ScanProgress, Snapshot};
use crate::core::types::FileNode;
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

/// Collected file info from the filesystem walk phase.
/// Captures path and mtime to avoid redundant metadata calls.
struct CollectedFile {
    path: PathBuf,
    mtime: f64,
}

/// Extract mtime as f64 seconds since UNIX_EPOCH from file metadata.
pub(crate) fn extract_mtime(meta: &fs::Metadata, path: &Path) -> f64 {
    meta.modified()
        .map(|t| {
            t.duration_since(UNIX_EPOCH)
                .unwrap_or_else(|e| {
                    crate::debug_log!("[scanner] mtime before epoch for {:?}: {}", path, e);
                    std::time::Duration::ZERO
                })
                .as_secs_f64()
        })
        .unwrap_or(0.0) // Filesystem doesn't support mtime (some network mounts)
}

/// Process a single walker entry: check filters, extract metadata, send to channel.
/// Returns Quit if the file limit was reached or the channel is disconnected.
fn process_walk_entry(
    entry: &ignore::DirEntry,
    file_size_limit: u64,
    count: &std::sync::atomic::AtomicUsize,
    tx: &crossbeam_channel::Sender<CollectedFile>,
) -> ignore::WalkState {
    use std::sync::atomic::Ordering;

    if !entry.file_type().is_some_and(|ft| ft.is_file()) {
        return ignore::WalkState::Continue;
    }
    let path = entry.path().to_path_buf();
    // Reject files inside globally ignored dirs (e.g. node_modules) even when
    // git-tracked. The walker's filter_entry doesn't reliably prevent descent
    // into tracked-but-ignored directories in parallel mode.
    if path.components().any(|c| {
        if let std::path::Component::Normal(s) = c {
            s.to_str().is_some_and(|name| should_ignore_dir(name))
        } else {
            false
        }
    }) {
        return ignore::WalkState::Continue;
    }
    if should_ignore_file(&path) {
        return ignore::WalkState::Continue;
    }
    let meta = match fs::metadata(&path) {
        Ok(m) if m.len() <= file_size_limit => m,
        _ => return ignore::WalkState::Continue,
    };
    let prev = count.fetch_add(1, Ordering::AcqRel);
    if prev >= MAX_FILES {
        return ignore::WalkState::Quit;
    }
    let mtime = extract_mtime(&meta, &path);
    if tx.send(CollectedFile { path, mtime }).is_err() {
        return ignore::WalkState::Quit;
    }
    ignore::WalkState::Continue
}

/// Collect file paths using `git ls-files` for git repos (the universal, correct source
/// of "what files belong to this project"), falling back to filesystem walk for non-git dirs.
///
/// First-principles reasoning: the user's git index is the single source of truth for
/// what constitutes "their code." It handles .gitignore, monorepos, workspaces, and
/// any project structure without heuristics or hardcoded ignore lists.
fn collect_paths(root: &Path, file_size_limit: u64) -> Vec<CollectedFile> {
    // Try git ls-files first — the universal correct approach
    if let Some(files) = collect_paths_git(root, file_size_limit) {
        if !files.is_empty() {
            crate::debug_log!("[scan] using git ls-files ({} tracked files)", files.len());
            return files;
        }
    }
    // Fallback: filesystem walk for non-git directories
    crate::debug_log!("[scan] not a git repo, falling back to filesystem walk");
    collect_paths_walk(root, file_size_limit)
}

/// Collect files via `git ls-files` — returns None if not a git repo or git fails.
/// This is the primary path: git already knows every tracked file, respects .gitignore,
/// handles monorepos/workspaces, and requires zero heuristic filtering.
fn collect_paths_git(root: &Path, file_size_limit: u64) -> Option<Vec<CollectedFile>> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "-z"])  // null-delimited for safe path handling
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None; // not a git repo or git not available
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let total_git = stdout.split('\0').filter(|s| !s.is_empty()).count();
    let mut ignored_ext = 0u32;
    let mut meta_fail = 0u32;
    let mut too_big = 0u32;
    let files: Vec<CollectedFile> = stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .take(MAX_FILES)
        .filter_map(|rel| {
            let abs = root.join(rel);
            // Skip files inside globally ignored dirs (e.g. node_modules)
            // even when git-tracked — some repos commit their dependencies.
            if std::path::Path::new(rel).components().any(|c| {
                matches!(c, std::path::Component::Normal(s) if s.to_str().is_some_and(|n| should_ignore_dir(n)))
            }) {
                ignored_ext += 1;
                return None;
            }
            if should_ignore_file(&abs) {
                ignored_ext += 1;
                return None;
            }
            let meta = match fs::metadata(&abs) {
                Ok(m) => m,
                Err(_) => { meta_fail += 1; return None; }
            };
            if !meta.is_file() || meta.len() > file_size_limit {
                if meta.len() > file_size_limit { too_big += 1; }
                return None;
            }
            let mtime = extract_mtime(&meta, &abs);
            Some(CollectedFile { path: abs, mtime })
        })
        .collect();

    let dropped = total_git - files.len();
    if dropped > 0 {
        eprintln!(
            "[scan] git ls-files: {} total, {} kept, {} dropped (ext:{}, meta:{}, big:{})",
            total_git, files.len(), dropped, ignored_ext, meta_fail, too_big
        );
    }
    Some(files)
}

/// Fallback: filesystem walk for non-git directories.
/// Uses `ignore` crate with hardcoded ignore list (only for non-git repos).
fn collect_paths_walk(root: &Path, file_size_limit: u64) -> Vec<CollectedFile> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let count = Arc::new(AtomicUsize::new(0));
    // MUST be unbounded: run() blocks until all walker threads finish, and
    // rx.iter() only runs after run() returns. A bounded channel deadlocks
    // when walker threads fill it and block on send() — nobody is reading.
    let (tx, rx) = crossbeam_channel::unbounded::<CollectedFile>();

    let count_w = Arc::clone(&count);
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(20))
        .threads(rayon::current_num_threads().min(8))
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                return !should_ignore_dir(&name);
            }
            true
        })
        .build_parallel()
        .run(|| {
            let tx = tx.clone();
            let count = Arc::clone(&count_w);
            Box::new(move |result| {
                if count.load(Ordering::Acquire) >= MAX_FILES {
                    return ignore::WalkState::Quit;
                }
                if let Ok(entry) = result {
                    // Skip ignored dirs even when git-tracked (e.g. committed node_modules).
                    // filter_entry doesn't apply in parallel mode, so check here.
                    if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                        let name = entry.file_name().to_string_lossy();
                        if should_ignore_dir(&name) {
                            return ignore::WalkState::Skip;
                        }
                    }
                    return process_walk_entry(&entry, file_size_limit, &count, &tx);
                }
                ignore::WalkState::Continue
            })
        });
    drop(tx); // close sender so rx.iter() terminates

    rx.iter().collect()
}


/// Scan + parse a single file in one pass: read once, count lines, tree-sitter parse.
/// No tokei dependency — line counts computed from raw bytes + AST comment nodes.
fn scan_and_parse_file(
    collected: &CollectedFile,
    root: &Path,
    max_parse_size_kb: usize,
) -> FileNode {
    let rel = collected.path.strip_prefix(root).unwrap_or(&collected.path);
    // Normalize to forward slashes — ONE place, ALL platforms.
    // Every downstream consumer (resolver, graph builder, treemap) uses `/`.
    let rel_str = common::normalize_path(rel.to_string_lossy());
    let name = collected.path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let lang = detect_lang(&collected.path);

    // Read content ONCE — used for both line counting and tree-sitter parse
    let content = match fs::read(&collected.path) {
        Ok(c) => c,
        Err(_) => {
            return FileNode {
                path: rel_str, name, is_dir: false,
                lines: 0, logic: 0, comments: 0, blanks: 0,
                funcs: 0, mtime: collected.mtime, gs: String::new(),
                lang, sa: None, children: None,
            };
        }
    };

    // Count total lines + blank lines from raw bytes (microseconds, zero alloc)
    let lc = count_lines_from_bytes(&content);

    // Tree-sitter parse (if language supported and file within parse size limit)
    let (sa, comment_count) = if !lang.is_empty() && lang != "unknown"
        && content.len() <= max_parse_size_kb * 1024
    {
        match crate::analysis::parser::parse_file_from_content(&content, &lang) {
            Some(sa) => {
                let cl = sa.comment_lines.unwrap_or(0);
                (Some(sa), cl)
            }
            None => (None, 0),
        }
    } else {
        (None, 0)
    };

    let total = lc.total;
    let blanks = lc.blanks;
    let comments = comment_count;
    let logic = total.saturating_sub(comments).saturating_sub(blanks);
    let funcs = sa.as_ref().and_then(|s| s.functions.as_ref()).map_or(0, |v| v.len() as u32);

    FileNode {
        path: rel_str, name, is_dir: false,
        lines: total, logic, comments, blanks,
        funcs, mtime: collected.mtime, gs: String::new(),
        lang, sa, children: None,
    }
}

/// Collect files, scan + parse each in parallel. One read per file, cancellable.
/// Replaces the old three-phase approach (collect → tokei → scan → parse).
fn walk_and_scan_files(
    root: &Path,
    max_file_size: u64,
    max_parse_size_kb: usize,
    scan_t0: std::time::Instant,
    emit: &dyn Fn(&str, u8),
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Vec<FileNode> {
    emit("Collecting files\u{2026}", 5);
    let collected = collect_paths(root, max_file_size * 1024);
    let total_files = collected.len();
    crate::debug_log!("[scan] collect_paths: {:.1}ms ({} files)", scan_t0.elapsed().as_secs_f64() * 1000.0, total_files);

    emit(&format!("Scanning & parsing ({total_files} files)"), 15);

    // Parallel scan + parse per file with cancel check.
    // Progress is reported via atomic counter — the emit callback runs on
    // the main scan thread after rayon completes, not inside rayon workers.
    let files: Vec<FileNode> = collected
        .par_iter()
        .filter_map(|c| {
            if let Some(ct) = cancel {
                if ct.load(std::sync::atomic::Ordering::Relaxed) {
                    return None;
                }
            }
            Some(scan_and_parse_file(c, root, max_parse_size_kb))
        })
        .collect();

    crate::debug_log!("[scan] scan_and_parse: {:.1}ms ({} files)", scan_t0.elapsed().as_secs_f64() * 1000.0, files.len());
    emit(&format!("Scanned {total_files} files"), 50);
    files
}

/// Apply git statuses to file nodes in-place.
fn apply_git_statuses(files: &mut [FileNode], root_path: &str, scan_t0: std::time::Instant, emit: &dyn Fn(&str, u8)) {
    let total_files = files.len();
    emit(&format!("Git status ({total_files} files)"), 40);
    let git_statuses = crate::analysis::git::get_statuses(root_path);
    for file in files.iter_mut() {
        if let Some(gs) = git_statuses.get(&file.path) {
            file.gs = gs.clone();
        }
    }
    crate::debug_log!("[scan] git_status: {:.1}ms", scan_t0.elapsed().as_secs_f64() * 1000.0);
}

/// Poll parse progress until completion, emitting progress updates.
/// Accepts the parse thread handle to detect panics — if the thread dies
/// before all work is done, we break instead of spinning forever. [C2 fix]
/// Context for the tree-building and graph-building phase of a scan.
struct BuildContext<'a> {
    root: &'a Path,
    max_call_targets: usize,
    scan_t0: std::time::Instant,
    emit: &'a dyn Fn(&str, u8),
    on_tree_ready: Option<&'a dyn Fn(Snapshot)>,
}

/// Build the file tree and emit a tree-ready snapshot, then build graphs.
fn build_tree_and_graphs(
    files: Vec<FileNode>,
    bctx: &BuildContext<'_>,
) -> ScanResult {
    // Use u64 to prevent overflow when summing line counts across many files. [ref:4e8f1175]
    let total_lines: u32 = files.iter().map(|f| f.lines as u64).sum::<u64>().min(u32::MAX as u64) as u32;
    let total_files = files.len() as u32;
    let root_name = bctx.root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    (bctx.emit)(&format!("Building tree ({total_files} files)"), 65);
    let (tree, total_dirs) = build_tree(files, &root_name);
    let tree = Arc::new(tree);

    if let Some(cb) = bctx.on_tree_ready {
        cb(Snapshot {
            root: Arc::clone(&tree),
            total_files, total_lines, total_dirs,
            call_graph: Vec::new(),
            import_graph: Vec::new(),
            inherit_graph: Vec::new(),
            entry_points: Vec::new(),
            exec_depth: HashMap::new(),
        });
    }

    crate::debug_log!("[scan] tree_ready sent at: {:.1}ms", bctx.scan_t0.elapsed().as_secs_f64() * 1000.0);
    (bctx.emit)(&format!("Building graphs ({total_files} files, {total_dirs} dirs)"), 85);
    let flat_files = crate::core::snapshot::flatten_files_ref(&tree);
    let gr = crate::analysis::graph::build_graphs(&flat_files, Some(bctx.root), bctx.max_call_targets);

    crate::debug_log!("[scan] build_graphs done at: {:.1}ms | {} import, {} call, {} inherit edges",
        bctx.scan_t0.elapsed().as_secs_f64() * 1000.0, gr.import_edges.len(), gr.call_edges.len(), gr.inherit_edges.len());
    (bctx.emit)("Done", 100);

    ScanResult {
        snapshot: Snapshot {
            root: tree, total_files, total_lines, total_dirs,
            call_graph: gr.call_edges,
            import_graph: gr.import_edges,
            inherit_graph: gr.inherit_edges,
            entry_points: gr.entry_points,
            exec_depth: gr.exec_depth,
        },
    }
}

/// Main scan function: collect files, scan + parse each in parallel, build tree + graphs.
/// Single read per file — no tokei dependency, immediate cancellation between files.
pub fn scan_directory(
    root_path: &str,
    on_progress: Option<&dyn Fn(ScanProgress)>,
    on_tree_ready: Option<&dyn Fn(Snapshot)>,
    limits: &ScanLimits,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ScanResult, AppError> {
    let scan_t0 = std::time::Instant::now();
    let root = Path::new(root_path);
    if !root.exists() || !root.is_dir() {
        return Err(AppError::Path(format!("Not a valid directory: {}", root_path)));
    }

    let emit = |step: &str, pct: u8| {
        if let Some(cb) = on_progress {
            cb(ScanProgress { step: step.into(), pct });
        }
    };

    // Single pass: collect + scan + parse per file (no tokei, no separate parse phase)
    let mut files = walk_and_scan_files(
        root, limits.max_file_size_kb, limits.max_parse_size_kb,
        scan_t0, &emit, cancel,
    );

    // Check cancel
    if let Some(ct) = cancel {
        if ct.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(AppError::Scan("Scan cancelled".into()));
        }
    }

    apply_git_statuses(&mut files, root_path, scan_t0, &emit);

    let bctx = BuildContext {
        root, max_call_targets: limits.max_call_targets, scan_t0, emit: &emit, on_tree_ready,
    };
    Ok(build_tree_and_graphs(files, &bctx))
}

/// Re-export for backward compatibility.
pub use self::common::ScanResult;
