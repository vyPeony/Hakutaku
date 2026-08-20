# Hakutaku

Hakutaku は、ログ、データベース、Web API、構造化ファイルなど、開発プロダクトに散在するデータを一つの場所で読み、探索し、関連付けるための開発用ビューアです。

> [!NOTE]
> 初期リリースの対象はテキストログのビューアです。段階0（現行の開発環境）での実装と検証を終え、段階1（Windows 10 IoT Enterprise LTSC 実機）の検証が残っています。実装技術（Rust + Tauri 2系）は**暫定採用**であり、確定は段階1の完了時です（[ADR-0001](docs/architecture/decisions/0001-rust-tauri-provisional-adoption.md)、`VER-001`）。正式リリース前のため、リリースブランチ、タグ、配布物の公開は行っていません。

## 解決したい課題

障害調査や動作確認では、複数のツールを行き来しながら、形式の異なるデータを人手で突き合わせる必要があります。Hakutaku は、データソースごとの差異を保ちながら、共通の操作と文脈で閲覧できる状態を目指します。

想定する利用例:

- アプリケーションログとデータベースのレコードを時刻や識別子で追跡する
- Web API の応答とローカルファイルの内容を同じ作業領域で比較する
- 大きなデータを段階的に読み込み、検索・絞り込み・詳細確認を行う
- 接続や解析の一部が失敗しても、取得済みデータと診断情報を確認する

## プロダクト原則

以下は [プロダクトビジョン](docs/product/vision.md) の要約です。原則の正本は同文書で管理します。

- **閲覧対象データを変更しない**: 作成・更新・削除機能を持たず、調査による影響を最小化する
- **元データへ戻れる**: 表示や正規化の後も、出所と原文を追跡できる
- **段階的に理解できる**: 概要から詳細、共通属性からデータソース固有の内容へ掘り下げられる
- **部分失敗を隔離する**: 一つのデータソースの失敗で他のデータや作業状態を失わない
- **秘密を露出しない**: 認証情報と閲覧データの保存・表示・ログ出力を最小化する
- **測ってから決める**: 技術や構造は、代表ユースケースと品質要件で検証して選ぶ

## 現在の状態

初期リリース（テキストログのビューア）に必要な機能は、段階0での実装と検証を終えています。段階1（LTSC 実機での検証と技術選定の確定）は未着手です。

- 段階0の検証結果と、未実施項目の引き継ぎ: [段階0検証記録](docs/verification/stage0-results.md)
- 段階0から段階1、初期リリース後までの順序とゲート: [ロードマップ](docs/roadmap.md)
- 進捗、受け入れ条件、担当: GitHub Issue / PR で管理

**段階0で計測した性能値は参考値です。** `VER-005` により合否判定へ使いません。機能の判定には、プロセスのメモリ量ではなく内部状態（保持行数・保持バイト数、破棄と再取得の回数、ヒープ会計値）を使っています。

実装済みの機能は次のとおりです。

