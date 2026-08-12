# Windows ビルド互換性と依存追加時の確認

- 状態: 採用済み（Accepted）
- 管理責任者: リポジトリメンテナー
- 最終更新日: 2026-07-31

## 目的

この文書は、Hakutaku の Windows 製品ビルドの対象と下限、および依存クレート追加時の互換性確認手順の正本です。対象は Windows 10 / 11 の 64 ビット版のみ（`PROD-002`）で、Windows 10 1809 / LTSC 2019 の build 17763 相当を想定下限（`ENV-001`、`VER-004`）とします。この下限の実機確定は P13 で行い、それまでは下限を上げません。

## ビルド構成

- [`.cargo/config.toml`](../../.cargo/config.toml) の `build.target` を `x86_64-pc-windows-msvc` に固定します。x86、ARM64、Windows 以外を製品ビルドとして作りません。
- 同設定の `_CL_` は、ネイティブ依存が Windows ヘッダーを使う際に `WINVER=0x0A00`、`_WIN32_WINNT=0x0A00`、`NTDDI_VERSION=0x0A000006`（RS5）を参照境界とします。
- [`src-tauri/build.rs`](../../src-tauri/build.rs) は対象 OS と CPU アーキテクチャを検査し、Tauri バイナリの PE subsystem version を `10.00` にします。debug はコンソール用エントリーポイントを維持するため `CONSOLE`、release は `WINDOWS` を指定します。

この構成の根拠は次のとおりです。

- Rust の Windows MSVC ターゲットでは `x86_64-pc-windows-msvc` が Windows x64 の Tier 1 ターゲットであり、Rust 1.78 以降のツールチェーンと生成物の基準は Windows 10 です。
- Windows 10 Enterprise LTSC 2019 は version 1809 / OS build 17763 です。
- Tauri 2 は Windows で WebView2 を使用し、WebView2 Runtime は Windows 10 Enterprise 2019 LTSC をサポートします。

`/SUBSYSTEM` の version と Windows ヘッダーのマクロは、利用可能な API の境界を構成に残すための防波堤です。Rust クレートが動的に新しい API を探索する場合や WebView2 Runtime の実際の挙動までは保証しないため、build 17763 x64 での実測を省略できません。

## Fixed Version WebView2 Runtime の版

Hakutaku は、OS 導入済みの Evergreen Runtime と、実行ファイル直下の `WebView2Runtime` に配置する Fixed Version Runtime の両経路を保守します（`DIST-006`、`ENV-008`）。Fixed Version Runtime の版はビルドごとに固定し、動作確認は当該版で行います。利用者が任意の版へ差し替えた場合の動作は保証しません（`DIST-016`）。

### 現在固定している版

