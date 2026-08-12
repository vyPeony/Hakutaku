//! `hakutaku_config::load_config` の受け入れ確認（P03-1）。
//!
//! `tasks/phase-03-configuration.md` の受け入れ条件に対応する単体テストを、各項目に
//! `CFG-0xx` のコメントを添えて並べる。ADR-0004（saphyr 採用の確定）と ADR-0005
//! （絶対ローカルパス以外は起動時検証エラー）の判定も併せて確認する。

use std::path::PathBuf;

use hakutaku_config::{
    load_config, DateTimeFormatSetting, EncodingSetting, FixedRuntimePreference, HakutakuConfig,
    LoadOutcome, ProcessPriority,
};

/// 一意な一時ファイルパスを作る（`std::env::temp_dir()` 配下。テスト専用の一時領域であり、
/// アプリ本体が実行時に書き込む対象ではない）。`crates/config/tests/preflight.rs` と
/// 同じ方式。
fn temp_yaml_path(label: &str) -> PathBuf {
    let unique = format!(
        "hakutaku-config-schema-test-{label}-{}-{:?}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

/// 指定した内容の一時 YAML ファイルへ書き込んで `load_config` を呼び、後片付けまで行う。
fn load_from_contents(label: &str, contents: &str) -> LoadOutcome {
    let path = temp_yaml_path(label);
    std::fs::write(&path, contents).unwrap();
    let outcome = load_config(&path);
    std::fs::remove_file(&path).unwrap();
    outcome
}

fn expect_loaded(label: &str, contents: &str) -> HakutakuConfig {
    match load_from_contents(label, contents) {
        LoadOutcome::Loaded(config) => config,
        other => panic!("Loaded を期待しましたが {other:?} でした"),
    }
}

fn expect_invalid(label: &str, contents: &str) -> hakutaku_config::ConfigErrors {
    match load_from_contents(label, contents) {
        LoadOutcome::Invalid(errors) => errors,
        other => panic!("Invalid を期待しましたが {other:?} でした"),
    }
}

// ---------------------------------------------------------------------
// 既定値起動（CFG-015）とファイル未検出。
// ---------------------------------------------------------------------

#[test]
fn missing_file_returns_missing_outcome() {
    // CFG-015: hakutaku.yaml が存在しない初回起動でも、組み込み既定値で起動できること。
    let path = temp_yaml_path("missing");
    assert!(!path.exists());
    let outcome = load_config(&path);
    assert!(matches!(outcome, LoadOutcome::Missing));
}

// ---------------------------------------------------------------------
// 既定値の確認。
// ---------------------------------------------------------------------

#[test]
fn empty_content_uses_all_builtin_defaults_but_is_invalid_without_config_version() {
    // 空の入力（config_version 欠落）は安全モード（CFG-016）になるが、その際に返る
    // 既定値そのものは HakutakuConfig::default() と一致することを確認する。
    let defaults = HakutakuConfig::default();
    assert_eq!(defaults.config_version, 1);
    assert_eq!(defaults.memory.budget_mib, 2048); // CFG-007
    assert_eq!(defaults.clipboard.max_copy_mib, 16); // CFG-018
    assert_eq!(defaults.clipboard.max_copy_lines, 100_000); // CFG-018
    assert_eq!(defaults.diagnostics.rotate_mib, 10); // CFG-020
    assert_eq!(defaults.diagnostics.keep_generations, 5); // CFG-020
    assert_eq!(defaults.frontend.max_rows, 10_000); // CFG-022(暫定)
    assert_eq!(defaults.frontend.max_mib, 64); // CFG-022(暫定)
    assert_eq!(
        defaults.webview2.force_fixed_version_runtime,
        FixedRuntimePreference::Auto
    ); // CFG-023
    assert_eq!(defaults.performance.parse_concurrency, 2); // CFG-024(暫定)
    assert_eq!(defaults.performance.io_interval_ms, 0); // CFG-024(暫定)
    assert_eq!(
        defaults.performance.process_priority,
        ProcessPriority::BelowNormal
    ); // CFG-024(暫定)
    assert!(defaults.data_sources.is_empty()); // CFG-003
    assert!(defaults.log_profiles.is_empty()); // CFG-008

    // 空ファイルは config_version が無いため安全モード（CFG-016）になる。
    let errors = expect_invalid("empty-content", "");
    assert_eq!(errors.len(), 1);
    assert!(errors.as_slice()[0].item_path.contains("config_version"));
}

// ---------------------------------------------------------------------
// 正常系: 全セクション指定の YAML が型付きで読める。
// ---------------------------------------------------------------------

#[test]
fn full_config_reads_all_sections_with_correct_types() {
    let config = expect_loaded(
        "full",
        r#"
config_version: 1

memory:
  budget_mib: 4096

clipboard:
  max_copy_mib: 32
  max_copy_lines: 50000

diagnostics:
  rotate_mib: 20
  keep_generations: 3

frontend:
  max_rows: 5000
  max_mib: 128

webview2:
  force_fixed_version_runtime: true

performance:
  parse_concurrency: 4
  io_interval_ms: 10
  process_priority: idle

data_sources:
  - name: device_logs
    path: "C:/Device/Logs"

log_profiles:
  - name: japanese_device_log
    path_pattern: "C:/Device/Logs/*.log"
    priority: 5
    encoding: auto
    ansi_codepage: 932
"#,
    );

    assert_eq!(config.config_version, 1);
    assert_eq!(config.memory.budget_mib, 4096);
    assert_eq!(config.clipboard.max_copy_mib, 32);
    assert_eq!(config.clipboard.max_copy_lines, 50_000);
    assert_eq!(config.diagnostics.rotate_mib, 20);
    assert_eq!(config.diagnostics.keep_generations, 3);
    assert_eq!(config.frontend.max_rows, 5000);
    assert_eq!(config.frontend.max_mib, 128);
    assert_eq!(
        config.webview2.force_fixed_version_runtime,
        FixedRuntimePreference::ForceFixedVersion
    );
    assert_eq!(config.performance.parse_concurrency, 4);
    assert_eq!(config.performance.io_interval_ms, 10);
    assert_eq!(config.performance.process_priority, ProcessPriority::Idle);

    assert_eq!(config.data_sources.len(), 1);
    assert_eq!(config.data_sources[0].name, "device_logs");
    assert_eq!(
        config.data_sources[0].path,
        PathBuf::from(r"C:\Device\Logs")
    );

    assert_eq!(config.log_profiles.len(), 1);
    assert_eq!(config.log_profiles[0].name, "japanese_device_log");
    assert_eq!(config.log_profiles[0].path_pattern, "C:/Device/Logs/*.log");
    assert_eq!(config.log_profiles[0].priority, 5);
    assert_eq!(config.log_profiles[0].encoding, EncodingSetting::Auto);
    assert_eq!(config.log_profiles[0].ansi_codepage, Some(932));
}

#[test]
fn log_profile_priority_defaults_to_zero_when_omitted() {
    // priority を省略した場合の既定値は 0（tasks/phase-05-log-parsing-core.md
    // 「プロファイルの対応付け」節）。
    let config = expect_loaded(
        "priority-default",
        "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n",
    );
    assert_eq!(config.log_profiles[0].priority, 0);
}

#[test]
fn log_profile_priority_accepts_negative_values() {
    // priority は i64（下限を設けない）。値が小さいほど優先度が低い。
    let config = expect_loaded(
        "priority-negative",
        "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n    priority: -3\n",
    );
    assert_eq!(config.log_profiles[0].priority, -3);
}

#[test]
fn log_profile_priority_non_integer_is_invalid() {
    let errors = expect_invalid(
        "priority-non-integer",
        "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n    priority: \"high\"\n",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "log_profiles[0].priority"));
}

#[test]
fn webview2_accepts_auto_string_as_well_as_boolean() {
    // tasks/phase-03-configuration.md の記述例にある文字列 "auto" も
    // 真偽値 false と同じ意味（自動判定）として受け付ける。
    let config = expect_loaded(
        "webview2-auto-string",
        "config_version: 1\nwebview2:\n  force_fixed_version_runtime: auto\n",
    );
    assert_eq!(
        config.webview2.force_fixed_version_runtime,
        FixedRuntimePreference::Auto
    );
}

#[test]
fn log_profile_named_encoding_is_preserved() {
    let config = expect_loaded(
        "named-encoding",
        r#"
config_version: 1
log_profiles:
  - name: shift_jis_log
    path_pattern: "C:/Device/Logs/*.log"
    encoding: shift_jis
"#,
    );
    assert_eq!(
        config.log_profiles[0].encoding,
        EncodingSetting::Named("shift_jis".to_string())
    );
    // ansi_codepage 省略時は None（未指定時のみ実行環境の ANSI を使用する。CFG-008）。
    assert_eq!(config.log_profiles[0].ansi_codepage, None);
    // datetime_format 省略時は自動判定（従来どおりの挙動）。
    assert_eq!(
        config.log_profiles[0].datetime_format,
        DateTimeFormatSetting::Auto
    );
}

// ---------------------------------------------------------------------
// log_profiles[].datetime_format（CFG-008）。
// ---------------------------------------------------------------------

// 受け入れ条件: datetime_format に要件 ID を書くと、その書式指定が保持される
// （消費側が自動判定を飛ばして解析するための入力になる）。
#[test]
fn log_profile_datetime_format_is_preserved() {
    let config = expect_loaded(
        "datetime-format",
        r#"
config_version: 1
log_profiles:
  - name: dt_004_log
    path_pattern: "C:/Device/Logs/*.log"
    datetime_format: LOG-DT-004
"#,
    );
    assert_eq!(
        config.log_profiles[0].datetime_format,
        DateTimeFormatSetting::LogDt004
    );
}

// 受け入れ条件: auto を明示した場合も既定（自動判定）と同じ値になる
// （encoding: auto と同じ書き方に揃えている）。
#[test]
fn log_profile_datetime_format_auto_is_accepted() {
    let config = expect_loaded(
        "datetime-format-auto",
        r#"
config_version: 1
log_profiles:
  - name: auto_log
    path_pattern: "C:/Device/Logs/*.log"
    datetime_format: auto
"#,
    );
    assert_eq!(
        config.log_profiles[0].datetime_format,
        DateTimeFormatSetting::Auto
    );
}

// 受け入れ条件: 6書式すべての要件 ID を受理する（案内する値と受理する値が
// 食い違わない）。
#[test]
fn log_profile_datetime_format_accepts_all_six_ids() {
    let expected = [
        DateTimeFormatSetting::LogDt001,
        DateTimeFormatSetting::LogDt002,
        DateTimeFormatSetting::LogDt003,
        DateTimeFormatSetting::LogDt004,
        DateTimeFormatSetting::LogDt005,
        DateTimeFormatSetting::LogDt006,
    ];
    for (index, id) in DateTimeFormatSetting::SPECIFIED_IDS.iter().enumerate() {
        let config = expect_loaded(
            "datetime-format-all",
            &format!(
                "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n    datetime_format: {id}\n"
            ),
        );
        assert_eq!(config.log_profiles[0].datetime_format, expected[index]);
    }
}

// 受け入れ条件: 不明な値は安全モード（CFG-016）になり、理由に受理できる値の
// 一覧が示される。
#[test]
fn log_profile_datetime_format_unknown_value_is_invalid() {
    let errors = expect_invalid(
        "datetime-format-unknown",
        "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n    datetime_format: LOG-DT-007\n",
    );
    let error = errors
        .iter()
        .find(|error| error.item_path == "log_profiles[0].datetime_format")
        .expect("datetime_format のエラーが積まれるはず");
    assert!(error.reason.contains("LOG-DT-007"));
    assert!(error.reason.contains("LOG-DT-004"));
    assert!(error.reason.contains("auto"));
}

// 受け入れ条件: 要件 ID は大文字・小文字を区別する（黙って受理して別の書式で
// 解析することがない）。
#[test]
fn log_profile_datetime_format_is_case_sensitive() {
    let errors = expect_invalid(
        "datetime-format-lowercase",
        "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n    datetime_format: log-dt-004\n",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "log_profiles[0].datetime_format"));
}

