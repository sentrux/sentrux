//! Test gap analysis — find high-risk files with zero test coverage.
//!
//! Cross-references the import graph with test file detection to find:
//! - Source files that no test file imports (untested)
//! - High-complexity untested files (highest risk)
//! - Test-to-source coverage ratio
//!
//! Test detection is path-based, matching common conventions across languages.

use crate::core::types::ImportEdge;
use crate::core::snapshot::Snapshot;
use std::collections::{HashMap, HashSet};

// ── Named constants ──

/// Grade thresholds for test coverage ratio (tested_files / total_source_files).
const COVERAGE_THRESHOLD_A: f64 = 0.80;
const COVERAGE_THRESHOLD_B: f64 = 0.60;
const COVERAGE_THRESHOLD_C: f64 = 0.40;
const COVERAGE_THRESHOLD_D: f64 = 0.20;

// ── Trait: TestGapAnalyzer ──

/// Interface for computing test gap analysis from a snapshot.
///
/// Abstracts the test-gap computation so that:
/// - Tests can inject pre-classified file lists without a real snapshot
/// - Alternative test-detection heuristics can be swapped in
/// - Cached analysis results can be returned directly
pub trait TestGapAnalyzer {
    /// Compute test gap analysis from a snapshot and complexity map.
    fn analyze(
        &self,
        snapshot: &Snapshot,
        complexity_map: &HashMap<String, u32>,
    ) -> TestGapReport;

    /// Check if a file path matches test file naming conventions.
    fn is_test(&self, path: &str) -> bool;
}

/// Default implementation using path-based test detection and BFS coverage.
pub struct DefaultTestGapAnalyzer;

impl TestGapAnalyzer for DefaultTestGapAnalyzer {
    fn analyze(
        &self,
        snapshot: &Snapshot,
        complexity_map: &HashMap<String, u32>,
    ) -> TestGapReport {
        compute_test_gaps(snapshot, complexity_map)
    }

    fn is_test(&self, path: &str) -> bool {
        is_test_file(path)
    }
}

// ── Public types ──

/// Complete test gap report.
#[derive(Debug, Clone)]
pub struct TestGapReport {
    /// Total source files (non-test).
    pub source_files: usize,

    /// Total test files detected.
    pub test_files: usize,

    /// Source files that at least one test imports.
    pub tested_source_files: usize,

    /// Source files with zero test imports.
    pub untested_source_files: usize,

    /// Coverage ratio: tested / total source.
    pub coverage_ratio: f64,

    /// Coverage grade (A-F).
    pub coverage_grade: char,

    /// Untested files ranked by risk (complexity × fan-in).
    pub gaps: Vec<TestGap>,

    /// Test files and what they cover.
    pub test_coverage: Vec<TestCoverage>,
}

/// A source file with no test coverage, ranked by risk.
#[derive(Debug, Clone)]
pub struct TestGap {
    /// Path of the untested source file
    pub file: String,
    /// Maximum cyclomatic complexity of any function in this file
    pub max_complexity: u32,
    /// Number of files that import this file
    pub fan_in: u32,
    /// risk = complexity × fan_in (high = most dangerous to leave untested)
    pub risk_score: u64,
    /// Programming language of the untested file
    pub lang: String,
}

/// A test file and the source files it imports.
#[derive(Debug, Clone)]
pub struct TestCoverage {
    /// Path of the test file
    pub test_file: String,
    /// Source files directly imported by this test file
    pub covers: Vec<String>,
}

// ── Public API ──

/// Compute test gap analysis from a snapshot.
///
/// `complexity_map`: file → max cyclomatic complexity (from HealthReport or snapshot).
pub fn compute_test_gaps(
    snapshot: &Snapshot,
    complexity_map: &HashMap<String, u32>,
) -> TestGapReport {
    let all_files = crate::core::snapshot::flatten_files_ref(&snapshot.root);

    // Classify files as test or source
    let (test_files, source_files, file_langs) = classify_files(&all_files);

    // Build fan-in map (how many files import each file)
    let fan_in = compute_fan_in(&snapshot.import_graph);

    // Find which source files are covered by tests
    let tested = find_tested_files(&snapshot.import_graph, &test_files, &source_files);

    let tested_count = tested.len();
    let untested: HashSet<&str> = source_files
        .iter()
        .filter(|f| !tested.contains(f.as_str()))
        .map(|f| f.as_str())
        .collect();

    // Build test coverage map
    let test_coverage = build_test_coverage(&snapshot.import_graph, &test_files, &source_files);

    // Build gap list ranked by risk
    let gaps = build_risk_gaps(&untested, complexity_map, &fan_in, &file_langs);

    let source_count = source_files.len();
    let coverage_ratio = if source_count > 0 {
        tested_count as f64 / source_count as f64
    } else {
        1.0 // no source files = fully covered (vacuously)
    };

    TestGapReport {
        source_files: source_count,
        test_files: test_files.len(),
        tested_source_files: tested_count,
        untested_source_files: untested.len(),
        coverage_ratio,
        coverage_grade: grade_coverage(coverage_ratio),
        gaps,
        test_coverage,
    }
}

