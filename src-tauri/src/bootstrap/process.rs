//! 起動プロセスの昇格判定（`DIAG-007`、P01-2）と、
//! プロセス優先度の適用・昇格プロセスの起動（`PERF-014`／`PRIV-002`〜`004`、
//! P11-1・P11-2）。
//!
//! `DIAG-007` は「昇格プロセスの出力を非昇格プロセスが書き込めない場合も `DIAG-006`
//! に従い、昇格プロセスの出力であることが判別できる」ことを求めている。この判別のため、
//! 現在のプロセスが昇格しているかどうかを判定する。判定結果は
//! `hakutaku_diagnostics::ProcessElevation` として診断ログ側へ渡す。
//!
//! 判定できない場合は `ProcessElevation::Unknown` を返し、panic しない。
//! ブートストラップ経路は起動できない理由を利用者へ通知することが目的のため、
//! `unwrap()` / `expect()` / `panic!` を使わない。
//!
//! # プロセス優先度の適用（`PERF-014`、`CFG-024`、P11-3）
//!
//! [`apply_process_priority`] は、設定（`hakutaku.yaml` の `performance.process_priority`）
//! に従い、起動時に一度だけ `SetPriorityClass` で自プロセスの優先度を設定します。
//! 失敗しても panic せず、呼び出し側（`bootstrap::run`）が診断ログへ記録した
//! うえで起動を継続します（性能の抑制が効かないだけであり、致命的ではないため）。
//!
//! # 昇格プロセスの起動（`PROD-012`、`PRIV-002`〜`004`、P11-2）
//!
//! [`launch_elevated_process`] は、`ShellExecuteW` の `"runas"` verb で、
//! **自分自身の実行ファイルを引数なしで新しいプロセスとして起動**します。
//!
//! - この呼び出しは**このプロセス自体を昇格させません**（`PROD-012`）。新しい
//!   別プロセスを起動するだけです
//! - 呼び出し元（元の非昇格プロセス）はこの呼び出しの後もそのまま動作を継続
//!   します（`PRIV-003`）。開いているタブ・表示位置・解析済みデータは一切
//!   自動転送しません（引数なしで起動するため、新しいプロセスは通常起動と
//!   同じ初期状態から始まります）
//! - ユーザーが UAC 同意ダイアログをキャンセルした場合は `ERROR_CANCELLED`
//!   （1223）で失敗し、[`LaunchElevatedError::Cancelled`] を返します。
//!   ポリシー禁止やその他の失敗も含め、いずれの失敗でも元プロセスには一切
//!   影響しません（`PRIV-004`）
//! - `#[tauri::command] launch_elevated` は、フロントエンドがアクセス拒否
//!   エラー表示の「管理者として新しいウィンドウで開く」ボタンを押した場合
//!   だけ呼ばれます（`src/shell.js` 参照）。他の経路からは呼ばれない構造に
//!   なっており、自動昇格や昇格を促す既定動作は作りません

use std::sync::Arc;

use hakutaku_diagnostics::{diag_info, diag_warn, Diagnostics, ProcessElevation};
use serde::Serialize;
use tauri::State;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcessToken, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
    IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_CREATION_FLAGS,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// 現在のプロセスが昇格しているかを返す（`DIAG-007`）。
///
/// `OpenProcessToken` または `GetTokenInformation` が失敗した場合は
/// `ProcessElevation::Unknown` を返す。取得したトークンハンドルは、成功・失敗の
/// いずれの経路でも必ず `CloseHandle` で閉じる。
pub fn current_elevation() -> ProcessElevation {
    // SAFETY: GetCurrentProcess は現在のプロセスを指す擬似ハンドルを返すだけであり、
    // 閉じる必要がない（CloseHandle の対象にしない）。
    let process = unsafe { GetCurrentProcess() };

    let mut token = HANDLE(std::ptr::null_mut());
    // SAFETY: process は上で取得した有効な擬似ハンドルであり、token は
    // OpenProcessToken が書き込む出力専用の変数である。
    let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };

    if opened.is_err() {
        return ProcessElevation::Unknown;
    }

    let elevation = query_token_elevation(token);

    // SAFETY: token は直前の OpenProcessToken が成功して取得した所有ハンドルであり、
    // ここで確実に閉じる。以降このハンドルは使用しない。
    unsafe {
        let _ = CloseHandle(token);
    }

    elevation
}