// 受け入れ条件: 文字列以外（型不一致）は理由に値の種類を示して安全モードにする。
#[test]
fn log_profile_datetime_format_non_string_is_invalid() {
    let errors = expect_invalid(
        "datetime-format-non-string",
        "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n    datetime_format: 4\n",
    );
    let error = errors
        .iter()
        .find(|error| error.item_path == "log_profiles[0].datetime_format")
        .expect("datetime_format のエラーが積まれるはず");
    assert!(error.reason.contains("整数"));
}

// 受け入れ条件: 空文字列は他のキーと同じ文言で拒否する。
#[test]
fn log_profile_datetime_format_empty_string_is_invalid() {
    let errors = expect_invalid(
        "datetime-format-empty",
        "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n    datetime_format: \"\"\n",
    );
    let error = errors
        .iter()
        .find(|error| error.item_path == "log_profiles[0].datetime_format")
        .expect("datetime_format のエラーが積まれるはず");
    assert!(error.reason.contains("空文字列"));
}

// ---------------------------------------------------------------------
// config_version の欠落・不正・将来版。
// ---------------------------------------------------------------------

#[test]
fn config_version_missing_is_invalid() {
    let errors = expect_invalid("version-missing", "memory:\n  budget_mib: 2048\n");
    assert!(errors
        .iter()
        .any(|error| error.item_path == "config_version"));
}