| 領域 | 内容 |
| --- | --- |
| 起動 | WebView2 Runtime の3経路の解決、実行ファイル直下の `logs`／`temp`／`WebView2` の用意と清掃、診断ログ |
| 設定 | `hakutaku.yaml` の起動時検証と3つの起動経路（正常起動、既定値起動、安全モード） |
| メモリ会計 | グローバルアロケータの計装、大きな確保の前の予約と拒否、ソフトしきい値、参考指標の計測 |
| ログ解析 | 6書式の日時解析と精度の保持、文字コードの判定とデコード、解析プロファイルの解決、継続行の結合 |
| 大容量読み込み | チャンク読み込みと行索引、ファイル数・サイズ・合計の上限判定、変更検知、共有違反の経路、明示的な再読み込み、進捗とキャンセル |
| 表示 | 仮想スクロールと保持上限の運用、原文と元の精度の保持、未確定行と日時未解析行の区別、行番号ジャンプ |
| 時系列統合 | 複数ファイルの時刻順マージと表示の切り替え（UI からは一時無効。表示品質の改修は [Issue #82](https://github.com/vyPeony/Hakutaku/issues/82)、無効化の経緯は [Issue #83](https://github.com/vyPeony/Hakutaku/issues/83)） |
| コピー | 行の選択（クリック、範囲、飛び飛び、ドラッグ、全選択）、上限判定と拒否、原文そのままのクリップボードコピー |
| 対象端末での運用 | 非昇格での起動、アクセス拒否時の昇格経路、資源抑制の設定、安全な停止 |

初期リリースに**含めない**ものは、SQLite・DICOM ビューア、検索と絞り込み、索引キャッシュ、ドラッグ＆ドロップによる追加です。これらは後続リリースで扱います（[Issue #13](https://github.com/vyPeony/Hakutaku/issues/13)）。

## コードの構成

コードは Cargo ワークスペースとして、GUI に依存しないコアクレート群（[`crates/`](crates/)）と、Tauri コマンドの薄い層（[`src-tauri/`](src-tauri/)）に分かれています。解析、読み込み、会計はすべてコア側にあり、GUI 層は表示と入力の受け渡しに限定しています。フロントエンド（[`src/`](src/)）は、フレームワークとバンドラーを使わない素の ES モジュールです（[ADR-0006](docs/architecture/decisions/0006-frontend-vanilla-es-modules.md)）。

Tauri を初期化する前には、Rust 側のブートストラップ処理（[`src-tauri/src/bootstrap/`](src-tauri/src/bootstrap/)）が動きます。WebView2 Runtime の解決（導入済み Evergreen、実行ファイル直下の Fixed Version、どちらも無ければネイティブダイアログで通知して終了する3経路）、`hakutaku.yaml` の読み込み、実行ファイル直下の `logs`／`temp`／`WebView2` フォルダの用意と清掃、診断ログの開始までをここで行います。詳細は[WebView2 Runtime の解決経路と開発環境での扱い](docs/development/webview2-runtime.md)を参照してください。

## 動かす

Windows での確認には、[Tauri の前提条件](https://v2.tauri.app/start/prerequisites/)に記載された Microsoft C++ Build Tools、Microsoft Edge WebView2、Rust、Node.js が必要です。製品ビルドの対象は 64 ビット版の Windows 10 / 11 だけで、下限は build 17763 相当です。詳細は [Windows ビルド互換性と依存追加時の確認](docs/development/windows-build-compatibility.md)を参照してください。

```powershell
npm ci
npm run tauri dev
```

配布用インストーラーを作らず、実行ファイルだけを確認する場合は次を実行します。

```powershell
npm run tauri build -- --no-bundle
```

Windows の生成物は `target/x86_64-pc-windows-msvc/release/Hakutaku.exe` です。ビルド対象は [`.cargo/config.toml`](.cargo/config.toml) で `x86_64-pc-windows-msvc` に固定しているため、生成物はターゲット名のディレクトリ配下に出ます。生成物は Git の追跡対象に含めません。

変更を送る前に実行する標準の検査コマンドは、[開発ワークフローの「段階0の標準検査」](docs/development/workflow.md#段階0の標準検査)にまとめています。

画面と操作を通して実装済みの機能を確認する場合は、[手動での動作確認手順](docs/verification/manual-check.md)に従ってください。動作確認の環境（ビルド、サンプル生成、設定ファイルの配置、起動）は、まず次の1コマンドでまとめて準備できます。

```powershell
./scripts/start-manual-check.ps1
```

サンプルログと設定ファイルの生成だけを行う場合は、次のコマンドを使います（生成先は既定で `%TEMP%\hakutaku-samples`。リポジトリ内には生成できません）。

```powershell
./scripts/generate-sample-logs.ps1
```

## 配布物

インストーラーは使いません（`DIST-001`）。本体は単一の実行ファイルで動作し、配布は次の2つに分けます。

- **本体パッケージ**: 実行ファイル、設定ファイルの記述例、リリースノートを含む ZIP
- **Runtime 追加パッケージ**: Fixed Version WebView2 Runtime の別 ZIP。Evergreen が導入されていない端末で事前に配置します（`DIST-007`、`DIST-012`）

どちらも [`scripts/package-release.ps1`](scripts/package-release.ps1) で組み立てます。版ごとの配布物、対応する Runtime の版、導入手順、既知の制約は[リリースノート](docs/release-notes/README.md)を参照してください。

## リポジトリ構成

リポジトリのルートには、自動検出、リポジトリ全体への適用、ライセンス表示などのためにルート配置が必要なファイルだけを置きます。現在の文書では、プロジェクトの入口である `README.md`、ライセンスの `LICENSE`、AI エージェントが自動検出する `AGENTS.md`、Claude Code がルートでのみ自動検出する入口 `CLAUDE.md` が該当します。`CLAUDE.md` は規則の正本ではなく、`AGENTS.md` への入口です。ビルド構成では、Cargo がワークスペースのルートを要求する `Cargo.toml` と `Cargo.lock`、npm が要求する `package.json` と `package-lock.json` が該当します。

- プロダクト、設計、開発運用などの一般文書は [`docs/`](docs/README.md) に置く
- 貢献ガイド、セキュリティポリシー、Issue / PR テンプレートなど GitHub と直接関係するファイルは [`.github/`](.github/) に置く
- `.gitignore`、`.gitattributes`、`.editorconfig` など、リポジトリ全体へ適用する機械可読の設定はルートに置く
- ツール固有の設定は、ツールが要求する場所に機械可読の設定だけを置き、説明文書は `docs/` に置く。Cargo の共通ビルド設定は、Cargo がワークスペースのルートからの相対位置で読む [`.cargo/config.toml`](.cargo/config.toml) に置く

コードの配置は次のとおりです。

| 場所 | 内容 |
| --- | --- |
| [`crates/`](crates/) | GUI に依存しないコアクレート群（設定、データソース、形式判定、パーサー、メモリ会計、診断、共通サービス） |
| [`src-tauri/`](src-tauri/) | Tauri コマンドの薄い GUI 層、Tauri 設定（`Tauri.toml`）、Capability と個別 Permission の定義 |
| [`src/`](src/) | フロントエンドの静的資産（HTML、CSS、素の ES モジュール） |
| [`scripts/`](scripts/) | 開発・検証用の PowerShell スクリプト（Runtime の配置、試験データ生成、動作確認用サンプル一式の生成、動作確認環境の一括準備、追記テスト、配布 ZIP の組み立て） |

試験データは合成データだけを使い、実データ（個人情報等の機密データを含み得るログ）をリポジトリへ置きません。生成手段は [`scripts/generate-test-log.ps1`](scripts/generate-test-log.ps1)（任意の行数・書式・文字コードのログを1本生成）と [`scripts/generate-sample-logs.ps1`](scripts/generate-sample-logs.ps1)（動作確認用のサンプル一式と設定ファイルを生成）です。いずれも生成先はリポジトリ外に限ります。

配置判断の正本は[リポジトリ運用規則の「ファイルの配置」](docs/development/repository-operations.md#ファイルの配置)です。新しいルートファイルを追加する場合は、ルートでなければならない理由を変更記録に残してください。

## ドキュメント

文書の一覧と正本の分担は[ドキュメント一覧](docs/README.md)を参照してください。主要な入口は次のとおりです。

- [プロダクトビジョン](docs/product/vision.md)
- [プロダクトスコープ](docs/product/scope.md)
- [ロードマップ](docs/roadmap.md)
- [手動での動作確認手順](docs/verification/manual-check.md)

## 開発運用

Hakutaku は個人開発のリポジトリであり、外部からのコード貢献は受け付けていません（[外部貢献の方針](.github/CONTRIBUTING.md)）。開発は AI エージェントとの並行セッションで行い、規則はルートの [`AGENTS.md`](AGENTS.md) と[開発運用](docs/development/README.md)を正本とします。

## セキュリティ

脆弱性や機密情報を含む問題を公開 Issue に投稿しないでください。報告方法は[セキュリティポリシー](.github/SECURITY.md)を参照してください。

診断ログと画面表示は、仕様（`DIAG-003`／`DIAG-004`、`ERR-002`）により実値をマスキングしません。ログ本文やフルパスがそのまま表示・記録され得ることを前提に、閲覧制限、持ち出し、保管、削除は利用者・導入組織の責任範囲として扱います（`SEC-005`）。

## ライセンス

[MIT License](LICENSE)
