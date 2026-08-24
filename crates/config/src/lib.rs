#![forbid(unsafe_code)]

//! `hakutaku.yaml` を解釈する唯一の入口です。
//!
//! # このクレートの位置づけ
//!
//! Hakutaku の設定ファイル `hakutaku.yaml` の YAML 解釈は、このクレートへ一本化します。
//! P01-1 では、起動手順 1（`DIST-017` / `CFG-023` の先行読み込み）に必要な、たった1項目
//! `webview2.force_fixed_version_runtime` を読み取る最小限の機能（[`read_fixed_runtime_preference`]）
//! を実装しました。P03（`tasks/phase-03-configuration.md`）はこれに、`config_version`、
//! 共通設定・データソース・ログ解析プロファイルを含む設定ファイル全体のスキーマ
//! （[`HakutakuConfig`]）と、起動時検証の入口 [`load_config`] を追加します。
//! 別クレートで YAML 解釈をやり直すと、同じファイル形式の解釈が二か所に分かれて
//! 食い違う危険があるため、**そうしないこと**がこのクレートを独立させた理由です。
//!
//! # YAML パーサーの選定
//!
//! パーサーには [`saphyr`](https://docs.rs/saphyr) 0.0.11 を使っています。`saphyr` は
//! [`saphyr::MarkedYaml`] を通じて値ごとの行・列（[`saphyr::Marker`]）を取得できるため、
//! `CFG-016`（ファイル名・行・列・理由の表示）に必要な位置情報を提供できます。
//!
//! この選定は ADR-0004（`docs/architecture/decisions/0004-yaml-parser-selection.md`）で
//! 確定しました。`saphyr` への依存はこのクレートの内部に閉じ込め、公開 API
//! （[`read_fixed_runtime_preference`]、[`PreflightOutcome`]、[`FixedRuntimePreference`]、
//! [`load_config`]、[`LoadOutcome`]、[`HakutakuConfig`] など）に `saphyr` の型は現れません。
//!
//! # 設定ファイルは読み取り専用
//!
//! このクレートは `hakutaku.yaml` を読み取るだけで、**作成も上書きもしません**
//! （入力資料 8.2）。ファイルが存在しない場合は [`PreflightOutcome::Missing`] /
//! [`LoadOutcome::Missing`] を返すのみで、既定値のファイルを生成するようなことはしません。
//!
//! # 構文エラー・型不一致の扱い
//!
//! [`read_fixed_runtime_preference`] は、Tauri 初期化前に安全な既定へフォールバックし
//! ながら `WebView2Runtime` の解決を進めるための先行読み込みです
//! （`tasks/phase-01-bootstrap-webview2.md` の「起動手順の実装順序」手順1）。そのため、
//! YAML の構文エラーや対象キーの型不一致は [`PreflightOutcome::Undetermined`] として返し、
//! 呼び出し側（`bootstrap::runtime`）は**既定（[`FixedRuntimePreference::Auto`]）で
//! Runtime 解決を続行します**。設定ファイル全体を安全モードとして扱うかどうかの判断
//! （`CFG-016`）は [`load_config`] が行います。
//!
//! ファイルの読み取りと構文解析は、目的が異なるため独立した実装のままです
//! （[`read_fixed_runtime_preference`] は Tauri 初期化前に1項目だけを読む最小限の
//! 経路、[`load_config`] は設定ファイル全体を検証する経路）。一方、
//! `webview2.force_fixed_version_runtime` の値の**解釈**（真偽値と文字列
//! `"auto"` をどちらも自動判定・強制の指定として受理する規則）は、
//! [`interpret_fixed_runtime_preference`] へ切り出し、両者がこれを共有する
//! ことで受理範囲の食い違いを防いでいます（`tasks/phase-03-configuration.md`
//! 作業項目9）。
//!
//! # 設定ファイル全体のスキーマと起動時検証（P03）
//!
//! [`load_config`] は `hakutaku.yaml` 全体を読み込み、起動時検証した結果を
//! [`LoadOutcome`] として返します。3つの起動経路（正常起動・既定値起動・安全モード）は
//! `tasks/phase-03-configuration.md` の「設定の三つの起動経路」を参照してください。
//! パスの絶対性判定は [`classify_path`] / [`is_absolute_local_path`] が
//! ADR-0005（`docs/architecture/decisions/0005-config-path-validation.md`）の判定表に
//! 従って行います。パスの正規化（[`normalize_path_separators`] / [`paths_equivalent`]）は、
//! P05 のログ解析プロファイル照合が同じ規則を再利用できるよう公開しています。
//!
//! ## 「書いたのに使われない設定」も黙って通さない（Issue #39）
//!
//! `CFG-016` は誤設定を黙って既定値へ置換しないことを求めます。値の型・値域だけ
//! でなく、**設定として読まれない記述**も同じ理由で検証エラーにします。
//!
//! - `---` 区切りで2件以上の YAML ドキュメントが書かれている場合（設定として
//!   読むのは先頭の1件だけ）
//! - 同じマッピングに同じキーが2回以上書かれている場合（後に書いた値だけが
//!   使われ、先に書いた値は捨てられる。検出方法は `crate::duplicate_keys` の
//!   doc コメントを参照）
//!
//! また、`log_profiles[].ansi_codepage` に書かれたコードページが実行環境に存在
//! するかどうかは、確認手段（Win32 の `GetCPInfoExW`）を持つ呼び出し側から
//! [`load_config_with_codepage_check`] で注入し、起動時の一括提示へ合流させます。
//! このクレート自体は Win32 を直接呼びません。
//!
//! ## 文字コード指定の検証（Issue #38）
//!
//! `log_profiles[].encoding` の名前（`utf-8`／`windows-<コードページ番号>`）と、
//! コードページを文字コードとして選べるかの分類（`ENC-006` の UTF-16、厳密な
//! 判定ができないコードページ）は、形式判定層
//! （`hakutaku_format_detection::parse_named_encoding` /
//! `hakutaku_format_detection::codepage_rejection`）を唯一の実装として直接
//! 呼びます。どちらも実行環境に依存しない純粋な判断であり、注入する理由が
//! ありません。設定側へ同じ規則を書き写すと、両者が食い違ったときに「起動時
//! 検証は通ったのにファイルを開くと失敗する」状態を生むため、そうしません。
//!
//! `encoding` の名前指定と `ansi_codepage` の同時指定も起動時検証エラーです。
//! 消費側は `encoding` を優先して `ansi_codepage` を黙って無視するため、
//! 上記の「書いたのに使われない設定」と同じ扱いにしています。
//!
//! # ログ解析プロファイルの glob 照合（P05）
//!
//! [`glob_match`] / [`is_glob_pattern`]（`crate::glob`）は `LogProfileConfig::path_pattern`
//! の意味解釈（`LOG-021` の第2・第3段階）を担います。起動時検証（[`load_config`]）は
//! 同一優先度内の glob 重複、および完全一致パターンの重複を検出します
//! （`tasks/phase-05-log-parsing-core.md` 作業項目3）。実際の4段階解決
//! （手動指定 → 完全一致 → glob → 自動判定）は `crates/core-services` の
//! `resolve_profile` が、この関数群を使って行います。

