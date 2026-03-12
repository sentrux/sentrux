//! Sentrux binary — GUI, CLI, and MCP entry points.
//!
//! All logic lives in `sentrux-core`. This crate is just the entry point
//! that wires together the three modes:
//! - GUI mode (default): interactive treemap/blueprint visualizer
//! - MCP mode (`--mcp`): Model Context Protocol server for AI agent integration
//! - Check mode (`check <path>`): CLI architectural rules enforcement

use sentrux_core::analysis;
use sentrux_core::app;
use sentrux_core::core;
use sentrux_core::metrics;

/// Run architectural rules check from CLI. Returns exit code.
fn run_check(path: &str) -> i32 {
    let root = std::path::Path::new(path);
    if !root.is_dir() {
        eprintln!("Error: not a directory: {path}");
        return 1;
    }

    let config = match metrics::rules::RulesConfig::try_load(root) {
        Some(c) => c,
        None => {
            eprintln!("No .sentrux/rules.toml found in {path}");
            eprintln!("Create one to define architectural constraints.");
            return 1;
        }
    };

    eprintln!("Scanning {path}...");
    let result = match analysis::scanner::scan_directory(
        path, None, None,
        &cli_scan_limits(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Scan failed: {e}");
            return 1;
        }
    };

    let health = metrics::compute_health(&result.snapshot);
    let arch_report = metrics::arch::compute_arch(&result.snapshot);
    let check = metrics::rules::check_rules(&config, &health, &arch_report, &result.snapshot.import_graph);

    print_check_results(&check, &health, &arch_report)
}

/// Print check results and return exit code (0 = pass, 1 = violations).
fn print_check_results(
    check: &metrics::rules::RuleCheckResult,
    health: &metrics::HealthReport,
    arch_report: &metrics::arch::ArchReport,
) -> i32 {
    println!("sentrux check — {} rules checked\n", check.rules_checked);
    println!("Structure grade: {}  Architecture grade: {}\n",
        health.grade, arch_report.arch_grade);

    if check.violations.is_empty() {
        println!("✓ All rules pass");
        0
    } else {
        for v in &check.violations {
            let icon = match v.severity {
                metrics::rules::Severity::Error => "✗",
                metrics::rules::Severity::Warning => "⚠",
            };
            println!("{icon} [{:?}] {}: {}", v.severity, v.rule, v.message);
            for f in &v.files {
                println!("    {f}");
            }
        }
        println!("\n✗ {} violation(s) found", check.violations.len());
        1
    }
}

/// Run structural regression gate from CLI. Returns exit code.
fn run_gate(args: &[String]) -> i32 {
    let save_mode = args.iter().any(|a| a == "--save");
    let path = args.iter()
        .skip(1)
        .rfind(|a| !a.starts_with('-') && *a != "gate")
        .map(|s| s.as_str())
        .unwrap_or(".");

    let root = std::path::Path::new(path);
    if !root.is_dir() {
        eprintln!("Error: not a directory: {path}");
        return 1;
    }

    let baseline_path = root.join(".sentrux").join("baseline.json");

    eprintln!("Scanning {path}...");
    let result = match analysis::scanner::scan_directory(
        path, None, None,
        &cli_scan_limits(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Scan failed: {e}");
            return 1;
        }
    };

    let health = metrics::compute_health(&result.snapshot);
    let arch_report = metrics::arch::compute_arch(&result.snapshot);

    if save_mode {
        gate_save(&baseline_path, &health, &arch_report)
    } else {
        gate_compare(&baseline_path, &health, &arch_report)
    }
}

fn gate_save(
    baseline_path: &std::path::Path,
    health: &metrics::HealthReport,
    arch_report: &metrics::arch::ArchReport,
) -> i32 {
    if let Some(parent) = baseline_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("Failed to create directory {}: {e}", parent.display());
            return 1;
        }
    }
    let baseline = metrics::arch::ArchBaseline::from_health(health);
    match baseline.save(baseline_path) {
        Ok(()) => {
            println!("Baseline saved to {}", baseline_path.display());
            println!("Structure grade: {}  Architecture grade: {}",
                health.grade, arch_report.arch_grade);
            println!("\nRun `sentrux gate` after making changes to compare.");
            0
        }
        Err(e) => {
            eprintln!("Failed to save baseline: {e}");
            1
        }
    }
}

