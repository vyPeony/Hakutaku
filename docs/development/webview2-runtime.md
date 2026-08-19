# WebView2 Runtime の解決経路と開発環境での扱い

- 状態: 採用済み（Accepted）
- 管理責任者: リポジトリメンテナー
- 最終更新日: 2026-08-19

## 目的

この文書は、Hakutaku が Tauri を初期化する**前**に行う WebView2 Runtime の解決処理について、開発者が把握すべき内容の正本です。対象は次の3点です。

- WebView2 Runtime の3つの解決経路と、両方とも使用できない場合の扱い
- `WebView2` と `WebView2Runtime` という似た名前の2つのフォルダの役割の違い
- 開発環境で Fixed Version Runtime を配置・使用するための設定キーと手順

実装は [`src-tauri/src/bootstrap/`](../../src-tauri/src/bootstrap/) 配下（`runtime.rs`、`acl.rs`、`notify.rs`、`layout.rs`、`process.rs`）にあります。要件の正本は[機能要件](../requirements/functional.md)（`TECH-005`、`DIST-006`〜`017`、`DIAG-001`〜`007`）で、実装時の受け入れ条件は当時の Issue / PR にあります。ビルド対象・Fixed Version Runtime の固定版・依存クレート追加時の確認手順は [Windows ビルド互換性と依存追加時の確認](windows-build-compatibility.md)を正本とし、本書では重複させません。

## 3つの解決経路（`DIST-006`、`DIST-008`、`DIST-009`）

Hakutaku は、Tauri を初期化する前に Rust 側で次の3経路のいずれかを解決します。

1. **導入済み Evergreen Runtime。** OS にインストール済みの WebView2 Runtime（Evergreen）を最初に確認します。
2. **実行ファイル直下の `WebView2Runtime`（Fixed Version）。** Evergreen が使用できない場合、実行ファイルと同じフォルダの `WebView2Runtime` を確認します。インストーラーやレジストリ登録、システムフォルダへの配置は行わず、プロセス内で `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER`（`DIST-008`）相当の環境変数を実行ファイルからの相対パスで設定するだけで使用可能にします。
3. **どちらも無い場合。** ネイティブダイアログ（Win32 の `MessageBoxW`。Tauri を使わない）で、必要な Runtime・配置先（絶対パス）・再起動手順を通知し、Tauri を初期化せず終了します（`DIST-009`）。

## 解決順序

P01（起動ブートストラップ）の実装計画が定めた「起動手順の実装順序」どおりに実装されています。

1. `webview2.force_fixed_version_runtime`（`DIST-017`／`CFG-023`）の先行読み込みを行う。強制指定があれば手順3へ進む
2. 互換性のある導入済み Evergreen Runtime を確認する
3. 見つからない場合、実行ファイルと同じフォルダの `WebView2Runtime` を確認する
4. Fixed Version が見つかった場合、必要なフォルダ ACL を確認してから相対パスを指定する
5. 使用する Runtime が決まったら、ユーザーデータフォルダとして実行ファイル直下の `WebView2` を指定し、Tauri を初期化する。作成・書き込みできない場合は通知して終了する（`DIST-014`）
6. どちらの Runtime も使用できない場合はネイティブダイアログで配置方法を通知し、Tauri を初期化せず終了する

この手順は [`src-tauri/src/bootstrap/runtime.rs`](../../src-tauri/src/bootstrap/runtime.rs) の `resolve()` が手順1〜4を、統合担当のコードが手順5〜6を実装します。

## `WebView2` と `WebView2Runtime` の違い

名前が似ているため取り違えやすい2つのフォルダです。実装・通知文（`bootstrap::notify`）ともに、この違いを明示しています。

| フォルダ名 | 役割 | 固定するファイル・要件 |
| --- | --- | --- |
| `WebView2` | WebView2 の**ユーザーデータ**（閲覧・実行状態、キャッシュなど）の保存先 | `DIST-013`。実行ファイル直下に固定し、既定の `<実行ファイル名>.exe.WebView2` は使わない。作成・書き込み不可時は別の場所へフォールバックせず起動を中止する（`DIST-014`） |
| `WebView2Runtime` | **Fixed Version Runtime 本体**（`msedgewebview2.exe` など）の配置先 | `DIST-008`。実行ファイル直下からの相対パスで `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` 相当を設定する。フォルダの ACL（メタデータ）だけ例外的に変更し得る（`DIST-010`）。ファイル内容は変更しない（`DIST-011`） |

