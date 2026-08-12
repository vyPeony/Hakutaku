//! `hakutaku.yaml` の型付きスキーマ（P03-1、`tasks/phase-03-configuration.md`）。
//!
//! 各フィールドの組み込み既定値は [`HakutakuConfig::default`] にまとめている。
//! 型・値域の検証は [`crate::load`] が行い、このモジュールは検証済みの値を
//! 保持する入れ物に徹する（`saphyr` の型はここには一切現れない。ADR-0004）。
//!
//! # スキーマに存在しない項目
//!
//! `CFG-011`（索引キャッシュ）・`CFG-013`（DB 認証情報）により、`cache` 区分、
//! キャッシュ関連の項目、DB 接続定義、認証情報の項目は**意図的に存在しない**。
//! 将来の拡張を見越した空の区分も置かない。

use std::path::PathBuf;

use crate::FixedRuntimePreference;

/// `hakutaku.yaml` 全体を表す、起動時検証を通過した設定値。
///
/// [`crate::load_config`] が正常系（[`crate::LoadOutcome::Loaded`]）で返す値であり、
/// 各消費フェーズ（メモリ予算は P02、クリップボードは P10、診断ログは P01、
/// フロントエンド保持上限は P08、資源抑制は P11）はこの構造体から型付きの値を
/// 受け取る想定である（受け渡し経路自体の実装は P03 の後続作業項目）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HakutakuConfig {
    /// 読み込み元の `config_version`。検証を通過した場合は常に `1`。
    pub config_version: u32,
    /// `memory` 区分（`CFG-007`）。
    pub memory: MemoryConfig,
    /// `clipboard` 区分（`CFG-018`）。
    pub clipboard: ClipboardConfig,
    /// `diagnostics` 区分（`CFG-020`）。
    pub diagnostics: DiagnosticsConfig,
    /// `frontend` 区分（`CFG-022`）。
    pub frontend: FrontendConfig,
    /// `webview2` 区分（`CFG-023` / `DIST-017`）。
    pub webview2: Webview2Config,
    /// `performance` 区分（`CFG-024`）。
    pub performance: PerformanceConfig,
    /// 事前定義されたデータソース（`CFG-003` / `PROD-006`）。既定は空。
    pub data_sources: Vec<DataSourceConfig>,
    /// ログ解析プロファイル（`CFG-008`）。既定は空。スキーマの**形**のみ P03 が確定し、
    /// 意味検証（同優先度 glob の重複検出など）は P05 が行う。
    pub log_profiles: Vec<LogProfileConfig>,
}

impl Default for HakutakuConfig {
    fn default() -> Self {
        Self {
            config_version: 1,
            memory: MemoryConfig::default(),
            clipboard: ClipboardConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            frontend: FrontendConfig::default(),
            webview2: Webview2Config::default(),
            performance: PerformanceConfig::default(),
            data_sources: Vec::new(),
            log_profiles: Vec::new(),
        }
    }
}

/// `memory` 区分（`CFG-007`）。ヒープ確保量を基準とするメモリ予算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryConfig {
    /// メモリ予算（MiB）。既定 2048（2 GiB）。
    pub budget_mib: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self { budget_mib: 2048 }
    }
}

/// `clipboard` 区分（`CFG-018`）。クリップボードコピーの上限。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipboardConfig {
    /// クリップボードコピーの最大バイト数（MiB 単位）。既定 16。
    pub max_copy_mib: u32,
    /// クリップボードコピーの最大行数。既定 10万。
    pub max_copy_lines: u32,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            max_copy_mib: 16,
            max_copy_lines: 100_000,
        }
    }
}

/// `diagnostics` 区分（`CFG-020`）。診断ログのローテーション。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagnosticsConfig {
    /// 診断ログのローテーションサイズ（MiB）。既定 10。
    pub rotate_mib: u32,
    /// 診断ログの保持世代数。既定 5。
    pub keep_generations: u32,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            rotate_mib: 10,
            keep_generations: 5,
        }
    }
}

