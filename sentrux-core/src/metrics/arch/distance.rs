//! Distance from Main Sequence (Robert C. Martin 2003).
//!
//! For each module (directory), compute:
//!   A = abstract_types / total_types  (abstractness)
//!   I = Ce / (Ca + Ce)                (instability)
//!   D = |A + I - 1|                   (distance from main sequence)
//!
//! Abstract types = interfaces/traits (ClassInfo with k="interface").
//! Concrete types = classes/structs/type aliases.
//!
//! A module on the "main sequence" line has D~0 — balanced abstraction vs stability.
//! Zone of pain (D~1, low A, low I): concrete and stable = hard to change.
//! Zone of uselessness (D~1, high A, high I): abstract and unstable = nobody uses it.

use crate::core::types::ImportEdge;
use crate::core::snapshot::Snapshot;
use std::collections::{HashMap, HashSet};

// ── Named constants [ref:736ae249] ──

/// Minimum types in a module to compute distance.
const MIN_TYPES_FOR_DISTANCE: usize = 1;

/// Grade thresholds for average distance from main sequence.
const DISTANCE_THRESHOLD_A: f64 = 0.15;
const DISTANCE_THRESHOLD_B: f64 = 0.25;
const DISTANCE_THRESHOLD_C: f64 = 0.40;
const DISTANCE_THRESHOLD_D: f64 = 0.55;

// ── Public types ──

/// Robert C. Martin 2003: Distance from Main Sequence per module (directory).
/// D = |A + I - 1| where A = abstractness, I = instability.
/// D close to 0 = good (on the main sequence).
/// D close to 1 = bad (zone of pain or zone of uselessness).
#[derive(Debug, Clone)]
pub struct ModuleDistance {
    /// Module name (directory path, e.g. "src/layout")
    pub module: String,
    /// Abstractness A = abstract_types / total_types (0.0-1.0)
    pub abstractness: f64,
    /// Instability I = Ce / (Ca + Ce) (0.0-1.0)
    pub instability: f64,
    /// Distance from main sequence D = |A + I - 1| (0.0-1.0)
    pub distance: f64,
    /// Number of abstract types (interfaces, traits, ADTs)
    pub abstract_count: usize,
    /// Total types (abstract + concrete) in the module
    pub total_types: usize,
    /// Afferent coupling Ca (number of modules depending on this one)
    pub fan_in: usize,
    /// Efferent coupling Ce (number of modules this one depends on)
    pub fan_out: usize,
    /// True if this module is a pure foundation (I ≤ 0.10) — excluded from
    /// distance averaging because high D is architecturally correct for
    /// concrete, stable core modules.
    pub is_foundation: bool,
}

// ── Computation ──

/// Instability threshold below which a module is "pure foundation" and excluded
/// from the distance average. Foundation modules (I ≤ 0.10) are architecturally
/// correct when 100% concrete — they implement core types that everything depends on.
/// Including them inflates average distance without indicating a real design flaw.
/// This is universal: in any language, stable core modules SHOULD be concrete.
/// A module with I ≤ 0.30 is clearly in the stable zone: it has at least 70% of
/// its coupling as fan-in. Martin (2003) defines I < 0.5 as "stable"; 0.30 is a
/// conservative first-quartile cutoff within that range.
const FOUNDATION_INSTABILITY_THRESHOLD: f64 = 0.30;

/// Compute Distance from Main Sequence per module.
/// Walks the file tree to count abstract vs concrete types per directory,
/// then combines with per-module instability from the import graph.
pub fn compute_distance_from_main_seq(
    snapshot: &Snapshot,
    edges: &[ImportEdge],
    module_depth: Option<usize>,
) -> Vec<ModuleDistance> {
    // 1. Count abstract and total types per module
    let (module_abstract, module_total) = count_types_per_module(&snapshot.root, module_depth);

    // 2. Compute per-module fan-in (Ca) and fan-out (Ce) from import edges.
    // Filter mod-declaration edges (Rust `pub mod foo;`) — structural containment
    // inflates parent module fan-out without representing functional coupling.
    let dep_edges: Vec<ImportEdge> = edges.iter()
        .filter(|e| !crate::metrics::types::is_mod_declaration_edge(e))
        .cloned()
        .collect();
    let (module_fan_out, module_fan_in) = compute_module_coupling(&dep_edges, module_depth);

    // 3. Compute distance for each module that has types
    let mut results = build_module_distances(
        &module_abstract, &module_total, &module_fan_out, &module_fan_in,
    );

    // Sort by distance descending — worst first
    results.sort_unstable_by(|a, b| {
        b.distance.partial_cmp(&a.distance).unwrap_or(std::cmp::Ordering::Equal)
    });
    results
}

/// Recursively count abstract and total types per module from the file tree.
fn count_types_per_module(
    root: &crate::core::types::FileNode,
    module_depth: Option<usize>,
) -> (HashMap<String, usize>, HashMap<String, usize>) {
    let mut module_abstract: HashMap<String, usize> = HashMap::new();
    let mut module_total: HashMap<String, usize> = HashMap::new();
    walk_types(root, &mut module_abstract, &mut module_total, module_depth);
    (module_abstract, module_total)
}

/// Check whether a class kind counts as abstract (interface or ADT).
fn is_abstract_kind(kind: Option<&str>) -> bool {
    matches!(kind, Some("interface") | Some("adt"))
}

