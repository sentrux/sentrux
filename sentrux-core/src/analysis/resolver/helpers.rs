//! Helper functions for module resolution: path normalization, closest-match
//! picking, relative import resolution, name-based resolution, and suffix-index
//! lookup with progressive prefix stripping.
//!
//! Extracted from resolver_suffix.rs to keep that module under 500 lines.

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// All known package-index filenames — aggregated from all loaded plugins.
/// Cached at first access via LazyLock.
static PACKAGE_INDEX_SET: std::sync::LazyLock<std::collections::HashSet<String>> =
    std::sync::LazyLock::new(|| {
        crate::analysis::lang_registry::all_package_index_files()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    });

/// Result of building the module suffix index.
pub(crate) struct SuffixIndex<'a> {
    /// Standard suffix index: file-path suffixes -> files
    pub(super) index: HashMap<String, Vec<&'a str>>,
    /// Manifest-derived name aliases: package/crate name → entry point file.
    /// Separate from general index to allow safe single-segment lookup.
    pub(super) manifest_name_aliases: HashMap<String, Vec<&'a str>>,
    /// Module prefix map: module path (from prefix files like go.mod) -> project dir.
    /// e.g. "github.com/user/repo" -> "server" when server's prefix file declares that module.
    pub(super) module_prefixes: Vec<(String, String)>,
}

/// Convert a file path to its module path.
/// Package index files -> parent dir. Everything else -> strip extension.
pub(super) fn file_to_module_path(file_path: &str) -> &str {
    let filename = file_path.rsplit('/').next().unwrap_or(file_path);
    let is_package_index = PACKAGE_INDEX_SET.contains(filename);
    if is_package_index {
        file_path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
    } else {
        file_path.rfind('.').map(|i| &file_path[..i]).unwrap_or("")
    }
}

/// Shared environment for module resolution — bundles the indexes and file-type
/// extensions that every resolve call needs.
pub(super) struct ResolveEnv<'a> {
    pub suffix_index: &'a SuffixIndex<'a>,
    pub known_files: &'a HashSet<&'a str>,
    pub exts: &'a [&'a str],
    /// Whether the importing language treats directories as packages.
    /// When true, a single-segment import matching multiple files in the same
    /// directory is CORRECT (they're all part of one package).
    /// Driven by `directory_is_package` in plugin TOML.
    pub directory_is_package: bool,
}

/// Strip a known module prefix from an import specifier.
/// e.g. "github.com/user/repo/internal/config" with module "github.com/user/repo"
/// returns Some("internal/config"). If the specifier also has a project dir prefix
/// (e.g. module is in "server/"), prepends it: "server/internal/config".
fn strip_module_prefix<'a>(specifier: &'a str, module_prefixes: &[(String, String)]) -> Option<String> {
    for (module_path, project_dir) in module_prefixes {
        if let Some(rest) = specifier.strip_prefix(module_path.as_str()) {
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            if rest.is_empty() {
                continue; // import of the module root itself, skip
            }
            if project_dir.is_empty() {
                return Some(rest.to_string());
            }
            return Some(format!("{}/{}", project_dir, rest));
        }
    }
    None
}

/// Progressive left-prefix stripping against the suffix index.
pub(super) fn try_suffix_resolve(
    specifier: &str,
    env: &ResolveEnv<'_>,
    file_dir_str: &str,
    file_dir: &Path,
) -> Option<String> {
    // Fast path: module-qualified imports — strip the module prefix first
    // so we jump straight to the local path instead of progressive stripping.
    if !env.suffix_index.module_prefixes.is_empty() {
        if let Some(local_path) = strip_module_prefix(specifier, &env.suffix_index.module_prefixes) {
            // Try the module-stripped path through the suffix index
            if let Some(result) = try_suffix_resolve_inner(&local_path, env, file_dir_str, file_dir) {
                return Some(result);
            }
        }
        // Module prefixes are configured (e.g., Go with go.mod) but this
        // specifier doesn't match any → it's an external dependency (stdlib
        // or third-party). Don't fall through to progressive suffix stripping,
        // which would create false edges (e.g., "encoding/json" → "json" →
        // matches library/json/).
        return None;
    }

    try_suffix_resolve_inner(specifier, env, file_dir_str, file_dir)
}

/// Inner suffix resolve: progressive left-prefix stripping against the suffix index.
fn try_suffix_resolve_inner(
    specifier: &str,
    env: &ResolveEnv<'_>,
    file_dir_str: &str,
    file_dir: &Path,
) -> Option<String> {
    let stripped = specifier.rfind('.').map(|i| &specifier[..i]);
    let specs: &[&str] = if let Some(s) = stripped {
        if specifier.rfind('/').is_none_or(|slash| specifier.rfind('.').unwrap() > slash) {
            &[specifier, s]
        } else {
            &[specifier]
        }
    } else {
        &[specifier]
    };

    for &spec in specs {
        let mut remainder = spec;
        while remainder.contains('/') {
            if let Some(candidates) = env.suffix_index.index.get(remainder) {
                return Some(pick_closest(candidates, file_dir_str).to_string());
            }
            remainder = &remainder[remainder.find('/').unwrap() + 1..];
        }
        if let Some(candidates) = env.suffix_index.index.get(remainder) {
            if candidates.len() == 1 {
                return Some(candidates[0].to_string());
            }
            // For directory_is_package languages (e.g., Go): a single-segment
            // import matching multiple files is CORRECT — they're all part of
            // the same package directory. Use pick_closest to select one.
            if env.directory_is_package && !candidates.is_empty() {
                return Some(pick_closest(candidates, file_dir_str).to_string());
            }
        }
        if let Some(found) = try_resolve_name(remainder, file_dir, env.known_files, env.exts) {
            return Some(found);
        }
    }
    None
}

