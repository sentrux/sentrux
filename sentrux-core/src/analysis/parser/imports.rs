//! Import normalization, per-language extraction, base-class extraction,
//! complexity counting, and string/comment stripping utilities.
//!
//! Extracted from parser.rs to keep the main parser module focused on
//! tree-sitter integration and caching.
//!
//! Per-language extractors live in lang_extractors.rs.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

use super::lang_extractors;
pub(crate) use super::strings::strip_strings_and_comments;

// ── Import extraction & normalization ───────────────────────────────────

/// Extract and **normalize** import module paths from raw source text.
///
/// # Universal contract
/// The output is a Vec of **normalized module paths**: slash-separated segments,
/// no language syntax (no braces, no quotes, no keywords, no semicolons).
/// The resolver is completely language-agnostic — all language knowledge lives here.
///

/// Whether '.' is a module separator (not a file extension) for this language.
/// Reads from the language profile (Layer 2). Falls back to false for unknown languages.
pub(crate) fn lang_uses_dot_separator(lang: &str) -> bool {
    crate::analysis::lang_registry::profile(lang).semantics.dot_is_module_separator
}

/// Normalize a module path to slash-separated form.
/// `dots_are_separators`: true for languages where '.' means module separator
/// (Python, Java, C#, Scala, Kotlin, Ruby, PHP). False for file-path languages
/// (C/C++, Go, HTML, CSS) and Rust (uses :: which is always converted).
pub(crate) fn normalize_module_path(raw: &str, dots_are_separators: bool) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }

    // Preserve leading dots (relative imports) but normalize the rest
    let (prefix, rest) = if s.starts_with('.') {
        let dot_count = s.bytes().take_while(|&b| b == b'.').count();
        (&s[..dot_count], &s[dot_count..])
    } else {
        ("", s)
    };

    // Always convert '::' → '/' (Rust paths) and '\' → '/' (PHP namespaces).
    // Convert '.' → '/' only when the language uses dots as module separators.
    // File-path languages (C, HTML, CSS) keep dots as-is (they're file extensions).
    let mut normalized = rest.replace("::", "/").replace('\\', "/");
    if dots_are_separators && !normalized.contains('/') && rest.contains('.') {
        // Only convert dots when no slashes present (avoids mangling file paths
        // that were already slash-separated by the :: conversion).
        normalized = normalized.replace('.', "/");
    }

    format!("{}{}", prefix, normalized)
}

// ── Brace expansion ─────────────────────────────────────────────────────

/// Generic brace expansion for import paths.
/// `Prefix.{A, B, C}` → `["Prefix.A", "Prefix.B", "Prefix.C"]`
/// `Prefix::{A, B}` → `["Prefix::A", "Prefix::B"]`
///
/// Works on raw text — no AST knowledge needed. Handles any separator
/// (`.`, `::`, `/`) that appears before the `{`. Language-agnostic.
///
/// If the text contains no `{...}`, returns an empty vec (caller uses other methods).
pub(crate) fn expand_braces(text: &str) -> Vec<String> {
    let brace_start = match text.find('{') {
        Some(i) => i,
        None => return vec![],
    };
    let brace_end = match text[brace_start..].find('}') {
        Some(i) => brace_start + i,
        None => return vec![], // Malformed — no closing brace
    };

    // Find the start of the token containing `{` (go back to last whitespace)
    let word_start = text[..brace_start]
        .rfind(|c: char| c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);

    // Prefix: everything from word start to `{`
    let prefix = &text[word_start..brace_start];

    // Items: comma-separated inside `{}`
    let items_str = &text[brace_start + 1..brace_end];
    let items: Vec<&str> = items_str
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if items.is_empty() {
        return vec![];
    }

    items.iter().map(|item| format!("{}{}", prefix, item)).collect()
}

// ── Base class extraction ───────────────────────────────────────────────

