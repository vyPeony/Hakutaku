//! `open_log_file` / `fetch_log_range` コマンド（P04-1）。
//!
//! GUI 層はネイティブファイル選択ダイアログの実行（`crate::file_dialog`）と、
//! `hakutaku_core` の応答を Tauri コマンドの応答形（`serde::Serialize`）へ
//! 変換するところまでを担当します。解析ロジック（日時解析、行分割、範囲取得の
//! 判定）は一切持たず、すべて `hakutaku_core`・`hakutaku_parser`・
//! `hakutaku_data_source` に委譲します（計画書「作業項目8: 層境界の確認」）。
//!
//! `SEC-012`（フロントエンドへ任意パスのファイルシステムアクセス権を与えない）
//! に従い、フロントエンドへは絶対パスを一切渡しません。表示用の来歴ラベルは
//! ファイル名だけです。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use windows::Win32::Foundation::HWND;

use hakutaku_diagnostics::{diag_info, diag_warn, Diagnostics};

use crate::bootstrap::config::ConfigState;
use crate::file_dialog::{self, FileSelection};
use crate::targets::{self, TargetRegistryState, UserFacingErrorDto};

/// 表示集合レジストリの managed state です。
///
/// `hakutaku_core::DisplaySetRegistry` 自体は GUI 非依存であり、`Mutex` で
/// 包んで Tauri の managed state にするのはこのモジュールの役割です。
pub struct DisplaySetRegistryState(pub Mutex<hakutaku_core::DisplaySetRegistry>);

impl Default for DisplaySetRegistryState {
    fn default() -> Self {
        DisplaySetRegistryState(Mutex::new(hakutaku_core::DisplaySetRegistry::new()))
    }
}

/// 読み込み中に [`DisplaySetRegistryState`] の `Mutex` を、**バッチ確定の
/// たびに取り直す**ための接続点です（コア層の
/// `hakutaku_core::RegistryAccess` 実装）。
///
/// # なぜこれが必要か
///
/// `hakutaku_core::register_source_with_control` は `&mut DisplaySetRegistry`
/// を受け取るため、読み込みの間ずっとロックを保持することになります。GB 級の
/// ファイルでは数秒〜数十秒に達し、その間 [`fetch_log_range`] は同じ `Mutex`
/// を取れず、UI からの範囲取得が完全に止まります（`ENV-004`・`PERF-009`）。
/// 読み込みワーカー（`crate::targets::run_open_core`）はこの型を
/// 渡して `hakutaku_core::register_source_with_access` を呼び、コアが確定した
/// バッチを登録する瞬間だけロックを取ります。
///
/// # デッドロック回避規則との整合
///
/// ロックの取得回数は増えますが、`crate::lib` が
/// `register_release_handler` へ登録するソフトしきい値ハンドラは
/// [`EvictionFlag`] を立てるだけで、ここで取るロックには触れません
/// （`crate::lib` の該当箇所と `hakutaku_core::register_source_with_access`
/// の doc コメント「デッドロック回避規則との整合」を参照）。したがって借用の
/// 内側でメモリ予約が走っても再入は起こりません。
///
/// 毒された `Mutex`（別スレッドの panic）は、このモジュールの他の経路と同じく
/// `PoisonError::into_inner` で内容を引き継ぎます。読み込み途中の表示集合は
/// 「その時点までの確定済みの内容」であり、部分的に見えても壊れていないため
/// です。
pub(crate) struct PerBatchRegistryLock<'a>(&'a Mutex<hakutaku_core::DisplaySetRegistry>);

impl<'a> PerBatchRegistryLock<'a> {
    pub(crate) fn new(registry: &'a Mutex<hakutaku_core::DisplaySetRegistry>) -> Self {
        PerBatchRegistryLock(registry)
    }
}

impl hakutaku_core::RegistryAccess for PerBatchRegistryLock<'_> {
    fn with_registry<R>(
        &mut self,
        borrow: impl FnOnce(&mut hakutaku_core::DisplaySetRegistry) -> R,
    ) -> R {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        borrow(&mut guard)
    }
}

/// P08-3: しきい値到達時の「解放要求フラグ」です。
///
/// `hakutaku_memory_accounting::MemoryBudget::register_release_handler` へ
/// 登録するハンドラは、このフラグを立てるだけで [`DisplaySetRegistryState`]
/// の `Mutex` には一切触れません（`crate::lib` の配線を参照）。実際の解放
/// （`hakutaku_core::DisplaySetRegistry::evict_inactive_sources`）は、
/// Tauri コマンド処理の入口（[`fetch_log_range`]）で、まだ他のロックを
/// 保持していない安全な地点からこのフラグを確認して行います（遅延方式。
/// デッドロック回避の設計判断は `hakutaku_core::registry` の
/// `evict_inactive_sources` doc コメント「呼び出しタイミング」を参照）。
#[derive(Clone, Default)]
pub struct EvictionFlag(pub Arc<AtomicBool>);

