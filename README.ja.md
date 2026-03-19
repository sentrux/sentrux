<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg?v=2">
  <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg?v=2">
  <img alt="sentrux" src="assets/logo-dark.svg?v=2" width="500">
</picture>

<br>

**AIエージェントのフィードバックループを閉じるセンサー。<br>コード品質の再帰的自己改善を実現。**



[![CI](https://github.com/sentrux/sentrux/actions/workflows/ci.yml/badge.svg)](https://github.com/sentrux/sentrux/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/sentrux/sentrux)](https://github.com/sentrux/sentrux/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)


[English](README.md) | [中文](README.zh-CN.md) | [Deutsch](README.de.md) | **日本語**

[仕組み](#仕組み) · [クイックスタート](#クイックスタート) · [MCP](#mcpサーバー) · [ルールエンジン](#ルールエンジン) · [Releases](https://github.com/sentrux/sentrux/releases)

</div>

<br>

<div align="center">

![sentrux デモ](assets/demo.gif)

</div>

<div align="center">
<sub>実況：Claude Code Opus 4.6 が FastAPI プロジェクトを構築。良いプロンプトでも品質は 6772 止まり。</sub>
<br>
<sub>エージェントの能力不足ではない — センサーがなければ、何を改善すべきか分からないだけ。</sub>
</div>

<div align="center">
<img src="assets/screenshot-health.gif" width="360" alt="Quality Signal">
</div>

## 仕組み

<div align="center">
<img src="assets/how-it-works.svg" width="600" alt="How sentrux works">
</div>


## クイックスタート

**インストール**（macOS · Linux · Windows）

**macOS**
```bash
brew install sentrux/tap/sentrux
```

**Linux**
```bash
curl -fsSL https://raw.githubusercontent.com/sentrux/sentrux/main/install.sh | sh
```

**Windows** — [Releases](https://github.com/sentrux/sentrux/releases)からダウンロード、または：
```
curl -L -o sentrux.exe https://github.com/sentrux/sentrux/releases/latest/download/sentrux-windows-x86_64.exe
```

Pure Rust。単一バイナリ。ランタイム依存なし。tree-sitterプラグインで**52言語**対応。**macOS**、**Linux**、**Windows**対応。

**実行する**

```bash
sentrux                    # GUIを開く——プロジェクトのライブtreemap
sentrux /path/to/project   # GUIを開き指定ディレクトリをスキャン
sentrux check .            # ルールチェック（CI対応、終了コード0または1）
sentrux gate --save .      # エージェントセッション前にベースラインを保存
sentrux gate .             # セッション後に比較——退行を検出
```

**AIエージェントへの接続（任意）**

[MCP](https://modelcontextprotocol.io) 経由で、エージェントに構造的健全性へのリアルタイムアクセスを提供する。

Claude Code:

```
/plugin marketplace add sentrux/sentrux
/plugin install sentrux
```

Cursor / Windsurf / OpenCode / OpenClaw / その他のMCPクライアント — MCP設定に追加：

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

**ソースからビルド / アップグレード / トラブルシューティング**

```bash
# ソースからビルド
git clone https://github.com/sentrux/sentrux.git
cd sentrux && cargo build --release

# アップグレード
brew update && brew upgrade sentrux
# またはcurlインストールを再実行——常に最新版を取得
```

**Linux GPUの問題？** アプリが起動しない場合、sentrux は自動的に複数のGPUバックエンド（Vulkan → GL → フォールバック）を試行する。手動で指定することも可能：

```bash
WGPU_BACKEND=vulkan sentrux    # Vulkan を強制
WGPU_BACKEND=gl sentrux        # OpenGL を強制
```

<br>

## 誰も語らない問題

Claude Code や Cursor でプロジェクトを始める。初日は魔法のようだ。エージェントはクリーンなコードを書き、意図を理解し、機能を素早く提供する。

そして、何かが変わり始める。

エージェントは存在しない関数を幻覚し始める。新しいコードを間違った場所に置く。昨日触ったばかりのファイルにバグを入れる。簡単な機能を頼むと、他の3つが壊れる。エージェントの出力を修復する時間の方が、自分で書くより長くなる。

みんなAIが劣化したと思う。**違う。** コードベースが劣化したのだ。

実際に起きていたこと：IDEを使っていた頃、あなたはファイルツリーが見えた。ファイルを開いた。アーキテクチャのメンタルモデルを構築していた——どのモジュールが何をするか、どう接続されているか、何がどこに属するか。あなたがガバナーだった。すべての編集は、全体への理解を通過していた。

そしてAIエージェントが私たちをターミナルに移した。エージェントはセッションごとに数十のファイルを変更する。`Modified src/foo.rs` というストリームが見える——しかし空間認識は失われた。そのファイルが依存グラフのどこにあるか見えない。循環依存が生まれたことも見えない。3つのモジュールが内部用のファイルに依存し始めたことも見えない。多くの開発者がファイルブラウザを一度も開かずにAIエージェントにアプリケーション全体を構築させている。

**あなたはすでにコントロールを失っている。そしてそれにまだ気づいていない。**

すべてのAIセッションがアーキテクチャを静かに劣化させる。同じ関数名、異なる目的、ファイルに散乱。無関係なコードが同じフォルダに投げ込まれる。依存関係がスパゲッティに絡まる。エージェントがプロジェクトを検索すると、矛盾する20件の結果が返り——間違ったものを選ぶ。セッションごとに混乱が深まり、混乱が深まるごとに次のセッションが難しくなる。

これがAI支援開発の汚い秘密だ：**AIがコードを生成するのが上手になるほど、コードベースは速く統治不能になる。**

従来の答え——*「先にアーキテクチャを設計してから、AIに実装させる」*——は正しく聞こえるが的を外している。GitHubの [Spec Kit](https://github.com/github/spec-kit) のようなツールはまさにこのアプローチを試みている：コードを書く前に詳細な仕様と計画を生成する。しかし実際には、[ウォーターフォールの再発明](https://blog.scottlogic.com/2025/11/26/putting-spec-kit-through-its-paces-radical-idea-or-reinvented-waterfall.html)になってしまう——大量のmarkdownドキュメントを生成しながら、実際に生成されたコードへの可視性はゼロ。フィードバックループなし。実装が仕様から乖離したことを検出する手段なし。構造分析は一切なし。仕様が入り、エージェントがコードを書き、何が出てきたかは誰もチェックしない。

そもそも、誰もAIエージェントをそうやって使っていない。素早くプロトタイプを作る。会話で反復する。インスピレーションに従う。創造的な流れにコードを導かせる。その創造的な流れこそがAIエージェントを強力にするものであり、コードベースを破壊するものでもある。

**必要なのはより良い計画ではない。より良いセンサーだ。**

## 解決策

**sentrux は失われたフィードバックループだ。**

スケールするすべてのシステムには一つある：現実を観察するセンサー、「良い」を定義する仕様、そして乖離を修正するアクチュエータ。コンパイラは構文のフィードバックループを閉じる。テストスイートは振る舞いのループを閉じる。リンターはスタイルのループを閉じる。

しかしアーキテクチャ——この変更はシステムに合っているか？この抽象化は成長とともに問題を起こすか？——にはセンサーもアクチュエータもなかった。人間だけがそれを判断でき、人間は機械速度のコード生成についていけない。

**sentrux はアーキテクチャレベルでループを閉じる。**

コードベースをリアルタイムで監視する——diffではなく、ターミナル出力でもなく——*実際の構造*を。すべてのファイル、すべての依存関係、すべてのアーキテクチャ上の関係。エージェントがコードを書くにつれて更新されるライブインタラクティブtreemapとして可視化。

5つの根本原因指標。一つの連続スコア。ミリ秒で計算。

アーキテクチャが劣化したとき、すぐにわかる——2週間後にすべてが壊れて、どのセッションが原因だったか誰も覚えていない、ということにはならない。

sentrux がセンサーを提供する。ルールが仕様を定める。エージェントがアクチュエータだ。**ループが閉じる。**

<br>

<div align="center">
<table>
<tr>
<td align="center" width="33%"><b>可視化</b><br><sub>依存関係エッジ付きライブtreemap<br>エージェントが変更するとファイルが光る</sub></td>
<td align="center" width="33%"><b>計測</b><br><sub>5つの根本原因指標、0–10000 連続スコア：<br>モジュール性、非循環性、深度、均等性、冗長性</sub></td>
<td align="center" width="33%"><b>統治</b><br><sub>品質ゲートが退行を検出<br>ルールエンジンが制約を強制</sub></td>
</tr>
</table>
</div>

<br>

## MCPサーバー

**エージェントワークフロー**

```
Agent: scan("/Users/me/myproject")
  → { quality_signal: 7342, files: 139, bottleneck: "modularity" }

Agent: session_start()
  → { status: "Baseline saved", quality_signal: 7342 }

  ... エージェントが500行のコードを書く ...

Agent: session_end()
  → { pass: false, signal_before: 7342, signal_after: 6891,
      summary: "Quality degraded during this session" }
```

**分析・リファクタリングツール**

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

13ツール：`scan` · `health` · `session_start` · `session_end` · `rescan` · `check_rules` · `git_stats` · `dsm` · `test_gaps` · `top_files` · `file_info` · `impact_analysis` · `suggest_refactoring`

## ルールエンジン

アーキテクチャ制約を定義。CIで強制。エージェントに境界を伝える。

**例：`.sentrux/rules.toml`**

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
reason = "App は core の内部に依存してはならない"
```

```bash
sentrux check .
# ✓ すべてのルールに合格 — Quality: 7342
```

## 対応言語

[tree-sitter](https://tree-sitter.github.io/) プラグインで52言語対応：

Bash · C · C++ · C# · Clojure · COBOL · Crystal · CSS · Dart · Dockerfile · Elixir · Erlang · F# · GDScript · GLSL · Go · Groovy · Haskell · HCL · HTML · Java · JavaScript · JSON · Julia · Kotlin · Lua · Markdown · Nim · Nix · Object Pascal · Objective-C · OCaml · Perl · PHP · PowerShell · Protobuf · Python · R · Ruby · Rust · Scala · SCSS · Solidity · SQL · Svelte · Swift · TOML · TypeScript · V · Vue · YAML · Zig

**プラグインシステム** — コミュニティ対応の言語を追加、または独自に作成：

```bash
sentrux plugin list              # インストール済みプラグインを表示
sentrux plugin add <name>        # コミュニティプラグインをインストール
sentrux plugin init my-lang      # 新しい言語プラグインを作成
```

プラグインはtree-sitterグラマーとシンプルなクエリファイルを使用。

言語が足りない？[issueを作成](https://github.com/sentrux/sentrux/issues)またはプラグインPRを提出。

---

## 設計思想

**人間の役割は変わりつつある——コードを書くことから、コードを統治することへ。**

AI以前に重要だったすべてのエンジニアリングプラクティス——ドキュメント、テスト、成文化されたアーキテクチャ、高速なフィードバックループ——が今や指数関数的に重要になっている。テストを省けばフィードバックループは閉じられない。アーキテクチャ制約を省けばドリフトが機械速度で複合する。そしてここに罠がある：エージェントが「きれい」とは何かを知らなければ、エージェントを使って混乱を片付けることはできない。

sentrux は3つの信念の上に構築されている：

**1. Human-in-the-loop は譲れない。** AIエージェントは強力だが限界がある。全体像と細部を同時に把握できない。人間はいつでも、エージェントが全体に対して何をしているかを見られなければならない——どのファイルを変更したかだけでなく、そのファイルがアーキテクチャにとって何を意味するかを。sentrux がそれを可能にする。

**2. 検証は生成より価値がある。** 正しい解を生成することは、検証することより難しい（P vs NP の背後にある直感）。機械をコーディングで上回る必要はない。評価で上回ればいい——「正しい」とはどういう状態かを定義し、出力がずれたときに認識し、方向が正しいかを判断する。sentrux はアーキテクチャの判断力を機械可読な評価と制約に変換する。

**3. 良いシステムは良い結果を必然にする。** うまく設計されたシステムは、正しいことが簡単なことになるよう行動を制約する。劣化をリリース前にブロックする品質ゲート。アーキテクチャ決定をコード化するルールエンジン。構造的腐敗を見逃せなくする可視化マップ。プラクティスは変わっていない。それを無視するペナルティが耐えられないものになっただけだ。

*機能するフィードバックループを手に入れたら、手動作業には戻らない。できないからではない。もはや意味がないからだ。*

---

<div align="center">

<sub>AIエージェントは機械速度でコードを書く。構造的ガバナンスなしには、コードベースも機械速度で腐敗する。<br><b>sentrux がガバナーだ。</b></sub>

</div>

<div align="center">

[MIT License](LICENSE)

</div>