`SEC-009` は実行時データの書き込み先を `logs`・`temp`・`WebView2` に限定していますが、`WebView2Runtime` はこの「実行時データ」には該当しません。`DIST-010` による ACL（メタデータ）変更だけが明示的な例外として認められています。

## 設定キー: `webview2.force_fixed_version_runtime`

Fixed Version Runtime を強制的に使用するかどうかを設定できます（`DIST-017`、`CFG-023`）。

| 項目 | 内容 |
| --- | --- |
| キー | `webview2.force_fixed_version_runtime` |
| 型 | 真偽値（`true` / `false`） |
| 既定 | `false`（自動判定。Evergreen を優先し、無ければ Fixed Version を使う） |
| `true` の効果 | Evergreen が導入済みでも Fixed Version Runtime を優先使用する |

`hakutaku.yaml` での指定例:

```yaml
webview2:
  force_fixed_version_runtime: true
```

**P01 で読み込むのはこの1項目だけです。** 設定ファイル全体の読み込みとスキーマ検証は P03（設定基盤）で行います。この先行読み込みは [`crates/config/src/lib.rs`](../../crates/config/src/lib.rs) の `read_fixed_runtime_preference` が担い、YAML の構文エラーやキーの型不一致があっても安全側の既定（`Auto`）へフォールバックしたうえで Runtime 解決を続行します。設定ファイル全体を安全モードとして扱うかどうかの判断（`CFG-016`）は P03 に引き継がれ、P01 では行いません。

## 固定している版

Fixed Version WebView2 Runtime の版は **150.0.4078.105（x64）** に固定しています。版の判別は `WebView2Runtime` フォルダ直下の `150.0.4078.105.manifest` です。取得物の保管場所はリポジトリ直下の `runtime/WebView2Runtime/` で、**Git 管理外**です（[`.gitignore`](../../.gitignore) の `/runtime/`）。

Git 管理外にしている理由は次の2点です。

- `DIST-007` により、Fixed Version Runtime は本体へ常時同梱せず、任意追加パッケージとして別 ZIP で提供するため
- 単一ファイル（`msedge.dll`）だけで GitHub の 100 MB ファイル上限を超えるため