/// Extract base/parent class names from a class definition AST node.
///
/// Uses three strategies in order:
/// 1. Profile `base_class_node_kinds` (data-driven, covers most languages)
/// 2. Compiled `base_class_extractor` (for Python which needs special handling)
/// 3. Generic fallback (pattern-match on node kind names)
pub(crate) fn extract_base_classes(node: tree_sitter::Node, content: &[u8], lang: &str) -> Option<Vec<String>> {
    let profile = crate::analysis::lang_registry::profile(lang);
    let mut bases = Vec::new();

    if !profile.semantics.base_class_node_kinds.is_empty() {
        // Data-driven: use node kinds from plugin.toml
        let kinds: Vec<&str> = profile.semantics.base_class_node_kinds.iter().map(|s| s.as_str()).collect();
        lang_extractors::extract_bases_by_kinds(node, content, &kinds, &mut bases, &profile.semantics);
    } else {
        // Generic fallback: pattern-match on node kind substrings
        lang_extractors::extract_bases_generic(node, content, &mut bases, &profile.semantics);
    }

    if bases.is_empty() { None } else { Some(bases) }
}

// Legacy text-based complexity counting has been removed.
// All complexity analysis is now AST-based via count_complexity_ast()
// and count_cognitive_complexity_ast() using branch_nodes/logic_nodes
// from plugin.toml [semantics.complexity].

// ── AST-based complexity counting ─────────────────────────────────────
// These functions walk the tree-sitter AST directly instead of scanning text.
// They use node kinds from the language profile (plugin.toml [semantics.complexity]).

use std::collections::HashSet as CxHashSet;

