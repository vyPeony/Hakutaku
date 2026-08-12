//! WebView2 Runtime の解決（`DIST-006`、`DIST-008`、`DIST-011`、`DIST-017`、`VER-003`、
//! P01-1）。
//!
//! `tasks/phase-01-bootstrap-webview2.md` の「起動手順の実装順序」手順 1〜4 を実装する。
//!
//! 1. `DIST-017`／`CFG-023` の先行読み込み（[`hakutaku_config::read_fixed_runtime_preference`]）。
//!    強制指定があれば手順 2（Evergreen 検出）を飛ばし、手順 3 へ進む。
//! 2. 互換性のある導入済み Evergreen Runtime を確認する。
//! 3. 見つからなければ、実行ファイル直下の `WebView2Runtime`（Fixed Version）を確認する。
//! 4. Fixed Version が見つかったら、`bootstrap::acl::ensure_app_container_access` で
//!    フォルダ ACL を確認してから、`WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` にその絶対パスを
//!    プロセス内で設定する（`DIST-008`。レジストリ登録・システムフォルダ配置は行わない）。
//!
//! ユーザーデータフォルダの指定（手順 5）と、両方の Runtime が使えない場合の
//! ネイティブダイアログ通知（手順 6）は、統合担当（`bootstrap::mod` / P01-2）が
//! この関数の戻り値（[`ResolvedRuntime`] / [`RuntimeUnavailable`]）を使って行う。
//!
//! # このモジュールが触れないもの
//!
//! `WebView2Runtime` 配下の**ファイル**は一切読み書きしない（`DIST-011`）。行うのは
//! フォルダの存在確認・版の問い合わせ（Win32 API 経由）・ACL の確認と、必要なら
//! ACL のメタデータだけの変更（`bootstrap::acl` に委譲）である。Runtime のインストール・
//! レジストリ登録・システムフォルダへの配置は行わない（`DIST-008`）。

use std::path::PathBuf;

use hakutaku_config::FixedRuntimePreference;
use hakutaku_diagnostics::{diag_error, diag_info, diag_warn, Diagnostics};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    CompareBrowserVersions, GetAvailableCoreWebView2BrowserVersionString,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::bootstrap::acl::{self, AclOutcome};
use crate::bootstrap::layout::Layout;

/// 診断ログの `module` フィールド（`DIAG-005`）。
const MODULE: &str = "bootstrap::runtime";

/// 環境変数 `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER`（`DIST-008`）の名前。
///
/// **重要（実機検証済み）:** この環境変数が既に設定されていると、
/// [`GetAvailableCoreWebView2BrowserVersionString`] は明示的に渡した
/// `browser_executable_folder` 引数よりもこの環境変数を優先する（WebView2Loader.dll の
/// 挙動）。そのため、Evergreen・Fixed Version いずれの検出の直前にも、必ずこの変数を
/// 未設定にしてから呼び出す。特に [`FixedRuntimePreference::ForceFixedVersion`] の
/// 場合は手順 2（Evergreen 検出）を経由しないため、そちらでの消去機会がない。
const WEBVIEW2_BROWSER_EXECUTABLE_FOLDER_ENV: &str = "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER";

/// 診断ログ用エラーコード（領域 `W2`: WebView2 Runtime の検出・準備）。
///
/// 書式と採番規則は `docs/development/error-codes.md` を正本とし、このモジュールが
/// 領域 `W2` の採番台帳である。番号は既存の最大値 + 1 で追加し、一度 `main` へ
/// マージした番号の意味変更と再利用はしない（欠番はコメントで残す）。
mod error_codes {
    /// `DIST-017`／`CFG-023` の先行読み込みで値を確定できなかった。
    pub const PREFLIGHT_UNDETERMINED: &str = "HKT-W2-0001";
    /// 導入済み Evergreen Runtime の版が最低要求版未満。
    pub const EVERGREEN_TOO_OLD: &str = "HKT-W2-0002";
    /// Fixed Version Runtime が見つからない。
    pub const FIXED_VERSION_NOT_FOUND: &str = "HKT-W2-0003";
    /// Fixed Version Runtime の版が最低要求版未満。
    pub const FIXED_VERSION_TOO_OLD: &str = "HKT-W2-0004";
    /// `WebView2Runtime` フォルダの ACL 要否を判定できなかった。
    pub const ACL_UNDETERMINED: &str = "HKT-W2-0005";
    /// `WebView2Runtime` フォルダの ACL 付与が現在の権限では行えない。
    pub const ACL_DENIED: &str = "HKT-W2-0006";
    /// Evergreen・Fixed Version のどちらも使用できない。
    pub const RUNTIME_UNAVAILABLE: &str = "HKT-W2-0007";
}