mod duplicate_keys;
mod error;
mod glob;
mod load;
mod path;
mod schema;

pub use error::{ConfigError, ConfigErrors};
pub use glob::{glob_match, is_glob_pattern};
pub use load::{load_config, load_config_with_codepage_check, LoadOutcome};
pub use path::{
    classify_path, is_absolute_local_path, normalize_path_separators, paths_equivalent, PathKind,
};
pub use schema::{
    ClipboardConfig, DataSourceConfig, DateTimeFormatSetting, DiagnosticsConfig, EncodingSetting,
    FrontendConfig, HakutakuConfig, LogProfileConfig, MemoryConfig, PerformanceConfig,
    ProcessPriority, Webview2Config,
};

use std::path::Path;

use saphyr::{LoadableYamlNode, MarkedYaml};

/// 診断ログ用エラーコード（領域 `CFG`: 設定ファイルの読み込みと起動時検証）。
///
/// 書式と採番規則は `docs/development/error-codes.md` を正本とし、この
/// モジュールが領域 `CFG` の採番台帳である。番号は既存の最大値 + 1 で追加し、
/// 一度 `main` へマージした番号の意味変更と再利用はしない（欠番はコメントで
/// 残す）。利用者向けの意味と対処は `docs/deployment/error-codes.md` に載せる。
///
/// このクレート自身は診断ログへ書かない（`hakutaku-diagnostics` に依存しない）。
/// 定数を公開しているのは、失敗の意味を持つこのクレートを採番台帳とし、実際に
/// 記録する呼び出し側（`src-tauri` の起動手順）が同じ名前を参照できるように
/// するためである。
pub mod error_codes {
    /// `hakutaku.yaml` の起動時検証に失敗し、安全モードで起動した（`CFG-016`）。
    ///
    /// 起動そのものは継続するが、設定由来のデータソース・ログ解析プロファイル・
    /// キャッシュが無効になり、利用者側の対処（設定ファイルの修正）が必要な
    /// ため採番する。診断ログには、検証エラーの件数を含む1行に続けて、
    /// 各エラー（ファイル名・行・列・項目・理由）を1行ずつ記録する。
    ///
    /// `hakutaku.yaml` が存在しない既定値起動（`CFG-015`）は、利用者の対処を
    /// 要さない正常な起動経路のため採番しない（診断ログでは `code=-`）。
    pub const SAFE_MODE_START: &str = "HKT-CFG-0001";
}