/// [`EvictionFlag`] が立っていれば、非アクティブなソースのデコード済み
/// バッファを解放します（P08-3）。`registry` の `Mutex` は呼び出し側が既に
/// ロック済みの前提です（二重ロックを避けるため、ここでは新たにロックしま
/// せん）。
pub(crate) fn drain_pending_eviction(
    eviction: &EvictionFlag,
    registry_guard: &mut hakutaku_core::DisplaySetRegistry,
    diagnostics: &Diagnostics,
) {
    if !eviction.0.swap(false, Ordering::AcqRel) {
        return;
    }
    let evicted = registry_guard.evict_inactive_sources();
    if !evicted.is_empty() {
        diag_info!(
            diagnostics,
            module = "memory",
            operation = "memory.evict_inactive",
            "ソフトしきい値到達により、非アクティブなソースのデコード済み\
             バッファを解放しました（P08-3）: source_id={evicted:?}"
        );
    }
}

/// `open_log_file` / `open_measurement_file` の応答です。
///
/// キャンセルは失敗ではなく正常応答の一種として表現します（呼び出し側の
/// doc コメントのとおり、`Result::Err` にはしません）。
///
/// `target_id` は P07-1 で追加した、対象一覧（`crate::targets`）
/// 上のエントリの識別子です。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpenLogFileResponse {
    /// ファイルを選択し、読み込みに成功した（**同期応答**）。
    ///
    /// `open_log_file`（利用者向け。P07-2 で非同期化）はこの variant を
    /// 返しません（`Loading` を返します）。計測モード
    /// （`crate::measurement::open_measurement_file`）だけが、この型を流用
    /// しつつ対象一覧を経由しない同期経路として返します（`target_id: None`。
    /// 計測は所要時間を確定的に測る必要があるため、意図的に非同期化して
    /// いません）。
    Opened {
        target_id: Option<u32>,
        source_id: u32,
        display_set_id: u32,
        generation: u64,
        total_items: u64,
        /// 表示用の来歴ラベル（ファイル名。絶対パスではない。`SEC-012`）。
        source_label: String,
        /// `LOG-022`: 日時書式またはログ解析プロファイルを一意に決定できず、
        /// 日時未解析の生表示へ退避したか。フロントエンドは非致命的な通知
        /// として表示します。
        fell_back_to_raw_display: bool,
    },
    /// ファイルの選択に成功し、対象一覧へ「読み込み中」で登録した
    /// （P07-2）。`open_log_file` はこの variant を返します。実際の成否は
    /// フロントエンド（`src/shell.js`）が `list_targets` のポーリングで
    /// 検出します。
    Loading {
        target_id: u32,
        source_label: String,
    },
    /// 利用者がダイアログをキャンセルした（正常応答）。
    Cancelled,
    /// ダイアログの実行、またはファイルの読み込みに失敗した。
    ///
    /// `target_id` は、ファイル選択自体（ダイアログ操作）が失敗した場合など
    /// 対象を登録する前に失敗した場合は `None` です。`error` は `ERR-002` の
    /// 5要素を持つ DTO（`crate::targets::UserFacingErrorDto`）で、フルパスを
    /// マスキングしません。
    Failed {
        target_id: Option<u32>,
        error: UserFacingErrorDto,
    },
}

/// ネイティブのファイル選択ダイアログを表示し、選択されたログファイルを対象一覧へ
/// 「読み込み中」として登録します。実際の読み込みは
/// `hakutaku_core::register_source_with_control` をバックグラウンドスレッドで
/// 実行します（P07-2、`crate::targets` のモジュール doc コメント「非同期化の
/// 設計」を参照）。
///
/// フロントエンドへ生パスの操作権を与えません（`SEC-012`）。応答に含まれる
/// `source_label` は表示用のファイル名だけです。
///
/// `manual_profile`（`LOG-022`、P07-2）を指定すると、そのプロファイル名で
/// 開きます。通常は `None`（自動解決、`LOG-021` の4段階）です。
/// `manual_datetime_format`（`LOG-022`）を指定すると、その日時書式
/// で解析します。通常は `None`（自動判定）です。
///
/// `#[tauri::command(async)]` を付けているのは、**同期関数のまま** Tauri の
/// ブロッキングスレッドプールで実行させるためです（Issue #44）。同期コマンドは
/// イベントループスレッド上でインライン実行され、ダイアログが閉じるまでメイン
/// ウィンドウのメッセージループを止めてしまいます。`async fn` にはしません
/// （`crate::targets` のモジュール doc コメント「非同期化の設計」の理由がその
/// まま当てはまります）。詳細は `crate::file_dialog` のモジュール doc コメント
/// 「呼び出し元スレッドと親ウィンドウ」を参照してください。
#[tauri::command(async)]
pub fn open_log_file(
    manual_profile: Option<String>,
    manual_datetime_format: Option<String>,
    app: AppHandle,
    targets: State<'_, TargetRegistryState>,
    diagnostics: State<'_, Arc<Diagnostics>>,
) -> OpenLogFileResponse {
    // 親ウィンドウの HWND は、ダイアログを開く**前に**このスレッドで取得する。
    // ダイアログ用の専用スレッドから取得しようとするとイベントループの応答待ちに
    // なる（Issue #44。`crate::file_dialog` のモジュール doc コメントを参照）。
    let owner = main_window_hwnd(&app, diagnostics.inner());

    let selection = match file_dialog::choose_log_file(owner) {
        Ok(selection) => selection,
        Err(error) => {
            diag_warn!(
                diagnostics,
                module = "log_view",
                operation = "log.open_dialog",
                "ファイル選択ダイアログの実行に失敗しました: {error}"
            );
            // ダイアログ自体の失敗は、まだ「対象」を識別できていない
            // （ファイルが選ばれていない）段階のため、対象一覧へは登録しない
            // （target_id は None）。
            let user_error = hakutaku_core::notification::UserFacingError::new(
                "ファイル選択ダイアログ",
                error.to_string(),
                "もう一度お試しください。",
            );
            return OpenLogFileResponse::Failed {
                target_id: None,
                error: UserFacingErrorDto::from(&user_error),
            };
        }
    };

    let path = match selection {
        FileSelection::Cancelled => {
            diag_info!(
                diagnostics,
                module = "log_view",
                operation = "log.open_dialog",
                "ファイル選択がキャンセルされました"
            );
            return OpenLogFileResponse::Cancelled;
        }
        FileSelection::Selected(path) => path,
    };

    // SEC-012: フロントエンドへ生パスを渡さない。表示用ラベルとしてファイル名
    // だけを使う（来歴。LOG-007 の下地）。
    let source_label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(不明なファイル名)".to_string());

    // P07-1: 対象一覧（crate::targets）へアドホック対象として
    // 登録する。P07-2: begin_loading でキャンセル可能な状態にした
    // 直後、読み込み本体をバックグラウンドスレッドへ委譲する。
    let target_id = {
        let mut target_guard = targets
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let target_id = target_guard.register(
            source_label.clone(),
            targets::TargetOrigin::AdHoc { path: path.clone() },
        );
        // 直前に登録したばかりの target_id のため、既存の読み込みトークンは
        // あり得ない（`begin_loading` が `None` を返すのは「既に読み込み中」の
        // 場合だけ。Issue #31）。`debug_assert!` は式を消さないので、release
        // ビルドでも begin_loading は実行される。
        let token = target_guard.begin_loading(target_id);
        debug_assert!(
            token.is_some(),
            "新規登録した対象に既存の読み込みトークンがあってはならない"
        );
        target_id
    };

    targets::spawn_open(
        app,
        targets::OpenRequest {
            target_id,
            path: path.clone(),
            source_label: source_label.clone(),
            manual_profile,
            manual_datetime_format,
            module: "log_view",
            operation: "log.open",
            error_target: path.display().to_string(),
            error_next_action: "再試行するか、別のファイルを選び直してください。",
        },
    );

    OpenLogFileResponse::Loading {
        target_id,
        source_label,
    }
}