/// `frontend` 区分（`CFG-022`）。
///
/// 初期値は「技術試作の実測により決定する」と定められており、P04 の実測
/// （30万行・連続スクロール2往復）に基づき現在の値で確定した。
/// 実測では最大保持 9,728 行（`max_rows` の 97%）・約 1.17 MB（`max_mib` の
/// 約 1.7%）であり、行数上限が実効的な歯止め、バイト数上限は長大行に対する
/// 安全網として機能する。運用先の実機での見直しは P13 の対象。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrontendConfig {
    /// フロントエンドが保持する最大行数。既定 10000（P04 実測で確定）。
    pub max_rows: u32,
    /// フロントエンドが保持する最大バイト数（MiB）。既定 64（P04 実測で確定）。
    pub max_mib: u32,
}

impl Default for FrontendConfig {
    fn default() -> Self {
        Self {
            max_rows: 10_000,
            max_mib: 64,
        }
    }
}

/// `webview2` 区分（`CFG-023` / `DIST-017`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Webview2Config {
    /// Fixed Version WebView2 Runtime を強制使用するか。既定は自動判定。
    ///
    /// P01 が起動ブートストラップで先行読み込みする既存キー
    /// （[`crate::read_fixed_runtime_preference`]）と同じ型 [`FixedRuntimePreference`]
    /// を再利用し、値の解釈が二か所で食い違わないようにしている。
    pub force_fixed_version_runtime: FixedRuntimePreference,
}

impl Default for Webview2Config {
    fn default() -> Self {
        Self {
            force_fixed_version_runtime: FixedRuntimePreference::Auto,
        }
    }
}

/// `performance` 区分（`CFG-024`）。解析の資源抑制。
///
/// 既定値は「運用先の専用端末での実行を前提に控えめに定める」とされているが、具体値の
/// 確定は P13（実機での実測）が行う。ここに置くのはそれまでの暫定既定値である。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerformanceConfig {
    /// 解析の同時実行数。暫定既定 2。
    pub parse_concurrency: u32,
    /// 読み込み時の I/O 発行間隔（ミリ秒）。暫定既定 0（間隔を空けない）。
    pub io_interval_ms: u32,
    /// Hakutaku プロセスの優先度。暫定既定 `below_normal`。
    pub process_priority: ProcessPriority,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            parse_concurrency: 2,
            io_interval_ms: 0,
            process_priority: ProcessPriority::BelowNormal,
        }
    }
}

/// Hakutaku プロセスの優先度（`CFG-024`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ProcessPriority {
    /// 通常優先度。
    Normal,
    /// 通常より低い優先度（既定）。対象端末の他プロセスを優先する。
    #[default]
    BelowNormal,
    /// アイドル優先度。
    Idle,
}

/// 事前定義されたデータソース1件（`CFG-003` / `PROD-006`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataSourceConfig {
    /// データソースの表示名。
    pub name: String,
    /// 読み込み先の絶対ローカルパス。
    ///
    /// 起動時検証で ADR-0005 の判定表（ドライブレター絶対パス、または
    /// `\\?\C:\...` 形式のローカル verbatim パスのみ許可）に合格した値のみが
    /// ここに入る。区切り文字は [`crate::normalize_path_separators`] により
    /// `\` へ統一済み。
    pub path: PathBuf,
}