/// 実行ファイルと同じフォルダに置かれる設定ファイルの固定名（`CFG-014`）。
///
/// 参照: `docs/security/data-handling.md`。
pub const CONFIG_FILE_NAME: &str = "hakutaku.yaml";

/// `webview2.force_fixed_version_runtime` の値（`CFG-023` / `DIST-017`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FixedRuntimePreference {
    /// 自動判定（既定）。導入済み Evergreen Runtime を優先する。
    #[default]
    Auto,
    /// Fixed Version Runtime を強制使用する。
    ForceFixedVersion,
}

/// `webview2.force_fixed_version_runtime` の先行読み込み結果。
#[derive(Clone, Debug)]
pub enum PreflightOutcome {
    /// 設定ファイルが存在しない。既定（`Auto`）で続行する。
    Missing,
    /// 値を確定できた。
    Determined(FixedRuntimePreference),
    /// 設定ファイルは存在するが、この先行読み込みでは値を確定できなかった。
    ///
    /// Runtime 解決は既定（`Auto`）で続行し、設定ファイル全体を安全モードとして
    /// 扱うかどうかの判断（`CFG-016`）は P03 が行う。
    Undetermined {
        /// 確定できなかった理由（日本語）。
        reason: String,
        /// 問題箇所の行番号（1始まり）。特定できない場合は `None`。
        line: Option<usize>,
        /// 問題箇所の列番号（1始まり）。特定できない場合は `None`。
        column: Option<usize>,
    },
}

impl PreflightOutcome {
    /// 確定できなかった場合（`Missing` / `Undetermined`）は `Auto` を返す。
    #[must_use]
    pub fn preference_or_default(&self) -> FixedRuntimePreference {
        match self {
            PreflightOutcome::Determined(preference) => *preference,
            PreflightOutcome::Missing | PreflightOutcome::Undetermined { .. } => {
                FixedRuntimePreference::Auto
            }
        }
    }
}