/// Classify files into test files, source files, and collect their languages.
fn classify_files(
    all_files: &[&crate::core::types::FileNode],
) -> (HashSet<String>, HashSet<String>, HashMap<String, String>) {
    let mut test_files: HashSet<String> = HashSet::new();
    let mut source_files: HashSet<String> = HashSet::new();
    let mut file_langs: HashMap<String, String> = HashMap::new();

    for f in all_files {
        if f.is_dir {
            continue;
        }
        file_langs.insert(f.path.clone(), f.lang.clone());
        if is_test_file(&f.path) {
            test_files.insert(f.path.clone());
        } else {
            source_files.insert(f.path.clone());
        }
    }
    (test_files, source_files, file_langs)
}

/// Build the gap list ranked by risk (complexity x fan-in), truncated to top 50.
fn build_risk_gaps(
    untested: &HashSet<&str>,
    complexity_map: &HashMap<String, u32>,
    fan_in: &HashMap<&str, u32>,
    file_langs: &HashMap<String, String>,
) -> Vec<TestGap> {
    let mut gaps: Vec<TestGap> = untested
        .iter()
        .map(|&path| {
            let cc = complexity_map.get(path).copied().unwrap_or(1);
            let fi = fan_in.get(path).copied().unwrap_or(0);
            let risk = cc as u64 * (fi as u64 + 1);
            TestGap {
                file: path.to_string(),
                max_complexity: cc,
                fan_in: fi,
                risk_score: risk,
                lang: file_langs.get(path).cloned().unwrap_or_default(),
            }
        })
        .collect();

    gaps.sort_by(|a, b| b.risk_score.cmp(&a.risk_score));
    gaps.truncate(50);
    gaps
}

// ── Test file detection ──

/// Detect if a file path is a test file based on naming conventions.
/// Covers Rust, Python, JavaScript/TypeScript, Go, Java, C#, Ruby, PHP, etc.
pub fn is_test_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    if is_test_directory(&lower) {
        return true;
    }
    is_test_filename(path, &lower)
}

/// Known test directory prefixes (path starts with these).
const TEST_DIR_PREFIXES: &[&str] = &[
    "test/", "tests/", "__tests__/", "spec/", "specs/",
    "fixtures/", "testdata/",
];

/// Known test directory infixes (path contains these).
const TEST_DIR_INFIXES: &[&str] = &[
    "/test/", "/tests/", "/__tests__/", "/spec/", "/specs/",
    "/fixtures/", "/testdata/",
];

/// Check if the file lives in a known test/spec/fixture directory.
fn is_test_directory(lower: &str) -> bool {
    TEST_DIR_PREFIXES.iter().any(|p| lower.starts_with(p))
        || TEST_DIR_INFIXES.iter().any(|p| lower.contains(p))
}

/// File suffixes that indicate test files (checked against lowered filename).
const TEST_SUFFIXES: &[&str] = &[
    "_test.rs", "_test.go", "_test.py", "_test.exs",
    ".test.js", ".test.ts", ".test.tsx", ".test.jsx",
    ".spec.js", ".spec.ts", ".spec.tsx", ".spec.jsx",
    "_spec.rb",
];

/// Stem suffixes that indicate test files (checked against name without extension).
const TEST_STEM_SUFFIXES: &[&str] = &[
    "_test", "_tests", "_spec", ".test", ".spec",
];