#[test]
fn config_version_future_value_reports_upgrade_reason() {
    // CFG-016: 将来版の値には「新しいアプリで開く必要がある」旨の理由を示す。
    let errors = expect_invalid("version-future", "config_version: 2\n");
    assert_eq!(errors.len(), 1);
    let error = &errors.as_slice()[0];
    assert_eq!(error.item_path, "config_version");
    assert!(error.reason.contains("新しいバージョン"));
    assert_eq!(error.line, Some(1));
}

#[test]
fn config_version_non_integer_is_invalid() {
    let errors = expect_invalid("version-non-integer", "config_version: \"1\"\n");
    assert_eq!(errors.len(), 1);
    assert!(errors.as_slice()[0].reason.contains("整数"));
}

#[test]
fn config_version_zero_is_invalid() {
    let errors = expect_invalid("version-zero", "config_version: 0\n");
    assert_eq!(errors.len(), 1);
    assert!(errors.as_slice()[0].reason.contains('1'));
}

// ---------------------------------------------------------------------
// 未知キーの検出（誤記の検出。CFG-016 の趣旨）。
// ---------------------------------------------------------------------

#[test]
fn unknown_top_level_key_is_invalid() {
    let errors = expect_invalid(
        "unknown-top-level",
        "config_version: 1\nmemroy:\n  budget_mib: 2048\n",
    );
    assert!(errors.iter().any(|error| error.reason.contains("memroy")));
}