/// `config_path`（絶対パス）から `webview2.force_fixed_version_runtime` だけを読み取る。
///
/// ファイルを作成・上書きすることは一切ない（入力資料 8.2）。
///
/// # 判定規則
///
/// - ファイルが存在しない → [`PreflightOutcome::Missing`]
/// - ファイルを読み取れない（権限など） → [`PreflightOutcome::Undetermined`]（行・列なし）
/// - YAML の構文が壊れている → [`PreflightOutcome::Undetermined`]（行・列あり）
/// - `---` 区切りのドキュメントが2件以上ある → [`PreflightOutcome::Undetermined`]（行・列あり）
/// - `webview2` 節がない、または `force_fixed_version_runtime` キーがない
///   → [`PreflightOutcome::Determined`]`(Auto)`
/// - `force_fixed_version_runtime` の値が真偽値でも文字列 `"auto"` でもない
///   → [`PreflightOutcome::Undetermined`]（行・列あり）（[`interpret_fixed_runtime_preference`]
///   が受理範囲を判定する。[`load_config`] と共通の規則）
/// - `force_fixed_version_runtime: true` → [`PreflightOutcome::Determined`]`(ForceFixedVersion)`
/// - `force_fixed_version_runtime: false` または `"auto"` → [`PreflightOutcome::Determined`]`(Auto)`
#[must_use]
pub fn read_fixed_runtime_preference(config_path: &Path) -> PreflightOutcome {
    let contents = match std::fs::read_to_string(config_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return PreflightOutcome::Missing;
        }
        Err(error) => {
            return PreflightOutcome::Undetermined {
                reason: format!(
                    "設定ファイル {} を読み取れませんでした: {error}",
                    config_path.display()
                ),
                line: None,
                column: None,
            };
        }
    };

    let documents = match MarkedYaml::load_from_str(&contents) {
        Ok(documents) => documents,
        Err(scan_error) => {
            let marker = scan_error.marker();
            return PreflightOutcome::Undetermined {
                reason: format!("YAML の構文エラーです: {}", scan_error.info()),
                line: Some(marker.line()),
                column: Some(normalize_column(marker.col())),
            };
        }
    };

    // `---` 区切りで複数のドキュメントが書かれている場合、この先行読み込みが
    // 見る先頭のドキュメントに対象キーが無くても、2件目以降には書かれている
    // かもしれない。どちらを設定とみなすかを勝手に決めず、安全側
    // （既定の `Auto` で Runtime 解決を続行）へ倒す。利用者への提示は
    // [`load_config`] の一括検証が位置つきで行う（Issue #39）。
    if let Some(second) = documents.get(1) {
        let marker = second.span.start;
        return PreflightOutcome::Undetermined {
            reason: multiple_documents_reason(documents.len()),
            line: Some(marker.line()),
            column: Some(normalize_column(marker.col())),
        };
    }

    // ドキュメントが無い（内容が空など）場合は、webview2 節も無いものとして扱う。
    let Some(root) = documents.first() else {
        return PreflightOutcome::Determined(FixedRuntimePreference::Auto);
    };

    // `webview2` 節が無い、またはマッピングでない場合は「節が無い」として扱う。
    // このキー1つだけの先行読み込みでは、`webview2` 自体の型検証までは行わない
    // （設定ファイル全体の検証は P03 の役割）。
    let Some(webview2) = root.data.as_mapping_get("webview2") else {
        return PreflightOutcome::Determined(FixedRuntimePreference::Auto);
    };

    // 対象キーが無ければ既定（Auto）。
    let Some(value_node) = webview2.data.as_mapping_get("force_fixed_version_runtime") else {
        return PreflightOutcome::Determined(FixedRuntimePreference::Auto);
    };

    match interpret_fixed_runtime_preference(&value_node.data) {
        Some(preference) => PreflightOutcome::Determined(preference),
        None => {
            let marker = value_node.span.start;
            PreflightOutcome::Undetermined {
                reason: format!(
                    "webview2.force_fixed_version_runtime は真偽値または \"auto\" である必要がありますが、{}でした",
                    describe_yaml_kind(&value_node.data)
                ),
                line: Some(marker.line()),
                column: Some(normalize_column(marker.col())),
            }
        }
    }
}

/// `webview2.force_fixed_version_runtime` の値を解釈する（`CFG-023`）。
///
/// 真偽値（`true` = 強制、`false` = 自動判定）と、文字列 `"auto"`
/// （`tasks/phase-03-configuration.md` の記述例にある自動判定の明示指定）の
/// 両方を受理する。[`read_fixed_runtime_preference`]（先行読み込み）と
/// [`crate::load::load_config`]（全体検証）の両方がこの関数を経由することで、
/// 受理範囲の食い違いを防ぐ（`tasks/phase-03-configuration.md` 作業項目9）。
///
/// 受理できない場合は `None`（呼び出し側がそれぞれの文脈で理由文言を組み立てる）。
pub(crate) fn interpret_fixed_runtime_preference<'a>(
    data: &saphyr::YamlData<'a, MarkedYaml<'a>>,
) -> Option<FixedRuntimePreference> {
    if let Some(value) = data.as_bool() {
        return Some(if value {
            FixedRuntimePreference::ForceFixedVersion
        } else {
            FixedRuntimePreference::Auto
        });
    }
    if data.as_str() == Some("auto") {
        return Some(FixedRuntimePreference::Auto);
    }
    None
}

