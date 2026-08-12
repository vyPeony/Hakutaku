//! ブートストラップの統合（P01 統合担当）。
//!
//! `tasks/phase-01-bootstrap-webview2.md` の「起動手順の実装順序」と、
//! `docs`（P01 実装契約の「契約 8」）が定める手順を、`tasks/phase-03-configuration.md`
//! （P03-2）の設定読み込み手順と合わせて、ここで 1 本の関数
//! [`run`] として結線する。各手順の実装そのものはサブモジュール
//! （[`layout`]・[`notify`]・[`process`]・[`acl`]・[`runtime`]・[`config`]）に
//! 委譲しており、このファイルはそれらの呼び出し順序と、失敗時の通知・継続可否の
//! 判断だけを持つ。
//!
//! # 手順の対応
//!
//! 1. 所要時間の計測を開始する（計画書「影響」節の「所要時間を記録します」）。
//! 2. [`layout::Layout::discover`] で実行時フォルダの位置を解決する。失敗すると
//!    診断ログの保存先も通知先パスも決められないため、[`notify::show`] で理由を
//!    表示して [`Aborted`] を返す。
//! 3. [`process::current_elevation`] で起動プロセスの昇格状態を取得する（`DIAG-007`）。
//! 4. [`config::ConfigState::load`] で `hakutaku.yaml` を読み込み、`CFG-007` の
//!    メモリ予算（`hakutaku_memory_accounting::set_global_budget_bytes`）を
//!    ここで確定・適用する（P03-2）。診断ログはまだ開いていないため、
//!    結果の記録は手順5まで持ち越す（[`config`] モジュール doc コメントの
//!    「`CFG-020` の適用順序について」を参照）。
//! 5. [`layout::Layout::ensure_logs`] で `logs` を用意し、手順4の設定から導いた
//!    [`config::ConfigState::rotation_policy`]（`CFG-020`）で診断ログを開く。
//!    失敗しても [`notify::diagnostics_unavailable`] を表示して**続行する**
//!    （`DIAG-006`）。別の保存先へは自動フォールバックしない。開けた後、手順4の
//!    設定読み込み結果（経路・エラー件数）を診断ログへ記録する（`CFG-015`、
//!    `CFG-016`）。
//! 6. [`layout::Layout::ensure_temp`] で `temp` を用意し、[`layout::Layout::purge_temp`]
//!    で残存ファイルを清掃する（`SEC-006`）。`temp` を用意できなくても起動は止めず、
//!    警告として診断ログに記録するだけにする。
//! 7. [`process::apply_process_priority`] で、手順4の設定（`performance.process_priority`、
//!    `CFG-024`）に従いプロセス優先度を設定する（`PERF-014`、P11-3）。失敗しても
//!    起動は止めず、警告として診断ログに記録するだけにする。
//! 8. [`runtime::resolve`] で WebView2 Runtime を解決する。失敗した場合、ACL 起因なら
//!    先に [`notify::acl_not_applicable`]、続けて [`notify::runtime_unavailable`] を
//!    表示し、**Tauri を一切初期化せず** [`Aborted`] を返す（`TECH-005`、`DIST-009`）。
//! 9. [`layout::Layout::ensure_webview2_data`] で WebView2 のユーザーデータフォルダを
//!    用意する（`DIST-013`）。失敗したら [`notify::webview2_data_unavailable`] を表示し、
//!    **別の場所へフォールバックせず** [`Aborted`] を返す（`DIST-014`）。
//! 10. 所要時間を確定し、使用した Runtime・各フォルダの絶対パスとともに診断ログへ
//!     記録して [`Bootstrap`] を返す。
//!
//! ユーザーデータフォルダの指定（Tauri の `WebviewWindowBuilder` への
//! `data_directory` 設定）そのものと、正常終了時の `temp` 再清掃（`SEC-006`）は
//! Tauri の初期化・終了に密接に絡むため、`src-tauri/src/lib.rs` 側の責務とする。

