//! Sentrux binary — GUI, CLI, and MCP entry points.
//!
//! All logic lives in `sentrux-core`. This crate is just the entry point
//! that wires together the three modes:
//! - GUI mode (default): interactive treemap/blueprint visualizer
//! - MCP mode (`sentrux mcp`): Model Context Protocol server for AI agent integration
//! - Check mode (`sentrux check [path]`): CLI architectural rules enforcement
//! - Gate mode (`sentrux gate [--save] [path]`): structural regression testing

use clap::{Parser, Subcommand};
use sentrux_core::analysis;
use sentrux_core::app;
use sentrux_core::core;
use sentrux_core::metrics;

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

fn edition_name() -> &'static str {
    let tier = sentrux_core::license::current_tier();
    if tier >= sentrux_core::license::Tier::Pro {
        "Pro"
    } else {
        ""            // Don't show "Free" or "Community" — just "sentrux"
    }
}

fn version_string() -> &'static str {
    use std::sync::OnceLock;
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let edition = edition_name();
        let base = if edition.is_empty() {
            env!("CARGO_PKG_VERSION").to_string()
        } else {
            format!("{} ({})", env!("CARGO_PKG_VERSION"), edition)
        };
        if let Some(latest) = sentrux_core::app::update_check::available_update() {
            format!("{}\n  Update available: v{} → brew upgrade sentrux", base, latest)
        } else {
            base
        }
    })
}

#[derive(Parser)]
#[command(
    name = "sentrux",
    about = "Live codebase visualization and structural quality gate",
    version = version_string(),
    arg_required_else_help = false,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Directory to open in the GUI
    #[arg(global = false)]
    path: Option<String>,

    /// Start MCP server (hidden alias for `sentrux mcp`)
    #[arg(long = "mcp", hide = true)]
    mcp_flag: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Enforce architectural rules defined in .sentrux/rules.toml
    Check {
        /// Directory to check
        #[arg(default_value = ".")]
        path: String,

        /// Include untracked, non-ignored Git worktree files in the scan
        #[arg(long)]
        include_untracked: bool,
    },

    /// Structural regression gate — compare against a saved baseline
    Gate {
        /// Save current metrics as the new baseline
        #[arg(long)]
        save: bool,

        /// Directory to gate
        #[arg(default_value = ".")]
        path: String,
    },

    /// Open the GUI with a pre-loaded directory
    Scan {
        /// Directory to visualize
        path: Option<String>,
    },

    /// Start the MCP (Model Context Protocol) server for AI agent integration
    Mcp,

    /// Manage language plugins
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },

    /// Control anonymous aggregate usage analytics
    Analytics {
        #[command(subcommand)]
        action: Option<AnalyticsAction>,
    },

    /// Open browser to purchase / sign in for Sentrux Pro
    Login,

    /// Manage Pro license and plugin
    Pro {
        #[command(subcommand)]
        action: ProAction,
    },
}

#[derive(Subcommand)]
enum ProAction {
    /// Activate Pro with a license key
    Activate {
        /// License key JSON string or path to key file
        key: String,
    },
    /// Show Pro license status
    Status,
    /// Deactivate Pro (remove license + plugin)
    Deactivate,
    /// Update Pro plugin to latest version
    Update,
}

#[derive(Subcommand)]
enum AnalyticsAction {
    /// Turn analytics on
    On,
    /// Turn analytics off
    Off,
}

#[derive(Subcommand)]
enum PluginAction {
    /// List installed plugins
    List,

    /// Install all standard language plugins
    AddStandard,

    /// Install a single language plugin from the plugin registry
    Add {
        /// Plugin name (e.g. "python", "rust")
        name: String,
    },

    /// Remove an installed plugin
    Remove {
        /// Plugin name to remove
        name: String,
    },

    /// Create a new plugin template
    Init {
        /// Language name for the new plugin
        name: String,
    },

