//! `read_fixed_runtime_preference`（先行読み込み）と `load_config`（全体検証）が、
//! `webview2.force_fixed_version_runtime` について同じ値集合を同じ意味で受理する
//! ことを確認する（`tasks/phase-03-configuration.md` 作業項目9）。
//!
//! 両者は `hakutaku_config::interpret_fixed_runtime_preference`（`pub(crate)`）を
//! 共有しているため、この受け入れ確認はクレートの公開 API（`read_fixed_runtime_preference`
//! と `load_config`）だけを呼び、内部実装には触れない。

use std::path::PathBuf;

use hakutaku_config::{
    load_config, read_fixed_runtime_preference, FixedRuntimePreference, LoadOutcome,
    PreflightOutcome,
};

fn temp_yaml_path(label: &str) -> PathBuf {
    let unique = format!(
        "hakutaku-config-unify-test-{label}-{}-{:?}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

/// `raw_value` を YAML の値としてそのまま埋め込み、先行読み込みと全体検証の
/// 両方に同じ意味を持つ値として渡す（例: `"true"`・`"\"auto\""`・`"\"yes\""`）。
fn write_yaml(label: &str, raw_value: &str) -> PathBuf {
    let path = temp_yaml_path(label);
    std::fs::write(
        &path,
        format!("config_version: 1\nwebview2:\n  force_fixed_version_runtime: {raw_value}\n"),
    )
    .unwrap();
    path
}

#[test]
fn valid_values_are_accepted_by_both_entry_points_with_matching_meaning() {
    let cases: &[(&str, &str, FixedRuntimePreference)] = &[
        ("true", "true", FixedRuntimePreference::ForceFixedVersion),
        ("false", "false", FixedRuntimePreference::Auto),
        ("auto-string", "\"auto\"", FixedRuntimePreference::Auto),
    ];

    for (label, raw_value, expected) in cases {
        let path = write_yaml(label, raw_value);

        let preflight = read_fixed_runtime_preference(&path);
        let full = load_config(&path);

        std::fs::remove_file(&path).unwrap();

        match preflight {
            PreflightOutcome::Determined(preference) => {
                assert_eq!(
                    preference, *expected,
                    "先行読み込みの結果が期待値と異なります（{label}）"
                );
            }
            other => panic!("{label}: Determined を期待しましたが {other:?} でした"),
        }

        match full {
            LoadOutcome::Loaded(config) => {
                assert_eq!(
                    config.webview2.force_fixed_version_runtime, *expected,
                    "load_config の結果が期待値と異なります（{label}）"
                );
            }
            other => panic!("{label}: Loaded を期待しましたが {other:?} でした"),
        }
    }
}

#[test]
fn invalid_values_are_rejected_by_both_entry_points() {
    // 真偽値でも "auto" でもない値は、先行読み込みでは Undetermined、
    // 全体検証では Invalid（webview2.force_fixed_version_runtime のエラー）になる。
    //
    // 受理範囲は「真偽値そのもの」と「文字列 `auto`（小文字ちょうど）」だけ
    // （`interpret_fixed_runtime_preference` の doc コメント）。よくある書き
    // 間違い——真偽値を引用符で囲む、`auto` を大文字にする、値を空にする——は
    // すべて誤設定として提示し、黙って既定値へ倒さない（`CFG-016`）。
    let cases: &[(&str, &str)] = &[
        ("string-yes", "\"yes\""),
        ("numeric", "1"),
        // 引用符で囲むと真偽値ではなく文字列になる。`true` に見えるのに強制
        // 指定として扱われないため、黙って無視すると誤解が残り続ける。
        ("quoted-true", "\"true\""),
        ("quoted-false", "\"false\""),
        // `auto` の受理は小文字ちょうど。大文字・先頭大文字は受理しない。
        ("uppercase-auto", "\"AUTO\""),
        ("capitalized-auto", "\"Auto\""),
        // 値を書き忘れた（`force_fixed_version_runtime:` だけ）状態は YAML の
        // null になる。キーが無い場合（= 既定の Auto）とは区別する。
        ("null", "null"),
        ("tilde-null", "~"),
        // 型そのものが違う場合。
        ("sequence", "[true]"),
        ("mapping", "{ value: true }"),
    ];

    for (label, raw_value) in cases {
        let path = write_yaml(label, raw_value);

        let preflight = read_fixed_runtime_preference(&path);
        let full = load_config(&path);

        std::fs::remove_file(&path).unwrap();

        assert!(
            matches!(preflight, PreflightOutcome::Undetermined { .. }),
            "{label}: 先行読み込みは Undetermined を期待しましたが {preflight:?} でした"
        );

        match full {
            LoadOutcome::Invalid(errors) => {
                assert!(
                    errors
                        .iter()
                        .any(|error| error.item_path == "webview2.force_fixed_version_runtime"),
                    "{label}: webview2.force_fixed_version_runtime のエラーが見つかりません: {errors}"
                );
            }
            other => panic!("{label}: Invalid を期待しましたが {other:?} でした"),
        }
    }
}

/// 受理するかどうかの判断そのものが、両入口で一致することを確認する
/// （`CFG-023` / `DIST-017`）。
///
/// 上の2つのテストは、個別の値に対する期待結果を書いている。ここでは
/// 「YAML の書き方としてどちらとも解釈し得る値」まで含めて、**結果が一致
/// すること**だけを表明する。真偽値の綴り（`True`・`yes`・`on` など）を
/// どこまで真偽値とみなすかは YAML の版と実装（`saphyr`）に依存するため、
/// この確認は特定の解釈を前提にしない。
///
/// 一致していないと何が起きるか: 起動手順1の先行読み込み（`bootstrap` が
/// WebView2 Runtime を選ぶ判断）と、手順の後半で行う設定全体の検証
/// （`CFG-016` の安全モード判定）が別々の結論を出す。利用者から見ると
/// 「設定は誤りだと言われるのに Fixed Version で起動している」、あるいは
/// その逆が起こる。
#[test]
fn both_entry_points_agree_on_whether_a_value_is_accepted() {
    let raw_values: &[&str] = &[
        "true", "false", "\"auto\"", "auto",
        // 版によって真偽値とも文字列とも解釈され得る綴り。解釈が変わっても、
        // 両入口が同じ結論を出すことだけは変わってはならない。
        "True", "TRUE", "yes", "on", "off",
        // 明確に受理できないもの（両方が拒否で一致するはず）。
        "\"yes\"", "1", "null",
    ];

    for raw_value in raw_values {
        let path = write_yaml("agreement", raw_value);
        let preflight = read_fixed_runtime_preference(&path);
        let full = load_config(&path);
        std::fs::remove_file(&path).unwrap();

        match (preflight, full) {
            (PreflightOutcome::Determined(preference), LoadOutcome::Loaded(config)) => {
                assert_eq!(
                    preference, config.webview2.force_fixed_version_runtime,
                    "{raw_value}: 両入口が受理しましたが、意味が異なります"
                );
            }
            (PreflightOutcome::Undetermined { .. }, LoadOutcome::Invalid(errors)) => {
                assert!(
                    errors
                        .iter()
                        .any(|error| error.item_path == "webview2.force_fixed_version_runtime"),
                    "{raw_value}: 両入口が拒否しましたが、全体検証の対象項目が違います: {errors}"
                );
            }
            (preflight, full) => panic!(
                "{raw_value}: 受理するかどうかが両入口で食い違いました（先行読み込み {preflight:?} / 全体検証 {full:?}）"
            ),
        }
    }
}

/// `webview2` 区分そのものの型が違う場合、**先行読み込みは既定（`Auto`）で
/// 続行し、全体検証だけがエラーとして提示する**（意図した非対称）。
///
/// 先行読み込みは起動手順1で Runtime を選ぶためだけに1項目を読む処理であり、
/// 区分の型検証までは行わない（`read_fixed_runtime_preference` の実装コメント
/// 「このキー1つだけの先行読み込みでは、`webview2` 自体の型検証までは行わない」）。
/// ここを「統一」して先行読み込みも失敗扱いにすると、設定の書き間違い1つで
/// Runtime の選択経路まで変わってしまう。誤設定の提示は `CFG-016` の安全モード
/// 判定（`load_config`）が一括で行えばよい。この非対称は意図したものなので、
/// 将来「両入口を揃える」変更で崩さないようテストで固定する。
#[test]
fn a_malformed_webview2_section_is_reported_only_by_full_validation() {
    let path = temp_yaml_path("section-type");
    std::fs::write(&path, "config_version: 1\nwebview2: \"true\"\n").unwrap();

    let preflight = read_fixed_runtime_preference(&path);
    let full = load_config(&path);

    std::fs::remove_file(&path).unwrap();

    assert!(
        matches!(
            preflight,
            PreflightOutcome::Determined(FixedRuntimePreference::Auto)
        ),
        "先行読み込みは既定（Auto）で続行するはずですが {preflight:?} でした"
    );

    match full {
        LoadOutcome::Invalid(errors) => {
            assert!(
                errors.iter().any(|error| error.item_path == "webview2"),
                "webview2 区分のエラーが見つかりません: {errors}"
            );
        }
        other => panic!("Invalid を期待しましたが {other:?} でした"),
    }
}