#[test]
fn unknown_key_within_section_is_invalid() {
    let errors = expect_invalid(
        "unknown-nested",
        "config_version: 1\nmemory:\n  budget_mib: 2048\n  extra_key: 1\n",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "memory.extra_key"));
}

// ---------------------------------------------------------------------
// cache・認証情報の項目がスキーマに存在しない（CFG-011、CFG-013）。
// ---------------------------------------------------------------------

#[test]
fn cache_section_is_rejected_as_unknown_key() {
    // CFG-011: 索引キャッシュの設定項目はスキーマに存在しない。
    let errors = expect_invalid(
        "cache-section",
        "config_version: 1\ncache:\n  enabled: true\n",
    );
    assert!(errors.iter().any(|error| error.item_path == "cache"));
}

#[test]
fn db_credentials_section_is_rejected_as_unknown_key() {
    // CFG-013: 現時点では DB 認証情報を設定ファイルに保持しない。
    let errors = expect_invalid(
        "db-credentials",
        "config_version: 1\ndb:\n  username: sa\n  password: secret\n",
    );
    assert!(errors.iter().any(|error| error.item_path == "db"));
}

// ---------------------------------------------------------------------
// 型不正・値域外が行・列付きで検出される。
// ---------------------------------------------------------------------

#[test]
fn memory_budget_below_minimum_is_invalid_with_position() {
    let errors = expect_invalid(
        "budget-too-small",
        "config_version: 1\nmemory:\n  budget_mib: 0\n",
    );
    assert_eq!(errors.len(), 1);
    let error = &errors.as_slice()[0];
    assert_eq!(error.item_path, "memory.budget_mib");
    assert_eq!(error.line, Some(3));
    assert!(error.column.is_some());
}