| 項目 | 内容 |
| --- | --- |
| 版 | **150.0.4078.105** |
| 対象アーキテクチャ | x64 のみ（`PROD-002`。上記「ビルド構成」と同じ） |
| 入手元 | `Microsoft.WebView2.FixedVersionRuntime.150.0.4078.105.x64.cab`（[Microsoft の配布 CDN](https://msedge.sf.dl.delivery.mp.microsoft.com/filestreamingservice/files/b401c036-cfb8-4dc4-a58e-8766441df4ac/Microsoft.WebView2.FixedVersionRuntime.150.0.4078.105.x64.cab)） |
| 入手日 | 2026-07-31 |
| 保管場所 | リポジトリ直下の `runtime/WebView2Runtime/`。**Git 管理外** |
| フォルダ構造 | CAB 内の版付きフォルダを平坦化し、`msedgewebview2.exe` を `WebView2Runtime` 直下へ置く |
| 版の判別 | 同フォルダ直下の `150.0.4078.105.manifest` |
| 配置日 | 2026-07-31 |

入手元の URL は配布 CDN の直リンクで、GUID を含み恒久的ではありません。再取得する場合は [Microsoft Edge WebView2](https://learn.microsoft.com/en-us/microsoft-edge/webview2/) の配布ページから同じ版（150.0.4078.105、x64）を選びます。版が一致していれば入手経路は問いません。

平坦化するのは、`DIST-011` が更新を「`WebView2Runtime` フォルダ一式の置き換え」と定めているためです。版付きフォルダを保つと参照パスが版に依存し、置き換えのたびに実装側の参照先が変わります。平坦化により、`WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` 相当が指す先は実行ファイル直下の `WebView2Runtime` そのものになります（`DIST-008`）。

`runtime/` は**取得物の保管場所**です。実行時に参照されるのは実行ファイル直下の `WebView2Runtime` であり、開発中に実行ファイルの隣へ配置する手段は別に必要です。

### リポジトリへ含めない理由

Fixed Version Runtime の実体を Git で管理しません。[`.gitignore`](../../.gitignore) で `/runtime/` を除外しています。

- `DIST-007` により、Fixed Version Runtime は本体へ常時同梱せず、任意追加パッケージとして別 ZIP で提供します
- 一式は 648 MB あり、`msedge.dll` 単体で 317 MB です。GitHub の 100 MB ファイル上限を超えます

### 版を更新するとき

Fixed Version Runtime は自動更新されません（`DIST-011`）。Runtime 側の修正を取り込む場合は、次を同じ Issue / PR で行います。

1. Hakutaku を終了し、`WebView2Runtime` フォルダ一式を新しい版で置き換える（差分更新をしない）
2. この節の版、マニフェスト名、配置日を更新する
3. 当該版で `DIST-006`〜`DIST-017` の経路（Evergreen、Fixed Version、Runtime なし）を再確認する
4. 配布物のリリースノートへ対応 Runtime 版を明記する（`DIST-016`。実施は P12）

## 依存クレートを追加・更新するときの確認

依存クレートの追加、直接依存のバージョン更新、feature 変更によって推移依存が変わる場合は、次を同じ Issue / PR で確認します。

1. `Cargo.toml`、`Cargo.lock`、`cargo tree` で、追加・更新する直接依存と推移依存、version、feature、利用目的を特定する。
2. 公式文書、リリースノート、ソースを確認し、`x86_64-pc-windows-msvc` と Windows 10 1809 / build 17763 を除外する要件、新しい Windows API の必須利用、追加の Runtime 要件がないことを根拠付きで確認する。不明な場合は追加を完了扱いにしない。
3. updater、telemetry、crash report、HTTP client など、実行時の外部通信を有効にする feature と初期化処理がないか確認する。参照内容を外部へ送信する経路がある依存・設定は採用しない（`SEC-001`）。ビルド時の取得と Hakutaku 実行時の通信は分けて記録する。
4. 固定ターゲットで `cargo build --workspace --locked`、`cargo test --workspace --locked`、`cargo build --workspace --release --locked` を実行し、`target/x86_64-pc-windows-msvc/` 配下に成果物が生成されることを確認する。
5. Windows 10 1809 / build 17763 x64 で release 版を起動し、追加・更新した依存を通る操作を確認する。WebView2 を使う操作では Runtime の配布方式と version も記録する。環境を用意できない場合は未実施として明示し、互換性確認を後続課題へ残す。

記録先は依存変更の Issue / PR の「依存関係と下限環境確認」節とし、次を記録します。

- 依存名、version、feature、利用目的、推移依存の主な変化
- 対応根拠の URL と確認日
- 実行時外部通信の有無、確認した feature と初期化経路
- 検証した Windows edition、version、OS build、CPU アーキテクチャ、WebView2 Runtime version
- 実行コマンドと結果、未実施項目、残るリスク

build 17763 の実機または仮想環境を用いた継続検査の自動化は P13 で扱います。

## 参照資料

- [Rust: Windows MSVC targets](https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html)
- [Rust: Windows 7, 8, and 8.1 targets](https://blog.rust-lang.org/2024/02/26/Windows-7/)
- [Cargo: Configuration](https://doc.rust-lang.org/cargo/reference/config.html)
- [Microsoft: `/SUBSYSTEM`](https://learn.microsoft.com/en-us/cpp/build/reference/subsystem-specify-subsystem)
- [Microsoft: Using the Windows Headers](https://learn.microsoft.com/en-us/windows/win32/winprog/using-the-windows-headers)
- [Microsoft: Windows 10 release information](https://learn.microsoft.com/en-us/windows/release-health/release-information)
- [Microsoft Edge WebView2](https://learn.microsoft.com/en-us/microsoft-edge/webview2/)
- [Tauri: Prerequisites](https://v2.tauri.app/start/prerequisites/)