版の入手元、更新手順、リポジトリへ含めない理由の詳細は[Windows ビルド互換性と依存追加時の確認の「Fixed Version WebView2 Runtime の版」](windows-build-compatibility.md#fixed-version-webview2-runtime-の版)を正本とし、本書では重複させません。

## 開発時の配置手順

実行時に参照される `WebView2Runtime` は実行ファイル（`Hakutaku.exe`）と同じフォルダに置かれている必要がありますが、取得物は `runtime/WebView2Runtime/` に保管するだけで、実行ファイルの隣（`target/x86_64-pc-windows-msvc/<profile>/`）ではありません。[`scripts/dev-webview2-runtime.ps1`](../../scripts/dev-webview2-runtime.ps1)は、両者をディレクトリジャンクションでつなぎます。

```powershell
# 既定（debug）のビルド出力先にジャンクションを作成する
pwsh scripts/dev-webview2-runtime.ps1

# release のビルド出力先にジャンクションを作成する
pwsh scripts/dev-webview2-runtime.ps1 -Profile release

# 既定（debug）のビルド出力先からジャンクションを削除する
pwsh scripts/dev-webview2-runtime.ps1 -Remove
```

| パラメーター | 内容 |
| --- | --- |
| `-Profile` | 対象のビルドプロファイル。`debug` または `release`。既定は `debug`。`target/x86_64-pc-windows-msvc/<Profile>/WebView2Runtime` にジャンクションを作る |
| `-Remove` | 指定すると、対象プロファイルのジャンクション（参照）だけを削除する。新規作成は行わない。`runtime/WebView2Runtime` の中身には触れない |

`runtime/WebView2Runtime` が無い場合、スクリプトは Fixed Version Runtime の入手・平坦化・配置手順を案内して終了します。ビルド出力先（`target/x86_64-pc-windows-msvc/<Profile>/`）がまだ無い場合も、先に `cargo build` を行うよう案内して終了します。既にジャンクションが存在する場合は何もしません。対象パスにジャンクションではない実体のディレクトリが既にある場合は、誤って中身を消さないよう作成・削除のどちらも中止して警告します。

コピーではなくディレクトリジャンクションを採用した理由は次のとおりです。

- コピーは数百 MB を毎回複製することになり、ビルドのたびに時間とディスクを消費する。ジャンクションは作成が瞬時で、実体を二重に持たない
- 実体を `runtime/WebView2Runtime` の1か所だけに保てるため、「Fixed Version Runtime のファイル内容を変更しない」（`DIST-011`）という保証が、どちらの実体を指しているかで曖昧にならない
- Windows のディレクトリジャンクションは、シンボリックリンクと異なり管理者権限を必要としない

## 最低要求版

起動を許可する WebView2 ブラウザーの最低版は **`86.0.616.0`** です（[`src-tauri/src/bootstrap/runtime.rs`](../../src-tauri/src/bootstrap/runtime.rs) の `MINIMUM_SUPPORTED_VERSION`）。

WebView2 は Edge 86（2020年11月、`86.0.616.0`）で GA（General Availability）となりました。Hakutaku はそれ以降に追加された特別な新しい WebView2 API には依存していないため、この GA 版を安全網としての下限に採用しています。**機能面の要求から導いた値ではなく**、壊れた・極端に古い Runtime を誤って採用しないための下限です。Evergreen Runtime は自動更新されるため実運用でこの下限に抵触することは基本的になく、現在配置している Fixed Version（150.0.4078.105）は十分にこの値を上回ります。

版の比較に失敗した場合（版文字列の形式が想定と異なるなど）は、Evergreen を不必要に弾かないよう安全側（許可）に倒し、警告として記録したうえで続行します。

## ACL について（`DIST-010`）

Windows 10 では、App Container からのアクセスに `WebView2Runtime` フォルダの ACL 設定が必要になる場合があります。

- **対象は `WebView2Runtime` フォルダのメタデータ（ACL）だけです。** フォルダ内のファイル内容は一切変更しません。これは `SEC-009`（実行時に作成・書き込みするフォルダを `logs`・`temp`・`WebView2` に限定する）の明示的な例外です。
- 付与する権限は、App Container（`ALL APPLICATION PACKAGES`。SID は `S-1-15-2-1`）に対する**読み取り + 実行**（継承あり）です。
- 既に必要なアクセスが許可されていれば何も変更しません。不足していれば ACE を追加します。現在の権限で変更できない場合はネイティブダイアログで理由と必要な対応を通知します（[`src-tauri/src/bootstrap/acl.rs`](../../src-tauri/src/bootstrap/acl.rs)）。
- 判定は許可 ACE だけでなく拒否 ACE も DACL の並び順どおりに読みます（`evaluate_dacl`）。App Container 用 SID への拒否 ACE が必要なアクセス権と重なっている場合、Windows の評価順（拒否が許可より優先）では許可 ACE を追加しても有効にならないため、付与を行わず `AclOutcome::BlockedByDenyAce` を返します。この場合、診断ログには「付与しました」ではなく、拒否 ACE により有効にならない旨と管理者による確認が必要である旨が記録されます（[Issue #45](https://github.com/vyPeony/Hakutaku/issues/45)）。
- 付与は対象フォルダの**継承構造を変えません**。親フォルダから継承していた ACE は継承のまま残り、明示 ACE へ複製されないため、付与後も親フォルダの権限変更が `WebView2Runtime` フォルダへ反映され続けます。実機（Windows 10 22H2）で書き戻し前後の ACL ダンプを比較して確認し、回帰テストで固定しています（[`src-tauri/src/bootstrap/acl.rs`](../../src-tauri/src/bootstrap/acl.rs) の `granting_access_keeps_inherited_aces_inherited`、[Issue #45](https://github.com/vyPeony/Hakutaku/issues/45)）。
- ACL の要否そのものが判定できない場合も、Runtime の使用は継続します（安全側に倒す）。

**ACL 要否の最終確定は P13（`VER-006`）です。** 段階0・段階1では「必要と判明したら設定を試み、できなければ通知する」経路を実装済みにしておくところまでが範囲です。

## 開発時の制約: ジャンクション経由では ACL がリンク先へ伝わらない

実機検証で確認した制約です。「開発時の配置手順」で使うディレクトリジャンクションは、ジャンクションのパスに対して ACL を設定しても、**リンク先の実フォルダには伝わりません。**

```
（ジャンクション linkdir → 実フォルダ realdir を作り、linkdir 経由で
  ALL APPLICATION PACKAGES へ読み取り+実行を付与した）

リンク経由で Get-Acl   →  rights=0x1200A9        （付与されている）
リンク先を直接 Get-Acl →  ALL APPLICATION PACKAGES の ACE なし
```

つまり、ジャンクションのパスに対する ACL の設定は**ジャンクション（再解析ポイント）自身の ACL** を変更するのであって、リンク先のフォルダ本体には反映されません。

実際に Hakutaku を Fixed Version 強制（`webview2.force_fixed_version_runtime: true`）で起動して確認した結果も同じでした。

- 診断ログには「App Container からのアクセスを付与しました: `<ジャンクションのパス>\WebView2Runtime`」と記録された
- しかし、リンク先の実フォルダ（`runtime/WebView2Runtime`）には `ALL APPLICATION PACKAGES` の ACE が1件も付与されていなかった
- それでも Fixed Version Runtime（150.0.4078.105）で正常に起動した。つまり、この開発端末では App Container 用 ACL は実際には不要だった

したがって、開発時にジャンクションを使っている間は、診断ログが「付与しました」（`AclOutcome::Applied`）と記録していても、**Runtime 本体のファイル（実フォルダ側）には App Container の許可が付いていない**ことがあります。実配布時はこの制約は生じません。配布物では `WebView2Runtime` がジャンクションではなく実フォルダそのものであり、`bootstrap::acl::ensure_app_container_access` が対象フォルダへ直接 ACL を設定するためです。

`DIST-010` の ACL 要否そのものの最終確定は P13（`VER-006`）です。**もし ACL が必要と確定した場合、** 開発時にジャンクションのまま ACL 経路を検証することはできません。次のいずれかが必要になります。

- `runtime/WebView2Runtime` を実フォルダとして `target/x86_64-pc-windows-msvc/<profile>/WebView2Runtime` へコピーして確認する（ジャンクションを使わない）
- ジャンクションを使わず、`runtime/WebView2Runtime` へ直接 ACL を付与して確認する

## 段階0で確認できないこと

この開発端末には Evergreen Runtime が導入済み（版 150.0.4078.105。レジストリ `HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}` の `pv` 値で確認）です。そのため、**Evergreen が存在しない端末での起動確認は、この開発端末では行えず、実機検証（P13）でしか行えません。**

`DIST-017`（`webview2.force_fixed_version_runtime: true`）による強制指定を使えば、Evergreen 導入済みのこの開発端末上でも Fixed Version 経路自体は確認できます。これにより `VER-003`（段階0でも Evergreen に依存しない実装であることの確認）は満たせますが、「Evergreen 未導入端末で Fixed Version 経路が唯一の起動手段になる」という状況そのものの確認は段階0の範囲外です。

## 既知の制約（`DIST-012`）

Evergreen 未導入端末では、Runtime 追加パッケージ（Fixed Version Runtime 一式）の事前配置が起動の前提条件です。Hakutaku はネットワークから WebView2 Runtime を取得しないため、この制約は実装では解消できない仕様上の制約であり、[WebView2 Runtime 導入手順書](../deployment/webview2-runtime-installation.md)で導入担当者へ案内しています。

導入担当者向けの事前確認・配置手順は[WebView2 Runtime 導入手順書](../deployment/webview2-runtime-installation.md)を正本とし、本書では重複させません。