pub mod acl;
pub mod config;
pub mod layout;
pub mod notify;
pub mod process;
pub mod runtime;

use std::time::{Duration, Instant};

use hakutaku_diagnostics::{diag_error, diag_info, diag_warn, Diagnostics, DiagnosticsUnavailable};

/// 実行ファイルの位置を解決できなかった場合の終了コード（手順2）。
pub const EXIT_CODE_LAYOUT_UNAVAILABLE: i32 = 2;
/// Evergreen・Fixed Version のどちらの WebView2 Runtime も使用できなかった場合の
/// 終了コード（手順7）。
pub const EXIT_CODE_RUNTIME_UNAVAILABLE: i32 = 3;
/// WebView2 のユーザーデータフォルダを用意できなかった場合の終了コード（手順8）。
pub const EXIT_CODE_WEBVIEW2_DATA_UNAVAILABLE: i32 = 4;

/// 診断ログの `module` フィールド（`DIAG-005`）。統合担当の記録はすべてここへ集約する。
const MODULE: &str = "bootstrap";

/// ブートストラップの成果。Tauri 初期化へ渡す。
pub struct Bootstrap {
    pub layout: layout::Layout,
    pub diagnostics: Diagnostics,
    pub runtime: runtime::ResolvedRuntime,
    pub webview2_data_dir: std::path::PathBuf,
    pub elapsed: Duration,
    /// 起動時に読み込んだ `hakutaku.yaml` の状態（P03-2）。
    /// `src-tauri/src/lib.rs` が Tauri の managed state として保持する。
    pub config: config::ConfigState,
}

/// ブートストラップを中止した場合の結果。
///
/// 通知（ネイティブダイアログ）は [`run`] の内部で表示済みであり、呼び出し側
/// （`src-tauri/src/lib.rs`）は Tauri を初期化せず `exit_code` で終了するだけでよい。
///
/// `exit_code` の一覧:
///
/// - `2`（[`EXIT_CODE_LAYOUT_UNAVAILABLE`]）: 実行ファイルの位置を解決できない
///   （[`layout::Layout::discover`] が失敗した）。
/// - `3`（[`EXIT_CODE_RUNTIME_UNAVAILABLE`]）: WebView2 Runtime を使用できない
///   （Evergreen・Fixed Version のいずれも解決できなかった）。
/// - `4`（[`EXIT_CODE_WEBVIEW2_DATA_UNAVAILABLE`]）: WebView2 のユーザーデータフォルダ
///   を作成・書き込みできない。
///
/// これら以外に、Tauri 自体の起動失敗用の終了コード `1` を `src-tauri/src/lib.rs`
/// 側で定義している（`bootstrap` モジュールの範囲外のため、ここには含めない）。
pub struct Aborted {
    pub exit_code: i32,
}

