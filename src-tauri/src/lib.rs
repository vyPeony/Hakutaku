pub mod bootstrap;
pub mod clipboard;
pub mod config_status;
pub mod file_dialog;
pub mod log_view;
pub mod measurement;
pub mod navigation;
pub mod targets;

use hakutaku_diagnostics::{diag_error, diag_info, diag_warn};

/// Tauri の起動処理（`tauri::Builder::run`）そのものが失敗した場合の終了コード。
///
/// `bootstrap::Aborted` が使う `2`〜`4`（実行ファイル位置不明・Runtime 使用不可・
/// WebView2 データフォルダ不可）と重複しないよう `1` を割り当てる。この経路は
/// `bootstrap::run()` が成功した**後**に Tauri 自体の初期化で失敗した場合だけに
/// 使うため、`bootstrap` モジュールの `Aborted` には含めていない。
const EXIT_CODE_TAURI_RUN_FAILURE: i32 = 1;

/// メインウィンドウ（WebView）の生成そのものが失敗した場合の終了コード
/// （Issue #51）。
///
/// [`EXIT_CODE_TAURI_RUN_FAILURE`] の `1` と `bootstrap::Aborted` が使う
/// `2`〜`5` のいずれとも重複しない値として `6` を割り当てる。`build`（Tauri の
/// 組み立て）の失敗とは別のコードにしているのは、利用者・導入担当が診断ログを
/// 見る前に「Tauri は組み立てられたが WebView を出せなかった」ことを終了コード
/// だけで切り分けられるようにするためで、この2つは原因（設定・生成物の破損 vs
/// WebView2 の実行時状態）も次の対処も異なる。
const EXIT_CODE_WINDOW_CREATION_FAILURE: i32 = 6;

/// 初期ウィンドウの既定の幅（論理ピクセル）。値の正本は `Tauri.toml` の
/// P01 実装契約コメント（width = 1024）と同期させる。
const INITIAL_WINDOW_WIDTH: f64 = 1024.0;

/// 初期ウィンドウの既定の高さ（論理ピクセル）。値の正本は `Tauri.toml` の
/// P01 実装契約コメント（height = 768）と同期させる。
const INITIAL_WINDOW_HEIGHT: f64 = 768.0;

/// ウィンドウの最小の幅（論理ピクセル）。800×600 未満では GUI のレイアウトが
/// 崩れる（1列に潰れて縦書きのように見える）ため、レイアウトが成立する下限
/// （Issue #9）。値の正本は `Tauri.toml` の P01 実装契約コメント（minWidth = 800）。
const MIN_WINDOW_WIDTH: f64 = 800.0;

/// ウィンドウの最小の高さ（論理ピクセル）。[`MIN_WINDOW_WIDTH`] と対。値の正本は
/// `Tauri.toml` の P01 実装契約コメント（minHeight = 600）。
const MIN_WINDOW_HEIGHT: f64 = 600.0;

/// 初期サイズの丸め計算で、モニタの論理幅から確保する余白（論理ピクセル）。
/// ウィンドウ装飾（左右の枠）相当。
const WINDOW_MARGIN_WIDTH: f64 = 16.0;

/// 初期サイズの丸め計算で、モニタの論理高さから確保する余白（論理ピクセル）。
/// タイトルバーとタスクバー相当。
const WINDOW_MARGIN_HEIGHT: f64 = 88.0;

