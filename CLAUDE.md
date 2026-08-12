# Claude Code 向けの入口

このファイルは、Claude Code が自動検出するための入口です。規則の正本ではありません。

**Hakutaku の AI エージェント規則の正本は、ルートの [`AGENTS.md`](AGENTS.md) です。作業を始める前に全文を読み、そのとおりに従ってください。** 同じ規則をこのファイルへ複製しません。規則を変更する場合は `AGENTS.md` と各正本文書を更新し、このファイルは入口のまま保ちます。

## 最初に行うこと

1. [`AGENTS.md`](AGENTS.md) を全文読む（必読文書、権限の境界、セッションの流れ、停止条件を含む）
2. [`README.md`](README.md) でプロジェクトの現在状態を確認する
3. [`docs/README.md`](docs/README.md) でタスクに対応する正本を選ぶ
4. 変更作業では [`.github/CONTRIBUTING.md`](.github/CONTRIBUTING.md) と [`docs/development/README.md`](docs/development/README.md) を読む

## Claude Code 固有の注意

- 応答、コミットメッセージ、Issue / PR、文書は、[使用言語と文章の規則](docs/development/repository-operations.md#使用言語と文章)に従い、原則として分かりやすい日本語で書く
- コミット、リモート送信、Issue / PR の操作、破壊的操作は、[`AGENTS.md` の「権限の境界」](AGENTS.md#権限の境界)に従い、利用者の明示的な依頼がある場合だけ行う
- サブエージェント（Task ツール）は、[`AGENTS.md` の「サブエージェントの手順」](AGENTS.md#サブエージェントの手順)に従い、書き込み範囲をファイル単位で一意に割り当てる
- 並行作業と worktree は[並行セッション](docs/development/concurrent-sessions.md)に従う。Codex を含む他セッションの worktree、ブランチ、未コミット変更を操作しない
- リポジトリ共通の Claude Code 設定は `.claude/settings.json` に置き、個人設定は `.claude/settings.local.json`（Git 管理外）に置く。認証情報、個人のモデル・承認設定、MCP の秘密情報をコミットしない
- ビルド、テスト、静的解析、整形の標準コマンドは[開発ワークフローの「段階0の標準検査」](docs/development/workflow.md#段階0の標準検査)にあります。実行していないコマンドの結果を装わず、実施した確認と未実施項目を報告します
- ファイル検索は Glob ツール、内容検索は Grep ツール、ファイルの読み取りは Read ツールを使います。**Bash から `find` や `grep` を実行しません。** リポジトリ外を起点とする広域の探索は、他の作業を妨げるため行いません