/// ファイル選択ダイアログの親にするメインウィンドウの HWND を返します
/// （Issue #44）。
///
/// ラベル `"main"` は `crate::lib` の `WebviewWindowBuilder::new` へ渡している
/// 値です（変えるならここも合わせて直します）。
///
/// 取得できない場合はコマンドを失敗させず `None` を返します。親が無いと
/// ダイアログは前面・モーダルになりませんが、それでもファイルは選べます。
/// 「開けない」より「所有者なしでも開ける」方が利用者にとって有益である、
/// という裁定です（`LOG-020`）。取得できなかったこと自体は診断ログへ残し、
/// 前面に出ないという申告があったときに切り分けられるようにします。
fn main_window_hwnd(app: &AppHandle, diagnostics: &Diagnostics) -> Option<HWND> {
    let Some(window) = app.get_webview_window("main") else {
        diag_warn!(
            diagnostics,
            module = "log_view",
            operation = "log.open_dialog",
            "メインウィンドウ（ラベル main）が見つからないため、ファイル選択\
             ダイアログを所有者なしで表示します"
        );
        return None;
    };

    match window.hwnd() {
        Ok(hwnd) => Some(hwnd),
        Err(error) => {
            diag_warn!(
                diagnostics,
                module = "log_view",
                operation = "log.open_dialog",
                "メインウィンドウの HWND を取得できないため、ファイル選択\
                 ダイアログを所有者なしで表示します: {error}"
            );
            None
        }
    }
}

/// [`hakutaku_core::LoadSummary`] の P05 で追加した診断情報（判定経路・
/// プロファイル解決経路・確定した日時書式とその決定経路・生表示退避・警告）を
/// 診断ログへ記録します。
///
/// `open_log_file`（このモジュール）と `measurement::open_measurement_file_core`
/// が共有します（`ENC-005`・`LOG-022` の受け入れ条件「判定経路を診断情報で
/// 確認できる」）。
pub(crate) fn log_load_summary(
    diagnostics: &Diagnostics,
    module: &'static str,
    operation: &'static str,
    source_label: &str,
    summary: &hakutaku_core::LoadSummary,
) {
    diag_info!(
        diagnostics,
        module = module,
        operation = operation,
        "文字コード判定: ファイル={}, 選択={}, 判定経路={}, プロファイル解決経路={}, \
         日時書式={}, 日時書式の決定経路={}",
        source_label,
        summary.selected_encoding,
        summary.encoding_route,
        summary.profile_resolution_route,
        summary.detected_datetime_format.unwrap_or("未確定"),
        // 書式の値だけでは、明示指定の誤り（日時行がすべて継続行へ結合される）と
        // 自動判定の失敗を切り分けられないため、経路も併記する。
        summary.datetime_format_route.route_label(),
    );

    if summary.fell_back_to_raw_display {
        diag_warn!(
            diagnostics,
            module = module,
            operation = operation,
            "日時未解析の生表示へ退避しました（LOG-022）: ファイル={}",
            source_label
        );
    }

    if !summary.decode_invalid_positions.is_empty() {
        diag_warn!(
            diagnostics,
            module = module,
            operation = operation,
            "デコードできないバイト列を検出しました: ファイル={}, 選択文字コード={}, \
             件数={}, 打ち切り={}, 位置={:?}",
            source_label,
            summary.selected_encoding,
            summary.decode_invalid_positions.len(),
            summary.decode_invalid_positions_truncated,
            summary.decode_invalid_positions
        );
    }

    for warning in &summary.encoding_warnings {
        diag_warn!(
            diagnostics,
            module = module,
            operation = operation,
            "文字コード判定の警告: ファイル={}, {}",
            source_label,
            warning
        );
    }
}