    /// Validate a plugin directory
    Validate {
        /// Path to the plugin directory
        dir: String,
    },
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

pub fn run() -> eframe::Result<()> {
    // Initialize license + Pro plugin (reads ~/.sentrux/license.key, loads pro.dylib if valid)
    sentrux_core::license::init();

    // Step 1: Download missing grammar binaries (may overwrite configs with old versions)
    ensure_grammars_installed();

    // Step 2: Sync embedded plugin configs LAST — always wins over downloaded configs.
    // This ensures configs match the binary version even if the grammar tarball
    // included old plugin.toml/tags.scm files.
    sentrux_core::analysis::plugin::sync_embedded_plugins();

    // Non-blocking update check (once per day, background thread)
    app::update_check::check_for_updates_async(env!("CARGO_PKG_VERSION"));

    let cli = Cli::parse();

    // Hidden --mcp flag for backward compat with MCP client configs
    if cli.mcp_flag {
        app::mcp_server::run_mcp_server(None);
        return Ok(());
    }

    match cli.command {
        Some(Command::Check { path, include_untracked }) => {
            std::process::exit(run_check(&path, include_untracked));
        }
        Some(Command::Gate { save, path }) => {
            std::process::exit(run_gate(&path, save));
        }
        Some(Command::Mcp) => {
            app::mcp_server::run_mcp_server(None);
            Ok(())
        }
        Some(Command::Plugin { action }) => {
            run_plugin(action);
            Ok(())
        }
        Some(Command::Analytics { action }) => {
            run_analytics(action);
            Ok(())
        }
        Some(Command::Login) => {
            run_login();
            Ok(())
        }
        Some(Command::Pro { action }) => {
            run_pro(action);
            Ok(())
        }
        Some(Command::Scan { path }) => {
            run_gui(path)
        }
        None => {
            run_gui(cli.path)
        }
    }
}

// ---------------------------------------------------------------------------
// Check
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Analytics
// ---------------------------------------------------------------------------

fn analytics_opt_out_path() -> Option<std::path::PathBuf> {
    sentrux_core::analysis::plugin::plugins_dir()
        .map(|d| d.parent().unwrap().join("telemetry_opt_out"))
}

fn run_login() {
    println!();
    println!("  Sentrux Pro — purchase at https://sentrux.dev/pro");
    println!();
    println!("  After purchase, activate with:");
    println!("    sentrux pro activate <license-key>");
    println!();
    println!("  Or paste your license key file:");
    println!("    sentrux pro activate /path/to/license.key");
    println!();
    // Try to open the browser
    let _ = open_url("https://sentrux.dev/pro");
}

fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    { let _ = std::process::Command::new("open").arg(url).spawn(); }
    #[cfg(target_os = "linux")]
    { let _ = std::process::Command::new("xdg-open").arg(url).spawn(); }
    #[cfg(target_os = "windows")]
    { let _ = std::process::Command::new("cmd").args(["/c", "start", url]).spawn(); }
}

fn run_pro(action: ProAction) {
    match action {
        ProAction::Activate { key } => pro_activate(&key),
        ProAction::Status => pro_status(),
        ProAction::Deactivate => pro_deactivate(),
        ProAction::Update => pro_update(),
    }
}

fn pro_activate(key_input: &str) {
    // key_input is either a JSON string or a path to a file
    let key_json = if key_input.starts_with('{') {
        key_input.to_string()
    } else if std::path::Path::new(key_input).exists() {
        match std::fs::read_to_string(key_input) {
            Ok(content) => content,
            Err(e) => {
                eprintln!("Failed to read key file: {}", e);
                return;
            }
        }
    } else {
        eprintln!("Invalid key: not a JSON string or file path");
        return;
    };

    // Validate the key
    match sentrux_core::license::validate_license(&key_json) {
        Some(license) => {
            // Save to disk
            let dir = match dirs::home_dir() {
                Some(h) => h.join(".sentrux"),
                None => { eprintln!("Cannot find home directory"); return; }
            };
            let _ = std::fs::create_dir_all(&dir);
            let key_path = dir.join("license.key");
            match std::fs::write(&key_path, &key_json) {
                Ok(_) => {
                    println!("License activated!");
                    println!("  User:    {}", license.user);
                    println!("  Tier:    {}", license.tier);
                    println!("  Expires: {}", license.expires);
                    println!("  Saved:   {}", key_path.display());
                    println!();
                    println!("Restart sentrux to enable Pro features.");
                }
                Err(e) => eprintln!("Failed to save license: {}", e),
            }
        }
        None => {
            eprintln!("Invalid or expired license key.");
        }
    }
}