/// 開いたトークンハンドルから昇格状態を取得する。ハンドルの解放は呼び出し側が行う。
fn query_token_elevation(token: HANDLE) -> ProcessElevation {
    let mut elevation = TOKEN_ELEVATION::default();
    let size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
    let mut returned_len: u32 = 0;

    // SAFETY: elevation は size で示すちょうどのサイズのローカル変数であり、
    // GetTokenInformation はそのサイズ以内にしか書き込まない。token は
    // 呼び出し元が所有する有効なトークンハンドルである。
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void),
            size,
            &mut returned_len,
        )
    };

    match result {
        Ok(()) if elevation.TokenIsElevated != 0 => ProcessElevation::Elevated,
        Ok(()) => ProcessElevation::Normal,
        Err(_) => ProcessElevation::Unknown,
    }
}

/// `hakutaku_config::ProcessPriority` を Win32 の優先度クラス定数へ変換します
/// （Win32 API を呼ばない純粋な変換）。
fn priority_class(priority: hakutaku_config::ProcessPriority) -> PROCESS_CREATION_FLAGS {
    match priority {
        hakutaku_config::ProcessPriority::Normal => NORMAL_PRIORITY_CLASS,
        hakutaku_config::ProcessPriority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        hakutaku_config::ProcessPriority::Idle => IDLE_PRIORITY_CLASS,
    }
}

/// 設定（`CFG-024`）に従い、自プロセスの優先度を設定します（`PERF-014`、P11-3）。
///
/// 失敗しても panic せず理由を返します。呼び出し側（`bootstrap::run`）が
/// 診断ログへ記録し、失敗しても起動は継続します（優先度の抑制が効かない
/// だけであり、致命的ではないため）。
pub fn apply_process_priority(priority: hakutaku_config::ProcessPriority) -> Result<(), String> {
    let class = priority_class(priority);

    // SAFETY: GetCurrentProcess は現在のプロセスを指す擬似ハンドルを返すだけで
    // あり、CloseHandle の対象にしない（current_elevation と同じ理由）。
    let process = unsafe { GetCurrentProcess() };

    // SAFETY: process は上で取得した有効な擬似ハンドルであり、class は
    // Win32 が定義する優先度クラス定数のいずれかである。
    unsafe { SetPriorityClass(process, class) }
        .map_err(|error| format!("プロセス優先度を設定できません（{}）。", error.message()))
}

/// [`launch_elevated_process`] が失敗した理由です（`PRIV-004`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchElevatedError {
    /// ユーザーが UAC 同意ダイアログをキャンセルした（`ERROR_CANCELLED`）。
    Cancelled,
    /// キャンセル以外の理由（ポリシー禁止、実行ファイルの位置を取得できない等）。
    Failed { reason: String },
}

impl std::fmt::Display for LaunchElevatedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchElevatedError::Cancelled => write!(f, "昇格がキャンセルされました。"),
            LaunchElevatedError::Failed { reason } => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for LaunchElevatedError {}

