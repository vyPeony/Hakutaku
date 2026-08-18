//! `hakutaku.yaml` 全体の読み込みと起動時検証（`CFG-015`〜`CFG-017`）。
//!
//! [`load_config`] が3つの起動経路（`tasks/phase-03-configuration.md`）を表現する
//! [`LoadOutcome`] を返す。検証は最初の1件で止めず、**全項目を走査してエラーを
//! 収集する**（利用者が一度の起動確認ですべて直せるようにするため）。

use std::path::Path;

use saphyr::{LoadableYamlNode, MarkedYaml, Marker};

use crate::duplicate_keys::{collect_duplicate_keys, DuplicateKeys};
use crate::error::{ConfigError, ConfigErrors};
use crate::glob;
use crate::path::PathKind;
use crate::schema::{
    ClipboardConfig, DataSourceConfig, DateTimeFormatSetting, DiagnosticsConfig, EncodingSetting,
    FrontendConfig, HakutakuConfig, LogProfileConfig, MemoryConfig, PerformanceConfig,
    ProcessPriority, Webview2Config,
};
use crate::{describe_yaml_kind, normalize_column, path, FixedRuntimePreference};

/// `load_config` の結果。`hakutaku.yaml` の3つの起動経路に対応する
/// （`tasks/phase-03-configuration.md` の「設定の三つの起動経路」）。
#[derive(Clone, Debug)]
pub enum LoadOutcome {
    /// 正常起動。`hakutaku.yaml` があり、構文と値がすべて妥当だった。
    Loaded(HakutakuConfig),
    /// 既定値起動（`CFG-015`）。`hakutaku.yaml` が存在しない。
    ///
    /// 呼び出し側は組み込み既定値（[`HakutakuConfig::default`]）で起動し、
    /// 設定未検出を非致命的な通知として表示する。
    Missing,
    /// 安全モード（`CFG-016`）。`hakutaku.yaml` は存在するが構文または値が不正。
    ///
    /// 呼び出し側は誤設定を黙って既定値に置換せず、設定由来のデータソース・
    /// プロファイル・キャッシュを無効化した状態で UI を起動する。
    Invalid(ConfigErrors),
}

/// `config_path`（実行ファイルと同じフォルダの `hakutaku.yaml`。`CFG-014`）を読み込み、
/// 起動時検証する。
///
/// ファイルを作成・上書きすることは一切ない（入力資料 8.2）。
///
/// # 判定規則
///
/// - ファイルが存在しない → [`LoadOutcome::Missing`]（`CFG-015`）
/// - ファイルを読み取れない（権限など）、YAML の構文が壊れている、`---` 区切りの
///   ドキュメントが2件以上ある、同じキーが2回以上書かれている、または値の
///   検証に1件でも失敗した → [`LoadOutcome::Invalid`]（`CFG-016`）
/// - 全項目の検証に成功した → [`LoadOutcome::Loaded`]
///
/// `log_profiles[].ansi_codepage` は値域（1 以上）だけを検証し、その番号の
/// コードページが実行環境に存在するかは確認しない。存在確認まで起動時に
/// 一括提示したい呼び出し側は [`load_config_with_codepage_check`] を使う。
#[must_use]
pub fn load_config(config_path: &Path) -> LoadOutcome {
    load_config_with_codepage_check(config_path, &|_| true)
}

/// [`load_config`] に、コードページの存在確認を注入して実行する（`CFG-008`、
/// `ENC-007`、Issue #39）。
///
/// # なぜ注入するのか
///
/// `ansi_codepage` に書かれた番号が実行環境に存在するかは、Win32 の
/// `GetCPInfoExW` にしか答えられない。一方このクレートは `hakutaku.yaml` の
/// 解釈だけを担い、Win32 へは依存しない（ADR-0004 で `saphyr` 依存をこの
/// クレートに封じ込めたのと同じく、層の依存の向きを保つため）。そこで確認手段
/// だけを呼び出し側から受け取り、判定結果を他の値検証と同じ
/// [`ConfigError`] として**起動時に一括提示**する。
///
/// これが無いと、誤ったコードページ番号は「そのプロファイルが適用される
/// ファイルを実際に開いた時点」まで表面化せず、`CFG-016` の「誤設定を起動時に
/// まとめて提示する」から漏れる。
///
/// `codepage_exists` には、実行環境で使用できるコードページ番号に対して `true`
/// を返す関数を渡す（`hakutaku_core::codepage_available`）。
#[must_use]
pub fn load_config_with_codepage_check(
    config_path: &Path,
    codepage_exists: &dyn Fn(u32) -> bool,
) -> LoadOutcome {
    let file_name = config_path.display().to_string();

    let contents = match std::fs::read_to_string(config_path) {
        Ok(contents) => contents,
        // 既定値起動（`CFG-015`）へ倒せるのはファイルが「存在しない」場合だけである。
        // 権限などで読み取れない場合は、利用者が置いた設定の内容を確認できていない
        // 以上、既定値で起動すると誤設定を黙って通したことになり得るため、
        // 安全モード（`CFG-016`）側の Invalid として扱う。
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadOutcome::Missing;
        }
        Err(error) => {
            return LoadOutcome::Invalid(ConfigErrors::new(vec![ConfigError {
                file_name,
                line: None,
                column: None,
                item_path: String::new(),
                reason: format!("設定ファイルを読み取れませんでした: {error}"),
            }]));
        }
    };

    let documents = match MarkedYaml::load_from_str(&contents) {
        Ok(documents) => documents,
        Err(scan_error) => {
            let marker = scan_error.marker();
            return LoadOutcome::Invalid(ConfigErrors::new(vec![ConfigError {
                file_name,
                line: Some(marker.line()),
                column: Some(normalize_column(marker.col())),
                item_path: String::new(),
                reason: format!("YAML の構文エラーです: {}", scan_error.info()),
            }]));
        }
    };

    let mut validator = Validator::new(file_name, codepage_exists);
    // ファイル全体の構造に関する検証（ドキュメント数・重複キー）を、個々の項目の
    // 検証より先に行う。どちらも「書いたのに使われない設定がある」という同じ
    // 種類の誤りであり、利用者が最初に読む位置へ置くほうが直しやすいため。
    validator.validate_single_document(&documents);
    validator.validate_duplicate_keys(&contents, documents.first());
    // [`Validator`] は不正値でも既定値を埋めて走査を続けるため、`config` が
    // 組み立てられたこと自体は妥当性を意味しない。採否は収集したエラーの有無だけで
    // 決め、1件でもあれば組み立て済みの `config` は捨てる。
    let config = validator.validate_documents(&documents);

    if validator.errors.is_empty() {
        LoadOutcome::Loaded(config)
    } else {
        LoadOutcome::Invalid(ConfigErrors::new(validator.errors))
    }
}

