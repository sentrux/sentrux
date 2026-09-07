//! Tests for architectural rule enforcement (`metrics::rules`).
//!
//! Validates rule checking against snapshots: forbidden dependency detection,
//! layer violation checks, and rule pass/fail logic. Tests cover boundary
//! (no rules = all pass), oracle (known violations produce known failures),
//! and conservation (adding a rule never removes existing violations).
//! Uses synthetic snapshots with controlled import edges.

#[cfg(test)]
mod tests {
    use crate::metrics::rules::*;
    use crate::metrics::arch;
    use crate::metrics;
    use crate::core::types::ImportEdge;
    use crate::core::types::FileNode;
    use crate::core::snapshot::Snapshot;
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::metrics::test_helpers::{edge, file};

    fn make_snapshot(edges: Vec<ImportEdge>, files: Vec<FileNode>) -> Snapshot {
        Snapshot {
            root: Arc::new(FileNode {
                path: ".".into(),
                name: ".".into(),
                is_dir: true,
                lines: 0, logic: 0, comments: 0, blanks: 0, funcs: 0,
                mtime: 0.0, gs: String::new(), lang: String::new(),
                sa: None,
                children: Some(files),
            }),
            total_files: 0, total_lines: 0, total_dirs: 0,
            call_graph: vec![], import_graph: edges,
            inherit_graph: vec![], entry_points: vec![],
            exec_depth: HashMap::new(),
        }
    }

    // ── TOML parsing ──