/// 文字列を NUL 終端の UTF-16（wide）バッファへ変換する
/// （`bootstrap::notify`・`bootstrap::acl` と同じ小さな複製。依存を増やさない
/// ための方針で、モジュールをまたいだ共有はしない）。
fn to_wide_null(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `ShellExecuteW` の戻り値（`HINSTANCE` を `isize` にした値）を分類する、
/// Win32 API を呼ばない純粋な関数です。
///
/// Win32 の仕様: 通常の失敗は 32 以下の値（`SE_ERR_*` 定数）で示され、32 を
/// 超える値は成功です。**ただし UAC の同意ダイアログをキャンセルした場合は
/// 例外的に `ERROR_CANCELLED`（1223、32 を超える値）が返る**ため、まず
/// [`ERROR_CANCELLED`] との一致を判定してから「32 を超える値は成功」の
/// 判定を行います（この順序を逆にすると、キャンセルが誤って成功として
/// 扱われてしまいます）。
fn classify_shell_execute_result(code: isize) -> Result<(), LaunchElevatedError> {
    if code == i64::from(ERROR_CANCELLED.0) as isize {
        return Err(LaunchElevatedError::Cancelled);
    }
    if code > 32 {
        return Ok(());
    }
    Err(LaunchElevatedError::Failed {
        reason: format!("ShellExecuteW が失敗しました（戻り値: {code}）。"),
    })
}

/// 自分自身の実行ファイルを、`"runas"` verb で新しい昇格済みプロセスとして
/// 起動します（`PROD-012`、`PRIV-002`、P11-2）。引数は渡しません。
///
/// モジュール doc コメント「昇格プロセスの起動」を参照してください。
pub fn launch_elevated_process() -> Result<(), LaunchElevatedError> {
    let exe_path = std::env::current_exe().map_err(|error| LaunchElevatedError::Failed {
        reason: format!("実行ファイルの位置を取得できません: {error}"),
    })?;

    let exe_wide = to_wide_null(&exe_path.to_string_lossy());
    let verb_wide = to_wide_null("runas");

    // SAFETY: exe_wide・verb_wide はこの呼び出しが完了するまで生存する NUL
    // 終端の UTF-16 バッファである。lpparameters・lpdirectory は既定（引数
    // なし・現在の作業ディレクトリ）を使うため PCWSTR::null() を渡す。親
    // ウィンドウは常に無効値（hwnd なし）を渡す。戻り値は分類してから使うだけで、
    // このハンドルを閉じる必要はない（ShellExecuteW が返す HINSTANCE はプロセス
    // ハンドルではない）。
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb_wide.as_ptr()),
            PCWSTR(exe_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    classify_shell_execute_result(result.0 as isize)
}

/// `launch_elevated` コマンドの応答です（`PRIV-002`〜`004`）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchElevatedResponse {
    /// 新しい昇格済みプロセスの起動要求を発行できた。実際に新しいプロセスが
    /// 起動したかどうかは別プロセスの話であり、ここでは確認できない
    /// （Windows の仕様。`ShellExecuteW` は要求の発行にだけ責任を持つ）。
    Launched,
    /// ユーザーが UAC 同意ダイアログをキャンセルした。元プロセスは一切影響を
    /// 受けない（`PRIV-004`）。
    Cancelled,
    /// キャンセル以外の理由で失敗した。元プロセスは一切影響を受けない
    /// （`PRIV-004`）。
    Failed { reason: String },
}

