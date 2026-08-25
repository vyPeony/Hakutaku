//! 計測モード（P04-3）。
//!
//! **開発・検証専用の機能であり、利用者向け機能ではありません。** 環境変数
//! `HAKUTAKU_MEASURE_FILE`（絶対パス）が起動時に設定されている場合だけ有効に
//! なり、通常の利用者向け起動では一切関与しません（`get_measurement_mode` は
//! 常に `active: false` を返し、`open_measurement_file` /
//! `record_measurement_results` は要求を拒否します）。
//!
//! # SEC-012 との整合
//!
//! フロントエンドは計測対象ファイルの絶対パスを一切知りません。
//! `open_measurement_file` は環境変数を Rust 側（このモジュール）だけで読み、
//! フロントエンドからパスを受け取りません（`open_log_file` のネイティブ
//! ダイアログ経路と同じ「絶対パスはフロントエンドへ渡さない」原則）。
//!
//! # SEC-009 との整合
//!
//! 計測結果 JSON の書き込み先は、起動時に確定した `logs` ディレクトリの直下へ
//! 作る `measurements` サブフォルダに限定します（実行時に作成・書き込みする
//! フォルダを `logs`・`temp`・`WebView2` に限定する方針に従う。`logs` の外へは
//! 書き込まず、実行時フォルダも増やしません）。導入フォルダごと退避・削除すれば
//! 計測結果も一緒に処分できます。サブフォルダは結果を書き出すときにだけ作るため、
//! 通常の利用者向け起動では作られません。
//!
//! 計測結果は診断ログではないため、`DIAG-002` のローテーション（10 MiB × 5世代）
//! の対象ではなく、`SEC-006` の `temp` 清掃の対象でもありません（削除は手動、
//! または導入フォルダごとの削除）。`logs` 直下ではなくサブフォルダへ分ける理由は
//! [`RESULT_DIR_NAME`] を参照してください（Issue #46）。
//!
//! # 計測モードでない場合の拒否
//!
//! `open_measurement_file` / `record_measurement_results` は、計測モードが
//! 有効でない場合、対象ファイルの読み込みや結果の書き込みを一切行わずに拒否
//! します（[`open_measurement_file_core`]・[`record_measurement_results_core`]
//! の単体テストで確認しています）。
//!
//! # 製品ビルドから分離しない理由（Issue #46）
//!
//! この3コマンドは release ビルドでも `tauri::generate_handler!` へ登録され、
//! Capability（`src-tauri/capabilities/default.toml`）にも常時含まれます。
//! ビルドで切り分けない理由:
//!
//! 1. 計測は release ビルドでの実測が目的であり、`#[cfg(debug_assertions)]` で
//!    落とすと、計測したいビルドから計測手段が消える
//! 2. Cargo feature で切り分けると `generate_handler!` の登録一覧が feature ごとに
//!    二重化し、静的な toml である Capability にも feature 分岐が要る。
//!    `scripts/check-capabilities.mjs` は「登録一覧と許可の1対1対応」を前提に
//!    整合を検査しており、その前提が壊れて検査が成立しなくなる
//! 3. 実行時ゲート（環境変数 `HAKUTAKU_MEASURE_FILE`）が3コマンドすべてで
//!    機能しており、未設定の起動では対象ファイルの読み取りも結果の書き込みも
//!    行われない（このモジュールの単体テストで固定）
//!
//! この判断は `docs/security/data-handling.md` の「計測モードの出力（開発・
//! 検証専用）」にも記録しています。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use hakutaku_diagnostics::{diag_info, diag_warn, Diagnostics};

use crate::bootstrap::config::ConfigState;
use crate::log_view::{log_load_summary, DisplaySetRegistryState, OpenLogFileResponse};

/// 計測モードの有効・無効を切り替える環境変数名。
pub const MEASURE_FILE_ENV_VAR: &str = "HAKUTAKU_MEASURE_FILE";

/// PrivateUsage サンプラーの採取間隔（作業項目5「500ms 間隔」）。
const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

/// 計測結果 JSON のファイル名の接頭辞。
const RESULT_FILE_PREFIX: &str = "measurement-p04-";