/// 起動手順 1〜9 を実行する。
///
/// `Err` の場合、利用者への通知は既に表示済みであり、呼び出し側は Tauri を
/// 初期化せず `Aborted::exit_code` で終了するだけでよい。
pub fn run() -> Result<Bootstrap, Aborted> {
    // 手順1: 所要時間の計測を開始する。
    let start = Instant::now();

    // 手順2: 実行時フォルダの位置を解決する。失敗すると診断ログの保存先も
    // 通知先パスも決められないため、ここでは診断ログを使わず直接通知する。
    let layout = match layout::Layout::discover() {
        Ok(layout) => layout,
        Err(error) => {
            let notice = notify::Notice {
                kind: notify::NoticeKind::Error,
                title: "Hakutaku: 起動に失敗しました".to_string(),
                body: format!(
                    "実行ファイルの位置を解決できないため、Hakutaku を起動できません。\n\
                     \n\
                     理由:\n\
                     \u{20}\u{20}{error}\n\
                     \n\
                     実行ファイルを移動・複製した直後である場合は、正しい場所に配置し直した\n\
                     うえで Hakutaku を再度起動してください。"
                ),
            };
            notify::show(&notice);
            return Err(Aborted {
                exit_code: EXIT_CODE_LAYOUT_UNAVAILABLE,
            });
        }
    };

    // 手順3: 起動プロセスの昇格状態を取得する（DIAG-007）。
    let elevation = process::current_elevation();

    // 手順4: hakutaku.yaml を読み込む（CFG-014〜CFG-017）。診断ログはまだ開いて
    // いないため、ここでは記録せず、手順5で診断ログを開いた後に記録する
    // （config モジュール doc コメントの「CFG-020 の適用順序について」参照）。
    let config_state = config::ConfigState::load(layout.config_path());

    // CFG-007: メモリ予算は予約が発生する前の、起動シーケンスのできるだけ早い
    // 段階で一度だけ適用する
    // （`hakutaku_memory_accounting::set_global_budget_bytes` の呼び出し契約）。
    // Tauri はまだ初期化しておらず、この時点で確実に予約はまだ発生していない。
    hakutaku_memory_accounting::set_global_budget_bytes(config_state.memory_budget_bytes());

    // 手順5: logs を用意し、手順4の設定から導いたローテーション設定（CFG-020）で
    // 診断ログを開く（DIAG-001、DIAG-006、DIAG-007）。失敗しても診断ログなしで
    // 続行し、別の保存先へは自動フォールバックしない。
    let rotation_policy = config_state.rotation_policy();
    let diagnostics = match layout.ensure_logs() {
        Ok(_) => {
            let (diagnostics, unavailable) =
                Diagnostics::open(layout.logs_dir(), rotation_policy, elevation);
            if let Some(reason) = &unavailable {
                notify::show(&notify::diagnostics_unavailable(reason));
            }
            diagnostics
        }
        Err(failure) => {
            let reason = diagnostics_unavailable_from_directory_failure(&failure);
            notify::show(&notify::diagnostics_unavailable(&reason));
            Diagnostics::unavailable(reason)
        }
    };

    // 手順4の設定読み込み結果を、診断ログが開いた後に記録する。正常起動・
    // 既定値起動（CFG-015）は Info、安全モード（CFG-016）は Warn とする
    // （安全モードは利用者の対処を要するため）。
    match config_state.route {
        config::ConfigRoute::Loaded => {
            diag_info!(
                diagnostics,
                module = MODULE,
                operation = "startup.config_load",
                "hakutaku.yaml を読み込みました: {}",
                layout.config_path().display()
            );
        }
        config::ConfigRoute::Missing => {
            diag_info!(
                diagnostics,
                module = MODULE,
                operation = "startup.config_load",
                "hakutaku.yaml が見つかりません。組み込み既定値で起動します（CFG-015）: {}",
                layout.config_path().display()
            );
        }
        config::ConfigRoute::Invalid => {
            diag_warn!(
                diagnostics,
                module = MODULE,
                operation = "startup.config_load",
                "hakutaku.yaml の検証に失敗しました。安全モードで起動します（CFG-016）: \
                 エラー {} 件, {}",
                config_state.errors.len(),
                layout.config_path().display()
            );
            for error in &config_state.errors {
                diag_warn!(
                    diagnostics,
                    module = MODULE,
                    operation = "startup.config_load",
                    "{error}"
                );
            }
        }
    }

    // 手順6: temp を用意し、残存ファイルを清掃する（SEC-006）。
    // 用意できなくても起動は止めず、警告として診断ログに記録するだけにする。
    match layout.ensure_temp() {
        Ok(_) => {
            let report = layout.purge_temp();
            diag_info!(
                diagnostics,
                module = MODULE,
                operation = "startup.temp_purge",
                "起動時の temp 清掃が完了しました: 削除 {} 件、失敗 {} 件（対象: {}）",
                report.removed_entries,
                report.failures.len(),
                layout.temp_dir().display()
            );
            for failure in &report.failures {
                diag_warn!(
                    diagnostics,
                    module = MODULE,
                    operation = "startup.temp_purge",
                    "temp 配下のエントリを削除できませんでした: {}（{}）",
                    failure.target.display(),
                    failure.reason
                );
            }
        }
        Err(failure) => {
            diag_warn!(
                diagnostics,
                module = MODULE,
                operation = "startup.temp_prepare",
                "temp フォルダを用意できません。起動は継続します: {}（{}）",
                failure.target.display(),
                failure.reason
            );
        }
    }

    // 手順7: プロセス優先度を設定する（PERF-014、CFG-024、P11-3）。既定値は
    // 運用先の対象端末上での実行を前提に控えめな below_normal（hakutaku_config::
    // PerformanceConfig::default の doc コメント参照。確定は P13 の実測）。
    // 失敗しても起動は止めず、警告として診断ログに記録するだけにする
    // （抑制が効かないだけであり、致命的ではないため）。
    match process::apply_process_priority(config_state.config.performance.process_priority) {
        Ok(()) => {
            diag_info!(
                diagnostics,
                module = MODULE,
                operation = "startup.process_priority",
                "プロセス優先度を設定しました（PERF-014）: {:?}",
                config_state.config.performance.process_priority
            );
        }
        Err(reason) => {
            diag_warn!(
                diagnostics,
                module = MODULE,
                operation = "startup.process_priority",
                "プロセス優先度を設定できません。既定（Windows が割り当てた優先度）のまま\
                 起動を継続します: {reason}"
            );
        }
    }

    // 手順8: WebView2 Runtime を解決する（DIST-006、DIST-008、DIST-010、DIST-017）。
    let runtime = match runtime::resolve(&layout, &diagnostics) {
        Ok(runtime) => runtime,
        Err(unavailable) => {
            // ACL 不足が原因の場合は、Runtime 不使用の通知より先にこちらを表示する
            // （契約8: 「先に notify::acl_not_applicable を表示する」）。
            if let Some(acl::AclOutcome::Denied {
                reason,
                required_privilege,
            }) = &unavailable.acl_denied
            {
                notify::show(&notify::acl_not_applicable(
                    layout.webview2_runtime_dir(),
                    reason,
                    required_privilege,
                ));
            }

            notify::show(&notify::runtime_unavailable(
                layout.webview2_runtime_dir(),
                &unavailable.evergreen_detail,
                &unavailable.fixed_detail,
            ));

            // TECH-005 / DIST-009: Tauri を一切初期化せず終了する。
            return Err(Aborted {
                exit_code: EXIT_CODE_RUNTIME_UNAVAILABLE,
            });
        }
    };

    // 手順9: WebView2 のユーザーデータフォルダを用意する（DIST-013、DIST-014）。
    // 失敗しても別の場所へはフォールバックせず、起動を中止する。
    let webview2_data_dir = match layout.ensure_webview2_data() {
        Ok(path) => path.to_path_buf(),
        Err(failure) => {
            notify::show(&notify::webview2_data_unavailable(&failure));
            diag_error!(
                diagnostics,
                module = MODULE,
                operation = "startup.webview2_data",
                "WebView2 のユーザーデータフォルダを用意できないため起動を中止します: {}（{}）",
                failure.target.display(),
                failure.reason
            );
            return Err(Aborted {
                exit_code: EXIT_CODE_WEBVIEW2_DATA_UNAVAILABLE,
            });
        }
    };

    // 手順10: 所要時間を確定し、ブートストラップ完了の要点を診断ログへ記録する。
    let elapsed = start.elapsed();
    diag_info!(
        diagnostics,
        module = MODULE,
        operation = "startup.complete",
        "ブートストラップが完了しました: 所要時間={:?}, {}, exe_dir={}, logs_dir={}, \
         temp_dir={}, WebView2={}, WebView2Runtime={}",
        elapsed,
        runtime_summary(&runtime),
        layout.exe_dir().display(),
        layout.logs_dir().display(),
        layout.temp_dir().display(),
        webview2_data_dir.display(),
        layout.webview2_runtime_dir().display()
    );

    Ok(Bootstrap {
        layout,
        diagnostics,
        runtime,
        webview2_data_dir,
        elapsed,
        config: config_state,
    })
}

