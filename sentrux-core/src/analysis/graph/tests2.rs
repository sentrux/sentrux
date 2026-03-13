#[cfg(test)]
mod tests {
    use crate::analysis::graph::*;
    use crate::analysis::test_helpers::make_file;
    use crate::core::types::ImportEdge;
    use crate::core::types::{FileNode, StructuralAnalysis};

    #[test]
    fn normalize_path_resolves_dotdot() {
        let p = std::path::Path::new("src/../a.css");
        assert_eq!(crate::analysis::resolver::suffix::normalize_path(p), "a.css");

        let p2 = std::path::Path::new("a/b/../c/./d.css");
        assert_eq!(crate::analysis::resolver::suffix::normalize_path(p2), "a/c/d.css");
    }

    #[test]
    fn python_absolute_import_resolves() {
        let tmp = std::env::temp_dir().join("sentrux_test_py_abs");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let files = vec![
            make_file("02_compute.py", "pipeline/02_compute.py", "python",
                Some(StructuralAnalysis {
                    functions: None, cls: None,
                    imp: Some(vec![
                        "orderflow_ml/config".to_string(),
                        "orderflow_ml/core/feature_store".to_string(),
                        "os".to_string(),
                    ]),
                    co: None, tags: None,
                }),
            ),
            make_file("config.py", "orderflow_ml/config.py", "python", None),
            make_file("feature_store.py", "orderflow_ml/core/feature_store.py", "python", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);
        assert_eq!(gr.import_edges.len(), 2, "expected 2 edges, got {:?}", gr.import_edges);
        let targets: Vec<&str> = gr.import_edges.iter().map(|e| e.to_file.as_str()).collect();
        assert!(targets.contains(&"orderflow_ml/config.py"));
        assert!(targets.contains(&"orderflow_ml/core/feature_store.py"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn python_nested_source_root_resolves() {
        let tmp = std::env::temp_dir().join("sentrux_test_py_nested");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let files = vec![
            make_file("02_compute_features.py", "python_ml/pipeline/02_compute_features.py", "python",
                Some(StructuralAnalysis {
                    functions: None, cls: None,
                    imp: Some(vec![
                        "orderflow_ml/config".to_string(),
                        "orderflow_ml/core/feature_store".to_string(),
                        "orderflow_ml/utils/symbol_utils".to_string(),
                    ]),
                    co: None, tags: None,
                }),
            ),
            make_file("__init__.py", "python_ml/orderflow_ml/config/__init__.py", "python", None),
            make_file("feature_store.py", "python_ml/orderflow_ml/core/feature_store.py", "python", None),
            make_file("symbol_utils.py", "python_ml/orderflow_ml/utils/symbol_utils.py", "python", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);
        assert_eq!(gr.import_edges.len(), 3, "expected 3 edges, got {:?}", gr.import_edges);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn python_relative_import_resolves() {
        let tmp = std::env::temp_dir().join("sentrux_test_py_rel");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let files = vec![
            make_file("main.py", "pkg/main.py", "python",
                Some(StructuralAnalysis {
                    functions: None, cls: None,
                    imp: Some(vec![".utils".to_string(), "..config".to_string()]),
                    co: None, tags: None,
                }),
            ),
            make_file("utils.py", "pkg/utils.py", "python", None),
            make_file("config.py", "config.py", "python", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);
        assert_eq!(gr.import_edges.len(), 2, "expected 2 edges, got {:?}", gr.import_edges);
        let targets: Vec<&str> = gr.import_edges.iter().map(|e| e.to_file.as_str()).collect();
        assert!(targets.contains(&"pkg/utils.py"));
        assert!(targets.contains(&"config.py"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn python_package_init_resolves() {
        let tmp = std::env::temp_dir().join("sentrux_test_py_init");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let files = vec![
            make_file("main.py", "main.py", "python",
                Some(StructuralAnalysis {
                    functions: None, cls: None,
                    imp: Some(vec!["mypackage".to_string()]),
                    co: None, tags: None,
                }),
            ),
            make_file("__init__.py", "mypackage/__init__.py", "python", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);
        assert_eq!(gr.import_edges.len(), 1);
        assert_eq!(gr.import_edges[0].to_file, "mypackage/__init__.py");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn python_no_self_edges() {
        let tmp = std::env::temp_dir().join("sentrux_test_py_self");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let files = vec![make_file("config.py", "orderflow_ml/config.py", "python",
            Some(StructuralAnalysis {
                functions: None, cls: None,
                imp: Some(vec!["orderflow_ml/config".to_string()]),
                co: None, tags: None,
            }),
        )];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);
        assert!(gr.import_edges.is_empty(), "Self-import should produce no edge");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn suffix_collision_picks_closest() {
        let tmp = std::env::temp_dir().join("sentrux_test_py_collision");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let files = vec![
            make_file("02_compute.py", "python_ml/pipeline/02_compute.py", "python",
                Some(StructuralAnalysis {
                    functions: None, cls: None,
                    imp: Some(vec!["orderflow_ml/config".to_string()]),
                    co: None, tags: None,
                }),
            ),
            make_file("02_compute_dup.py", "arch/python_ml1/pipeline/02_compute.py", "python",
                Some(StructuralAnalysis {
                    functions: None, cls: None,
                    imp: Some(vec!["orderflow_ml/config".to_string()]),
                    co: None, tags: None,
                }),
            ),
            make_file("config1.py", "python_ml/orderflow_ml/config.py", "python", None),
            make_file("config2.py", "arch/python_ml1/orderflow_ml/config.py", "python", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);
        assert_eq!(gr.import_edges.len(), 2, "expected 2 edges, got {:?}", gr.import_edges);

        for edge in &gr.import_edges {
            if edge.from_file == "python_ml/pipeline/02_compute.py" {
                assert_eq!(edge.to_file, "python_ml/orderflow_ml/config.py");
            } else if edge.from_file == "arch/python_ml1/pipeline/02_compute.py" {
                assert_eq!(edge.to_file, "arch/python_ml1/orderflow_ml/config.py");
            } else {
                panic!("unexpected from_file: {:?}", edge.from_file);
            }
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn idempotency_test() {
        let tmp = std::env::temp_dir().join("sentrux_test_idempotency");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let files = vec![
            make_file("index.html", "index.html", "html",
                Some(StructuralAnalysis {
                    functions: None, cls: None,
                    imp: Some(vec!["app.js".to_string()]),
                    co: None, tags: None,
                }),
            ),
            make_file("app.js", "app.js", "javascript", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr1 = build_graphs(&refs, Some(&tmp), 5);
        let gr2 = build_graphs(&refs, Some(&tmp), 5);

        assert_eq!(gr1.import_edges.len(), gr2.import_edges.len());
        assert_eq!(gr1.call_edges.len(), gr2.call_edges.len());
        assert_eq!(gr1.inherit_edges.len(), gr2.inherit_edges.len());
        assert_eq!(gr1.entry_points.len(), gr2.entry_points.len());
        assert_eq!(gr1.exec_depth, gr2.exec_depth);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn monorepo_no_cross_project_edges() {
        let tmp = std::env::temp_dir().join("sentrux_test_monorepo_boundary");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("project_a/src")).unwrap();
        std::fs::create_dir_all(tmp.join("project_b/src")).unwrap();
        std::fs::write(tmp.join("project_a/Cargo.toml"), "[package]\nname = \"project_a\"").unwrap();
        std::fs::write(tmp.join("project_b/Cargo.toml"), "[package]\nname = \"project_b\"").unwrap();

        let files = vec![
            make_file("main.rs", "project_a/src/main.rs", "rust",
                Some(StructuralAnalysis {
                    functions: None, cls: None,
                    imp: Some(vec!["crate/graph".to_string()]),
                    co: None, tags: None,
                }),
            ),
            make_file("graph.rs", "project_a/src/graph.rs", "rust", None),
            make_file("graph.rs", "project_b/src/graph.rs", "rust", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);
        assert_eq!(gr.import_edges.len(), 1,
            "expected 1 edge within project_a, got {:?}", gr.import_edges);
        assert_eq!(gr.import_edges[0].from_file, "project_a/src/main.rs");
        assert_eq!(gr.import_edges[0].to_file, "project_a/src/graph.rs");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rust_use_tree_resolves_to_module_files() {
        let tmp = std::env::temp_dir().join("sentrux_test_rust_usetree");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src/models")).unwrap();
        std::fs::create_dir_all(tmp.join("src/store")).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "[package]\nname = \"beemem\"").unwrap();

        let files = vec![
            make_file("db.rs", "src/store/db.rs", "rust",
                Some(StructuralAnalysis {
                    functions: None, cls: None, co: None, tags: None,
                    imp: Some(vec![
                        "crate/models/episode".to_string(),
                        "crate/models/primitive".to_string(),
                        "crate/store/schema".to_string(),
                    ]),
                }),
            ),
            make_file("mod.rs", "src/models/mod.rs", "rust",
                Some(StructuralAnalysis {
                    functions: None, cls: None, co: None, tags: None,
                    imp: Some(vec!["episode".to_string(), "primitive".to_string()]),
                }),
            ),
            make_file("mod.rs", "src/store/mod.rs", "rust",
                Some(StructuralAnalysis {
                    functions: None, cls: None, co: None, tags: None,
                    imp: Some(vec!["db".to_string(), "schema".to_string(), "vectors".to_string()]),
                }),
            ),
            make_file("episode.rs", "src/models/episode.rs", "rust", None),
            make_file("primitive.rs", "src/models/primitive.rs", "rust", None),
            make_file("schema.rs", "src/store/schema.rs", "rust", None),
            make_file("vectors.rs", "src/store/vectors.rs", "rust", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);

        let db_edges: Vec<&ImportEdge> = gr.import_edges.iter()
            .filter(|e| e.from_file == "src/store/db.rs").collect();
        let db_targets: std::collections::HashSet<&str> = db_edges.iter()
            .map(|e| e.to_file.as_str()).collect();

        assert!(db_targets.contains("src/models/episode.rs"));
        assert!(db_targets.contains("src/models/primitive.rs"));
        assert!(db_targets.contains("src/store/schema.rs"));
        assert!(!db_targets.contains("src/models/mod.rs"));
        assert!(!db_targets.contains("src/store/mod.rs"));
        assert_eq!(db_edges.len(), 3);

        let mod_edges: Vec<&ImportEdge> = gr.import_edges.iter()
            .filter(|e| e.from_file == "src/models/mod.rs").collect();
        let mod_targets: std::collections::HashSet<&str> = mod_edges.iter()
            .map(|e| e.to_file.as_str()).collect();
        assert!(mod_targets.contains("src/models/episode.rs"));
        assert!(mod_targets.contains("src/models/primitive.rs"));

        let store_mod_edges: Vec<&ImportEdge> = gr.import_edges.iter()
            .filter(|e| e.from_file == "src/store/mod.rs").collect();
        let store_targets: std::collections::HashSet<&str> = store_mod_edges.iter()
            .map(|e| e.to_file.as_str()).collect();
        assert!(store_targets.contains("src/store/db.rs"));
        assert!(store_targets.contains("src/store/schema.rs"));
        assert!(store_targets.contains("src/store/vectors.rs"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rust_mod_rs_resolves_as_directory_module() {
        let tmp = std::env::temp_dir().join("sentrux_test_mod_rs");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src/commands")).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "[package]\nname = \"beemem\"").unwrap();

        let files = vec![
            make_file("main.rs", "src/main.rs", "rust",
                Some(StructuralAnalysis {
                    functions: None, cls: None, co: None, tags: None,
                    imp: Some(vec!["beemem/commands".to_string()]),
                }),
            ),
            make_file("mod.rs", "src/commands/mod.rs", "rust", None),
            make_file("inject.rs", "src/commands/inject.rs", "rust", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);

        let main_edges: Vec<&ImportEdge> = gr.import_edges.iter()
            .filter(|e| e.from_file == "src/main.rs").collect();
        let targets: Vec<&str> = main_edges.iter().map(|e| e.to_file.as_str()).collect();
        assert!(targets.contains(&"src/commands/mod.rs"),
            "main.rs should connect to commands/mod.rs, got: {:?}", targets);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rust_crate_name_resolves_to_lib_rs() {
        let tmp = std::env::temp_dir().join("sentrux_test_crate_name");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "[package]\nname = \"sentrux-tauri\"").unwrap();

        let files = vec![
            make_file("main.rs", "src/main.rs", "rust",
                Some(StructuralAnalysis {
                    functions: None, cls: None, co: None, tags: None,
                    imp: Some(vec!["sentrux_tauri".to_string()]),
                }),
            ),
            make_file("lib.rs", "src/lib.rs", "rust", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);

        let main_edges: Vec<&ImportEdge> = gr.import_edges.iter()
            .filter(|e| e.from_file == "src/main.rs").collect();
        let targets: Vec<&str> = main_edges.iter().map(|e| e.to_file.as_str()).collect();
        assert!(targets.contains(&"src/lib.rs"),
            "main.rs should connect to lib.rs via crate name, got: {:?}", targets);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn html_links_to_css() {
        let tmp = std::env::temp_dir().join("sentrux_test_html_css");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("index.html"), r#"<link rel="stylesheet" href="src/style.css" />"#).unwrap();
        std::fs::write(tmp.join("src/style.css"), "body { color: red; }").unwrap();

        let files = vec![
            make_file("index.html", "index.html", "html",
                Some(StructuralAnalysis {
                    functions: None, cls: None, co: None, tags: None,
                    imp: Some(vec!["src/style.css".to_string()]),
                })),
            make_file("style.css", "src/style.css", "css", None),
        ];
        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);
        assert!(gr.import_edges.iter().any(|e| e.from_file == "index.html" && e.to_file == "src/style.css"),
            "expected HTML->CSS edge, got {:?}", gr.import_edges);
    }

    #[test]
    fn python_dotted_import_resolved() {
        let tmp = std::env::temp_dir().join("sentrux_test_python_dotted");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("mylib/utils")).unwrap();
        std::fs::write(tmp.join("main.py"), "from mylib.utils.helpers import foo").unwrap();
        std::fs::write(tmp.join("mylib/utils/helpers.py"), "def foo(): pass").unwrap();

        let files = vec![
            make_file("main.py", "main.py", "python",
                Some(StructuralAnalysis {
                    functions: None, cls: None, co: None, tags: None,
                    imp: Some(vec!["mylib/utils/helpers".to_string()]),
                })),
            make_file("helpers.py", "mylib/utils/helpers.py", "python", None),
        ];
        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);
        assert!(gr.import_edges.iter().any(|e| e.from_file == "main.py" && e.to_file == "mylib/utils/helpers.py"),
            "expected main.py->helpers.py edge, got {:?}", gr.import_edges);
    }

    #[test]
    fn go_package_imports_resolve_to_directory() {
        // Go imports reference packages (directories), not files.
        // "internal/config" should resolve to a .go file inside internal/config/.
        let tmp = std::env::temp_dir().join("sentrux_test_go_pkg");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("internal/config")).unwrap();
        std::fs::create_dir_all(tmp.join("internal/handler")).unwrap();
        std::fs::write(tmp.join("go.mod"), "module github.com/slush-dev/phonevault\n\ngo 1.21\n").unwrap();

        let files = vec![
            make_file("main.go", "cmd/server/main.go", "go",
                Some(StructuralAnalysis {
                    functions: None, cls: None, co: None, tags: None,
                    imp: Some(vec![
                        "github.com/slush-dev/phonevault/internal/config".to_string(),
                        "github.com/slush-dev/phonevault/internal/handler".to_string(),
                    ]),
                }),
            ),
            make_file("config.go", "internal/config/config.go", "go", None),
            make_file("defaults.go", "internal/config/defaults.go", "go", None),
            make_file("handler.go", "internal/handler/handler.go", "go", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);

        let main_edges: Vec<&ImportEdge> = gr.import_edges.iter()
            .filter(|e| e.from_file == "cmd/server/main.go").collect();
        let targets: std::collections::HashSet<&str> = main_edges.iter()
            .map(|e| e.to_file.as_str()).collect();

        assert!(targets.iter().any(|t| t.starts_with("internal/config/")),
            "expected edge to internal/config/*.go, got targets: {:?}", targets);
        assert!(targets.iter().any(|t| t.starts_with("internal/handler/")),
            "expected edge to internal/handler/*.go, got targets: {:?}", targets);
        assert!(main_edges.len() >= 2,
            "expected at least 2 import edges, got {}: {:?}", main_edges.len(), main_edges);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn go_nested_module_imports_resolve() {
        // Go project nested under a subdirectory (e.g. monorepo with server/go.mod).
        // Module-qualified imports like "github.com/user/repo/internal/config"
        // must resolve even when go.mod is not at the scan root.
        let tmp = std::env::temp_dir().join("sentrux_test_go_nested_mod");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("server/cmd/server")).unwrap();
        std::fs::create_dir_all(tmp.join("server/internal/config")).unwrap();
        std::fs::create_dir_all(tmp.join("server/internal/handler")).unwrap();
        std::fs::write(tmp.join("server/go.mod"),
            "module github.com/example/myapp\n\ngo 1.21\n").unwrap();

        let files = vec![
            make_file("main.go", "server/cmd/server/main.go", "go",
                Some(StructuralAnalysis {
                    functions: None, cls: None, co: None, tags: None,
                    imp: Some(vec![
                        "github.com/example/myapp/internal/config".to_string(),
                        "github.com/example/myapp/internal/handler".to_string(),
                    ]),
                }),
            ),
            make_file("config.go", "server/internal/config/config.go", "go", None),
            make_file("handler.go", "server/internal/handler/handler.go", "go", None),
        ];

        let refs: Vec<&FileNode> = files.iter().collect();
        let gr = build_graphs(&refs, Some(&tmp), 5);

        let main_edges: Vec<&ImportEdge> = gr.import_edges.iter()
            .filter(|e| e.from_file == "server/cmd/server/main.go").collect();
        let targets: std::collections::HashSet<&str> = main_edges.iter()
            .map(|e| e.to_file.as_str()).collect();

        assert!(targets.iter().any(|t| t.starts_with("server/internal/config/")),
            "expected edge to server/internal/config/*.go, got targets: {:?}", targets);
        assert!(targets.iter().any(|t| t.starts_with("server/internal/handler/")),
            "expected edge to server/internal/handler/*.go, got targets: {:?}", targets);
        assert!(main_edges.len() >= 2,
            "expected at least 2 import edges, got {}: {:?}", main_edges.len(), main_edges);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