/// 範囲取得応答の1項目（`hakutaku_core::ItemDto` の serde 化）。
///
/// `confirmed`・`continuation_count`・`raw_display` は P08-1 で
/// 追加した表示メタデータです。既存フィールドはいずれも意味を変えていません
/// （`src/` の JS は未知フィールドを無視するため、この追加だけでは既存の
/// フロントエンドは壊れません）。
#[derive(Debug, Clone, Serialize)]
pub struct LogItemDto {
    pub source_id: u32,
    pub seq: u64,
    /// ISO 8601 風の表示文字列。解析できなかった場合 `null`。
    pub timestamp: Option<String>,
    /// 原文。`hakutaku_core::ItemDto::raw_text` の共有本文をそのまま受け取り、
    /// IPC 直前まで複製しません。
    #[serde(serialize_with = "serialize_shared_str")]
    pub raw_text: Arc<str>,
    pub source_label: String,
    pub source_line_number: u64,
    /// 未確定行（書き込み途中の可能性がある末尾断片）ではないか（`LOG-026`）。
    /// 解析エラーとは区別する表示メタデータです。
    pub confirmed: bool,
    /// 結合された継続行（`LOG-014`）の数。表示側の行高導出に使います。
    pub continuation_count: u32,
    /// 日時未解析の生データ項目か。
    pub raw_display: bool,
}

/// 共有本文（`Arc<str>`）を、通常の文字列と同じ JSON 文字列として直列化します。
///
/// serde の `Arc` 対応（`rc` フィーチャ）を有効にせずに済ませるための最小の
/// 実装です。出力は `String` を直列化した場合と1バイトも変わりません。本文の
/// 共有はメモリ確保を減らすための Rust 側の都合であり、フロントエンドとの
/// IPC 契約は変えません。
fn serialize_shared_str<S>(value: &Arc<str>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(value)
}

impl From<hakutaku_core::ItemDto> for LogItemDto {
    fn from(dto: hakutaku_core::ItemDto) -> Self {
        LogItemDto {
            source_id: dto.item_id.source_id,
            seq: dto.item_id.seq,
            timestamp: dto.timestamp_display,
            raw_text: dto.raw_text,
            source_label: dto.source_label,
            source_line_number: dto.source_line_number,
            confirmed: dto.confirmed,
            continuation_count: dto.continuation_count,
            raw_display: dto.raw_display,
        }
    }
}

/// `fetch_log_range` の応答（範囲取得契約。`hakutaku_core::RangeResponse` の
/// serde 化）。
#[derive(Debug, Clone, Serialize)]
pub struct FetchLogRangeResponse {
    pub generation: u64,
    pub total_items: u64,
    pub start: u64,
    pub items: Vec<LogItemDto>,
    pub truncated: bool,
}

impl From<hakutaku_core::RangeResponse> for FetchLogRangeResponse {
    fn from(response: hakutaku_core::RangeResponse) -> Self {
        FetchLogRangeResponse {
            generation: response.generation,
            total_items: response.total_items,
            start: response.start,
            items: response.items.into_iter().map(LogItemDto::from).collect(),
            truncated: response.truncated,
        }
    }
}

/// `fetch_log_range` が失敗した理由です。
///
/// フロントエンドは `kind` で分岐します。`generation_mismatch` は
/// `LOG-023`／`LOG-028` の下地（表示集合が再構築されたため、古い範囲を掴んだ
/// ままにならないようにする）です。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FetchLogRangeError {
    UnknownDisplaySet,
    GenerationMismatch { expected: u64, current: u64 },
}

impl std::fmt::Display for FetchLogRangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchLogRangeError::UnknownDisplaySet => {
                write!(f, "指定された表示集合が見つかりません。")
            }
            FetchLogRangeError::GenerationMismatch { expected, current } => write!(
                f,
                "表示集合の世代が一致しません（要求 {expected}、現在 {current}）。"
            ),
        }
    }
}

impl std::error::Error for FetchLogRangeError {}

impl From<hakutaku_core::FetchRangeError> for FetchLogRangeError {
    fn from(error: hakutaku_core::FetchRangeError) -> Self {
        match error {
            hakutaku_core::FetchRangeError::UnknownDisplaySet => {
                FetchLogRangeError::UnknownDisplaySet
            }
            hakutaku_core::FetchRangeError::GenerationMismatch { expected, current } => {
                FetchLogRangeError::GenerationMismatch { expected, current }
            }
        }
    }
}

