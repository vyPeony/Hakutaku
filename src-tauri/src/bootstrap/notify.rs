//! Tauri に依存しないネイティブダイアログ通知（P01-2）。
//!
//! Runtime を解決できない場合の通知は Tauri を使えない（`TECH-005`）。このモジュールは
//! Win32 の `MessageBoxW` だけを使い、Tauri 初期化前でも呼び出せる通知経路を提供する。
//!
//! 満たす要件:
//!
//! - `TECH-005`: Tauri 初期化前に、Tauri に依存しない通知経路で利用者へ伝える。
//! - `DIST-009`: どちらの Runtime も使用できない場合、必要な Runtime・配置先（絶対パス）・
//!   再起動手順を通知して終了する。
//! - `DIST-010`: `WebView2Runtime` フォルダの ACL を現在の権限で設定できない場合、
//!   理由と必要な対応を通知する。
//! - `DIST-014`: `WebView2`（ユーザーデータフォルダ）を作成・書き込みできない場合、
//!   理由・対象パス・必要な権限を通知して起動を中止する。別の場所へは自動フォールバックしない。
//! - `DIAG-006`: 診断ログを使えない場合も、診断ログなしで動作を継続する旨を通知する。
//!
//! 文面の組み立て関数（`runtime_unavailable` など）は Win32 API を一切呼ばず、
//! [`Notice`] を返すだけにしている。これにより Win32 の実行環境がなくても
//! 文面の内容を単体テストできる。実際に画面へ出すのは [`show`] の役割であり、
//! こちらだけが `MessageBoxW` を呼ぶ。
//!
//! パスはすべて絶対パスで表示する（10.3 の注記）。`WebView2`（ユーザーデータの
//! 保存先）と `WebView2Runtime`（Fixed Version Runtime の配置先）は名前が似ているため、
//! 取り違えを防ぐ目的で、該当する文面では必ず両者の役割の違いを明記する。

use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND,
    MB_TOPMOST, MESSAGEBOX_STYLE,
};

use crate::bootstrap::layout::{DirectoryAction, DirectoryFailure};

/// 通知の種別。アイコンの選択に使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Error,
    Warning,
    Information,
}

/// ネイティブダイアログへ表示する内容。
///
/// 文面の組み立て（`runtime_unavailable` など）と実際の表示（[`show`]）を分離し、
/// 前者を Win32 API に依存しない形で単体テストできるようにしている。
#[derive(Clone, Debug)]
pub struct Notice {
    pub kind: NoticeKind,
    pub title: String,
    pub body: String,
}

/// `Notice` を Win32 の `MessageBoxW` で表示する。Tauri に依存しない（`TECH-005`）。
///
/// 表示自体に失敗した場合（`MessageBoxW` が 0 を返した場合）も panic せず、
/// 標準エラー出力へ本文を出す。呼び出し元はいずれの場合も処理を継続できる。
pub fn show(notice: &Notice) {
    let title_wide = to_wide_null(&notice.title);
    let body_wide = to_wide_null(&notice.body);
    let style = icon_style(notice.kind) | MB_OK | MB_SETFOREGROUND | MB_TOPMOST;

    // SAFETY: title_wide・body_wide はこの関数のスコープ内で生存する NUL 終端の
    // UTF-16 バッファであり、MessageBoxW の呼び出しが完了するまで解放されない。
    // 親ウィンドウは常に無効値（HWND(null)）を渡す。戻り値が 0（失敗）の場合も
    // このクレートの他の呼び出しには影響しない値であり、panic しない。
    let result = unsafe {
        MessageBoxW(
            Some(HWND(std::ptr::null_mut())),
            PCWSTR(body_wide.as_ptr()),
            PCWSTR(title_wide.as_ptr()),
            style,
        )
    };

    if result.0 == 0 {
        eprintln!(
            "Hakutaku: ネイティブダイアログを表示できませんでした。件名: {}\n{}",
            notice.title, notice.body
        );
    }
}

fn icon_style(kind: NoticeKind) -> MESSAGEBOX_STYLE {
    match kind {
        NoticeKind::Error => MB_ICONERROR,
        NoticeKind::Warning => MB_ICONWARNING,
        NoticeKind::Information => MB_ICONINFORMATION,
    }
}

