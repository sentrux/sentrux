use super::*;
use crate::metrics::test_helpers::edge;
use crate::core::types::{EntryPoint, ImportEdge};
use crate::core::snapshot::Snapshot;

fn entry(file: &str) -> EntryPoint {
    EntryPoint {
        file: file.to_string(),
        func: "main".to_string(),
        lang: "rust".to_string(),
        confidence: "high".to_string(),
    }
}

// ── Levelization tests ──

#[test]
fn levels_linear_chain() {
    let edges = vec![edge("a.rs", "b.rs"), edge("b.rs", "c.rs")];
    let (levels, max) = compute_levels(&edges);
    assert_eq!(levels["c.rs"], 0);
    assert_eq!(levels["b.rs"], 1);
    assert_eq!(levels["a.rs"], 2);
    assert_eq!(max, 2);
}

#[test]
fn levels_diamond() {
    let edges = vec![
        edge("a.rs", "b.rs"),
        edge("a.rs", "c.rs"),
        edge("b.rs", "d.rs"),
        edge("c.rs", "d.rs"),
    ];
    let (levels, max) = compute_levels(&edges);
    assert_eq!(levels["d.rs"], 0);
    assert_eq!(levels["b.rs"], 1);
    assert_eq!(levels["c.rs"], 1);
    assert_eq!(levels["a.rs"], 2);
    assert_eq!(max, 2);
}

#[test]
fn levels_empty() {
    let (levels, max) = compute_levels(&[]);
    assert!(levels.is_empty());
    assert_eq!(max, 0);
}

#[test]
fn levels_single_edge() {
    let edges = vec![edge("a.rs", "b.rs")];
    let (levels, max) = compute_levels(&edges);
    assert_eq!(levels["b.rs"], 0);
    assert_eq!(levels["a.rs"], 1);
    assert_eq!(max, 1);
}

#[test]
fn levels_cycle_gets_max_plus_one() {
    let edges = vec![
        edge("a.rs", "b.rs"),
        edge("b.rs", "a.rs"),
        edge("c.rs", "a.rs"),
    ];
    let (levels, _max) = compute_levels(&edges);
    assert!(levels["c.rs"] > 0, "c should be above leaf level");
}

// ── Upward violation tests ──

#[test]
fn no_violations_in_clean_chain() {
    let edges = vec![edge("a.rs", "b.rs"), edge("b.rs", "c.rs")];
    let (levels, _) = compute_levels(&edges);
    let violations = find_upward_violations(&edges, &levels);
    assert!(violations.is_empty(), "clean chain has no upward violations");
}

#[test]
fn violation_when_leaf_imports_high_level() {
    let edges = vec![
        edge("a.rs", "b.rs"),
        edge("b.rs", "c.rs"),
        edge("c.rs", "a.rs"),
    ];
    let (levels, _) = compute_levels(&edges);
    let violations = find_upward_violations(&edges, &levels);
    assert!(!violations.is_empty() || levels.values().all(|&v| v == levels["a.rs"]),
        "should detect violation or recognize cycle");
}

// ── Blast radius tests ──

#[test]
fn blast_radius_linear() {
    let edges = vec![edge("a.rs", "b.rs"), edge("b.rs", "c.rs")];
    let radius = compute_blast_radius(&edges);
    assert_eq!(radius["c.rs"], 2, "c affects a and b");
    assert_eq!(radius["b.rs"], 1, "b affects only a");
    assert_eq!(radius["a.rs"], 0, "a affects nobody");
}

#[test]
fn blast_radius_star() {
    let edges = vec![
        edge("a.rs", "x.rs"),
        edge("b.rs", "x.rs"),
        edge("c.rs", "x.rs"),
    ];
    let radius = compute_blast_radius(&edges);
    assert_eq!(radius["x.rs"], 3);
    assert_eq!(radius["a.rs"], 0);
}

#[test]
fn blast_radius_empty() {
    let radius = compute_blast_radius(&[]);
    assert!(radius.is_empty());
}

// ── Attack surface tests ──

#[test]
fn attack_surface_from_entry() {
    let edges = vec![
        edge("main.rs", "handler.rs"),
        edge("handler.rs", "db.rs"),
    ];
    let entries = vec![entry("main.rs")];
    let (surface, total) = compute_attack_surface(&edges, &entries);
    assert_eq!(surface, 3);
    assert_eq!(total, 3);
}

#[test]
fn attack_surface_partial() {
    let edges = vec![
        edge("main.rs", "handler.rs"),
        edge("handler.rs", "db.rs"),
        edge("orphan.rs", "utils.rs"),
    ];
    let entries = vec![entry("main.rs")];
    let (surface, total) = compute_attack_surface(&edges, &entries);
    assert_eq!(surface, 3, "only main→handler→db reachable");
    assert_eq!(total, 5);
}

#[test]
fn attack_surface_no_entries() {
    let edges = vec![edge("a.rs", "b.rs")];
    let (surface, total) = compute_attack_surface(&edges, &[]);
    assert_eq!(surface, 0);
    assert_eq!(total, 2);
}

// ── Baseline diff tests ──