/// 初期ウィンドウサイズ（論理ピクセル）を、モニタの論理サイズに収まるよう丸めます。
///
/// # なぜ丸めが必要か
///
/// Tauri の `WebviewWindowBuilder::inner_size` は論理ピクセルを受け取り
/// （tauri-2.11.5/src/webview/webview_window.rs 804行目「Window size in logical
/// pixels.」、tauri-runtime-wry-2.11.4/src/lib.rs 1008行目で `TaoLogicalSize` へ
/// 変換）、tao 0.35.3 はウィンドウ生成時にモニタサイズへのクランプを行わない
/// （min/max 制約の適用のみ）。そのため `ENV-005` の基準解像度 1920×1080 でも、
/// 表示スケール 150% ではモニタの論理サイズが 1280×720 になり、既定の
/// 1024×768 のままでは初期ウィンドウが画面からはみ出す（Issue #9）。
///
/// # 丸めの仕様
///
/// - モニタの論理サイズから、ウィンドウ装飾とタスクバー相当の余白
///   （幅 [`WINDOW_MARGIN_WIDTH`]、高さ [`WINDOW_MARGIN_HEIGHT`]）を引いた
///   利用可能サイズまで縮める。利用可能サイズが既定
///   （[`INITIAL_WINDOW_WIDTH`]×[`INITIAL_WINDOW_HEIGHT`]）以上なら丸めない。
/// - ただし最小サイズ（[`MIN_WINDOW_WIDTH`]×[`MIN_WINDOW_HEIGHT`]）は下回らない。
///   最小サイズを優先するため、約 175% 以上の表示スケールでは画面に収まらない
///   場合がある（レイアウトが成立する下限を優先する既知の制約）。
///
/// 例: モニタ論理 1280×720 → (1024.0, 632.0)、1920×1080 → (1024.0, 768.0)、
/// 1097×617 → (1024.0, 600.0)。
fn clamped_initial_window_size(
    monitor_logical_width: f64,
    monitor_logical_height: f64,
) -> (f64, f64) {
    let available_width = monitor_logical_width - WINDOW_MARGIN_WIDTH;
    let available_height = monitor_logical_height - WINDOW_MARGIN_HEIGHT;
    let width = INITIAL_WINDOW_WIDTH
        .min(available_width)
        .max(MIN_WINDOW_WIDTH);
    let height = INITIAL_WINDOW_HEIGHT
        .min(available_height)
        .max(MIN_WINDOW_HEIGHT);
    (width, height)
}

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
///    利用者へ理由を伝え、`std::process::exit` する。メインウィンドウの生成
///    （`.setup(...)` の中の `WebviewWindowBuilder::build`）が失敗した場合も同じ
///    形で終える（`EXIT_CODE_WINDOW_CREATION_FAILURE`、Issue #51）。起動を
///    中止するすべての経路が、理由を利用者へ伝えてから終わるようにするため。
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
/// # `setup` から `Err` を返さない理由（Tauri 側の挙動）
///
/// `App::run` / `App::run_return` は、`Builder::setup` に渡した関数
/// （このモジュールの `.setup(...)`）が `Err` を返した場合、**Tauri 側が
/// `panic!` する**（`tauri-2.11.5/src/app.rs` の `App::run_return` の doc コメント
/// に `# Panics` として明記されている、ライブラリ自身の既定動作）。この挙動は
/// Tauri 本体の内部実装であり、`src-tauri` 側からは変更できない。
///
/// そこで `.setup(...)` の中で失敗し得る唯一の処理（メインウィンドウの生成）を
/// クロージャーの中で処理し切り、`Err` を返さないようにしている（Issue #51）。
/// 以前はここが `?` でそのまま伝播し、**起動を中止する経路の中で唯一、利用者へ
/// 何も伝えないまま panic する**という非一貫な扱いになっていた。初期サイズの
/// 丸め（`clamped_initial_window_size`）はモニタ情報が取れないときに黙って
/// 既定サイズへ倒す設計であり、そもそも失敗を返さない。
///
/// この形を保つため、`.setup(...)` へ新しい処理を足すときは、失敗を `?` で
/// 返さず、その場で「診断ログ → ネイティブ通知 → `std::process::exit`」または
/// 「無視して続行」のどちらかへ倒してください。
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
    // ウィンドウ生成の失敗を `setup` の中で通知・記録するために使う
    // （Issue #51。`setup` から `Err` を返すと Tauri 側が panic するため、
    // 失敗の扱いをクロージャーの中で完結させる必要がある）。
    let setup_diagnostics = std::sync::Arc::clone(&diagnostics);

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
    // 起動や継続を止めるものではないため `Warn` とする。
    //
    // エラーコード（`docs/development/error-codes.md`）は「起動できない、または
    // 処理を継続できない失敗」「利用者・導入組織側の対処が必要な失敗」にだけ
    // 割り当てる。予約拒否は、要求された読み込み・索引作成をその場では継続でき
    // ず、対象を閉じる・`memory.budget_mib` を見直すという利用者側の対処を要する
    // ためこの基準に該当し、`HKT-MEM-0001` を付ける。しきい値到達と参考指標の
    // 超過は、先読み停止・バッファ解放でアプリ側が自動的に縮退し、利用者の対処を
    // 要さないため付けない（`code=-`）。
    let memory_event_diagnostics = std::sync::Arc::clone(&diagnostics);
    hakutaku_memory_accounting::global_budget().set_event_sink(Box::new(
        move |event| match event {
            hakutaku_memory_accounting::AccountingEvent::ReservationRejected(rejected) => {
                diag_warn!(
                    memory_event_diagnostics,
                    module = "memory",
                    operation = "memory.reserve",
                    error_code = hakutaku_memory_accounting::error_codes::RESERVATION_REJECTED,
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
    // evict_inactive_sources`）は Tauri コマンドの入口（新しいロックをまだ
    // 取っていない安全な地点）で遅延して行います。消費点は
    // `log_view::fetch_log_range`（スクロール契機）と
    // `targets::list_targets`（読み込み中の 500ms ポーリング契機）の2つです
    // （Issue #51。`log_view::EvictionFlag` の doc コメント参照）。設計判断の
    // 詳細は `hakutaku_core::registry::DisplaySetRegistry::
    // evict_inactive_sources` の doc コメント「呼び出しタイミング」を参照。
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
             （PERF-014、P08-3）。実際の解放は次回の fetch_log_range または\
             list_targets 呼び出し時に遅延して行われます。"
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
        // SEC-012: 登録するコマンドは、フロントエンドが実際に呼ぶものだけに
        // 限る。ここへ登録したコマンドは `src-tauri/permissions/<コマンド名>.toml`
        // の許可定義と `src-tauri/capabilities/default.toml` の列挙を必ず伴う
        // （対応関係は `node scripts/check-capabilities.mjs` が検査する）。
        .invoke_handler(tauri::generate_handler![
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
        // `register_release_handler` が立て、`log_view::fetch_log_range` と
        // `targets::list_targets` が入口で確認・消費する（`eviction_flag`
        // 変数は上のクロージャへクローンだけ渡し、実体はこの managed state
        // 側に残す）。
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
            let window = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("Hakutaku")
            // Issue #9: 800×600 未満では GUI のレイアウトが崩れる（1列に潰れて
            // 縦書きのように見える）ため、レイアウトが成立する下限として最小
            // サイズを 800×600 とする。初期サイズ 1024×768 はその上で余裕を
            // 持たせた値。どちらも論理ピクセルであり、初期サイズは build 後に
            // モニタの論理サイズへ丸める（`clamped_initial_window_size`。表示
            // スケール 150% の 1920×1080 ではモニタの論理サイズが 1280×720 に
            // なるため）。最小サイズを下回る丸めはしないため、約 175% 以上の
            // 表示スケールでは画面に収まらない場合がある（既知の制約）。
            // 値の正本は Tauri.toml の P01 実装契約コメントと同期させる。
            .inner_size(INITIAL_WINDOW_WIDTH, INITIAL_WINDOW_HEIGHT)
            .min_inner_size(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT)
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
            .build();
            // ウィンドウを出せなければ、GUI アプリとして続行する意味が無い。
            // ここで `?` により `Err` を返すと Tauri 側が panic し（`run` の
            // doc コメント参照）、利用者には何の説明も残らないまま落ちるため、
            // 他の起動失敗（`bootstrap::run` の各手順・後述の `build` 失敗）と
            // 同じく「診断ログへ記録 → ネイティブ通知 → 終了コード」で終える
            // （Issue #51）。
            let window = match window {
                Ok(window) => window,
                Err(error) => {
                    diag_error!(
                        setup_diagnostics,
                        module = "bootstrap",
                        operation = "startup.window_create",
                        "メインウィンドウを生成できませんでした: {error}"
                    );

                    let notice = bootstrap::notify::Notice {
                        kind: bootstrap::notify::NoticeKind::Error,
                        title: "Hakutaku: 起動に失敗しました".to_string(),
                        body: format!(
                            "メインウィンドウを生成できなかったため、Hakutaku を起動できませんでした。\n\
                             \n\
                             理由:\n\
                             \u{20}\u{20}{error}\n\
                             \n\
                             WebView2 Runtime の状態が変わっていないかを確認し、Hakutaku を再度\n\
                             起動してください。改善しない場合は診断ログ（logs フォルダ）を\n\
                             確認してください。"
                        ),
                    };
                    bootstrap::notify::show(&notice);

                    std::process::exit(EXIT_CODE_WINDOW_CREATION_FAILURE);
                }
            };
            // モニタ情報が取れた場合だけ、初期サイズをモニタの論理サイズへ
            // 丸める（Issue #9）。取得失敗（Err / None）や set_size の失敗は
            // 無視して既定サイズのまま起動する（丸めは表示位置の改善であり、
            // 起動を止める理由にはしないため）。
            if let Ok(Some(monitor)) = window.current_monitor() {
                let logical = monitor.size().to_logical::<f64>(monitor.scale_factor());
                let (width, height) = clamped_initial_window_size(logical.width, logical.height);
                if (width, height) != (INITIAL_WINDOW_WIDTH, INITIAL_WINDOW_HEIGHT) {
                    let _ = window.set_size(tauri::LogicalSize::new(width, height));
                }
            }
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
    use super::{
        clamped_initial_window_size, EXIT_CODE_TAURI_RUN_FAILURE, EXIT_CODE_WINDOW_CREATION_FAILURE,
    };

    /// 受け入れ条件（Issue #51）: 起動を中止する経路の終了コードは、`bootstrap`
    /// 側（`Aborted::exit_code`）とこのモジュール側を合わせてすべて異なる。
    /// 重複すると、利用者・導入担当が終了コードから中止理由を切り分けられなく
    /// なる（`bootstrap::tests::exit_codes_are_pairwise_distinct` の検査を、
    /// `bootstrap` の外で定義しているコードまで広げたもの）。
    #[test]
    fn startup_exit_codes_are_pairwise_distinct_across_modules() {
        let codes = [
            EXIT_CODE_TAURI_RUN_FAILURE,
            EXIT_CODE_WINDOW_CREATION_FAILURE,
            crate::bootstrap::EXIT_CODE_LAYOUT_UNAVAILABLE,
            crate::bootstrap::EXIT_CODE_RUNTIME_UNAVAILABLE,
            crate::bootstrap::EXIT_CODE_WEBVIEW2_DATA_UNAVAILABLE,
            crate::bootstrap::EXIT_CODE_RUNTIME_FOLDER_IS_LINK,
        ];

        for (index, code) in codes.iter().enumerate() {
            for other in codes.iter().skip(index + 1) {
                assert_ne!(code, other, "終了コードが重複しています: {codes:?}");
            }
        }
    }

    /// 表示スケール 150% の 1920×1080（モニタ論理サイズ 1280×720）では、高さを
    /// 利用可能サイズ（720 − 88 = 632）へ丸める（Issue #9 の再現条件）。
    #[test]
    fn shrinks_height_to_fit_monitor_at_150_percent_scale() {
        assert_eq!(clamped_initial_window_size(1280.0, 720.0), (1024.0, 632.0));
    }

    /// 表示スケール 100% の 1920×1080 では余白を引いても既定サイズ以上のため、
    /// 既定の 1024×768 のまま丸めない。
    #[test]
    fn keeps_default_size_when_monitor_is_large_enough() {
        assert_eq!(clamped_initial_window_size(1920.0, 1080.0), (1024.0, 768.0));
    }

    /// 利用可能サイズが最小サイズを下回る場合（1097×617 → 高さ 617 − 88 = 529）
    /// は、最小サイズ 800×600 を優先して 600 で止める（画面に収まらないことを
    /// 許容する既知の制約）。
    #[test]
    fn never_shrinks_below_minimum_size() {
        assert_eq!(clamped_initial_window_size(1097.0, 617.0), (1024.0, 600.0));
    }

    /// 境界: 余白を引いた利用可能サイズがちょうど既定サイズ
    /// （1040 − 16 = 1024、856 − 88 = 768）なら丸めない。
    #[test]
    fn keeps_default_size_at_exact_boundary() {
        assert_eq!(clamped_initial_window_size(1040.0, 856.0), (1024.0, 768.0));
    }
}
