//! Individual rule-check helpers for the architecture rules engine.
//!
//! Extracted from rules.rs to keep each file under 500 lines.
//! Each function checks one constraint and returns violations (if any).

use crate::metrics::arch::ArchReport;
use crate::metrics::types::HealthReport;
use serde::Deserialize;

/// Structural constraints parsed from `.sentrux/rules.toml`.
/// Each field is optional — only set constraints are checked.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Constraints {
    /// Required minimum overall health grade (e.g., 'B')
    pub max_grade: Option<char>,
    /// Required maximum coupling grade
    pub max_coupling: Option<char>,
    /// Maximum allowed circular dependency cycles
    pub max_cycles: Option<usize>,
    /// Maximum cyclomatic complexity per function
    pub max_cc: Option<u32>,
    /// Maximum lines per file
    pub max_file_lines: Option<u32>,
    /// Maximum lines per function
    pub max_fn_lines: Option<u32>,
    /// Whether god files (fan-out > threshold) are forbidden
    #[serde(default)]
    pub no_god_files: bool,
    /// Maximum allowed upward dependency violations
    pub max_upward_violations: Option<usize>,
    /// Module boundary detection depth (2 = depth-2, 3 = depth-3, default: 3).
    /// Use 2 for monorepos where package boundaries are at depth-2.
    pub module_depth: Option<usize>,
}

impl Constraints {
    /// Count how many constraint thresholds are actively set.
    pub fn count_active(&self) -> usize {
        let mut n = 0;
        if self.max_grade.is_some() { n += 1; }
        if self.max_coupling.is_some() { n += 1; }
        if self.max_cycles.is_some() { n += 1; }
        if self.max_cc.is_some() { n += 1; }
        if self.max_file_lines.is_some() { n += 1; }
        if self.max_fn_lines.is_some() { n += 1; }
        if self.no_god_files { n += 1; }
        if self.max_upward_violations.is_some() { n += 1; }
        n
    }
}

/// Result of running all architecture rules against a codebase.
#[derive(Debug, Clone)]
pub struct RuleCheckResult {
    /// Whether all checked rules passed
    pub passed: bool,
    /// List of rule violations found
    pub violations: Vec<RuleViolation>,
    /// Number of rules that were checked
    pub rules_checked: usize,
}

/// A single rule violation with context about what failed.
#[derive(Debug, Clone)]
pub struct RuleViolation {
    /// Rule name (e.g., "max_cycles", "no_god_files")
    pub rule: String,
    /// Severity level (error or warning)
    pub severity: Severity,
    /// Human-readable violation description
    pub message: String,
    /// Files involved in the violation
    pub files: Vec<String>,
}

/// Severity level for rule violations.
#[derive(Debug, Clone, PartialEq)]
pub enum Severity {
    /// Hard failure — the gate should reject
    Error,
    /// Soft failure — informational, does not block the gate
    Warning,
}

/// Check overall structure grade.
pub fn check_max_grade(c: &Constraints, health: &HealthReport) -> Option<RuleViolation> {
    let max_grade = c.max_grade?;
    if health.grade > max_grade {
        Some(RuleViolation {
            rule: "max_grade".into(),
            severity: Severity::Error,
            message: format!("Structure grade {} exceeds maximum allowed {}", health.grade, max_grade),
            files: vec![],
        })
    } else {
        None
    }
}

/// Check coupling grade.
pub fn check_max_coupling(c: &Constraints, health: &HealthReport) -> Option<RuleViolation> {
    let max_coupling = c.max_coupling?;
    if health.dimensions.coupling > max_coupling {
        Some(RuleViolation {
            rule: "max_coupling".into(),
            severity: Severity::Error,
            message: format!("Coupling grade {} exceeds maximum allowed {}", health.dimensions.coupling, max_coupling),
            files: vec![],
        })
    } else {
        None
    }
}

/// Check circular dependency count.
pub fn check_max_cycles(c: &Constraints, health: &HealthReport) -> Option<RuleViolation> {
    let max_cycles = c.max_cycles?;
    if health.circular_dep_count > max_cycles {
        Some(RuleViolation {
            rule: "max_cycles".into(),
            severity: Severity::Error,
            message: format!("Found {} circular dependencies, maximum allowed is {}", health.circular_dep_count, max_cycles),
            files: health.circular_dep_files.iter().flatten().cloned().collect(),
        })
    } else {
        None
    }
}

