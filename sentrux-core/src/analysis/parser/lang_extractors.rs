//! Per-language import extraction and base-class extraction helpers.
//!
//! Split from parser_imports.rs to keep each module focused.
//! All functions here are `pub(super)` — called only from parser_imports.rs.

// ── Per-language import extractors ──────────────────────────────────────
// Each returns raw module strings. normalize_module_path() handles the rest.

pub(super) fn extract_python(text: &str) -> Vec<String> {
    if let Some(rest) = text.strip_prefix("from ") {
        let module = rest.split_whitespace().next().unwrap_or("")
            .trim_end_matches(',')
            .to_string();
        if module.is_empty() { vec![] } else { vec![module] }
    } else if let Some(rest) = text.strip_prefix("import ") {
        let cleaned = rest.replace(['(', ')'], "");
        cleaned.split(',')
            .flat_map(|s| s.split('\n'))
            .map(|s| s.split_whitespace().next().unwrap_or("").trim_end_matches(',').to_string())
            .filter(|s| !s.is_empty() && s != "import")
            .collect()
    } else {
        vec![]
    }
}

/// Split a string at commas that are NOT inside nested braces.
/// e.g. `"episode::{Episode, Injection}, primitive"` → `["episode::{Episode, Injection}", " primitive"]`
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, ch) in s.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                result.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    result.push(&s[start..]);
    result
}

/// Strip Rust visibility modifier (pub, pub(crate), pub(super), etc.) from a declaration.
fn strip_rust_visibility(s: &str) -> &str {
    let rest = match s.strip_prefix("pub") {
        Some(r) => r.trim_start(),
        None => return s,
    };
    if rest.starts_with('(') {
        match rest.find(')') {
            Some(i) => rest[i + 1..].trim_start(),
            None => rest,
        }
    } else {
        rest
    }
}

/// Expand a Rust use-tree with braces: `prefix::{A, b, C}` → expanded module paths.
/// Lowercase/underscore items are submodules (prefix::item), uppercase are types (just prefix).
fn expand_rust_use_tree(prefix: &str, brace_content: &str) -> Vec<String> {
    let items = split_top_level_commas(brace_content);
    let mut result = Vec::new();
    let mut prefix_added = false;
    for item in &items {
        let trimmed = item.trim();
        if trimmed.is_empty() { continue; }
        let base = trimmed.split("::{").next().unwrap_or(trimmed);
        let first = base.chars().next().unwrap_or('A');
        if first.is_lowercase() || first == '_' {
            result.push(format!("{}::{}", prefix, base));
        } else if !prefix_added {
            result.push(prefix.to_string());
            prefix_added = true;
        }
    }
    if result.is_empty() {
        vec![prefix.to_string()]
    } else {
        result
    }
}

pub(super) fn extract_rust(text: &str) -> Vec<String> {
    let trimmed = text.trim().trim_end_matches(';').trim();
    let trimmed = strip_rust_visibility(trimmed);
    if let Some(modname) = trimmed.strip_prefix("mod ") {
        return vec![modname.trim().to_string()];
    }
    let rest = match trimmed.strip_prefix("use ") {
        Some(r) => r.trim(),
        None => return vec![trimmed.to_string()],
    };
    // Handle Rust use-trees: strip ::{...} or expand submodules.
    // Supports nested braces like `use crate::models::{episode::{Episode}, primitive}`
    // by splitting only at top-level commas (not inside nested braces). [ref:28b7bc6f]
    if let Some(brace_start) = rest.find("::{") {
        let prefix = &rest[..brace_start];
        let brace_end = rest.rfind('}').unwrap_or(rest.len());
        let brace_content = &rest[brace_start + 3..brace_end];
        expand_rust_use_tree(prefix, brace_content)
    } else {
        vec![rest.to_string()]
    }
}

pub(super) fn extract_go(text: &str) -> Vec<String> {
    // Go import paths are always double-quoted strings inside import declarations.
    // Aliases (`_`, `.`, named) are unquoted identifiers that precede the path.
    // By extracting only quoted strings we correctly skip all aliases without
    // heuristics that confuse aliases like `cfg` with stdlib packages.
    let mut results = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1; // skip opening quote
            let start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            if i > start {
                results.push(String::from_utf8_lossy(&bytes[start..i]).into_owned());
            }
            i += 1; // skip closing quote
        } else {
            i += 1;
        }
    }
    results
}

pub(super) fn extract_c_cpp(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("#include") {
        let path = rest.trim()
            .trim_matches(|c: char| c == '"' || c == '<' || c == '>')
            .to_string();
        if path.is_empty() { vec![] } else { vec![path] }
    } else {
        vec![]
    }
}