#[test]
fn memory_budget_wrong_type_is_invalid() {
    let errors = expect_invalid(
        "budget-wrong-type",
        "config_version: 1\nmemory:\n  budget_mib: \"lots\"\n",
    );
    assert_eq!(errors.len(), 1);
    assert!(errors.as_slice()[0].reason.contains("整数"));
}

#[test]
fn process_priority_invalid_value_lists_allowed_values() {
    let errors = expect_invalid(
        "priority-invalid",
        "config_version: 1\nperformance:\n  process_priority: super_fast\n",
    );
    assert_eq!(errors.len(), 1);
    let error = &errors.as_slice()[0];
    assert_eq!(error.item_path, "performance.process_priority");
    assert!(error.reason.contains("below_normal"));
}

#[test]
fn negative_integer_is_invalid_for_unsigned_field() {
    let errors = expect_invalid(
        "negative-value",
        "config_version: 1\nclipboard:\n  max_copy_lines: -1\n",
    );
    assert_eq!(errors.len(), 1);
    assert!(errors.as_slice()[0].item_path == "clipboard.max_copy_lines");
}

#[test]
fn io_interval_ms_allows_zero() {
    // io_interval_ms は 0 以上（間隔なしを許す）。CFG-024。
    let config = expect_loaded(
        "io-interval-zero",
        "config_version: 1\nperformance:\n  io_interval_ms: 0\n",
    );
    assert_eq!(config.performance.io_interval_ms, 0);
}

// ---------------------------------------------------------------------
// パス種別ごとの許可・エラー（ADR-0005 の判定表の全種別）。
// ---------------------------------------------------------------------

#[test]
fn data_source_drive_absolute_path_is_allowed() {
    let config = expect_loaded(
        "path-drive-absolute",
        "config_version: 1\ndata_sources:\n  - name: a\n    path: \"C:\\\\Device\\\\Logs\"\n",
    );
    assert_eq!(
        config.data_sources[0].path,
        PathBuf::from(r"C:\Device\Logs")
    );
}

#[test]
fn data_source_local_verbatim_path_is_allowed() {
    let config = expect_loaded(
        "path-local-verbatim",
        r"config_version: 1
data_sources:
  - name: a
    path: '\\?\C:\Device\Logs'
",
    );
    assert_eq!(config.data_sources.len(), 1);
}

#[test]
fn data_source_relative_path_is_invalid() {
    let errors = expect_invalid(
        "path-relative",
        "config_version: 1\ndata_sources:\n  - name: a\n    path: \"logs\\\\a.log\"\n",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "data_sources[0].path"));
}

#[test]
fn data_source_drive_relative_path_is_invalid() {
    let errors = expect_invalid(
        "path-drive-relative",
        "config_version: 1\ndata_sources:\n  - name: a\n    path: \"C:logs\"\n",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "data_sources[0].path"));
}

#[test]
fn data_source_root_relative_path_is_invalid() {
    let errors = expect_invalid(
        "path-root-relative",
        "config_version: 1\ndata_sources:\n  - name: a\n    path: \"\\\\logs\"\n",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "data_sources[0].path"));
}