/// 起動を許可する WebView2 ブラウザーの最低版。
///
/// WebView2 は Edge 86（2020年11月、`86.0.616.0`）で GA（General Availability）となった。
/// Hakutaku はそれ以降に追加された特別な新しい WebView2 API には依存していないため、
/// この GA 版を安全網としての下限に採用する。**機能面の要求から導いた値ではなく**、
/// 「壊れた・極端に古い Runtime を誤って採用しない」ための下限であり、
/// Evergreen Runtime は自動更新されるため実運用でこの下限に抵触することは基本的にない
/// （現在配置されている Fixed Version は `150.0.4078.105` であり、十分に上回る）。
pub const MINIMUM_SUPPORTED_VERSION: &str = "86.0.616.0";

/// 解決した Runtime の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeKind {
    /// 導入済みの Evergreen Runtime。
    Evergreen,
    /// 実行ファイル直下の `WebView2Runtime`（Fixed Version）。
    FixedVersion,
}

/// Runtime 解決に成功した結果。
#[derive(Clone, Debug)]
pub struct ResolvedRuntime {
    /// 使用する Runtime の種類。
    pub kind: RuntimeKind,
    /// 検出したブラウザーの版文字列。
    pub version: String,
    /// `FixedVersion` のときだけ `Some`。`WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` に
    /// 設定した絶対パス。
    pub browser_executable_folder: Option<PathBuf>,
    /// `FixedVersion` のときだけ `Some`。`WebView2Runtime` フォルダの ACL 確認結果。
    pub acl: Option<AclOutcome>,
    /// `DIST-017`／`CFG-023` の先行読み込み結果（診断ログ用）。
    pub preference: FixedRuntimePreference,
}

/// Evergreen・Fixed Version のどちらも使用できなかった場合の詳細。
#[derive(Clone, Debug)]
pub struct RuntimeUnavailable {
    /// Evergreen Runtime を使用できなかった理由（日本語）。
    pub evergreen_detail: String,
    /// Fixed Version Runtime を使用できなかった理由（日本語）。
    pub fixed_detail: String,
    /// ACL 不足が原因の場合だけ `Some`。呼び出し側が
    /// `bootstrap::notify::acl_not_applicable` を出す判断材料にする。
    pub acl_denied: Option<AclOutcome>,
}

/// 起動手順 1〜4 を実行する。
///
/// 成功時は `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` の設定まで完了している
/// （`FixedVersion` の場合のみ。`Evergreen` の場合はプロセス内の環境変数を変更しない）。
pub fn resolve(
    layout: &Layout,
    diagnostics: &Diagnostics,
) -> Result<ResolvedRuntime, RuntimeUnavailable> {
    let preference = read_preference(layout, diagnostics);

    // 手順 1: 強制指定があれば Evergreen 検出（手順 2）を飛ばして手順 3 へ進む。
    let evergreen_detail = if matches!(preference, FixedRuntimePreference::ForceFixedVersion) {
        let detail = "DIST-017 の設定（webview2.force_fixed_version_runtime）により、Evergreen \
            Runtime の検出をスキップしました。"
            .to_string();
        diag_info!(
            diagnostics,
            module = MODULE,
            operation = "runtime.evergreen.skip",
            "{detail}"
        );
        detail
    } else {
        match detect_evergreen(diagnostics) {
            Ok(version) => {
                let resolved = ResolvedRuntime {
                    kind: RuntimeKind::Evergreen,
                    version,
                    browser_executable_folder: None,
                    acl: None,
                    preference,
                };
                diag_info!(
                    diagnostics,
                    module = MODULE,
                    operation = "runtime.resolve",
                    "{}",
                    resolved.diagnostic_summary()
                );
                return Ok(resolved);
            }
            Err(detail) => detail,
        }
    };

    diag_info!(
        diagnostics,
        module = MODULE,
        operation = "runtime.resolve",
        "Evergreen Runtime を使用できないため、Fixed Version Runtime を確認します: \
         {evergreen_detail}"
    );

    match resolve_fixed_version(layout, diagnostics, preference) {
        FixedVersionOutcome::Available(resolved) => {
            diag_info!(
                diagnostics,
                module = MODULE,
                operation = "runtime.resolve",
                "{}",
                resolved.diagnostic_summary()
            );
            Ok(resolved)
        }
        FixedVersionOutcome::NotFound(fixed_detail) => {
            let unavailable = RuntimeUnavailable {
                evergreen_detail,
                fixed_detail,
                acl_denied: None,
            };
            diag_error!(
                diagnostics,
                module = MODULE,
                operation = "runtime.resolve",
                error_code = error_codes::RUNTIME_UNAVAILABLE,
                "{}",
                unavailable.diagnostic_summary()
            );
            Err(unavailable)
        }
        FixedVersionOutcome::AclDenied {
            detail: fixed_detail,
            outcome,
        } => {
            let unavailable = RuntimeUnavailable {
                evergreen_detail,
                fixed_detail,
                acl_denied: Some(outcome),
            };
            diag_error!(
                diagnostics,
                module = MODULE,
                operation = "runtime.resolve",
                error_code = error_codes::RUNTIME_UNAVAILABLE,
                "{}",
                unavailable.diagnostic_summary()
            );
            Err(unavailable)
        }
    }
}