/// 計測結果 JSON を置く、`logs` 直下のサブフォルダ名（Issue #46）。
///
/// `logs` 直下へ直接置くと、`DIAG-002` のローテーション（10 MiB × 5世代）で
/// 管理される診断ログと、ローテーションも自動清掃もされない計測結果とが同じ
/// 場所に並ぶ。`docs/security/data-handling.md` の `logs` の説明（10 MiB × 5世代）
/// からは後者の無期限の蓄積を想定できないため、計測結果はこのサブフォルダへ
/// 隔離し、そこだけを手動削除の対象として文書化する。
const RESULT_DIR_NAME: &str = "measurements";

/// PrivateUsage 時系列の1点（経過ミリ秒・合計バイト・プロセス数）。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PrivateUsageSamplePoint {
    pub elapsed_ms: u64,
    pub total_private_usage_bytes: usize,
    pub process_count: usize,
}

/// 計測モードの managed state です。
///
/// `measure_file` が `Some` の間だけ計測モードが有効です
/// （[`MeasurementState::from_env`] 参照）。`samples` は起動直後から
/// [`start_sampler_if_active`] が起動するサンプラースレッドが書き込み、
/// `record_measurement_results` がスナップショットを読み出します。
pub struct MeasurementState {
    measure_file: Option<PathBuf>,
    logs_dir: PathBuf,
    samples: Mutex<Vec<PrivateUsageSamplePoint>>,
}

impl MeasurementState {
    /// 環境変数 [`MEASURE_FILE_ENV_VAR`] を読んで計測モードの状態を構築します。
    ///
    /// 値が絶対パスでない場合は無効として扱います（誤設定を安全側に倒す。
    /// 計測モードが意図せず有効になることを避ける）。`logs_dir` は結果 JSON の
    /// 書き込み先（`record_measurement_results`）です。
    pub fn from_env(logs_dir: PathBuf) -> Self {
        Self::from_raw_env_value(std::env::var_os(MEASURE_FILE_ENV_VAR), logs_dir)
    }

    /// 環境変数の生の値から状態を組み立てます。
    ///
    /// [`MeasurementState::from_env`] は現在のプロセスの環境変数を読むだけで、
    /// 有効・無効の判断はすべてこちらへ集約します。これにより、単体テストは
    /// 「環境変数が未設定」（`None`）の状態を、プロセスの環境変数を書き換えずに
    /// 再現できます（環境変数の書き換えは同一プロセスで並行して走る他のテストへ
    /// 影響するため使いません）。
    fn from_raw_env_value(raw: Option<std::ffi::OsString>, logs_dir: PathBuf) -> Self {
        MeasurementState {
            measure_file: resolve_measure_file_path(raw),
            logs_dir,
            samples: Mutex::new(Vec::new()),
        }
    }

    /// 計測モードが有効かどうか。
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.measure_file.is_some()
    }

    /// テスト専用: サンプラースレッドを起動せずに、時系列へ1点だけ直接追加する。
    #[cfg(test)]
    fn push_sample_for_test(&self, point: PrivateUsageSamplePoint) {
        if let Ok(mut guard) = self.samples.lock() {
            guard.push(point);
        }
    }
}

/// 環境変数の生の値から、計測対象ファイルのパスを解決する純粋関数です。
///
/// 絶対パスでない値（未設定・空・相対パス）は `None`（無効）として扱います。
/// Win32 API を呼ばないため、`#[cfg(test)]` から直接、環境変数の実体を汚さずに
/// 検証できます。
fn resolve_measure_file_path(raw: Option<std::ffi::OsString>) -> Option<PathBuf> {
    raw.map(PathBuf::from).filter(|path| path.is_absolute())
}