/// Check if a lowered filename matches test naming conventions by suffix.
fn has_test_suffix(name: &str) -> bool {
    name.starts_with("test_") || TEST_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// Check if the stem (name without extension) matches test naming.
fn has_test_stem(name_no_ext: &str) -> bool {
    matches!(name_no_ext, "test" | "tests" | "spec")
        || TEST_STEM_SUFFIXES.iter().any(|s| name_no_ext.ends_with(s))
}

/// Check if the original (non-lowered) stem matches PascalCase test patterns.
fn has_pascal_test_suffix(orig_no_ext: &str) -> bool {
    orig_no_ext.ends_with("Test") || orig_no_ext.ends_with("Tests") || orig_no_ext.ends_with("Spec")
}

/// Check if the filename matches test naming conventions across languages.
fn is_test_filename(path: &str, lower: &str) -> bool {
    let name = lower.rsplit('/').next().unwrap_or(lower);
    // Strip only the last extension, not all dots
    let name_no_ext = match name.rfind('.') {
        Some(dot) => &name[..dot],
        None => name,
    };
    let orig_name = path.rsplit('/').next().unwrap_or(path);
    let orig_no_ext = match orig_name.rfind('.') {
        Some(dot) => &orig_name[..dot],
        None => orig_name,
    };

    has_test_suffix(name) || has_test_stem(name_no_ext) || has_pascal_test_suffix(orig_no_ext)
}

// ── Internal helpers ──

/// Compute fan-in for each file (number of files that import it).
fn compute_fan_in(edges: &[ImportEdge]) -> HashMap<&str, u32> {
    let mut fan_in: HashMap<&str, u32> = HashMap::new();
    for edge in edges {
        *fan_in.entry(edge.to_file.as_str()).or_default() += 1;
    }
    fan_in
}

/// Build forward adjacency list from import edges.
fn build_forward_adj(edges: &[ImportEdge]) -> HashMap<&str, Vec<&str>> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        adj.entry(edge.from_file.as_str())
            .or_default()
            .push(edge.to_file.as_str());
    }
    adj
}

/// BFS from a single test file, collecting reachable source files into `tested`.
fn bfs_from_test_file<'a>(
    start: &'a str,
    adj: &HashMap<&'a str, Vec<&'a str>>,
    source_files: &HashSet<String>,
    tested: &mut HashSet<String>,
) {
    let mut visited: HashSet<&str> = HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    visited.insert(start);
    queue.push_back(start);

    while let Some(node) = queue.pop_front() {
        if source_files.contains(node) {
            tested.insert(node.to_string());
        }
        if let Some(targets) = adj.get(node) {
            for &target in targets {
                if visited.insert(target) {
                    queue.push_back(target);
                }
            }
        }
    }
}

/// Find all source files that are transitively imported by any test file.
fn find_tested_files(
    edges: &[ImportEdge],
    test_files: &HashSet<String>,
    source_files: &HashSet<String>,
) -> HashSet<String> {
    let adj = build_forward_adj(edges);
    let mut tested: HashSet<String> = HashSet::new();
    for test_file in test_files {
        bfs_from_test_file(test_file.as_str(), &adj, source_files, &mut tested);
    }
    tested
}

/// Build test coverage: for each test file, list the source files it directly imports.
fn build_test_coverage(
    edges: &[ImportEdge],
    test_files: &HashSet<String>,
    source_files: &HashSet<String>,
) -> Vec<TestCoverage> {
    let mut coverage: HashMap<&str, Vec<String>> = HashMap::new();

    for edge in edges {
        if test_files.contains(&edge.from_file) && source_files.contains(&edge.to_file) {
            coverage
                .entry(edge.from_file.as_str())
                .or_default()
                .push(edge.to_file.clone());
        }
    }

    let mut result: Vec<TestCoverage> = coverage
        .into_iter()
        .map(|(test_file, mut covers)| {
            covers.sort_unstable();
            covers.dedup();
            TestCoverage {
                test_file: test_file.to_string(),
                covers,
            }
        })
        .collect();

    result.sort_by(|a, b| b.covers.len().cmp(&a.covers.len()));
    result
}

fn grade_coverage(ratio: f64) -> char {
    if ratio >= COVERAGE_THRESHOLD_A {
        'A'
    } else if ratio >= COVERAGE_THRESHOLD_B {
        'B'
    } else if ratio >= COVERAGE_THRESHOLD_C {
        'C'
    } else if ratio >= COVERAGE_THRESHOLD_D {
        'D'
    } else {
        'F'
    }
}

#[cfg(test)]
mod tests;