/// アクセス拒否エラー表示の「管理者として新しいウィンドウで開く」ボタンから
/// だけ呼ばれる Tauri コマンドです（`src/shell.js` 参照。個別 permission
/// `allow-launch-elevated` を要求する。`src-tauri/capabilities/default.toml`・
/// `src-tauri/permissions/launch-elevated.toml`）。
///
/// UI 操作なしに自動で呼ばれることはありません。自動昇格や昇格を促す既定動作
/// は作らない、というフェーズの方針（`tasks/phase-11-device-operation.md`
/// 「影響」節）をこの構造で守っています。
#[tauri::command]
pub fn launch_elevated(diagnostics: State<'_, Arc<Diagnostics>>) -> LaunchElevatedResponse {
    let diagnostics_ref: &Diagnostics = diagnostics.inner();
    match launch_elevated_process() {
        Ok(()) => {
            diag_info!(
                diagnostics_ref,
                module = "elevation",
                operation = "elevation.launch",
                "管理者として新しいウィンドウの起動要求を発行しました（PRIV-002）。"
            );
            LaunchElevatedResponse::Launched
        }
        Err(LaunchElevatedError::Cancelled) => {
            diag_warn!(
                diagnostics_ref,
                module = "elevation",
                operation = "elevation.launch",
                "昇格がキャンセルされました。元のプロセスは動作を継続します（PRIV-004）。"
            );
            LaunchElevatedResponse::Cancelled
        }
        Err(LaunchElevatedError::Failed { reason }) => {
            diag_warn!(
                diagnostics_ref,
                module = "elevation",
                operation = "elevation.launch",
                "昇格に失敗しました。元のプロセスは動作を継続します（PRIV-004）: {reason}"
            );
            LaunchElevatedResponse::Failed { reason }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // GetPriorityClass はテスト（優先度設定の観測）だけで使うため、非テスト
    // ビルドで未使用importの警告を出さないようここへ限定して import する。
    use windows::Win32::System::Threading::GetPriorityClass;

    /// Win32 API を実際に呼び出す統合的なスモークテスト。
    ///
    /// このモジュールの唯一のロジックは Win32 呼び出しそのものであり、
    /// モック化できる純粋なロジックが分離できないため、実際に呼び出して
    /// panic しないこと・既知の 3 値のいずれかを返すことだけを確認する。
    /// Windows 上でのみ実行される想定（`cargo test --workspace` は統合後に行う）。
    #[test]
    fn current_elevation_does_not_panic_and_returns_a_known_variant() {
        let elevation = current_elevation();
        assert!(matches!(
            elevation,
            ProcessElevation::Normal | ProcessElevation::Elevated | ProcessElevation::Unknown
        ));
    }

    // --- priority_class（純粋関数。PERF-014、CFG-024） ---

    #[test]
    fn priority_class_maps_each_config_variant_to_the_expected_win32_constant() {
        assert_eq!(
            priority_class(hakutaku_config::ProcessPriority::Normal),
            NORMAL_PRIORITY_CLASS
        );
        assert_eq!(
            priority_class(hakutaku_config::ProcessPriority::BelowNormal),
            BELOW_NORMAL_PRIORITY_CLASS
        );
        assert_eq!(
            priority_class(hakutaku_config::ProcessPriority::Idle),
            IDLE_PRIORITY_CLASS
        );
        // 受け入れ条件（PERF-014）: 「通常以下」に設定できること。上の3つの
        // assert_eq! で、BelowNormal・Idle がそれぞれ Windows の定義する
        // BELOW_NORMAL_PRIORITY_CLASS・IDLE_PRIORITY_CLASS（いずれも
        // NORMAL_PRIORITY_CLASS より低い優先度として Windows が扱う既知の
        // 優先度クラス）へ正しく写像することを確認済み。
        //
        // 注意: PROCESS_CREATION_FLAGS の生の数値は歴史的な経緯によるもので
        // あり、大小関係が優先度の高低と対応しない（例:
        // BELOW_NORMAL_PRIORITY_CLASS=0x4000 > NORMAL_PRIORITY_CLASS=0x20）。
        // そのため数値比較による判定はしない。実際の優先度の高低は
        // GetPriorityClass による観測
        // （apply_process_priority_sets_and_is_observable_via_get_priority_class）
        // で別途確認する。
    }

    // --- apply_process_priority（実際に Win32 API を呼ぶ統合テスト） ---
    //
    // プロセス優先度クラスはプロセス単位（スレッド単位ではない）であるため、
    // このテストは cargo test の同一プロセス内で並行実行される他のテストの
    // スケジューリングにも影響し得る。実害（正誤判定への影響）は無いが、
    // 影響を最小化するため、確認後は直ちに NORMAL へ戻す。
    #[test]
    fn apply_process_priority_sets_and_is_observable_via_get_priority_class() {
        let applied = apply_process_priority(hakutaku_config::ProcessPriority::BelowNormal);
        assert!(applied.is_ok(), "{applied:?}");

        // SAFETY: GetCurrentProcess は現在のプロセスを指す擬似ハンドルを返す
        // だけであり、閉じる必要がない。
        let process = unsafe { GetCurrentProcess() };
        // SAFETY: process は上で取得した有効な擬似ハンドルである。
        let observed = unsafe { GetPriorityClass(process) };
        assert_eq!(
            observed, BELOW_NORMAL_PRIORITY_CLASS.0,
            "設定した優先度クラスが GetPriorityClass で観測できるはず"
        );

        // 後始末: 他のテストへの影響を最小化する。
        let restored = apply_process_priority(hakutaku_config::ProcessPriority::Normal);
        assert!(restored.is_ok(), "{restored:?}");
    }

    // --- classify_shell_execute_result（純粋関数。PRIV-002〜004） ---

    #[test]
    fn classify_shell_execute_result_treats_values_above_32_as_success() {
        assert!(classify_shell_execute_result(33).is_ok());
        assert!(classify_shell_execute_result(1000).is_ok());
    }

    #[test]
    fn classify_shell_execute_result_treats_error_cancelled_as_cancelled() {
        let result = classify_shell_execute_result(i64::from(ERROR_CANCELLED.0) as isize);
        assert_eq!(result, Err(LaunchElevatedError::Cancelled));
    }

    #[test]
    fn classify_shell_execute_result_treats_other_low_values_as_failed_not_cancelled() {
        // SE_ERR_FNF（ファイルが見つからない）相当の値。
        let result = classify_shell_execute_result(2);
        assert!(matches!(result, Err(LaunchElevatedError::Failed { .. })));
        assert_ne!(result, Err(LaunchElevatedError::Cancelled));
    }

    #[test]
    fn classify_shell_execute_result_boundary_value_32_is_still_a_failure() {
        // 仕様上「32 を超える」が成功の条件であり、32 自体は失敗側。
        assert!(classify_shell_execute_result(32).is_err());
    }
}
