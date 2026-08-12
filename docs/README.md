# Hakutaku のドキュメント

このディレクトリは、Hakutaku のプロダクト、要件、設計、開発運用に関する正本です。文書は原則としてこのディレクトリへ置き、GitHub が直接利用する文書とテンプレートだけを `.github/` へ置きます。詳しい基準は[ファイルの配置](development/repository-operations.md#ファイルの配置)を参照してください。

## 最初に読む文書

1. [プロダクトビジョン](product/vision.md) — なぜ作るか
2. [プロダクトスコープ](product/scope.md) — 何を作るか、何を作らないか
3. [用語集](domain/glossary.md) — 同じ言葉で話すための定義
4. [時間モデル](domain/time-model.md) — 時刻、順序、相関の意味
5. [アーキテクチャ概要](architecture/overview.md) — どの境界で構成するか
6. [データ取り扱いとセキュリティ](security/data-handling.md) — 守るデータと原則

## 文書マップ

| 領域 | 文書 | 役割 |
| --- | --- | --- |
| プロダクト | [ビジョン](product/vision.md) | 背景、対象利用者、価値、非目標 |
| プロダクト | [対象範囲](product/scope.md) | MVP の候補、境界、未決事項 |
| プロダクト | [ロードマップ](roadmap.md) | 現在、次、将来の作業と検証条件 |
| プロダクト | [リリースノート](release-notes/README.md) | 版ごとの配布物、対応 WebView2 Runtime 版、導入手順、既知の制約 |
| 導入 | [WebView2 Runtime 導入手順書](deployment/webview2-runtime-installation.md) | 導入担当者向けの Evergreen 導入状況の確認方法、Runtime 追加パッケージの入手・配置手順、責任範囲 |
| 導入 | [エラーコード一覧（導入担当者向け）](deployment/error-codes.md) | 診断ログ `code=` に記録されるアプリ内エラーコードの意味と対処 |
| ドメイン | [用語集](domain/glossary.md) | 用語の意味と使い分け |
| ドメイン | [時間モデル](domain/time-model.md) | 時刻、順序、時計のずれ、相関の意味 |
| 要件 | [機能要件](requirements/functional.md) | 利用者から見える機能要件 |
| 要件 | [品質要件](requirements/quality.md) | 性能、安全性、信頼性などの品質要件 |
| 検証 | [手動での動作確認手順](verification/manual-check.md) | サンプルログの生成と、画面・操作を通した動作確認の手順 |
| 検証 | [段階0検証記録](verification/stage0-results.md) | 段階0検証シナリオの実施結果、不成立・未実施項目の記録 |
| アーキテクチャ | [概要](architecture/overview.md) | 論理構成、データフロー、信頼境界 |
| アーキテクチャ | [コネクター契約](architecture/connector-contract.md) | データソース統合境界の契約 |
| アーキテクチャ | [設計判断](architecture/decisions/README.md) | 重要な設計判断と理由 |
| セキュリティ | [データの取り扱い](security/data-handling.md) | 認証情報、キャッシュ、ログ、外部通信 |
| 開発 | [運用の索引](development/README.md) | 人間・AIのリポジトリ運用と正本一覧 |
| 開発 | [リポジトリ運用規則](development/repository-operations.md) | 役割、タスク状態、権限、例外、正式リリース前の方針 |
| 開発 | [開発ワークフロー](development/workflow.md) | Issue、ブランチ、コミット、PR、レビュー、マージ |
| 開発 | [Windows ビルド互換性](development/windows-build-compatibility.md) | 対象 OS と下限、64 ビット限定、WebView2 Runtime の版、依存追加時の確認 |
| 開発 | [並行セッション](development/concurrent-sessions.md) | worktree、担当範囲、競合、引き継ぎ |
| 開発 | [WebView2 Runtime の解決経路と開発環境での扱い](development/webview2-runtime.md) | Runtime の3経路、`WebView2`／`WebView2Runtime` の違い、設定キー、開発時の配置手順 |
| 開発 | [エラーコード体系](development/error-codes.md) | 診断ログ `code=` の書式、領域の割り当て、採番手順 |
| 開発 | [コードコメント規約](development/code-comments.md) | doc コメント・行内コメントの書き方、要件 ID・ADR・Issue 番号の参照 |

## 関連する入口

- プロジェクト全体の入口: [`README.md`](../README.md)
- 貢献者向け入口: [`.github/CONTRIBUTING.md`](../.github/CONTRIBUTING.md)
- 脆弱性の報告: [`.github/SECURITY.md`](../.github/SECURITY.md)
- AI エージェントの自動検出と規則: [`AGENTS.md`](../AGENTS.md)
- Claude Code の入口（規則は `AGENTS.md` を参照）: [`CLAUDE.md`](../CLAUDE.md)

## 正本の分担

- **ビジョン**: なぜ必要か、誰にどの価値を届けるか
- **対象範囲と要件**: 何を提供し、どこまでを受け入れるか
- **アーキテクチャと ADR**: どの境界で実現し、なぜその判断をしたか
- **ロードマップ**: いつ何を検証するか、優先順位をどう考えるか
- **用語集**: 言葉が何を意味するか

同じ情報を複数文書へ複製せず、正本へリンクしてください。

## 文書ステータス

各設計文書は冒頭に次のメタデータを持ちます。

- `状態`: 草案（`Draft`）、提案中（`Proposed`）、採用済み（`Accepted`）、廃止（`Deprecated`）
- `管理責任者`: 内容の保守責任者。未定の場合は未定（`TBD`）
- `最終更新日`: 意味のある内容変更の日付

草案（`Draft`）の文は現在の作業仮説です。明示的なレビューまたは ADR なしに、確定仕様として扱わないでください。

## 更新ルール

- 振る舞いを変える PR では、関連要件も同時に更新する
- 用語を追加・変更する場合は用語集を更新する
- 複数領域へ影響する、または後戻りが高コストな判断は ADR に残す
- 未決事項は本文へ紛れ込ませず、各文書の「未決事項」に残す
- 機密情報、実際の接続文字列、本番データを文書や画像に含めない
- 人が読む文章は[使用言語と文章の規則](development/repository-operations.md#使用言語と文章)に従い、分かりやすい日本語で書く