fn pro_status() {
    let tier = sentrux_core::license::current_tier();
    println!("Tier: {}", tier);

    // Try to read and show license details
    if let Some(home) = dirs::home_dir() {
        let key_path = home.join(".sentrux").join("license.key");
        if let Ok(content) = std::fs::read_to_string(&key_path) {
            if let Some(license) = sentrux_core::license::validate_license(&content) {
                println!("User:    {}", license.user);
                println!("Expires: {}", license.expires);
                println!("ID:      {}", license.id);
            } else {
                println!("License: invalid or expired");
            }
        } else {
            println!("License: not found");
        }

        let dylib_name = if cfg!(target_os = "macos") { "pro.dylib" }
            else if cfg!(target_os = "windows") { "pro.dll" }
            else { "pro.so" };
        let dylib_path = home.join(".sentrux").join("pro").join(dylib_name);
        if dylib_path.exists() {
            println!("Plugin:  {} (installed)", dylib_path.display());
        } else {
            println!("Plugin:  not installed");
        }
    }

    if let Some((name, version)) = sentrux_core::pro_registry::plugin_info() {
        println!("Loaded:  {} v{}", name, version);
    }

    if sentrux_core::pro_registry::is_loaded() {
        println!("Status:  Pro features active");
    } else {
        println!("Status:  Free");
    }
}

fn pro_deactivate() {
    if let Some(home) = dirs::home_dir() {
        let key_path = home.join(".sentrux").join("license.key");
        let pro_dir = home.join(".sentrux").join("pro");

        if key_path.exists() {
            let _ = std::fs::remove_file(&key_path);
            println!("License removed.");
        }
        if pro_dir.exists() {
            let _ = std::fs::remove_dir_all(&pro_dir);
            println!("Pro plugin removed.");
        }
        println!("Deactivated. Restart sentrux to return to free mode.");
    }
}

fn pro_update() {
    println!("Pro plugin update: not yet implemented.");
    println!("For now, download the latest pro.dylib from https://sentrux.dev/pro");
    println!("and place it in ~/.sentrux/pro/");
}

fn run_analytics(action: Option<AnalyticsAction>) {
    let path = analytics_opt_out_path();
    match action {
        None => {
            // No subcommand = show state (like `brew analytics`)
            let opted_out = path.as_ref().map_or(false, |p| p.exists());
            if opted_out {
                println!("Analytics are disabled.");
            } else {
                println!("Analytics are enabled.");
            }
        }
        Some(AnalyticsAction::On) => {
            if let Some(p) = &path {
                let _ = std::fs::remove_file(p);
            }
            println!("Analytics are enabled.");
        }
        Some(AnalyticsAction::Off) => {
            if let Some(p) = &path {
                let _ = std::fs::create_dir_all(p.parent().unwrap());
                let _ = std::fs::write(p, "1");
            }
            println!("Analytics are disabled.");
        }
    }
}

// ---------------------------------------------------------------------------
// Check
// ---------------------------------------------------------------------------