fn gate_compare(
    baseline_path: &std::path::Path,
    health: &metrics::HealthReport,
    arch_report: &metrics::arch::ArchReport,
) -> i32 {
    let baseline = match metrics::arch::ArchBaseline::load(baseline_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to load baseline at {}: {e}", baseline_path.display());
            eprintln!("Run `sentrux gate --save` first to create one.");
            return 1;
        }
    };

    let diff = baseline.diff(health);

    println!("sentrux gate — structural regression check\n");
    println!("Structure:    {} → {}  Architecture: {}",
        diff.structure_grade_before, diff.structure_grade_after,
        arch_report.arch_grade);
    println!("Coupling:     {:.2} → {:.2}", diff.coupling_before, diff.coupling_after);
    println!("Cycles:       {} → {}", diff.cycles_before, diff.cycles_after);
    println!("God files:    {} → {}", diff.god_files_before, diff.god_files_after);

    if !arch_report.distance_metrics.is_empty() {
        println!("\nDistance from Main Sequence: {:.2} (grade {})",
            arch_report.avg_distance, arch_report.distance_grade);
    }

    if diff.degraded {
        println!("\n✗ DEGRADED");
        for v in &diff.violations {
            println!("  ✗ {v}");
        }
        1
    } else {
        println!("\n✓ No degradation detected");
        0
    }
}

fn cli_scan_limits() -> analysis::scanner::common::ScanLimits {
    let s = core::settings::Settings::default();
    analysis::scanner::common::ScanLimits {
        max_file_size_kb: s.max_file_size_kb,
        max_parse_size_kb: s.max_parse_size_kb,
        max_call_targets: s.max_call_targets,
    }
}

/// Auto-install standard language plugins if none are found.
/// Runs on first launch — gives users a working tool without manual steps.
fn auto_install_plugins_if_needed() {
    let dir = match sentrux_core::analysis::plugin::plugins_dir() {
        Some(d) => d,
        None => return,
    };
    // Check if any plugin directory with a valid plugin.toml exists
    if dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let has_valid_plugin = entries
                .filter_map(|e| e.ok())
                .any(|e| e.path().is_dir() && e.path().join("plugin.toml").exists());
            if has_valid_plugin {
                return; // plugins exist, skip auto-install
            }
        }
    }

    eprintln!("\nFirst run — installing standard language plugins...\n");
    std::fs::create_dir_all(&dir).ok();

    let platform = sentrux_core::analysis::plugin::manifest::PluginManifest::grammar_filename();
    let platform_key = platform.rsplit_once('.').map_or(platform, |(k, _)| k);

    let standard = [
        "python", "javascript", "typescript", "rust", "go",
        "c", "cpp", "java", "ruby", "csharp", "php", "bash",
        "html", "css", "scss", "swift", "lua", "scala",
        "elixir", "haskell", "zig", "r",
    ];

    let mut installed = 0;
    for name in &standard {
        let plugin_dir = dir.join(name);
        if plugin_dir.exists() { continue; }
        let url = format!(
            "https://github.com/sentrux/plugins/releases/download/{name}-v0.1.0/{name}-{platform_key}.tar.gz"
        );
        eprint!("  {name}...");
        let ok = std::process::Command::new("curl")
            .args(["-fsSL", &url, "-o"])
            .arg(dir.join(format!("{name}.tar.gz")))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            let extracted = std::process::Command::new("tar")
                .args(["xzf", &format!("{name}.tar.gz")])
                .current_dir(&dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            let _ = std::fs::remove_file(dir.join(format!("{name}.tar.gz")));
            if extracted {
                eprint!(" ok  ");
                installed += 1;
                if installed % 6 == 0 { eprintln!(); }
            }
        } else {
            let _ = std::fs::remove_file(dir.join(format!("{name}.tar.gz")));
        }
    }
    eprintln!("\n\n  Installed {installed} language plugins.\n");
}