    #[test]
    fn parse_minimal_rules() {
        let toml = r#"
[constraints]
max_cycles = 0
no_god_files = true
"#;
        let config: RulesConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.constraints.max_cycles, Some(0));
        assert!(config.constraints.no_god_files);
        assert!(config.layers.is_empty());
        assert!(config.boundaries.is_empty());
    }

    #[test]
    fn parse_full_rules() {
        let toml = r#"
[constraints]
min_quality = 0.4
max_coupling_score = 0.35
max_cycles = 0
max_cc = 20
max_fn_lines = 80
no_god_files = true
max_upward_violations = 0

[[layers]]
name = "infrastructure"
paths = ["src/scanner.rs", "src/watcher.rs", "src/git.rs"]
order = 0

[[layers]]
name = "domain"
paths = ["src/metrics.rs", "src/graph.rs", "src/arch.rs"]
order = 1

[[layers]]
name = "presentation"
paths = ["src/ui/*", "src/renderer/*"]
order = 2

[[boundaries]]
from = "src/renderer/*"
to = "src/scanner.rs"
reason = "Renderer must not know about scanning"
"#;
        let config: RulesConfig = toml::from_str(toml).unwrap();
        assert!((config.constraints.min_quality.unwrap() - 0.4).abs() < 0.01);
        assert!((config.constraints.max_coupling_score.unwrap() - 0.35).abs() < 0.01);
        assert_eq!(config.layers.len(), 3);
        assert_eq!(config.layers[0].name, "infrastructure");
        assert_eq!(config.boundaries.len(), 1);
        assert_eq!(config.boundaries[0].reason, "Renderer must not know about scanning");
    }

    // ── Glob matching ──

    #[test]
    fn glob_star_matches_direct_children() {
        assert!(glob_match("src/ui/*", "src/ui/panel.rs"));
        assert!(!glob_match("src/ui/*", "src/ui/sub/deep.rs"));
        assert!(!glob_match("src/ui/*", "src/app.rs"));
    }

    #[test]
    fn glob_doublestar_matches_any_depth() {
        assert!(glob_match("src/ui/**", "src/ui/panel.rs"));
        assert!(glob_match("src/ui/**", "src/ui/sub/deep.rs"));
        assert!(!glob_match("src/ui/**", "src/app.rs"));
    }

    #[test]
    fn glob_exact_match() {
        assert!(glob_match("src/app.rs", "src/app.rs"));
        assert!(!glob_match("src/app.rs", "src/main.rs"));
    }

    #[test]
    fn glob_prefix_match() {
        assert!(glob_match("src/ui", "src/ui/panel.rs"));
        assert!(!glob_match("src/ui", "src/utils/helper.rs"));
    }

    #[test]
    fn glob_extension_match() {
        assert!(glob_match("*.rs", "src/app.rs"));
        assert!(glob_match("*.rs", "deep/nested/file.rs"));
        assert!(!glob_match("*.rs", "src/app.ts"));
    }

    #[test]
    fn glob_has_no_middle_wildcard_support() {
        // `*`/`**` only work as a trailing segment. Patterns are matched verbatim
        // against scan-root-relative paths, so crate-prefixed workspace paths
        // ("sentrux-core/src/...") need the prefix spelled out in the pattern.
        assert!(!glob_match("*/src/renderer/*", "sentrux-core/src/renderer/x.rs"));
        assert!(!glob_match("src/renderer/*", "sentrux-core/src/renderer/x.rs"));
        assert!(glob_match("sentrux-core/src/renderer/*", "sentrux-core/src/renderer/x.rs"));
    }

    // ── Constraint checks ──

    #[test]
    fn constraint_max_cycles_catches_violations() {
        let config: RulesConfig = toml::from_str(r#"
[constraints]
max_cycles = 0
"#).unwrap();

        let edges = vec![edge("a.rs", "b.rs"), edge("b.rs", "a.rs")];
        let snap = make_snapshot(edges.clone(), vec![file("a.rs"), file("b.rs")]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        assert!(!result.passed, "should fail: cycles exist but max_cycles=0");
        assert!(result.violations.iter().any(|v| v.rule == "max_cycles"));
    }

    #[test]
    fn constraint_passes_when_met() {
        let config: RulesConfig = toml::from_str(r#"
[constraints]
max_cycles = 5
"#).unwrap();

        let edges = vec![edge("a.rs", "b.rs")];
        let snap = make_snapshot(edges.clone(), vec![file("a.rs"), file("b.rs")]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        assert!(result.passed, "should pass: 0 cycles <= 5 max");
    }

    // ── Layer checks ──

    #[test]
    fn layer_violation_detected() {
        let config: RulesConfig = toml::from_str(r#"
[[layers]]
name = "infrastructure"
paths = ["src/scanner.rs"]
order = 0

[[layers]]
name = "presentation"
paths = ["src/ui/*"]
order = 2
"#).unwrap();

        // Infrastructure (foundational, order=0) imports presentation (higher layer, order=2) = violation
        let edges = vec![edge("src/scanner.rs", "src/ui/panel.rs")];
        let snap = make_snapshot(edges.clone(), vec![
            file("src/scanner.rs"),
            file("src/ui/panel.rs"),
        ]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        assert!(!result.passed);
        assert!(result.violations.iter().any(|v| v.rule == "layer_direction"));
    }

    #[test]
    fn layer_correct_direction_passes() {
        let config: RulesConfig = toml::from_str(r#"
[[layers]]
name = "infrastructure"
paths = ["src/scanner.rs"]
order = 0

[[layers]]
name = "presentation"
paths = ["src/ui/*"]
order = 2
"#).unwrap();

        // Presentation (higher layer, order=2) imports infrastructure (foundational, order=0) = correct direction
        let edges = vec![edge("src/ui/panel.rs", "src/scanner.rs")];
        let snap = make_snapshot(edges.clone(), vec![
            file("src/ui/panel.rs"),
            file("src/scanner.rs"),
        ]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        let layer_violations: Vec<_> = result.violations.iter()
            .filter(|v| v.rule == "layer_direction")
            .collect();
        assert!(layer_violations.is_empty(), "correct direction should not violate");
    }

    // ── Boundary checks ──

    #[test]
    fn boundary_violation_detected() {
        let config: RulesConfig = toml::from_str(r#"
[[boundaries]]
from = "src/renderer/*"
to = "src/scanner.rs"
reason = "Renderer must not know about scanning"
"#).unwrap();

        let edges = vec![edge("src/renderer/edges.rs", "src/scanner.rs")];
        let snap = make_snapshot(edges.clone(), vec![
            file("src/renderer/edges.rs"),
            file("src/scanner.rs"),
        ]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        assert!(!result.passed);
        assert!(result.violations.iter().any(|v|
            v.rule == "boundary" && v.message.contains("Renderer must not know")
        ));
    }

    #[test]
    fn boundary_non_matching_passes() {
        let config: RulesConfig = toml::from_str(r#"
[[boundaries]]
from = "src/renderer/*"
to = "src/scanner.rs"
"#).unwrap();

        // This edge doesn't match the boundary rule
        let edges = vec![edge("src/app.rs", "src/scanner.rs")];
        let snap = make_snapshot(edges.clone(), vec![
            file("src/app.rs"),
            file("src/scanner.rs"),
        ]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        let boundary_violations: Vec<_> = result.violations.iter()
            .filter(|v| v.rule == "boundary")
            .collect();
        assert!(boundary_violations.is_empty());
    }

    // ── Boundary scenarios (from the _dbg_fixture experiment) ──
    //
    // Mirrors `.sentrux/rules.toml`: 6 layers (lower order = more foundational,
    // dependencies must flow from higher to lower) plus the two deny boundaries.
    // These cases pin down how boundary checks differ from the layer order check.

    fn fixture_style_config() -> RulesConfig {
        toml::from_str(r#"
[[layers]]
name = "core"
paths = ["src/core/*"]
order = 0

[[layers]]
name = "analysis"
paths = ["src/analysis/*"]
order = 1

[[layers]]
name = "metrics"
paths = ["src/metrics/*"]
order = 2

[[layers]]
name = "layout"
paths = ["src/layout/*"]
order = 3

[[layers]]
name = "renderer"
paths = ["src/renderer/*"]
order = 4

[[layers]]
name = "app"
paths = ["src/app/*"]
order = 5

[[boundaries]]
from = "src/renderer/*"
to = "src/analysis/*"
reason = "Renderer must not depend on analysis directly"

[[boundaries]]
from = "src/layout/*"
to = "src/app/*"
reason = "Layout must not depend on app layer"
"#).unwrap()
    }

    #[test]
    fn boundary_fires_even_when_layer_direction_is_legal() {
        // renderer (order=4) imports analysis (order=1): higher -> lower is the LEGAL
        // layer direction, so no layer_direction violation. The deny boundary is
        // order-agnostic and still fires — this is the fixture group 1 scenario.
        let config = fixture_style_config();
        let edges = vec![edge("src/renderer/edges.rs", "src/analysis/graph.rs")];
        let snap = make_snapshot(edges.clone(), vec![
            file("src/renderer/edges.rs"),
            file("src/analysis/graph.rs"),
        ]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        assert!(!result.passed);
        assert!(result.violations.iter().any(|v|
            v.rule == "boundary" && v.message.contains("Renderer must not depend on analysis")
        ));
        assert!(result.violations.iter().all(|v| v.rule != "layer_direction"),
            "higher -> lower is the legal layer direction, must not fire");
    }

    #[test]
    fn boundary_and_layer_check_fire_together_for_wrong_direction() {
        // layout (order=3) imports app (order=5): lower -> higher is the forbidden
        // layer direction AND matches the layout -> app deny boundary, so BOTH rules
        // fire — this is the fixture group 2 scenario.
        let config = fixture_style_config();
        let edges = vec![edge("src/layout/types.rs", "src/app/state.rs")];
        let snap = make_snapshot(edges.clone(), vec![
            file("src/layout/types.rs"),
            file("src/app/state.rs"),
        ]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        assert!(!result.passed);
        assert!(result.violations.iter().any(|v| v.rule == "layer_direction"));
        assert!(result.violations.iter().any(|v|
            v.rule == "boundary" && v.message.contains("Layout must not depend on app")
        ));
    }

    #[test]
    fn boundary_from_match_only_is_allowed() {
        // (m_from=true, m_to=false): renderer imports core. The importer is in the
        // restricted group but the target is not the forbidden group; the layer
        // direction is also legal (4 -> 0). Everything passes.
        let config = fixture_style_config();
        let edges = vec![edge("src/renderer/colors.rs", "src/core/types.rs")];
        let snap = make_snapshot(edges.clone(), vec![
            file("src/renderer/colors.rs"),
            file("src/core/types.rs"),
        ]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        assert!(result.passed);
        assert!(result.violations.is_empty());
    }

    #[test]
    fn boundary_is_directional_reverse_edge_does_not_fire() {
        // analysis (order=1) imports renderer (order=4): the reverse of the deny
        // rule. Boundaries only forbid the from -> to direction, so neither boundary
        // fires; the wrong-direction edge is caught by the layer check instead.
        let config = fixture_style_config();
        let edges = vec![edge("src/analysis/graph.rs", "src/renderer/edges.rs")];
        let snap = make_snapshot(edges.clone(), vec![
            file("src/analysis/graph.rs"),
            file("src/renderer/edges.rs"),
        ]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        let boundary_violations: Vec<_> = result.violations.iter()
            .filter(|v| v.rule == "boundary")
            .collect();
        assert!(boundary_violations.is_empty());
        assert!(result.violations.iter().any(|v| v.rule == "layer_direction"),
            "lower -> higher layer import must be caught by the layer check");
    }

    #[test]
    fn boundary_patterns_match_scan_root_relative_paths_only() {
        // Regression for the silent-pass root cause: when scanning a workspace root,
        // edge paths carry the crate prefix ("sentrux-core/src/..."). glob_match has
        // no middle-wildcard support, so "src/renderer/*" patterns match nothing and
        // the boundary check silently passes. Patterns must spell out the prefix.
        let config = fixture_style_config();
        let edges = vec![edge(
            "sentrux-core/src/renderer/colors.rs",
            "sentrux-core/src/analysis/lang_registry.rs",
        )];
        let snap = make_snapshot(edges.clone(), vec![
            file("sentrux-core/src/renderer/colors.rs"),
            file("sentrux-core/src/analysis/lang_registry.rs"),
        ]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        assert!(result.passed, "crate-prefixed paths never match the src/* patterns");
        assert!(result.violations.is_empty());
    }

    #[test]
    fn boundary_reports_one_violation_per_matching_edge() {
        // Unlike layer_direction (deduplicated by message), check_boundary pushes
        // one violation per matching edge — two importing files produce two reports.
        let config = fixture_style_config();
        let edges = vec![
            edge("src/renderer/edges.rs", "src/analysis/graph.rs"),
            edge("src/renderer/colors.rs", "src/analysis/lang_registry.rs"),
        ];
        let snap = make_snapshot(edges.clone(), vec![
            file("src/renderer/edges.rs"),
            file("src/renderer/colors.rs"),
            file("src/analysis/graph.rs"),
            file("src/analysis/lang_registry.rs"),
        ]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        assert!(!result.passed);
        let boundary_violations: Vec<_> = result.violations.iter()
            .filter(|v| v.rule == "boundary")
            .collect();
        assert_eq!(boundary_violations.len(), 2);
    }

    // ── Empty rules pass everything ──

    #[test]
    fn empty_rules_always_pass() {
        let config: RulesConfig = toml::from_str("[constraints]").unwrap();
        let edges = vec![edge("a.rs", "b.rs"), edge("b.rs", "a.rs")];
        let snap = make_snapshot(edges.clone(), vec![file("a.rs"), file("b.rs")]);
        let health = metrics::compute_health(&snap);
        let arch_report = arch::compute_arch(&snap);

        let result = check_rules(&config, &health, &arch_report, &edges);
        assert!(result.passed, "no rules = no violations");
        assert_eq!(result.rules_checked, 0);
    }
}