/// [`hakutaku_core::ReloadOutcome`]（`restore_evicted_source` の戻り値）を
/// 診断ログへ記録します。復元の成否そのものはこのコマンドの応答形
/// （`FetchLogRangeResponse`／`FetchLogRangeError`）を変えません。復元に
/// 失敗した場合、対象ソースの状態は `hakutaku_core::SourceStatus::Changed`
/// 等へ遷移しており、後続の `fetch_range` はその表示集合の項目数どおりの
/// 応答（`Changed` 直後は0件）を返します（`registry::DisplaySetRegistry::
/// mark_changed_now` と同じ経路）。フロントエンドへの明示的な通知
/// （`list_targets` の状態反映）は P08 の後続課題です。
fn log_restore_outcome(
    diagnostics: &Diagnostics,
    source_id: u32,
    outcome: &hakutaku_core::ReloadOutcome,
) {
    match outcome {
        hakutaku_core::ReloadOutcome::Reloaded {
            generation,
            total_items,
            // 復元も表示集合を作り直すため生表示退避の有無が変わり得るが、この
            // 経路は対象一覧（`crate::targets`）の状態を一切更新しない（世代・
            // 件数も同じ）。反映は doc コメントに記した P08 の後続課題であり、
            // 現時点の範囲では診断ログに残すだけにとどめる。
            fell_back_to_raw_display,
        } => {
            diag_info!(
                diagnostics,
                module = "memory",
                operation = "memory.restore_evicted",
                "解放済みのソースを再読み込みしました（P08-3）: source_id={source_id}, \
                 generation={generation}, total_items={total_items}, \
                 fell_back_to_raw_display={fell_back_to_raw_display:?}"
            );
        }
        other => {
            diag_warn!(
                diagnostics,
                module = "memory",
                operation = "memory.restore_evicted",
                "解放済みのソースの復元に失敗しました（P08-3）: source_id={source_id}, {other:?}"
            );
        }
    }
}

/// 表示集合に対して範囲を取得します（契約に織り込む4点。
/// `tasks/phase-04-vertical-slice.md` を参照）。
///
/// `start` は表示集合内のインデックス（0起点）であり、ファイルの物理オフセット
/// ではありません。`expected_generation` が現在の世代と一致しない場合、
/// `generation_mismatch` エラーを返します（フロントエンドは古い範囲を捨てて
/// 最新の世代で取得し直してください）。
///
/// # P08-3: 遅延解放ドレインとアクティブソースの伝播・透過復元
///
/// このコマンドは、範囲取得の本来の処理に加えて次の3つを行います
/// （いずれも同じ `Mutex` ロック区間の中で行い、`hakutaku_core` 側の関数は
/// いずれも `&mut DisplaySetRegistry` を直接受け取るだけなので、ロックの
/// 再入は発生しません）。
///
/// 1. **遅延解放ドレイン**（[`drain_pending_eviction`]）: しきい値到達時に
///    立てられた解放要求フラグ（[`EvictionFlag`]）を確認し、立っていれば
///    非アクティブなソースのバッファを解放します。
/// 2. **アクティブソースの伝播**: `display_set_id` から `source_id` を逆引きし
///    （`最小の実装`。専用の「タブ切り替え」コマンドを新設せず、範囲取得の
///    対象から常に導出する）、`hakutaku_core::DisplaySetRegistry::
///    set_active_source` へ伝えます。
/// 3. **透過復元**: 対象ソースが解放済み（`SourceStatus::Evicted`）だった
///    場合、`hakutaku_core::restore_evicted_source` で再読み込みします。
///    復元後は世代が進むため、この呼び出しの `expected_generation` が古い
///    世代のままであれば `generation_mismatch` を返し、フロントエンドは
///    既存の再取得ロジック（`src/log_view.js`）で最新世代を取得し直します。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn fetch_log_range(
    registry: State<'_, DisplaySetRegistryState>,
    eviction: State<'_, EvictionFlag>,
    diagnostics: State<'_, Arc<Diagnostics>>,
    config: State<'_, ConfigState>,
    display_set_id: u32,
    expected_generation: u64,
    start: u64,
    max_items: u32,
) -> Result<FetchLogRangeResponse, FetchLogRangeError> {
    let diagnostics_ref: &Diagnostics = diagnostics.inner();
    let mut registry_guard = registry.0.lock().unwrap_or_else(PoisonError::into_inner);

    drain_pending_eviction(eviction.inner(), &mut registry_guard, diagnostics_ref);

    if let Some(source_id) = registry_guard.source_id_for_display_set(display_set_id) {
        registry_guard.set_active_source(Some(source_id));

        if matches!(
            registry_guard.source_status(source_id),
            Some(hakutaku_core::SourceStatus::Evicted)
        ) {
            if let Some(outcome) = hakutaku_core::restore_evicted_source(
                &mut registry_guard,
                source_id,
                &config.config.log_profiles,
            ) {
                log_restore_outcome(diagnostics_ref, source_id, &outcome);
            }
        }
    }

    let request = hakutaku_core::RangeRequest {
        start,
        max_items,
        expected_generation,
    };

    registry_guard
        .fetch_range(display_set_id, request)
        .map(FetchLogRangeResponse::from)
        .map_err(FetchLogRangeError::from)
}

// --- 統合表示集合（P09-1。`LOG-007`〜`LOG-008`、`LOG-015`） ---

/// `enable_merged_view` の応答（`hakutaku_core::MergedViewHandle` の serde 化）。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct MergedViewHandleDto {
    pub display_set_id: u32,
    pub generation: u64,
    pub total_items: u64,
}

impl From<hakutaku_core::MergedViewHandle> for MergedViewHandleDto {
    fn from(handle: hakutaku_core::MergedViewHandle) -> Self {
        MergedViewHandleDto {
            display_set_id: handle.display_set_id,
            generation: handle.generation,
            total_items: handle.total_items,
        }
    }
}

