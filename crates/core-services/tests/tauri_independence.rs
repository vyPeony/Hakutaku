use std::{env, ffi::OsString, path::Path, process::Command};

/// Tauri への依存を許可するワークスペースメンバーです。
///
/// ここに載せないメンバーは全てコア層として検査します。新しいクレートを
/// 追加しただけで検査から漏れないよう、除外は明示的な追記でだけ行います。
const GUI_PACKAGES: [&str; 1] = ["hakutaku"];

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ワークスペースのルートを解決できません")
}

fn cargo() -> OsString {
    env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// `cargo tree` を実行し、標準出力を返します。
fn cargo_tree(args: &[&str]) -> String {
    let output = Command::new(cargo())
        .current_dir(workspace_root())
        // GitHub Actions では cargo が色付きで出力し、ANSI 制御列がパッケージ名の解析を壊すため無効化する。
        .env("CARGO_TERM_COLOR", "never")
        .arg("tree")
        .arg("--locked")
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("cargo tree {args:?} を実行できません: {error}"));

    assert!(
        output.status.success(),
        "cargo tree {args:?} に失敗しました:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout).unwrap_or_else(|error| {
        panic!("cargo tree {args:?} の出力が UTF-8 ではありません: {error}")
    })
}

/// ワークスペースメンバーのパッケージ名を返します。
///
/// `cargo tree --depth 0 --workspace` は各メンバーを `名前 vバージョン (パス)` の
/// 行で列挙します。`[build-dependencies]` のような節見出しと空行は読み飛ばします。
fn workspace_members() -> Vec<String> {
    cargo_tree(&["--depth", "0", "--workspace"])
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with(char::is_whitespace))
        .filter(|line| !line.starts_with('['))
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

#[test]
fn core_crates_do_not_depend_on_tauri() {
    let members = workspace_members();
    assert!(
        !members.is_empty(),
        "ワークスペースメンバーを取得できませんでした"
    );

    for gui_package in GUI_PACKAGES {
        assert!(
            members.iter().any(|member| member == gui_package),
            "GUI 層として除外している {gui_package} がワークスペースに見つかりません。\
             クレート名の変更に合わせて GUI_PACKAGES を更新してください（検出: {members:?}）"
        );
    }

    let core_packages: Vec<&String> = members
        .iter()
        .filter(|member| !GUI_PACKAGES.contains(&member.as_str()))
        .collect();
    assert!(
        !core_packages.is_empty(),
        "検査対象のコアクレートがありません（検出: {members:?}）"
    );

    for package in core_packages {
        let tree = cargo_tree(&["--edges", "normal,build,dev", "--package", package]);
        assert!(
            !tree.to_ascii_lowercase().contains("tauri"),
            "{package} の依存グラフに Tauri が含まれています:\n{tree}"
        );
    }
}