pub(super) fn extract_java_csharp(text: &str) -> Vec<String> {
    let s = text.trim()
        .trim_start_matches("import ")
        .trim_start_matches("using ")
        .trim_start_matches("static ")
        .trim_end_matches(';')
        .trim()
        .to_string();
    if s.is_empty() { vec![] } else { vec![s] }
}

pub(super) fn extract_ruby(text: &str) -> Vec<String> {
    let s = text.trim()
        .trim_start_matches("require_relative")
        .trim_start_matches("require")
        .trim()
        // Strip parentheses: require("json") and require_relative("./helper")
        .trim_matches(|c: char| c == '(' || c == ')')
        .trim_matches(|c: char| c == '\'' || c == '"')
        .to_string();
    if s.is_empty() { vec![] } else { vec![s] }
}

pub(super) fn extract_php(text: &str) -> Vec<String> {
    let trimmed = text.trim().trim_end_matches(';').trim();
    let s = if let Some(rest) = trimmed.strip_prefix("use ") {
        rest.trim().to_string()
    } else {
        trimmed
            .trim_start_matches("require_once ")
            .trim_start_matches("include_once ")
            .trim_start_matches("require ")
            .trim_start_matches("include ")
            .trim()
            .trim_matches(|c: char| c == '\'' || c == '"' || c == '(' || c == ')')
            .to_string()
    };
    if s.is_empty() { vec![] } else { vec![s] }
}

pub(super) fn extract_html(text: &str) -> Vec<String> {
    let trimmed = text.trim().trim_matches('"').trim_matches('\'');
    if trimmed.contains("://") || trimmed.is_empty() || !trimmed.contains('.') {
        return vec![];
    }
    vec![trimmed.trim_start_matches("./").to_string()]
}

pub(super) fn extract_css(text: &str) -> Vec<String> {
    let rest = text.trim().trim_start_matches("@import").trim();
    let s = if rest.starts_with("url(") {
        // Strip semicolon FIRST (it's outermost), then closing paren.
        // Previously `;` blocked `trim_end_matches(')')`, leaving `)` in the path.
        rest.trim_start_matches("url(")
            .trim_end_matches(';')
            .trim_end_matches(')')
            .trim()
            .trim_matches(|c: char| c == '"' || c == '\'')
            .to_string()
    } else {
        rest.trim_end_matches(';')
            .trim()
            .trim_matches(|c: char| c == '"' || c == '\'')
            .to_string()
    };
    if s.is_empty() { vec![] } else { vec![s] }
}

/// GDScript: extract path from preload("res://path") and load("res://path")
pub(super) fn extract_gdscript(text: &str) -> Vec<String> {
    let mut results = Vec::new();
    let search = text;
    for prefix in &["preload(", "load("] {
        let mut pos = 0;
        while let Some(start) = search[pos..].find(prefix) {
            let abs_start = pos + start + prefix.len();
            // Find the string argument inside quotes
            if let Some(quote_start) = search[abs_start..].find('"').or_else(|| search[abs_start..].find('\'')) {
                let q = abs_start + quote_start + 1;
                let quote_char = search.as_bytes()[abs_start + quote_start];
                if let Some(end) = search[q..].find(quote_char as char) {
                    let path = &search[q..q + end];
                    // Strip "res://" prefix and convert to relative path
                    let clean = path
                        .strip_prefix("res://")
                        .unwrap_or(path)
                        .trim_end_matches(".tscn")
                        .trim_end_matches(".tres");
                    if !clean.is_empty() {
                        results.push(clean.to_string());
                    }
                    pos = q + end;
                    continue;
                }
            }
            pos = abs_start;
        }
    }
    results
}

pub(super) fn extract_jvm_like(text: &str) -> Vec<String> {
    let s = text.trim().trim_start_matches("import ").trim().to_string();
    if s.is_empty() { vec![] } else { vec![s] }
}

pub(super) fn extract_fallback(text: &str) -> Vec<String> {
    // Search for standalone "from" keyword (word boundary), not substring inside
    // identifiers like "transform", "perform", "platform".
    let bytes = text.as_bytes();
    let mut end = text.len();
    while let Some(rel) = text[..end].rfind("from") {
        let left_ok = rel == 0 || { let c = bytes[rel - 1]; !c.is_ascii_alphanumeric() && c != b'_' };
        let right_ok = rel + 4 >= bytes.len() || { let c = bytes[rel + 4]; !c.is_ascii_alphanumeric() && c != b'_' };
        if left_ok && right_ok {
            let s = text[rel + 4..]
                .trim()
                .trim_matches(|c: char| c == '\'' || c == '"' || c == ';')
                .to_string();
            return if s.is_empty() { vec![] } else { vec![s] };
        }
        end = rel;
    }
    vec![]
}