/// 計測モードが有効な場合、PrivateUsage サンプラースレッドを起動します。
///
/// スレッドは計測モードの間、[`SAMPLE_INTERVAL`] ごとにサンプリングを続けます。
/// このスレッドを明示的に停止する経路は設けていません。計測モードは開発・
/// 検証専用であり、計測完了後は呼び出し側（計測実行スクリプト）がプロセスごと
/// 終了させる運用を前提とするためです（`tasks/phase-04-vertical-slice.md`
/// 作業項目4「taskkill で終了」）。
pub fn start_sampler_if_active(state: &Arc<MeasurementState>, diagnostics: &Arc<Diagnostics>) {
    if !state.is_active() {
        return;
    }

    let state = Arc::clone(state);
    let diagnostics_for_thread = Arc::clone(diagnostics);
    // エラー時の diag_warn! はスレッド起動元（この関数の呼び出し元）でだけ使う
    // ため、スレッドへ move する分とは別に、警告用のクローンを手元に残しておく
    // （move クロージャへ渡した Arc は以降ここから参照できない）。
    let diagnostics = Arc::clone(diagnostics);
    let spawn_result = thread::Builder::new()
        .name("hakutaku-measurement-sampler".to_string())
        .spawn(move || run_sampler_loop(&state, &diagnostics_for_thread));

    if let Err(error) = spawn_result {
        diag_warn!(
            diagnostics,
            module = "measurement",
            operation = "measurement.sampler_start",
            "PrivateUsage サンプラースレッドを起動できませんでした（計測モードの \
             時系列は記録されません）: {error}"
        );
    }
}

/// [`start_sampler_if_active`] が起動するスレッドの本体です。
fn run_sampler_loop(state: &Arc<MeasurementState>, diagnostics: &Arc<Diagnostics>) {
    let start = Instant::now();
    loop {
        thread::sleep(SAMPLE_INTERVAL);
        match hakutaku_memory_accounting::measure_private_usage() {
            Ok(sample) => {
                let point = PrivateUsageSamplePoint {
                    elapsed_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    total_private_usage_bytes: sample.total_private_usage_bytes,
                    process_count: sample.processes.len(),
                };
                if let Ok(mut guard) = state.samples.lock() {
                    guard.push(point);
                }
            }
            Err(error) => {
                diag_warn!(
                    diagnostics,
                    module = "measurement",
                    operation = "measurement.sample",
                    "PrivateUsage サンプリングに失敗しました: {error}"
                );
            }
        }
    }
}

/// `get_measurement_mode` の応答。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct MeasurementModeResponse {
    pub active: bool,
}

/// 計測モードでない場合の拒否理由、または結果の書き込みに失敗した理由です。
#[derive(Debug, Clone, Serialize)]
pub struct MeasurementModeError {
    pub reason: String,
}

impl std::fmt::Display for MeasurementModeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

impl std::error::Error for MeasurementModeError {}

fn rejected(reason: impl Into<String>) -> MeasurementModeError {
    MeasurementModeError {
        reason: reason.into(),
    }
}

/// [`get_measurement_mode`] の中核ロジックです。ほかの2コマンドと同じく、
/// `State` を必要としない形に切り出して単体テストできるようにします。
fn measurement_mode_core(measurement: &MeasurementState) -> MeasurementModeResponse {
    MeasurementModeResponse {
        active: measurement.is_active(),
    }
}

/// 計測モードが有効かどうかを返します。フロントエンドの起動フロー
/// （`src/main.js`）がこれを見て、計測スクリプト（`src/measurement.js`）を
/// 自動実行するかどうかと、保持上限の内部状態観測 API
/// （`window.__hakutakuStats`、`src/retention_stats.js`）を公開するかどうかを
/// 判断します（Issue #46）。
#[tauri::command]
pub fn get_measurement_mode(state: State<'_, Arc<MeasurementState>>) -> MeasurementModeResponse {
    measurement_mode_core(state.inner())
}