/// ログ解析プロファイル1件（`CFG-008`）。
///
/// スキーマの**形**を P03 が確定させる（`VER-007`）。プロファイルの意味解釈・照合
/// （同優先度 glob の重複検出、解決順の妥当性など）は P05 が行う。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogProfileConfig {
    /// プロファイルの表示名。
    pub name: String,
    /// 対象ファイルの glob パターン、または絶対パス完全一致の指定。
    ///
    /// パターンの基点（ワイルドカードより前の部分）は絶対ローカルパスである
    /// 必要がある（ADR-0005）。
    ///
    /// # 完全一致指定（glob 記号を含まない場合）
    ///
    /// `path_pattern` が glob 記号（`*`・`?`）を**含まない**場合、値全体を
    /// 絶対パス完全一致の指定として扱う（`LOG-021` の第2段階）。スキーマに
    /// 専用フィールドを追加しない設計判断であり（`tasks/phase-05-log-parsing-core.md`
    /// 作業項目3）、含むかどうかの判定は [`crate::glob::is_glob_pattern`] が行う。
    /// 含む場合は glob として扱う（`LOG-021` の第3段階、[`crate::glob::glob_match`]）。
    ///
    /// この値自体は読み込み時に正規化しない。区切り文字の統一・大文字小文字を
    /// 区別しない比較は、照合時に [`crate::path::normalize_path_separators`]・
    /// [`crate::path::paths_equivalent`]・[`crate::glob::glob_match`] が行う。
    pub path_pattern: String,
    /// glob 解決（`LOG-021` の第3段階）での優先度。既定 0。値が大きいほど優先。
    ///
    /// 絶対パス完全一致の段階（第2段階）はこの値を参照しない。完全一致は
    /// glob より常に優先されるため、完全一致どうしの優劣を `priority` で
    /// 決める必要がないからである。
    pub priority: i64,
    /// 文字コード指定（`CFG-008`）。既定は自動判定。
    pub encoding: EncodingSetting,
    /// 明示的な Windows ANSI コードページ識別子（`CFG-008`）。
    ///
    /// 未指定（`None`）の場合のみ、実行環境の Windows ANSI コードページを使用する。
    pub ansi_codepage: Option<u32>,
    /// 日時書式指定（`CFG-008`）。既定は自動判定。
    ///
    /// 明示指定した場合、消費側（P05 の解析経路）は内容による自動判定を
    /// 行わず、この書式だけで解析する。
    pub datetime_format: DateTimeFormatSetting,
}

/// ログ解析プロファイルの文字コード指定（`CFG-008`）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum EncodingSetting {
    /// 自動判定（既定）。
    #[default]
    Auto,
    /// 明示された文字コード名（例: `shift_jis`、`utf-8`）。
    ///
    /// 文字コード名の妥当性検証（実際に解釈可能かどうか）は消費側（P05）が行う。
    /// P03 は非空の文字列であることのみを検証する。
    Named(String),
}

/// ログ解析プロファイルの日時書式指定（`CFG-008`）。
///
/// 既定は自動判定（[`DateTimeFormatSetting::Auto`]）であり、このキーを書かない
/// プロファイルの挙動は従来と変わらない。`LOG-DT-001`〜`006` のいずれかを明示
/// した場合、消費側（P05 の解析経路）は内容による自動判定を行わず、その書式
/// だけでファイル全体を解析する。
///
/// # 明示指定できるようにした理由
///
/// `LOG-DT-004`（`YYYY/MM/DD HH:mm:ss:SS`）だけで構成されるファイルは、内容に
/// よる自動判定では必ず `LOG-DT-005` とも同時に成立する（`crates/parser` の
/// 曖昧性検出の設計）。`LOG-022` に従って貪欲マッチで推測しないため、この書式
/// のログは自動判定だけでは日時未解析の生表示にしかならない。どちらの書式で
/// 記録されたログなのかを知っているのは利用者だけなので、設定で明示できる経路
/// を用意した。
///
/// # 書式そのものを持たない理由（依存の向き）
///
/// このクレートは `hakutaku-parser` に依存せず YAML 解釈だけを担うため
/// （逆向きの依存もない）、書式の列挙をここで独自に持つ。`hakutaku_parser::
/// LogDateTimeFormat` への写像は消費側（`crates/core-services`）が行う。要件 ID
/// の綴りが両者で食い違わないことは、その写像の単体テストが守る。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DateTimeFormatSetting {
    /// 自動判定（既定）。ファイルの内容から書式を判定する。
    #[default]
    Auto,
    /// `YYYY/MM/DD HH:mm:ss.SSS`（ミリ秒）。
    LogDt001,
    /// `YYYY-MM-DD HH:mm:ss:SSS`（ミリ秒）。
    LogDt002,
    /// `YYYY/MM/DD HH:mm:ss.SS`（1/100秒）。
    LogDt003,
    /// `YYYY/MM/DD HH:mm:ss:SS`（1/100秒）。
    LogDt004,
    /// `YYYY/MM/DD HH:mm:ss`（秒）。
    LogDt005,
    /// `YYYY/MM/DD HH:mm`（分）。
    LogDt006,
}

