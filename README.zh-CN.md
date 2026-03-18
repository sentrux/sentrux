<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg?v=2">
  <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg?v=2">
  <img alt="sentrux" src="assets/logo-dark.svg?v=2" width="500">
</picture>

<br>

**帮助 AI Agent 闭合反馈回路的传感器。<br>实现代码质量的递归式自我改进。**



[![CI](https://github.com/sentrux/sentrux/actions/workflows/ci.yml/badge.svg)](https://github.com/sentrux/sentrux/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/sentrux/sentrux)](https://github.com/sentrux/sentrux/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)


[English](README.md) | **中文** | [Deutsch](README.de.md) | [日本語](README.ja.md)

[工作原理](#工作原理) · [快速开始](#快速开始) · [MCP 集成](#mcp-服务器) · [规则引擎](#规则引擎) · [Releases](https://github.com/sentrux/sentrux/releases)

</div>

<br>

<div align="center">

![sentrux demo](assets/demo.gif)

</div>

<div align="center">
<sub>实况：Claude Code Opus 4.6 构建 FastAPI 项目。即使 prompt 清晰，质量仅得 6772。</sub>
<br>
<sub>不是 Agent 做不到更好 — 而是没有传感器，它不知道该改进什么。</sub>
</div>

<div align="center">
<img src="assets/screenshot-health.gif" width="360" alt="Quality Signal">
</div>

## 工作原理

<div align="center">
<img src="assets/how-it-works.svg" width="600" alt="How sentrux works">
</div>


## 快速开始

**安装**（macOS · Linux · Windows）

**macOS**
```bash
brew install sentrux/tap/sentrux
```

**Linux**
```bash
curl -fsSL https://raw.githubusercontent.com/sentrux/sentrux/main/install.sh | sh
```

**Windows** — 从 [Releases](https://github.com/sentrux/sentrux/releases) 下载，或：
```
curl -L -o sentrux.exe https://github.com/sentrux/sentrux/releases/latest/download/sentrux-windows-x86_64.exe
```

纯 Rust 实现。单一二进制文件。无运行时依赖。通过 tree-sitter 支持 **52 种语言**。支持 **macOS**、**Linux** 和 **Windows**。

**运行**

```bash
sentrux                    # 打开 GUI——项目的实时 treemap
sentrux /path/to/project   # 打开 GUI 扫描指定目录
sentrux check .            # 检查规则（CI 友好，退出码 0 或 1）
sentrux gate --save .      # Agent 会话前保存基线
sentrux gate .             # 会话后比较——拦截退化
```

**连接到你的 AI Agent（可选）**

通过 [MCP](https://modelcontextprotocol.io) 让你的 Agent 实时访问结构健康状况。

Claude Code：

```
/plugin marketplace add sentrux/sentrux
/plugin install sentrux
```

Cursor / Windsurf / OpenCode / OpenClaw / 任何 MCP 客户端 — 添加到你的 MCP 配置：

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

**从源码构建 / 升级 / 故障排除**

```bash
# 从源码构建
git clone https://github.com/sentrux/sentrux.git
cd sentrux && cargo build --release

# 升级
brew update && brew upgrade sentrux
# 或者重新运行 curl 安装——总是拉取最新版本
```

**Linux GPU 问题？** 如果应用无法启动，sentrux 会自动尝试多个 GPU 后端（Vulkan → GL → 回退）。你也可以手动指定：

```bash
WGPU_BACKEND=vulkan sentrux    # 强制使用 Vulkan
WGPU_BACKEND=gl sentrux        # 强制使用 OpenGL
```

<br>

## 一个没人谈论的问题

你用 Claude Code 或 Cursor 开始一个项目。第一天像魔法一样——Agent 写出干净的代码，理解你的意图，快速交付功能。

然后，某些东西开始变化。

Agent 开始幻想不存在的函数。它把新代码放错了位置。它在昨天刚改过的文件里引入 bug。你要求一个简单的功能，它却搞坏了其他三个地方。你花在修复 Agent 输出上的时间，比自己写还多。

所有人都以为 AI 变笨了。**不是的。** 是你的代码库变烂了。

事情是这样发生的：当你用 IDE 的时候，你能看到文件树，你打开文件，你在脑子里建立了一个架构的心智模型——哪个模块做什么，它们怎么连接，什么东西该放在哪里。你就是那个掌舵者。每一次编辑都经过你对整体的理解。

然后 AI Agent 把我们带到了终端。Agent 每次会话修改几十个文件。你看到的只是一行行 `Modified src/foo.rs`——但你失去了空间感知。你看不到这个文件在依赖图中的位置。你看不到它刚刚创建了一个循环依赖。你看不到三个模块现在都依赖了一个本应是内部的文件。很多开发者让 AI Agent 构建整个应用程序，从头到尾都没有打开过文件浏览器。

**你已经失去了控制。而你甚至还不知道。**

每一次 AI 会话都在悄悄降解你的架构。相同的函数名，不同的功能，散落在不同文件中。不相关的代码堆在同一个文件夹里。依赖关系缠绕成意大利面条。当 Agent 搜索你的项目时，它找到二十个互相矛盾的结果——然后选了错误的那个。每次会话让混乱更严重，每次混乱让下次会话更困难。

这是 AI 辅助开发的一个肮脏秘密：**AI 生成代码越快，你的代码库就越快变得无法治理。**

传统的回答是"先规划好架构，再让 AI 实现"——听起来对，但没抓住重点。像 GitHub 的 [Spec Kit](https://github.com/github/spec-kit) 就试图走这条路：在写代码之前生成详细的规范和计划。但实际使用中，它[重新发明了瀑布流](https://blog.scottlogic.com/2025/11/26/putting-spec-kit-through-its-paces-radical-idea-or-reinvented-waterfall.html)——产出大量 markdown 文档，却对实际生成的代码完全没有可见性。没有反馈回路。无法检测实现何时偏离了规范。没有任何结构分析。规范进去了，Agent 写了代码，没有人检查产出了什么。

而且这也不是任何人实际使用 AI Agent 的方式。你快速原型，通过对话迭代，跟着灵感走，让创意驱动代码。这种创造性的流程正是 AI Agent 强大的原因，也正是它摧毁代码库的原因。

**你不需要更好的计划。你需要更好的传感器。**

## 解决方案

**sentrux 是缺失的反馈回路。**

每一个能规模化运行的系统都有一个：传感器观察现实，规范定义"好"，执行器纠正偏差。编译器闭合了语法层的反馈回路。测试套件闭合了行为层的反馈回路。代码检查器闭合了风格层的反馈回路。

但架构——这个改动是否符合系统设计？这个抽象会不会随着代码增长带来问题？——没有传感器，没有执行器。只有人类能做出这种判断，而人类跟不上机器速度的代码生成。

**sentrux 在架构层闭合了反馈回路。**

它实时监视你的代码库——不是看 diff，不是看终端输出——是*真实的结构*。每一个文件，每一条依赖，每一个架构关系。可视化为一个实时交互式的 treemap，随着 Agent 写代码实时更新。

5 个根因指标。一个连续评分。毫秒级计算。

当架构退化时，你立刻就能看到——而不是两周后一切崩溃、没人记得是哪次会话导致的。

sentrux 给你传感器，你的规则给出规范，Agent 是执行器。**回路闭合了。**

<br>

<div align="center">
<table>
<tr>
<td align="center" width="33%"><b>可视化</b><br><sub>实时 treemap + 依赖边<br>Agent 修改文件时文件会发光</sub></td>
<td align="center" width="33%"><b>度量</b><br><sub>5 个根因指标，0–10000 连续评分：<br>模块化、无环性、深度、均衡性、冗余度</sub></td>
<td align="center" width="33%"><b>治理</b><br><sub>质量门禁拦截退化<br>规则引擎强制约束</sub></td>
</tr>
</table>
</div>

<br>

## MCP 服务器

**Agent 工作流程**

```
Agent: scan("/Users/me/myproject")
  → { quality_signal: 7342, files: 139, bottleneck: "modularity" }

Agent: session_start()
  → { status: "Baseline saved", quality_signal: 7342 }

  ... Agent 写了 500 行代码 ...

Agent: session_end()
  → { pass: false, signal_before: 7342, signal_after: 6891,
      summary: "Quality degraded during this session" }
```

**分析与重构工具**

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

13 个工具：`scan` · `health` · `session_start` · `session_end` · `rescan` · `check_rules` · `git_stats` · `dsm` · `test_gaps` · `top_files` · `file_info` · `impact_analysis` · `suggest_refactoring`

## 规则引擎

定义架构约束。在 CI 中强制执行。让 Agent 知道边界在哪里。

**示例 `.sentrux/rules.toml`**

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
reason = "App 不应依赖 core 内部实现"
```

```bash
sentrux check .
# ✓ 所有规则通过 — Quality: 7342
```

## 支持的语言

52 种语言，通过 [tree-sitter](https://tree-sitter.github.io/) 插件支持：

Bash · C · C++ · C# · Clojure · COBOL · Crystal · CSS · Dart · Dockerfile · Elixir · Erlang · F# · GDScript · GLSL · Go · Groovy · Haskell · HCL · HTML · Java · JavaScript · JSON · Julia · Kotlin · Lua · Markdown · Nim · Nix · Object Pascal · Objective-C · OCaml · Perl · PHP · PowerShell · Protobuf · Python · R · Ruby · Rust · Scala · SCSS · Solidity · SQL · Svelte · Swift · TOML · TypeScript · V · Vue · YAML · Zig

**插件系统** — 添加社区支持的任何语言，或创建自己的：

```bash
sentrux plugin list              # 查看已安装的插件
sentrux plugin add <name>        # 安装社区插件
sentrux plugin init my-lang      # 创建新的语言插件模板
```

插件使用 tree-sitter 语法和简单的查询文件。

缺少某种语言？[提交 issue](https://github.com/sentrux/sentrux/issues) 或提交插件 PR。

---

## 设计哲学

**人的角色正在改变——从写代码到治理代码。**

AI 出现之前就重要的每一个工程实践——文档、测试、编纂的架构决策、快速反馈回路——现在重要性呈指数级增长。跳过测试，反馈回路就无法闭合。跳过架构约束，漂移就以机器速度复合。而这里有一个陷阱：如果 Agent 不知道"干净"长什么样，你就无法用 Agent 来清理混乱。

sentrux 建立在三个信念之上：

**1. 人在回路中不可妥协。** AI Agent 强大但有局限。它们无法同时关注全局和细节。人类必须能在任何时刻看到 Agent 对整体做了什么——不仅仅是它修改了哪个文件，而是这个文件对架构意味着什么。sentrux 让这成为可能。

**2. 验证比生成更有价值。** 生成一个正确的解决方案比验证一个更难（P vs NP 背后的直觉）。你不需要在编码上胜过机器，你需要在评估上胜过它——定义"正确"长什么样，识别输出何时偏离，判断方向是否正确。sentrux 把架构判断力转化为机器可读的评分和约束。

**3. 好的体系让好的结果成为必然。** 一个设计良好的系统约束行为，让正确的事情成为容易的事情。一个质量门禁在退化发布前拦截它。一个规则引擎编纂你的架构决策。一个可视化地图让结构腐化无处遁形。实践没有变。忽视它们的代价已经变得不可承受。

*一旦你有了有效的反馈回路，你不会再回去手动操作。不是因为你不能，是因为那已经没有意义了。*

---

<div align="center">

<sub>AI Agent 以机器速度写代码。没有结构治理，代码库也以机器速度腐化。<br><b>sentrux 就是那个调速器。</b></sub>

</div>

<div align="center">

[MIT License](LICENSE)

</div>