#[test]
fn data_source_unc_path_is_invalid() {
    let errors = expect_invalid(
        "path-unc",
        r"config_version: 1
data_sources:
  - name: a
    path: '\\server\share'
",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "data_sources[0].path"));
}

#[test]
fn data_source_unc_verbatim_path_is_invalid() {
    let errors = expect_invalid(
        "path-unc-verbatim",
        r"config_version: 1
data_sources:
  - name: a
    path: '\\?\UNC\server\share'
",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "data_sources[0].path"));
}

#[test]
fn log_profile_path_pattern_base_must_be_absolute() {
    // ADR-0005: path_pattern の基点も絶対ローカルパスである必要がある。
    let errors = expect_invalid(
        "profile-relative-pattern",
        "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"*.log\"\n",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "log_profiles[0].path_pattern"));
}

// ---------------------------------------------------------------------
// 複数エラーが一度に収集される。
// ---------------------------------------------------------------------

#[test]
fn multiple_errors_are_collected_in_a_single_pass() {
    let errors = expect_invalid(
        "multiple-errors",
        r#"
config_version: 2
memory:
  budget_mib: 0
clipboard:
  max_copy_lines: -5
unknown_section:
  foo: 1
"#,
    );
    // config_version, memory.budget_mib, clipboard.max_copy_lines, unknown_section の
    // 4件すべてが一度に検出される（最初の1件で止めない）。
    assert_eq!(errors.len(), 4);
}

// ---------------------------------------------------------------------
// log_profiles の形の検証。
// ---------------------------------------------------------------------

#[test]
fn log_profile_missing_name_is_invalid() {
    let errors = expect_invalid(
        "profile-missing-name",
        "config_version: 1\nlog_profiles:\n  - path_pattern: \"C:/Device/Logs/*.log\"\n",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "log_profiles[0].name"));
}

#[test]
fn log_profile_encoding_wrong_type_is_invalid() {
    let errors = expect_invalid(
        "profile-encoding-type",
        "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n    encoding: 123\n",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "log_profiles[0].encoding"));
}

#[test]
fn log_profile_ansi_codepage_non_integer_is_invalid() {
    let errors = expect_invalid(
        "profile-codepage-non-integer",
        "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n    ansi_codepage: \"shift_jis\"\n",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "log_profiles[0].ansi_codepage"));
}

#[test]
fn log_profile_missing_path_pattern_is_invalid() {
    let errors = expect_invalid(
        "profile-missing-pattern",
        "config_version: 1\nlog_profiles:\n  - name: a\n",
    );
    assert!(errors
        .iter()
        .any(|error| error.item_path == "log_profiles[0].path_pattern"));
}

// ---------------------------------------------------------------------
// 構文エラー・読み取り不能。
// ---------------------------------------------------------------------

#[test]
fn malformed_yaml_is_invalid_with_position() {
    let errors = expect_invalid(
        "malformed-yaml",
        "config_version: 1\nmemory:\n  budget_mib: [1\n",
    );
    assert_eq!(errors.len(), 1);
    let error = &errors.as_slice()[0];
    assert!(error.line.is_some());
    assert!(error.column.is_some());
    assert!(error.reason.contains("構文エラー"));
}

#[test]
fn data_sources_not_a_sequence_is_invalid() {
    let errors = expect_invalid(
        "data-sources-not-sequence",
        "config_version: 1\ndata_sources: not_a_list\n",
    );
    assert_eq!(errors.len(), 1);
    assert!(errors.as_slice()[0].reason.contains("配列"));
}

// ---------------------------------------------------------------------
// Display 表現（CFG-016: ファイル名・行・列・理由）。
// ---------------------------------------------------------------------

#[test]
fn config_error_display_contains_file_name_line_column_and_reason() {
    let path = temp_yaml_path("display-check");
    std::fs::write(&path, "config_version: 1\nmemory:\n  budget_mib: 0\n").unwrap();
    let outcome = load_config(&path);
    std::fs::remove_file(&path).unwrap();

    let LoadOutcome::Invalid(errors) = outcome else {
        panic!("Invalid を期待しました");
    };
    let rendered = errors.to_string();
    assert!(rendered.contains(&path.display().to_string()));
    assert!(rendered.contains("memory.budget_mib"));
    assert!(rendered.contains(':'));
}

// ---------------------------------------------------------------------
// 名前の重複検出（P03-2 の「重複」。glob の意味的な重複検出は P05 の所有）。
// ---------------------------------------------------------------------

