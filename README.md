<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg?v=2">
  <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg?v=2">
  <img alt="sentrux" src="assets/logo-dark.svg?v=2" width="500">
</picture>

<br>

**The sensor that helps AI agents close the feedback loop.<br>Recursive self-improvement of code quality.**


[![CI](https://github.com/sentrux/sentrux/actions/workflows/ci.yml/badge.svg)](https://github.com/sentrux/sentrux/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/sentrux/sentrux)](https://github.com/sentrux/sentrux/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)


**English** | [中文](README.zh-CN.md) | [Deutsch](README.de.md) | [日本語](README.ja.md)

[How it Works](#how-it-works) · [Quick Start](#quick-start) · [MCP Integration](#mcp-server) · [Rules Engine](#rules-engine) · [Releases](https://github.com/sentrux/sentrux/releases)

</div>

<br>

<div align="center">

![sentrux demo](assets/demo.gif)

</div>

<div align="center">
<sub>Live: Claude Code Opus 4.6 builds a FastAPI project. Even with good prompts, quality lands at 6772.</sub>
<br>
<sub>Not because the agent can't do better — but because without a sensor, it doesn't know what to improve.</sub>
</div>

<div align="center">
<img src="assets/screenshot-health.gif" width="360" alt="Quality Signal">
</div>

## How it works

<div align="center">
<img src="assets/how-it-works.svg" width="600" alt="How sentrux works: scan → score → agent improves → rescan → better score → repeat">
</div>


## Quick Start

**Install** (macOS · Linux · Windows)

**macOS**
```bash
brew install sentrux/tap/sentrux
```

**Linux**
```bash
curl -fsSL https://raw.githubusercontent.com/sentrux/sentrux/main/install.sh | sh
```

**Windows** — download from [Releases](https://github.com/sentrux/sentrux/releases), or:
```
curl -L -o sentrux.exe https://github.com/sentrux/sentrux/releases/latest/download/sentrux-windows-x86_64.exe
```

Pure Rust. Single binary. No runtime dependencies. **52 languages** via tree-sitter plugins. Runs on **macOS**, **Linux**, and **Windows**.

**Run it**

```bash
sentrux                    # open the GUI — live treemap of your project
sentrux /path/to/project   # open GUI scanning a specific directory
sentrux check .            # check rules (CI-friendly, exits 0 or 1)
sentrux gate --save .      # save baseline before agent session
sentrux gate .             # compare after — catches degradation
```

**Connect to your AI agent (optional)**

Give your agent real-time access to structural health via [MCP](https://modelcontextprotocol.io).

Claude Code:

```
/plugin marketplace add sentrux/sentrux
/plugin install sentrux
```

Cursor / Windsurf / OpenCode / OpenClaw / any MCP client — add to your MCP config:

```json
{
  "mcpServers": {
    "sentrux": {
      "command": "sentrux",
      "args": ["--mcp"]
    }
  }
}
```

**From source / upgrade / troubleshooting**

```bash
# Build from source
git clone https://github.com/sentrux/sentrux.git
cd sentrux && cargo build --release

# Upgrade
brew update && brew upgrade sentrux
# or re-run the curl install — it always pulls the latest release
```

**Linux GPU issues?** If the app won't start, sentrux automatically tries multiple GPU backends (Vulkan → GL → fallback). You can also force one:

```bash
WGPU_BACKEND=vulkan sentrux    # force Vulkan
WGPU_BACKEND=gl sentrux        # force OpenGL
```

<br>

## The problem nobody talks about

You start a project with Claude Code or Cursor. Day one is magic. The agent writes clean code, understands your intent, ships features fast.

Then something shifts.

The agent starts hallucinating functions that don't exist. It puts new code in the wrong place. It introduces bugs in files it touched yesterday. You ask for a simple feature and it breaks three other things. You're spending more time fixing the agent's output than writing it yourself.

Everyone assumes the AI got worse. **It didn't.** Your codebase did.

Here's what actually happened: when you used an IDE, you saw the file tree. You opened files. You built a mental model of the architecture — which module does what, how they connect, where things belong. You were the governor. Every edit passed through your understanding of the whole.

Then AI agents moved us to the terminal. The agent modifies dozens of files per session. You see a stream of `Modified src/foo.rs` — but you've lost the spatial awareness. You don't see where that file sits in the dependency graph. You don't see that it just created a cycle. You don't see that three modules now depend on a file that was supposed to be internal. Many developers let AI agents build entire applications without ever opening the file browser.

**You've lost control. And you don't even know it yet.**

Every AI session silently degrades your architecture. Same function names, different purposes, scattered across files. Unrelated code dumped in the same folder. Dependencies tangling into spaghetti. When the agent searches your project, it finds twenty conflicting matches — and picks the wrong one. Every session makes the mess worse. Every mess makes the next session harder.

This is the dirty secret of AI-assisted development: **the better the AI generates code, the faster your codebase becomes ungovernable.**

The traditional answer — *"plan your architecture first, then let AI implement"* — sounds right but misses the point. Tools like GitHub's [Spec Kit](https://github.com/github/spec-kit) try this approach: generate detailed specs and plans before writing code. But in practice, it [reinvents waterfall](https://blog.scottlogic.com/2025/11/26/putting-spec-kit-through-its-paces-radical-idea-or-reinvented-waterfall.html) — producing seas of markdown documents while having zero visibility into the code that actually gets produced. No feedback loop. No way to detect when the implementation drifts from the spec. No structural analysis of any kind. The spec goes in, the agent writes code, and nobody checks what came out.

That's not how anyone actually works with AI agents anyway. You prototype fast. You iterate through conversation. You follow inspiration. You let the creative flow drive the code. That creative flow is exactly what makes AI agents powerful. And it's exactly what destroys codebases.

**You don't need a better plan. You need a better sensor.**

## The solution

**sentrux is the missing feedback loop.**

Every system that works at scale has one: a sensor that observes reality, a spec that defines "good," and an actuator that corrects drift. Compilers close a feedback loop on syntax. Test suites close a loop on behavior. Linters close a loop on style.

But architecture — does this change fit the system? will this abstraction cause problems as the codebase grows? — had no sensor and no actuator. Only humans could judge that. And humans can't keep up with machine-speed code generation.

**sentrux closes the loop at the architecture level.**

It watches your codebase in real-time — not the diffs, not the terminal output — the *actual structure*. Every file. Every dependency. Every architectural relationship. Visualized as a live interactive treemap that updates as the agent writes code.

5 root cause metrics. One continuous score. Computed in milliseconds.

When architecture degrades, you see it immediately — not two weeks later when everything is broken and nobody remembers which session caused it.

sentrux gives you the sensor. Your rules give you the spec. The agent is the actuator. **The loop closes.**

<br>

<div align="center">
<table>
<tr>
<td align="center" width="33%"><b>Visualize</b><br><sub>Live treemap with dependency edges,<br>files glow when the agent modifies them</sub></td>
<td align="center" width="33%"><b>Measure</b><br><sub>5 root cause metrics, one score 0–10000:<br>modularity, acyclicity, depth, equality, redundancy</sub></td>
<td align="center" width="33%"><b>Govern</b><br><sub>Quality gate catches regression.<br>Rules engine enforces constraints.</sub></td>
</tr>
</table>
</div>

<br>

## MCP server

**Agent workflow**

```
Agent: scan("/Users/me/myproject")
  → { quality_signal: 7342, files: 139, bottleneck: "modularity" }

Agent: session_start()
  → { status: "Baseline saved", quality_signal: 7342 }

  ... agent writes 500 lines of code ...

Agent: session_end()
  → { pass: false, signal_before: 7342, signal_after: 6891,
      summary: "Quality degraded during this session" }
```

**Analysis & refactoring tools**

```
Agent: top_files(metric="risk", limit=3)
  → { files: [{ path: "src/db/conn.rs", risk_score: 87400, fan_in: 20, churn: 19 }, ...] }

Agent: file_info(path="src/db/conn.rs")
  → { functions: [...], dependencies: { fan_in: 20, fan_out: 3, instability: 1300 },
      issues: { is_hotspot: true, complex_functions: 2 }, git_history: { churn: {...} } }

Agent: impact_analysis(path="src/db/conn.rs")
  → { severity: "critical", transitive_impact: { total_affected: 46, blast_radius_pct: 4600 },
      bidirectional_coupled: { count: 1 }, change_coupling: { pairs: [...] } }

Agent: suggest_refactoring(scope="src/db")
  → { suggestions: [{ category: "split_file", action: "Split 'src/db/conn.rs' into ...",
       reason: "God file: fan-out=12, 8 functions", estimated_impact: "..." }] }
```

13 tools: `scan` · `health` · `session_start` · `session_end` · `rescan` · `check_rules` · `git_stats` · `dsm` · `test_gaps` · `top_files` · `file_info` · `impact_analysis` · `suggest_refactoring`

## Rules engine

Define architectural constraints. Enforce them in CI. Let the agent know the boundaries.

**Example `.sentrux/rules.toml`**

```toml
[constraints]
max_cycles = 0
max_coupling = "B"
max_cc = 25
no_god_files = true

[[layers]]
name = "core"
paths = ["src/core/*"]
order = 0

[[layers]]
name = "app"
paths = ["src/app/*"]
order = 2

[[boundaries]]
from = "src/app/*"
to = "src/core/internal/*"
reason = "App must not depend on core internals"
```

```bash
sentrux check .
# ✓ All rules pass — Quality: 7342
```

## Supported languages

**52 languages** built-in via [tree-sitter](https://tree-sitter.github.io/) plugins — zero language knowledge in the binary:

| | | | | | |
|---|---|---|---|---|---|
| Bash | C | C++ | C# | Clojure | COBOL |
| Crystal | CSS | Dart | Dockerfile | Elixir | Erlang |
| F# | GDScript | GLSL | Go | Groovy | Haskell |
| HCL | HTML | Java | JavaScript | JSON | Julia |
| Kotlin | Lua | Markdown | Nim | Nix | Objective-C |
| Object Pascal | OCaml | Perl | PHP | PowerShell | Protobuf |
| Python | R | Ruby | Rust | Scala | SCSS |
| Solidity | SQL | Svelte | Swift | TOML | TypeScript |
| V | Vue | YAML | Zig | | |

**Plugin system** — add any language, or create your own:

```bash
sentrux plugin list              # see installed plugins
sentrux plugin add <name>        # install from registry
sentrux plugin add-standard      # install all 52 languages
sentrux plugin init my-lang      # scaffold a new language plugin
```

Architecture: the binary is a **generic platform** — all language knowledge lives in `plugin.toml` + `tags.scm` query files. Adding a new language requires zero Rust code.

Missing a language? [Open an issue](https://github.com/sentrux/sentrux/issues) or add a plugin to [`plugins/`](plugins/).

---

## Philosophy

**The human role is changing — from writing code to governing code.**

Every engineering practice that mattered before AI — documentation, testing, codified architecture, fast feedback loops — now matters exponentially more. Skip the tests and the feedback loop can't close. Skip the architectural constraints and drift compounds at machine speed. And here's the trap: you can't use agents to clean up the mess if the agents don't know what clean looks like.

sentrux is built on three beliefs:

**1. Human-in-the-loop is non-negotiable.** AI agents are powerful but limited. They cannot hold the big picture and the small details at the same time. A human must be able to see, at any moment, what the agent is doing to the whole — not just which file it touched, but what that file means to the architecture. sentrux makes that possible.

**2. Verification is more valuable than generation.** Generating a correct solution is harder than verifying one (the intuition behind P vs NP). You don't need to out-code the machine. You need to out-evaluate it — specify what "correct" looks like, recognize when the output misses, judge whether the direction is right. sentrux turns architectural judgment into machine-readable grades and constraints.

**3. Good systems make good outcomes inevitable.** A well-designed system constrains behavior so that the right thing is the easy thing. A quality gate that blocks degradation before it ships. A rules engine that encodes your architectural decisions. A visual map that makes structural rot impossible to ignore. The practices haven't changed. The penalty for ignoring them has become unbearable.

*Once you have a feedback loop that works, you don't go back to doing it by hand. Not because you can't. Because it no longer makes sense.*

---

<div align="center">

<sub>AI agents write code at machine speed. Without structural governance, codebases decay at machine speed too.<br><b>sentrux is the governor.</b></sub>

</div>

<div align="center">

[MIT License](LICENSE)

</div>