/// `log_profiles` の重複パターン検証（[`Validator::validate_no_duplicate_patterns`]）の
/// ために、1件のエントリについて保持しておく記録。
///
/// 検証対象は「正規化後のパターン文字列そのものが一致するか」だけであり、
/// 値検証を通過した（＝絶対パスとして妥当な）エントリだけがここに積まれる。
struct PatternRecord {
    /// エラー報告で使う項目パス（例: `log_profiles[1].path_pattern`）。
    item_path: String,
    /// エラー報告で使う位置（`path_pattern` の値ノードの位置）。
    marker: Marker,
    /// 区切り文字統一 + 大文字化まで済ませたパターン文字列。
    normalized_pattern: String,
    /// glob 記号（`*`・`?`）を含むかどうか（[`crate::glob::is_glob_pattern`]）。
    is_glob: bool,
    /// このエントリの `priority`。
    priority: i64,
}

/// 検証エラーを収集しながら [`HakutakuConfig`] を組み立てる内部ヘルパー。
///
/// 個々の `validate_*` メソッドは、値が不正でもエラーを `errors` へ積んだうえで
/// 既定値を返す。これにより1件のエラーで止まらず、他の項目の検証を続けられる。
struct Validator<'check> {
    file_name: String,
    errors: Vec<ConfigError>,
    /// `log_profiles[].ansi_codepage` の存在確認（注入の理由は
    /// [`load_config_with_codepage_check`] の doc コメントを参照）。
    codepage_exists: &'check dyn Fn(u32) -> bool,
}

impl<'check> Validator<'check> {
    fn new(file_name: String, codepage_exists: &'check dyn Fn(u32) -> bool) -> Self {
        Self {
            file_name,
            errors: Vec::new(),
            codepage_exists,
        }
    }

    fn push(&mut self, marker: Marker, item_path: impl Into<String>, reason: impl Into<String>) {
        self.errors.push(ConfigError {
            file_name: self.file_name.clone(),
            line: Some(marker.line()),
            column: Some(normalize_column(marker.col())),
            item_path: item_path.into(),
            reason: reason.into(),
        });
    }

    /// `---` 区切りで複数の YAML ドキュメントが書かれていないことを検証する
    /// （`CFG-016`、Issue #39）。
    ///
    /// Hakutaku が設定として読むのは先頭のドキュメントだけである。2件目以降を
    /// 黙って読み飛ばすと、利用者は書いたはずの設定が使われていないことに
    /// 気づけないため、起動時検証エラーにする。末尾に区切りだけを置いた場合
    /// （2件目が空のドキュメントになる場合）も同じ扱いとし、「区切りより後は
    /// 使われない」ことを一律に示す。
    fn validate_single_document(&mut self, documents: &[MarkedYaml]) {
        let Some(second) = documents.get(1) else {
            return;
        };
        self.push(
            second.span.start,
            "",
            crate::multiple_documents_reason(documents.len()),
        );
    }

    /// 同じキーが2回以上書かれていないことを検証する（`CFG-016`、Issue #39）。
    ///
    /// 検出そのものは [`crate::duplicate_keys`] が別の走査で行う（`saphyr` が
    /// 組み立てた木では二重定義が1件へ潰れてしまう理由は、同モジュールの doc
    /// コメントを参照）。ここでは、その結果（項目パスとキー名）を
    /// [`MarkedYaml`] の木と突き合わせ、表示に使う行・列を補って積む。
    fn validate_duplicate_keys(&mut self, source: &str, root: Option<&MarkedYaml>) {
        let Some(root) = root else {
            return;
        };
        let duplicates = collect_duplicate_keys(source);
        if duplicates.is_empty() {
            return;
        }
        self.push_duplicate_key_errors(root, "", &duplicates);
    }