#[test]
fn duplicate_data_source_names_are_invalid() {
    // 同名の data_sources 定義は起動時検証エラーになり、黙って片方が採用される
    // ことがない（CFG-016 の趣旨）。
    let errors = expect_invalid(
        "dup-ds-name",
        r#"
config_version: 1
data_sources:
  - name: device_logs
    path: "C:/Device/Logs"
  - name: device_logs
    path: "D:/Other/Logs"
"#,
    );
    let text = errors.to_string();
    assert!(
        text.contains("data_sources の名前 \"device_logs\" が重複しています"),
        "重複エラーが含まれるはず: {text}"
    );
}

#[test]
fn duplicate_log_profile_names_are_invalid() {
    // 同名の log_profiles 定義も同じ規則で起動時検証エラーになる。
    let errors = expect_invalid(
        "dup-profile-name",
        r#"
config_version: 1
log_profiles:
  - name: device_log
    path_pattern: "C:/Device/Logs/*.log"
  - name: device_log
    path_pattern: "C:/Device/Logs/*.txt"
"#,
    );
    let text = errors.to_string();
    assert!(
        text.contains("log_profiles の名前 \"device_log\" が重複しています"),
        "重複エラーが含まれるはず: {text}"
    );
}

// ---------------------------------------------------------------------
// 同優先度 glob の重複検証、完全一致の重複検証（P05-3、作業項目3）。
// ---------------------------------------------------------------------

#[test]
fn same_priority_same_glob_pattern_is_invalid_with_position() {
    // 「同一優先度内で、正規化後のパターン文字列が大文字・小文字の不区別で
    // 一致する」明確な重複は起動時検証エラー（行・列つき）。
    let errors = expect_invalid(
        "dup-glob-same-priority",
        r#"
config_version: 1
log_profiles:
  - name: a
    path_pattern: "C:/Device/Logs/*.log"
    priority: 10
  - name: b
    path_pattern: "c:\\device\\logs\\*.log"
    priority: 10
"#,
    );
    assert_eq!(errors.len(), 1);
    let error = &errors.as_slice()[0];
    assert_eq!(error.item_path, "log_profiles[1].path_pattern");
    assert!(error.line.is_some());
    assert!(error.column.is_some());
}

#[test]
fn same_glob_pattern_with_different_priority_is_valid() {
    // priority が異なる glob どうしは解決時に優先度で一意に決まるため、
    // パターン文字列が同じでも起動時検証エラーにしない（設計判断は
    // crates/config/src/load.rs の validate_no_duplicate_patterns を参照）。
    let config = expect_loaded(
        "dup-glob-different-priority",
        r#"
config_version: 1
log_profiles:
  - name: a
    path_pattern: "C:/Device/Logs/*.log"
    priority: 10
  - name: b
    path_pattern: "C:/Device/Logs/*.log"
    priority: 20
"#,
    );
    assert_eq!(config.log_profiles.len(), 2);
}

#[test]
fn different_glob_patterns_same_priority_are_valid_at_load_time() {
    // パターン文字列が異なる場合の潜在的な重なり（例: *.log と a*.log）は
    // 起動時検証では検出しない。実際に同時一致した場合の一意化は解決時
    // （crates/core-services::resolve_profile）の Ambiguous が担う。
    let config = expect_loaded(
        "different-patterns-same-priority",
        r#"
config_version: 1
log_profiles:
  - name: a
    path_pattern: "C:/Device/Logs/*.log"
    priority: 10
  - name: b
    path_pattern: "C:/Device/Logs/a*.log"
    priority: 10
"#,
    );
    assert_eq!(config.log_profiles.len(), 2);
}

#[test]
fn same_exact_match_pattern_is_invalid_regardless_of_priority() {
    // 完全一致（glob 記号なし）パターンの重複は、priority の値に関わらず
    // 常にエラーにする（絶対パス完全一致の段階は priority を参照しないため）。
    let errors = expect_invalid(
        "dup-exact-different-priority",
        r#"
config_version: 1
log_profiles:
  - name: a
    path_pattern: "C:/Device/Logs/a.log"
    priority: 1
  - name: b
    path_pattern: "c:\\device\\logs\\a.log"
    priority: 99
"#,
    );
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors.as_slice()[0].item_path,
        "log_profiles[1].path_pattern"
    );
}
