pub mod bootstrap;
pub mod clipboard;
pub mod config_status;
pub mod file_dialog;
pub mod log_view;
pub mod measurement;
pub mod navigation;
pub mod targets;

use hakutaku_diagnostics::{diag_error, diag_info, diag_warn};

#[tauri::command]
fn core_responsibilities() -> [&'static str; 4] {
    hakutaku_core::responsibilities()
}

/// Tauri の起動処理（`tauri::Builder::run`）そのものが失敗した場合の終了コード。
///
/// `bootstrap::Aborted` が使う `2`〜`4`（実行ファイル位置不明・Runtime 使用不可・
/// WebView2 データフォルダ不可）と重複しないよう `1` を割り当てる。この経路は
/// `bootstrap::run()` が成功した**後**に Tauri 自体の初期化で失敗した場合だけに
/// 使うため、`bootstrap` モジュールの `Aborted` には含めていない。
const EXIT_CODE_TAURI_RUN_FAILURE: i32 = 1;

/// Hakutaku の起動処理。
///
/// 1. [`bootstrap::run`] を実行する。失敗した場合、通知は `bootstrap::run` の内部で
///    既に表示済みであり、ここでは **Tauri を一切初期化せず** `Aborted::exit_code`
///    で終了する（`TECH-005`、`DIST-009`、`DIST-014`）。
/// 2. 成功した場合は Tauri を組み立てる（[`tauri::Builder::build`]）。メインウィンドウは
///    `Tauri.toml`（設定ファイル）ではなく、ここ（Rust 側）で組み立てる。理由は
///    `Tauri.toml` のコメントのとおりで、Tauri 2.11.5 はウィンドウ設定に
///    `data_directory` の明示がないと WebView2 のユーザーデータフォルダを
///    `%LOCALAPPDATA%\<identifier>` へ強制するため（`SEC-009`、`DIST-013` に違反する）。
/// 3. 組み立てた `App` を [`tauri::App::run_return`] で実行し、アプリが正常終了した
///    場合（イベントループが終了して呼び出しが戻った場合）、`SEC-006`
///    「正常終了時に削除」に従って `temp` を再度清掃する。
/// 4. Tauri の起動自体が失敗した場合（`build` が `Err` を返した場合）も `panic!` /
///    `expect` せず、診断ログへ記録したうえで [`bootstrap::notify::show`] で
///    利用者へ理由を伝え、`std::process::exit` する。
///
/// # `Builder::run` ではなく `build` + `App::run_return` を使う理由
///
/// `tauri::Builder::run(context)` は内部で `App::run` を呼び、`App::run` はさらに
/// `tao::event_loop::EventLoop::run`（シグネチャが `-> !`、すなわち Windows 上では
/// メッセージループ終了時にそのままプロセスを終了し、**呼び出し元へ戻らない**）を
/// 使う。実機で確認済み: `Builder::run` を使うと、ウィンドウを閉じてプロセスが
/// 終了した後も、`.run(...)` の戻り値を待つ後続コード（本関数の手順3の清掃処理）が
/// 一切実行されない。
///
/// 一方 `App::run_return`（`tao::platform::run_return::EventLoopExtRunReturn` 経由）は
/// イベントループの終了後に実際に呼び出し元へ制御を戻し、終了コード（`i32`）を返す
/// （Tauri 自身の doc コメントに `std::process::exit(exit_code)` を呼ぶ使用例が
/// 示されている）。`SEC-006`（正常終了時に削除）を満たすには、清掃処理を実行できる
/// 地点が必要なため、こちらを使う。
///
/// # 既知の制約（Tauri 側の挙動、要調整として報告済み）
///
/// `App::run` / `App::run_return` は、`Builder::setup` に渡した関数
/// （このモジュールの `.setup(...)`）が `Err` を返した場合、**Tauri 側が
/// `panic!` する**（`tauri-2.11.5/src/app.rs` の `App::run_return` の doc コメント
/// に `# Panics` として明記されている、ライブラリ自身の既定動作）。この経路は
/// `src-tauri` 側のコードではなく Tauri 本体の内部実装であり、本フェーズの
/// 対象ファイル（`bootstrap/mod.rs`・`lib.rs`・`main.rs`）からは変更できない。
///
/// `#[cfg_attr(mobile, tauri::mobile_entry_point)]` は Tauri のデスクトップ向け
/// テンプレートが標準的に付与するものであり、`mobile` cfg は Windows 専用ビルド
/// （iOS・Android をターゲットにしない）では常に偽になるため実害はない。将来
/// 他プラットフォームを対象に加える可能性を残す意味でも、あえて外さずそのまま残す。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 手順1: ブートストラップを実行する。失敗時は通知済みのため、
    // Tauri を初期化せずそのまま終了コードで終わる。
    let bootstrap::Bootstrap {
        layout,
        diagnostics,
        runtime: _runtime,
        webview2_data_dir,
        elapsed: _elapsed,
        config: config_state,
    } = match bootstrap::run() {
        Ok(bootstrap) => bootstrap,
        Err(aborted) => std::process::exit(aborted.exit_code),
    };

    // 手順2: Tauri を組み立てる。メインウィンドウのラベル・タイトル・寸法は
    // Tauri.toml のコメントに掲げた値と一致させる。ウィンドウの実際の生成は
    // Builder::setup 経由（Tauri の仕様上、最初の Ready イベントまで遅延される）。
    // ナビゲーション制限（SEC-011）の記録に診断ログを使うため、クロージャーと
    // 本関数の後半で共有できるようにする。
    let diagnostics = std::sync::Arc::new(diagnostics);
    let navigation_diagnostics = std::sync::Arc::clone(&diagnostics);

    // P04-3: 計測モード（開発・検証専用。HAKUTAKU_MEASURE_FILE
    // 環境変数）の状態を確定する。環境変数が絶対パスを指していない限り無効の
    // ままであり、通常の利用者向け起動では以降の処理は何もしない
    // （measurement::MeasurementState::is_active）。有効な場合だけ、
    // PrivateUsage サンプラースレッド（500ms 間隔）を起動する。
    let measurement_state = std::sync::Arc::new(measurement::MeasurementState::from_env(
        layout.logs_dir().to_path_buf(),
    ));
    measurement::start_sampler_if_active(&measurement_state, &diagnostics);

    // メモリ会計イベント（予約拒否・ソフトしきい値到達）の通知先を配線する
    // （DIAG-005）。`crates/memory-accounting` は
    // `hakutaku-diagnostics` に依存しない設計（コア層を薄く保つ判断）のため、
    // 実際の診断ログ出力はここ（`src-tauri` 側）で行う。プロセス全体で共有する
    // グローバル予算（`global_budget()`）に対して起動時に一度だけ配線する
    // （`MemoryBudget::set_event_sink` は `OnceLock` のため、2回目以降の呼び出し
    // は無視される）。予約拒否・しきい値到達のどちらも、対処が必要ではあるが
    // 起動や継続を止めるものではないため `Warn` とする。エラーコード
    // （`docs/development/error-codes.md`）は、起動できない／利用者の対処が
    // 必要な失敗にだけ割り当てる基準のため、ここでは付与しない（`code=-`）。
    let memory_event_diagnostics = std::sync::Arc::clone(&diagnostics);
    hakutaku_memory_accounting::global_budget().set_event_sink(Box::new(
        move |event| match event {
            hakutaku_memory_accounting::AccountingEvent::ReservationRejected(rejected) => {
                diag_warn!(
                    memory_event_diagnostics,
                    module = "memory",
                    operation = "memory.reserve",
                    "{rejected}"
                );
            }
            hakutaku_memory_accounting::AccountingEvent::SoftThresholdReached {
                allocated_bytes,
                outstanding_reserved_bytes,
                budget_bytes,
                peak_bytes,
            } => {
                diag_warn!(
                    memory_event_diagnostics,
                    module = "memory",
                    operation = "memory.threshold",
                    "ソフトしきい値に到達しました（確保済み {allocated_bytes} バイト、\
                     予約済み {outstanding_reserved_bytes} バイト、予算 {budget_bytes} バイト、\
                     ピーク {peak_bytes} バイト）"
                );
            }
            hakutaku_memory_accounting::AccountingEvent::ReferenceIndicatorExceeded {
                total_private_usage_bytes,
                budget_bytes,
                limit_bytes,
            } => {
                // PERF-011 の参考指標（PrivateUsage 合計）が予算値 + 1 GiB を
                // 超えた警告。参考指標であり合否判定には使わない。進行中の
                // 追加読み込みのキャンセル接続は P06 以降で行う。
                diag_warn!(
                    memory_event_diagnostics,
                    module = "memory",
                    operation = "memory.reference_indicator",
                    "参考指標（PrivateUsage 合計）が上限を超えました（合計 \
                     {total_private_usage_bytes} バイト、予算 {budget_bytes} バイト、\
                     上限 {limit_bytes} バイト）"
                );
            }
        },
    ));

    // P06-2（`tasks/phase-06-large-file-loading.md` 作業項目10）:
    // ソフトしきい値到達時の解放処理の登録口（`register_release_handler`）へ
    // 「先読み停止 + 診断への記録」を登録する。実際の先読み抑制（未要求範囲の
    // 読み込みを発行しない判断）は `hakutaku_data_source::
    // read_snapshotted_bytes_chunked` が `MemoryBudget::prefetch_paused()` を
    // 直接読んで行う（作業項目9）。
    //
    // P08-3（作業項目3）: 本文バッファ（`hakutaku_core::
    // DisplaySetRegistry` が保持する `IndexedText`）の実解放も、ここから
    // 同じハンドラで「要求」します。ただし **このハンドラ自身は
    // `DisplaySetRegistryState` のロックを一切取りません**。
    // `MemoryBudget::register_release_handler` が呼ぶタイミングは
    // `MemoryBudget::reserve`／`ReservationToken::mark_allocated` の内部
    // （= 呼び出し側が `DisplaySetRegistryState` の `Mutex` を保持したまま
    // メモリ確保を試みている経路、`hakutaku_core::item::
    // build_items_from_pending` からの呼び出しを含む）であり得るため、ここで
    // ロックを取ると再入してデッドロックします。代わりに `EvictionFlag`
    // （`log_view::EvictionFlag`。`Arc<AtomicBool>` の薄いラッパー）を立てる
    // だけにし、実際の解放（`hakutaku_core::DisplaySetRegistry::
    // evict_inactive_sources`）は `log_view::fetch_log_range` の入口
    // （`log_view::drain_pending_eviction`。新しいロックをまだ取っていない
    // 安全な地点）で遅延して行います（設計判断の詳細は
    // `hakutaku_core::registry::DisplaySetRegistry::evict_inactive_sources`
    // の doc コメント「呼び出しタイミング」を参照）。
    let eviction_flag = log_view::EvictionFlag::default();
    let eviction_flag_for_handler = std::sync::Arc::clone(&eviction_flag.0);
    let prefetch_diagnostics = std::sync::Arc::clone(&diagnostics);
    hakutaku_memory_accounting::global_budget().register_release_handler(Box::new(move || {
        diag_warn!(
            prefetch_diagnostics,
            module = "memory",
            operation = "memory.prefetch_suppressed",
            "ソフトしきい値に到達したため、読み込み中の対象で先読み（未要求範囲の\
             読み込み）を停止し、非アクティブなソースのバッファ解放を要求しました\
             （PERF-014、P08-3）。実際の解放は次回の fetch_log_range 呼び出し時に\
             遅延して行われます。"
        );
        eviction_flag_for_handler.store(true, std::sync::atomic::Ordering::Relaxed);
    }));

    // P11-3（PERF-014／CFG-024）: 解析の同時実行数の上限・I/O 発行間隔の
    // 抑制を、全対象で共有する1つの IoThrottle インスタンスとして構築する
    // （`crates/data-source::chunk` の「PERF-014 の接続点」）。`targets::
    // run_open` がこれを managed state から取得し、`LoadControl::throttle`
    // へ渡す。`parse_concurrency` は `hakutaku_config` 側で 1 以上に検証済み
    // （`crates/config/src/load.rs` の `validate_u32(..., 1)`）のため、
    // `NonZeroUsize::new` は常に `Some` になるが、値の由来（設定検証）を
    // 過信しない防御として `.max(1)` で下限を保つ。
    let performance_config = config_state.config.performance;
    let parse_concurrency =
        std::num::NonZeroUsize::new((performance_config.parse_concurrency as usize).max(1));
    let io_throttle = hakutaku_data_source::IoThrottle::new(
        parse_concurrency,
        u64::from(performance_config.io_interval_ms),
    );

    let build_result = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            core_responsibilities,
            config_status::get_config_status,
            log_view::open_log_file,
            log_view::fetch_log_range,
            log_view::enable_merged_view,
            log_view::disable_merged_view,
            clipboard::copy_selection,
            measurement::get_measurement_mode,
            measurement::open_measurement_file,
            measurement::record_measurement_results,
            targets::list_targets,
            targets::list_log_profiles,
            targets::list_datetime_formats,
            targets::open_config_data_source,
            targets::close_target,
            targets::cancel_load,
            targets::retry_target,
            targets::reload_target,
            bootstrap::process::launch_elevated
        ])
        // P03-2: 起動時に読み込んだ設定状態を Tauri の managed state
        // として保持する。`get_config_status` コマンド（src-tauri/src/config_status.rs）
        // がこれを読み、フロントエンド向けの応答型へ変換して返す。
        .manage(config_state)
        // P04-1: 表示集合レジストリを managed state として保持する。
        // `log_view::open_log_file` / `log_view::fetch_log_range` がこれを使う。
        .manage(log_view::DisplaySetRegistryState::default())
        // P08-3: しきい値到達時の解放要求フラグ。上記の
        // `register_release_handler` が立て、`log_view::fetch_log_range` が
        // 入口で確認・消費する（`eviction_flag` 変数は上のクロージャへ
        // クローンだけ渡し、実体はこの managed state 側に残す）。
        .manage(eviction_flag)
        // P07-1: 参照対象一覧（アドホックに開いた対象と、設定由来の
        // データソースを開いたセッション）を managed state として保持する。
        // `targets` モジュールのコマンドと `log_view::open_log_file` がこれを使う。
        .manage(targets::TargetRegistryState::default())
        // P07-2／P06-5: 複数ソースの合計サイズ・
        // ファイル数の上限判定（PERF-004〜006）を、対象を開く・閉じる・再試行
        // する・明示的に再読み込みするたびに行う。P06 が実装した
        // `hakutaku_core::register_source_with_control`・`reload_source` へ
        // 結線するために必要な予算状態で、プロセス全体で単一の予算を共有する
        // （`hakutaku_core::SourceBudget` は内部で Mutex により排他制御する
        // ため、Send + Sync で managed state にそのまま乗せられる。追加の
        // ラッパー型は不要）。`targets::close_target`・`targets::retry_target`・
        // `targets::reload_target` がこれを使う。
        .manage(hakutaku_core::SourceBudget::new())
        // P11-3: 抑制の接続点（同時実行数の上限・I/O 発行間隔、
        // PERF-014／CFG-024）。`targets::run_open` がこれを使う。
        .manage(io_throttle)
        // P04-1: `log_view` コマンドが診断ログへ記録できるよう、
        // 起動時に構築した Diagnostics（Arc）を managed state としても保持する。
        .manage(std::sync::Arc::clone(&diagnostics))
        // P04-3: 計測モードの状態（有効・無効、PrivateUsage 時系列）を
        // managed state として保持する。`measurement` の3コマンドがこれを使う。
        .manage(std::sync::Arc::clone(&measurement_state))
        .setup(move |app| {
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("Hakutaku")
            // Issue #9: 800×600 未満では GUI のレイアウトが崩れる（1列に潰れて
            // 縦書きのように見える）ため、レイアウトが成立する下限として最小
            // サイズを 800×600 とする。初期サイズ 1024×768 はその上で余裕を
            // 持たせた値で、どちらも `ENV-005` の基準解像度 1920×1080 に収まる。
            // 値の正本は Tauri.toml の P01 実装契約コメントと同期させる。
            .inner_size(1024.0, 768.0)
            .min_inner_size(800.0, 600.0)
            .resizable(true)
            .data_directory(webview2_data_dir) // DIST-013 / SEC-009。必須。
            .devtools(false) // SEC-011。多重防御（CSP・feature 無効化に加えて明示指定）。
            // SEC-011: ローカルの同梱リソース以外へのナビゲーションを禁止する。
            // CSP には最上位ナビゲーションを止める手段がなく、Tauri の既定の
            // ハンドラは全許可のため、ここで登録しないと誰も止めない。
            // 詳細は crate::navigation の doc コメントを参照。
            .on_navigation(move |url| {
                if navigation::is_allowed(url) {
                    return true;
                }
                diag_warn!(
                    navigation_diagnostics,
                    module = "navigation",
                    operation = "webview.navigate",
                    "ローカルの同梱リソース以外へのナビゲーションを拒否しました（SEC-011）: {url}"
                );
                false
            })
            .build()?;
            Ok(())
        })
        .build(tauri::generate_context!());

    // 手順4: Tauri の組み立て自体が失敗した場合。panic! / expect せず、
    // 診断ログへ記録したうえでネイティブダイアログへ理由を伝えて終了する。
    let app = match build_result {
        Ok(app) => app,
        Err(error) => {
            diag_error!(
                diagnostics,
                module = "bootstrap",
                operation = "startup.tauri_build",
                "Tauri の起動処理でエラーが発生しました: {error}"
            );

            let notice = bootstrap::notify::Notice {
                kind: bootstrap::notify::NoticeKind::Error,
                title: "Hakutaku: 起動に失敗しました".to_string(),
                body: format!(
                    "Tauri の起動処理でエラーが発生したため、Hakutaku を起動できませんでした。\n\
                     \n\
                     理由:\n\
                     \u{20}\u{20}{error}\n\
                     \n\
                     Hakutaku を再度起動してください。改善しない場合は診断ログ（logs フォルダ）を\n\
                     確認してください。"
                ),
            };
            bootstrap::notify::show(&notice);

            std::process::exit(EXIT_CODE_TAURI_RUN_FAILURE);
        }
    };

    // 手順3（前半）: イベントループを実行する。run_return はイベントループが
    // 終了すると（アプリが正常終了すると）呼び出し元へ戻る（上記 doc コメント参照）。
    // 終了イベント（RunEvent::Exit）で、ヒープ確保のピーク値（PERF-008）を
    // 診断ログへ記録する（DIAG-005）。起動できない・利用者の対処が必要な失敗
    // ではないため、エラーコードは付与しない（`docs/development/error-codes.md`
    // の割り当て基準に該当しない）。
    let exit_diagnostics = std::sync::Arc::clone(&diagnostics);
    let exit_code = app.run_return(move |_app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            diag_info!(
                exit_diagnostics,
                module = "memory",
                operation = "memory.peak",
                "終了時点のヒープ確保ピーク値: {} バイト",
                hakutaku_memory_accounting::peak_bytes()
            );
        }
    });

    // 手順3（後半）: アプリが正常終了したので、SEC-006「正常終了時に削除」に従って
    // temp を再度清掃し、結果を診断ログへ記録する。
    let report = layout.purge_temp();
    diag_info!(
        diagnostics,
        module = "bootstrap",
        operation = "shutdown.temp_purge",
        "正常終了時の temp 清掃が完了しました: 削除 {} 件、失敗 {} 件（対象: {}）",
        report.removed_entries,
        report.failures.len(),
        layout.temp_dir().display()
    );
    for failure in &report.failures {
        diag_warn!(
            diagnostics,
            module = "bootstrap",
            operation = "shutdown.temp_purge",
            "temp 配下のエントリを削除できませんでした: {}（{}）",
            failure.target.display(),
            failure.reason
        );
    }

    // Tauri 自身の doc コメントが推奨する通り、run_return が返した終了コードで終える。
    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::core_responsibilities;

    #[test]
    fn tauri_command_delegates_to_the_core_layer() {
        assert_eq!(
            core_responsibilities(),
            ["データソース", "形式判定", "パーサー", "共通サービス"]
        );
    }
}