impl ResolvedRuntime {
    /// 診断ログ用に、使用する Runtime の要点を1行にまとめる（`DIST-017` の受け入れ条件:
    /// 「使用中の Runtime を診断ログで確認できる」）。
    ///
    /// Win32 API を呼ばない純粋な文字列組み立てであり、`#[cfg(test)]` から直接検証できる。
    fn diagnostic_summary(&self) -> String {
        match &self.browser_executable_folder {
            Some(folder) => format!(
                "Fixed Version WebView2 Runtime を使用します: 版={}, フォルダ={}, 先行読み込み={:?}",
                self.version,
                folder.display(),
                self.preference
            ),
            None => format!(
                "導入済み Evergreen Runtime を使用します: 版={}, 先行読み込み={:?}",
                self.version, self.preference
            ),
        }
    }
}

impl RuntimeUnavailable {
    /// 診断ログ用に、両方の Runtime が使用できなかった理由を1行にまとめる。
    ///
    /// Win32 API を呼ばない純粋な文字列組み立てであり、`#[cfg(test)]` から直接検証できる。
    fn diagnostic_summary(&self) -> String {
        format!(
            "使用可能な WebView2 Runtime がありません。Evergreen: {} / Fixed Version: {}",
            self.evergreen_detail, self.fixed_detail
        )
    }
}

/// `DIST-017`／`CFG-023` の先行読み込みを行い、結果を診断ログへ記録する。
///
/// 値を確定できない場合（`Missing` / `Undetermined`）も含め、常に既定へ落とし込んだ
/// [`FixedRuntimePreference`] を返す。安全モードへ入るかどうかの判断（`CFG-016`）は
/// P03 に引き継ぎ、ここでは行わない。
fn read_preference(layout: &Layout, diagnostics: &Diagnostics) -> FixedRuntimePreference {
    let outcome = hakutaku_config::read_fixed_runtime_preference(layout.config_path());

    match &outcome {
        hakutaku_config::PreflightOutcome::Missing => {
            diag_info!(
                diagnostics,
                module = MODULE,
                operation = "runtime.preflight",
                "設定ファイルが見つかりません。既定（Auto）で続行します: {}",
                layout.config_path().display()
            );
        }
        hakutaku_config::PreflightOutcome::Determined(preference) => {
            diag_info!(
                diagnostics,
                module = MODULE,
                operation = "runtime.preflight",
                "webview2.force_fixed_version_runtime の先行読み込み結果: {preference:?}"
            );
        }
        hakutaku_config::PreflightOutcome::Undetermined {
            reason,
            line,
            column,
        } => {
            diag_warn!(
                diagnostics,
                module = MODULE,
                operation = "runtime.preflight",
                error_code = error_codes::PREFLIGHT_UNDETERMINED,
                "webview2.force_fixed_version_runtime を確定できませんでした（既定 Auto \
                 で続行します）: {reason}（行={line:?}, 列={column:?}）"
            );
        }
    }

    outcome.preference_or_default()
}

