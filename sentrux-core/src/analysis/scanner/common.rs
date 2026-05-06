//! Shared types and utilities for scanner and scanner_rescan.
//!
//! Extracted to break the circular dependency between scanner.rs and scanner_rescan.rs.
//! Both modules import from here instead of from each other.

use crate::core::snapshot::Snapshot;
use std::path::{Path, PathBuf};

pub(crate) const MAX_FILES: usize = 100_000;

/// Normalize a path to forward slashes. Called at every entry point where
/// OS paths become string keys. All downstream code uses `/` only.
#[inline]
pub(crate) fn normalize_path(path: std::borrow::Cow<'_, str>) -> String {
    if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.into_owned()
    }
}

/// Interface for scanning a directory into a structured snapshot.
/// Enables alternative implementations for testing or cached scanning.
pub trait DirectoryScanner {
    /// Scan a directory and produce a snapshot of its structure and dependencies.
    fn scan(&self, root: &Path, max_file_size_kb: u64, max_parse_size_kb: u64) -> Result<ScanResult, crate::core::types::AppError>;
}

/// Return type that bundles the scan result.
pub struct ScanResult {
    /// The complete snapshot produced by the scan
    pub snapshot: Snapshot,
}

/// Resource limits for scanning — always passed together across scan functions.
#[derive(Debug, Clone, Copy)]
pub struct ScanLimits {
    /// Maximum file size in KB to include in scan
    pub max_file_size_kb: u64,
    /// Maximum file size in KB for structural parsing
    pub max_parse_size_kb: usize,
    /// Maximum call targets per call name before skipping
    pub max_call_targets: usize,
}

/// Detect language from file extension — delegates to lang_registry,
/// with fallback for non-parseable languages (display-only).
/// Also checks filename for extensionless files like Dockerfile.
pub(crate) fn detect_lang(path: &Path) -> String {
    // Check filename first for extensionless files (Dockerfile, Makefile, etc.)
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if let Some(lang) = crate::analysis::lang_registry::detect_lang_from_filename(name) {
            return lang.to_string();
        }
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => crate::analysis::lang_registry::detect_lang_from_ext(ext),
        None => "unknown".into(),
    }
}

/// Global ignored dirs (OS/tool artifacts, not language-specific).
/// Language-specific ignored dirs come from plugin.toml [semantics.project].
const GLOBAL_IGNORED_DIRS: &[&str] = &[
    ".git", ".jj", ".DS_Store", ".claude", ".cognitive", ".beemem",
    "lib64", "include",
];

/// Merged ignored dirs: global + all plugins. Cached at first access.
static ALL_IGNORED_DIRS: std::sync::LazyLock<std::collections::HashSet<String>> =
    std::sync::LazyLock::new(|| {
        let mut set: std::collections::HashSet<String> = GLOBAL_IGNORED_DIRS.iter()
            .map(|s| s.to_string()).collect();
        for dir in crate::analysis::lang_registry::all_ignored_dirs() {
            set.insert(dir.to_string());
        }
        set
    });

/// Extensions to ignore
const IGNORED_EXTENSIONS: &[&str] = &[
    "pyc", "pyo", "swp", "swo", "tmp", "bak", "orig", "db", "sqlite", "sqlite3", "o", "so",
    "dylib", "a", "dll", "exe", "wasm", "class", "jar", "png", "jpg", "jpeg", "gif", "ico",
    "svg", "mp3", "mp4", "wav", "webp", "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "lock",
    "parquet", "csv", "tsv", "h5", "hdf5", "pkl", "pickle", "npy", "npz", "bin", "dat", "pack",
    "idx", "onnx", "pt", "pth", "safetensors", "gguf", "log", "pdf", "dmg",
];

/// Check if a directory name should be ignored during scanning.
/// Checks both global ignored dirs and per-language ignored dirs from plugins.
pub(crate) fn should_ignore_dir(name: &str) -> bool {
    ALL_IGNORED_DIRS.contains(name)
}

/// Check if a file should be ignored based on its extension.
pub(crate) fn should_ignore_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if IGNORED_EXTENSIONS.contains(&ext) {
            return true;
        }
    }
    false
}

/// Line counts computed from raw file content (no external dependency).
pub(crate) struct LineCounts {
    pub total: u32,
    pub blanks: u32,
}

/// Count total lines and blank lines from raw bytes.
/// Single pass, zero allocation, O(N) in file size.
/// Replaces the entire tokei dependency.
pub(crate) fn count_lines_from_bytes(content: &[u8]) -> LineCounts {
    if content.is_empty() {
        return LineCounts { total: 0, blanks: 0 };
    }
    let mut total: u32 = 0;
    let mut blanks: u32 = 0;
    let mut line_has_non_ws = false;

    for &b in content {
        if b == b'\n' {
            total += 1;
            if !line_has_non_ws {
                blanks += 1;
            }
            line_has_non_ws = false;
        } else if b != b' ' && b != b'\t' && b != b'\r' {
            line_has_non_ws = true;
        }
    }
    // Handle last line without trailing newline
    if content.last() != Some(&b'\n') {
        total += 1;
        if !line_has_non_ws {
            blanks += 1;
        }
    }
    LineCounts { total, blanks }
}