/// 文字列を NUL 終端の UTF-16（wide）バッファへ変換する。
fn to_wide_null(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `DIST-009`。どちらの Runtime も使用できない場合の通知文を組み立てる。
///
/// `runtime_dir` は `WebView2Runtime`（Fixed Version Runtime の配置先）の絶対パス。
/// `evergreen_detail` / `fixed_detail` は、それぞれの検出結果の説明（診断向けの詳細）。
pub fn runtime_unavailable(
    runtime_dir: &Path,
    evergreen_detail: &str,
    fixed_detail: &str,
) -> Notice {
    let runtime_dir_display = runtime_dir.display();
    let body = format!(
        "Hakutaku を起動できません。WebView2 Runtime が見つかりませんでした。\n\
\n\
【検出結果】\n\
・導入済み Evergreen Runtime: {evergreen_detail}\n\
・実行ファイル直下の Fixed Version Runtime: {fixed_detail}\n\
\n\
【対処方法 1: Fixed Version Runtime を配置する】\n\
Fixed Version WebView2 Runtime 一式を、次のフォルダへ「平坦化」して展開してください。\n\
\n\
  {runtime_dir_display}\n\
\n\
「平坦化」とは、ZIP 内のフォルダ構造をそのまま展開するのではなく、\n\
msedgewebview2.exe がこのフォルダの直下に来るように展開することです。\n\
展開後、Hakutaku を再起動してください。\n\
\n\
【対処方法 2: Evergreen WebView2 Runtime を導入する】\n\
上記の代わりに、Microsoft Edge WebView2 Runtime（Evergreen）を通常のインストーラーで\n\
導入する方法もあります。導入後、Hakutaku を再起動してください。\n\
\n\
Hakutaku は WebView2 Runtime をネットワークから自動取得しません。上記のいずれかの\n\
方法で、利用者自身が配置または導入する必要があります。\n\
\n\
【重要】上記フォルダ（WebView2Runtime）は Fixed Version Runtime の配置先です。\n\
WebView2 の閲覧データ等を保存する「WebView2」フォルダ（ユーザーデータフォルダ）とは\n\
別のフォルダです。取り違えないようにしてください。"
    );

    Notice {
        kind: NoticeKind::Error,
        title: "Hakutaku: WebView2 Runtime が見つかりません".to_string(),
        body,
    }
}

/// `DIST-010`。`WebView2Runtime` フォルダの ACL を現在の権限で設定できない場合の
/// 通知文を組み立てる。`reason` は失敗理由、`required_privilege` は利用者が
/// 取るべき具体的な対処。
pub fn acl_not_applicable(runtime_dir: &Path, reason: &str, required_privilege: &str) -> Notice {
    let runtime_dir_display = runtime_dir.display();
    let body = format!(
        "Fixed Version WebView2 Runtime 用フォルダのアクセス許可を設定できませんでした。\n\
\n\
対象フォルダ（絶対パス）:\n\
  {runtime_dir_display}\n\
\n\
理由:\n\
  {reason}\n\
\n\
必要な対応:\n\
  {required_privilege}\n\
\n\
この操作は WebView2Runtime フォルダのアクセス許可（ACL）というメタ情報の変更だけであり、\n\
フォルダ内の Runtime ファイルの内容は変更しません。"
    );

    Notice {
        kind: NoticeKind::Error,
        title: "Hakutaku: WebView2 Runtime のアクセス許可を設定できません".to_string(),
        body,
    }
}

/// `DIST-014`。WebView2 のユーザーデータフォルダ（`WebView2`）を用意できない場合の
/// 通知文を組み立てる。`failure` は `bootstrap::layout::ensure_webview2_data` が
/// 返した失敗理由。
pub fn webview2_data_unavailable(failure: &DirectoryFailure) -> Notice {
    let target_display = failure.target.display();
    let action_label = match failure.action {
        DirectoryAction::Create => "作成",
        DirectoryAction::Write => "書き込み",
    };

    let mut body = format!(
        "Hakutaku を起動できません。WebView2 のユーザーデータフォルダを{action_label}できません。\n\
\n\
対象フォルダ（絶対パス）:\n\
  {target_display}\n\
\n\
理由:\n\
  {reason}\n",
        reason = failure.reason,
    );

    if let Some(code) = failure.os_error_code {
        body.push_str(&format!("OS エラーコード: {code}\n"));
    }

    body.push_str(&format!(
        "\n必要な対応:\n  {required}\n\n\
このフォルダは実行ファイル直下の「WebView2」に固定されています（Fixed Version Runtime の\n\
配置先である「WebView2Runtime」とは別のフォルダです）。別の場所へは自動的に作成しません。\n\
上記の対応を行ったうえで、Hakutaku を再起動してください。",
        required = failure.required_privilege,
    ));

    Notice {
        kind: NoticeKind::Error,
        title: "Hakutaku: WebView2 データフォルダを準備できません".to_string(),
        body,
    }
}

/// `DIAG-006`。診断ログを使えないが、動作は継続する場合の通知文を組み立てる。
pub fn diagnostics_unavailable(reason: &hakutaku_diagnostics::DiagnosticsUnavailable) -> Notice {
    let target_display = reason.target.display();

    let mut body = format!(
        "診断ログ（logs フォルダ）を利用できません。\n\
\n\
対象フォルダ（絶対パス）:\n\
  {target_display}\n\
\n\
理由:\n\
  {detail}\n",
        detail = reason.reason,
    );

    if let Some(code) = reason.os_error_code {
        body.push_str(&format!("OS エラーコード: {code}\n"));
    }

    body.push_str(
        "\nHakutaku は診断ログなしで動作を継続します。別の保存先へは自動的に切り替えません。\n\
診断ログが必要な場合は、上記フォルダへの書き込み権限を確認したうえで Hakutaku を\n\
再起動してください。",
    );

    Notice {
        kind: NoticeKind::Warning,
        title: "Hakutaku: 診断ログを開始できません".to_string(),
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn runtime_unavailable_includes_absolute_path_restart_and_folder_distinction() {
        let runtime_dir = PathBuf::from(r"C:\apps\Hakutaku\WebView2Runtime");
        let notice = runtime_unavailable(&runtime_dir, "未導入", "見つかりません");

        assert_eq!(notice.kind, NoticeKind::Error);
        assert!(notice.body.contains(r"C:\apps\Hakutaku\WebView2Runtime"));
        assert!(notice.body.contains("再起動"));
        assert!(notice.body.contains("WebView2Runtime"));
        // 「WebView2」（ユーザーデータフォルダ）との違いが説明されていること。
        assert!(notice.body.contains("ユーザーデータフォルダ"));
        assert!(notice.body.contains("未導入"));
        assert!(notice.body.contains("見つかりません"));
        assert!(notice.body.contains("ネットワーク"));
    }

    #[test]
    fn acl_not_applicable_includes_absolute_path_reason_and_required_privilege() {
        let runtime_dir = PathBuf::from(r"D:\Hakutaku\WebView2Runtime");
        let notice = acl_not_applicable(
            &runtime_dir,
            "管理者権限が必要です",
            "管理者として Hakutaku を再起動してください",
        );

        assert_eq!(notice.kind, NoticeKind::Error);
        assert!(notice.body.contains(r"D:\Hakutaku\WebView2Runtime"));
        assert!(notice.body.contains("管理者権限が必要です"));
        assert!(notice
            .body
            .contains("管理者として Hakutaku を再起動してください"));
        assert!(notice.body.contains("内容は変更しません"));
    }

    #[test]
    fn webview2_data_unavailable_includes_target_privilege_and_no_fallback_notice() {
        let failure = DirectoryFailure {
            target: PathBuf::from(r"C:\apps\Hakutaku\WebView2"),
            action: DirectoryAction::Write,
            reason: "書き込みできません".to_string(),
            os_error_code: Some(5),
            required_privilege: "書き込み権限を確認してください".to_string(),
        };

        let notice = webview2_data_unavailable(&failure);

        assert_eq!(notice.kind, NoticeKind::Error);
        assert!(notice.body.contains(r"C:\apps\Hakutaku\WebView2"));
        assert!(notice.body.contains("書き込みできません"));
        assert!(notice.body.contains("書き込み権限を確認してください"));
        assert!(notice.body.contains("別の場所へは自動的に作成しません"));
    }

    #[test]
    fn diagnostics_unavailable_includes_continue_and_no_fallback_notice() {
        let reason = hakutaku_diagnostics::DiagnosticsUnavailable {
            target: PathBuf::from(r"C:\apps\Hakutaku\logs\hakutaku.log"),
            reason: "logs フォルダを作成できません".to_string(),
            os_error_code: Some(5),
        };

        let notice = diagnostics_unavailable(&reason);

        assert_eq!(notice.kind, NoticeKind::Warning);
        assert!(notice.body.contains(r"C:\apps\Hakutaku\logs\hakutaku.log"));
        assert!(notice.body.contains("動作を継続します"));
        assert!(notice.body.contains("別の保存先へは自動的に切り替えません"));
    }
}