    /// [`Self::validate_duplicate_keys`] の再帰本体。
    ///
    /// `path` の組み立て規則は [`crate::duplicate_keys`] 側と一致させる必要が
    /// ある（両者の走査結果を突き合わせる鍵がこの文字列であるため）。
    fn push_duplicate_key_errors(
        &mut self,
        node: &MarkedYaml,
        path: &str,
        duplicates: &DuplicateKeys,
    ) {
        if let Some(mapping) = node.data.as_mapping() {
            let duplicated_here = duplicates.get(path);
            for (key_node, value_node) in mapping {
                let Some(key) = key_node.data.as_str() else {
                    continue;
                };
                let child_path = if path.is_empty() {
                    key.to_string()
                } else {
                    format!("{path}.{key}")
                };
                if duplicated_here.is_some_and(|keys| keys.iter().any(|known| known == key)) {
                    // `saphyr` は二重定義を「先に書いたキーのノード」と「後に
                    // 書いた値」の組へ潰す。したがってここで得られる位置は
                    // **先に書いたほう**であり、黙って捨てられる側を指す。
                    self.push(
                        key_node.span.start,
                        child_path.clone(),
                        format!(
                            "設定項目 {child_path} が2回以上指定されています。同じ項目を複数回書くと、後に書いた値だけが使われ、先に書いた値は黙って捨てられます。1か所だけ残してください"
                        ),
                    );
                }
                self.push_duplicate_key_errors(value_node, &child_path, duplicates);
            }
        } else if let Some(items) = node.data.as_vec() {
            for (index, item) in items.iter().enumerate() {
                self.push_duplicate_key_errors(item, &format!("{path}[{index}]"), duplicates);
            }
        }
    }

    fn validate_documents(&mut self, documents: &[MarkedYaml]) -> HakutakuConfig {
        let Some(root) = documents.first() else {
            // 内容が空（コメントのみ、空ファイル等）の場合は空マッピングとして扱う。
            // 結果として config_version 欠落のエラーになる（ファイル先頭を指す）。
            self.push(
                Marker::new(0, 1, 0),
                "config_version",
                "config_version が指定されていません。config_version: 1 を指定してください",
            );
            return HakutakuConfig::default();
        };
        self.validate_root_mapping(root)
    }

    fn validate_root_mapping(&mut self, root: &MarkedYaml) -> HakutakuConfig {
        let mut config = HakutakuConfig::default();
        let Some(mapping) = root.data.as_mapping() else {
            self.push(
                root.span.start,
                "",
                format!(
                    "設定ファイルの最上位はマッピングである必要がありますが、{}でした",
                    describe_yaml_kind(&root.data)
                ),
            );
            return config;
        };

        // `config_version` は既定値 1 を持つため、組み立て後の値だけでは「書かれて
        // いない」状態と「1 と書かれた」状態を区別できない。未指定を検証エラーに
        // するには、キーの出現有無をこのフラグで別に記録する必要がある。
        let mut seen_config_version = false;
        for (key_node, value_node) in mapping {
            let Some(key) = key_node.data.as_str() else {
                self.push(
                    key_node.span.start,
                    "",
                    "設定ファイルのキーは文字列である必要があります",
                );
                continue;
            };
            match key {
                "config_version" => {
                    seen_config_version = true;
                    config.config_version = self.validate_config_version(value_node);
                }
                "memory" => config.memory = self.validate_memory(value_node),
                "clipboard" => config.clipboard = self.validate_clipboard(value_node),
                "diagnostics" => config.diagnostics = self.validate_diagnostics(value_node),
                "frontend" => config.frontend = self.validate_frontend(value_node),
                "webview2" => config.webview2 = self.validate_webview2(value_node),
                "performance" => config.performance = self.validate_performance(value_node),
                "data_sources" => config.data_sources = self.validate_data_sources(value_node),
                "log_profiles" => config.log_profiles = self.validate_log_profiles(value_node),
                // 未知のキーは黙って読み飛ばさない。キー名の綴り誤りを無視すると、
                // 利用者は設定したつもりの値が既定値のまま使われていることに気づけず、
                // `CFG-016` の「誤設定を黙って既定値に置換しない」に反するため。
                other => self.push(
                    key_node.span.start,
                    other.to_string(),
                    format!("未知の設定項目です: {other}"),
                ),
            }
        }

        if !seen_config_version {
            self.push(
                root.span.start,
                "config_version",
                "config_version が指定されていません。config_version: 1 を指定してください",
            );
        }

        config
    }

    fn validate_config_version(&mut self, node: &MarkedYaml) -> u32 {
        match node.data.as_integer() {
            Some(1) => 1,
            // 1 より大きい値は、より新しい Hakutaku が書いた設定ファイルとみなし、
            // 「1 である必要があります」ではなく移行案内を返す（`tasks/phase-03-
            // configuration.md` 作業項目2の「将来の構造変更時に明確なエラーまたは
            // 移行案内を出せるようにします」）。0 以下は将来の形式ではあり得ないので、
            // 下の一般の値域エラーへ倒す。
            Some(value) if value > 1 => {
                self.push(
                    node.span.start,
                    "config_version",
                    format!(
                        "config_version の値 {value} はこのアプリでは扱えません。新しいバージョンの Hakutaku で開く必要があります"
                    ),
                );
                1
            }
            Some(value) => {
                self.push(
                    node.span.start,
                    "config_version",
                    format!("config_version は 1 である必要がありますが、{value} でした"),
                );
                1
            }
            None => {
                self.push(
                    node.span.start,
                    "config_version",
                    format!(
                        "config_version は整数である必要がありますが、{}でした",
                        describe_yaml_kind(&node.data)
                    ),
                );
                1
            }
        }
    }