/// [`open_measurement_file`] の中核ロジックです。`State` を必要としない形に
/// 切り出すことで、Tauri アプリを起動せずに単体テストできます。
fn open_measurement_file_core(
    measurement: &MeasurementState,
    registry: &mut hakutaku_core::DisplaySetRegistry,
    diagnostics: &Diagnostics,
    log_profiles: &[hakutaku_config::LogProfileConfig],
) -> Result<OpenLogFileResponse, MeasurementModeError> {
    let Some(path) = measurement.measure_file.clone() else {
        diag_warn!(
            diagnostics,
            module = "measurement",
            operation = "measurement.open_rejected",
            "計測モードが無効な状態で open_measurement_file が呼び出されました。拒否します。"
        );
        return Err(rejected(
            "計測モードではないため、計測用ファイルを開く要求を拒否しました。",
        ));
    };

    // SEC-012: フロントエンドからパスを受け取らず、環境変数から読み取り済みの
    // 絶対パスだけを使う。表示用ラベルはファイル名のみ（open_log_file と同じ
    // 方針）。
    let source_label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(不明なファイル名)".to_string());

    match hakutaku_core::load_file_into_registry(
        registry,
        &path,
        source_label.clone(),
        log_profiles,
    ) {
        Ok((handle, summary)) => {
            diag_info!(
                diagnostics,
                module = "measurement",
                operation = "measurement.open",
                "計測用ファイルを読み込みました: 行数={}, バイト数={}, 予約量={} バイト",
                summary.line_count,
                summary.file_size_bytes,
                summary.reserved_bytes
            );
            log_load_summary(
                diagnostics,
                "measurement",
                "measurement.open",
                &source_label,
                &summary,
            );
            Ok(OpenLogFileResponse::Opened {
                // 計測モードは対象一覧（crate::targets）を経由しない専用の
                // 読み込み経路であるため target_id を持たない（モジュール
                // doc コメント、および OpenLogFileResponse の doc コメント参照）。
                target_id: None,
                source_id: handle.source_id,
                display_set_id: handle.display_set_id,
                generation: handle.generation,
                total_items: handle.total_items,
                source_label,
                fell_back_to_raw_display: summary.fell_back_to_raw_display,
            })
        }
        Err(error) => {
            diag_warn!(
                diagnostics,
                module = "measurement",
                operation = "measurement.open",
                "計測用ファイルを読み込めませんでした: {error}"
            );
            let user_error = hakutaku_core::notification::UserFacingError::new(
                source_label,
                error.to_string(),
                "計測用ファイルを確認してください。",
            );
            Ok(OpenLogFileResponse::Failed {
                target_id: None,
                error: crate::targets::UserFacingErrorDto::from(&user_error),
            })
        }
    }
}

/// 計測モード専用のファイルオープンです。
///
/// フロントエンドから対象パスを受け取りません（`SEC-012`）。環境変数から
/// 読み取り済みのパス（[`MeasurementState`]）を Rust 側だけで使い、
/// `open_log_file` と同じ読み込み経路
/// （[`hakutaku_core::load_file_into_registry`]）で表示集合を構築し、
/// `open_log_file` と同じ形の応答（[`OpenLogFileResponse`]）を返します。
/// 計測モードでない場合は要求を拒否します（[`open_measurement_file_core`]）。
/// `config`（`hakutaku.yaml` の `log_profiles`）は `open_log_file` と同じ
/// プロファイル解決へ渡します。
#[tauri::command]
pub fn open_measurement_file(
    state: State<'_, Arc<MeasurementState>>,
    registry: State<'_, DisplaySetRegistryState>,
    diagnostics: State<'_, Arc<Diagnostics>>,
    config: State<'_, ConfigState>,
) -> Result<OpenLogFileResponse, MeasurementModeError> {
    let measurement_state: &MeasurementState = state.inner();
    let diagnostics_ref: &Diagnostics = diagnostics.inner();
    let mut registry_guard = registry
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    open_measurement_file_core(
        measurement_state,
        &mut registry_guard,
        diagnostics_ref,
        &config.config.log_profiles,
    )
}

