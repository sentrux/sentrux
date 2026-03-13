//! Tree-building logic: assembles flat FileNode lists into a hierarchical directory tree.
//!
//! Extracted from scanner.rs — pure data structure manipulation, no I/O.

use crate::core::types::FileNode;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Get the parent directory string for a file path.
pub(crate) fn parent_dir_str(file_path: &str) -> String {
    Path::new(file_path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Group files by parent directory, returning the map and the set of direct parent dirs.
pub(crate) fn group_files_by_parent(files: Vec<FileNode>) -> (HashMap<String, Vec<FileNode>>, HashSet<String>) {
    let mut all_dirs_set: HashSet<String> = HashSet::new();
    for file in &files {
        all_dirs_set.insert(parent_dir_str(&file.path));
    }
    let mut file_children: HashMap<String, Vec<FileNode>> = HashMap::new();
    for file in files {
        file_children.entry(parent_dir_str(&file.path)).or_default().push(file);
    }
    (file_children, all_dirs_set)
}

/// Expand all ancestor directory paths so every intermediate dir is present.
///
/// `all_dirs_set` starts with only the direct parent dirs of files (from
/// `group_files_by_parent`). This function walks up from each file path
/// to ensure ALL intermediate ancestor directories are added.
///
/// We track which dirs have been fully expanded (ancestors walked) separately
/// from which dirs exist in the set — a dir can be in `all_dirs_set` because
/// it's a direct parent of a file, but its own ancestors may not have been
/// added yet. Breaking early on set membership caused subtrees to become
/// orphaned when a leaf parent dir was already in the set but its ancestors
/// were not.
pub(crate) fn expand_ancestor_dirs(file_children: &HashMap<String, Vec<FileNode>>, all_dirs_set: &mut HashSet<String>) {
    // Track dirs whose ancestors have been fully walked to the root.
    let mut expanded: HashSet<String> = HashSet::new();

    let file_paths: Vec<String> = file_children.values()
        .flat_map(|v| v.iter().map(|f| f.path.clone()))
        .collect();
    for path in &file_paths {
        let mut p = Path::new(path.as_str());
        while let Some(parent) = p.parent() {
            let s = parent.to_string_lossy().to_string();
            if s.is_empty() {
                all_dirs_set.insert(s);
                break;
            }
            all_dirs_set.insert(s.clone());
            // Safe to break only if this dir was previously fully expanded
            // (meaning all its ancestors are guaranteed to be in the set).
            if !expanded.insert(s) {
                break;
            }
            p = parent;
        }
    }
}

/// Build parent→child_dirs map for O(1) lookup from dir set. Sorted for determinism.
pub(crate) fn build_dir_children_map(all_dirs_set: &HashSet<String>) -> HashMap<String, Vec<String>> {
    let mut dir_children: HashMap<String, Vec<String>> = HashMap::new();
    for dir in all_dirs_set {
        if dir.is_empty() {
            continue; // root has no parent
        }
        let parent = Path::new(dir)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        dir_children.entry(parent).or_default().push(dir.clone());
    }
    for children in dir_children.values_mut() {
        children.sort_unstable();
    }
    dir_children
}

/// Create a directory FileNode by recursively assembling children.
pub(crate) fn assemble_dir_node(
    dir_path: &str,
    file_children: &mut HashMap<String, Vec<FileNode>>,
    dir_children: &HashMap<String, Vec<String>>,
) -> FileNode {
    let mut children = Vec::new();

    // Add direct file children — remove to consume instead of cloning
    if let Some(files) = file_children.remove(dir_path) {
        children.extend(files);
    }
    // Add direct subdirectory children (O(1) lookup)
    if let Some(sub_dirs) = dir_children.get(dir_path).cloned() {
        for sub in &sub_dirs {
            children.push(assemble_dir_node(sub, file_children, dir_children));
        }
    }

    // Saturating add to prevent u32 overflow in deep directory aggregation [ref:4e8f1175]
    let (lines, logic, comments, blanks, funcs) =
        children.iter().fold((0u32, 0u32, 0u32, 0u32, 0u32), |acc, c| {
            (acc.0.saturating_add(c.lines), acc.1.saturating_add(c.logic),
             acc.2.saturating_add(c.comments), acc.3.saturating_add(c.blanks),
             acc.4.saturating_add(c.funcs))
        });

    let name = if dir_path.is_empty() {
        "root".to_string()
    } else {
        Path::new(dir_path).file_name().unwrap_or_default().to_string_lossy().to_string()
    };

    FileNode {
        path: dir_path.to_string(), name, is_dir: true,
        lines, logic, comments, blanks, funcs,
        mtime: 0.0, gs: String::new(), lang: String::new(),
        sa: None, children: Some(children),
    }
}

/// Build tree from flat list of FileNodes.
/// Uses parent→children map for O(D) directory traversal instead of O(D²).
/// Consumes the Vec to avoid cloning each FileNode. [ref:93cf32d4]
pub(crate) fn build_tree(files: Vec<FileNode>, root_name: &str) -> (FileNode, u32) {
    let file_count = files.len();
    let (mut file_children, mut all_dirs_set) = group_files_by_parent(files);
    expand_ancestor_dirs(&file_children, &mut all_dirs_set);
    let dir_children = build_dir_children_map(&all_dirs_set);
    let total_dirs = all_dirs_set.iter().filter(|d| !d.is_empty()).count() as u32;

    let mut root = assemble_dir_node("", &mut file_children, &dir_children);
    root.name = root_name.to_string();

    // Safety check: all files must be consumed into the tree.
    let orphaned: usize = file_children.values().map(|v| v.len()).sum();
    if orphaned > 0 {
        eprintln!("[tree] BUG: {} of {} files orphaned (not reachable from root)", orphaned, file_count);
        let sample: Vec<&str> = file_children.values()
            .flat_map(|v| v.iter().map(|f| f.path.as_str()))
            .take(5)
            .collect();
        eprintln!("[tree] orphaned sample: {:?}", sample);
    }

    (root, total_dirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::FileNode;

    fn test_file(path: &str) -> FileNode {
        FileNode {
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            is_dir: false, lines: 10, logic: 8, comments: 1, blanks: 1,
            funcs: 0, mtime: 0.0, gs: String::new(), lang: "go".into(),
            sa: None, children: None,
        }
    }

    #[test]
    fn deep_nested_files_not_orphaned() {
        // Regression test: files in deep subdirectories must not be lost
        // when their parent dir was already in the initial dir set but
        // had no ancestors expanded yet.
        let files = vec![
            test_file("server/go.mod"),        // parent = "server" (in initial set)
            test_file("server/internal/handler/handler.go"),
            test_file("server/internal/config/config.go"),
            test_file("scripts/test.sh"),
            test_file("README.md"),
        ];

        let (tree, _) = build_tree(files, "root");
        let flat = crate::core::snapshot::flatten_files_ref(&tree);
        assert_eq!(flat.len(), 5, "All 5 files must survive tree building, got {}", flat.len());

        let paths: Vec<&str> = flat.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"server/internal/handler/handler.go"));
        assert!(paths.contains(&"server/internal/config/config.go"));
    }
}