/// `enable_merged_view` が失敗した理由です（`ERR-002`、Issue #37）。
///
/// 以前は理由の文字列だけを返していましたが、フロントエンドが「何が起きたか」
/// 「なぜか」「次に何をすればよいか」を組み立てられるよう、種別と実値
/// （`reason`）を分けた形にしています。種別ごとに利用者へ案内すべき次の操作が
/// 異なるため（メモリ予算超過なら、開いている対象を閉じてから再試行する）、
/// フロントエンドは `kind` で分岐します。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnableMergedViewError {
    /// `PERF-008`。統合表示の参照列（`(source_id, seq)` の並び）の確保が
    /// メモリ予算で拒否された。**既存の表示は何も変わりません**（統合表示を
    /// 開始しなかっただけで、ファイル別タブの閲覧は継続できます）。
    MemoryReservationRejected { reason: String },
}

/// 現在開いている全ソースを横断する統合表示集合を構築し、ON にします
/// （`LOG-007`〜`LOG-008`）。**参加ソースの索引の再読み込み・再解析は行いません**
/// （「再読み込みなしの切り替え」。計画正本 `tasks/phase-09-timeline-merge.md`）。
///
/// フロントエンド（`src/shell.js`）は、応答の `display_set_id` を使って
/// `fetch_log_range`・`copy_selection` を呼び出します。範囲取得契約
/// （`RangeRequest`／`RangeResponse`）は個別ファイルの表示集合と変わりません。
///
/// メモリ予算（`PERF-008`）の逼迫で参照列（`(source_id, seq)` の並び）の確保が
/// 拒否された場合、統合表示集合を開始せず [`EnableMergedViewError`] を返します。
#[tauri::command]
pub fn enable_merged_view(
    registry: State<'_, DisplaySetRegistryState>,
) -> Result<MergedViewHandleDto, EnableMergedViewError> {
    let mut registry_guard = registry.0.lock().unwrap_or_else(PoisonError::into_inner);
    registry_guard
        .enable_merged_view()
        .map(MergedViewHandleDto::from)
        .map_err(
            |rejected| EnableMergedViewError::MemoryReservationRejected {
                reason: rejected.to_string(),
            },
        )
}