/// [`record_measurement_results`] の中核ロジックです。`State` を必要としない
/// 形に切り出すことで、Tauri アプリを起動せずに単体テストできます。
fn record_measurement_results_core(
    measurement: &MeasurementState,
    diagnostics: &Diagnostics,
    results_json: String,
) -> Result<(), MeasurementModeError> {
    if !measurement.is_active() {
        diag_warn!(
            diagnostics,
            module = "measurement",
            operation = "measurement.record_rejected",
            "計測モードが無効な状態で record_measurement_results が呼び出されました。拒否します。"
        );
        return Err(rejected(
            "計測モードではないため、計測結果の記録要求を拒否しました。",
        ));
    }

    // フロントエンドが送ってきた JSON 文字列を値として解釈する。万一パースに
    // 失敗しても（通常発生しない）記録自体は諦めず、生文字列を保持して続行する。
    let frontend_results: serde_json::Value =
        serde_json::from_str(&results_json).unwrap_or_else(|error| {
            diag_warn!(
                diagnostics,
                module = "measurement",
                operation = "measurement.record",
                "計測結果 JSON の解析に失敗しました。生文字列のまま記録します: {error}"
            );
            serde_json::Value::String(results_json.clone())
        });

    let samples: Vec<PrivateUsageSamplePoint> = measurement
        .samples
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let sample_count = samples.len();

    let document = serde_json::json!({
        "frontend": frontend_results,
        "private_usage_time_series": samples,
    });

    let body = serde_json::to_vec_pretty(&document).unwrap_or_else(|_| results_json.into_bytes());

    // SEC-009: 書き込み先は logs 配下だけに限る。サブフォルダ
    // （[`RESULT_DIR_NAME`]）は計測結果を書き出すこの場面でだけ作るため、
    // 通常の利用者向け起動では作られない。
    let target_dir = measurement.logs_dir.join(RESULT_DIR_NAME);
    if let Err(error) = std::fs::create_dir_all(&target_dir) {
        diag_warn!(
            diagnostics,
            module = "measurement",
            operation = "measurement.record",
            "計測結果の保存先フォルダを作成できませんでした: {}（{error}）",
            target_dir.display()
        );
        return Err(rejected(format!(
            "計測結果の保存先フォルダを作成できませんでした: {error}"
        )));
    }
    let target_path = target_dir.join(result_file_name());

    match std::fs::write(&target_path, &body) {
        Ok(()) => {
            diag_info!(
                diagnostics,
                module = "measurement",
                operation = "measurement.record",
                "計測結果を書き出しました: {}（バイト数={}, PrivateUsageサンプル数={}）",
                target_path.display(),
                body.len(),
                sample_count
            );
            Ok(())
        }
        Err(error) => {
            diag_warn!(
                diagnostics,
                module = "measurement",
                operation = "measurement.record",
                "計測結果を書き出せませんでした: {}（{error}）",
                target_path.display()
            );
            Err(rejected(format!("計測結果を書き出せませんでした: {error}")))
        }
    }
}

/// 計測結果を `logs\measurements` フォルダへ書き出します。
///
/// フロントエンド（`src/measurement.js`）が集計した JSON 文字列
/// （`results_json`）に、Rust 側で採取した PrivateUsage 時系列（作業項目5）を
/// 添えて、`measurement-p04-<PID>-<ナノ秒>.json` として `logs` 直下の
/// `measurements` フォルダへ書き出します（`SEC-009`。フォルダが無ければ作成
/// します）。計測モードでない場合は書き込みを行わず拒否します
/// （[`record_measurement_results_core`]）。
#[tauri::command]
pub fn record_measurement_results(
    state: State<'_, Arc<MeasurementState>>,
    diagnostics: State<'_, Arc<Diagnostics>>,
    results_json: String,
) -> Result<(), MeasurementModeError> {
    let measurement_state: &MeasurementState = state.inner();
    let diagnostics_ref: &Diagnostics = diagnostics.inner();
    record_measurement_results_core(measurement_state, diagnostics_ref, results_json)
}

