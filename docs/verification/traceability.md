# 要件と充足根拠の対応表

- 状態: 採用済み（Accepted）
- 管理責任者: リポジトリメンテナー
- 最終更新日: 2026-08-25

## 位置づけ

本書は、**検証記録に結果が現れない要件**と、**コード上に要件 ID の参照がない要件**について、何をもって充足しているとみなすかを1か所へ集めた対応表です（[Issue #54](https://github.com/vyPeony/Hakutaku/issues/54)）。

- 要件本文の正本は[機能要件](../requirements/functional.md)と[品質要件](../requirements/quality.md)です。本書は要件を作らず、変えません
- 検証結果の正本は[段階0検証記録](stage0-results.md)と[手動での動作確認手順](manual-check.md)です。本書は結果を複製せず、節を指すだけにします
- 各行の根拠は、実装ファイルまたは検証記録の現物を確認して書きます。**確認できないものは「未検証」と書き、どう検証すべきかを添えます**

「状態」列の意味は次のとおりです。

| 状態 | 意味 |
| --- | --- |
| 実装で担保 | 実装またはビルド設定が要件を構造的に満たしており、該当箇所を特定できる |
| 検証済み | 実装で担保したうえで、検証記録に実施結果がある |
| 未検証 | 充足根拠はあるが、実施結果の記録がない。備考に検証方法を書く |
| 検証不能 | 現在の体制・環境では検証手段が無い。備考に理由を書く |

## 1. 動作環境の確定要件（ENV）

[品質要件の「動作環境の確定要件」](../requirements/quality.md#動作環境の確定要件)のうち、検証記録に結果が現れない7件です。`ENV-004`（ストレージ）・`ENV-005`（画面解像度）・`ENV-010`（実行形態）・`ENV-012`（異常時の運用）は検証記録に記載があるため、本表には含めません。

| 要件 | 充足根拠（現物確認済み） | 状態 | 備考 |
| --- | --- | --- | --- |
| `ENV-001`（OS） | [`.cargo/config.toml`](../../.cargo/config.toml) 3行目 `target = "x86_64-pc-windows-msvc"`（64ビット限定）、6〜8行目 `_CL_` によるネイティブ依存の下限指定（`WINVER`／`_WIN32_WINNT` = `0x0A00`、`NTDDI_VERSION` = `0x0A000006` = Windows 10 1809 / RS5）。正本は[Windows ビルド互換性](../development/windows-build-compatibility.md) | 未検証 | ビルド設定として下限を強制しているが、下限ビルド（build 17763）の実機起動は未実施。段階1（P13、`VER-006`）で確定する |
| `ENV-002`（CPU） | [`.cargo/config.toml`](../../.cargo/config.toml) 全文に `target-cpu` 等の CPU 固有指定が無く、ビルド設定は `x86_64-pc-windows-msvc` のみ。リポジトリ内に `RUSTFLAGS` での CPU 指定も無い | 実装で担保 | 「特定の CPU モデルを前提としない」という否定形の要件であり、指定が無いことが充足そのもの。実機での性能差は段階1（`PERF-015`）の測定対象 |
| `ENV-003`（メモリ） | [`crates/memory-accounting/src/budget.rs`](../../crates/memory-accounting/src/budget.rs) 57行目 `DEFAULT_BUDGET_BYTES: usize = 2 * 1024 * 1024 * 1024`（2 GiB）。設定での引き下げは同 83行目 `set_global_budget_bytes`（`CFG-007`） | 検証済み（予算部分） | 予算の動作は[段階0検証記録](stage0-results.md)の[2.4](stage0-results.md#24-ヒープ会計scale_verify)節で確認済み。「搭載メモリ最低 8 GB」は端末側の前提であり、Hakutaku 側で検証する対象ではない |
| `ENV-006`（ネットワーク） | [`src-tauri/Tauri.toml`](../../src-tauri/Tauri.toml) 79行目の CSP（`default-src 'self'`。リモートオリジンを一切許可せず、`connect-src` は Tauri の IPC 用 `ipc:`／`http://ipc.localhost` のみ）。[`src-tauri/Cargo.toml`](../../src-tauri/Cargo.toml) に HTTP クライアント・ネットワークプラグインの依存が無い。[`src-tauri/src/bootstrap/runtime.rs`](../../src-tauri/src/bootstrap/runtime.rs) 1〜23行目（WebView2 Runtime の解決はローカルの検出のみで、取得・インストールを行わない） | 一部未検証 | 送信の不在は[段階0検証記録](stage0-results.md)の[3.2](stage0-results.md#32-静的監査-a)節（静的監査）・[3.3](stage0-results.md#33-接続列挙-b)節（接続列挙）で確認済み。**ネットワークを遮断した状態での起動確認は未実施**（同[8](stage0-results.md#8-実施できなかった項目と理由)節） |
| `ENV-007`（UI 言語） | [`src/index.html`](../../src/index.html) 2行目 `<html lang="ja">`。UI 文字列は日本語で直書きされ、言語切り替え・翻訳リソースの仕組みを持たない（`src/` に locale・i18n の資源が無い） | 未検証 | 実際の画面での日本語表示は[段階0検証記録](stage0-results.md)の[8](stage0-results.md#8-実施できなかった項目と理由)節でヘッドレス環境のため未実施とされ、利用者による画面確認へ引き継がれている。[手動での動作確認手順](manual-check.md)の 4 章全体が実質的な確認になる |
| `ENV-008`（想定環境） | [`src-tauri/src/bootstrap/runtime.rs`](../../src-tauri/src/bootstrap/runtime.rs) 1〜23行目（導入済み Evergreen の確認 → 実行ファイル直下の `WebView2Runtime`（Fixed Version）の確認、の2経路。`CFG-023`／`DIST-017` による強制指定を含む）、[`src-tauri/src/bootstrap/layout.rs`](../../src-tauri/src/bootstrap/layout.rs) 47行目 `WEBVIEW2_RUNTIME_DIR_NAME` | 一部未検証 | 両経路が実装されていることは確認済み。**Fixed Version 経路と Evergreen 未導入環境での起動確認は未実施**（[段階0検証記録](stage0-results.md)の[8](stage0-results.md#8-実施できなかった項目と理由)節。段階1／`VER-006`） |
| `ENV-009`（導入時の制約） | [`src-tauri/src/bootstrap/layout.rs`](../../src-tauri/src/bootstrap/layout.rs) 42〜49行目（`logs`／`temp`／`WebView2`／`WebView2Runtime` を実行ファイル直下の固定名とする）、228行目（`WebView2` フォルダを作成し書き込めることを確認する）、[`src-tauri/src/bootstrap/acl.rs`](../../src-tauri/src/bootstrap/acl.rs)（Fixed Version 使用時の ACL 確認と、必要ならメタデータのみの変更） | 一部未検証 | 実行ファイル直下へのフォルダ作成は[手動での動作確認手順](manual-check.md)の[4.16](manual-check.md#416-実行時フォルダと診断ログ)で確認する。**フォルダ ACL の要否の確定は段階1**（`DIST-010`、`VER-006`） |
| `ENV-011`（資源の共有） | `PERF-014` の実装3点。[`crates/data-source/src/chunk.rs`](../../crates/data-source/src/chunk.rs) 191〜196行目 `IoThrottle`（同時実行数の上限・I/O 発行間隔の接続点）、[`src-tauri/src/bootstrap/process.rs`](../../src-tauri/src/bootstrap/process.rs) 122行目 `apply_process_priority`（プロセス優先度、`CFG-024`）、[`crates/data-source/src/lib.rs`](../../crates/data-source/src/lib.rs) 262〜289行目 `open_read_only_shared`（他プロセスの読み書きをブロックしない共有指定） | 未検証 | 抑制手段の存在は確認済み。**他の業務ソフトウェアと同時稼働した状態での影響測定は未実施**（`PERF-015`、段階1／P13）。既定値の確定も段階1 |

## 2. コード上に要件 ID の参照がない要件

次の6件は、実装は存在するが、実装ファイルに要件 ID を書いたコメントがありません。[コードコメント規約](../development/code-comments.md)は要件 ID の参照を推奨しますが、実装が要件の言い換えにすぎない場合や、要件が「〜しない」という否定形の場合は、書ける場所が一意に定まりません。ここでは充足根拠と、ID コメントを置いていない理由を記録します。

| 要件 | 充足根拠（現物確認済み） | ID コメントを置いていない理由 |
| --- | --- | --- |
| `LOG-002`（各行の先頭に日時があるログを対象とする） | [`crates/parser/src/datetime.rs`](../../crates/parser/src/datetime.rs) 445行目「1つの書式定義に基づき、**行頭**の日時を解析します」、541行目「指定した1書式だけで**行頭**の日時を解析します」。日時の探索は行頭に限定され、行中の日時は解析しない | 「行頭を見る」という実装の性質そのものが要件であり、特定の1行に印を付ける対象がない。行頭限定であることは両関数の doc コメントに明記済み |
| `LOG-011`（日時には年が含まれる） | [`crates/parser/src/datetime.rs`](../../crates/parser/src/datetime.rs) 63〜78行目 `LogDateTimeFormat`。既知の6書式（`LOG-DT-001`〜`006`）はすべて `YYYY/MM/DD` または `YYYY-MM-DD` で始まり、年を持たない書式を定義していない | 6書式の定義そのものが充足根拠であり、`LOG-DT-001`〜`006` の ID が既に記載されている。`LOG-011` を重ねて書いても指す対象は同じ |
| `ENC-002`（OS 言語設定に依存しない UTF-8） | [`crates/format-detection/src/lib.rs`](../../crates/format-detection/src/lib.rs) 15〜27行目の `ENC-005` 4段階判定。UTF-8 BOM の検出と、BOM が無くても先頭バイト列が妥当な UTF-8 なら UTF-8 とみなす段階が、実行環境の既定 ANSI コードページ（`GetACP`。[`crates/format-detection/src/win32.rs`](../../crates/format-detection/src/win32.rs) 37行目）へのフォールバックより**前**にある。したがって UTF-8 の読み取りは OS の言語設定に依存しない | `ENC-005` の4段階の**順序**が充足根拠であり、既に `ENC-005` の ID が各段階へ記載されている。`ENC-002` は同じ順序の別の言い方になる |
| `CFG-001`（JSON は使用しない） | [`crates/config/Cargo.toml`](../../crates/config/Cargo.toml) の依存は `saphyr`（YAML）のみで、JSON パーサーへの依存が無い。設定の入口は [`crates/config/src/lib.rs`](../../crates/config/src/lib.rs)（`hakutaku.yaml` の解釈をこのクレートへ一本化）に限られる | 否定形の要件であり、「無いこと」を示す場所が特定の行として存在しない。依存関係の不在が根拠のため、本表への記録が適切 |
| `CFG-002`（初期リリースでは YAML を使用する） | [`crates/config/src/lib.rs`](../../crates/config/src/lib.rs) 7行目・16〜25行目（`hakutaku.yaml` の YAML 解釈をこのクレートへ一本化し、パーサーは `saphyr` 0.0.11。依存はクレート内部に封じ込め）。判断の記録は [ADR-0004](../architecture/decisions/0004-yaml-parser-selection.md) | 採用理由と適用範囲は ADR-0004 が正本であり、実装側は ADR を参照すれば足りる。HCL を含めないことは否定形のため、書ける行がない |
| `CFG-009`（保持する設定は1つ） | [`src-tauri/src/bootstrap/layout.rs`](../../src-tauri/src/bootstrap/layout.rs) 49行目 `CONFIG_FILE_NAME: &str = "hakutaku.yaml"`、114行目 `exe_dir.join(CONFIG_FILE_NAME)`（読み込み先は実行ファイル直下の固定名1つ）。[`crates/config/src/lib.rs`](../../crates/config/src/lib.rs) 303〜312行目（`---` 区切りで複数ドキュメントがある場合も、設定として読むのは先頭1件だけ） | 「複数設定を切り替える機能は不要」という否定形の要件で、切り替えの UI・コマンドが存在しないことが根拠。該当行は `CFG-014`（固定名・固定位置）のコメントと同じ場所を指すため、ID を重ねない |

> [!NOTE]
> 本表の作成にあたり、これら6件へ要件 ID コメントを追記することを検討しましたが、いずれも「実装の一意な1か所」を指せないため追記していません（[Issue #54](https://github.com/vyPeony/Hakutaku/issues/54) の裁定「実装箇所が一意に特定できる場合のみ追記してよい」に従いました）。

## 3. `PERF-016`（安全な停止）の3側面

[`PERF-016`](../requirements/quality.md#性能メモリの確定要件) は3つのことを同時に求めています。側面ごとに充足根拠と検証の所在が異なるため、分けて対応付けます。

| 側面 | 充足根拠（現物確認済み） | 検証 | 状態 |
| --- | --- | --- | --- |
| (1) 参照対象のログ・DICOM・データベースに影響を与えない | [`crates/data-source/src/lib.rs`](../../crates/data-source/src/lib.rs) 262〜289行目 `open_read_only_shared`（読み取り専用で開き、Windows では `FILE_SHARE_READ \| FILE_SHARE_WRITE \| FILE_SHARE_DELETE` を明示指定して他プロセスの追記・削除をブロックしない）。書き込みの経路を持たないため、強制終了時も対象ファイルは変更されず、ハンドルは OS が閉じる | [手動での動作確認手順](manual-check.md)の[4.18](manual-check.md#418-強制終了後の影響他プロセスと共有資源) 手順2・5 | 未検証（手順を追加済み。実施はこれから） |
| (2) 同じ端末で稼働する他の業務ソフトウェアの動作を妨げない | (1) の共有指定に加え、`PERF-014` の抑制3点（本書1節の `ENV-011` の行を参照）。強制終了はプロセスの消滅であり、グローバルなロック・ミューテックス・サービス登録・システム設定の変更を行わないため、他プロセスへ引き継がれる状態が残らない | [手動での動作確認手順](manual-check.md)の[4.18](manual-check.md#418-強制終了後の影響他プロセスと共有資源) 手順1・2 | 未検証（開発環境で確認できる範囲のみ） |
| (3) 次回起動時に `temp` の残存ファイルを清掃する | `SEC-006` として実装・検証済み。[`src-tauri/src/bootstrap/layout.rs`](../../src-tauri/src/bootstrap/layout.rs) 42行目（`temp` の固定名）・238行目（`temp` 配下の残存物の削除）、[`src-tauri/src/bootstrap/mod.rs`](../../src-tauri/src/bootstrap/mod.rs) 240行目（起動手順6で清掃）、[`src-tauri/src/lib.rs`](../../src-tauri/src/lib.rs) 447行目（正常終了時の再清掃） | [手動での動作確認手順](manual-check.md)の[4.16](manual-check.md#416-実行時フォルダと診断ログ) 手順3、[4.18](manual-check.md#418-強制終了後の影響他プロセスと共有資源) 手順3・4 | 検証済み（起動時清掃）／異常終了後の清掃は 4.18 で追加 |

**検証不能として残る範囲**: 側面(2)の本来の確認、すなわち「運用先の専用端末で実際の業務ソフトウェアが稼働している状態で、Hakutaku を強制終了しても業務ソフトウェアが影響を受けない」ことは、**開発環境では検証できません**。実機と実際の業務ソフトウェアが必要なため、段階1（P13、`PERF-015`、`ENV-010`）の対象です。[4.18](manual-check.md#418-強制終了後の影響他プロセスと共有資源) はその代理として、ファイルのロック残存・`temp` の残存物・追記を続ける別プロセスの継続という、開発環境で観測できる部分だけを確認します。

## 4. メモリ予算の表記と実装定数

[品質要件](../requirements/quality.md)の `PERF-008`・`PERF-011` の数値表記は、次の実装定数と対応します。単位の解釈（10進の GB か2進の GiB か）で 7% 以上ずれるため、対応を明記します。

| 記述 | 実装定数 | 値 |
| --- | --- | --- |
| `PERF-008`／`CFG-007` のヒープ予算の初期値「2 GiB」 | [`crates/memory-accounting/src/budget.rs`](../../crates/memory-accounting/src/budget.rs) 57行目 `DEFAULT_BUDGET_BYTES` | `2 * 1024 * 1024 * 1024` バイト（2 GiB。doc コメントも「2 GiB」と記載） |
| `PERF-011` の参考指標のマージン「+ 1 GiB」 | [`crates/memory-accounting/src/private_usage.rs`](../../crates/memory-accounting/src/private_usage.rs) 64行目 `REFERENCE_INDICATOR_MARGIN_BYTES` | `1024 * 1024 * 1024` バイト（1 GiB。doc コメントも「1 GiB」と記載） |

`PERF-011` のマージンは**要件 ID を持たない暫定値**であり、段階1（P12・P13）の実測で再確定する予定です（同定数の doc コメント）。再確定の際は、要件本文の数値も同時に更新します。

## 5. 未決事項

- [機能要件](../requirements/functional.md)の `CFG-007` は「初期値は 2 GB」のままです。[品質要件](../requirements/quality.md)側は本変更で「2 GiB」へ統一しましたが、機能要件は別の作業と編集範囲が重なるため触れていません。次の機能要件の更新時に合わせます
- `ENC-005` の段階番号が、コード内の2か所で食い違っています。[`crates/format-detection/src/lib.rs`](../../crates/format-detection/src/lib.rs) 19〜27行目は「1: 明示指定、2: UTF-8 BOM、3: BOM なし UTF-8 の妥当性確認、4: 環境の ANSI コードページ」としますが、[`crates/format-detection/src/decision.rs`](../../crates/format-detection/src/decision.rs) 39・42行目の `DetectionRoute` は UTF-8 BOM を「第1段階」、BOM なし UTF-8 を「第2段階」と書いています。判定の**順序そのもの**は両者で同じで、番号の付け方だけがずれています。本書の作成時に見つけた食い違いであり、`ENC-005` の要件本文と実装の挙動には影響しません。コメントの番号を揃えるかどうかは、次に同クレートへ触れる作業で判断します
- 本表は、検証記録に現れない要件と ID 参照のない要件だけを対象にした部分的な対応表です。全145要件の網羅的なトレーサビリティ行列は作っていません。必要になった時点で、対象範囲を決めてから着手します

## 関連文書

- [機能要件](../requirements/functional.md)・[品質要件](../requirements/quality.md)
- [段階0検証記録](stage0-results.md)・[手動での動作確認手順](manual-check.md)・[回帰検査の対象と判定方法](regression-checks.md)
- [設計判断（ADR）の索引](../architecture/decisions/README.md)
- [コードコメント規約](../development/code-comments.md)