/// Run architectural rules check from CLI. Returns exit code.
fn run_check(path: &str, include_untracked: bool) -> i32 {
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

    if include_untracked {
        eprintln!("Scanning {path} including untracked files...");
    } else {
        eprintln!("Scanning {path}...");
    }
    let result = match analysis::scanner::scan_directory_with_options(
        path, None, None,
        &cli_scan_limits(),
        None,
        analysis::scanner::ScanOptions { include_untracked },
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
    println!("Quality: {}\n",
        (health.quality_signal * 10000.0).round() as u32);

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

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

/// Run structural regression gate from CLI. Returns exit code.
fn run_gate(path: &str, save_mode: bool) -> i32 {
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
        None,
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
            println!("Quality: {}",
                (health.quality_signal * 10000.0).round() as u32);
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
    println!("Quality:      {} -> {}",
        (diff.signal_before * 10000.0).round() as u32,
        (diff.signal_after * 10000.0).round() as u32);
    println!("Coupling:     {:.2} → {:.2}", diff.coupling_before, diff.coupling_after);
    println!("Cycles:       {} → {}", diff.cycles_before, diff.cycles_after);
    println!("God files:    {} → {}", diff.god_files_before, diff.god_files_after);

    if !arch_report.distance_metrics.is_empty() {
        println!("\nDistance from Main Sequence: {:.2}", arch_report.avg_distance);
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

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

fn run_plugin(action: PluginAction) {
    match action {
        PluginAction::List => plugin_list(),
        PluginAction::Init { name } => plugin_init(&name),
        PluginAction::Validate { dir } => plugin_validate(&dir),
        PluginAction::AddStandard => plugin_add_standard(),
        PluginAction::Add { name } => plugin_add(&name),
        PluginAction::Remove { name } => plugin_remove(&name),
    }
}

fn plugin_list() {
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
}

fn plugin_init(name: &str) {
    let dir = sentrux_core::analysis::plugin::plugins_dir()
        .unwrap_or_else(|| { eprintln!("Cannot determine home directory"); std::process::exit(1); });
    let plugin_dir = dir.join(name);
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
}

fn plugin_validate(dir: &str) {
    let plugin_dir = std::path::Path::new(dir);
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
}

fn plugin_add_standard() {
    sentrux_core::analysis::plugin::sync_embedded_plugins();
    ensure_grammars_installed();
    println!("Done. All plugins synced from embedded data.");
}

fn plugin_add(name: &str) {
    let dir = sentrux_core::analysis::plugin::plugins_dir()
        .unwrap_or_else(|| { eprintln!("Cannot determine home directory"); std::process::exit(1); });
    let plugin_dir = dir.join(name);
    if plugin_dir.exists() {
        eprintln!("Plugin '{}' already installed at {}", name, plugin_dir.display());
        eprintln!("Remove it first: sentrux plugin remove {}", name);
        std::process::exit(1);
    }

    let platform = sentrux_core::analysis::plugin::manifest::PluginManifest::grammar_filename();
    let platform_key = platform.rsplit_once('.').map_or(platform, |(k, _)| k);

    let version = match sentrux_core::analysis::plugin::embedded::EMBEDDED_PLUGINS
        .iter()
        .find(|&&(n, _, _)| n == name)
        .and_then(|&(_, toml, _)| toml.lines()
            .find(|l| l.starts_with("version"))
            .and_then(|l| l.split('"').nth(1)))
    {
        Some(v) => v,
        None => {
            eprintln!("Plugin '{}' not found in embedded data. Is it a valid plugin name?", name);
            std::process::exit(1);
        }
    };
    let url = format!(
        "https://github.com/sentrux/plugins/releases/download/{name}-v{version}/{name}-{platform_key}.tar.gz"
    );
    println!("Downloading {name} plugin for {platform_key}...");
    println!("  {url}");

    std::fs::create_dir_all(&dir).unwrap();
    let tarball = dir.join(format!("{name}.tar.gz"));
    download_and_extract_plugin(&dir, name, &tarball, &url, &plugin_dir);
}

fn download_and_extract_plugin(
    dir: &std::path::Path,
    name: &str,
    tarball: &std::path::Path,
    url: &str,
    plugin_dir: &std::path::Path,
) {
    let output = std::process::Command::new("curl")
        .args(["-fsSL", url, "-o"])
        .arg(tarball)
        .status();

    match output {
        Ok(s) if s.success() => {
            let extract = std::process::Command::new("tar")
                .args(["xzf", &format!("{}.tar.gz", name)])
                .current_dir(dir)
                .status();
            let _ = std::fs::remove_file(tarball);
            match extract {
                Ok(s) if s.success() => {
                    println!("Installed {} to {}", name, plugin_dir.display());
                }
                _ => {
                    eprintln!("Failed to extract plugin archive");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            let _ = std::fs::remove_file(tarball);
            eprintln!("Failed to download plugin '{}'.", name);
            eprintln!("Check available plugins: https://github.com/sentrux/plugins/releases");
            std::process::exit(1);
        }
    }
}

fn plugin_remove(name: &str) {
    let dir = sentrux_core::analysis::plugin::plugins_dir()
        .unwrap_or_else(|| { eprintln!("Cannot determine home directory"); std::process::exit(1); });
    let plugin_dir = dir.join(name);
    if !plugin_dir.exists() {
        eprintln!("Plugin '{}' not installed.", name);
        std::process::exit(1);
    }
    std::fs::remove_dir_all(&plugin_dir).unwrap();
    println!("Removed plugin '{}'", name);
}

// ---------------------------------------------------------------------------
// GUI
// ---------------------------------------------------------------------------

/// Probe which wgpu backends have usable GPU adapters on this system.
/// Returns only backends that actually have hardware support, avoiding
/// blind attempts that panic on unsupported drivers.
fn probe_available_backends() -> Vec<eframe::wgpu::Backends> {
    let candidates = [
        ("Primary+GL", eframe::wgpu::Backends::PRIMARY | eframe::wgpu::Backends::GL),
        ("GL-only",    eframe::wgpu::Backends::GL),
        ("Primary",    eframe::wgpu::Backends::PRIMARY),
    ];

    let mut available = Vec::new();
    for (label, backends) in &candidates {
        let instance = eframe::wgpu::Instance::new(&eframe::wgpu::InstanceDescriptor {
            backends: *backends,
            ..Default::default()
        });
        let adapters: Vec<_> = instance.enumerate_adapters(eframe::wgpu::Backends::all());
        if !adapters.is_empty() {
            sentrux_core::debug_log!("[gpu] probe {label}: {} adapter(s) found", adapters.len());
            available.push(*backends);
        } else {
            sentrux_core::debug_log!("[gpu] probe {label}: no adapters");
        }
    }
    available
}

fn run_gui(path: Option<String>) -> eframe::Result<()> {
    let initial_path = path
        .map(|p| {
            std::path::Path::new(&p)
                .canonicalize()
                .map(|c| c.to_string_lossy().to_string())
                .unwrap_or(p)
        })
        .filter(|p| std::path::Path::new(p).is_dir());

    // Determine backends: respect user override, otherwise probe hardware.
    let env_backends = eframe::wgpu::Backends::from_env();
    let backend_attempts: Vec<eframe::wgpu::Backends> = if let Some(user_choice) = env_backends {
        // User explicitly chose via WGPU_BACKEND — respect it, no fallback
        vec![user_choice]
    } else {
        let probed = probe_available_backends();
        if probed.is_empty() {
            // No hardware GPU — try software rendering via glow (OpenGL)
            return run_gui_glow(initial_path);
        }
        probed
    };

    let version = env!("CARGO_PKG_VERSION");
    let title = {
        let edition = edition_name();
        if edition.is_empty() {
            format!("sentrux v{}", version)
        } else {
            format!("Sentrux {} v{}", edition, version)
        }
    };
    let title = title.as_str();

    for (i, backends) in backend_attempts.iter().enumerate() {
        sentrux_core::debug_log!("[gpu] attempt {}/{}: backends {:?}", i + 1, backend_attempts.len(), backends);

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1600.0, 1000.0])
                .with_maximized(true)
                .with_title(title),
            renderer: eframe::Renderer::Wgpu,
            wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
                wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(eframe::egui_wgpu::WgpuSetupCreateNew {
                    instance_descriptor: eframe::wgpu::InstanceDescriptor {
                        backends: *backends,
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let path_clone = initial_path.clone();
        // catch_unwind as safety net: wgpu can panic on surface creation
        // even when adapter enumeration succeeded (driver bugs, missing DRI3)
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            eframe::run_native(
                "Sentrux",
                options,
                Box::new(move |cc| Ok(Box::new(app::SentruxApp::new(cc, path_clone)))),
            )
        }));

        match result {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) => {
                sentrux_core::debug_log!("[gpu] backend {:?} failed: {e}", backends);
            }
            Err(_panic) => {
                sentrux_core::debug_log!("[gpu] backend {:?} panicked (driver issue)", backends);
            }
        }

        if i + 1 == backend_attempts.len() {
            // All wgpu backends failed — fall back to glow (software OpenGL)
            return run_gui_glow(initial_path);
        }
    }
    Ok(())
}

/// Fallback GUI using glow (OpenGL) renderer — works on systems without
/// hardware GPU (VMs, RDP, headless servers with software OpenGL).
fn run_gui_glow(initial_path: Option<String>) -> eframe::Result<()> {
    sentrux_core::debug_log!("[gpu] falling back to glow (software OpenGL)");
    let version = env!("CARGO_PKG_VERSION");
    let title = {
        let edition = edition_name();
        if edition.is_empty() {
            format!("sentrux v{}", version)
        } else {
            format!("Sentrux {} v{}", edition, version)
        }
    };
    let title = title.as_str();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_maximized(true)
            .with_title(title),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Sentrux",
        options,
        Box::new(move |cc| Ok(Box::new(app::SentruxApp::new(cc, initial_path)))),
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cli_scan_limits() -> analysis::scanner::common::ScanLimits {
    let s = core::settings::Settings::default();
    analysis::scanner::common::ScanLimits {
        max_file_size_kb: s.max_file_size_kb,
        max_parse_size_kb: s.max_parse_size_kb,
        max_call_targets: s.max_call_targets,
    }
}

/// Ensure grammar binaries are installed for all embedded plugins.
/// Downloads ONE tarball with ALL grammars — not 49 individual downloads.
///
/// Architecture:
///   Each binary release on GitHub includes asset:
///     grammars-darwin-arm64.tar.gz (all grammars in one archive)
///   This function downloads that ONE file and extracts all grammars at once.
///
/// Handles: first launch, upgrade, accidental deletion.
fn ensure_grammars_installed() {
    // CI sets this to prevent overwriting already-installed grammars
    // with a 404 from a version tag that doesn't have grammar assets yet
    if std::env::var("SENTRUX_SKIP_GRAMMAR_DOWNLOAD").is_ok() {
        return;
    }

    let dir = match sentrux_core::analysis::plugin::plugins_dir() {
        Some(d) => d,
        None => return,
    };

    let platform = sentrux_core::analysis::plugin::manifest::PluginManifest::grammar_filename();
    let platform_key = platform.rsplit_once('.').map_or(platform, |(k, _)| k);

    let _ = std::fs::create_dir_all(&dir);

    // Check if ANY grammar is missing
    let any_missing = sentrux_core::analysis::plugin::embedded::EMBEDDED_PLUGINS
        .iter()
        .any(|&(name, toml, _)| {
            toml.contains("[grammar]")
                && !dir.join(name).join("grammars").join(platform).exists()
        });

    if !any_missing {
        return;
    }

    let version = env!("CARGO_PKG_VERSION");
    let url = format!(
        "https://github.com/sentrux/sentrux/releases/download/v{version}/grammars-{platform_key}.tar.gz"
    );
    let tarball = dir.join("grammars.tar.gz");

    eprintln!();
    eprintln!("  Downloading language grammars for v{version}...");
    eprintln!("  (one-time download, ~30MB)");
    eprint!("  [░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░]   0%");
    let _ = std::io::Write::flush(&mut std::io::stderr());

    let ok = std::process::Command::new("curl")
        .args(["-fsSL", "--progress-bar", &url, "-o"])
        .arg(&tarball)
        .stderr(std::process::Stdio::inherit()) // Show curl progress
        .stdout(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if ok {
        // Extract: tarball contains <lang>/grammars/<platform>.dylib for each language
        let extracted = std::process::Command::new("tar")
            .args(["xzf"])
            .arg(&tarball)
            .current_dir(&dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        let _ = std::fs::remove_file(&tarball);

        if extracted {
            // Count how many grammars we now have
            let count = sentrux_core::analysis::plugin::embedded::EMBEDDED_PLUGINS
                .iter()
                .filter(|&&(name, _, _)| dir.join(name).join("grammars").join(platform).exists())
                .count();
            eprintln!("  {count} language grammars ready.");
        } else {
            eprintln!("  Failed to extract grammars archive.");
        }
    } else {
        let _ = std::fs::remove_file(&tarball);
        eprintln!("  Download failed. Check your network and try again.");
        eprintln!("  URL: {url}");
    }
    eprintln!();
}