    fn validate_memory(&mut self, node: &MarkedYaml) -> MemoryConfig {
        let mut config = MemoryConfig::default();
        let Some(mapping) = self.require_mapping(node, "memory") else {
            return config;
        };
        for (key_node, value_node) in mapping {
            let Some(key) = key_node.data.as_str() else {
                self.push(
                    key_node.span.start,
                    "memory",
                    "memory の子キーは文字列である必要があります",
                );
                continue;
            };
            match key {
                "budget_mib" => {
                    if let Some(value) = self.validate_u32(value_node, "memory.budget_mib", 1) {
                        config.budget_mib = value;
                    }
                }
                other => self.push_unknown_key(key_node, "memory", other),
            }
        }
        config
    }

    fn validate_clipboard(&mut self, node: &MarkedYaml) -> ClipboardConfig {
        let mut config = ClipboardConfig::default();
        let Some(mapping) = self.require_mapping(node, "clipboard") else {
            return config;
        };
        for (key_node, value_node) in mapping {
            let Some(key) = key_node.data.as_str() else {
                self.push(
                    key_node.span.start,
                    "clipboard",
                    "clipboard の子キーは文字列である必要があります",
                );
                continue;
            };
            match key {
                "max_copy_mib" => {
                    if let Some(value) = self.validate_u32(value_node, "clipboard.max_copy_mib", 1)
                    {
                        config.max_copy_mib = value;
                    }
                }
                "max_copy_lines" => {
                    if let Some(value) =
                        self.validate_u32(value_node, "clipboard.max_copy_lines", 1)
                    {
                        config.max_copy_lines = value;
                    }
                }
                other => self.push_unknown_key(key_node, "clipboard", other),
            }
        }
        config
    }

    fn validate_diagnostics(&mut self, node: &MarkedYaml) -> DiagnosticsConfig {
        let mut config = DiagnosticsConfig::default();
        let Some(mapping) = self.require_mapping(node, "diagnostics") else {
            return config;
        };
        for (key_node, value_node) in mapping {
            let Some(key) = key_node.data.as_str() else {
                self.push(
                    key_node.span.start,
                    "diagnostics",
                    "diagnostics の子キーは文字列である必要があります",
                );
                continue;
            };
            match key {
                "rotate_mib" => {
                    if let Some(value) = self.validate_u32(value_node, "diagnostics.rotate_mib", 1)
                    {
                        config.rotate_mib = value;
                    }
                }
                "keep_generations" => {
                    if let Some(value) =
                        self.validate_u32(value_node, "diagnostics.keep_generations", 1)
                    {
                        config.keep_generations = value;
                    }
                }
                other => self.push_unknown_key(key_node, "diagnostics", other),
            }
        }
        config
    }

    fn validate_frontend(&mut self, node: &MarkedYaml) -> FrontendConfig {
        let mut config = FrontendConfig::default();
        let Some(mapping) = self.require_mapping(node, "frontend") else {
            return config;
        };
        for (key_node, value_node) in mapping {
            let Some(key) = key_node.data.as_str() else {
                self.push(
                    key_node.span.start,
                    "frontend",
                    "frontend の子キーは文字列である必要があります",
                );
                continue;
            };
            match key {
                "max_rows" => {
                    if let Some(value) = self.validate_u32(value_node, "frontend.max_rows", 1) {
                        config.max_rows = value;
                    }
                }
                "max_mib" => {
                    if let Some(value) = self.validate_u32(value_node, "frontend.max_mib", 1) {
                        config.max_mib = value;
                    }
                }
                other => self.push_unknown_key(key_node, "frontend", other),
            }
        }
        config
    }

    fn validate_webview2(&mut self, node: &MarkedYaml) -> Webview2Config {
        let mut config = Webview2Config::default();
        let Some(mapping) = self.require_mapping(node, "webview2") else {
            return config;
        };
        for (key_node, value_node) in mapping {
            let Some(key) = key_node.data.as_str() else {
                self.push(
                    key_node.span.start,
                    "webview2",
                    "webview2 の子キーは文字列である必要があります",
                );
                continue;
            };
            match key {
                "force_fixed_version_runtime" => {
                    config.force_fixed_version_runtime =
                        self.validate_fixed_runtime_preference(value_node);
                }
                other => self.push_unknown_key(key_node, "webview2", other),
            }
        }
        config
    }

    /// `webview2.force_fixed_version_runtime` の値を解釈する。
    ///
    /// 受理範囲（真偽値、および `tasks/phase-03-configuration.md` の記述例にある
    /// 文字列 `"auto"`）の判定は [`crate::interpret_fixed_runtime_preference`]
    /// に委譲する。[`crate::read_fixed_runtime_preference`]（先行読み込み）と
    /// 同じ関数を経由することで、両者の受理範囲を一致させる（作業項目9）。
    fn validate_fixed_runtime_preference(&mut self, node: &MarkedYaml) -> FixedRuntimePreference {
        if let Some(preference) = crate::interpret_fixed_runtime_preference(&node.data) {
            return preference;
        }
        self.push(
            node.span.start,
            "webview2.force_fixed_version_runtime",
            format!(
                "webview2.force_fixed_version_runtime は真偽値または \"auto\" である必要がありますが、{}でした",
                describe_yaml_kind(&node.data)
            ),
        );
        FixedRuntimePreference::Auto
    }

