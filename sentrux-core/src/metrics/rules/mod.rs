//! Architecture rules engine — `.sentrux/rules.toml` parser and checker.
//!
//! Externalizes architectural judgment into machine-readable constraints.
//! The cybernetic governor: human defines desired state, sentrux enforces it.
//!
//! Usage:
//!   1. Create `.sentrux/rules.toml` in project root
//!   2. Run `sentrux check` or call `sentrux.check_rules()` via MCP
//!   3. Exit code 0 = all rules pass, 1 = violations found

use super::arch::ArchReport;
use super::types::HealthReport;
use crate::core::types::ImportEdge;
pub mod checks;
#[cfg(test)]
mod tests;

pub use self::checks::{Constraints, RuleCheckResult, RuleViolation, Severity};
use serde::Deserialize;
use std::path::Path;

// ── Rule definitions (parsed from TOML) ──

/// Root config structure for `.sentrux/rules.toml`.
///
/// Supports per-language constraint overrides via `[language.<name>.constraints]`:
/// ```toml
/// [constraints]
/// max_cc = 20              # global default
///
/// [language.python.constraints]
/// max_cc = 10              # stricter for Python
///
/// [language.rust.constraints]
/// max_cc = 25              # more lenient for Rust match arms
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct RulesConfig {
    /// Structural constraints (thresholds on existing metrics).
    #[serde(default)]
    pub constraints: Constraints,

    /// Per-language constraint overrides (highest priority in the cascade).
    /// Keys are language names (e.g., "python", "rust").
    #[serde(default)]
    pub language: std::collections::HashMap<String, LanguageConstraints>,

    /// Layer definitions for dependency direction enforcement.
    #[serde(default)]
    pub layers: Vec<LayerDef>,

    /// Explicit deny rules between file patterns.
    #[serde(default)]
    pub boundaries: Vec<BoundaryRule>,
}

/// Per-language section in rules.toml.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct LanguageConstraints {
    /// Language-specific constraint overrides.
    #[serde(default)]
    pub constraints: Constraints,
}

impl RulesConfig {
    /// Get effective constraints for a specific language.
    /// Merges: language-specific overrides > global constraints.
    pub fn effective_constraints(&self, lang: &str) -> Constraints {
        match self.language.get(lang) {
            Some(lc) => self.constraints.merge(&lc.constraints),
            None => self.constraints.clone(),
        }
    }
}


/// A named layer with glob patterns for file matching.
#[derive(Debug, Clone, Deserialize)]
pub struct LayerDef {
    /// Layer name (e.g., "presentation", "domain", "infrastructure").
    pub name: String,
    /// Glob patterns matching files in this layer (e.g., "src/ui/*").
    pub paths: Vec<String>,
    /// Layer order (lower = more foundational like core/infrastructure, higher = higher layer like app/presentation).
    /// Dependencies flow upward: foundational layers can be imported by higher layers, but not vice versa.
    /// If not specified, order is determined by position in the array.
    pub order: Option<u32>,
}

/// Deny rule: files matching `from` must not import files matching `to`.
#[derive(Debug, Clone, Deserialize)]
pub struct BoundaryRule {
    /// Glob pattern for source files.
    pub from: String,
    /// Glob pattern for target files.
    pub to: String,
    /// Human-readable reason for the rule.
    #[serde(default)]
    pub reason: String,
}


// ── Loading ──

impl RulesConfig {
    /// Load rules from a TOML file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        toml::from_str(&content)
            .map_err(|e| format!("Failed to parse {}: {e}", path.display()))
    }

    /// Try to load from `.sentrux/rules.toml` relative to a root directory.
    /// Returns None if the file doesn't exist.
    pub fn try_load(root: &Path) -> Option<Self> {
        let path = root.join(".sentrux").join("rules.toml");
        if path.exists() {
            match Self::load(&path) {
                Ok(config) => Some(config),
                Err(e) => {
                    eprintln!("Warning: {e}");
                    None
                }
            }
        } else {
            None
        }
    }
}

// ── Checking ──

/// Check all rules against current health and architecture reports.
/// Orchestrates per-rule helpers from `rules_checks` module.
pub fn check_rules(
    config: &RulesConfig,
    health: &HealthReport,
    arch: &ArchReport,
    edges: &[ImportEdge],
) -> RuleCheckResult {
    use self::checks::*;

    let mut violations = Vec::new();
    let mut rules_checked = 0;
    let c = &config.constraints;

    // ── Constraint checks (each helper returns Option<RuleViolation>) ──
    let checks: [(&dyn Fn() -> Option<RuleViolation>, bool); 13] = [
        // Root cause gates
        (&|| check_min_quality(c, health), c.min_quality.is_some()),
        (&|| check_min_modularity(c, health), c.min_modularity.is_some()),
        (&|| check_min_acyclicity(c, health), c.min_acyclicity.is_some()),
        (&|| check_min_depth(c, health), c.min_depth.is_some()),
        (&|| check_min_equality(c, health), c.min_equality.is_some()),
        (&|| check_min_redundancy(c, health), c.min_redundancy.is_some()),
        // Specific engineering limits
        (&|| check_max_coupling(c, health), c.max_coupling_score.is_some()),
        (&|| check_max_cycles(c, health), c.max_cycles.is_some()),
        (&|| check_max_cc(c, health), c.max_cc.is_some()),
        (&|| check_max_file_lines(c, health), c.max_file_lines.is_some()),
        (&|| check_max_fn_lines(c, health), c.max_fn_lines.is_some()),
        (&|| check_no_god_files(c, health), c.no_god_files),
        (&|| check_max_upward(c, arch), c.max_upward_violations.is_some()),
    ];
    for (check_fn, active) in &checks {
        if *active {
            rules_checked += 1;
            if let Some(v) = check_fn() {
                violations.push(v);
            }
        }
    }

    // ── Layer checks ──
    if config.layers.len() >= 2 {
        rules_checked += 1;
        violations.extend(check_layers(&config.layers, edges));
    }

    // ── Boundary checks ──
    for boundary in &config.boundaries {
        rules_checked += 1;
        violations.extend(check_boundary(boundary, edges));
    }

    let passed = violations.iter().all(|v| v.severity != Severity::Error);
    RuleCheckResult { passed, violations, rules_checked }
}