/// 診断ログ用に、解決した Runtime の要点を1行にまとめる（`DIST-017` の受け入れ条件:
/// 「使用中の Runtime を診断ログで確認できる」）。Win32 API を呼ばない純粋な文字列
/// 組み立てであり、`#[cfg(test)]` から直接検証できる。
fn runtime_summary(resolved: &runtime::ResolvedRuntime) -> String {
    match &resolved.browser_executable_folder {
        Some(folder) => format!(
            "Runtime種別=FixedVersion, 版={}, フォルダ={}",
            resolved.version,
            folder.display()
        ),
        None => format!("Runtime種別=Evergreen, 版={}", resolved.version),
    }
}

/// [`layout::DirectoryFailure`] を [`DiagnosticsUnavailable`] へ変換する
/// （手順4で `ensure_logs` 自体が失敗した場合に使う）。
///
/// Win32 API を呼ばない純粋な変換であり、`#[cfg(test)]` から直接検証できる。
fn diagnostics_unavailable_from_directory_failure(
    failure: &layout::DirectoryFailure,
) -> DiagnosticsUnavailable {
    DiagnosticsUnavailable {
        target: failure.target.clone(),
        reason: failure.reason.clone(),
        os_error_code: failure.os_error_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_pairwise_distinct() {
        assert_ne!(EXIT_CODE_LAYOUT_UNAVAILABLE, EXIT_CODE_RUNTIME_UNAVAILABLE);
        assert_ne!(
            EXIT_CODE_RUNTIME_UNAVAILABLE,
            EXIT_CODE_WEBVIEW2_DATA_UNAVAILABLE
        );
        assert_ne!(
            EXIT_CODE_LAYOUT_UNAVAILABLE,
            EXIT_CODE_WEBVIEW2_DATA_UNAVAILABLE
        );
    }

    #[test]
    fn directory_failure_converts_to_diagnostics_unavailable_preserving_fields() {
        let failure = layout::DirectoryFailure {
            target: std::path::PathBuf::from(r"C:\app\logs"),
            action: layout::DirectoryAction::Create,
            reason: "フォルダを作成できません".to_string(),
            os_error_code: Some(5),
            required_privilege: "管理者権限が必要です".to_string(),
        };

        let unavailable = diagnostics_unavailable_from_directory_failure(&failure);

        assert_eq!(unavailable.target, failure.target);
        assert_eq!(unavailable.reason, failure.reason);
        assert_eq!(unavailable.os_error_code, failure.os_error_code);
    }

    #[test]
    fn runtime_summary_for_evergreen_omits_folder() {
        let resolved = runtime::ResolvedRuntime {
            kind: runtime::RuntimeKind::Evergreen,
            version: "120.0.0.0".to_string(),
            browser_executable_folder: None,
            acl: None,
            preference: hakutaku_config::FixedRuntimePreference::Auto,
        };

        let summary = runtime_summary(&resolved);
        assert!(summary.contains("Evergreen"));
        assert!(summary.contains("120.0.0.0"));
        assert!(!summary.contains("フォルダ="));
    }

    #[test]
    fn runtime_summary_for_fixed_version_includes_absolute_folder() {
        let resolved = runtime::ResolvedRuntime {
            kind: runtime::RuntimeKind::FixedVersion,
            version: "150.0.4078.105".to_string(),
            browser_executable_folder: Some(std::path::PathBuf::from(r"C:\App\WebView2Runtime")),
            acl: Some(acl::AclOutcome::AlreadyAccessible),
            preference: hakutaku_config::FixedRuntimePreference::ForceFixedVersion,
        };

        let summary = runtime_summary(&resolved);
        assert!(summary.contains("FixedVersion"));
        assert!(summary.contains("150.0.4078.105"));
        assert!(summary.contains(r"C:\App\WebView2Runtime"));
    }
}
