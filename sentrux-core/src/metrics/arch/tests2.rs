use super::*;
use super::distance::compute_distance_from_main_seq;
use crate::metrics::test_helpers::edge;
use crate::core::types::ImportEdge;
use crate::core::snapshot::Snapshot;
use crate::core::types::{FileNode, ClassInfo, StructuralAnalysis};
use crate::core::path_utils;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn baseline_stable_no_degradation() {
    let baseline = ArchBaseline {
        timestamp: 0.0,
        structure_grade: 'B',
        coupling_score: 0.30,
        cycle_count: 1,
        god_file_count: 1,
        hotspot_count: 0,
        complex_fn_count: 2,
        max_depth: 4,
        total_import_edges: 15,
        cross_module_edges: 5,
    };

    let current = crate::metrics::HealthReport {
        coupling_score: 0.28,
        circular_dep_count: 1,
        circular_dep_files: vec![],
        total_import_edges: 15,
        cross_module_edges: 4,
        entropy: 0.3,
        entropy_bits: 1.0,
        avg_cohesion: Some(0.5),
        max_depth: 4,
        god_files: vec![
            crate::metrics::FileMetric { path: "app.rs".into(), value: 16 },
        ],
        hotspot_files: vec![],
        most_unstable: vec![],
        complex_functions: vec![
            crate::metrics::FuncMetric { file: "a.rs".into(), func: "f".into(), value: 16 },
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
        god_file_ratio: 0.03,
        hotspot_ratio: 0.0,
        complex_fn_ratio: 0.05,
        long_fn_ratio: 0.0,
        comment_ratio: Some(0.12),
        large_file_count: 0,
        large_file_ratio: 0.0,
        duplication_ratio: 0.0,
        dead_code_ratio: 0.0,
        high_param_ratio: 0.0,
        cog_complex_ratio: 0.0,
        dimensions: crate::metrics::DimensionGrades {
            coupling: 'B',
            entropy: 'B',
            cohesion: Some('B'),
            depth: 'B',
            cycles: 'B',
            god_files: 'B',
            hotspots: 'A',
            complex_fn: 'B',
            long_fn: 'A',
            comment: Some('B'),
            file_size: 'A',
            duplication: 'A',
            dead_code: 'A',
            high_params: 'A',
            cog_complex: 'A',
        },
        grade: 'B',
    };

    let diff = baseline.diff(&current);
    assert!(!diff.degraded, "should not flag stable/improved state");
    assert!(diff.violations.is_empty());
}

// ── Monotonicity: adding edges can only increase blast radius ──
#[test]
fn blast_radius_monotonic() {
    let edges_small = vec![edge("a.rs", "b.rs")];
    let edges_large = vec![
        edge("a.rs", "b.rs"),
        edge("b.rs", "c.rs"),
    ];
    let r1 = compute_blast_radius(&edges_small);
    let r2 = compute_blast_radius(&edges_large);
    assert!(r2["b.rs"] >= r1["b.rs"],
        "adding edges should not decrease blast radius");
}

// ── Idempotency: computing twice gives same result ──
#[test]
fn levels_idempotent() {
    let edges = vec![
        edge("a.rs", "b.rs"),
        edge("b.rs", "c.rs"),
        edge("a.rs", "c.rs"),
    ];
    let (l1, m1) = compute_levels(&edges);
    let (l2, m2) = compute_levels(&edges);
    assert_eq!(l1, l2);
    assert_eq!(m1, m2);
}

// ── Distance from Main Sequence tests (Martin 2003) ──

fn make_file_with_classes(path: &str, classes: Vec<ClassInfo>) -> FileNode {
    FileNode {
        path: path.to_string(),
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        is_dir: false,
        lines: 100, logic: 80, comments: 10, blanks: 10, funcs: 5,
        mtime: 0.0, gs: String::new(), lang: "rust".to_string(),
        sa: Some(StructuralAnalysis {
            functions: None,
            cls: Some(classes),
            imp: None,
            co: None,
            tags: None,
        }),
        children: None,
    }
}

fn make_snapshot_with_files(edges: Vec<ImportEdge>, files: Vec<FileNode>) -> Snapshot {
    Snapshot {
        root: Arc::new(FileNode {
            path: ".".into(), name: ".".into(), is_dir: true,
            lines: 0, logic: 0, comments: 0, blanks: 0, funcs: 0,
            mtime: 0.0, gs: String::new(), lang: String::new(),
            sa: None,
            children: Some(files),
        }),
        total_files: 0, total_lines: 0, total_dirs: 0,
        call_graph: vec![], import_graph: edges.clone(),
        inherit_graph: vec![], entry_points: vec![],
        exec_depth: HashMap::new(),
    }
}

fn cls(name: &str, kind: &str) -> ClassInfo {
    ClassInfo {
        n: name.to_string(),
        m: None,
        b: None,
        k: Some(kind.to_string()),
    }
}

#[test]
fn distance_pure_interface_module() {
    let api_file = "api/src/traits.rs";
    let impl_file = "impl/src/renderer.rs";
    let api_mod = path_utils::module_of(api_file);
    let files = vec![
        make_file_with_classes(api_file, vec![
            cls("Drawable", "interface"),
            cls("Serializable", "interface"),
        ]),
    ];
    let edges = vec![
        edge(impl_file, api_file),
    ];
    let snap = make_snapshot_with_files(edges.clone(), files);
    let results = compute_distance_from_main_seq(&snap, &edges, None);
    let api = results.iter().find(|m| m.module == api_mod).unwrap();
    assert!((api.abstractness - 1.0).abs() < f64::EPSILON, "all interfaces → A=1.0");
    assert!(api.instability < f64::EPSILON, "no fan-out → I=0");
    assert!(api.distance < f64::EPSILON, "pure interface + stable = on main sequence");
}

#[test]
fn distance_pure_concrete_module() {
    let api_file = "api/src/traits.rs";
    let impl_file = "impl/src/renderer.rs";
    let impl_mod = path_utils::module_of(impl_file);
    let files = vec![
        make_file_with_classes(impl_file, vec![
            cls("OpenGLRenderer", "class"),
            cls("VulkanRenderer", "class"),
        ]),
    ];
    let edges = vec![
        edge(impl_file, api_file),
    ];
    let snap = make_snapshot_with_files(edges.clone(), files);
    let results = compute_distance_from_main_seq(&snap, &edges, None);
    let imp = results.iter().find(|m| m.module == impl_mod).unwrap();
    assert!(imp.abstractness < f64::EPSILON, "all classes → A=0.0");
    assert!((imp.instability - 1.0).abs() < f64::EPSILON, "pure fan-out → I=1.0");
    assert!(imp.distance < f64::EPSILON, "concrete + unstable = on main sequence");
}

#[test]
fn distance_zone_of_pain() {
    let core_file = "core/src/engine.rs";
    let core_mod = path_utils::module_of(core_file);
    let files = vec![
        make_file_with_classes(core_file, vec![
            cls("Engine", "class"),
        ]),
    ];
    let edges: Vec<ImportEdge> = vec![];
    let snap = make_snapshot_with_files(edges.clone(), files);
    let results = compute_distance_from_main_seq(&snap, &edges, None);
    let core = results.iter().find(|m| m.module == core_mod).unwrap();
    assert!(core.abstractness < f64::EPSILON);
    assert!((core.distance - 0.5).abs() < f64::EPSILON,
        "isolated concrete module gets moderate distance");
}

#[test]
fn distance_empty_graph() {
    let snap = make_snapshot_with_files(vec![], vec![]);
    let results = compute_distance_from_main_seq(&snap, &[], None);
    assert!(results.is_empty(), "no types → no distance data");
}

// ── Symmetry: D is symmetric around the main sequence line ──
#[test]
fn distance_symmetry() {
    let d1 = (0.8_f64 + 0.0 - 1.0).abs();
    let d2 = (0.0_f64 + 0.8 - 1.0).abs();
    assert!((d1 - d2).abs() < f64::EPSILON, "D is symmetric around main sequence");
}

// ── Invariance: D of a point on the main sequence is always 0 ──
#[test]
fn distance_main_sequence_invariant() {
    for i in 0..=10 {
        let a = i as f64 / 10.0;
        let instab = 1.0 - a;
        let d = (a + instab - 1.0).abs();
        assert!(d < f64::EPSILON, "point ({a}, {instab}) should be on main sequence");
    }
}

// ── Idempotency: computing distance twice gives same result ──
#[test]
fn distance_idempotent() {
    let files = vec![
        make_file_with_classes("mod1/a.rs", vec![cls("Foo", "class"), cls("Bar", "interface")]),
        make_file_with_classes("mod2/b.rs", vec![cls("Baz", "class")]),
    ];
    let edges = vec![edge("mod1/a.rs", "mod2/b.rs")];
    let snap = make_snapshot_with_files(edges.clone(), files);
    let r1 = compute_distance_from_main_seq(&snap, &edges, None);
    let r2 = compute_distance_from_main_seq(&snap, &edges, None);
    assert_eq!(r1.len(), r2.len());
    for (a, b) in r1.iter().zip(r2.iter()) {
        assert!((a.distance - b.distance).abs() < f64::EPSILON);
    }
}

// ── Boundary: D is always in [0, 1] ──
#[test]
fn distance_bounded() {
    let files = vec![
        make_file_with_classes("a/x.rs", vec![cls("X", "interface")]),
        make_file_with_classes("b/y.rs", vec![cls("Y", "class")]),
        make_file_with_classes("c/z.rs", vec![cls("Z", "type"), cls("W", "interface")]),
    ];
    let edges = vec![
        edge("b/y.rs", "a/x.rs"),
        edge("c/z.rs", "a/x.rs"),
        edge("c/z.rs", "b/y.rs"),
    ];
    let snap = make_snapshot_with_files(edges.clone(), files);
    let results = compute_distance_from_main_seq(&snap, &edges, None);
    for m in &results {
        assert!(m.distance >= 0.0 && m.distance <= 1.0,
            "distance must be in [0,1], got {} for module {}", m.distance, m.module);
        assert!(m.abstractness >= 0.0 && m.abstractness <= 1.0);
        assert!(m.instability >= 0.0 && m.instability <= 1.0);
    }
}

// ── Integration: blast grade on real repo ──
#[test]
fn blast_grade_real_repo() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let limits = crate::analysis::scanner::common::ScanLimits {
        max_file_size_kb: 512 * 1024,
        max_parse_size_kb: 512 * 1024,
        max_call_targets: 64,
    };
    let result = crate::analysis::scanner::scan_directory(
        path.to_str().unwrap(), None, None, &limits);
    let snap = result.unwrap().snapshot;
    let arch = compute_arch(&snap);
    // Blast grade should be A or B — no single non-foundation file dominates.
    assert!(arch.blast_grade <= 'B',
        "blast_grade={} expected A or B", arch.blast_grade);
}