fn main() -> eframe::Result<()> {
    // Auto-install standard plugins on first run
    auto_install_plugins_if_needed();

    // Non-blocking update check (once per day, background thread)
    app::update_check::check_for_updates_async(env!("CARGO_PKG_VERSION"));

    // --version: show version + edition (free or pro)
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        let edition = if sentrux_core::license::current_tier() >= sentrux_core::license::Tier::Pro { "Pro" } else { "Free" };
        println!("sentrux {} ({})", env!("CARGO_PKG_VERSION"), edition);
        return Ok(());
    }

    if std::env::args().any(|a| a == "--mcp") {
        app::mcp_server::run_mcp_server(None);
        return Ok(());
    }

    // Plugin management commands
    if std::env::args().any(|a| a == "plugin") {
        let sub = std::env::args().skip_while(|a| a != "plugin").nth(1);
        match sub.as_deref() {
            Some("list") => {
                let dir = sentrux_core::analysis::plugin::plugins_dir();
                println!("Plugin directory: {}", dir.as_ref().map_or("(none)".into(), |d| d.display().to_string()));
                let (loaded, errors) = sentrux_core::analysis::plugin::load_all_plugins();
                if loaded.is_empty() && errors.is_empty() {
                    println!("No plugins installed.");
                    println!("\nInstall a plugin by placing it in ~/.sentrux/plugins/<name>/");
                } else {
                    for p in &loaded {
                        println!("  {} v{} [{}] — {}", p.name, p.version, p.extensions.join(", "), p.display_name);
                    }
                    for e in &errors {
                        println!("  (error) {} — {}", e.plugin_dir.display(), e.error);
                    }
                }
                return Ok(());
            }
            Some("init") => {
                let name = std::env::args().skip_while(|a| a != "init").nth(1)
                    .unwrap_or_else(|| { eprintln!("Usage: sentrux plugin init <language-name>"); std::process::exit(1); });
                let dir = sentrux_core::analysis::plugin::plugins_dir()
                    .unwrap_or_else(|| { eprintln!("Cannot determine home directory"); std::process::exit(1); });
                let plugin_dir = dir.join(&name);
                if plugin_dir.exists() {
                    eprintln!("Plugin directory already exists: {}", plugin_dir.display());
                    std::process::exit(1);
                }
                std::fs::create_dir_all(plugin_dir.join("grammars")).unwrap();
                std::fs::create_dir_all(plugin_dir.join("queries")).unwrap();
                std::fs::create_dir_all(plugin_dir.join("tests")).unwrap();
                std::fs::write(plugin_dir.join("plugin.toml"), format!(r#"[plugin]
name = "{name}"
display_name = "{name}"
version = "0.1.0"
extensions = ["TODO"]
min_sentrux_version = "0.1.3"

[plugin.metadata]
author = ""
description = ""

[grammar]
source = "https://github.com/TODO/tree-sitter-{name}"
ref = "main"
abi_version = 14

[queries]
capabilities = ["functions", "classes", "imports"]

[checksums]
"#)).unwrap();
                std::fs::write(plugin_dir.join("queries").join("tags.scm"),
                    ";; TODO: Write tree-sitter queries for this language\n;;\n;; Required captures:\n;;   @func.def / @func.name — function definitions\n;;   @class.def / @class.name — class definitions\n;;   @import.path — import statements\n;;   @call.name — function calls (optional)\n"
                ).unwrap();
                println!("Created plugin template at {}", plugin_dir.display());
                println!("\nNext steps:");
                println!("  1. Edit plugin.toml — set extensions, grammar source");
                println!("  2. Build the grammar: tree-sitter generate && cc -shared -o grammars/{} src/parser.c",
                    sentrux_core::analysis::plugin::manifest::PluginManifest::grammar_filename());
                println!("  3. Write queries/tags.scm");
                println!("  4. Test: sentrux plugin validate {}", plugin_dir.display());
                return Ok(());
            }
            Some("validate") => {
                let path = std::env::args().skip_while(|a| a != "validate").nth(1)
                    .unwrap_or_else(|| { eprintln!("Usage: sentrux plugin validate <plugin-dir>"); std::process::exit(1); });
                let plugin_dir = std::path::Path::new(&path);
                print!("Validating {}... ", plugin_dir.display());
                match sentrux_core::analysis::plugin::manifest::PluginManifest::load(plugin_dir) {
                    Ok(manifest) => {
                        println!("plugin.toml OK");
                        println!("  name: {}", manifest.plugin.name);
                        println!("  version: {}", manifest.plugin.version);
                        println!("  extensions: [{}]", manifest.plugin.extensions.join(", "));
                        println!("  capabilities: [{}]", manifest.queries.capabilities.join(", "));
                        let query_path = plugin_dir.join("queries").join("tags.scm");
                        match std::fs::read_to_string(&query_path) {
                            Ok(qs) => {
                                match manifest.validate_query_captures(&qs) {
                                    Ok(()) => println!("  queries/tags.scm: OK (captures valid)"),
                                    Err(e) => println!("  queries/tags.scm: FAIL — {}", e),
                                }
                            }
                            Err(e) => println!("  queries/tags.scm: MISSING — {}", e),
                        }
                        let gf = sentrux_core::analysis::plugin::manifest::PluginManifest::grammar_filename();
                        let gp = plugin_dir.join("grammars").join(gf);
                        if gp.exists() {
                            println!("  grammars/{}: OK", gf);
                        } else {
                            println!("  grammars/{}: MISSING — build the grammar first", gf);
                        }
                    }
                    Err(e) => {
                        println!("FAIL — {}", e);
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            Some("add-standard") => {
                let standard = [
                    "python", "javascript", "typescript", "rust", "go",
                    "c", "cpp", "java", "ruby", "csharp", "php", "bash",
                    "html", "css", "scss", "swift", "lua", "scala",
                    "elixir", "haskell", "zig", "r", "ocaml",
                ];
                let dir = sentrux_core::analysis::plugin::plugins_dir()
                    .unwrap_or_else(|| { eprintln!("Cannot determine home directory"); std::process::exit(1); });
                std::fs::create_dir_all(&dir).unwrap();
                let platform = sentrux_core::analysis::plugin::manifest::PluginManifest::grammar_filename();
                let platform_key = platform.rsplit_once('.').map_or(platform, |(k, _)| k);
                let mut installed = 0;
                let mut skipped = 0;
                for name in &standard {
                    let plugin_dir = dir.join(name);
                    if plugin_dir.exists() {
                        skipped += 1;
                        continue;
                    }
                    let url = format!(
                        "https://github.com/sentrux/plugins/releases/download/{name}-v0.1.0/{name}-{platform_key}.tar.gz"
                    );
                    print!("  Installing {name}...");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                    let ok = std::process::Command::new("curl")
                        .args(["-fsSL", &url, "-o"])
                        .arg(dir.join(format!("{name}.tar.gz")))
                        .status()
                        .is_ok_and(|s| s.success());
                    if ok {
                        let extracted = std::process::Command::new("tar")
                            .args(["xzf", &format!("{name}.tar.gz")])
                            .current_dir(&dir)
                            .status()
                            .is_ok_and(|s| s.success());
                        let _ = std::fs::remove_file(dir.join(format!("{name}.tar.gz")));
                        if extracted {
                            println!(" ok");
                            installed += 1;
                        } else {
                            println!(" failed (extract)");
                        }
                    } else {
                        let _ = std::fs::remove_file(dir.join(format!("{name}.tar.gz")));
                        println!(" failed (download)");
                    }
                }
                println!("\nInstalled {installed} languages ({skipped} already installed)");
                return Ok(());
            }
            Some("add") => {
                let name = std::env::args().skip_while(|a| a != "add").nth(1)
                    .unwrap_or_else(|| { eprintln!("Usage: sentrux plugin add <name>"); std::process::exit(1); });
                let dir = sentrux_core::analysis::plugin::plugins_dir()
                    .unwrap_or_else(|| { eprintln!("Cannot determine home directory"); std::process::exit(1); });
                let plugin_dir = dir.join(&name);
                if plugin_dir.exists() {
                    eprintln!("Plugin '{}' already installed at {}", name, plugin_dir.display());
                    eprintln!("Remove it first: sentrux plugin remove {}", name);
                    std::process::exit(1);
                }

                // Determine platform
                let platform = sentrux_core::analysis::plugin::manifest::PluginManifest::grammar_filename();
                let platform_key = platform.rsplit_once('.').map_or(platform, |(k, _)| k);

                // Download from GitHub releases
                let url = format!(
                    "https://github.com/sentrux/plugins/releases/download/{name}-v0.1.0/{name}-{platform_key}.tar.gz"
                );
                println!("Downloading {name} plugin for {platform_key}...");
                println!("  {url}");

                // Ensure plugins directory exists before downloading
                std::fs::create_dir_all(&dir).unwrap();

                let output = std::process::Command::new("curl")
                    .args(["-fsSL", &url, "-o"])
                    .arg(dir.join(format!("{name}.tar.gz")))
                    .status();

                match output {
                    Ok(s) if s.success() => {
                        let extract = std::process::Command::new("tar")
                            .args(["xzf", &format!("{name}.tar.gz")])
                            .current_dir(&dir)
                            .status();
                        let _ = std::fs::remove_file(dir.join(format!("{name}.tar.gz")));
                        match extract {
                            Ok(s) if s.success() => {
                                println!("Installed {name} to {}", plugin_dir.display());
                            }
                            _ => {
                                eprintln!("Failed to extract plugin archive");
                                std::process::exit(1);
                            }
                        }
                    }
                    _ => {
                        let _ = std::fs::remove_file(dir.join(format!("{name}.tar.gz")));
                        eprintln!("Failed to download plugin '{name}'.");
                        eprintln!("Check available plugins: https://github.com/sentrux/plugins/releases");
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            Some("remove") => {
                let name = std::env::args().skip_while(|a| a != "remove").nth(1)
                    .unwrap_or_else(|| { eprintln!("Usage: sentrux plugin remove <name>"); std::process::exit(1); });
                let dir = sentrux_core::analysis::plugin::plugins_dir()
                    .unwrap_or_else(|| { eprintln!("Cannot determine home directory"); std::process::exit(1); });
                let plugin_dir = dir.join(&name);
                if !plugin_dir.exists() {
                    eprintln!("Plugin '{}' not installed.", name);
                    std::process::exit(1);
                }
                std::fs::remove_dir_all(&plugin_dir).unwrap();
                println!("Removed plugin '{}'", name);
                return Ok(());
            }
            _ => {
                println!("Usage: sentrux plugin <add-standard|add|remove|list|init|validate>");
                println!("  add-standard         — install all 23 standard languages");
                println!("  add <name>          — install a single language plugin");
                println!("  remove <name>        — remove an installed plugin");
                println!("  list                — show installed plugins");
                println!("  init <name>          — create a plugin template");
                println!("  validate <dir>       — validate a plugin directory");
                return Ok(());
            }
        }
    }

    if std::env::args().any(|a| a == "check") {
        let path = std::env::args()
            .skip_while(|a| a != "check")
            .nth(1)
            .unwrap_or_else(|| ".".to_string());
        std::process::exit(run_check(&path));
    }

    if std::env::args().any(|a| a == "gate") {
        let args: Vec<String> = std::env::args().collect();
        std::process::exit(run_gate(&args));
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title(if sentrux_core::license::current_tier() >= sentrux_core::license::Tier::Pro { "Sentrux Pro" } else { "sentrux" }),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(eframe::egui_wgpu::WgpuSetupCreateNew {
                instance_descriptor: eframe::wgpu::InstanceDescriptor {
                    // Vulkan surface creation via EGL/DRI3 is unreliable on many
                    // Linux setups; default to GL (still hardware-accelerated).
                    // Override with WGPU_BACKEND=vulkan if desired.
                    backends: eframe::wgpu::Backends::from_env()
                        .unwrap_or(if cfg!(target_os = "linux") {
                            eframe::wgpu::Backends::GL
                        } else {
                            eframe::wgpu::Backends::PRIMARY | eframe::wgpu::Backends::GL
                        }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    eframe::run_native(
        "Sentrux",
        options,
        Box::new(|cc| Ok(Box::new(app::SentruxApp::new(cc)))),
    )
}