/// Python base classes that indicate an abstract type.
/// `typing.Protocol` (PEP 544) is the idiomatic way to define structural
/// interfaces in modern Python — equivalent to Go interfaces or TypeScript
/// structural types. `abc.ABC` / `ABCMeta` are the traditional approach.
const PYTHON_ABSTRACT_BASES: &[&str] = &[
    "Protocol", "ABC", "ABCMeta",
];

/// Check whether a class's base classes indicate it is abstract (Python).
/// Covers `typing.Protocol`, `abc.ABC`, and `abc.ABCMeta`.
fn has_abstract_base(bases: Option<&Vec<String>>) -> bool {
    match bases {
        Some(bs) => bs.iter().any(|b| {
            let name = b.rsplit('.').next().unwrap_or(b);
            PYTHON_ABSTRACT_BASES.contains(&name)
        }),
        None => false,
    }
}

/// Count abstract and total types in a single file node's class list.
fn count_file_types(
    node: &crate::core::types::FileNode,
    module_abstract: &mut HashMap<String, usize>,
    module_total: &mut HashMap<String, usize>,
    module_depth: Option<usize>,
) {
    let classes = match node.sa.as_ref().and_then(|sa| sa.cls.as_ref()) {
        Some(cls) if !cls.is_empty() => cls,
        _ => return,
    };
    let module = match module_depth {
        Some(d) => crate::core::path_utils::module_of_at_depth(&node.path, d).to_string(),
        None => crate::core::path_utils::module_of(&node.path).to_string(),
    };
    // Count abstract and total in a single pass, then insert once to avoid N string clones.
    let mut total_count = 0usize;
    let mut abstract_count = 0usize;
    for cls in classes {
        total_count += 1;
        if is_abstract_kind(cls.k.as_deref()) || has_abstract_base(cls.b.as_ref()) {
            abstract_count += 1;
        }
    }
    *module_total.entry(module.clone()).or_default() += total_count;
    if abstract_count > 0 {
        *module_abstract.entry(module).or_default() += abstract_count;
    }
}

fn walk_types(
    node: &crate::core::types::FileNode,
    module_abstract: &mut HashMap<String, usize>,
    module_total: &mut HashMap<String, usize>,
    module_depth: Option<usize>,
) {
    if node.is_dir {
        if let Some(children) = &node.children {
            for child in children {
                walk_types(child, module_abstract, module_total, module_depth);
            }
        }
        return;
    }
    count_file_types(node, module_abstract, module_total, module_depth);
}

/// Compute cross-module fan-in (Ca) and fan-out (Ce) from import edges.
fn compute_module_coupling(
    edges: &[ImportEdge],
    module_depth: Option<usize>,
) -> (HashMap<String, HashSet<String>>, HashMap<String, HashSet<String>>) {
    let mut module_fan_out: HashMap<String, HashSet<String>> = HashMap::new();
    let mut module_fan_in: HashMap<String, HashSet<String>> = HashMap::new();

    for edge in edges {
        let from_mod = match module_depth {
            Some(d) => crate::core::path_utils::module_of_at_depth(&edge.from_file, d).to_string(),
            None => crate::core::path_utils::module_of(&edge.from_file).to_string(),
        };
        let to_mod = match module_depth {
            Some(d) => crate::core::path_utils::module_of_at_depth(&edge.to_file, d).to_string(),
            None => crate::core::path_utils::module_of(&edge.to_file).to_string(),
        };
        if from_mod != to_mod {
            module_fan_out.entry(from_mod.clone()).or_default().insert(to_mod.clone());
            module_fan_in.entry(to_mod).or_default().insert(from_mod);
        }
    }

    (module_fan_out, module_fan_in)
}

/// Build ModuleDistance entries for all modules with sufficient types.
fn build_module_distances(
    module_abstract: &HashMap<String, usize>,
    module_total: &HashMap<String, usize>,
    module_fan_out: &HashMap<String, HashSet<String>>,
    module_fan_in: &HashMap<String, HashSet<String>>,
) -> Vec<ModuleDistance> {
    let mut all_modules: HashSet<String> = HashSet::new();
    all_modules.extend(module_total.keys().cloned());

    all_modules
        .into_iter()
        .filter_map(|module| {
            let total = *module_total.get(&module).unwrap_or(&0);
            if total < MIN_TYPES_FOR_DISTANCE {
                return None;
            }

            let abs_count = *module_abstract.get(&module).unwrap_or(&0);
            let abstractness = abs_count as f64 / total as f64;

            let ce = module_fan_out.get(&module).map_or(0, |s| s.len());
            let ca = module_fan_in.get(&module).map_or(0, |s| s.len());
            let instability = if ce + ca == 0 {
                0.5 // Isolated module — neutral
            } else {
                ce as f64 / (ca + ce) as f64
            };

            let distance = (abstractness + instability - 1.0).abs();

            Some(ModuleDistance {
                module,
                abstractness,
                instability,
                distance,
                abstract_count: abs_count,
                total_types: total,
                fan_in: ca,
                fan_out: ce,
                is_foundation: instability <= FOUNDATION_INSTABILITY_THRESHOLD,
            })
        })
        .collect()
}

// ── Grading ──

/// Grade distance from main sequence: average distance across modules.
/// Lower average distance = better (modules are on the main sequence).
/// [ref:736ae249]
pub fn grade_distance(avg_distance: f64) -> char {
    if avg_distance <= DISTANCE_THRESHOLD_A { 'A' }
    else if avg_distance <= DISTANCE_THRESHOLD_B { 'B' }
    else if avg_distance <= DISTANCE_THRESHOLD_C { 'C' }
    else if avg_distance <= DISTANCE_THRESHOLD_D { 'D' }
    else { 'F' }
}
