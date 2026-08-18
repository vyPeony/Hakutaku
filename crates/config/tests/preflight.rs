//! `hakutaku_config::read_fixed_runtime_preference` の受け入れ確認。
//!
//! `webview2.force_fixed_version_runtime` の先行読み込み（`DIST-017` / `CFG-023`）が、
//! 契約書「契約 2: hakutaku-config」の判定規則どおりに動作することを確認する。

use hakutaku_config::{read_fixed_runtime_preference, FixedRuntimePreference, PreflightOutcome};
use std::path::PathBuf;

/// 一意な一時ファイルパスを作る（`std::env::temp_dir()` 配下。テスト専用の一時領域であり、
/// アプリ本体が実行時に書き込む対象ではない）。
fn temp_yaml_path(label: &str) -> PathBuf {
    let unique = format!(
        "hakutaku-config-test-{label}-{}-{:?}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

#[test]
fn missing_file_is_missing() {
    let path = temp_yaml_path("missing");
    assert!(!path.exists());

    let outcome = read_fixed_runtime_preference(&path);
    assert!(matches!(outcome, PreflightOutcome::Missing));
    assert_eq!(
        outcome.preference_or_default(),
        FixedRuntimePreference::Auto
    );
}

#[test]
fn empty_file_is_determined_auto() {
    let path = temp_yaml_path("empty");
    std::fs::write(&path, "").unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    assert!(matches!(
        outcome,
        PreflightOutcome::Determined(FixedRuntimePreference::Auto)
    ));

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn missing_webview2_section_is_determined_auto() {
    let path = temp_yaml_path("no-section");
    std::fs::write(&path, "some_other_key: 1\n").unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    assert!(matches!(
        outcome,
        PreflightOutcome::Determined(FixedRuntimePreference::Auto)
    ));

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn missing_key_within_section_is_determined_auto() {
    let path = temp_yaml_path("no-key");
    std::fs::write(&path, "webview2:\n  other_key: true\n").unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    assert!(matches!(
        outcome,
        PreflightOutcome::Determined(FixedRuntimePreference::Auto)
    ));

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn force_true_is_determined_force_fixed_version() {
    let path = temp_yaml_path("force-true");
    std::fs::write(&path, "webview2:\n  force_fixed_version_runtime: true\n").unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    assert!(matches!(
        outcome,
        PreflightOutcome::Determined(FixedRuntimePreference::ForceFixedVersion)
    ));
    assert_eq!(
        outcome.preference_or_default(),
        FixedRuntimePreference::ForceFixedVersion
    );

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn force_false_is_determined_auto() {
    let path = temp_yaml_path("force-false");
    std::fs::write(&path, "webview2:\n  force_fixed_version_runtime: false\n").unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    assert!(matches!(
        outcome,
        PreflightOutcome::Determined(FixedRuntimePreference::Auto)
    ));

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn force_auto_string_is_determined_auto() {
    // 作業項目9: 先行読み込みも load_config と同じく、文字列
    // "auto" を自動判定の明示指定として受理する
    // （interpret_fixed_runtime_preference の共有により一致させた受理範囲）。
    let path = temp_yaml_path("force-auto-string");
    std::fs::write(&path, "webview2:\n  force_fixed_version_runtime: auto\n").unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    assert!(matches!(
        outcome,
        PreflightOutcome::Determined(FixedRuntimePreference::Auto)
    ));

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn string_value_is_undetermined_with_position() {
    let path = temp_yaml_path("string-value");
    std::fs::write(&path, "webview2:\n  force_fixed_version_runtime: \"yes\"\n").unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    match outcome {
        PreflightOutcome::Undetermined {
            reason,
            line,
            column,
        } => {
            assert!(reason.contains('真'));
            // 2行目（0始まりではなく1始まり）にある値を指しているはず。
            assert_eq!(line, Some(2));
            assert!(column.is_some());
        }
        other => panic!("Undetermined を期待しましたが {other:?} でした"),
    }

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn numeric_value_is_undetermined() {
    let path = temp_yaml_path("numeric-value");
    std::fs::write(&path, "webview2:\n  force_fixed_version_runtime: 1\n").unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    assert!(matches!(outcome, PreflightOutcome::Undetermined { .. }));
    // 確定できない場合でも Runtime 解決は既定（Auto）で続行する。
    assert_eq!(
        outcome.preference_or_default(),
        FixedRuntimePreference::Auto
    );

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn malformed_yaml_is_undetermined_with_position() {
    let path = temp_yaml_path("malformed");
    // フローシーケンスの開き括弧を閉じない、意図的に不正な YAML。
    std::fs::write(&path, "webview2:\n  force_fixed_version_runtime: [true\n").unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    match outcome {
        PreflightOutcome::Undetermined { line, column, .. } => {
            assert!(line.is_some());
            assert!(column.is_some());
        }
        other => panic!("Undetermined を期待しましたが {other:?} でした"),
    }

    std::fs::remove_file(&path).unwrap();
}

// 受け入れ条件: `---` 区切りで複数ドキュメントがある場合、先頭で値を確定
// できても安全側（Undetermined → 既定の Auto で続行）へ倒す。利用者への提示は
// load_config の一括検証が行う（Issue #39）。
#[test]
fn multiple_documents_are_undetermined_with_position() {
    let path = temp_yaml_path("multi-document");
    std::fs::write(
        &path,
        "webview2:\n  force_fixed_version_runtime: true\n---\nwebview2:\n  force_fixed_version_runtime: false\n",
    )
    .unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    match &outcome {
        PreflightOutcome::Undetermined {
            reason,
            line,
            column,
        } => {
            assert!(reason.contains("ドキュメント"), "理由: {reason}");
            // 無視される側（2件目のドキュメント）の先頭位置を指す。区切り行
            // （3行目の `---`）ではなく、その次の内容の行を指す。
            assert_eq!(*line, Some(4));
            assert!(column.is_some());
        }
        other => panic!("Undetermined を期待しましたが {other:?} でした"),
    }
    assert_eq!(
        outcome.preference_or_default(),
        FixedRuntimePreference::Auto
    );

    std::fs::remove_file(&path).unwrap();
}

// 受け入れ条件: 存在するが読み取れない設定ファイルは、Missing（既定値起動）
// ではなく Undetermined として扱い、Runtime 解決は既定で続行する。
#[test]
fn unreadable_file_is_undetermined_with_default_preference() {
    // Windows でファイルの代わりにディレクトリを開くとアクセスが拒否される
    // ため、権限設定に依存せず決定的に読み取り失敗を起こせる。
    let path = temp_yaml_path("unreadable");
    std::fs::create_dir(&path).unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    std::fs::remove_dir(&path).unwrap();

    match &outcome {
        PreflightOutcome::Undetermined { reason, line, .. } => {
            assert!(reason.contains("読み取れませんでした"), "理由: {reason}");
            assert_eq!(*line, None);
        }
        other => panic!("Undetermined を期待しましたが {other:?} でした"),
    }
    assert_eq!(
        outcome.preference_or_default(),
        FixedRuntimePreference::Auto
    );
}

#[test]
fn realistic_config_with_many_keys_reads_target_key_only() {
    let path = temp_yaml_path("realistic");
    std::fs::write(
        &path,
        r"config_version: 1

memory:
  budget_gib: 2

clipboard:
  max_bytes: 16777216
  max_lines: 100000

webview2:
  force_fixed_version_runtime: true

log_profiles:
  - name: japanese_device_log
    path_pattern: 'C:/Device/Logs/*.log'
    encoding: auto
    ansi_codepage: 932
",
    )
    .unwrap();

    let outcome = read_fixed_runtime_preference(&path);
    assert!(matches!(
        outcome,
        PreflightOutcome::Determined(FixedRuntimePreference::ForceFixedVersion)
    ));

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn reading_does_not_modify_the_file() {
    let path = temp_yaml_path("no-mutation");
    let original_contents = "webview2:\n  force_fixed_version_runtime: true\n";
    std::fs::write(&path, original_contents).unwrap();

    let modified_before = std::fs::metadata(&path).unwrap().modified().unwrap();

    let _ = read_fixed_runtime_preference(&path);

    let modified_after = std::fs::metadata(&path).unwrap().modified().unwrap();
    let contents_after = std::fs::read_to_string(&path).unwrap();

    // 読み込み後も更新時刻と内容が変わっていないこと（自動生成・上書きをしない証拠）。
    assert_eq!(modified_before, modified_after);
    assert_eq!(contents_after, original_contents);

    std::fs::remove_file(&path).unwrap();
}