impl DateTimeFormatSetting {
    /// 明示指定として受理する書式 ID の一覧（要件 ID の綴りそのもの）。
    ///
    /// 起動時検証のエラー文言もこの一覧から組み立てるため、受理する値と利用者
    /// へ案内する値が食い違わない。
    pub const SPECIFIED_IDS: [&'static str; 6] = [
        "LOG-DT-001",
        "LOG-DT-002",
        "LOG-DT-003",
        "LOG-DT-004",
        "LOG-DT-005",
        "LOG-DT-006",
    ];

    /// 設定ファイルへ書かれた値を解釈する。
    ///
    /// `auto` は [`DateTimeFormatSetting::Auto`]（既定と同じ自動判定を明示した
    /// 場合）として受理する。`encoding: auto`・`webview2.
    /// force_fixed_version_runtime: auto` と同じ書き方に揃えている。
    /// 要件 ID は大文字・小文字を区別する完全一致で照合する
    /// （`performance.process_priority` と同じ厳密さ）。
    ///
    /// 受理できない値は `None`（呼び出し側がそれぞれの文脈で理由文言を組み立てる）。
    #[must_use]
    pub fn from_setting_str(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(DateTimeFormatSetting::Auto),
            "LOG-DT-001" => Some(DateTimeFormatSetting::LogDt001),
            "LOG-DT-002" => Some(DateTimeFormatSetting::LogDt002),
            "LOG-DT-003" => Some(DateTimeFormatSetting::LogDt003),
            "LOG-DT-004" => Some(DateTimeFormatSetting::LogDt004),
            "LOG-DT-005" => Some(DateTimeFormatSetting::LogDt005),
            "LOG-DT-006" => Some(DateTimeFormatSetting::LogDt006),
            _ => None,
        }
    }

    /// 明示指定の場合に要件 ID を返す。[`DateTimeFormatSetting::Auto`] は `None`。
    #[must_use]
    pub fn id(&self) -> Option<&'static str> {
        match self {
            DateTimeFormatSetting::Auto => None,
            DateTimeFormatSetting::LogDt001 => Some("LOG-DT-001"),
            DateTimeFormatSetting::LogDt002 => Some("LOG-DT-002"),
            DateTimeFormatSetting::LogDt003 => Some("LOG-DT-003"),
            DateTimeFormatSetting::LogDt004 => Some("LOG-DT-004"),
            DateTimeFormatSetting::LogDt005 => Some("LOG-DT-005"),
            DateTimeFormatSetting::LogDt006 => Some("LOG-DT-006"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DateTimeFormatSetting;

    // 受け入れ条件: 受理する書式 ID の一覧（SPECIFIED_IDS）と、解釈
    // （from_setting_str）・逆変換（id）が互いに食い違わない。エラー文言は
    // SPECIFIED_IDS から組み立てるため、案内した値がすべて実際に受理される
    // ことをここで担保する。
    #[test]
    fn specified_ids_round_trip_through_from_setting_str_and_id() {
        for id in DateTimeFormatSetting::SPECIFIED_IDS {
            let setting = DateTimeFormatSetting::from_setting_str(id)
                .expect("SPECIFIED_IDS の値はすべて受理されるはず");
            assert_eq!(setting.id(), Some(id));
        }
    }

    // 受け入れ条件: auto は自動判定（既定）として受理し、id() は None を返す。
    #[test]
    fn auto_is_accepted_and_has_no_requirement_id() {
        assert_eq!(
            DateTimeFormatSetting::from_setting_str("auto"),
            Some(DateTimeFormatSetting::Auto)
        );
        assert_eq!(
            DateTimeFormatSetting::default(),
            DateTimeFormatSetting::Auto
        );
        assert_eq!(DateTimeFormatSetting::Auto.id(), None);
    }

    // 受け入れ条件: 要件 ID は大文字・小文字を区別する完全一致で照合する。
    #[test]
    fn unknown_and_differently_cased_values_are_rejected() {
        assert_eq!(DateTimeFormatSetting::from_setting_str("log-dt-004"), None);
        assert_eq!(DateTimeFormatSetting::from_setting_str("LOG-DT-007"), None);
        assert_eq!(DateTimeFormatSetting::from_setting_str(""), None);
    }
}