// ── Per-language base-class extraction helpers ──────────────────────────

/// Try to extract a base class name from a single child node of a superclass list.
fn try_extract_base_name(arg: tree_sitter::Node, content: &[u8]) -> Option<String> {
    let kind = arg.kind();
    if kind != "identifier" && kind != "attribute" {
        return None;
    }
    let text = arg.utf8_text(content).ok()?;
    let name = text.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// Collect identifiers/attributes from a superclass/argument_list node.
fn collect_bases_from_list(list_node: tree_sitter::Node, content: &[u8], bases: &mut Vec<String>) {
    for j in 0..list_node.child_count() {
        let arg = list_node.child(j).unwrap();
        if let Some(name) = try_extract_base_name(arg, content) {
            bases.push(name);
        }
    }
}

/// Collect base classes for Python: class Foo(Bar, Baz): → argument_list/superclasses
pub(super) fn extract_bases_python(node: tree_sitter::Node, content: &[u8], bases: &mut Vec<String>) {
    for i in 0..node.child_count() {
        let child = node.child(i).unwrap();
        if child.kind() == "argument_list" || child.kind() == "superclasses" {
            collect_bases_from_list(child, content, bases);
        }
    }
}

/// Collect base classes for Java/Kotlin/C#/Scala via superclass/interface nodes.
pub(super) fn extract_bases_jvm(node: tree_sitter::Node, content: &[u8], bases: &mut Vec<String>) {
    for i in 0..node.child_count() {
        let child = node.child(i).unwrap();
        match child.kind() {
            "superclass" | "super_interfaces" | "type_list"
            | "extends_type_clause" | "class_type" | "delegation_specifiers" => {
                collect_type_identifiers(child, content, bases);
            }
            _ => {}
        }
    }
}

/// Collect base classes by matching child node kinds against a set of patterns.
pub(super) fn extract_bases_by_kinds(node: tree_sitter::Node, content: &[u8], kinds: &[&str], bases: &mut Vec<String>) {
    for i in 0..node.child_count() {
        let child = node.child(i).unwrap();
        if kinds.contains(&child.kind()) {
            collect_type_identifiers(child, content, bases);
        }
    }
}

/// Generic fallback: collect base classes from children whose kind contains inheritance keywords.
pub(super) fn extract_bases_generic(node: tree_sitter::Node, content: &[u8], bases: &mut Vec<String>) {
    for i in 0..node.child_count() {
        let child = node.child(i).unwrap();
        let k = child.kind();
        if k.contains("superclass") || k.contains("extends")
            || k.contains("base_class") || k.contains("heritage")
        {
            collect_type_identifiers(child, content, bases);
        }
    }
}

/// Check if a node kind is a type identifier that should be collected.
fn is_type_identifier_kind(kind: &str) -> bool {
    matches!(kind, "type_identifier" | "identifier" | "constant" | "scope_resolution")
}

/// Check if a node is a leaf type identifier (no children, or scope_resolution).
fn is_leaf_type_node(node: tree_sitter::Node) -> bool {
    is_type_identifier_kind(node.kind())
        && (node.child_count() == 0 || node.kind() == "scope_resolution")
}

/// Check if a name is a visibility keyword that should be excluded.
fn is_visibility_keyword(name: &str) -> bool {
    matches!(name, "public" | "private" | "protected")
}

/// Maximum recursion depth for type identifier collection to prevent stack overflow.
const MAX_TYPE_COLLECT_DEPTH: usize = 64;

/// Recursively collect type identifiers from an AST node.
fn collect_type_identifiers(node: tree_sitter::Node, content: &[u8], out: &mut Vec<String>) {
    collect_type_identifiers_inner(node, content, out, 0);
}

fn collect_type_identifiers_inner(node: tree_sitter::Node, content: &[u8], out: &mut Vec<String>, depth: usize) {
    if depth >= MAX_TYPE_COLLECT_DEPTH {
        return;
    }
    if is_leaf_type_node(node) {
        if let Ok(text) = node.utf8_text(content) {
            let name = text.trim().to_string();
            if !name.is_empty() && !is_visibility_keyword(&name) {
                out.push(name);
                return;
            }
        }
    }
    for i in 0..node.child_count() {
        collect_type_identifiers_inner(node.child(i).unwrap(), content, out, depth + 1);
    }
}
