use std::env;

/// Windows アプリケーションマニフェスト（`requestedExecutionLevel=asInvoker`
/// の明示。`PRIV-001`／`PROD-012`、P11-1）。
///
/// tauri-build 2.6.3 の既定マニフェスト（`tauri-build` クレート同梱の
/// `windows-app-manifest.xml`）は、Tauri のダイアログ API が要求する
/// Common-Controls v6 依存だけを持ち、`trustInfo`／`requestedExecutionLevel`
/// を含みません。Windows の仕様では、マニフェストに
/// `requestedExecutionLevel` が無い場合は `asInvoker` を指定したときと同じ
/// 扱いになり、かつ「マニフェストが存在すること自体」でインストーラー検出
/// ヒューリスティック（ファイル名等から推測して昇格を促す既定動作。
/// マニフェストが一切無い実行ファイルにだけ働く）が無効になるため、
/// tauri-build の既定のままでも実質的に非昇格起動になります。
///
/// ただし `PRIV-001`／`PROD-012` は「常時非昇格で起動する」ことを確定要件と
/// して定めており、暗黙の既定動作に依存し続けると、将来 Common-Controls
/// 以外の理由でこのマニフェストを差し替えた際に意図せず変わるリスクが
/// あります。そのため、Common-Controls への依存はそのまま維持しつつ、
/// `requestedExecutionLevel="asInvoker"` を明示し、根拠を監査できる形に
/// します（`uiAccess="false"` は UI オートメーション特権を要求しないことの
/// 明示。Hakutaku はこれを一切必要としません）。
///
/// `src-tauri/windows-app-manifest.xml` の XML 本体には意図的にコメントを
/// 含めていません。`tauri_winres::WindowsResource::write_resource_file`
/// は、マニフェスト文字列を1行ずつ `.rc` の文字列リテラルへ変換して
/// `RT_MANIFEST` リソースとして埋め込むため、内容は最小限にしてリソース
/// コンパイル経路の変数を減らしています（根拠はここに集約）。
const WINDOWS_APP_MANIFEST: &str = include_str!("windows-app-manifest.xml");

fn main() {
    let target_os =
        env::var("CARGO_CFG_TARGET_OS").expect("Cargo から CARGO_CFG_TARGET_OS が渡されていません");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")
        .expect("Cargo から CARGO_CFG_TARGET_ARCH が渡されていません");

    if target_os != "windows" || target_arch != "x86_64" {
        panic!(
            "Hakutaku の製品ビルドは x86_64-pc-windows-msvc のみです（検出: {target_arch}-{target_os}）"
        );
    }

    let profile = env::var("PROFILE").expect("Cargo から PROFILE が渡されていません");
    let subsystem = if profile == "release" {
        "WINDOWS"
    } else {
        "CONSOLE"
    };

    // PRIV-001／PROD-012（P11-1）: tauri-build の既定マニフェスト（Common-Controls
    // 依存のみ）を、requestedExecutionLevel="asInvoker" を明示したものへ差し替える。
    // tauri_build::build() は Attributes::default()（既定マニフェスト）を使うため、
    // ここでは try_build を直接呼び、windows_attributes.app_manifest を上書きする。
    // build() と同じ失敗時の扱い（メッセージを表示して終了）にする。
    let windows_attributes =
        tauri_build::WindowsAttributes::new().app_manifest(WINDOWS_APP_MANIFEST);
    let attributes = tauri_build::Attributes::new().windows_attributes(windows_attributes);
    if let Err(error) = tauri_build::try_build(attributes) {
        panic!("tauri_build::try_build に失敗しました: {error:#}");
    }

    println!("cargo::rustc-link-arg-bin=Hakutaku=/SUBSYSTEM:{subsystem},10.00");
}