/// 導入済み Evergreen Runtime を検出する。成功すれば版文字列、失敗すれば
/// 日本語の理由を返す。
fn detect_evergreen(diagnostics: &Diagnostics) -> Result<String, String> {
    // 要件: Evergreen 検出の前に、環境変数を必ず未設定にする。
    // 残っていると Evergreen 検出が Fixed Version を指してしまう
    // （`WEBVIEW2_BROWSER_EXECUTABLE_FOLDER_ENV` の doc コメントを参照）。
    //
    // SAFETY 注記: `remove_var` はプロセス環境を変更する。Hakutaku の起動シーケンスは
    // シングルスレッドでここまで到達するため、他スレッドとの競合はない。
    std::env::remove_var(WEBVIEW2_BROWSER_EXECUTABLE_FOLDER_ENV);

    let version = match query_browser_version(PCWSTR::null()) {
        Ok(version) => version,
        Err(error) => {
            let detail = format!("導入済み Evergreen Runtime を検出できませんでした: {error}");
            diag_info!(
                diagnostics,
                module = MODULE,
                operation = "runtime.evergreen.detect",
                "{detail}"
            );
            return Err(detail);
        }
    };

    if meets_minimum_version(&version, diagnostics, "runtime.evergreen.detect") {
        diag_info!(
            diagnostics,
            module = MODULE,
            operation = "runtime.evergreen.detect",
            "導入済み Evergreen Runtime を検出しました: 版={version}"
        );
        Ok(version)
    } else {
        let detail = format!(
            "導入済み Evergreen Runtime の版 {version} が最低要求版 \
             {MINIMUM_SUPPORTED_VERSION} 未満です。"
        );
        diag_warn!(
            diagnostics,
            module = MODULE,
            operation = "runtime.evergreen.detect",
            error_code = error_codes::EVERGREEN_TOO_OLD,
            "{detail}"
        );
        Err(detail)
    }
}

/// [`resolve_fixed_version`] の内部結果。ACL 拒否かどうかを呼び出し側
/// （[`resolve`]）が区別できるようにするための列挙体。
enum FixedVersionOutcome {
    /// 使用可能。
    Available(ResolvedRuntime),
    /// 見つからない、または最低要求版未満。
    NotFound(String),
    /// 見つかったが ACL が現在の権限では設定できない。
    AclDenied { detail: String, outcome: AclOutcome },
}