/// `---` 区切りで複数の YAML ドキュメントが書かれていた場合の理由文言
/// （`CFG-016`、Issue #39）。
///
/// [`read_fixed_runtime_preference`]（先行読み込み）と
/// [`crate::load::load_config`]（全体検証）は、複数ドキュメントに対する扱い
/// （前者は安全側へ倒す、後者は検証エラー）が異なるが、利用者が読む理由は
/// 同じ事実を指すため、文言をここで共有して食い違いを防ぐ。
pub(crate) fn multiple_documents_reason(count: usize) -> String {
    format!(
        "設定ファイルに YAML ドキュメントが {count} 件あります（`---` 区切り）。Hakutaku が設定として読むのは先頭の1件だけで、2件目以降は使われません。1件にまとめてください"
    )
}

/// `saphyr`（実体は `saphyr-parser`）の [`saphyr::Marker::col`] は、実装上
/// **0始まり**の値を返す（`saphyr::Marker::line` は既に1始まり）。本クレートの
/// 公開 API では行・列とも1始まりに統一するため、列だけここで +1 する。
///
/// [`crate::load`] モジュールの起動時検証（`CFG-016`）も同じ規則で位置情報を
/// 正規化するため、`pub(crate)` にしてクレート内から共有している。
pub(crate) fn normalize_column(marker_col: usize) -> usize {
    marker_col + 1
}

/// エラーメッセージ用に、YAML ノードの種類を日本語で簡潔に説明する。
///
/// このクレートで扱うノードは常に [`MarkedYaml`] なので、汎用化はせず具象型で受け取る。
/// [`crate::load`] モジュールの型検証エラーの理由文言でも共有する。
pub(crate) fn describe_yaml_kind<'a>(data: &saphyr::YamlData<'a, MarkedYaml<'a>>) -> &'static str {
    if data.is_string() {
        "文字列"
    } else if data.is_integer() {
        "整数"
    } else if data.is_floating_point() {
        "浮動小数点数"
    } else if data.is_null() {
        "null"
    } else if data.is_sequence() {
        "配列（シーケンス）"
    } else if data.is_mapping() {
        "マッピング"
    } else if data.is_alias() {
        "エイリアス"
    } else {
        "不明な値"
    }
}

#[cfg(test)]
mod tests {
    use super::{describe_yaml_kind, normalize_column};
    use saphyr::{MarkedYaml, Scalar, YamlData};

    #[test]
    fn normalize_column_converts_zero_indexed_marker_to_one_indexed() {
        // saphyr の Marker::col() は 0 始まりで返るため、+1 して1始まりへ正規化する。
        assert_eq!(normalize_column(0), 1);
        assert_eq!(normalize_column(4), 5);
    }

    #[test]
    fn describe_yaml_kind_reports_japanese_labels() {
        let string_node: YamlData<'_, MarkedYaml<'_>> = YamlData::Value(Scalar::String("x".into()));
        assert_eq!(describe_yaml_kind(&string_node), "文字列");

        let integer_node: YamlData<'_, MarkedYaml<'_>> = YamlData::Value(Scalar::Integer(1));
        assert_eq!(describe_yaml_kind(&integer_node), "整数");

        let null_node: YamlData<'_, MarkedYaml<'_>> = YamlData::Value(Scalar::Null);
        assert_eq!(describe_yaml_kind(&null_node), "null");

        let mapping_node: YamlData<'_, MarkedYaml<'_>> = YamlData::Mapping(Default::default());
        assert_eq!(describe_yaml_kind(&mapping_node), "マッピング");

        let sequence_node: YamlData<'_, MarkedYaml<'_>> = YamlData::Sequence(Vec::new());
        assert_eq!(describe_yaml_kind(&sequence_node), "配列（シーケンス）");
    }
}