/// 計測結果 JSON のファイル名を作ります。
///
/// プロセス ID と現在時刻（UNIX エポックからのナノ秒）を組み合わせます
/// （`bootstrap::layout` の書き込み確認用ファイル名と同じ流儀）。以前は秒精度
/// だったため、同じ秒のうちに2回書き出すと1回目を黙って上書きしていました
/// （Issue #46）。同一プロセスの連続書き出しはナノ秒で、別プロセスの同時計測は
/// PID で区別されます。
///
/// `SystemTime::duration_since` が失敗する（システム時計がエポックより前）
/// という通常起こり得ない状況でも panic せず `0` へフォールバックします。
fn result_file_name() -> String {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{RESULT_FILE_PREFIX}{pid}-{nanos}.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hakutaku_diagnostics::DiagnosticsUnavailable;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn inactive_diagnostics() -> Diagnostics {
        Diagnostics::unavailable(DiagnosticsUnavailable {
            target: PathBuf::from("C:\\example\\logs\\hakutaku.log"),
            reason: "テスト用（診断ログは使わない）".to_string(),
            os_error_code: None,
        })
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "hakutaku-measurement-test-{label}-{}-{count}-{nanos}",
            std::process::id()
        ))
    }

    /// 環境変数 `HAKUTAKU_MEASURE_FILE` が未設定のまま起動した状態
    /// （通常の利用者向け起動と同じ）を、プロセスの環境変数を書き換えずに作る。
    fn state_without_env_var(logs_dir: PathBuf) -> MeasurementState {
        MeasurementState::from_raw_env_value(None, logs_dir)
    }

    /// 環境変数へ絶対パスを設定して起動した状態（計測モード）を作る。
    fn state_with_env_var(measure_file: &std::path::Path, logs_dir: PathBuf) -> MeasurementState {
        MeasurementState::from_raw_env_value(
            Some(measure_file.as_os_str().to_os_string()),
            logs_dir,
        )
    }

    /// `logs\measurements` 配下の計測結果ファイル名を昇順で返す。
    fn result_file_names(logs_dir: &std::path::Path) -> Vec<String> {
        let measurements_dir = logs_dir.join(RESULT_DIR_NAME);
        let Ok(entries) = std::fs::read_dir(&measurements_dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(RESULT_FILE_PREFIX))
            .collect();
        names.sort();
        names
    }

    // --- resolve_measure_file_path（純粋関数）の単体テスト ---

    #[test]
    fn resolve_measure_file_path_accepts_absolute_path() {
        let resolved =
            resolve_measure_file_path(Some(std::ffi::OsString::from(r"C:\logs\sample.log")));
        assert_eq!(resolved, Some(PathBuf::from(r"C:\logs\sample.log")));
    }

    #[test]
    fn resolve_measure_file_path_rejects_relative_path() {
        let resolved = resolve_measure_file_path(Some(std::ffi::OsString::from("sample.log")));
        assert_eq!(resolved, None, "相対パスは無効として扱うはず");
    }

    #[test]
    fn resolve_measure_file_path_rejects_unset_env_var() {
        assert_eq!(resolve_measure_file_path(None), None);
    }

    // --- 環境変数が未設定のとき（＝通常の利用者向け起動）の3コマンド ---

    // 受け入れ条件: 環境変数 HAKUTAKU_MEASURE_FILE が未設定の起動では、
    // get_measurement_mode が active: false を返す（`SEC-012`、Issue #46）。
    // 3コマンドは release ビルドにも登録・許可されたままのため、実行時ゲートが
    // 効いていることをここで固定する（モジュール doc コメント「製品ビルドから
    // 分離しない理由」参照）。
    #[test]
    fn measurement_mode_core_reports_inactive_when_env_var_is_unset() {
        let measurement = state_without_env_var(unique_temp_dir("mode-inactive"));

        assert!(!measurement.is_active());
        assert!(
            !measurement_mode_core(&measurement).active,
            "環境変数が未設定なら計測モードは無効のはず"
        );
    }

    // 受け入れ条件: 環境変数が未設定なら、実在する読み取り可能なファイルが
    // あっても open_measurement_file はそれを読まない（`SEC-012`、Issue #46）。
    //
    // open_measurement_file はフロントエンドからパスを受け取らず（引数が無い）、
    // 対象パスの唯一の入力は環境変数である。したがって環境変数が未設定の起動では、
    // フロントエンド（WebView 上のスクリプト）がこのコマンドを呼び出せたとしても、
    // 任意パスの読み取りには使えない。
    #[test]
    fn open_measurement_file_core_cannot_read_any_path_without_the_env_var() {
        let dir = unique_temp_dir("open-no-env");
        std::fs::create_dir_all(&dir).expect("作業ディレクトリを作成できません");
        let readable_file = dir.join("readable.log");
        std::fs::write(
            &readable_file,
            "2026/07/28 15:12:23.456 読まれないはずの行\n",
        )
        .expect("対象ファイルを作成できません");

        let measurement = state_without_env_var(dir.clone());
        let mut registry = hakutaku_core::DisplaySetRegistry::new();
        let diagnostics = inactive_diagnostics();

        let result = open_measurement_file_core(&measurement, &mut registry, &diagnostics, &[]);

        assert!(result.is_err(), "環境変数が未設定の場合は拒否するはず");
        assert!(registry.is_empty(), "拒否時は表示集合を登録しないはず");
        assert!(
            readable_file.exists(),
            "対象ファイル自体は残る（読み取りも行われない）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- open_measurement_file_core の単体テスト ---

    // 受け入れ条件: 計測モードでないときに open_measurement_file が動かない
    // （要求を拒否する）。
    #[test]
    fn open_measurement_file_core_rejects_when_inactive() {
        let measurement = state_without_env_var(unique_temp_dir("open-inactive"));
        let mut registry = hakutaku_core::DisplaySetRegistry::new();
        let diagnostics = inactive_diagnostics();

        let result = open_measurement_file_core(&measurement, &mut registry, &diagnostics, &[]);

        assert!(result.is_err(), "計測モードでない場合は拒否するはず");
        assert!(registry.is_empty(), "拒否時は表示集合を登録しないはず");
    }

    // 受け入れ条件: 計測モードが有効な場合、環境変数由来のパスを読み込み、
    // open_log_file と同じ形の Opened 応答を返す。
    #[test]
    fn open_measurement_file_core_opens_file_when_active() {
        let dir = unique_temp_dir("open-active");
        std::fs::create_dir_all(&dir).expect("作業ディレクトリを作成できません");
        let file_path = dir.join("target.log");
        std::fs::write(
            &file_path,
            "2026/07/28 15:12:23.456 計測用の1行目\n2行目（書式に一致しない）\n",
        )
        .expect("計測対象ファイルを作成できません");

        let measurement = state_with_env_var(&file_path, dir.clone());
        let mut registry = hakutaku_core::DisplaySetRegistry::new();
        let diagnostics = inactive_diagnostics();

        let response = open_measurement_file_core(&measurement, &mut registry, &diagnostics, &[])
            .expect("計測モードが有効なので成功するはず");

        match response {
            OpenLogFileResponse::Opened {
                total_items,
                source_label,
                ..
            } => {
                // 2行目（書式に一致しない）は日時付き1行目への継続行として結合
                // されるため（LOG-014）、論理項目数は1件になる。
                assert_eq!(total_items, 1);
                assert_eq!(source_label, "target.log");
            }
            other => panic!("Opened 応答を期待しましたが {other:?} でした"),
        }
        assert_eq!(registry.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- record_measurement_results_core の単体テスト ---

    // 受け入れ条件: 計測モードでないときに record_measurement_results が動かない
    // （要求を拒否し、ファイルを書き出さない）。環境変数が未設定なら、
    // measurements サブフォルダの作成も行わない（Issue #46）。
    #[test]
    fn record_measurement_results_core_rejects_when_inactive() {
        let dir = unique_temp_dir("record-inactive");
        let measurement = state_without_env_var(dir.clone());
        let diagnostics = inactive_diagnostics();

        let result = record_measurement_results_core(&measurement, &diagnostics, "{}".to_string());

        assert!(result.is_err(), "計測モードでない場合は拒否するはず");
        assert!(
            !dir.exists(),
            "拒否時は logs ディレクトリへ一切書き込まないはず"
        );
    }

    // 受け入れ条件: 計測モードが有効な場合、logs\measurements へ
    // measurement-p04-*.json を書き出し、フロントエンド結果と PrivateUsage
    // 時系列の両方を含む。
    #[test]
    fn record_measurement_results_core_writes_file_with_frontend_and_time_series_when_active() {
        let dir = unique_temp_dir("record-active");
        std::fs::create_dir_all(&dir).expect("logs ディレクトリを作成できません");
        let measure_file = dir.join("dummy.log");
        let measurement = state_with_env_var(&measure_file, dir.clone());
        measurement.push_sample_for_test(PrivateUsageSamplePoint {
            elapsed_ms: 500,
            total_private_usage_bytes: 12_345,
            process_count: 3,
        });
        measurement.push_sample_for_test(PrivateUsageSamplePoint {
            elapsed_ms: 1000,
            total_private_usage_bytes: 23_456,
            process_count: 3,
        });
        let diagnostics = inactive_diagnostics();

        let result = record_measurement_results_core(
            &measurement,
            &diagnostics,
            r#"{"note":"テスト用の結果"}"#.to_string(),
        );
        assert!(result.is_ok(), "{result:?}");

        let names = result_file_names(&dir);
        assert_eq!(names.len(), 1, "計測結果ファイルが1件生成されるはず");

        let content = std::fs::read_to_string(dir.join(RESULT_DIR_NAME).join(&names[0]))
            .expect("計測結果ファイルを読み取れません");
        let parsed: serde_json::Value = serde_json::from_str(&content).expect("有効な JSON のはず");
        assert_eq!(parsed["frontend"]["note"], "テスト用の結果");
        let time_series = parsed["private_usage_time_series"]
            .as_array()
            .expect("配列のはず");
        assert_eq!(time_series.len(), 2);
        assert_eq!(time_series[0]["elapsed_ms"], 500);
        assert_eq!(time_series[1]["total_private_usage_bytes"], 23_456);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // 受け入れ条件: 計測結果は logs 直下ではなく logs\measurements へ書き出し、
    // サブフォルダが無ければ書き込み時に作成する（`SEC-009`、Issue #46）。
    #[test]
    fn record_measurement_results_core_writes_into_the_measurements_subdirectory() {
        let dir = unique_temp_dir("record-subdir");
        std::fs::create_dir_all(&dir).expect("logs ディレクトリを作成できません");
        let measurements_dir = dir.join(RESULT_DIR_NAME);
        assert!(
            !measurements_dir.exists(),
            "書き込み前はサブフォルダが無い状態から始める"
        );

        let measure_file = dir.join("dummy.log");
        let measurement = state_with_env_var(&measure_file, dir.clone());
        let diagnostics = inactive_diagnostics();

        let result = record_measurement_results_core(&measurement, &diagnostics, "{}".to_string());
        assert!(result.is_ok(), "{result:?}");

        assert!(
            measurements_dir.is_dir(),
            "書き込み時にサブフォルダを作成するはず"
        );
        assert_eq!(result_file_names(&dir).len(), 1);

        // logs 直下（診断ログとローテーションの場所）へは何も置かない。
        let leftovers_in_logs_root: Vec<String> = std::fs::read_dir(&dir)
            .expect("logs ディレクトリを読み取れません")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(RESULT_FILE_PREFIX))
            .collect();
        assert!(
            leftovers_in_logs_root.is_empty(),
            "logs 直下に計測結果が残っています: {leftovers_in_logs_root:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // 受け入れ条件: 同一プロセスで連続して書き出しても、前回の結果を上書きせず
    // 別名で残る（秒精度のファイル名による黙った上書きの解消。Issue #46）。
    #[test]
    fn record_measurement_results_core_does_not_overwrite_consecutive_results() {
        let dir = unique_temp_dir("record-consecutive");
        std::fs::create_dir_all(&dir).expect("logs ディレクトリを作成できません");
        let measure_file = dir.join("dummy.log");
        let measurement = state_with_env_var(&measure_file, dir.clone());
        let diagnostics = inactive_diagnostics();

        for note in ["1回目", "2回目"] {
            let result = record_measurement_results_core(
                &measurement,
                &diagnostics,
                format!(r#"{{"note":"{note}"}}"#),
            );
            assert!(result.is_ok(), "{result:?}");
        }

        let names = result_file_names(&dir);
        assert_eq!(names.len(), 2, "2回分が別名で残るはず: {names:?}");
        assert_ne!(names[0], names[1]);

        // どちらの回の内容も失われていないこと（先の回が後の回で潰されていない）。
        let mut notes: Vec<String> = names
            .iter()
            .map(|name| {
                let content = std::fs::read_to_string(dir.join(RESULT_DIR_NAME).join(name))
                    .expect("計測結果ファイルを読み取れません");
                let parsed: serde_json::Value =
                    serde_json::from_str(&content).expect("有効な JSON のはず");
                parsed["frontend"]["note"]
                    .as_str()
                    .expect("note は文字列のはず")
                    .to_string()
            })
            .collect();
        notes.sort();
        assert_eq!(notes, vec!["1回目".to_string(), "2回目".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