/// 実行ファイル直下の `WebView2Runtime`（Fixed Version）を確認し、使用可能であれば
/// ACL を確認したうえで `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` を設定する。
///
/// `WebView2Runtime` 配下の**ファイル**は一切読み書きしない（`DIST-011`）。
fn resolve_fixed_version(
    layout: &Layout,
    diagnostics: &Diagnostics,
    preference: FixedRuntimePreference,
) -> FixedVersionOutcome {
    let runtime_dir = layout.webview2_runtime_dir();

    // `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER_ENV` の doc コメントのとおり、この環境変数が
    // 残っていると、これから渡す `runtime_dir` ではなく環境変数側のフォルダが検出されて
    // しまう。`force_fixed_version_runtime` による強制時は Evergreen 検出（手順2）を
    // 経由せず、そちらでの消去機会がないため、ここで改めて消しておく。
    std::env::remove_var(WEBVIEW2_BROWSER_EXECUTABLE_FOLDER_ENV);

    let version = match query_browser_version(&HSTRING::from(runtime_dir)) {
        Ok(version) => version,
        Err(error) => {
            let detail = format!(
                "{} に Fixed Version WebView2 Runtime が見つかりません: {error}",
                runtime_dir.display()
            );
            diag_warn!(
                diagnostics,
                module = MODULE,
                operation = "runtime.fixed_version.detect",
                error_code = error_codes::FIXED_VERSION_NOT_FOUND,
                "{detail}"
            );
            return FixedVersionOutcome::NotFound(detail);
        }
    };

    if !meets_minimum_version(&version, diagnostics, "runtime.fixed_version.detect") {
        let detail = format!(
            "{} の Fixed Version WebView2 Runtime（版 {version}）が最低要求版 \
             {MINIMUM_SUPPORTED_VERSION} 未満です。",
            runtime_dir.display()
        );
        diag_warn!(
            diagnostics,
            module = MODULE,
            operation = "runtime.fixed_version.detect",
            error_code = error_codes::FIXED_VERSION_TOO_OLD,
            "{detail}"
        );
        return FixedVersionOutcome::NotFound(detail);
    }

    diag_info!(
        diagnostics,
        module = MODULE,
        operation = "runtime.fixed_version.detect",
        "Fixed Version WebView2 Runtime を検出しました: 版={version}, フォルダ={}",
        runtime_dir.display()
    );

    // `WebView2Runtime` フォルダ**だけ**を対象にした ACL の確認・設定（`DIST-010`）。
    // フォルダ内容（ファイル）は一切変更しない（`DIST-011`）。
    let acl_outcome = acl::ensure_app_container_access(runtime_dir);

    let denied_detail = match &acl_outcome {
        AclOutcome::AlreadyAccessible => {
            diag_info!(
                diagnostics,
                module = MODULE,
                operation = "runtime.fixed_version.acl",
                "App Container からのアクセスは既に許可されています: {}",
                runtime_dir.display()
            );
            None
        }
        AclOutcome::Applied => {
            diag_info!(
                diagnostics,
                module = MODULE,
                operation = "runtime.fixed_version.acl",
                "App Container からのアクセスを付与しました: {}",
                runtime_dir.display()
            );
            None
        }
        AclOutcome::Undetermined { reason } => {
            diag_warn!(
                diagnostics,
                module = MODULE,
                operation = "runtime.fixed_version.acl",
                error_code = error_codes::ACL_UNDETERMINED,
                "App Container の ACL 要否を判定できませんでした（続行します）: {reason}"
            );
            None
        }
        AclOutcome::Denied {
            reason,
            required_privilege,
        } => {
            let detail = format!(
                "{} の App Container 用 ACL を現在の権限では設定できません: {reason}\
                 （必要な権限: {required_privilege}）",
                runtime_dir.display()
            );
            diag_error!(
                diagnostics,
                module = MODULE,
                operation = "runtime.fixed_version.acl",
                error_code = error_codes::ACL_DENIED,
                "{detail}"
            );
            Some(detail)
        }
    };

    if let Some(detail) = denied_detail {
        // ACL 不足のため Fixed Version は使用できないものとして扱う。
        // 通知（`bootstrap::notify::acl_not_applicable`）は統合担当が行う。
        return FixedVersionOutcome::AclDenied {
            detail,
            outcome: acl_outcome,
        };
    }

    // DIST-008: レジストリ登録・システムフォルダへの配置・インストールは行わず、
    // プロセス内で環境変数を設定するだけで Fixed Version を使用可能にする。
    //
    // SAFETY 注記: `set_var` はプロセス環境を変更する。Hakutaku の起動シーケンスは
    // シングルスレッドでここまで到達するため、他スレッドとの競合はない。
    std::env::set_var(WEBVIEW2_BROWSER_EXECUTABLE_FOLDER_ENV, runtime_dir);

    FixedVersionOutcome::Available(ResolvedRuntime {
        kind: RuntimeKind::FixedVersion,
        version,
        browser_executable_folder: Some(runtime_dir.to_path_buf()),
        acl: Some(acl_outcome),
        preference,
    })
}

/// `browser_executable_folder` にある WebView2Loader へ問い合わせ、検出された
/// ブラウザーの版文字列を返す。
///
/// `folder` に `PCWSTR::null()` を渡すと導入済み Evergreen Runtime を、
/// `WebView2Runtime` の絶対パス（`HSTRING` 経由）を渡すと Fixed Version を検出する。
fn query_browser_version<P0>(folder: P0) -> windows::core::Result<String>
where
    P0: windows::core::Param<PCWSTR>,
{
    let mut version_ptr = PWSTR::null();

    // SAFETY: `folder` は null（導入済み Evergreen の検索）か、呼び出し元が生存させて
    // いる NUL 終端のワイド文字列（`HSTRING`）を指す `PCWSTR` である。`version_ptr` は
    // このスタックフレーム上の有効な `PWSTR` への可変参照であり、WebView2Loader.dll は
    // 成功した場合にのみ `CoTaskMemAlloc` で確保したバッファのポインタをここへ書き込む。
    let result = unsafe { GetAvailableCoreWebView2BrowserVersionString(folder, &mut version_ptr) };
    result?;

    // `version_ptr` は直前の呼び出しが成功した場合にのみ、CoTaskMemAlloc 済みの
    // NUL 終端ワイド文字列を指す。`webview2_com::take_pwstr` はこれを `String` へ
    // コピーしたうえで `CoTaskMemFree` により解放する（同関数自体は safe fn であり、
    // 内部で必要な unsafe 操作を完結させている）。
    Ok(webview2_com::take_pwstr(version_ptr))
}