    fn validate_performance(&mut self, node: &MarkedYaml) -> PerformanceConfig {
        let mut config = PerformanceConfig::default();
        let Some(mapping) = self.require_mapping(node, "performance") else {
            return config;
        };
        for (key_node, value_node) in mapping {
            let Some(key) = key_node.data.as_str() else {
                self.push(
                    key_node.span.start,
                    "performance",
                    "performance の子キーは文字列である必要があります",
                );
                continue;
            };
            match key {
                "parse_concurrency" => {
                    if let Some(value) =
                        self.validate_u32(value_node, "performance.parse_concurrency", 1)
                    {
                        config.parse_concurrency = value;
                    }
                }
                "io_interval_ms" => {
                    if let Some(value) =
                        self.validate_u32(value_node, "performance.io_interval_ms", 0)
                    {
                        config.io_interval_ms = value;
                    }
                }
                "process_priority" => {
                    config.process_priority = self.validate_process_priority(value_node);
                }
                other => self.push_unknown_key(key_node, "performance", other),
            }
        }
        config
    }

    fn validate_process_priority(&mut self, node: &MarkedYaml) -> ProcessPriority {
        match node.data.as_str() {
            Some("normal") => ProcessPriority::Normal,
            Some("below_normal") => ProcessPriority::BelowNormal,
            Some("idle") => ProcessPriority::Idle,
            Some(other) => {
                self.push(
                    node.span.start,
                    "performance.process_priority",
                    format!(
                        "performance.process_priority の値 \"{other}\" は不明です。normal / below_normal / idle のいずれかを指定してください"
                    ),
                );
                ProcessPriority::BelowNormal
            }
            None => {
                self.push(
                    node.span.start,
                    "performance.process_priority",
                    format!(
                        "performance.process_priority は文字列である必要がありますが、{}でした",
                        describe_yaml_kind(&node.data)
                    ),
                );
                ProcessPriority::BelowNormal
            }
        }
    }