/// Check maximum cyclomatic complexity per function.
/// Uses `all_function_ccs` (unfiltered) so user thresholds stricter than the
/// hardcoded CC_THRESHOLD_HIGH (15) are correctly enforced.
pub fn check_max_cc(c: &Constraints, health: &HealthReport) -> Option<RuleViolation> {
    let max_cc = c.max_cc?;
    let over: Vec<_> = health.all_function_ccs.iter().filter(|f| f.value > max_cc).collect();
    if !over.is_empty() {
        Some(RuleViolation {
            rule: "max_cc".into(),
            severity: Severity::Error,
            message: format!("{} function(s) exceed max cyclomatic complexity of {}", over.len(), max_cc),
            files: over.iter().map(|f| format!("{}:{} (cc={})", f.file, f.func, f.value)).collect(),
        })
    } else {
        None
    }
}

/// Check maximum file length in lines.
/// Uses `all_file_lines` (unfiltered) so user thresholds stricter than the
/// hardcoded LARGE_FILE_THRESHOLD (500) are correctly enforced.
pub fn check_max_file_lines(c: &Constraints, health: &HealthReport) -> Option<RuleViolation> {
    let max_file_lines = c.max_file_lines?;
    let over: Vec<_> = health.all_file_lines.iter().filter(|f| f.value > max_file_lines as usize).collect();
    if !over.is_empty() {
        Some(RuleViolation {
            rule: "max_file_lines".into(),
            severity: Severity::Error,
            message: format!("{} file(s) exceed max length of {} lines", over.len(), max_file_lines),
            files: over.iter().map(|f| format!("{} ({} lines)", f.path, f.value)).collect(),
        })
    } else {
        None
    }
}

/// Check maximum function length in lines.
/// Uses `all_function_lines` (unfiltered) so user thresholds stricter than the
/// hardcoded FUNC_LENGTH_THRESHOLD (50) are correctly enforced.
pub fn check_max_fn_lines(c: &Constraints, health: &HealthReport) -> Option<RuleViolation> {
    let max_fn_lines = c.max_fn_lines?;
    let over: Vec<_> = health.all_function_lines.iter().filter(|f| f.value > max_fn_lines).collect();
    if !over.is_empty() {
        Some(RuleViolation {
            rule: "max_fn_lines".into(),
            severity: Severity::Error,
            message: format!("{} function(s) exceed max length of {} lines", over.len(), max_fn_lines),
            files: over.iter().map(|f| format!("{}:{} ({} lines)", f.file, f.func, f.value)).collect(),
        })
    } else {
        None
    }
}

/// Check for god files (high fan-out).
pub fn check_no_god_files(c: &Constraints, health: &HealthReport) -> Option<RuleViolation> {
    if !c.no_god_files {
        return None;
    }
    if !health.god_files.is_empty() {
        Some(RuleViolation {
            rule: "no_god_files".into(),
            severity: Severity::Error,
            message: format!("{} god file(s) found (fan-out > 15)", health.god_files.len()),
            files: health.god_files.iter().map(|f| format!("{} (fan-out={})", f.path, f.value)).collect(),
        })
    } else {
        None
    }
}

/// Check maximum upward dependency violations.
pub fn check_max_upward(c: &Constraints, arch: &ArchReport) -> Option<RuleViolation> {
    let max_upward = c.max_upward_violations?;
    if arch.upward_violations.len() > max_upward {
        Some(RuleViolation {
            rule: "max_upward_violations".into(),
            severity: Severity::Error,
            message: format!("{} upward dependency violations, maximum allowed is {}", arch.upward_violations.len(), max_upward),
            files: arch.upward_violations.iter().take(5)
                .map(|v| format!("{} (L{}) \u{2192} {} (L{})", v.from_file, v.from_level, v.to_file, v.to_level))
                .collect(),
        })
    } else {
        None
    }
}