/// `detected` が [`MINIMUM_SUPPORTED_VERSION`] 以上かどうかを判定する。
///
/// 版文字列の形式が想定と異なるなどで比較そのものに失敗した場合は、Evergreen を
/// 不必要に弾かないよう安全側（許可）に倒し、警告として記録したうえで `true` を返す。
fn meets_minimum_version(detected: &str, diagnostics: &Diagnostics, operation: &str) -> bool {
    let mut comparison: i32 = 0;
    let detected_wide = HSTRING::from(detected);
    let minimum_wide = HSTRING::from(MINIMUM_SUPPORTED_VERSION);

    // SAFETY: `detected_wide` と `minimum_wide` はどちらも、この呼び出しが完了するまで
    // 生存する有効な NUL 終端のワイド文字列（`HSTRING`）である。`comparison` は
    // このスタックフレーム上の有効な `i32` への可変参照である。
    let result = unsafe { CompareBrowserVersions(&detected_wide, &minimum_wide, &mut comparison) };

    match result {
        Ok(()) => is_at_least_minimum(comparison),
        Err(error) => {
            diag_warn!(
                diagnostics,
                module = MODULE,
                operation = operation,
                "版 {detected} と最低要求版 {MINIMUM_SUPPORTED_VERSION} の比較に失敗しました。\
                 安全側として続行します: {error}"
            );
            true
        }
    }
}

/// `CompareBrowserVersions` の比較結果（`detected <=> minimum`）が、最低要求版以上を
/// 意味するかどうかを判定する。Win32 API を呼ばない純粋なロジックであり、
/// `#[cfg(test)]` から直接検証できる。
fn is_at_least_minimum(comparison: i32) -> bool {
    comparison >= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    // 以下は Win32 API を一切呼ばない純粋なロジックだけを検証する。

    #[test]
    fn is_at_least_minimum_treats_zero_and_positive_as_supported() {
        assert!(is_at_least_minimum(0));
        assert!(is_at_least_minimum(1));
        assert!(!is_at_least_minimum(-1));
    }

    #[test]
    fn minimum_supported_version_has_the_expected_dotted_form() {
        // 最低要求版そのものが、CompareBrowserVersions が期待するドット区切りの
        // 数値形式になっていることを確認する（比較不能な値を選んでいないことの保証）。
        assert_eq!(MINIMUM_SUPPORTED_VERSION, "86.0.616.0");
        assert_eq!(MINIMUM_SUPPORTED_VERSION.split('.').count(), 4);
        for part in MINIMUM_SUPPORTED_VERSION.split('.') {
            assert!(
                part.chars().all(|c| c.is_ascii_digit()),
                "数値以外を含んでいます: {part}"
            );
        }
    }

    #[test]
    fn diagnostic_summary_for_evergreen_mentions_version_but_not_a_folder() {
        let resolved = ResolvedRuntime {
            kind: RuntimeKind::Evergreen,
            version: "120.0.0.0".to_string(),
            browser_executable_folder: None,
            acl: None,
            preference: FixedRuntimePreference::Auto,
        };

        let summary = resolved.diagnostic_summary();
        assert!(summary.contains("Evergreen"));
        assert!(summary.contains("120.0.0.0"));
        assert!(summary.contains("Auto"));
    }

    #[test]
    fn diagnostic_summary_for_fixed_version_mentions_version_and_absolute_folder() {
        let resolved = ResolvedRuntime {
            kind: RuntimeKind::FixedVersion,
            version: "150.0.4078.105".to_string(),
            browser_executable_folder: Some(PathBuf::from(r"C:\App\WebView2Runtime")),
            acl: Some(AclOutcome::AlreadyAccessible),
            preference: FixedRuntimePreference::ForceFixedVersion,
        };

        let summary = resolved.diagnostic_summary();
        assert!(summary.contains("Fixed Version"));
        assert!(summary.contains("150.0.4078.105"));
        assert!(summary.contains(r"C:\App\WebView2Runtime"));
        assert!(summary.contains("ForceFixedVersion"));
    }

    #[test]
    fn diagnostic_summary_for_unavailable_includes_both_details() {
        let unavailable = RuntimeUnavailable {
            evergreen_detail: "Evergreen の理由".to_string(),
            fixed_detail: "Fixed Version の理由".to_string(),
            acl_denied: None,
        };

        let summary = unavailable.diagnostic_summary();
        assert!(summary.contains("Evergreen の理由"));
        assert!(summary.contains("Fixed Version の理由"));
    }
}
