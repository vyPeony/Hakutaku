//! WebView のナビゲーション制限（`SEC-011`）。
//!
//! `SEC-011` は「ローカルの同梱リソース以外への**ナビゲーション**および
//! リモートオリジンへの**通信**を禁止する」ことを求めています。このうち通信は
//! CSP（`Tauri.toml` の `[app.security] csp`）が担いますが、**CSP には最上位
//! ドキュメントのナビゲーションを止める手段がありません**（`navigate-to`
//! ディレクティブは仕様から削除されました）。`frame-ancestors` や `frame-src`
//! が制限するのは埋め込み側だけです。
//!
//! Tauri の既定のナビゲーションハンドラは、アプリが `on_navigation` を登録して
//! いない場合すべてのナビゲーションを許可します
//! （`tauri-2.11.5/src/manager/webview.rs` の `pending.navigation_handler`。
//! プラグインが拒否しない限り `true` を返します）。実機で確認したところ、
//! 登録しない状態では `window.location.href` によるリモートオリジンへの遷移が
//! 成立し、WebView2 の子プロセスが外部ホストへ TCP 接続を確立しました。
//!
//! そのため、許可するオリジンを明示的に列挙し、それ以外を拒否します。
//!
//! # なぜ重要か
//!
//! 現在のフロントエンドは静的で信頼できる内容だけを表示しますが、以降の
//! フェーズではログ本文や DICOM のタグ値など、**信頼できない入力に由来する
//! 内容**を WebView へ表示します。ナビゲーションを開いたままにすると、
//! 表示内容が参照データを URL に載せて外部へ送り出す経路になり得ます
//! （`SEC-001`「参照内容をネットワークへ送信しない」）。

/// ナビゲーションを許可するホスト。
///
/// Tauri は Windows で、同梱アセットを `http://tauri.localhost`、IPC を
/// `http://ipc.localhost` から提供します（`tauri-2.11.5/src/manager/mod.rs`、
/// `tauri-2.11.5/scripts/core.js`）。`use_https_scheme` を有効にすると `https`
/// になるため、スキームは `http` と `https` の両方を受け付けます。
const ALLOWED_HOSTS: [&str; 2] = ["tauri.localhost", "ipc.localhost"];

/// このナビゲーションを許可してよいかを返します（`SEC-011`）。
///
/// 許可するのは、同梱リソースと IPC のオリジンだけです。判断できない URL は
/// すべて拒否します（既定拒否）。
pub fn is_allowed(url: &tauri::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }

    match url.host_str() {
        Some(host) => ALLOWED_HOSTS
            .iter()
            .any(|allowed| host.eq_ignore_ascii_case(allowed)),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_allowed;

    fn url(text: &str) -> tauri::Url {
        text.parse().expect("テスト用の URL を解釈できません")
    }

    #[test]
    fn allows_the_bundled_asset_and_ipc_origins() {
        assert!(is_allowed(&url("http://tauri.localhost/")));
        assert!(is_allowed(&url("http://tauri.localhost/index.html")));
        assert!(is_allowed(&url("http://ipc.localhost/get_config_status")));
        // use_https_scheme を有効にした場合に備えて https も許可する。
        assert!(is_allowed(&url("https://tauri.localhost/")));
        assert!(is_allowed(&url("https://ipc.localhost/")));
    }

    #[test]
    fn rejects_remote_origins() {
        assert!(!is_allowed(&url("https://example.com/")));
        assert!(!is_allowed(&url("http://example.com/")));
        assert!(!is_allowed(&url("https://tauri.localhost.example.com/")));
        assert!(!is_allowed(&url("https://evil.tauri.localhost/")));
    }

    #[test]
    fn rejects_non_http_schemes() {
        // ローカルファイルや任意のスキームで境界を越えさせない（既定拒否）。
        assert!(!is_allowed(&url("file:///C:/Windows/win.ini")));
        assert!(!is_allowed(&url("data:text/html,<h1>x</h1>")));
        assert!(!is_allowed(&url("javascript:void(0)")));
        assert!(!is_allowed(&url("about:blank")));
        assert!(!is_allowed(&url("asset://localhost/C:/Windows/win.ini")));
    }

    #[test]
    fn host_comparison_is_case_insensitive_but_exact() {
        assert!(is_allowed(&url("http://TAURI.LOCALHOST/")));
        assert!(!is_allowed(&url("http://tauri.localhost.evil.test/")));
    }
}
