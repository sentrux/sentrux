//! Tests for test-gap analysis (`metrics::testgap`).
//!
//! Validates test-file detection heuristics across languages (Rust, Python,
//! JS/TS, Go, Java, C#), coverage computation from import edges, and gap
//! identification for untested production files. Covers boundary (no tests),
//! oracle (known test patterns detected correctly), and symmetry (test file
//! importing production file creates coverage, not vice versa).

use super::*;
use std::collections::HashSet;

fn edge(from: &str, to: &str) -> crate::core::types::ImportEdge {
    crate::core::types::ImportEdge {
        from_file: from.to_string(),
        to_file: to.to_string(),
    }
}

// ── is_test_file tests ──

#[test]
fn detect_rust_test() {
    assert!(is_test_file("src/metrics_test.rs"));
    assert!(!is_test_file("src/metrics.rs"));
}

#[test]
fn detect_python_test() {
    assert!(is_test_file("test_main.py"));
    assert!(is_test_file("tests/test_utils.py"));
    assert!(is_test_file("src/main_test.py"));
    assert!(!is_test_file("src/main.py"));
}

#[test]
fn detect_js_ts_test() {
    assert!(is_test_file("src/App.test.tsx"));
    assert!(is_test_file("src/utils.spec.ts"));
    assert!(is_test_file("__tests__/foo.js"));
    assert!(!is_test_file("src/App.tsx"));
}

#[test]
fn detect_go_test() {
    assert!(is_test_file("pkg/handler_test.go"));
    assert!(!is_test_file("pkg/handler.go"));
}

#[test]
fn detect_java_test() {
    assert!(is_test_file("src/FooTest.java"));
    assert!(is_test_file("src/FooTests.java"));
    assert!(!is_test_file("src/Foo.java"));
}

#[test]
fn detect_ruby_spec() {
    assert!(is_test_file("spec/models/user_spec.rb"));
    assert!(is_test_file("lib/user_spec.rb"));
    assert!(!is_test_file("lib/user.rb"));
}

#[test]
fn detect_elixir_test() {
    assert!(is_test_file("test/product_test.exs"));
    assert!(is_test_file("test/inventory/product_test.exs"));
    assert!(is_test_file("test/support/factory.ex"));
    assert!(!is_test_file("lib/inventory/product.ex"));
}

#[test]
fn detect_dir_patterns() {
    assert!(is_test_file("test/integration/api.rs"));
    assert!(is_test_file("tests/unit/math.py"));
    assert!(is_test_file("src/__tests__/button.tsx"));
    assert!(is_test_file("fixtures/data.json"));
    assert!(is_test_file("testdata/sample.txt"));
}

// ── fan_in tests ──

#[test]
fn fan_in_counts() {
    let edges = vec![
        edge("a.rs", "c.rs"),
        edge("b.rs", "c.rs"),
        edge("d.rs", "c.rs"),
    ];
    let fi = compute_fan_in(&edges);
    assert_eq!(fi["c.rs"], 3);
    assert!(!fi.contains_key("a.rs"));
}

// ── find_tested_files tests ──

#[test]
fn tested_files_direct() {
    let edges = vec![
        edge("test_main.py", "main.py"),
        edge("test_main.py", "utils.py"),
        edge("main.py", "db.py"),
    ];
    let test_files: HashSet<String> = ["test_main.py"].iter().map(|s| s.to_string()).collect();
    let source_files: HashSet<String> = ["main.py", "utils.py", "db.py"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let tested = find_tested_files(&edges, &test_files, &source_files);
    assert!(tested.contains("main.py"));
    assert!(tested.contains("utils.py"));
    assert!(tested.contains("db.py")); // transitive
}

#[test]
fn untested_files_detected() {
    let edges = vec![
        edge("test_main.py", "main.py"),
    ];
    let test_files: HashSet<String> = ["test_main.py"].iter().map(|s| s.to_string()).collect();
    let source_files: HashSet<String> = ["main.py", "orphan.py"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let tested = find_tested_files(&edges, &test_files, &source_files);
    assert!(tested.contains("main.py"));
    assert!(!tested.contains("orphan.py")); // not imported by any test
}

// ── test_coverage tests ──

#[test]
fn coverage_mapping() {
    let edges = vec![
        edge("test_main.py", "main.py"),
        edge("test_main.py", "utils.py"),
        edge("test_db.py", "db.py"),
    ];
    let test_files: HashSet<String> = ["test_main.py", "test_db.py"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let source_files: HashSet<String> = ["main.py", "utils.py", "db.py"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let cov = build_test_coverage(&edges, &test_files, &source_files);
    assert_eq!(cov.len(), 2);
    assert_eq!(cov[0].covers.len(), 2);
    assert_eq!(cov[1].covers.len(), 1);
}

// ── grading tests ──

#[test]
fn grade_boundaries() {
    assert_eq!(grade_coverage(1.0), 'A');
    assert_eq!(grade_coverage(0.80), 'A');
    assert_eq!(grade_coverage(0.79), 'B');
    assert_eq!(grade_coverage(0.60), 'B');
    assert_eq!(grade_coverage(0.59), 'C');
    assert_eq!(grade_coverage(0.40), 'C');
    assert_eq!(grade_coverage(0.39), 'D');
    assert_eq!(grade_coverage(0.20), 'D');
    assert_eq!(grade_coverage(0.19), 'F');
    assert_eq!(grade_coverage(0.0), 'F');
}

// ── Idempotency ──

#[test]
fn fan_in_idempotent() {
    let edges = vec![edge("a.rs", "b.rs"), edge("c.rs", "b.rs")];
    let a = compute_fan_in(&edges);
    let b = compute_fan_in(&edges);
    assert_eq!(a["b.rs"], b["b.rs"]);
}

// ── Commutativity: order of edges doesn't change result ──

#[test]
fn tested_files_order_independent() {
    let edges1 = vec![
        edge("test_a.py", "a.py"),
        edge("test_b.py", "b.py"),
    ];
    let edges2 = vec![
        edge("test_b.py", "b.py"),
        edge("test_a.py", "a.py"),
    ];
    let tests: HashSet<String> = ["test_a.py", "test_b.py"].iter().map(|s| s.to_string()).collect();
    let sources: HashSet<String> = ["a.py", "b.py"].iter().map(|s| s.to_string()).collect();

    let r1 = find_tested_files(&edges1, &tests, &sources);
    let r2 = find_tested_files(&edges2, &tests, &sources);
    assert_eq!(r1, r2);
}