/// 統合表示集合を破棄し、OFF にします（`LOG-008`、`LOG-015`）。参加していた
/// 各ソースの索引・状態には一切触れません（ファイル別タブ表示へ、参照対象
/// ファイルを変更せずに戻せます。`ERR-003`）。既に OFF の場合は何もしません。
#[tauri::command]
pub fn disable_merged_view(registry: State<'_, DisplaySetRegistryState>) {
    let mut registry_guard = registry.0.lock().unwrap_or_else(PoisonError::into_inner);
    registry_guard.disable_merged_view();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_item_dto_conversion_preserves_all_fields() {
        let dto = hakutaku_core::ItemDto {
            item_id: hakutaku_core::ItemId {
                source_id: 3,
                seq: 7,
            },
            timestamp_display: Some("2026-07-28T15:12:23.456".to_string()),
            raw_text: std::sync::Arc::from("本文"),
            source_label: "a.log".to_string(),
            source_line_number: 42,
            confirmed: false,
            continuation_count: 2,
            raw_display: false,
        };

        let converted = LogItemDto::from(dto);

        assert_eq!(converted.source_id, 3);
        assert_eq!(converted.seq, 7);
        assert_eq!(
            converted.timestamp.as_deref(),
            Some("2026-07-28T15:12:23.456")
        );
        assert_eq!(&*converted.raw_text, "本文");
        assert_eq!(converted.source_label, "a.log");
        assert_eq!(converted.source_line_number, 42);
        assert!(!converted.confirmed);
        assert_eq!(converted.continuation_count, 2);
        assert!(!converted.raw_display);
    }

    #[test]
    fn fetch_log_range_error_conversion_preserves_generation_mismatch_fields() {
        let core_error = hakutaku_core::FetchRangeError::GenerationMismatch {
            expected: 1,
            current: 2,
        };
        let converted = FetchLogRangeError::from(core_error);
        assert_eq!(
            converted,
            FetchLogRangeError::GenerationMismatch {
                expected: 1,
                current: 2
            }
        );
    }

    #[test]
    fn fetch_log_range_error_conversion_maps_unknown_display_set() {
        let converted = FetchLogRangeError::from(hakutaku_core::FetchRangeError::UnknownDisplaySet);
        assert_eq!(converted, FetchLogRangeError::UnknownDisplaySet);
    }

    // 受け入れ条件（`ERR-002`、Issue #37）: 統合表示を開始できなかった理由が、
    // 種別（kind）と実値（reason）に分かれてフロントエンドへ届く。
    #[test]
    fn enable_merged_view_error_carries_kind_and_reason() {
        let rejected = hakutaku_memory_accounting::ReservationRejected {
            requested_bytes: 16,
            allocated_bytes: 1,
            outstanding_reserved_bytes: 2,
            budget_bytes: 3,
        };
        let error = EnableMergedViewError::MemoryReservationRejected {
            reason: rejected.to_string(),
        };
        let json = serde_json::to_value(&error).expect("直列化できるはず");
        assert_eq!(json["kind"], "memory_reservation_rejected");
        assert!(
            json["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("メモリ予約を拒否しました")),
            "拒否理由の実値がそのまま含まれるはず: {json}"
        );
    }

    // --- P08-3: EvictionFlag / drain_pending_eviction ---

    use hakutaku_diagnostics::DiagnosticsUnavailable;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    fn inactive_diagnostics() -> Diagnostics {
        Diagnostics::unavailable(DiagnosticsUnavailable {
            target: PathBuf::from("C:\\example\\logs\\hakutaku.log"),
            reason: "テスト用（診断ログは使わない）".to_string(),
            os_error_code: None,
        })
    }

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn create_text(label: &str, contents: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "hakutaku-log-view-test-{label}-{}-{count}-{nanos}.log",
                std::process::id()
            ));
            std::fs::write(&path, contents.as_bytes()).expect("テスト用ファイルを作成できません");
            TempFile { path }
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    // 受け入れ条件: 解放要求フラグが立っている場合、drain_pending_eviction は
    // 非アクティブなソースのデコード済みキャッシュを解放し、フラグを下ろす。
    //
    // P08-5 以降、`evict_inactive_sources` はもはや
    // `SourceStatus::Evicted` へ遷移させません（索引そのものが小さいため
    // 解放の必要がなくなり、デコード済みチャンクキャッシュをクリアするだけに
    // 単純化されました。`hakutaku_core::registry` の doc コメント「P08-3 →
    // P08-5: しきい値到達時の解放の単純化」参照）。ステータスは `Loaded` の
    // ままで、範囲取得は引き続き成功することを確認します。
    #[test]
    fn drain_pending_eviction_clears_cache_for_inactive_sources_and_clears_the_flag() {
        let mut registry = hakutaku_core::DisplaySetRegistry::new();
        let budget = hakutaku_core::SourceBudget::new();
        let file = TempFile::create_text("drain", "2026/07/28 15:12:23.456 line\n");

        let (handle, _summary) = hakutaku_core::register_source(
            &mut registry,
            &budget,
            &file.path,
            "a.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        let eviction = EvictionFlag::default();
        eviction.0.store(true, Ordering::Relaxed);
        let diagnostics = inactive_diagnostics();

        drain_pending_eviction(&eviction, &mut registry, &diagnostics);

        assert!(
            !eviction.0.load(Ordering::Relaxed),
            "ドレイン後はフラグが下りるはず"
        );
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(hakutaku_core::SourceStatus::Loaded),
            "P08-5 以降、キャッシュのクリアだけなので状態は変わらないはず"
        );
        let response = registry
            .fetch_range(
                handle.display_set_id,
                hakutaku_core::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("キャッシュがクリアされても本文を再読み出しできるはず");
        assert_eq!(&*response.items[0].raw_text, "2026/07/28 15:12:23.456 line");
    }

    // フラグが立っていなければ何もしない（誤って毎回解放しないことの確認）。
    #[test]
    fn drain_pending_eviction_does_nothing_when_flag_is_not_set() {
        let mut registry = hakutaku_core::DisplaySetRegistry::new();
        let budget = hakutaku_core::SourceBudget::new();
        let file = TempFile::create_text("drain-noop", "2026/07/28 15:12:23.456 line\n");

        let (handle, _summary) = hakutaku_core::register_source(
            &mut registry,
            &budget,
            &file.path,
            "a.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        let eviction = EvictionFlag::default();
        let diagnostics = inactive_diagnostics();

        drain_pending_eviction(&eviction, &mut registry, &diagnostics);

        assert_eq!(
            registry.source_status(handle.source_id),
            Some(hakutaku_core::SourceStatus::Loaded),
            "フラグが立っていないので解放されないはず"
        );
    }

    // 受け入れ条件（再入なし）: しきい値判定が `DisplaySetRegistryState` の
    // ロック保持中に呼ばれても、`register_release_handler` へ登録するハンドラ
    // （ここでは実配線と同じ形のクロージャを直接構築する）はロックを一切
    // 取らないため、デッドロックしない。もしハンドラが誤って `registry` を
    // 再ロックする実装になっていれば、このテストはハング（タイムアウト）する
    // （`std::sync::Mutex` は非再入のため）。
    #[test]
    fn eviction_release_handler_never_locks_the_registry_even_while_it_is_held() {
        let registry_state = DisplaySetRegistryState::default();
        let eviction = EvictionFlag::default();

        // crate::lib::run が register_release_handler へ渡すのと同じ形
        // （registry には一切触れない）。
        let handler_flag = eviction.clone();
        let handler: Box<dyn Fn() + Send + Sync> = Box::new(move || {
            handler_flag.0.store(true, Ordering::Relaxed);
        });

        {
            let _guard = registry_state
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // registry のロックを保持したまま handler を呼ぶ。ここで固まらない
            // ことがこのテストの主眼（しきい値判定が reserve/mark_allocated
            // 経路から、registry ロック保持中に呼ばれる状況の再現）。
            handler();
        }

        assert!(eviction.0.load(Ordering::Relaxed), "フラグが立っているはず");
    }

    // --- 統合表示集合（P09-1） ---

    // enable_merged_view / disable_merged_view 自体（#[tauri::command]）は
    // ロックを取って hakutaku_core::DisplaySetRegistry へ委譲するだけの薄い
    // 配線であり、実体（統合順序の構築・世代管理）は
    // crates/core-services/src/registry.rs で検証済み（他のコマンドと同じ方針。
    // 本ファイルの既存テストも #[tauri::command] 関数自体は State を介した
    // 呼び出しでは検証せず、DTO 変換や委譲先のロジックを検証している）。
    // ここでは DTO 変換だけを確認する。
    #[test]
    fn merged_view_handle_dto_conversion_preserves_all_fields() {
        let handle = hakutaku_core::MergedViewHandle {
            display_set_id: 7,
            generation: 3,
            total_items: 42,
        };

        let dto = MergedViewHandleDto::from(handle);

        assert_eq!(dto.display_set_id, 7);
        assert_eq!(dto.generation, 3);
        assert_eq!(dto.total_items, 42);
    }

    // --- 読み込みサマリーの診断ログ出力 ---

    /// 実際に書き出した診断ログを読み返すための一時ディレクトリです。
    ///
    /// `log_load_summary` は書式化した1行を `Diagnostics` へ渡すだけであり、
    /// 出力そのものを確かめるにはアクティブな `Diagnostics`（＝実ファイル）が
    /// 要ります。`crates/diagnostics/tests/diagnostics.rs` と同じ理由で
    /// `tempfile` は使わず、プロセス ID・カウンター・ナノ秒時刻で一意な名前を
    /// 組み立て、後片付けはベストエフォートで行います。
    struct TempLogsDir(PathBuf);

    impl TempLogsDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            TempLogsDir(std::env::temp_dir().join(format!(
                "hakutaku-log-view-diag-{label}-{}-{count}-{nanos}",
                std::process::id()
            )))
        }
    }

    impl Drop for TempLogsDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 日時書式とその決定経路だけを差し替えた読み込みサマリーを組み立てます
    /// （他の項目は既存の出力と同じ形の代表値で固定します）。
    fn summary_with_datetime_route(
        detected_datetime_format: Option<&'static str>,
        datetime_format_route: hakutaku_core::DatetimeFormatRoute,
    ) -> hakutaku_core::LoadSummary {
        hakutaku_core::LoadSummary {
            file_size_bytes: 128,
            line_count: 2,
            reserved_bytes: 64,
            encoding_route: "UTF-8（BOM無し・妥当性確認）",
            selected_encoding: "utf-8".to_string(),
            profile_resolution_route: "自動判定へ委譲",
            detected_datetime_format,
            datetime_format_route,
            decode_invalid_positions: Vec::new(),
            decode_invalid_positions_truncated: false,
            encoding_warnings: Vec::new(),
            fell_back_to_raw_display: false,
            has_unconfirmed_trailing_line: false,
            // 段階別内訳は診断ログの出力対象ではないため、
            // このテストでは既定値（すべて0）で足りる。
            stage_timings: hakutaku_core::LoadStageTimings::default(),
        }
    }

    // 受け入れ条件（DIAG-005）: 読み込みサマリーの1行から、確定した
    // 日時書式の値と決定経路の両方を読み取れる。既定（自動判定）の行は従来の
    // 項目をすべて残したまま経路だけが増える（既存のログと矛盾しない）。
    #[test]
    fn load_summary_line_contains_datetime_format_and_its_route() {
        use hakutaku_diagnostics::{ProcessElevation, RotationPolicy};

        let temp = TempLogsDir::new("datetime-route");
        let logs_dir = temp.0.join("logs");
        let (diagnostics, unavailable) = Diagnostics::open(
            &logs_dir,
            RotationPolicy::default(),
            ProcessElevation::Normal,
        );
        assert!(
            unavailable.is_none(),
            "書き込み可能な環境では診断ログを開けるはず"
        );

        log_load_summary(
            &diagnostics,
            "log_view",
            "log.open",
            "auto.log",
            &summary_with_datetime_route(
                Some("LOG-DT-001"),
                hakutaku_core::DatetimeFormatRoute::Auto,
            ),
        );
        log_load_summary(
            &diagnostics,
            "log_view",
            "log.open",
            "manual.log",
            &summary_with_datetime_route(
                Some("LOG-DT-004"),
                hakutaku_core::DatetimeFormatRoute::Manual,
            ),
        );

        let log_path = diagnostics
            .log_path()
            .expect("アクティブなら Some")
            .to_path_buf();
        let content = std::fs::read_to_string(&log_path).expect("ログファイルを読み取れる");
        let lines: Vec<&str> = content.lines().collect();

        let auto_line = lines
            .iter()
            .find(|line| line.contains("ファイル=auto.log"))
            .expect("自動判定の行が記録されているはず");
        // 既存項目（ENC-005 の判定経路、LOG-021 の解決経路、確定した書式）を
        // 落としていないこと。
        assert!(auto_line.contains("判定経路=UTF-8（BOM無し・妥当性確認）"));
        assert!(auto_line.contains("プロファイル解決経路=自動判定へ委譲"));
        assert!(auto_line.contains("日時書式=LOG-DT-001"));
        assert!(auto_line.contains("日時書式の決定経路=内容からの自動判定"));

        let manual_line = lines
            .iter()
            .find(|line| line.contains("ファイル=manual.log"))
            .expect("手動選択の行が記録されているはず");
        assert!(manual_line.contains("日時書式=LOG-DT-004"));
        assert!(
            manual_line.contains("日時書式の決定経路=UI での手動選択"),
            "同じ書式の値でも、決定経路の記録で自動判定と切り分けられること"
        );
    }
}