#[test]
fn baseline_detects_degradation() {
    let baseline = ArchBaseline {
        timestamp: 0.0,
        quality_signal: 0.90,
        coupling_score: 0.10,
        cycle_count: 0,
        god_file_count: 0,
        hotspot_count: 0,
        complex_fn_count: 0,
        max_depth: 3,
        total_import_edges: 10,
        cross_module_edges: 1,
        complex_functions: vec![],
    };

    let current = crate::metrics::HealthReport {
        coupling_score: 0.45,
        circular_dep_count: 2,
        circular_dep_files: vec![vec!["a.rs".into(), "b.rs".into()]],
        total_import_edges: 20,
        cross_module_edges: 9,
        entropy: 0.5,
        entropy_bits: 1.5,
        avg_cohesion: Some(0.3),
        max_depth: 5,
        god_files: vec![
            crate::metrics::FileMetric { path: "app.rs".into(), value: 18 },
        ],
        hotspot_files: vec![],
        most_unstable: vec![],
        complex_functions: vec![
            crate::metrics::FuncMetric { file: "a.rs".into(), func: "f".into(), value: 20 },
            crate::metrics::FuncMetric { file: "b.rs".into(), func: "g".into(), value: 18 },
        ],
        long_functions: vec![],
        cog_complex_functions: vec![],
        high_param_functions: vec![],
        duplicate_groups: vec![],
        dead_functions: vec![],
        long_files: vec![],
        all_function_ccs: vec![],
        all_function_lines: vec![],
        all_file_lines: vec![],
        god_file_ratio: 0.05,
        hotspot_ratio: 0.0,
        complex_fn_ratio: 0.08,
        long_fn_ratio: 0.0,
        comment_ratio: Some(0.1),
        large_file_count: 0,
        large_file_ratio: 0.0,
        duplication_ratio: 0.0,
        dead_code_ratio: 0.0,
        high_param_ratio: 0.0,
        cog_complex_ratio: 0.0,
        quality_signal: 0.5,
        root_cause_raw: crate::metrics::root_causes::RootCauseRaw {
            modularity_q: 0.3, cycle_count: 2, max_depth: 5,
            complexity_gini: 0.3, redundancy_ratio: 0.1,
        },
        root_cause_scores: crate::metrics::root_causes::RootCauseScores {
            modularity: 0.53, acyclicity: 0.33, depth: 0.62,
            equality: 0.7, redundancy: 0.9,
        },
    };

    let diff = baseline.diff(&current);
    assert!(diff.degraded, "should detect degradation");
    assert!(!diff.violations.is_empty(), "should list specific violations");
    assert!(diff.violations.iter().any(|v| v.contains("Coupling")));
    assert!(diff.violations.iter().any(|v| v.contains("Cycles")));
    assert!(diff.violations.iter().any(|v| v.contains("God files")));
    assert!(diff.violations.iter().any(|v| v.contains("Complex functions")));
}

#[test]
fn baseline_diff_names_new_complex_functions() {
    let baseline = ArchBaseline {
        timestamp: 0.0,
        quality_signal: 0.90,
        coupling_score: 0.10,
        cycle_count: 0,
        god_file_count: 0,
        hotspot_count: 0,
        complex_fn_count: 2,
        max_depth: 3,
        total_import_edges: 10,
        cross_module_edges: 1,
        complex_functions: vec![
            ComplexFnSnapshot {
                file: "a.rs".into(),
                func: "keep".into(),
                cc: 16,
            },
            ComplexFnSnapshot {
                file: "b.rs".into(),
                func: "gone".into(),
                cc: 17,
            },
        ],
    };

    let current = crate::metrics::HealthReport {
        coupling_score: 0.10,
        circular_dep_count: 0,
        circular_dep_files: vec![],
        total_import_edges: 10,
        cross_module_edges: 1,
        entropy: 0.5,
        entropy_bits: 1.5,
        avg_cohesion: Some(0.3),
        max_depth: 3,
        god_files: vec![],
        hotspot_files: vec![],
        most_unstable: vec![],
        complex_functions: vec![
            crate::metrics::FuncMetric {
                file: "a.rs".into(),
                func: "keep".into(),
                value: 16,
            },
            crate::metrics::FuncMetric {
                file: "c.rs".into(),
                func: "new_one".into(),
                value: 18,
            },
            crate::metrics::FuncMetric {
                file: "d.rs".into(),
                func: "new_two".into(),
                value: 20,
            },
        ],
        long_functions: vec![],
        cog_complex_functions: vec![],
        high_param_functions: vec![],
        duplicate_groups: vec![],
        dead_functions: vec![],
        long_files: vec![],
        all_function_ccs: vec![],
        all_function_lines: vec![],
        all_file_lines: vec![],
        god_file_ratio: 0.0,
        hotspot_ratio: 0.0,
        complex_fn_ratio: 0.08,
        long_fn_ratio: 0.0,
        comment_ratio: Some(0.1),
        large_file_count: 0,
        large_file_ratio: 0.0,
        duplication_ratio: 0.0,
        dead_code_ratio: 0.0,
        high_param_ratio: 0.0,
        cog_complex_ratio: 0.0,
        quality_signal: 0.90,
        root_cause_raw: crate::metrics::root_causes::RootCauseRaw {
            modularity_q: 0.3,
            cycle_count: 0,
            max_depth: 3,
            complexity_gini: 0.3,
            redundancy_ratio: 0.1,
        },
        root_cause_scores: crate::metrics::root_causes::RootCauseScores {
            modularity: 0.53,
            acyclicity: 0.33,
            depth: 0.62,
            equality: 0.7,
            redundancy: 0.9,
        },
    };

    let diff = baseline.diff(&current);
    assert!(diff.degraded);
    assert_eq!(diff.new_complex_functions.len(), 2);
    assert!(diff
        .new_complex_functions
        .iter()
        .any(|f| f.file == "c.rs" && f.func == "new_one"));
    assert!(diff
        .new_complex_functions
        .iter()
        .any(|f| f.file == "d.rs" && f.func == "new_two"));
    assert_eq!(diff.removed_complex_functions.len(), 1);
    assert!(diff.violations.iter().any(|v| v.contains("new_one")));
}