/// Try to resolve a module name to a file, checking all extensions and package index files.
///
/// Handles TypeScript ESM pattern: `./fleet.js` → tries `fleet.js` (exact),
/// then `fleet.ts`, `fleet.tsx` (extension substitution), then `fleet/index.ts`.
pub(super) fn try_resolve_name(name: &str, base_dir: &Path, known_files: &HashSet<&str>, exts: &[&str]) -> Option<String> {
    let joined = base_dir.join(name);

    // A. Exact match
    let exact = normalize_path(&joined);
    if known_files.contains(exact.as_str()) {
        return Some(exact);
    }

    // B. Try appending extensions (extensionless imports: "./fleet" → "fleet.ts")
    for ext in exts {
        let candidate = format!("{}.{}", exact, ext);
        if known_files.contains(candidate.as_str()) {
            return Some(candidate);
        }
    }

    // C. Extension substitution: "./fleet.js" → strip ".js" → try "fleet.ts"
    // Handles TypeScript ESM where imports use .js but files are .ts
    if let Some(dot_pos) = name.rfind('.') {
        let name_without_ext = &name[..dot_pos];
        let stem = base_dir.join(name_without_ext);
        let stem_path = normalize_path(&stem);
        // Try each extension as a replacement
        for ext in exts {
            let candidate = format!("{}.{}", stem_path, ext);
            if known_files.contains(candidate.as_str()) {
                return Some(candidate);
            }
        }
        // Also try package index under the stem name
        for index_file in PACKAGE_INDEX_SET.iter() {
            let candidate = normalize_path(&stem.join(index_file));
            if known_files.contains(candidate.as_str()) {
                return Some(candidate);
            }
        }
    }

    // D. Package index files (directory import: "./utils" → "utils/index.ts")
    for index_file in PACKAGE_INDEX_SET.iter() {
        let candidate = normalize_path(&joined.join(index_file));
        if known_files.contains(candidate.as_str()) {
            return Some(candidate);
        }
    }

    None
}

/// Resolve a relative import (starts with '.').
pub(super) fn resolve_relative(specifier: &str, file_dir: &Path, known_files: &HashSet<&str>, exts: &[&str]) -> Option<String> {
    let dots = specifier.bytes().take_while(|&b| b == b'.').count();
    let remainder = &specifier[dots..];
    let mut base = file_dir.to_path_buf();
    for _ in 1..dots {
        match base.parent() {
            Some(p) => base = p.to_path_buf(),
            None => return None, // Already at filesystem root — can't go higher
        }
    }
    if remainder.is_empty() {
        for index_file in PACKAGE_INDEX_SET.iter() {
            let candidate = normalize_path(&base.join(index_file));
            if known_files.contains(candidate.as_str()) {
                return Some(candidate);
            }
        }
        return None;
    }
    try_resolve_name(remainder.trim_start_matches('/'), &base, known_files, exts)
}

/// When multiple files match a suffix, pick the one closest to the importer.
pub(super) fn pick_closest<'a>(candidates: &[&'a str], file_dir: &str) -> &'a str {
    if candidates.len() == 1 {
        return candidates[0];
    }
    let dir_parts: Vec<&str> = if file_dir.is_empty() { Vec::new() } else { file_dir.split('/').collect() };
    let mut best = candidates[0];
    let mut best_shared = 0usize;
    for &c in candidates {
        let c_dir = c.rfind('/').map(|i| &c[..i]).unwrap_or("");
        let c_parts: Vec<&str> = if c_dir.is_empty() { Vec::new() } else { c_dir.split('/').collect() };
        let shared = c_parts.iter()
            .zip(dir_parts.iter())
            .take_while(|(a, b)| a == b)
            .count();
        if shared > best_shared || (shared == best_shared && c < best) {
            best_shared = shared;
            best = c;
        }
    }
    best
}

/// Normalize a path by resolving `.` and `..` components without filesystem access.
pub(crate) fn normalize_path(path: &Path) -> String {
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    let mut underflow = 0u32;
    let mut is_absolute = false;
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if parts.pop().is_none() {
                    underflow += 1;
                }
            }
            std::path::Component::Normal(s) => {
                parts.push(s);
            }
            std::path::Component::RootDir => {
                parts.clear();
                underflow = 0;
                is_absolute = true;
            }
            std::path::Component::Prefix(p) => {
                parts.clear();
                underflow = 0;
                parts.push(p.as_os_str());
            }
        }
    }
    if underflow > 0 && !is_absolute {
        return String::new();
    }
    let suffix: Vec<std::borrow::Cow<'_, str>> = parts.iter().map(|s| s.to_string_lossy()).collect();
    let all: Vec<&str> = suffix.iter().map(|s| s.as_ref()).collect();
    let joined = all.join("/");
    if is_absolute {
        format!("/{}", joined)
    } else {
        joined
    }
}