    fn validate_data_sources(&mut self, node: &MarkedYaml) -> Vec<DataSourceConfig> {
        let Some(items) = self.require_sequence(node, "data_sources") else {
            return Vec::new();
        };

        // 名前の重複検出（`tasks/phase-03-configuration.md` P03-2 の「重複」）。
        // 名前は UI 表示とソース識別に使うため、同名の定義は起動時検証エラーに
        // する。glob パターンの意味的な重複（同優先度 glob の重複検出）は P05 の
        // 所有であり、ここでは扱わない。
        let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut result = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let prefix = format!("data_sources[{index}]");
            let Some(mapping) = self.require_mapping(item, &prefix) else {
                continue;
            };

            // 値の妥当性（`name`／`raw_path`）とキーの出現（`has_name`／`has_path`）を
            // 分けて持つ。キーが無い場合と、キーはあるが値が不正な場合とでは利用者へ
            // 示すべき理由が異なり、後者で「指定されていません」を重ねて積むと同じ
            // 誤りに対して2件のエラーが出てしまうため。
            let mut name: Option<String> = None;
            let mut has_name = false;
            let mut raw_path: Option<String> = None;
            let mut has_path = false;

            for (key_node, value_node) in mapping {
                let Some(key) = key_node.data.as_str() else {
                    self.push(
                        key_node.span.start,
                        prefix.as_str(),
                        "キーは文字列である必要があります",
                    );
                    continue;
                };
                match key {
                    "name" => {
                        has_name = true;
                        name =
                            self.validate_non_empty_string(value_node, &format!("{prefix}.name"));
                    }
                    "path" => {
                        has_path = true;
                        raw_path = self.validate_absolute_local_path_string(
                            value_node,
                            &format!("{prefix}.path"),
                        );
                    }
                    other => self.push_unknown_key(key_node, prefix.as_str(), other),
                }
            }

            if !has_name {
                self.push(
                    item.span.start,
                    format!("{prefix}.name"),
                    format!("{prefix}.name が指定されていません"),
                );
            }
            if !has_path {
                self.push(
                    item.span.start,
                    format!("{prefix}.path"),
                    format!("{prefix}.path が指定されていません"),
                );
            }

            // 両方が妥当だったエントリだけを採用する。`None` 側は検証時にエラーを
            // 積み済みであり、ここで既定値を補って採用すると、誤設定を黙って通した
            // 状態で起動してしまう（`CFG-016`）。
            if let (Some(name), Some(raw_path)) = (name, raw_path) {
                if !seen_names.insert(name.clone()) {
                    self.push(
                        item.span.start,
                        format!("{prefix}.name"),
                        format!("data_sources の名前 \"{name}\" が重複しています"),
                    );
                    continue;
                }
                result.push(DataSourceConfig {
                    name,
                    path: std::path::PathBuf::from(path::normalize_path_separators(&raw_path)),
                });
            }
        }
        result
    }

    fn validate_log_profiles(&mut self, node: &MarkedYaml) -> Vec<LogProfileConfig> {
        let Some(items) = self.require_sequence(node, "log_profiles") else {
            return Vec::new();
        };

        // 名前の重複検出（data_sources と同じ規則。glob の意味的な重複検出は P05）。
        let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        // priority・パターンの重複検証（validate_no_duplicate_patterns）用に、
        // 値検証を通過したエントリの記録を積んでおく。
        let mut pattern_records: Vec<PatternRecord> = Vec::new();
        let mut result = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let prefix = format!("log_profiles[{index}]");
            let Some(mapping) = self.require_mapping(item, &prefix) else {
                continue;
            };

            let mut name: Option<String> = None;
            let mut has_name = false;
            let mut path_pattern: Option<String> = None;
            let mut has_path_pattern = false;
            let mut path_pattern_marker: Option<Marker> = None;
            let mut priority: i64 = 0;
            let mut encoding = EncodingSetting::default();
            let mut ansi_codepage: Option<u32> = None;
            let mut datetime_format = DateTimeFormatSetting::default();

            for (key_node, value_node) in mapping {
                let Some(key) = key_node.data.as_str() else {
                    self.push(
                        key_node.span.start,
                        prefix.as_str(),
                        "キーは文字列である必要があります",
                    );
                    continue;
                };
                match key {
                    "name" => {
                        has_name = true;
                        name =
                            self.validate_non_empty_string(value_node, &format!("{prefix}.name"));
                    }
                    "path_pattern" => {
                        has_path_pattern = true;
                        path_pattern_marker = Some(value_node.span.start);
                        path_pattern = self.validate_absolute_local_path_string(
                            value_node,
                            &format!("{prefix}.path_pattern"),
                        );
                    }
                    "priority" => {
                        if let Some(value) =
                            self.validate_i64(value_node, &format!("{prefix}.priority"))
                        {
                            priority = value;
                        }
                    }
                    "encoding" => {
                        encoding =
                            self.validate_encoding(value_node, &format!("{prefix}.encoding"));
                    }
                    "ansi_codepage" => {
                        ansi_codepage = self
                            .validate_ansi_codepage(value_node, &format!("{prefix}.ansi_codepage"));
                    }
                    "datetime_format" => {
                        datetime_format = self.validate_datetime_format(
                            value_node,
                            &format!("{prefix}.datetime_format"),
                        );
                    }
                    other => self.push_unknown_key(key_node, prefix.as_str(), other),
                }
            }

            if !has_name {
                self.push(
                    item.span.start,
                    format!("{prefix}.name"),
                    format!("{prefix}.name が指定されていません"),
                );
            }
            if !has_path_pattern {
                self.push(
                    item.span.start,
                    format!("{prefix}.path_pattern"),
                    format!("{prefix}.path_pattern が指定されていません"),
                );
            }

            if let (Some(name), Some(path_pattern), Some(path_pattern_marker)) =
                (name, path_pattern, path_pattern_marker)
            {
                if !seen_names.insert(name.clone()) {
                    self.push(
                        item.span.start,
                        format!("{prefix}.name"),
                        format!("log_profiles の名前 \"{name}\" が重複しています"),
                    );
                    continue;
                }

                // 重複判定は、実際の照合と同じ土俵で行わなければ意味がない。P05 の
                // 解決は完全一致・glob とも「正規化したローカル絶対パスに対して
                // Windows と同様に大文字・小文字を区別せず」評価する
                // （`tasks/phase-05-log-parsing-core.md`）ため、ここでも区切り統一と
                // 大文字化を済ませた文字列を突き合わせる。
                let normalized_pattern =
                    path::normalize_path_separators(&path_pattern).to_uppercase();
                pattern_records.push(PatternRecord {
                    item_path: format!("{prefix}.path_pattern"),
                    marker: path_pattern_marker,
                    normalized_pattern,
                    is_glob: glob::is_glob_pattern(&path_pattern),
                    priority,
                });

                result.push(LogProfileConfig {
                    name,
                    path_pattern,
                    priority,
                    encoding,
                    ansi_codepage,
                    datetime_format,
                });
            }
        }

        self.validate_no_duplicate_patterns(&pattern_records);

        result
    }

    /// `log_profiles` の重複パターンを検証する
    /// （`tasks/phase-05-log-parsing-core.md` 作業項目3）。
    ///
    /// # 検証規則と根拠
    ///
    /// 「同じ優先度の複数の glob が同一のパスに一致し得るか」を一般に静的判定
    /// するのは、パターン同士の包含関係を総当たりで解く必要があり現実的では
    /// ない。そこでここでは、静的に確実に判定できる部分集合、すなわち
    /// **正規化後のパターン文字列が完全に一致する（＝同一設定の単純な二重定義）**
    /// 場合だけを起動時検証エラーにする。パターン文字列が異なる場合の潜在的な
    /// 重なり（例: `*.log` と `a*.log`）はここでは検出しない。この場合は
    /// `crates/core-services` の解決エンジンが、実際の対象パスに対して複数の
    /// glob が一致したときに `Ambiguous` を返す（`LOG-022` の「貪欲マッチで
    /// 推測しない」思想を起動時検証にも解決時にも一貫させている）。
    ///
    /// - **glob パターン**（[`crate::glob::is_glob_pattern`] が真）は、**同一
    ///   `priority` 内**でのみ重複を検証する。`priority` が異なる glob どうしは
    ///   解決時に優先度で一意に決まる（大きい方が勝つ）ため、パターン文字列が
    ///   同じでも重複とはみなさない
    /// - **完全一致パターン**（glob 記号を含まない）は、`priority` の値に
    ///   かかわらず常に重複を検証する。LOG-021 の解決順で絶対パス完全一致の
    ///   段階（第2段階）は `priority` を一切参照しない（glob 段階である第3段階
    ///   より前に確定する）ため、`priority` が異なっていても実行時の挙動は
    ///   変わらず、常に一意に決められない状態になる。これは work item 3 の
    ///   記述（glob の重複検証）を、より決定可能な完全一致の場合にも一貫して
    ///   広げた設計判断である。glob の重なりと異なり、文字列としての完全一致か
    ///   否かは常に静的に決定できるため、「一般には静的検出が困難」という制約が
    ///   そもそも当てはまらない
    fn validate_no_duplicate_patterns(&mut self, records: &[PatternRecord]) {
        // グルーピングキー: glob は (パターン, Some(priority))、完全一致は
        // (パターン, None) とすることで、上記2つの規則を1つの HashMap で表現する。
        let mut seen: std::collections::HashMap<(String, Option<i64>), &PatternRecord> =
            std::collections::HashMap::new();

        for record in records {
            let key = (
                record.normalized_pattern.clone(),
                if record.is_glob {
                    Some(record.priority)
                } else {
                    None
                },
            );
            if let Some(first) = seen.get(&key) {
                let reason = if record.is_glob {
                    format!(
                        "{} と同一優先度（priority: {}）で、正規化後のパターン文字列が同じ glob パターンです（大文字・小文字は区別しません）。解決時にどちらが選ばれるか一意に決められないため、起動時検証エラーとします",
                        first.item_path, record.priority
                    )
                } else {
                    format!(
                        "{} と正規化後のパターン文字列が同じ絶対パス完全一致の指定です（大文字・小文字は区別しません）。絶対パス完全一致の段階は priority を参照しないため、常に一意に決められません",
                        first.item_path
                    )
                };
                self.push(record.marker, record.item_path.clone(), reason);
            } else {
                seen.insert(key, record);
            }
        }
    }

    /// `log_profiles[].ansi_codepage` を検証する（`CFG-008`、`ENC-007`）。
    ///
    /// 値域（1 以上の `u32`）に加えて、その番号のコードページが**実行環境に
    /// 存在するか**も確認する（Issue #39）。存在確認の手段は呼び出し側から
    /// 注入する（理由は [`load_config_with_codepage_check`] を参照）。
    ///
    /// 存在しない番号は、そのプロファイルが適用されるファイルを開いた時点で
    /// どのみち失敗する。起動時に位置つきで示すほうが、利用者は「どの行を
    /// 直せばよいか」へ直接たどり着ける（`CFG-016`）。
    fn validate_ansi_codepage(&mut self, node: &MarkedYaml, item_path: &str) -> Option<u32> {
        let codepage = self.validate_u32(node, item_path, 1)?;
        if (self.codepage_exists)(codepage) {
            return Some(codepage);
        }
        self.push(
            node.span.start,
            item_path.to_string(),
            format!(
                "{item_path} の値 {codepage} は、この実行環境に存在しない Windows コードページです。実行環境で使用できるコードページ番号を指定してください"
            ),
        );
        None
    }

    fn validate_encoding(&mut self, node: &MarkedYaml, item_path: &str) -> EncodingSetting {
        match node.data.as_str() {
            Some("auto") => EncodingSetting::Auto,
            // 文字コード名が実在するかはここでは判定しない。判定できる知識を持つのは
            // 消費側（P05 の文字コード判定）であり、P03 が確定させるのはスキーマの形
            // までだからである（`tasks/phase-03-configuration.md` の「このフェーズが
            // 確定させるもの／させないもの」）。ここは非空であることだけを検証する。
            Some(value) if !value.is_empty() => EncodingSetting::Named(value.to_string()),
            Some(_) => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!("{item_path} を空文字列にはできません"),
                );
                EncodingSetting::Auto
            }
            None => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!(
                        "{item_path} は文字列である必要がありますが、{}でした",
                        describe_yaml_kind(&node.data)
                    ),
                );
                EncodingSetting::Auto
            }
        }
    }

    /// `log_profiles[].datetime_format` を検証する（`CFG-008`）。
    ///
    /// 受理する値は `auto` と `LOG-DT-001`〜`006`（[`DateTimeFormatSetting::
    /// SPECIFIED_IDS`]）だけであり、`performance.process_priority` と同じく
    /// 固定候補の完全一致で照合する。受理できない場合はエラーを積み、既定
    /// （自動判定）へ倒す。
    fn validate_datetime_format(
        &mut self,
        node: &MarkedYaml,
        item_path: &str,
    ) -> DateTimeFormatSetting {
        match node.data.as_str() {
            // 空文字列は下の `from_setting_str` へ渡す前に捕まえる。渡すと候補一覧
            // だけを並べた「値 "" は不明です」という理由になり、キーを書いたまま値を
            // 書き忘れたという実態が読み取れないため、専用の理由へ分ける。
            Some("") => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!("{item_path} を空文字列にはできません"),
                );
                DateTimeFormatSetting::Auto
            }
            Some(value) => match DateTimeFormatSetting::from_setting_str(value) {
                Some(setting) => setting,
                None => {
                    self.push(
                        node.span.start,
                        item_path.to_string(),
                        format!(
                            "{item_path} の値 \"{value}\" は不明です。auto / {} のいずれかを指定してください",
                            DateTimeFormatSetting::SPECIFIED_IDS.join(" / ")
                        ),
                    );
                    DateTimeFormatSetting::Auto
                }
            },
            None => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!(
                        "{item_path} は文字列である必要がありますが、{}でした",
                        describe_yaml_kind(&node.data)
                    ),
                );
                DateTimeFormatSetting::Auto
            }
        }
    }

    fn validate_non_empty_string(&mut self, node: &MarkedYaml, item_path: &str) -> Option<String> {
        match node.data.as_str() {
            Some(value) if !value.is_empty() => Some(value.to_string()),
            Some(_) => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!("{item_path} を空文字列にはできません"),
                );
                None
            }
            None => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!(
                        "{item_path} は文字列である必要がありますが、{}でした",
                        describe_yaml_kind(&node.data)
                    ),
                );
                None
            }
        }
    }

    /// パス（またはパスパターンの基点）が ADR-0005 の判定表に合格するかを検証する。
    fn validate_absolute_local_path_string(
        &mut self,
        node: &MarkedYaml,
        item_path: &str,
    ) -> Option<String> {
        let raw = match node.data.as_str() {
            Some(value) => value,
            None => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!(
                        "{item_path} は文字列である必要がありますが、{}でした",
                        describe_yaml_kind(&node.data)
                    ),
                );
                return None;
            }
        };
        // 空文字列は `classify_path` では相対パス（`PathKind::Relative`）と判定される。
        // 先に弾かないと「相対パスが指定されています」という、実態（値が空）と食い違う
        // 理由を利用者へ示すことになる。
        if raw.is_empty() {
            self.push(
                node.span.start,
                item_path.to_string(),
                format!("{item_path} を空文字列にはできません"),
            );
            return None;
        }
        let kind = path::classify_path(raw);
        if kind.is_allowed() {
            Some(raw.to_string())
        } else {
            self.push(
                node.span.start,
                item_path.to_string(),
                format!(
                    "{item_path} は絶対ローカルパスである必要があります（{}）。ADR-0005 により、相対パス、ネットワーク共有パス（UNC）、デバイス名前空間のパスには対応していません",
                    describe_path_kind(kind)
                ),
            );
            None
        }
    }

    /// 整数フィールドを検証し、`min` 以上かつ `u32` に収まる値だけを返す。
    ///
    /// オーバーフローし得る値は `u32::try_from` の checked 変換で扱う。
    fn validate_u32(&mut self, node: &MarkedYaml, item_path: &str, min: u32) -> Option<u32> {
        match node.data.as_integer() {
            // 下限判定を `u32::try_from` より先に行う。負値は変換にも失敗するため、
            // 順序を入れ替えると「値が大きすぎます」という実態と正反対の理由になる。
            Some(value) if value < i64::from(min) => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!(
                        "{item_path} は {min} 以上の整数である必要がありますが、{value} でした"
                    ),
                );
                None
            }
            Some(value) => match u32::try_from(value) {
                Ok(converted) => Some(converted),
                Err(_) => {
                    self.push(
                        node.span.start,
                        item_path.to_string(),
                        format!("{item_path} の値 {value} が大きすぎます"),
                    );
                    None
                }
            },
            None => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!(
                        "{item_path} は整数である必要がありますが、{}でした",
                        describe_yaml_kind(&node.data)
                    ),
                );
                None
            }
        }
    }

    /// 符号あり整数フィールドを検証する。`priority` のように負の値も許可する
    /// フィールド用（[`Self::validate_u32`] と異なり値域の下限を設けない）。
    fn validate_i64(&mut self, node: &MarkedYaml, item_path: &str) -> Option<i64> {
        match node.data.as_integer() {
            Some(value) => Some(value),
            None => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!(
                        "{item_path} は整数である必要がありますが、{}でした",
                        describe_yaml_kind(&node.data)
                    ),
                );
                None
            }
        }
    }

    /// `node` がマッピングであることを要求する。そうでなければエラーを積んで `None`。
    ///
    /// 戻り値の借用は `node`（`'n`）に紐づき、`&mut self`（このメソッドの借用）とは
    /// 独立している。両者を同一の省略ライフタイムに任せると、借用検査器は既定で
    /// `&mut self` 側を選んでしまうため、ここでは明示的に書き分けている。
    fn require_mapping<'n, 'a>(
        &mut self,
        node: &'n MarkedYaml<'a>,
        item_path: &str,
    ) -> Option<&'n saphyr::AnnotatedMapping<'a, MarkedYaml<'a>>> {
        match node.data.as_mapping() {
            Some(mapping) => Some(mapping),
            None => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!(
                        "{item_path} はマッピングである必要がありますが、{}でした",
                        describe_yaml_kind(&node.data)
                    ),
                );
                None
            }
        }
    }

    /// `node` が配列（シーケンス）であることを要求する。そうでなければエラーを積んで `None`。
    fn require_sequence<'n, 'a>(
        &mut self,
        node: &'n MarkedYaml<'a>,
        item_path: &str,
    ) -> Option<&'n Vec<MarkedYaml<'a>>> {
        match node.data.as_vec() {
            Some(items) => Some(items),
            None => {
                self.push(
                    node.span.start,
                    item_path.to_string(),
                    format!(
                        "{item_path} は配列である必要がありますが、{}でした",
                        describe_yaml_kind(&node.data)
                    ),
                );
                None
            }
        }
    }

    fn push_unknown_key(&mut self, key_node: &MarkedYaml, section: &str, key: &str) {
        self.push(
            key_node.span.start,
            format!("{section}.{key}"),
            format!("未知の設定項目です: {section}.{key}"),
        );
    }
}

/// [`PathKind`] のエラー種別を日本語で簡潔に説明する（許可される種別は呼び出されない）。
fn describe_path_kind(kind: PathKind) -> &'static str {
    match kind {
        PathKind::DriveAbsolute | PathKind::LocalVerbatim => "絶対ローカルパスです",
        PathKind::DriveRelative => "ドライブ相対パスが指定されています",
        PathKind::RootRelative => "ルート相対パスが指定されています",
        PathKind::Unc => "ネットワーク共有パス（UNC）が指定されています",
        PathKind::DeviceNamespace => "デバイス名前空間のパスが指定されています",
        PathKind::Relative => "相対パスが指定されています",
    }
}