/// Check layer ordering: files in lower layers (more foundational) must not import files in higher layers.
/// Layers are ordered by their `order` field: lower order = more foundational (core/infrastructure), higher order = higher layer (app/presentation).
/// Dependencies must flow upward: foundational layers can be imported by higher layers, but not vice versa.
fn check_layers(layers: &[LayerDef], edges: &[ImportEdge]) -> Vec<RuleViolation> {
    let mut violations = Vec::new();

    // Assign order to each layer (lower order = more foundational, higher order = higher layer)
    let layer_order: Vec<(usize, &LayerDef)> = layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let order = l.order.unwrap_or(i as u32) as usize;
            (order, l)
        })
        .collect();

    // For each edge, check if it violates layer ordering
    for edge in edges {
        let from_layer = find_layer(&edge.from_file, &layer_order);
        let to_layer = find_layer(&edge.to_file, &layer_order);

        if let (Some((from_ord, from_name)), Some((to_ord, to_name))) = (from_layer, to_layer) {
            // Violation: a foundational layer (lower order) imports a higher layer (higher order)
            // Lower order = more foundational. A file in order=0 importing order=2 is wrong
            // (core layer must not depend on app layer).
            if from_ord < to_ord {
                violations.push(RuleViolation {
                    rule: "layer_direction".into(),
                    severity: Severity::Error,
                    message: format!(
                        "Layer violation: {} ({}) imports {} ({}). {} must not depend on {}.",
                        edge.from_file, from_name, edge.to_file, to_name, from_name, to_name
                    ),
                    files: vec![edge.from_file.clone(), edge.to_file.clone()],
                });
            }
        }
    }

    // Deduplicate by (from_file, to_file) to avoid noise
    violations.sort_by(|a, b| a.message.cmp(&b.message));
    violations.dedup_by(|a, b| a.message == b.message);

    violations
}

/// Find which layer a file belongs to based on glob patterns.
fn find_layer<'a>(
    file: &str,
    layers: &'a [(usize, &LayerDef)],
) -> Option<(usize, &'a str)> {
    for &(order, layer) in layers {
        for pattern in &layer.paths {
            if glob_match(pattern, file) {
                return Some((order, &layer.name));
            }
        }
    }
    None
}

/// Simple glob matching: supports `*` (any within dir) and `**` (any depth).
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    // Handle exact match
    if pattern == path {
        return true;
    }

    // Handle `dir/**/*` — match files at any depth under dir (check before `/*`)
    if let Some(prefix) = pattern.strip_suffix("/**/*") {
        return path.starts_with(prefix) && path.len() > prefix.len();
    }

    // Handle `dir/**` — match files at any depth under dir
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path.starts_with(prefix) && path.len() > prefix.len();
    }

    // Handle `dir/*` — match files directly in dir
    if let Some(prefix) = pattern.strip_suffix("/*") {
        if let Some(rest) = path.strip_prefix(prefix) {
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            return !rest.contains('/');
        }
        return false;
    }

    // Handle `*.ext` — match by extension anywhere
    if let Some(ext) = pattern.strip_prefix("*.") {
        return path.ends_with(&format!(".{ext}"));
    }

    // Prefix match for patterns like "src/ui" matching "src/ui/panel.rs"
    if path.starts_with(pattern) && path.as_bytes().get(pattern.len()) == Some(&b'/') {
        return true;
    }

    false
}

/// Check a single boundary rule against all edges.
fn check_boundary(rule: &BoundaryRule, edges: &[ImportEdge]) -> Vec<RuleViolation> {
    let mut violations = Vec::new();

    for edge in edges {
        // m_from: the rule's `from` glob matches this edge's importer — the file that
        // contains the import statement (the dependent side).
        // m_to: the rule's `to` glob matches this edge's imported file — the dependency
        // target. The two checks are independent; a violation requires BOTH on the same edge.
        let m_from = glob_match(&rule.from, &edge.from_file);
        let m_to = glob_match(&rule.to, &edge.to_file);
        // Three possible outcomes per edge (false/false is trivially allowed):
        //   (true, false) — importer is in the restricted group, but the target is not the
        //     forbidden group (e.g. renderer imports core). Not the forbidden pattern: allowed.
        //   (false, true) — target is in the forbidden group, but the importer is not the
        //     restricted source (e.g. metrics imports analysis). Allowed.
        //   (true, true)  — an actual "from-group file imports to-group file" dependency
        //     exists: exactly what this deny rule forbids → violation.
        if m_from && m_to {
            violations.push(RuleViolation {
                rule: "boundary".into(),
                severity: Severity::Error,
                message: format!(
                    "Boundary violation: {} imports {}{}",
                    edge.from_file,
                    edge.to_file,
                    if rule.reason.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", rule.reason)
                    }
                ),
                files: vec![edge.from_file.clone(), edge.to_file.clone()],
            });
        }
    }

    violations
}