/// Check if a node's operator text matches one of the logic operators.
fn is_logic_operator(node: tree_sitter::Node, content: &[u8], operators: &[String]) -> bool {
    if operators.is_empty() {
        return true; // No filter = count all logic_nodes
    }
    // Check the "operator" field first (many grammars have it)
    if let Some(op_node) = node.child_by_field_name("operator") {
        if let Ok(op_text) = op_node.utf8_text(content) {
            return operators.iter().any(|op| op == op_text.trim());
        }
    }
    // Fallback: check if any child is one of the operators
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if !child.is_named() {
                if let Ok(text) = child.utf8_text(content) {
                    if operators.iter().any(|op| op == text.trim()) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Count nesting depth of a node by walking up to the function root.
/// Only counts ancestors whose kind is in `nesting_set`.
fn nesting_depth(
    node: tree_sitter::Node,
    func_node: tree_sitter::Node,
    nesting_set: &CxHashSet<&str>,
) -> u32 {
    let mut depth = 0u32;
    let mut current = node.parent();
    let func_id = func_node.id();
    while let Some(p) = current {
        if p.id() == func_id {
            break; // Don't count beyond the function boundary
        }
        if nesting_set.contains(p.kind()) {
            depth += 1;
        }
        current = p.parent();
    }
    depth
}

/// Walk every node in the subtree rooted at `func_node`, calling `visitor` on each.
fn walk_subtree(
    func_node: tree_sitter::Node,
    mut visitor: impl FnMut(tree_sitter::Node),
) {
    let mut cursor = func_node.walk();
    let mut visited_root = false;
    loop {
        if !visited_root { visited_root = true; }
        visitor(cursor.node());
        if cursor.goto_first_child() { continue; }
        if cursor.goto_next_sibling() { continue; }
        loop {
            if !cursor.goto_parent() { return; }
            if cursor.node().id() == func_node.id() { return; }
            if cursor.goto_next_sibling() { break; }
        }
    }
}

/// Walk the AST subtree of a function node and compute cyclomatic complexity.
/// CC = 1 + (number of branch_nodes) + (number of logic_nodes with matching operator).
pub(crate) fn count_complexity_ast(
    func_node: tree_sitter::Node,
    content: &[u8],
    profile: &crate::analysis::plugin::profile::LanguageProfile,
) -> u32 {
    let cx = &profile.semantics.complexity;
    let branch_set: CxHashSet<&str> = cx.branch_nodes.iter().map(|s| s.as_str()).collect();
    let logic_set: CxHashSet<&str> = cx.logic_nodes.iter().map(|s| s.as_str()).collect();
    let mut cc = 1u32;
    walk_subtree(func_node, |node| {
        if branch_set.contains(node.kind()) {
            cc += 1;
        } else if logic_set.contains(node.kind()) && is_logic_operator(node, content, &cx.logic_operators) {
            cc += 1;
        }
    });
    cc
}

/// Walk the AST subtree of a function node and compute cognitive complexity.
/// COG = sum of (1 + nesting_depth) for each branch node + 1 for each logic operator.
pub(crate) fn count_cognitive_complexity_ast(
    func_node: tree_sitter::Node,
    content: &[u8],
    profile: &crate::analysis::plugin::profile::LanguageProfile,
) -> u32 {
    let cx = &profile.semantics.complexity;
    let branch_set: CxHashSet<&str> = cx.branch_nodes.iter().map(|s| s.as_str()).collect();
    let logic_set: CxHashSet<&str> = cx.logic_nodes.iter().map(|s| s.as_str()).collect();
    let nesting_set: CxHashSet<&str> = cx.nesting_nodes.iter().map(|s| s.as_str()).collect();
    let mut cog = 0u32;
    walk_subtree(func_node, |node| {
        if branch_set.contains(node.kind()) {
            cog += 1 + nesting_depth(node, func_node, &nesting_set);
        } else if logic_set.contains(node.kind()) && is_logic_operator(node, content, &cx.logic_operators) {
            cog += 1;
        }
    });
    cog
}

/// Default parameter list node kinds (used when plugin TOML doesn't specify).
const DEFAULT_PARAM_LIST_KINDS: &[&str] = &[
    "parameters", "formal_parameters", "parameter_list",
    "function_type_parameters", "type_parameters",
];

/// Default self/this parameter node kinds.
const DEFAULT_SELF_PARAM_KINDS: &[&str] = &["self_parameter", "self"];

/// Default self/this parameter text values.
const DEFAULT_SELF_PARAM_TEXTS: &[&str] = &["self", "&self", "&mut self", "this"];

/// Default countable parameter node kinds.
const DEFAULT_PARAM_KINDS: &[&str] = &[
    "parameter", "formal_parameter",
    "simple_parameter", "typed_parameter",
    "default_parameter", "typed_default_parameter",
    "identifier", "required_parameter",
    "optional_parameter", "rest_parameter",
    "spread_parameter", "variadic_parameter",
    "keyword_argument", "list_splat_pattern",
    "dictionary_splat_pattern",
];

/// Check if a parameter node represents self/this (should be excluded from count).
fn is_self_or_this(param: tree_sitter::Node, content: &[u8], sem: &crate::analysis::plugin::profile::LanguageSemantics) -> bool {
    let pk = param.kind();
    let self_kinds = if sem.self_param_kinds.is_empty() {
        DEFAULT_SELF_PARAM_KINDS
    } else {
        // Use slice of the Vec — lifetime ok since sem outlives this call
        &[] // handled below
    };
    if !sem.self_param_kinds.is_empty() {
        if sem.self_param_kinds.iter().any(|k| k == pk) { return true; }
    } else if self_kinds.contains(&pk) {
        return true;
    }
    if let Ok(text) = param.utf8_text(content) {
        let t = text.trim();
        if !sem.self_param_texts.is_empty() {
            sem.self_param_texts.iter().any(|s| s == t)
        } else {
            DEFAULT_SELF_PARAM_TEXTS.contains(&t)
        }
    } else {
        false
    }
}

/// Check if a node kind represents a countable parameter.
fn is_parameter_kind(kind: &str, sem: &crate::analysis::plugin::profile::LanguageSemantics) -> bool {
    if !sem.param_kinds.is_empty() {
        sem.param_kinds.iter().any(|k| k == kind)
    } else {
        DEFAULT_PARAM_KINDS.contains(&kind)
    }
}

/// Count parameters in a parameter list node, excluding self/this.
fn count_params_in_list(param_list: tree_sitter::Node, content: &[u8], sem: &crate::analysis::plugin::profile::LanguageSemantics) -> u32 {
    let mut count = 0u32;
    for j in 0..param_list.named_child_count() {
        let param = param_list.named_child(j).unwrap();
        if is_self_or_this(param, content, sem) { continue; }
        if is_parameter_kind(param.kind(), sem) {
            count += 1;
        }
    }
    count
}

/// Count function parameters from a tree-sitter node, excluding self/this.
/// Uses param_list_kinds from plugin TOML, with universal defaults as fallback.
pub(crate) fn count_parameters(node: tree_sitter::Node, content: &[u8], lang: &str) -> u32 {
    let profile = crate::analysis::lang_registry::profile(lang);
    let sem = &profile.semantics;
    for i in 0..node.child_count() {
        let child = node.child(i).unwrap();
        let is_param_list = if !sem.param_list_kinds.is_empty() {
            sem.param_list_kinds.iter().any(|k| k == child.kind())
        } else {
            DEFAULT_PARAM_LIST_KINDS.contains(&child.kind())
        };
        if is_param_list {
            return count_params_in_list(child, content, sem);
        }
    }
    0
}

/// Compute a normalized body hash for duplication detection.
/// Strips whitespace and comments, then hashes the result.
pub(crate) fn hash_body(body: &str, lang: &str) -> u64 {
    let stripped = strip_strings_and_comments(body, lang);
    // Normalize: remove all whitespace for content-only comparison
    let normalized: String = stripped.chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if normalized.len() < 20 {
        // Too short to be meaningful duplication
        return 0;
    }
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brace_expansion_elixir_style() {
        let result = expand_braces("alias Acme.Domain.{Product, Error}");
        assert_eq!(result, vec!["Acme.Domain.Product", "Acme.Domain.Error"]);
    }

    #[test]
    fn brace_expansion_three_items() {
        let result = expand_braces("alias Acme.Inventory.Domain.{Product, ProductNotFoundError, InsufficientStockError}");
        assert_eq!(result, vec![
            "Acme.Inventory.Domain.Product",
            "Acme.Inventory.Domain.ProductNotFoundError",
            "Acme.Inventory.Domain.InsufficientStockError",
        ]);
    }

    #[test]
    fn brace_expansion_no_braces() {
        let result = expand_braces("alias Acme.Shared.V1");
        assert!(result.is_empty());
    }

    #[test]
    fn brace_expansion_double_colon() {
        let result = expand_braces("use std::collections::{HashMap, BTreeMap}");
        assert_eq!(result, vec!["std::collections::HashMap", "std::collections::BTreeMap"]);
    }

    #[test]
    fn brace_expansion_no_prefix_keyword() {
        // Direct path without keyword
        let result = expand_braces("Foo.{Bar, Baz}");
        assert_eq!(result, vec!["Foo.Bar", "Foo.Baz"]);
    }

    #[test]
    fn brace_expansion_empty_braces() {
        let result = expand_braces("Foo.{}");
        assert!(result.is_empty());
    }

    #[test]
    fn normalize_dot_separator() {
        assert_eq!(normalize_module_path("os.path", true), "os/path");
        assert_eq!(normalize_module_path("os.path", false), "os.path");
    }

    #[test]
    fn normalize_rust_path() {
        assert_eq!(normalize_module_path("std::collections::HashMap", false), "std/collections/HashMap");
    }

    #[test]
    fn normalize_relative() {
        assert_eq!(normalize_module_path("..utils", true), "..utils");
        assert_eq!(normalize_module_path("...deep.path", true), "...deep/path");
    }

    #[test]
    fn normalize_php_backslash() {
        // PHP namespace imports use backslashes: App\Entity\User
        assert_eq!(normalize_module_path("App\\Entity\\User", true), "App/Entity/User");
        assert_eq!(normalize_module_path("App\\Entity\\User", false), "App/Entity/User");
    }

    #[test]
    fn normalize_php_deeply_nested() {
        assert_eq!(
            normalize_module_path("App\\GraphQL\\Resolver\\Admin\\PermissionRequestResolver", true),
            "App/GraphQL/Resolver/Admin/PermissionRequestResolver"
        );
    }
}

