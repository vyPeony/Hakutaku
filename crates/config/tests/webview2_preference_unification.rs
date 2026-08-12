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
    let cases: &[(&str, &str)] = &[("string-yes", "\"yes\""), ("numeric", "1")];

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
