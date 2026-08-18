//! 参照対象一覧の managed state と関連コマンド（P07-1／P07-2／P06-5）。
//!
//! # 位置づけ
//!
//! `tasks/phase-07-shell-ui.md` の共通シェルが表示する「参照対象一覧」
//! （設定由来の事前定義パスと、アドホックに開いた対象の両方）を Tauri の
//! managed state として保持します。
//!
//! P06 の読み込み実装（`hakutaku_core::register_source_with_control`）と
//! 結合し、進捗・キャンセル・手動プロファイル選択を実際に機能させます
//! （P07-2）。あわせて、コア層（`hakutaku_core`）が提供する複数
//! ソースの上限判定（`SourceBudget`）・共有違反の経路
//! （`SourceStatus::SharingViolation`）・明示的な再読み込み
//! （`hakutaku_core::reload_source`、ADR-0007）も結線します（P06-5）。
//!
//! - 対象一覧のエントリ（[`TargetEntry`]）と、コア層の表示集合
//!   （`hakutaku_core::DisplaySet`）は別物ですが、[`TargetStatus::Ready`] と
//!   [`TargetStatus::CancelledPartial`] はいずれもコア層の `source_id` を
//!   保持し、[`close_target`]・[`retry_target`]・[`reload_target`] がそれを
//!   鍵にコア側の状態（`SourceBudget` の予約・表示集合）を操作します
//!   （[`active_source_id`]）
//! - ここで構築するエラーは、`hakutaku_core::notification::UserFacingError`
//!   （P04-6、`ERR-002` の5要素）をそのまま使い、DTO 化して返します
//!   （[`UserFacingErrorDto`]）。`ERR-002` に従い、フルパスをマスキング
//!   しません
//! - `SEC-012` に従い、`hakutaku_config::DataSourceConfig::path` や
//!   アドホックに選択した絶対パスは、この managed state（Rust 側）にだけ
//!   保持します。フロントエンドへは表示名（`display_name`）だけを渡します。
//!   例外は `ERR-002` のエラー領域で、対象を識別するために意図的にフルパスを
//!   文字列として含めます（`SEC-012` はフロントエンドへ「ファイル
//!   アクセス権」を与えないことが趣旨であり、読み取り専用の表示文字列に
//!   パスを含めること自体は禁じていません）
//!
//! # 非同期化の設計（P07-2）
//!
//! `open_log_file`・`open_config_data_source`・`retry_target` は、対象を
//! 「読み込み中」で即時登録・状態遷移させたうえでコマンド自体はすぐに応答を
//! 返し、実際の読み込み（`hakutaku_core::register_source_with_control`）は
//! [`std::thread::spawn`] したワーカースレッドで実行します。
//!
//! Tauri は `#[tauri::command]` を `async fn` にすると自動的に非同期ランタイム
//! （Tokio）上へスケジュールしますが、ここでは意図的に**同期関数のまま**にし、
//! 読み込み本体だけを生スレッドへ逃がしています。理由は次のとおりです。
//!
//! 1. 既存の読み込みパイプライン（`hakutaku_core`）は同期 API であり、
//!    `std::sync::Mutex` で保護された managed state（`TargetRegistryState`・
//!    `DisplaySetRegistryState`）を素朴に扱えます。`async fn` 化すると
//!    `.await` をまたいで `std::sync::MutexGuard` を保持できない（Send 制約）
//!    ため、`tokio::sync::Mutex` への置き換えや `spawn_blocking` の判断が
//!    別途必要になり、変更範囲が広がります
//! 2. `std::thread::spawn` は Tauri の async ランタイムの詳細（フィーチャー
//!    フラグ、ワーカースレッド数の調整）に依存せず、単体テストでも
//!    そのまま呼び出せます（後述の `run_open_core`）
//!
//! ## ロックの保持区間
//!
//! 同期 API + 生スレッド + `std::sync::Mutex` という上記の構成は維持しつつ、
//! **読み込みワーカーがレジストリのロックを保持する区間だけを分割**して
//! います。[`run_open_core`] は `hakutaku_core::register_source_with_access`
//! へ [`PerBatchRegistryLock`](crate::log_view::PerBatchRegistryLock) を渡し、
//! コアが確定したバッチを登録する瞬間だけ `DisplaySetRegistryState` の
//! `Mutex` を取り直します。ファイルの読み込み・デコード・日時解析はロックの
//! 外で行われるため、読み込み中でも `fetch_log_range` が待たされません
//! （`ENV-004`・`PERF-009`）。読み込み途中の表示集合が範囲取得から見えるのは
//! P06-2（`grow_source_items`）からの設計どおりで、世代は伸長では進まず
//! `total_items` だけが増えます。
//!
//! この経路で `register_source_with_control`（`&mut DisplaySetRegistry` を
//! 受け取る版）へロック済みのガードを渡してはいけません。読み込みが終わる
//! まで `Mutex` を手放せなくなります。設計の詳細（なぜバッチ境界か、ロック内
//! で何だけを行うか、デッドロック回避規則との整合）は
//! `hakutaku_core::register_source_with_access` の doc コメントにあります。
//!
//! なお `TargetRegistryState`（対象一覧）側のロックは従来どおり短時間の
//! 更新ごとに取得・解放しており、この分割の対象ではありません。
//!
//! ## 読み込み中の対象を閉じる・やり直す（Issue #31）
//!
//! P06-2 の逐次登録以降、**読み込み中（[`TargetStatus::Loading`]）の対象も、
//! 最初のバッチ境界でコア層へ登録済み**です（`hakutaku_core::
//! register_source_with_access` の中で `insert_source` が呼ばれます）。ソース
//! と `SourceBudget` の予約は既に生きており、統合表示集合（`LOG-007`・
//! `LOG-008`）にも参加します。一方でその `source_id` が GUI 層へ返るのは
//! 読み込みの終了時だけのため、読み込み中の対象一覧はまだ `source_id` を
//! 持ちません（[`active_source_id`] が `None` を返すのはこのためです）。
//!
//! そこで、読み込み中の対象に対する操作は次の規則で扱います。
//!
//! 1. [`close_target`] は、読み込み中なら (a) キャンセルを要求し、(b)
//!    「閉じられた」印（[`TargetRegistry::mark_close_pending`]）を付けてから
//!    一覧から除去します。ワーカーは終端処理（[`FinishLoadGuard`]）でこの印を
//!    回収し、自分が登録したソースを `close_source` します。印を介するのは、
//!    閉じる時点では `source_id` がまだ分からないためです
//! 2. [`retry_target`] は読み込み中の対象を拒否します
//!    （[`RetryTargetResponse::AlreadyLoading`]）。受け付けると同じ対象に
//!    ワーカーが2つでき、[`TargetRegistry::begin_loading`] のトークン上書きに
//!    より先発ワーカーがキャンセル不能・そのソースが解放不能になります
//! 3. [`open_config_data_source`] は、同名の対象が既に開かれている場合に
//!    新しいワーカーを作りません（[`reserve_configured_target`]）
//!
//! これらが満たす不変条件は「**ワーカーがどの時点で終わっても、対象がどの
//! 時点で閉じられても、コア側のソース・`SourceBudget` の予約・`active_loads`
//! のトークンが残らない**」ことです。
//!
//! ワーカースレッドは [`tauri::AppHandle`]（`Send + Sync + 'static`。`Clone`
//! でスレッドへ渡せる）経由で managed state を再取得し（[`run_open`]）、進捗・
//! 完了・失敗・キャンセルを Tauri イベント（`EVENT_LOAD_PROGRESS`・
//! `EVENT_LOAD_OUTCOME`）として emit します。
//!
//! [`reload_target`] はこの非同期化の対象外です（同期のまま）。
//! `hakutaku_core::reload_source` は進捗・キャンセルを受け付けない同期 API
//! であり（`tasks/phase-06-large-file-loading.md` は「対象を開く」経路
//! だけを P07-2 の非同期化対象としています）、明示的な再読み込みは短時間で
//! 終わる想定のためです。
//!
//! # イベント vs ポーリング（採用: ポーリング。理由は `src/shell.js` を参照）
//!
//! `AppHandle::emit` はフロントエンドの Capability 許可を必要としません
//! （ACL は `invoke()` 経由のコマンド呼び出しだけを制限し、Rust 側から
//! WebView へ発行するイベントは対象外です）。そのため、このモジュールは
//! 常にイベントを発行します。一方フロントエンド（`src/shell.js`）は
//! `list_targets` のポーリング（読み込み中の対象がある間だけ 500ms 間隔）を
//! 主経路として採用しており、進捗・完了状態は対象一覧
//! （[`TargetStatus::Loading`] の `progress` フィールド等）からも取得できる
//! ようにしています。採用理由の詳細は `src/shell.js` のモジュール doc
//! コメントを参照してください。
//!
//! # 対象の状態
//!
//! [`TargetStatus`] は `Loading` / `Ready` / `CancelledPartial` / `Error` の
//! 4種類です。`CancelledPartial`（P06-2 の `TaskOutcome::Cancelled` を受けて
//! 追加）は、キャンセル要求により部分読み込みで終了した状態で、
//! `retry_target`（同じパスからの再オープン）で再試行できます。
//! `tasks/phase-07-shell-ui.md` が例示する「変更済み」は、削除・縮小・置換の
//! 検知（`LOG-023`）・共有違反（`LOG-027`）・再読み込み失敗（`LOG-028`）の
//! いずれも `Error` へ集約しています（既存の `retry_target`＝同じパスでの
//! 再登録が、すべての再試行経路を兼ねられるため。専用の状態を新設すると、
//! 対応するフロントエンド表示が無いまま `default:` 分岐（`src/targets.js`）
//! の「読み込み中」表示に落ちてしまい、かえって分かりづらくなる）。上限超過
//! による再読み込み拒否（ADR-0007）だけは例外で、表示中の内容が引き続き
//! 有効なため `Ready` のまま `update_pending` を立てます（`reload_target`。
//! `CancelledPartial` は対象外で、完全な再取得は `retry_target` を使い
//! ます）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use hakutaku_core::notification::{
    CancellationToken, Progress, ProgressSink, ProgressUnit, TaskId, TaskOutcome, UserFacingError,
};
use hakutaku_diagnostics::{diag_info, diag_warn, Diagnostics};

use crate::bootstrap::config::ConfigState;
use crate::log_view::{log_load_summary, DisplaySetRegistryState, PerBatchRegistryLock};

/// 進捗イベント名（`hakutaku://load-progress`）。payload は
/// [`LoadProgressEventPayload`]。
const EVENT_LOAD_PROGRESS: &str = "hakutaku://load-progress";

/// 完了・失敗・キャンセルイベント名（`hakutaku://load-outcome`）。payload は
/// [`LoadOutcomeEventPayload`]。
const EVENT_LOAD_OUTCOME: &str = "hakutaku://load-outcome";

/// アクセス拒否（`ERROR_ACCESS_DENIED`）時の `ERR-002` 理由欄（`PRIV-002`、
/// P11-1）。共通の `error_next_action`（対象ごとに異なる汎用文言）とは別に、
/// 昇格による再試行を案内する専用の文面を使う。
const ACCESS_DENIED_REASON: &str = "アクセスが拒否されました。管理者権限で開き直すことができます。";

/// アクセス拒否時の `ERR-002` 次操作欄（`PRIV-002`、P11-1）。
const ACCESS_DENIED_NEXT_ACTION: &str =
    "「管理者として新しいウィンドウで開く」を選ぶか、対象への権限を確認してから再試行してください。";

/// 対象一覧の managed state です。
///
/// フィールドはこのクレート内の他モジュール（`crate::log_view`）からも
/// `.0.lock()` で直接触るため `pub(crate)` にしています（`TargetRegistry`
/// 自体はこのモジュールの外へ型として公開しません。呼び出し側は返り値の型を
/// 明示せずメソッド呼び出しだけで完結できます）。
#[derive(Default)]
pub struct TargetRegistryState(pub(crate) Mutex<TargetRegistry>);

/// 対象1件の由来です。`path` はいずれも Rust 側だけが保持し、フロントエンドへ
/// 構造化データとしては渡しません（`SEC-012`）。`LOG-027`（読み取れない場合の
/// 再試行）のために、選択済み・解決済みのパスをここへ保持しています。
#[derive(Debug, Clone)]
pub(crate) enum TargetOrigin {
    /// ネイティブダイアログでアドホックに選んだファイル。
    AdHoc { path: PathBuf },
    /// 設定（`hakutaku.yaml` の `data_sources`）に事前定義されたデータソース。
    Configured { name: String, path: PathBuf },
}

impl TargetOrigin {
    fn path(&self) -> &Path {
        match self {
            TargetOrigin::AdHoc { path } | TargetOrigin::Configured { path, .. } => path,
        }
    }
}

/// 読み込み中の進捗（P07-2）。バイト単位（`hakutaku_core::notification::ProgressUnit::Bytes`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LoadProgress {
    done_bytes: u64,
    /// 総量。`Progress::Indeterminate`（総量不明）の場合は `None`。
    total_bytes: Option<u64>,
}

/// 対象1件の現在の状態です（モジュール doc コメント「対象の状態」参照）。
#[derive(Debug, Clone)]
enum TargetStatus {
    /// 読み込み中。
    ///
    /// この状態でも、最初のバッチ境界を越えていればコア層にはソースが登録
    /// 済みです（モジュール doc コメント「読み込み中の対象を閉じる・やり直す
    /// （Issue #31）」）。ただし払い出された `source_id` は読み込みの終了時に
    /// しか GUI 層へ返らないため、ここでは保持できません。読み込み中に閉じた
    /// 場合の後始末は、ワーカーの終端処理（[`FinishLoadGuard`]）が
    /// [`TargetRegistry::mark_close_pending`] の印を頼りに代行します。
    Loading { progress: Option<LoadProgress> },
    /// 読み込み済み。
    Ready {
        /// コア層（`hakutaku_core::DisplaySetRegistry`）が払い出した
        /// ソース識別子。[`close_target`]・[`retry_target`]（[`active_source_id`]
        /// 経由）が `SourceBudget` の予約を解放する際の鍵、
        /// [`reload_target`]（P06-5）が `hakutaku_core::reload_source` を
        /// 呼ぶ際の鍵として使います。フロントエンドへは渡しません
        /// （`TargetStatusDto` には含めない。`SEC-012` と同じ「必要最小限
        /// だけを渡す」方針）。
        source_id: u32,
        display_set_id: u32,
        generation: u64,
        total_items: u64,
        /// `LOG-022`: 日時未解析の生表示へ退避したか。明示的な再読み込み
        /// （`LOG-028`）が表示集合を作り直した場合は、その実結果で更新します
        /// （[`update_ready_after_reload`] の doc コメント）。
        fell_back_to_raw_display: bool,
        /// 明示的な再読み込み（`LOG-028`）が上限超過で拒否され、旧
        /// スナップショットの表示を維持したまま「更新未反映」になっている
        /// か（ADR-0007）。
        update_pending: bool,
    },
    /// キャンセル要求により部分読み込みで終了した（P06-2、`TaskOutcome::Cancelled`）。
    /// 読み込み済み範囲は保持されており、`display_set_id` 等はその時点の値。
    /// `retry_target` で同じパスから再オープンして再試行できます。
    CancelledPartial {
        /// [`TargetStatus::Ready::source_id`] と同じ理由で保持します。
        /// キャンセルされた読み込みも、途中まで解析済みの範囲は既にコア層へ
        /// 登録済みで `SourceBudget` の予約も生きたままのため、
        /// [`close_target`]・[`retry_target`] が [`active_source_id`] 経由で
        /// これを解放します。
        source_id: u32,
        display_set_id: u32,
        generation: u64,
        total_items: u64,
        fell_back_to_raw_display: bool,
    },
    /// エラー（`ERR-002` の5要素を保持）。共有違反（`LOG-027`）・変更検知
    /// （`LOG-023`）・再読み込み失敗（`LOG-028`）・アクセス拒否（`PRIV-002`、
    /// P11-1）もすべてこの状態へ集約し、既存の `retry_target`（同じパスでの
    /// 再登録）で再試行できるようにしています（`crate::targets` モジュール
    /// doc コメント「対象の状態」）。
    Error {
        error: UserFacingError,
        /// `ERROR_ACCESS_DENIED` による失敗か（`PRIV-002`、P11-1）。真の場合
        /// だけ、フロントエンドは一覧に「アクセス拒否（昇格で再試行可）」を
        /// 表示し、「管理者として新しいウィンドウで開く」ボタン
        /// （[`launch_elevated`](crate::bootstrap::process::launch_elevated)
        /// を呼ぶ）を出します（`src/shell.js` 参照）。誤用防止のため、この
        /// フラグが立っている場合だけボタンを表示する設計です。
        access_denied: bool,
    },
}

/// 対象一覧の1件です。
#[derive(Debug, Clone)]
struct TargetEntry {
    target_id: u32,
    /// 表示名（アドホックはファイル名、設定由来は設定上の名前。いずれも
    /// フルパスではない。`SEC-012`）。
    display_name: String,
    origin: TargetOrigin,
    status: TargetStatus,
}

/// 対象一覧です。登録順を `Vec` で保持し、一覧表示の順序を安定させます。
#[derive(Debug, Default)]
pub(crate) struct TargetRegistry {
    next_target_id: u32,
    targets: Vec<TargetEntry>,
    /// 読み込み中の対象のキャンセルトークン（P07-2）。[`Self::begin_loading`]
    /// で読み込み開始と同時に登録し、[`Self::finish_loading`] で読み込み終了
    /// （成功・失敗・キャンセルのいずれでも）時に除去します。`cancel_load`
    /// コマンド（[`Self::request_cancel`]）はここを参照します。
    ///
    /// ここに登録があることは「その `target_id` のワーカーが走っている（か、
    /// 走り始める直前である）」ことと同義です。[`Self::mark_close_pending`] は
    /// この事実を、後始末を代行してくれる相手がいるかどうかの判断に使います。
    active_loads: HashMap<u32, CancellationToken>,
    /// 読み込み中に閉じられた対象（Issue #31）。[`close_target`] が印を付け、
    /// ワーカーの終端処理（[`FinishLoadGuard`]）が [`Self::take_close_pending`]
    /// で回収します。
    ///
    /// 対象一覧のエントリそのものは閉じた時点で消えるため、この集合が
    /// 「エントリは無いが、コア側の後始末だけが残っている `target_id`」の
    /// 記録になります。`target_id` は [`Self::register`] が単調増加で払い出し
    /// 再利用しないため、印が別の対象へ誤って効くことはありません。
    close_pending_loads: HashSet<u32>,
}

impl TargetRegistry {
    /// 対象を新規登録し、状態 `Loading`（進捗未確定）で追加します。割り当てた
    /// `target_id` を返します。
    ///
    /// これだけではキャンセル要求を受け付けられません（`cancel_load` が対象を
    /// 見つけられるようにするには [`Self::begin_loading`] も呼び出す必要が
    /// あります）。
    ///
    /// 呼び出し側は **`register` と [`Self::begin_loading`] を同じロック区間で**
    /// 続けて呼んでください（Issue #31）。分けると、その隙間に
    /// [`close_target`] が入って対象が消え、担当するエントリが無いまま
    /// ワーカーだけが走る状態（＝登録したソースを誰も回収できない状態）が
    /// 作れてしまいます。読み込みを結局開始しない経路（`open_config_data_source`
    /// のフォルダ判定など）は、[`Self::abort_loading`] で取り消します。
    pub(crate) fn register(&mut self, display_name: String, origin: TargetOrigin) -> u32 {
        let target_id = self.next_target_id;
        self.next_target_id += 1;
        self.targets.push(TargetEntry {
            target_id,
            display_name,
            origin,
            status: TargetStatus::Loading { progress: None },
        });
        target_id
    }

    /// 読み込み開始を記録します（P07-2）。状態を `Loading`（進捗未確定）へ
    /// 設定し、新しいキャンセルトークンを発行して `active_loads` へ登録し、
    /// そのクローンを返します。
    ///
    /// **読み込みスレッドを起動する前に、同期的に**（コマンドハンドラの中で）
    /// 呼び出してください。これにより、コマンドが応答を返した直後に
    /// `cancel_load` が呼ばれても（フロントエンドが `target_id` を知った時点
    /// では既にこのメソッドを呼び終えているため）確実にトークンを見つけられ、
    /// 取りこぼしません。
    ///
    /// # `None` を返す場合（Issue #31）
    ///
    /// 次のどちらかの場合は **`None` を返し、状態も `active_loads` も一切
    /// 変更しません**。いずれも「呼び出し側はワーカーを起動してはいけない」
    /// という同じ意味です。
    ///
    /// 1. その `target_id` が対象一覧に無い（既に [`close_target`] された、
    ///    または最初から存在しない）。担当するエントリが無いままワーカーを
    ///    起動すると、登録したソースを誰も回収できません
    /// 2. 同じ `target_id` のトークンが既にある（既に読み込み中）。トークンを
    ///    黙って上書きすると、
    ///    - 先発ワーカーのトークンが失われて `cancel_load` で止められなくなる
    ///    - 先に終わった側の [`Self::finish_loading`] が後発のトークンまで消す
    ///    - 同じファイルが二重にコア層へ登録され、対象一覧が知らない側の
    ///      ソース・`SourceBudget` の予約が解放不能になる
    ///
    ///    が同時に起こります
    ///
    /// 呼び出し側が対象の判定（存在確認・読み込み中判定）とこの呼び出しを
    /// 同じロック区間で行っていれば `None` にはなりません。競合に対する
    /// 最後の防御として、上書きしない側・起動しない側へ倒します。
    pub(crate) fn begin_loading(&mut self, target_id: u32) -> Option<CancellationToken> {
        if self.find(target_id).is_none() || self.active_loads.contains_key(&target_id) {
            return None;
        }
        self.set_status(target_id, TargetStatus::Loading { progress: None });
        let token = CancellationToken::new();
        self.active_loads.insert(target_id, token.clone());
        Some(token)
    }

    /// 読み込み終了を記録し、`active_loads` から除去します（成功・失敗・
    /// キャンセルのいずれでも、読み込みスレッドの最後に必ず呼びます）。
    pub(crate) fn finish_loading(&mut self, target_id: u32) {
        self.active_loads.remove(&target_id);
    }

    /// [`Self::begin_loading`] の後に「結局ワーカーを起動しない」と決めた場合の
    /// 取り消しです（`open_config_data_source`・[`retry_target`] のフォルダ
    /// 未対応判定。Issue #31）。
    ///
    /// トークンに加えて「閉じられた」印も回収します。印を残すと、回収する
    /// ワーカーがいないまま [`Self::close_pending_loads`] へ残り続けるため
    /// です（この経路ではコア層へのソース登録も起きていないので、後始末は
    /// 印の破棄だけで足ります）。
    fn abort_loading(&mut self, target_id: u32) {
        self.finish_loading(target_id);
        self.take_close_pending(target_id);
    }

    /// 読み込み中に閉じられたことを記録します（Issue #31）。記録できた場合
    /// （＝ワーカーが走っており、後始末を代行してもらえる場合）に `true` を
    /// 返します。
    ///
    /// ワーカーが走っていない場合（`active_loads` にトークンが無い場合）は
    /// 記録しません。回収する相手がいない印を残すと、`close_pending_loads`
    /// に永久に残るだけだからです。この場合、コア側にそのソースは存在しない
    /// （ワーカーが登録する前か、既に終端処理を終えている）ため、後始末も
    /// 必要ありません。
    fn mark_close_pending(&mut self, target_id: u32) -> bool {
        if !self.active_loads.contains_key(&target_id) {
            return false;
        }
        self.close_pending_loads.insert(target_id);
        true
    }

    /// [`Self::mark_close_pending`] の印を回収します（印があれば `true`）。
    /// ワーカーの終端処理から一度だけ呼びます。
    fn take_close_pending(&mut self, target_id: u32) -> bool {
        self.close_pending_loads.remove(&target_id)
    }

    /// `target_id` の読み込みにキャンセルを要求します。読み込み中でなければ
    /// （`active_loads` に無ければ）`false` を返し、何もしません
    /// （`cancel_load` コマンドの戻り値。ERR-001: 無関係な対象への影響なし）。
    pub(crate) fn request_cancel(&mut self, target_id: u32) -> bool {
        match self.active_loads.get(&target_id) {
            Some(token) => {
                token.request_cancel();
                true
            }
            None => false,
        }
    }

    /// 読み込み中の対象の進捗を更新します。対象が `Loading` 状態でない場合
    /// （既に完了・失敗・キャンセル済みの後に届いた遅延通知など）は無視します。
    fn set_progress(&mut self, target_id: u32, done_bytes: u64, total_bytes: Option<u64>) {
        if let Some(entry) = self
            .targets
            .iter_mut()
            .find(|entry| entry.target_id == target_id)
        {
            if let TargetStatus::Loading { progress } = &mut entry.status {
                *progress = Some(LoadProgress {
                    done_bytes,
                    total_bytes,
                });
            }
        }
    }

    fn set_status(&mut self, target_id: u32, status: TargetStatus) {
        if let Some(entry) = self
            .targets
            .iter_mut()
            .find(|entry| entry.target_id == target_id)
        {
            entry.status = status;
        }
    }

    fn find(&self, target_id: u32) -> Option<&TargetEntry> {
        self.targets
            .iter()
            .find(|entry| entry.target_id == target_id)
    }

    /// 対象一覧からエントリを除去するだけの下請けです。コア側の表示集合・
    /// `SourceBudget` の予約を合わせて解放する判断（[`active_source_id`] が
    /// `Some` を返す場合の `close_source` 呼び出し）と、読み込み中だった場合の
    /// キャンセル要求・「閉じられた」印は、呼び出し側の [`close_target`] が
    /// 行います（`TargetRegistry` はコア層の型を知らないため、`close_source`
    /// はここでは行えません）。
    ///
    /// `active_loads` のトークンはここでは除去しません。ワーカーが
    /// [`Self::finish_loading`] で自ら除去するまで残しておく必要があります
    /// （先に消すと、[`Self::mark_close_pending`] が「回収してくれる相手が
    /// いない」と誤判定し、Issue #31 のソース孤児化が戻ります）。
    fn remove(&mut self, target_id: u32) -> bool {
        let before = self.targets.len();
        self.targets.retain(|entry| entry.target_id != target_id);
        self.targets.len() != before
    }

    fn list(&self) -> Vec<TargetDto> {
        self.targets.iter().map(TargetDto::from).collect()
    }
}

/// 対象の由来（フロントエンド表示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetOriginDto {
    AdHoc,
    Configured,
}

/// `ERR-002` の5要素（対象・発生位置・理由・継続可否・次操作）を持つ、利用者
/// 向けエラーの DTO です。`hakutaku_core::notification::UserFacingError`
/// （P04-6）をそのまま直列化した形で、いずれのフィールドもマスキングしません
/// （`ERR-002`／`DIAG-004`）。
#[derive(Debug, Clone, Serialize)]
pub struct UserFacingErrorDto {
    pub target: String,
    pub location: Option<String>,
    pub reason: String,
    pub continuable: bool,
    pub next_action: String,
    pub error_code: Option<String>,
}

impl From<&UserFacingError> for UserFacingErrorDto {
    fn from(error: &UserFacingError) -> Self {
        UserFacingErrorDto {
            target: error.target.clone(),
            location: error.location.clone(),
            reason: error.reason.clone(),
            continuable: error.continuable,
            next_action: error.next_action.clone(),
            error_code: error.error_code.clone(),
        }
    }
}

/// [`LoadProgress`] のフロントエンド向け DTO。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct LoadProgressDto {
    pub done_bytes: u64,
    pub total_bytes: Option<u64>,
}

impl From<LoadProgress> for LoadProgressDto {
    fn from(progress: LoadProgress) -> Self {
        LoadProgressDto {
            done_bytes: progress.done_bytes,
            total_bytes: progress.total_bytes,
        }
    }
}

/// [`TargetStatus`] のフロントエンド向け DTO。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetStatusDto {
    Loading {
        progress: Option<LoadProgressDto>,
    },
    Ready {
        display_set_id: u32,
        generation: u64,
        total_items: u64,
        fell_back_to_raw_display: bool,
        /// `LOG-028`・ADR-0007: 再読み込みが上限超過で拒否され、旧
        /// スナップショットの表示を維持したまま「更新未反映」になっている
        /// か。新規追加フィールドのため、これを未対応のフロントエンドは
        /// 単に無視します（既存 JS が壊れない範囲の DTO 追加）。
        update_pending: bool,
    },
    CancelledPartial {
        display_set_id: u32,
        generation: u64,
        total_items: u64,
        fell_back_to_raw_display: bool,
    },
    Error {
        error: UserFacingErrorDto,
        /// `PRIV-002`、P11-1: [`TargetStatus::Error::access_denied`] のフロント
        /// エンド向け DTO。真の場合だけ「管理者として新しいウィンドウで開く」
        /// ボタンを表示します。
        access_denied: bool,
    },
}

impl From<&TargetStatus> for TargetStatusDto {
    fn from(status: &TargetStatus) -> Self {
        match status {
            TargetStatus::Loading { progress } => TargetStatusDto::Loading {
                progress: progress.map(LoadProgressDto::from),
            },
            TargetStatus::Ready {
                source_id: _,
                display_set_id,
                generation,
                total_items,
                fell_back_to_raw_display,
                update_pending,
            } => TargetStatusDto::Ready {
                display_set_id: *display_set_id,
                generation: *generation,
                total_items: *total_items,
                fell_back_to_raw_display: *fell_back_to_raw_display,
                update_pending: *update_pending,
            },
            TargetStatus::CancelledPartial {
                source_id: _,
                display_set_id,
                generation,
                total_items,
                fell_back_to_raw_display,
            } => TargetStatusDto::CancelledPartial {
                display_set_id: *display_set_id,
                generation: *generation,
                total_items: *total_items,
                fell_back_to_raw_display: *fell_back_to_raw_display,
            },
            TargetStatus::Error {
                error,
                access_denied,
            } => TargetStatusDto::Error {
                error: UserFacingErrorDto::from(error),
                access_denied: *access_denied,
            },
        }
    }
}

/// `list_targets` が返す一覧1件の DTO。
#[derive(Debug, Clone, Serialize)]
pub struct TargetDto {
    pub target_id: u32,
    pub display_name: String,
    pub origin: TargetOriginDto,
    /// `origin` が `configured` のときだけ、設定上の名前（`open_config_data_source`
    /// の引数と同じ値。フロントエンドが `get_config_status` の
    /// `data_source_names` と突き合わせるための鍵）。
    pub source_name: Option<String>,
    pub status: TargetStatusDto,
}

impl From<&TargetEntry> for TargetDto {
    fn from(entry: &TargetEntry) -> Self {
        let (origin, source_name) = match &entry.origin {
            TargetOrigin::AdHoc { .. } => (TargetOriginDto::AdHoc, None),
            TargetOrigin::Configured { name, .. } => {
                (TargetOriginDto::Configured, Some(name.clone()))
            }
        };
        TargetDto {
            target_id: entry.target_id,
            display_name: entry.display_name.clone(),
            origin,
            source_name,
            status: TargetStatusDto::from(&entry.status),
        }
    }
}

/// 開いている対象の一覧を返します（`CFG-003`／`PROD-006` の事前定義パスと、
/// アドホックに開いた対象の両方。設定由来だが未オープンの対象はここには
/// 含まれません。それらは `get_config_status` の `data_source_names` を
/// フロントエンド側で突き合わせて表示します）。
///
/// フロントエンド（`src/shell.js`）は、読み込み中の対象がある間はこのコマンド
/// を 500ms 間隔でポーリングし、進捗表示と完了検知の両方に使います
/// （モジュール doc コメント「イベント vs ポーリング」参照）。
#[tauri::command]
pub fn list_targets(targets: State<'_, TargetRegistryState>) -> Vec<TargetDto> {
    let target_guard = targets.0.lock().unwrap_or_else(PoisonError::into_inner);
    target_guard.list()
}

/// ログ解析プロファイルの名前一覧を返します（`LOG-022` の手動選択 UI、
/// P07-2）。`SEC-012` に反する情報（パス等）は含みません。
#[tauri::command]
pub fn list_log_profiles(config: State<'_, ConfigState>) -> Vec<String> {
    config
        .config
        .log_profiles
        .iter()
        .map(|profile| profile.name.clone())
        .collect()
}

/// 日時書式1件の選択肢です（[`list_datetime_formats`] の応答要素）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DateTimeFormatDto {
    /// 要件 ID（`LOG-DT-001` など）。再解析要求でそのまま送り返される値です。
    pub id: String,
    /// 利用者へ見せる書式パターン（`YYYY/MM/DD HH:mm:ss:SS` など）。
    pub pattern: String,
}

/// 既知の日時書式（`LOG-DT-001`〜`006`）の一覧を返します（`LOG-022` の手動
/// 選択 UI）。設定に依存しない固定の一覧です。
///
/// フロントエンドが6書式を自前の定数として持つと、書式が増減したときに
/// 更新漏れが起きます。解析側の `LogDateTimeFormat::ALL` を唯一の出所とし、
/// 表示用のパターン文字列もそこから取ります。
#[tauri::command]
pub fn list_datetime_formats() -> Vec<DateTimeFormatDto> {
    hakutaku_core::LogDateTimeFormat::ALL
        .iter()
        .map(|format| DateTimeFormatDto {
            id: format.id().to_string(),
            pattern: format.pattern().to_string(),
        })
        .collect()
}

/// 対象が現在コア層（`hakutaku_core::DisplaySetRegistry`）に生きたまま登録
/// されており、**その `source_id` を対象一覧が知っている**場合に、その
/// `source_id` を返します（P06-5）。
///
/// `Ready`・`CancelledPartial` のいずれも、`register_source_with_control` が
/// 少なくとも最初のバッチを登録済みで、`SourceBudget` の予約も生きたままの
/// 状態です。`Error` は、最初のバッチの登録前に失敗した（この場合コア側が
/// 予約を返却済み）か、登録済みのまま壊れた状態としてマークされたかの
/// どちらかで、いずれも `retry_target`・`close_target` から解放すべき
/// `source_id` を対象一覧が保持していないため `None` です。
///
/// `Loading` が `None` なのは「コア側に未登録だから」**ではありません**。
/// 読み込み中の対象も最初のバッチ境界を越えていればコア側に登録済みですが、
/// `source_id` が GUI 層へ返るのは読み込みの終了時だけのため、対象一覧が
/// それを保持できないだけです（モジュール doc コメント「読み込み中の対象を
/// 閉じる・やり直す（Issue #31）」）。読み込み中の対象を閉じたときの解放は、
/// [`close_target`] が付けた印を [`FinishLoadGuard`] が回収して代行します。
///
/// [`close_target`]・[`retry_target`] がこれを使い、新しい登録の前・対象
/// 除去の際にコア側の予約を解放します。
fn active_source_id(status: &TargetStatus) -> Option<u32> {
    match status {
        TargetStatus::Ready { source_id, .. }
        | TargetStatus::CancelledPartial { source_id, .. } => Some(*source_id),
        TargetStatus::Loading { .. } | TargetStatus::Error { .. } => None,
    }
}

/// 対象を一覧から除去します。存在しない `target_id` を渡した場合は何もせず
/// `false` を返します（`ERR-001`: 無関係な対象の操作に影響しない）。
///
/// 対象一覧が `source_id` を保持している場合（`Ready`・`CancelledPartial`。
/// [`active_source_id`]）は、コア側の表示集合と `SourceBudget` の予約も合わせて
/// 解放します（P06-5。解放しないと合計サイズ・ファイル数の上限判定
/// （`PERF-004`〜`006`）へ永久に計上され続け、統合表示集合にも閉じたはずの
/// 行が残り続けます）。
///
/// 読み込み中（`Loading`）の対象は、コア側に登録済みかどうかにかかわらず
/// `source_id` を対象一覧が知りません。そこで (a) キャンセルを要求し、(b)
/// 「閉じられた」印（[`TargetRegistry::mark_close_pending`]）を残してから
/// 一覧より除去します。実際の `close_source` は、ワーカーの終端処理
/// （[`FinishLoadGuard`]）が印を回収して代行します（Issue #31。モジュール
/// doc コメント「読み込み中の対象を閉じる・やり直す（Issue #31）」）。
///
/// キャンセルを代行するのは、閉じた対象のためにワーカーが GB 級ファイルを
/// 読み続け、その成果が誰からも解放できない形で残るのを防ぐためです（以前は
/// 「一覧からの除去とキャンセルは独立した操作」としてあえて代行していません
/// でしたが、コア側に登録済みのまま孤児化する経路が生まれたため見直しました）。
///
/// `Error` 状態の対象は [`active_source_id`] が `None` を返し、ワーカーも
/// 走っていないため、一覧からの除去だけで足ります。
#[tauri::command]
pub fn close_target(
    target_id: u32,
    targets: State<'_, TargetRegistryState>,
    registry: State<'_, DisplaySetRegistryState>,
    budget: State<'_, hakutaku_core::SourceBudget>,
) -> bool {
    close_target_core(&targets.0, &registry.0, budget.inner(), target_id)
}

/// [`close_target`] の中核処理です。[`run_open_core`]・[`run_reload_core`] と
/// 同じ理由（単体テスト容易性。`tauri::State` は動作する Tauri アプリなしには
/// 構築できない）で、素の `Mutex`・`SourceBudget` を直接受け取ります。
fn close_target_core(
    targets: &Mutex<TargetRegistry>,
    display_set_registry: &Mutex<hakutaku_core::DisplaySetRegistry>,
    budget: &hakutaku_core::SourceBudget,
    target_id: u32,
) -> bool {
    // ロックの順序は「対象一覧 → レジストリ」（既存の close_target・
    // retry_target と同じ。逆順で取る経路を作らない）。
    let mut target_guard = targets.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(entry) = target_guard.find(target_id) else {
        return false;
    };
    let source_id = active_source_id(&entry.status);
    let is_loading = matches!(entry.status, TargetStatus::Loading { .. });

    if is_loading {
        // 印とキャンセルは同じロック区間で行う。ここでロックを手放すと、
        // ワーカーが終端処理（印の回収）を済ませた後に印を付けてしまい、
        // 誰も回収しないまま残る。
        target_guard.request_cancel(target_id);
        target_guard.mark_close_pending(target_id);
    }

    if let Some(source_id) = source_id {
        let mut registry_guard = display_set_registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        registry_guard.close_source(source_id, budget);
    }
    target_guard.remove(target_id)
}

/// 読み込み中の対象にキャンセルを要求します（P07-2）。対象が読み込み中
/// でなければ（既に完了・失敗・キャンセル済み、または存在しない場合）
/// `false` を返し、何もしません。
///
/// キャンセルはチャンク境界で確認されるため（`hakutaku_core` の
/// `CancellationToken` の規約）、要求してから実際に状態が
/// `cancelled_partial` へ遷移するまでには短い遅延があります。フロントエンド
/// はポーリングでこの遷移を検出します。
#[tauri::command]
pub fn cancel_load(target_id: u32, targets: State<'_, TargetRegistryState>) -> bool {
    let mut target_guard = targets.0.lock().unwrap_or_else(PoisonError::into_inner);
    target_guard.request_cancel(target_id)
}

/// 進捗イベント（`EVENT_LOAD_PROGRESS`）の payload です。
#[derive(Debug, Clone, Serialize)]
struct LoadProgressEventPayload {
    target_id: u32,
    done_bytes: u64,
    total_bytes: Option<u64>,
}

/// 完了・失敗・キャンセルイベント（`EVENT_LOAD_OUTCOME`）の payload です。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum LoadOutcomeEventPayload {
    Completed {
        target_id: u32,
    },
    Cancelled {
        target_id: u32,
    },
    Failed {
        target_id: u32,
        error: UserFacingErrorDto,
        /// `PRIV-002`、P11-1: [`TargetStatus::Error::access_denied`] と同じ値。
        access_denied: bool,
    },
}

/// [`ProgressSink`] の実装（P07-2）。対象一覧の進捗フィールドを更新しつつ、
/// `emit_progress` コールバック（本体は Tauri イベント発行、単体テストでは
/// 記録用クロージャ）を呼びます。
///
/// `AppHandle` へ直接依存させていない理由は [`run_open_core`] の doc
/// コメントを参照してください（単体テストで `AppHandle` を必要としないための
/// 設計）。
struct TargetProgressSink<'a> {
    targets: &'a Mutex<TargetRegistry>,
    target_id: u32,
    /// `Send + Sync` を要求するのは、[`ProgressSink`]（`P04-6` の契約）自体が
    /// `Send + Sync` を要求するためです（複数スレッドから安全に共有できる
    /// 進捗通知の受け口という契約）。
    emit_progress: &'a (dyn Fn(LoadProgressEventPayload) + Send + Sync),
}

impl ProgressSink for TargetProgressSink<'_> {
    fn report(&self, _task_id: TaskId, progress: Progress) {
        let (done_bytes, total_bytes) = match progress {
            Progress::Determinate { done, total, unit } => {
                debug_assert_eq!(unit, ProgressUnit::Bytes, "読み込み進捗はバイト単位のはず");
                (done, Some(total))
            }
            Progress::Indeterminate { done, unit } => {
                debug_assert_eq!(unit, ProgressUnit::Bytes, "読み込み進捗はバイト単位のはず");
                (done, None)
            }
        };

        {
            let mut guard = self.targets.lock().unwrap_or_else(PoisonError::into_inner);
            guard.set_progress(self.target_id, done_bytes, total_bytes);
        }

        (self.emit_progress)(LoadProgressEventPayload {
            target_id: self.target_id,
            done_bytes,
            total_bytes,
        });
    }
}

/// 対象1件の読み込み要求（[`spawn_open`] / [`run_open`] / [`run_open_core`]
/// が共有する入力）。
pub(crate) struct OpenRequest {
    pub target_id: u32,
    pub path: PathBuf,
    pub source_label: String,
    /// `LOG-022` の手動プロファイル選択（P07-2）。`None` なら自動解決。
    pub manual_profile: Option<String>,
    /// `LOG-022` の手動書式選択。要件 ID の文字列
    /// （`LOG-DT-001` など。[`list_datetime_formats`] が返す `id`）。`None`
    /// なら書式の手動指定なし。既知の6書式以外の値の扱いは
    /// [`manual_datetime_format_of`] を参照してください。
    pub manual_datetime_format: Option<String>,
    pub module: &'static str,
    pub operation: &'static str,
    /// 失敗時に `UserFacingError::target` へ設定する文字列（`ERR-002` により
    /// フルパスを含めてよい）。
    pub error_target: String,
    pub error_next_action: &'static str,
}

/// [`OpenRequest::manual_datetime_format`]（要件 ID の文字列）を解析側の書式へ
/// 変換します。
///
/// 既知の6書式以外の値は `None`（書式の手動指定なし）として扱い、読み込み
/// 自体は続行します。この値の選択肢は [`list_datetime_formats`] が返すため、
/// 一致しない ID はフロントエンドの不整合か外部からの不正な要求であり、
/// 利用者の操作ミスではありません。そのため利用者向けエラーにはせず、診断
/// ログへ警告を残すだけに留めます（[`retry_target`] に存在しないプロファイル
/// 名を渡した場合＝`ManualNotFound` と同じ扱い。読み込みは成功し、対象は
/// 安全側の表示へ落ちます）。
///
/// 「安全側」の中身は両者で異なりますが、いずれも推測でどれかの書式へ
/// 寄せない点が共通です。書式の手動指定が無い状態は自動判定であり、自動判定
/// は決められない入力を `Ambiguous`＝生表示退避にするため（`LOG-022`）、
/// 利用者は再解析 UI が出たままの状態から選び直せます。
fn manual_datetime_format_of(
    request: &OpenRequest,
    diagnostics: &Diagnostics,
) -> Option<hakutaku_core::LogDateTimeFormat> {
    let requested = request.manual_datetime_format.as_deref()?;
    let format = hakutaku_core::LogDateTimeFormat::from_id(requested);
    if format.is_none() {
        diag_warn!(
            diagnostics,
            module = request.module,
            operation = request.operation,
            "未知の日時書式 ID が指定されたため、書式の手動指定を無視します: {requested}"
        );
    }
    format
}

/// 対象の読み込みをバックグラウンドスレッドで実行します（P07-2）。
///
/// 呼び出し側（`open_log_file` 等のコマンドハンドラ）は、この関数を呼ぶ**前**
/// に対象を登録し [`TargetRegistry::begin_loading`] まで済ませておく必要が
/// あります（`cancel_load` との競合を避ける設計。`TargetRegistry::begin_loading`
/// の doc コメント参照）。
pub(crate) fn spawn_open(app: AppHandle, request: OpenRequest) {
    std::thread::spawn(move || run_open(&app, request));
}

/// [`spawn_open`] が起動するワーカースレッドの本体です。`AppHandle` から
/// managed state を再取得し、[`run_open_core`]（`AppHandle` 非依存の中核
/// 処理）へ委譲します。
fn run_open(app: &AppHandle, request: OpenRequest) {
    let targets_state = app.state::<TargetRegistryState>();
    let registry_state = app.state::<DisplaySetRegistryState>();
    let diagnostics_state = app.state::<Arc<Diagnostics>>();
    let budget_state = app.state::<hakutaku_core::SourceBudget>();
    let config_state = app.state::<ConfigState>();
    // P11-3（PERF-014／CFG-024）: 全対象で共有する抑制の接続点
    // （同時実行数の上限・I/O 発行間隔）。lib.rs::run が設定値から一度だけ
    // 構築し managed state として載せている。
    let throttle_state = app.state::<hakutaku_data_source::IoThrottle>();

    let diagnostics: &Diagnostics = diagnostics_state.inner();
    let budget: &hakutaku_core::SourceBudget = budget_state.inner();
    let throttle: &hakutaku_data_source::IoThrottle = throttle_state.inner();

    run_open_core(
        &targets_state.0,
        &registry_state.0,
        budget,
        diagnostics,
        &config_state.config.log_profiles,
        throttle,
        hakutaku_data_source::DEFAULT_CHUNK_BYTES,
        &request,
        &|payload| {
            let _ = app.emit(EVENT_LOAD_PROGRESS, payload);
        },
        &|payload| {
            let _ = app.emit(EVENT_LOAD_OUTCOME, payload);
        },
    );
}

/// 読み込みワーカーの終端処理をまとめた型です（Issue #31）。
///
/// ワーカーは終わり方（完了・キャンセル・失敗）によらず、次の2つを必ず
/// 行う必要があります。
///
/// 1. `active_loads` のキャンセルトークンを除去する
///    （[`TargetRegistry::finish_loading`]）
/// 2. 読み込み中に対象が閉じられていた場合（[`TargetRegistry::mark_close_pending`]
///    の印がある場合）、自分が登録したコア側のソースを `close_source` し、
///    `SourceBudget` の予約を解放する
///
/// 2 をワーカー側で行うのは、[`close_target`] の時点では `source_id` がまだ
/// 対象一覧に無いためです。ワーカーは読み込みの終了時に必ず `source_id` を
/// 知るので、そこが唯一の確実な回収点になります。
///
/// [`Self::finish`] を呼ばずに破棄された場合（読み込み中のパニックによる
/// 巻き戻し）でも `Drop` で同じ後始末を試みます。その場合 `source_id` が
/// 未設定でコア側のソースまでは回収できないことがありますが、少なくとも
/// `active_loads` にトークンが残り続けること（＝閉じた後もキャンセル可能な
/// 読み込みとして参照され続けること）は防げます。
struct FinishLoadGuard<'a> {
    targets: &'a Mutex<TargetRegistry>,
    display_set_registry: &'a Mutex<hakutaku_core::DisplaySetRegistry>,
    budget: &'a hakutaku_core::SourceBudget,
    target_id: u32,
    /// コア層が払い出したソース識別子。読み込みの結果が得られるまで `None`。
    source_id: Option<u32>,
    /// [`Self::finish`] 済みか（`Drop` で二重に走らせないための印）。
    finished: bool,
}

impl<'a> FinishLoadGuard<'a> {
    fn new(
        targets: &'a Mutex<TargetRegistry>,
        display_set_registry: &'a Mutex<hakutaku_core::DisplaySetRegistry>,
        budget: &'a hakutaku_core::SourceBudget,
        target_id: u32,
    ) -> Self {
        FinishLoadGuard {
            targets,
            display_set_registry,
            budget,
            target_id,
            source_id: None,
            finished: false,
        }
    }

    /// コア層が払い出したソース識別子を記録します（読み込みの結果が
    /// 得られた時点で呼びます）。
    fn set_source_id(&mut self, source_id: u32) {
        self.source_id = Some(source_id);
    }

    /// 終端処理を実行します。読み込み中に対象が閉じられていた場合は `true` を
    /// 返します（この場合、呼び出し側は完了・失敗イベントを発行しません。
    /// 宛先の対象が既に一覧から消えているためです）。
    fn finish(mut self) -> bool {
        self.finished = true;
        self.reclaim()
    }

    fn reclaim(&mut self) -> bool {
        // 対象一覧のロックを先に手放してからレジストリのロックを取る
        // （「対象一覧 → レジストリ」というロック順序を守りつつ、入れ子に
        // しない）。印を取り切ってからレジストリを触るため、この間に別経路が
        // 同じ source_id を閉じることはない（対象一覧のエントリは既に無く、
        // source_id を知っているのはこのワーカーだけ）。
        let closed = {
            let mut target_guard = self.targets.lock().unwrap_or_else(PoisonError::into_inner);
            target_guard.finish_loading(self.target_id);
            target_guard.take_close_pending(self.target_id)
        };
        if !closed {
            return false;
        }
        if let Some(source_id) = self.source_id {
            let mut registry_guard = self
                .display_set_registry
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            registry_guard.close_source(source_id, self.budget);
        }
        true
    }
}

impl Drop for FinishLoadGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.reclaim();
        }
    }
}

/// [`run_open`] の中核処理です。`AppHandle` に依存せず、素の `Mutex`・
/// `SourceBudget`・`Diagnostics` を直接受け取ります。
///
/// この形にしている理由は単体テスト容易性のためです。`tauri::AppHandle` は
/// 実際に動作する Tauri アプリ（`tauri::test::mock_app` 等）がないと
/// 構築できませんが、この関数はそれを必要とせず、プレーンな構造体だけで
/// 「読み込み開始 → `register_source_with_control` 呼び出し → 対象一覧への
/// 反映 → イベント発行」という一連の処理を検証できます。
#[allow(clippy::too_many_arguments)]
fn run_open_core(
    targets: &Mutex<TargetRegistry>,
    display_set_registry: &Mutex<hakutaku_core::DisplaySetRegistry>,
    budget: &hakutaku_core::SourceBudget,
    diagnostics: &Diagnostics,
    log_profiles: &[hakutaku_config::LogProfileConfig],
    throttle: &hakutaku_data_source::IoThrottle,
    // 1チャンクあたりのバイト数（PERF-014 の接続点の一部。実運用では常に
    // hakutaku_data_source::DEFAULT_CHUNK_BYTES）。テスト専用に小さい値を
    // 注入できるようにし、IoThrottle の同時実行数の上限が複数対象をまたいで
    // 効くことを、実際のチャンク境界越しの待機として観測できるようにしている
    // （run_open_core_shares_io_throttle_across_concurrent_targets_and_limits_concurrency
    // 参照）。
    chunk_bytes: u64,
    request: &OpenRequest,
    emit_progress: &(dyn Fn(LoadProgressEventPayload) + Send + Sync),
    emit_outcome: &dyn Fn(LoadOutcomeEventPayload),
) {
    // 呼び出し側が begin_loading 済みであることを前提とするが、念のため
    // ここでも取得を試みる（テストの直接呼び出しなど、begin_loading を
    // 呼び忘れた場合の防御。その場合は真新しいトークンが使われる）。
    let cancellation = {
        let mut guard = targets.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.active_loads.get(&request.target_id).cloned() {
            Some(token) => token,
            // 同じロック区間でトークンが無いことを確認済みなので、
            // begin_loading は必ず新しいトークンを返す（`None` は「既に
            // 読み込み中」の意味であり、ここでは起こらない）。到達しない側は
            // 真新しいトークン（＝キャンセルされていない状態）で続行する。
            None => guard.begin_loading(request.target_id).unwrap_or_default(),
        }
    };

    // 終端処理（Issue #31）。`finish` を呼ばずにこの関数を抜けた場合
    // （読み込み中のパニックによる巻き戻し）でも Drop が同じ後始末を試みる。
    let mut finish_guard =
        FinishLoadGuard::new(targets, display_set_registry, budget, request.target_id);

    let sink = TargetProgressSink {
        targets,
        target_id: request.target_id,
        emit_progress,
    };

    let control = hakutaku_core::LoadControl {
        task_id: TaskId::generate(),
        progress: Some(&sink),
        cancellation: Some(&cancellation),
        manual_profile: request.manual_profile.as_deref(),
        // UI で選んだ日時書式。未知の ID は無視される（診断ログへ
        // 警告のみ。manual_datetime_format_of の doc コメント参照）。
        manual_datetime_format: manual_datetime_format_of(request, diagnostics),
        // P11-3（PERF-014／CFG-024）: 同時実行数の上限・I/O 発行間隔の
        // 接続点。全対象で1つの IoThrottle インスタンスを共有するため、複数の
        // 対象を同時に開いても、実際にチャンク読み込みを行っている対象の数が
        // `parse_concurrency` を超えない（`crates/data-source::chunk` の
        // モジュール doc コメント「同時実行数の上限」を参照）。
        throttle: throttle.clone(),
        chunk_bytes,
        ..hakutaku_core::LoadControl::none()
    };

    // 読み込みの間ずっとレジストリのロックを保持せず、コアが確定
    // したバッチを登録する瞬間だけ取り直す（[`PerBatchRegistryLock`]）。ここで
    // `register_source_with_control`（`&mut DisplaySetRegistry` を受け取る版）
    // を呼ぶと、読み込みが終わるまで `fetch_log_range` が止まる。
    let register_result = hakutaku_core::register_source_with_access(
        &mut PerBatchRegistryLock::new(display_set_registry),
        budget,
        &request.path,
        request.source_label.clone(),
        log_profiles,
        &control,
    );

    let event = match register_result {
        Ok(register_outcome) => {
            // 読み込み中に閉じられていた場合にコア側の後始末を代行できるよう、
            // 払い出された source_id を終端処理へ渡す（Issue #31）。`Err` の
            // 場合はコア層が予約を返却済みでソースも残らないため不要。
            finish_guard.set_source_id(register_outcome.handle.source_id);
            diag_info!(
                diagnostics,
                module = request.module,
                operation = request.operation,
                "対象を読み込みました: 行数={}, バイト数={}, 予約量={} バイト",
                register_outcome.summary.line_count,
                register_outcome.summary.file_size_bytes,
                register_outcome.summary.reserved_bytes
            );
            log_load_summary(
                diagnostics,
                request.module,
                request.operation,
                &request.source_label,
                &register_outcome.summary,
            );
            apply_register_outcome(targets, request.target_id, register_outcome)
        }
        Err(error) => {
            diag_warn!(
                diagnostics,
                module = request.module,
                operation = request.operation,
                "対象を読み込めませんでした: {error}"
            );
            // PRIV-002・P11-1: 初回オープン失敗（このブロック）でのアクセス
            // 拒否だけを判定する。register_source_with_control が最初の
            // バッチを登録済みの段階（apply_register_outcome の
            // TaskOutcome::Failed）で ERROR_ACCESS_DENIED が新たに発生する
            // ことは実務上想定しない（アクセス拒否はファイルを開く時点
            // （open_and_snapshot）でしか起こらないため）。
            let access_denied = hakutaku_core::is_access_denied(&error);
            let user_error = if access_denied {
                UserFacingError::new(
                    request.error_target.clone(),
                    ACCESS_DENIED_REASON,
                    ACCESS_DENIED_NEXT_ACTION,
                )
            } else {
                UserFacingError::new(
                    request.error_target.clone(),
                    error.to_string(),
                    request.error_next_action,
                )
            };
            {
                let mut guard = targets.lock().unwrap_or_else(PoisonError::into_inner);
                guard.set_status(
                    request.target_id,
                    TargetStatus::Error {
                        error: user_error.clone(),
                        access_denied,
                    },
                );
            }
            LoadOutcomeEventPayload::Failed {
                target_id: request.target_id,
                error: UserFacingErrorDto::from(&user_error),
                access_denied,
            }
        }
    };

    if finish_guard.finish() {
        // 読み込み中に閉じられた対象。コア側のソースと予約は終端処理が回収
        // 済みで、対象一覧にもエントリが無い。完了・失敗イベントの宛先も
        // 無いため、ここで終える（Issue #31）。
        return;
    }

    emit_outcome(event);
}

/// [`hakutaku_core::RegisterSourceOutcome`] を対象一覧の状態へ反映し、
/// イベント payload を組み立てます。
fn apply_register_outcome(
    targets: &Mutex<TargetRegistry>,
    target_id: u32,
    outcome: hakutaku_core::RegisterSourceOutcome,
) -> LoadOutcomeEventPayload {
    let fell_back_to_raw_display = outcome.summary.fell_back_to_raw_display;
    let mut guard = targets.lock().unwrap_or_else(PoisonError::into_inner);
    match outcome.outcome {
        TaskOutcome::Completed => {
            guard.set_status(
                target_id,
                TargetStatus::Ready {
                    source_id: outcome.handle.source_id,
                    display_set_id: outcome.handle.display_set_id,
                    generation: outcome.handle.generation,
                    total_items: outcome.handle.total_items,
                    fell_back_to_raw_display,
                    // 新規に開いた（開き直した）対象なので、更新未反映
                    // （ADR-0007）は常に false（reload_target だけが立てる）。
                    update_pending: false,
                },
            );
            LoadOutcomeEventPayload::Completed { target_id }
        }
        TaskOutcome::Cancelled => {
            guard.set_status(
                target_id,
                TargetStatus::CancelledPartial {
                    source_id: outcome.handle.source_id,
                    display_set_id: outcome.handle.display_set_id,
                    generation: outcome.handle.generation,
                    total_items: outcome.handle.total_items,
                    fell_back_to_raw_display,
                },
            );
            LoadOutcomeEventPayload::Cancelled { target_id }
        }
        TaskOutcome::Failed(error) => {
            // PRIV-002・P11-1: ここは最初のバッチを登録済みの段階での失敗
            // （run_open_core モジュール内コメント参照）であり、アクセス拒否
            // は判定しない（常に false）。
            guard.set_status(
                target_id,
                TargetStatus::Error {
                    error: error.clone(),
                    access_denied: false,
                },
            );
            LoadOutcomeEventPayload::Failed {
                target_id,
                error: UserFacingErrorDto::from(&error),
                access_denied: false,
            }
        }
    }
}

/// フォルダを指定された場合の明確な「未対応」エラーを組み立てます
/// （計画書 P07-1「フォルダは『未対応』の明確なエラー」）。フォルダ走査
/// そのもの（`DCM-009`／`DCM-013`）は P14 の対象です。
fn folder_unsupported_error(display_name: &str, path: &Path) -> UserFacingError {
    UserFacingError::new(
        format!("{display_name}（{}）", path.display()),
        "フォルダの読み込みは現在未対応です。",
        "ファイルを個別に選択して開いてください（フォルダの走査は今後のリリースで対応予定です）。",
    )
}

/// `open_config_data_source` の応答です。
///
/// 読み込みは非同期化されているため（P07-2）、`Opened` は返さず、読み込み中で
/// あることだけを即時に応答します。実際の成否はフロントエンドが
/// `list_targets` のポーリングで検出します（モジュール doc コメント参照）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpenConfigDataSourceResponse {
    Loading {
        target_id: u32,
        source_label: String,
    },
    /// 同じ名前の対象が既に開かれている（読み込み中または読み込み済み）ため、
    /// 新しい読み込みを開始しなかった（Issue #31。[`reserve_configured_target`]）。
    ///
    /// `target_id` は既存の対象のもので、フロントエンドはそれへ切り替えれば
    /// 済みます（新しい行・新しいタブは増えません）。
    AlreadyOpen {
        target_id: u32,
        source_label: String,
    },
    Failed {
        /// 名前解決自体に失敗した場合（設定に存在しない名前）は対象を
        /// 登録しないため `None`。
        target_id: Option<u32>,
        error: UserFacingErrorDto,
    },
}

/// [`reserve_configured_target`] の結果です（Issue #31）。
enum ConfiguredTargetSlot {
    /// 同名の対象が既に開かれていた。新しいワーカーを起動してはいけません。
    AlreadyOpen(u32),
    /// 新しく登録した。呼び出し側が読み込みを開始します。
    Registered(u32),
}

/// 設定由来の対象を開く前に、同名の対象が既に開かれていないかを判定し、
/// 開かれていなければ新規登録します（Issue #31）。
///
/// 判定と登録を**1回のロック区間で**行うのが要点です。分けると、同じ名前への
/// 連続した要求（フロントエンドの二重送信防止をすり抜けた場合など）が両方とも
/// 「未登録」と判定し、同じファイルに対してワーカーが2つ走ります。すると
/// 一方のソースはどの対象エントリからも参照されないまま `SourceBudget` の
/// 予約ごと解放不能になります（`PERF-004`〜`006` の上限判定へ永久計上）。
///
/// 「既に開かれている」とみなすのは `Loading`（読み込み中）と `Ready`
/// （読み込み済み）です。`Error`・`CancelledPartial` は開き直す意図が明確な
/// 状態であり、既存エントリのやり直しは [`retry_target`] が担当するため、
/// ここでは新しい対象として登録します。
///
/// 新規登録した場合は [`TargetRegistry::begin_loading`] まで済ませます（同じ
/// 理由で、登録とキャンセル受付開始の間にもロックの切れ目を作らないため）。
/// 呼び出し側は、結局ワーカーを起動しないと決めた場合
/// （フォルダ未対応など）に [`TargetRegistry::abort_loading`] を呼びます。
fn reserve_configured_target(
    registry: &mut TargetRegistry,
    name: &str,
    path: &Path,
) -> ConfiguredTargetSlot {
    let already_open = registry.targets.iter().find(|entry| {
        matches!(
            &entry.origin,
            TargetOrigin::Configured { name: opened, .. } if opened == name
        ) && matches!(
            entry.status,
            TargetStatus::Loading { .. } | TargetStatus::Ready { .. }
        )
    });
    if let Some(entry) = already_open {
        return ConfiguredTargetSlot::AlreadyOpen(entry.target_id);
    }
    let target_id = registry.register(
        name.to_string(),
        TargetOrigin::Configured {
            name: name.to_string(),
            path: path.to_path_buf(),
        },
    );
    // 登録とキャンセル受付開始も同じロック区間で済ませる
    // （[`TargetRegistry::register`] の doc コメント）。登録した直後なので
    // `None` にはならない。`debug_assert!` は式を消さないので、release
    // ビルドでも `begin_loading` は実行される。
    let token = registry.begin_loading(target_id);
    debug_assert!(
        token.is_some(),
        "登録した直後の対象は必ず読み込みを開始できるはず"
    );
    ConfiguredTargetSlot::Registered(target_id)
}

/// 設定（`hakutaku.yaml` の `data_sources`）に事前定義されたデータソースを、
/// 名前で参照して開きます（`CFG-003`／`PROD-006`）。
///
/// `SEC-012` により、フロントエンドからパスは渡されません。名前から
/// `ConfigState` 側でパスを解決します。フォルダは現段階では未対応のため、
/// 明確なエラーとして返します（複数ファイルの同時読み込みは P06、DICOM の
/// フォルダ走査対応は P14）。
///
/// 同じ名前の対象が既に開かれている（読み込み中または読み込み済み）場合は、
/// 新しい読み込みを開始せず [`OpenConfigDataSourceResponse::AlreadyOpen`] を
/// 返します（Issue #31。[`reserve_configured_target`]）。
///
/// `manual_profile`（`LOG-022`、P07-2）を指定すると、そのプロファイル名で
/// 開き直します（`hakutaku_core::resolve_profile` の第1段階）。
/// `manual_datetime_format`（`LOG-022`）を指定すると、その日時書式
/// で解析します。通常の初回オープンではいずれも `None` を渡します。
#[tauri::command]
pub fn open_config_data_source(
    name: String,
    manual_profile: Option<String>,
    manual_datetime_format: Option<String>,
    app: AppHandle,
    targets: State<'_, TargetRegistryState>,
    diagnostics: State<'_, Arc<Diagnostics>>,
    config: State<'_, ConfigState>,
) -> OpenConfigDataSourceResponse {
    let diagnostics_ref: &Diagnostics = diagnostics.inner();

    let Some(data_source) = config
        .config
        .data_sources
        .iter()
        .find(|candidate| candidate.name == name)
    else {
        diag_warn!(
            diagnostics_ref,
            module = "targets",
            operation = "target.open_configured",
            "設定に存在しないデータソース名が要求されました: {name}"
        );
        let error = UserFacingError::new(
            name.clone(),
            "設定にこの名前のデータソースが見つかりません。",
            "参照対象一覧を更新するか、アプリを再起動してください。",
        );
        return OpenConfigDataSourceResponse::Failed {
            target_id: None,
            error: UserFacingErrorDto::from(&error),
        };
    };
    let path = data_source.path.clone();

    let target_id = {
        let mut target_guard = targets.0.lock().unwrap_or_else(PoisonError::into_inner);
        match reserve_configured_target(&mut target_guard, &name, &path) {
            ConfiguredTargetSlot::AlreadyOpen(target_id) => {
                diag_info!(
                    diagnostics_ref,
                    module = "targets",
                    operation = "target.open_configured",
                    "データソース \"{name}\" は既に開いているため、読み込みを開始しません\
                     （Issue #31）: target_id={target_id}"
                );
                return OpenConfigDataSourceResponse::AlreadyOpen {
                    target_id,
                    source_label: name,
                };
            }
            ConfiguredTargetSlot::Registered(target_id) => target_id,
        }
    };

    if path.is_dir() {
        diag_warn!(
            diagnostics_ref,
            module = "targets",
            operation = "target.open_configured",
            "データソース \"{name}\" はフォルダです（未対応）: {}",
            path.display()
        );
        let error = folder_unsupported_error(&name, &path);
        let mut target_guard = targets.0.lock().unwrap_or_else(PoisonError::into_inner);
        // ワーカーを起動しないので、reserve_configured_target が始めた
        // 読み込み受付を取り消す（Issue #31）。
        target_guard.abort_loading(target_id);
        target_guard.set_status(
            target_id,
            TargetStatus::Error {
                error: error.clone(),
                access_denied: false,
            },
        );
        return OpenConfigDataSourceResponse::Failed {
            target_id: Some(target_id),
            error: UserFacingErrorDto::from(&error),
        };
    }

    spawn_open(
        app,
        OpenRequest {
            target_id,
            path: path.clone(),
            source_label: name.clone(),
            manual_profile,
            manual_datetime_format,
            module: "targets",
            operation: "target.open_configured",
            error_target: format!("{name}（{}）", path.display()),
            error_next_action: "再試行するか、設定を確認してください。",
        },
    );

    OpenConfigDataSourceResponse::Loading {
        target_id,
        source_label: name,
    }
}

/// `retry_target` の応答です（P07-2 により非同期化。`open_config_data_source`
/// と同じ理由で `Opened` ではなく `Loading` を返します）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetryTargetResponse {
    Loading {
        target_id: u32,
    },
    Failed {
        target_id: u32,
        error: UserFacingErrorDto,
    },
    /// `target_id` が一覧に存在しない（`close_target` 済みなど）。
    NotFound,
    /// 対象が現在読み込み中のため、再試行を受け付けなかった（Issue #31）。
    ///
    /// `reload_target` が `Ready` 以外を拒否するのと対称の判定です。読み込み
    /// 中の再試行を受け付けると、同じ対象に対してワーカーが2つ走り、
    /// [`TargetRegistry::begin_loading`] の doc コメントに挙げた3つの不具合
    /// （先発ワーカーがキャンセル不能・トークンの早すぎる除去・二重登録した
    /// ソースの解放不能）が同時に起こります。
    ///
    /// 読み込み中の対象に対する利用者の選択肢は「キャンセル」（`cancel_load`）
    /// か「閉じる」（`close_target`）であり、フロントエンドも読み込み中の行に
    /// 再試行ボタンを出しません。したがってこの応答は、二重送信や競合に対する
    /// 防御としてだけ返ります。
    AlreadyLoading {
        target_id: u32,
    },
}

/// 再試行の実行計画（対象一覧の Mutex を保持したまま長時間ロックしないよう、
/// 必要な値をコピーしてから読み込み処理へ渡すための内部構造体）。
struct RetryPlan {
    path: PathBuf,
    display_name: String,
    module: &'static str,
    operation: &'static str,
    error_target: String,
    error_next_action: &'static str,
    is_configured_folder: bool,
    /// 再試行前に対象が `Ready`／`CancelledPartial` だった場合の旧
    /// `source_id`（[`active_source_id`]。`close_source` で `SourceBudget` の
    /// 予約を解放するために使う。通常は `None`。「対象一覧の状態はエラー・
    /// キャンセル時にだけ再試行できる」という運用上の想定では `Ready` の
    /// まま再試行されることは稀ですが、`CancelledPartial` からの再試行では
    /// 必ず `Some` になります）。
    previous_source_id: Option<u32>,
}

/// [`begin_retry`] の結果です（[`RetryTargetResponse`] のうち、対象一覧だけで
/// 決まる3通り）。
enum RetryDecision {
    /// `target_id` が一覧に存在しない。
    NotFound,
    /// 読み込み中のため再試行できない（Issue #31）。
    AlreadyLoading,
    /// 再試行してよい（[`TargetRegistry::begin_loading`] まで済んでいる）。
    Proceed(RetryPlan),
}

/// [`retry_target`] が対象一覧のロック区間で行う判定と、読み込み受付の開始
/// です。ロックを保持したまま読み込みへ進まないよう、必要な値をここで
/// [`RetryPlan`] へコピーします。
///
/// 次の3つを**1回のロック区間**で行うのが要点です（Issue #31）。
///
/// 1. 対象の存在確認
/// 2. 読み込み中（`Loading`）の拒否（[`RetryDecision::AlreadyLoading`]。理由は
///    [`RetryTargetResponse::AlreadyLoading`] の doc コメント）
/// 3. [`TargetRegistry::begin_loading`]（キャンセル受付の開始）
///
/// 分けると、その隙間に [`close_target`] が入って対象が消え、担当する
/// エントリが無いままワーカーだけが走る状態（＝登録したソースを誰も回収
/// できない状態）が作れてしまいます。
///
/// [`RetryDecision::Proceed`] を返したのにワーカーを起動しない場合
/// （フォルダ未対応）は、呼び出し側が [`TargetRegistry::abort_loading`] で
/// 取り消します。
fn begin_retry(registry: &mut TargetRegistry, target_id: u32) -> RetryDecision {
    let Some(entry) = registry.find(target_id) else {
        return RetryDecision::NotFound;
    };
    if matches!(entry.status, TargetStatus::Loading { .. }) {
        return RetryDecision::AlreadyLoading;
    }
    // begin_loading は状態を `Loading` へ変えるため、旧 source_id はその前に
    // 取り出しておく（`Ready`／`CancelledPartial` からの再試行で必要）。
    let previous_source_id = active_source_id(&entry.status);
    let path = entry.origin.path().to_path_buf();
    let display_name = entry.display_name.clone();
    let plan = match &entry.origin {
        TargetOrigin::AdHoc { .. } => RetryPlan {
            error_target: path.display().to_string(),
            path,
            display_name,
            module: "log_view",
            operation: "log.retry",
            error_next_action: "再試行するか、別のファイルを選び直してください。",
            is_configured_folder: false,
            previous_source_id,
        },
        TargetOrigin::Configured { name, .. } => RetryPlan {
            error_target: format!("{name}（{}）", path.display()),
            is_configured_folder: path.is_dir(),
            path,
            display_name,
            module: "targets",
            operation: "target.retry_configured",
            error_next_action: "再試行するか、設定を確認してください。",
            previous_source_id,
        },
    };
    if registry.begin_loading(target_id).is_none() {
        // 直前に存在と「読み込み中でないこと」を同じロック区間で確認済みの
        // ため到達しない。防御的に、ワーカーを起動しない側へ倒す。
        return RetryDecision::AlreadyLoading;
    }
    RetryDecision::Proceed(plan)
}

/// 失敗・キャンセル済みの対象を再試行します（`LOG-027`）。アドホックに選んだ
/// ファイルは選択済みのパスを Rust 側に保持しており、再度ダイアログを表示
/// しません。設定由来のデータソースは、登録時に解決したパスをそのまま
/// 再利用します。
///
/// 再試行 = 同じパスでの読み込みのやり直しです（共有違反（`LOG-027`）・
/// 変更検知（`LOG-023`）・キャンセル（`CancelledPartial`）のいずれも、この
/// コマンドが再試行経路を兼ねます）。対象が直前まで `Ready`／
/// `CancelledPartial` だった場合は、新しい登録を行う前に古い `source_id` を
/// `close_source` し、`SourceBudget` の予約が二重に計上され続けないように
/// します（P06-5）。
///
/// **読み込み中（`Loading`）の対象は受け付けません**
/// （[`RetryTargetResponse::AlreadyLoading`]。Issue #31）。`reload_target` が
/// `Ready` 以外を拒否するのと対称の判定です。
///
/// `manual_profile`（`LOG-022`、P07-2）を指定すると、そのプロファイル名で
/// 開き直します。`manual_datetime_format`（`LOG-022`）を指定すると、
/// その日時書式で解析し直します（プロファイルの `datetime_format` 設定より
/// 優先されます。`hakutaku_core::LoadControl::manual_datetime_format`）。
/// 生表示へ退避した対象に対する「プロファイル・日時書式を選んで再解析」操作
/// （`src/shell.js` の `buildReparseControl`）は、このコマンドをそれらの引数
/// 付きで呼び出します。どちらか一方だけの指定もできます。
// 引数のうち後半4つは Tauri が注入する managed state であり、呼び出し側が
// 渡すのは target_id と2つの手動指定だけ。state を1つの構造体へまとめると
// Tauri の注入対象でなくなるため、まとめられない。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn retry_target(
    target_id: u32,
    manual_profile: Option<String>,
    manual_datetime_format: Option<String>,
    app: AppHandle,
    targets: State<'_, TargetRegistryState>,
    registry: State<'_, DisplaySetRegistryState>,
    budget: State<'_, hakutaku_core::SourceBudget>,
    diagnostics: State<'_, Arc<Diagnostics>>,
) -> RetryTargetResponse {
    let diagnostics_ref: &Diagnostics = diagnostics.inner();

    let plan = {
        let mut target_guard = targets.0.lock().unwrap_or_else(PoisonError::into_inner);
        match begin_retry(&mut target_guard, target_id) {
            RetryDecision::NotFound => return RetryTargetResponse::NotFound,
            RetryDecision::AlreadyLoading => {
                return RetryTargetResponse::AlreadyLoading { target_id }
            }
            RetryDecision::Proceed(plan) => plan,
        }
    };

    if let Some(previous_source_id) = plan.previous_source_id {
        let mut registry_guard = registry.0.lock().unwrap_or_else(PoisonError::into_inner);
        registry_guard.close_source(previous_source_id, budget.inner());
    }

    if plan.is_configured_folder {
        diag_warn!(
            diagnostics_ref,
            module = "targets",
            operation = "target.retry_configured",
            "対象 \"{}\" はフォルダです（未対応）: {}",
            plan.display_name,
            plan.path.display()
        );
        let error = folder_unsupported_error(&plan.display_name, &plan.path);
        let mut target_guard = targets.0.lock().unwrap_or_else(PoisonError::into_inner);
        // ワーカーを起動しないので、begin_retry が始めた読み込み受付を
        // 取り消す（Issue #31）。
        target_guard.abort_loading(target_id);
        target_guard.set_status(
            target_id,
            TargetStatus::Error {
                error: error.clone(),
                access_denied: false,
            },
        );
        return RetryTargetResponse::Failed {
            target_id,
            error: UserFacingErrorDto::from(&error),
        };
    }

    // キャンセル受付の開始（begin_loading）は begin_retry が判定と同じロック
    // 区間で済ませている（Issue #31）。ここでは読み込み本体を起動するだけ。
    spawn_open(
        app,
        OpenRequest {
            target_id,
            path: plan.path,
            source_label: plan.display_name,
            manual_profile,
            manual_datetime_format,
            module: plan.module,
            operation: plan.operation,
            error_target: plan.error_target,
            error_next_action: plan.error_next_action,
        },
    );

    RetryTargetResponse::Loading { target_id }
}

/// `reload_target` の応答です（`LOG-028`、ADR-0007。P06-5）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReloadTargetResponse {
    /// 最新状態を反映した（純粋な追記の反映、または変化なしの再確認）。
    Reloaded {
        target_id: u32,
        display_set_id: u32,
        generation: u64,
        total_items: u64,
    },
    /// 再読み込み後の見込み合計が上限（`PERF-004`〜`006`）を超えるため、
    /// 再読み込み全体を拒否した（ADR-0007）。旧スナップショットの表示は
    /// 維持され、対象一覧の状態は `Ready` のまま `update_pending` だけが
    /// 真になる（`list_targets` で確認できる）。
    RejectedOverLimit {
        target_id: u32,
        error: UserFacingErrorDto,
    },
    /// 削除・縮小・置換の検知（`LOG-023`）、共有違反（`LOG-027`）、その他の
    /// 読み込み・解析エラーのいずれか。対象一覧の状態は `Error` へ遷移する
    /// （`retry_target` で再試行できる）。
    Failed {
        target_id: u32,
        error: UserFacingErrorDto,
    },
    /// `target_id` が一覧に存在しない、または現在 `Ready` 状態ではない
    /// （読み込み中・キャンセル済み・既にエラー状態など、再読み込みの対象外。
    /// `CancelledPartial` は `retry_target` を使ってください）。
    NotFound,
}

/// [`reload_target`] が必要とする最小限の情報です（対象一覧の Mutex を
/// 保持したまま `hakutaku_core::reload_source` を呼ばないよう、値を
/// コピーしてから渡すための内部構造体。`RetryPlan` と同じ設計）。
struct ReloadPlan {
    source_id: u32,
    /// `ERR-002` の対象欄に使う表示用文字列（名称＋フルパス）。
    error_target: String,
}

/// [`hakutaku_core::ChangeKind`] を利用者向けの日本語ラベルへ変換します。
fn change_kind_label(kind: hakutaku_core::ChangeKind) -> &'static str {
    match kind {
        hakutaku_core::ChangeKind::Shrunk => "縮小（切り詰め）を検知しました",
        hakutaku_core::ChangeKind::Replaced => "別ファイルへの置換を検知しました",
        hakutaku_core::ChangeKind::Deleted => "削除を検知しました",
    }
}

/// 対象が現在 `Ready` であれば、`generation_total`（`Some` の場合）・
/// `raw_display`（`Some` の場合）・`update_pending` で状態を更新します
/// （`display_set_id`・`source_id` は変えません。再読み込みは同じ表示集合を
/// 書き換えるだけだからです）。戻り値は更新後の `display_set_id`（対象が
/// 見つからない、または既に `Ready` でなくなっていた場合は `None`）。
///
/// `raw_display` は「再読み込みが表示集合を作り直した結果の
/// `fell_back_to_raw_display`」です（`None` は作り直していないため据え置き）。
/// 以前はこの値を常に据え置き、「`LOG-022` のプロファイル解決結果は初回
/// オープン時から変わらない」ことを理由にしていましたが、その前提は手動指定
/// （`manual_profile`・`manual_datetime_format`）の導入で成り立たなくなり
/// ました。手動指定は1回の読み込み要求限りで再読み込みへは
/// 引き継がれないため、手動書式で日時付き表示にした対象を再読み込みすると
/// 実際には生表示へ戻ります。据え置くとフラグだけが偽のまま残り、生表示なのに
/// 再解析 UI（`src/shell.js`）が出ず、利用者が復旧操作へたどり着けません。
/// 実際の読み込み結果で更新すれば、表示とフラグが必ず一致します。
fn update_ready_after_reload(
    target_guard: &mut TargetRegistry,
    target_id: u32,
    source_id: u32,
    generation_total: Option<(u64, u64)>,
    raw_display: Option<bool>,
    update_pending: bool,
) -> Option<u32> {
    let entry = target_guard.find(target_id)?;
    let (display_set_id, generation, total_items, fell_back_to_raw_display) = match entry.status {
        TargetStatus::Ready {
            display_set_id,
            generation,
            total_items,
            fell_back_to_raw_display,
            ..
        } => (
            display_set_id,
            generation,
            total_items,
            fell_back_to_raw_display,
        ),
        _ => return None,
    };
    let (generation, total_items) = generation_total.unwrap_or((generation, total_items));
    let fell_back_to_raw_display = raw_display.unwrap_or(fell_back_to_raw_display);
    target_guard.set_status(
        target_id,
        TargetStatus::Ready {
            source_id,
            display_set_id,
            generation,
            total_items,
            fell_back_to_raw_display,
            update_pending,
        },
    );
    Some(display_set_id)
}

/// 利用者の明示的な指示で対象を開き直し、最新状態を反映します（`LOG-028`）。
/// リアルタイム追従は行いません（`LOG-010`）。`Ready` 状態の対象にだけ作用し
/// ます（`CancelledPartial` は対象外。`retry_target` で完全に開き直して
/// ください）。
///
/// `hakutaku_core::reload_source` を呼び、結果に応じて対象一覧の状態を
/// 更新します。
///
/// - 最新状態を反映できた場合（追記の反映、または変化なしの確認）:
///   `Ready` のまま `generation`・`total_items` を更新し、
///   [`ReloadTargetResponse::Reloaded`] を返します。
/// - 上限超過で拒否された場合（ADR-0007）: `Ready` のまま
///   `update_pending: true` にし、[`ReloadTargetResponse::RejectedOverLimit`]
///   を返します（旧スナップショットの表示は変更しません）。
/// - 削除・縮小・置換の検知（`LOG-023`）・共有違反（`LOG-027`）・その他の
///   失敗: `Error` へ遷移させ、[`ReloadTargetResponse::Failed`] を返します
///   （`retry_target` で再試行できます）。
#[tauri::command]
pub fn reload_target(
    target_id: u32,
    targets: State<'_, TargetRegistryState>,
    registry: State<'_, DisplaySetRegistryState>,
    budget: State<'_, hakutaku_core::SourceBudget>,
    diagnostics: State<'_, Arc<Diagnostics>>,
    config: State<'_, ConfigState>,
) -> ReloadTargetResponse {
    run_reload_core(
        &targets.0,
        &registry.0,
        budget.inner(),
        diagnostics.inner(),
        &config.config.log_profiles,
        target_id,
    )
}

/// [`reload_target`] の中核処理です。[`run_open_core`] と同じ理由（単体テスト
/// 容易性。`tauri::State` は動作する Tauri アプリなしには構築できない）で、
/// 素の `Mutex`・`SourceBudget`・`Diagnostics` を直接受け取る形に分けています。
fn run_reload_core(
    targets: &Mutex<TargetRegistry>,
    display_set_registry: &Mutex<hakutaku_core::DisplaySetRegistry>,
    budget: &hakutaku_core::SourceBudget,
    diagnostics_ref: &Diagnostics,
    log_profiles: &[hakutaku_config::LogProfileConfig],
    target_id: u32,
) -> ReloadTargetResponse {
    let plan = {
        let target_guard = targets.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = target_guard.find(target_id) else {
            return ReloadTargetResponse::NotFound;
        };
        let source_id = match entry.status {
            TargetStatus::Ready { source_id, .. } => source_id,
            _ => return ReloadTargetResponse::NotFound,
        };
        ReloadPlan {
            source_id,
            error_target: format!(
                "{}（{}）",
                entry.display_name,
                entry.origin.path().display()
            ),
        }
    };

    let outcome = {
        let mut registry_guard = display_set_registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        hakutaku_core::reload_source(&mut registry_guard, budget, plan.source_id, log_profiles)
    };

    let Some(outcome) = outcome else {
        // reload_context 取得時点で source_id が既に消えていた（通常は
        // 発生しない防御的な経路。並行操作は Mutex 保持中は起きないため）。
        return ReloadTargetResponse::NotFound;
    };

    match outcome {
        hakutaku_core::ReloadOutcome::Reloaded {
            generation,
            total_items,
            fell_back_to_raw_display,
        } => {
            diag_info!(
                diagnostics_ref,
                module = "targets",
                operation = "target.reload",
                "対象を再読み込みしました: target_id={target_id}, generation={generation}, \
                 total_items={total_items}, fell_back_to_raw_display={fell_back_to_raw_display:?}"
            );
            let mut target_guard = targets.lock().unwrap_or_else(PoisonError::into_inner);
            match update_ready_after_reload(
                &mut target_guard,
                target_id,
                plan.source_id,
                Some((generation, total_items)),
                // 表示集合を作り直した場合は、その結果で生表示退避の有無を
                // 更新する。`None`（変化なしの再確認）は据え置き。
                fell_back_to_raw_display,
                false,
            ) {
                Some(display_set_id) => ReloadTargetResponse::Reloaded {
                    target_id,
                    display_set_id,
                    generation,
                    total_items,
                },
                None => ReloadTargetResponse::NotFound,
            }
        }
        hakutaku_core::ReloadOutcome::RejectedOverLimit(rejection) => {
            diag_warn!(
                diagnostics_ref,
                module = "targets",
                operation = "target.reload",
                "再読み込みが上限超過のため拒否されました（ADR-0007）: target_id={target_id}, \
                 {rejection}"
            );
            let mut target_guard = targets.lock().unwrap_or_else(PoisonError::into_inner);
            // 上限拒否では旧スナップショットの表示をそのまま維持する（ADR-0007）。
            // 表示集合を作り直していないので、生表示退避の有無も据え置く。
            update_ready_after_reload(
                &mut target_guard,
                target_id,
                plan.source_id,
                None,
                None,
                true,
            );
            let error = UserFacingError::new(
                plan.error_target,
                rejection.to_string(),
                "他の対象を閉じてから、再読み込みを再試行してください。",
            );
            ReloadTargetResponse::RejectedOverLimit {
                target_id,
                error: UserFacingErrorDto::from(&error),
            }
        }
        hakutaku_core::ReloadOutcome::Changed(kind) => {
            let error = UserFacingError::new(
                plan.error_target,
                format!(
                    "再読み込み時に元ファイルの変更を検知しました（LOG-023）: {}。索引を無効化しました。",
                    change_kind_label(kind)
                ),
                "対象を閉じてから、変更後の内容を開き直してください。",
            );
            diag_warn!(
                diagnostics_ref,
                module = "targets",
                operation = "target.reload",
                "{error}"
            );
            let mut target_guard = targets.lock().unwrap_or_else(PoisonError::into_inner);
            target_guard.set_status(
                target_id,
                TargetStatus::Error {
                    error: error.clone(),
                    access_denied: false,
                },
            );
            ReloadTargetResponse::Failed {
                target_id,
                error: UserFacingErrorDto::from(&error),
            }
        }
        hakutaku_core::ReloadOutcome::SharingViolation => {
            let error = UserFacingError::new(
                plan.error_target,
                "他のプロセスが共有を許可せずに開いているため、再読み込みできません（LOG-027）。",
                "対象を閉じているプロセスを確認し、再試行してください。",
            );
            diag_warn!(
                diagnostics_ref,
                module = "targets",
                operation = "target.reload",
                "{error}"
            );
            let mut target_guard = targets.lock().unwrap_or_else(PoisonError::into_inner);
            target_guard.set_status(
                target_id,
                TargetStatus::Error {
                    error: error.clone(),
                    access_denied: false,
                },
            );
            ReloadTargetResponse::Failed {
                target_id,
                error: UserFacingErrorDto::from(&error),
            }
        }
        hakutaku_core::ReloadOutcome::Failed(error) => {
            diag_warn!(
                diagnostics_ref,
                module = "targets",
                operation = "target.reload",
                "{error}"
            );
            let mut target_guard = targets.lock().unwrap_or_else(PoisonError::into_inner);
            target_guard.set_status(
                target_id,
                TargetStatus::Error {
                    error: error.clone(),
                    access_denied: false,
                },
            );
            ReloadTargetResponse::Failed {
                target_id,
                error: UserFacingErrorDto::from(&error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hakutaku_diagnostics::DiagnosticsUnavailable;
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicU64, Ordering};

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
            let count = COUNTER.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "hakutaku-targets-test-{label}-{}-{count}-{nanos}.log",
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

    /// [`run_open_core`] のテスト用の共通土台。
    struct Fixture {
        targets: Mutex<TargetRegistry>,
        display_set_registry: Mutex<hakutaku_core::DisplaySetRegistry>,
        budget: hakutaku_core::SourceBudget,
        diagnostics: Diagnostics,
        /// P11-3: 抑制なし（既存テストの前提を変えないため）。
        throttle: hakutaku_data_source::IoThrottle,
        /// P11-3: 既定は実運用と同じ [`hakutaku_data_source::DEFAULT_CHUNK_BYTES`]。
        chunk_bytes: u64,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_throttle(hakutaku_data_source::IoThrottle::unlimited())
        }

        /// P11-3: 抑制ありの `IoThrottle` を注入したい呼び出し側向け（複数の
        /// `Fixture` インスタンス間で同じ `IoThrottle`（`Clone` で同じ
        /// セマフォ状態を共有）を渡し、同時実行数の上限が対象をまたいで
        /// 効くことを確認するテストで使う）。
        fn with_throttle(throttle: hakutaku_data_source::IoThrottle) -> Self {
            Self::with_throttle_and_chunk_bytes(throttle, hakutaku_data_source::DEFAULT_CHUNK_BYTES)
        }

        /// P11-3: `IoThrottle` と `chunk_bytes` の両方を注入する（同時実行数の
        /// 上限が、実際にチャンク境界を挟んだ待機として観測できるよう、
        /// テストだけ小さい `chunk_bytes` を使う）。
        fn with_throttle_and_chunk_bytes(
            throttle: hakutaku_data_source::IoThrottle,
            chunk_bytes: u64,
        ) -> Self {
            Fixture {
                targets: Mutex::new(TargetRegistry::default()),
                display_set_registry: Mutex::new(hakutaku_core::DisplaySetRegistry::new()),
                budget: hakutaku_core::SourceBudget::new(),
                diagnostics: inactive_diagnostics(),
                throttle,
                chunk_bytes,
            }
        }

        fn register_and_begin(&self, display_name: &str, path: PathBuf) -> u32 {
            let mut guard = self.targets.lock().unwrap();
            let target_id = guard.register(display_name.to_string(), TargetOrigin::AdHoc { path });
            assert!(
                guard.begin_loading(target_id).is_some(),
                "新規登録した対象なので、必ず新しいトークンが発行されるはず"
            );
            target_id
        }

        fn status_of(&self, target_id: u32) -> TargetStatusDto {
            let list = self.targets.lock().unwrap().list();
            list.into_iter()
                .find(|dto| dto.target_id == target_id)
                .expect("対象が一覧に存在するはず")
                .status
        }

        /// テスト専用: DTO では隠している `source_id` を含む内部状態を直接
        /// 取得します（budget 解放のテストで `active_source_id` に渡すため）。
        fn internal_status(&self, target_id: u32) -> TargetStatus {
            let guard = self.targets.lock().unwrap();
            guard
                .find(target_id)
                .expect("対象が一覧に存在するはず")
                .status
                .clone()
        }

        fn run(&self, request: OpenRequest) -> Vec<LoadOutcomeEventPayload> {
            self.run_with_profiles(&[], request)
        }

        /// 設定のログ解析プロファイルを渡して開く（`CFG-008` の
        /// `datetime_format` が効く経路を通したいテスト向け）。
        fn run_with_profiles(
            &self,
            log_profiles: &[hakutaku_config::LogProfileConfig],
            request: OpenRequest,
        ) -> Vec<LoadOutcomeEventPayload> {
            let outcomes = Mutex::new(Vec::new());
            run_open_core(
                &self.targets,
                &self.display_set_registry,
                &self.budget,
                &self.diagnostics,
                log_profiles,
                &self.throttle,
                self.chunk_bytes,
                &request,
                &|_progress| {},
                &|outcome| outcomes.lock().unwrap().push(outcome),
            );
            outcomes.into_inner().unwrap()
        }

        /// [`close_target_core`]（`close_target` の中核）をこの土台の状態に
        /// 対して呼びます。
        fn close(&self, target_id: u32) -> bool {
            close_target_core(
                &self.targets,
                &self.display_set_registry,
                &self.budget,
                target_id,
            )
        }

        /// 読み込み中に閉じられた対象の印（Issue #31）が残っていないことを
        /// 確かめるための覗き見です。
        fn close_pending_is_empty(&self) -> bool {
            self.targets.lock().unwrap().close_pending_loads.is_empty()
        }

        /// [`run_reload_core`]（`reload_target` の中核）をこの土台の状態に対して
        /// 呼びます。
        fn reload(
            &self,
            log_profiles: &[hakutaku_config::LogProfileConfig],
            target_id: u32,
        ) -> ReloadTargetResponse {
            run_reload_core(
                &self.targets,
                &self.display_set_registry,
                &self.budget,
                &self.diagnostics,
                log_profiles,
                target_id,
            )
        }
    }

    /// テスト用ファイルへ1行追記します（再読み込みで `SnapshotVerdict::Appended`
    /// の経路＝表示集合の作り直しを起こすための道具）。
    fn append_line(path: &Path, line: &str) {
        use std::io::Write;
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("追記用に開けるはず");
        writer.write_all(line.as_bytes()).expect("追記できるはず");
    }

    fn request(target_id: u32, path: PathBuf) -> OpenRequest {
        OpenRequest {
            target_id,
            path,
            source_label: "test.log".to_string(),
            manual_profile: None,
            manual_datetime_format: None,
            module: "log_view",
            operation: "log.open",
            error_target: "test.log".to_string(),
            error_next_action: "再試行してください。",
        }
    }

    // 受け入れ条件（PERF-014、CFG-024、P11-3）: 複数の対象を同時に開いても、
    // `IoThrottle` の同時実行数の上限（`parse_concurrency`）が対象をまたいで
    // 効く。`crates/data-source::chunk` の `io_throttle_limits_concurrent_permits`
    // は `IoThrottle` そのものの Semaphore 挙動を検証済みだが、ここでは
    // `run_open_core`（`src-tauri` 側の実際の配線経路）を通しても、複数対象を
    // 共有 `IoThrottle` インスタンスで同時に開いた場合に、同時実行数の上限を
    // 超えて並行読み込みが進まないことを統合的に確認する。
    //
    // 検証方法（時間ベース、緩め）: `max_concurrent=1` の共有 `IoThrottle` の
    // もと、それぞれ複数チャンク（約5個）に分かれる2つのファイルを2スレッド
    // から同時に読み込む。1チャンクあたり `io_interval_ms`（60ms）の待機を
    // 挟むため、1ファイルあたりの許可保持時間はおよそ 4 回分の待機
    // （最初のチャンクは待機しない）＝約 240ms。上限が対象をまたいで効いて
    // いれば（許可の保持がファイル間で直列化されるため）合計の所要時間は
    // 両ファイル分の合計（約 480ms）に近づく。上限が効いていなければ
    // （＝配線に問題があり、実質的に無制限の別々の抑制になっていれば）
    // 2対象が並行に進むため合計時間は「1ファイル分（約 240ms）」に近づく。
    // 480ms と 240ms の間には十分な差があるため、緩いしきい値（350ms）でも
    // 安定して判定できる。
    #[test]
    fn run_open_core_shares_io_throttle_across_concurrent_targets_and_limits_concurrency() {
        // 6行 × 2対象。1行あたり約28バイトなので、chunk_bytes=40 でおよそ
        // 5チャンクに分割される。
        let mut contents = String::new();
        for i in 0..6 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file_a = TempFile::create_text("open-core-throttle-shared-a", &contents);
        let file_b = TempFile::create_text("open-core-throttle-shared-b", &contents);

        // max_concurrent=1・io_interval_ms=60・chunk_bytes=40 バイトを、2つの
        // Fixture（＝2対象）間で共有する。
        let shared_throttle = hakutaku_data_source::IoThrottle::new(NonZeroUsize::new(1), 60);
        let fixture_a = Arc::new(Fixture::with_throttle_and_chunk_bytes(
            shared_throttle.clone(),
            40,
        ));
        let fixture_b = Arc::new(Fixture::with_throttle_and_chunk_bytes(shared_throttle, 40));

        let target_a = fixture_a.register_and_begin("a.log", file_a.path.clone());
        let target_b = fixture_b.register_and_begin("b.log", file_b.path.clone());

        let req_a = request(target_a, file_a.path.clone());
        let req_b = request(target_b, file_b.path.clone());

        let started = std::time::Instant::now();
        let fa = Arc::clone(&fixture_a);
        let handle_a = std::thread::spawn(move || fa.run(req_a));
        let fb = Arc::clone(&fixture_b);
        let handle_b = std::thread::spawn(move || fb.run(req_b));

        let events_a = handle_a.join().expect("パニックしないはず");
        let events_b = handle_b.join().expect("パニックしないはず");
        let elapsed = started.elapsed();

        assert!(matches!(
            events_a[0],
            LoadOutcomeEventPayload::Completed { .. }
        ));
        assert!(matches!(
            events_b[0],
            LoadOutcomeEventPayload::Completed { .. }
        ));

        // 直列化されていれば約480ms、並行に進んでいれば約240msになる想定
        // （テスト冒頭のコメント参照）。中間かつ十分離れたしきい値で判定する。
        assert!(
            elapsed >= std::time::Duration::from_millis(350),
            "同時実行数の上限（max_concurrent=1）が対象をまたいで効いていれば、\
             直列化により長めの時間がかかるはず: {elapsed:?}"
        );
    }

    // 受け入れ条件（`ENV-004`・`PERF-009`）: 読み込み中でも、別
    // スレッドからの範囲取得（`fetch_log_range` と同じ「レジストリのロック →
    // `fetch_range`」の形）が、読み込みの完了を待たずに応答する。
    //
    // ロックの分割そのもの（改善前との待ち時間の比較）は、コア層の
    // `hakutaku_core` 側テスト
    // （`per_batch_registry_access_keeps_range_fetch_responsive_during_load`）
    // で計測している。ここで確認するのは `src-tauri` 側の配線
    // （`run_open_core` が `PerBatchRegistryLock` 経由で
    // `register_source_with_access` を呼んでいること）であり、これが
    // `register_source_with_control` へ戻ると読み込み完了までロックを握り続け、
    // このテストは待ち時間の上限で落ちる。
    //
    // 1チャンク 512 バイト・チャンクごとに 10ms 待つ設定で、実運用の GB 級
    // ファイル（数秒〜数十秒）を数百 ms へ縮めて模している。
    #[test]
    fn run_open_core_keeps_display_set_registry_available_during_load() {
        const LINE_COUNT: u64 = 600;

        let mut contents = String::new();
        for i in 0..LINE_COUNT {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("open-core-lock-split", &contents);

        let fixture = Arc::new(Fixture::with_throttle_and_chunk_bytes(
            hakutaku_data_source::IoThrottle::new(None, 10),
            512,
        ));
        let target_id = fixture.register_and_begin("lock-split.log", file.path.clone());
        let req = request(target_id, file.path.clone());

        let runner = Arc::clone(&fixture);
        let started = std::time::Instant::now();
        let loader = std::thread::spawn(move || runner.run(req));

        let mut max_lock_wait = std::time::Duration::ZERO;
        let mut fetch_ok = 0usize;
        let mut partial_observations = 0usize;
        while !loader.is_finished() {
            let begin = std::time::Instant::now();
            let mut guard = fixture.display_set_registry.lock().unwrap();
            max_lock_wait = max_lock_wait.max(begin.elapsed());
            if let Some(display_set_id) = guard.list_sources().first().map(|s| s.display_set_id) {
                let request = hakutaku_core::RangeRequest {
                    start: 0,
                    max_items: 512,
                    // 伸長では世代が進まないため、読み込み中は常に初回の世代。
                    expected_generation: 1,
                };
                if let Ok(response) = guard.fetch_range(display_set_id, request) {
                    fetch_ok += 1;
                    if response.total_items < LINE_COUNT {
                        partial_observations += 1;
                    }
                }
            }
            drop(guard);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let events = loader.join().expect("パニックしないはず");
        let load_elapsed = started.elapsed();

        assert!(matches!(
            events[0],
            LoadOutcomeEventPayload::Completed { .. }
        ));
        assert!(
            load_elapsed >= std::time::Duration::from_millis(200),
            "抑制により読み込みに十分な時間がかかっている前提が崩れている: {load_elapsed:?}"
        );
        assert!(
            max_lock_wait < std::time::Duration::from_millis(200),
            "読み込み中でもレジストリのロックはすぐ取れるはず（読み込み全体 {load_elapsed:?} に対し \
             最長待ち {max_lock_wait:?}）"
        );
        assert!(
            fetch_ok > 0,
            "読み込み中に範囲取得が1回も応答していない（ロックを保持し続けている疑い）"
        );
        assert!(
            partial_observations > 0,
            "読み込み途中の表示集合（伸長中の total_items）が観測できていない"
        );
    }

    // --- TargetRegistry（状態遷移: 登録 -> 一覧 -> クローズ） ---

    #[test]
    fn register_adds_entry_in_loading_state_and_appears_in_list() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );

        let list = registry.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].target_id, target_id);
        assert_eq!(list[0].display_name, "a.log");
        assert_eq!(list[0].origin, TargetOriginDto::AdHoc);
        assert_eq!(list[0].source_name, None);
        assert!(matches!(
            list[0].status,
            TargetStatusDto::Loading { progress: None }
        ));
    }

    #[test]
    fn register_assigns_distinct_ids_across_calls() {
        let mut registry = TargetRegistry::default();
        let first = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );
        let second = registry.register(
            "b.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\b.log"),
            },
        );
        assert_ne!(first, second);
        assert_eq!(registry.list().len(), 2);
    }

    #[test]
    fn configured_origin_exposes_source_name_in_dto() {
        let mut registry = TargetRegistry::default();
        registry.register(
            "端末A".to_string(),
            TargetOrigin::Configured {
                name: "端末A".to_string(),
                path: PathBuf::from("C:\\device\\a.log"),
            },
        );

        let list = registry.list();
        assert_eq!(list[0].origin, TargetOriginDto::Configured);
        assert_eq!(list[0].source_name.as_deref(), Some("端末A"));
    }

    #[test]
    fn set_status_transitions_from_loading_to_ready() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );

        registry.set_status(
            target_id,
            TargetStatus::Ready {
                source_id: 0,
                display_set_id: 1,
                generation: 1,
                total_items: 42,
                fell_back_to_raw_display: false,
                update_pending: false,
            },
        );

        let list = registry.list();
        match &list[0].status {
            TargetStatusDto::Ready {
                display_set_id,
                generation,
                total_items,
                fell_back_to_raw_display,
                update_pending,
            } => {
                assert_eq!(*display_set_id, 1);
                assert_eq!(*generation, 1);
                assert_eq!(*total_items, 42);
                assert!(!fell_back_to_raw_display);
                assert!(!update_pending);
            }
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }
    }

    #[test]
    fn set_status_transitions_to_error_and_preserves_five_elements() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );

        let error = UserFacingError::new(
            "C:\\logs\\a.log",
            "共有違反で読み取れません",
            "他のプロセスを終了してから再試行してください",
        );
        registry.set_status(
            target_id,
            TargetStatus::Error {
                error,
                access_denied: false,
            },
        );

        let list = registry.list();
        match &list[0].status {
            TargetStatusDto::Error {
                error,
                access_denied,
            } => {
                assert_eq!(error.target, "C:\\logs\\a.log");
                assert_eq!(error.reason, "共有違反で読み取れません");
                assert_eq!(
                    error.next_action,
                    "他のプロセスを終了してから再試行してください"
                );
                assert!(error.continuable);
                assert!(!access_denied);
            }
            other => panic!("Error を期待しましたが {other:?} でした"),
        }
    }

    #[test]
    fn set_status_for_unknown_target_id_does_not_panic_or_affect_others() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );

        registry.set_status(
            9999,
            TargetStatus::Ready {
                source_id: 0,
                display_set_id: 1,
                generation: 1,
                total_items: 1,
                fell_back_to_raw_display: false,
                update_pending: false,
            },
        );

        let list = registry.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].target_id, target_id);
        assert!(matches!(
            list[0].status,
            TargetStatusDto::Loading { progress: None }
        ));
    }

    #[test]
    fn remove_deletes_entry_and_returns_true() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );

        let removed = registry.remove(target_id);
        assert!(removed);
        assert!(registry.list().is_empty());
    }

    #[test]
    fn remove_unknown_target_id_returns_false_and_keeps_others() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );

        let removed = registry.remove(9999);
        assert!(!removed);
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list()[0].target_id, target_id);
    }

    #[test]
    fn one_target_failure_does_not_affect_another_targets_status() {
        // ERR-001: 1対象の失敗が他の対象へ波及しない。
        let mut registry = TargetRegistry::default();
        let first = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );
        let second = registry.register(
            "b.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\b.log"),
            },
        );

        registry.set_status(
            first,
            TargetStatus::Error {
                error: UserFacingError::new("a.log", "失敗", "再試行してください"),
                access_denied: false,
            },
        );
        registry.set_status(
            second,
            TargetStatus::Ready {
                source_id: 0,
                display_set_id: 1,
                generation: 1,
                total_items: 3,
                fell_back_to_raw_display: false,
                update_pending: false,
            },
        );

        let list = registry.list();
        let second_entry = list.iter().find(|dto| dto.target_id == second).unwrap();
        assert!(matches!(second_entry.status, TargetStatusDto::Ready { .. }));
    }

    // --- キャンセルの受け付け（P07-2） ---

    #[test]
    fn begin_loading_registers_a_cancellable_token() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );

        registry.begin_loading(target_id);

        assert!(
            registry.request_cancel(target_id),
            "begin_loading 直後は request_cancel が対象を見つけられるはず"
        );
    }

    #[test]
    fn request_cancel_for_unknown_or_not_loading_target_returns_false() {
        let mut registry = TargetRegistry::default();
        assert!(
            !registry.request_cancel(9999),
            "未登録の対象は false のはず（ERR-001: 他への影響なし）"
        );

        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );
        // begin_loading を呼んでいないので、まだキャンセル可能として登録
        // されていない。
        assert!(!registry.request_cancel(target_id));
    }

    #[test]
    fn finish_loading_removes_the_cancellation_entry() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );
        registry.begin_loading(target_id);
        registry.finish_loading(target_id);

        assert!(
            !registry.request_cancel(target_id),
            "finish_loading 後はキャンセル対象として見つからないはず"
        );
    }

    #[test]
    fn begin_loading_resets_status_to_loading_for_retry() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );
        registry.set_status(
            target_id,
            TargetStatus::Error {
                error: UserFacingError::new("a.log", "失敗", "再試行してください"),
                access_denied: false,
            },
        );

        registry.begin_loading(target_id);

        let list = registry.list();
        assert!(matches!(
            list[0].status,
            TargetStatusDto::Loading { progress: None }
        ));
    }

    // --- active_source_id（P06-5: budget 解放の対象判定） ---

    #[test]
    fn active_source_id_is_some_for_ready_and_cancelled_partial_only() {
        assert_eq!(
            active_source_id(&TargetStatus::Ready {
                source_id: 7,
                display_set_id: 1,
                generation: 1,
                total_items: 1,
                fell_back_to_raw_display: false,
                update_pending: false,
            }),
            Some(7)
        );
        assert_eq!(
            active_source_id(&TargetStatus::CancelledPartial {
                source_id: 9,
                display_set_id: 1,
                generation: 1,
                total_items: 1,
                fell_back_to_raw_display: false,
            }),
            Some(9)
        );
        assert_eq!(
            active_source_id(&TargetStatus::Loading { progress: None }),
            None
        );
        assert_eq!(
            active_source_id(&TargetStatus::Error {
                error: UserFacingError::new("a.log", "失敗", "再試行してください"),
                access_denied: false,
            }),
            None
        );
    }

    // --- UserFacingErrorDto 変換（P04-6 の5要素を落とさないこと） ---

    #[test]
    fn user_facing_error_dto_preserves_all_five_elements_and_error_code() {
        let error = UserFacingError::new("対象X", "理由", "次の操作")
            .with_location("3行目")
            .not_continuable()
            .with_error_code("HKT-TARGETS-0001");

        let dto = UserFacingErrorDto::from(&error);

        assert_eq!(dto.target, "対象X");
        assert_eq!(dto.location.as_deref(), Some("3行目"));
        assert_eq!(dto.reason, "理由");
        assert!(!dto.continuable);
        assert_eq!(dto.next_action, "次の操作");
        assert_eq!(dto.error_code.as_deref(), Some("HKT-TARGETS-0001"));
    }

    #[test]
    fn user_facing_error_dto_defaults_location_and_error_code_to_none() {
        let error = UserFacingError::new("対象", "理由", "次の操作");
        let dto = UserFacingErrorDto::from(&error);
        assert_eq!(dto.location, None);
        assert_eq!(dto.error_code, None);
        assert!(dto.continuable);
    }

    #[test]
    fn folder_unsupported_error_reports_clear_reason_and_full_path() {
        let error = folder_unsupported_error("端末A", Path::new("C:\\Device\\Logs"));
        assert!(error.target.contains("端末A"));
        assert!(error.target.contains("C:\\Device\\Logs"));
        assert_eq!(error.reason, "フォルダの読み込みは現在未対応です。");
        assert!(error.continuable);
    }

    // --- run_open_core（コアの読み込み経路との統合、AppHandle 非依存） ---

    #[test]
    fn run_open_core_marks_target_ready_on_success_and_emits_completed() {
        let contents = "2026/07/28 15:12:23.456 起動しました\n";
        let file = TempFile::create_text("open-core-ok", contents);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("a.log", file.path.clone());

        let events = fixture.run(request(target_id, file.path.clone()));

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            LoadOutcomeEventPayload::Completed { target_id: t } if t == target_id
        ));
        match fixture.status_of(target_id) {
            TargetStatusDto::Ready {
                total_items,
                update_pending,
                ..
            } => {
                assert_eq!(total_items, 1);
                assert!(!update_pending);
            }
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }
        assert!(
            !fixture.targets.lock().unwrap().request_cancel(target_id),
            "完了後は active_loads から除去されているはず"
        );
    }

    #[test]
    fn run_open_core_marks_target_error_on_missing_file_and_emits_failed() {
        let missing = std::env::temp_dir().join("hakutaku-targets-test-open-core-missing-91af.log");
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("missing.log", missing.clone());

        let events = fixture.run(request(target_id, missing));

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            LoadOutcomeEventPayload::Failed { target_id: t, .. } if t == target_id
        ));
        match fixture.status_of(target_id) {
            TargetStatusDto::Error {
                error,
                access_denied,
            } => {
                assert!(error.continuable);
                assert!(
                    !access_denied,
                    "存在しないファイルは NotFound であり ERROR_ACCESS_DENIED ではないはず"
                );
            }
            other => panic!("Error を期待しましたが {other:?} でした"),
        }
    }

    // 受け入れ条件（LOG-027）: 共有を許可しない方法で開かれた対象への
    // run_open_core は、ERR-002 の5要素（対象・理由・継続可否・次操作）を
    // 持つ UserFacingError を状態へ反映する。対象にはフルパスが含まれ
    // （マスキングしない）、継続可能（他の対象の閲覧を止めない）で、次操作に
    // 再試行の案内が含まれる。budget・registry の状態も変わらない。
    #[test]
    fn run_open_core_reports_sharing_violation_with_all_five_error_elements() {
        use std::os::windows::fs::OpenOptionsExt;

        let file = TempFile::create_text(
            "open-core-sharing-violation",
            "2026/07/28 15:12:23.456 locked\n",
        );
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("a.log", file.path.clone());

        let locker = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file.path)
            .expect("排他的に開けるはず（LOG-027 再現用の前提）");

        let mut req = request(target_id, file.path.clone());
        req.error_target = file.path.display().to_string();
        let events = fixture.run(req);

        assert_eq!(events.len(), 1);
        match &events[0] {
            LoadOutcomeEventPayload::Failed { error, .. } => {
                assert_eq!(error.target, file.path.display().to_string());
                assert!(
                    error.reason.contains("共有") || error.reason.contains("LOG-027"),
                    "理由に共有違反である旨が含まれるはず: {}",
                    error.reason
                );
                assert!(error.continuable);
                assert!(error.next_action.contains("再試行"));
            }
            other => panic!("Failed を期待しましたが {other:?} でした"),
        }

        assert_eq!(fixture.budget.total_bytes(), 0);
        assert!(fixture.display_set_registry.lock().unwrap().is_empty());

        drop(locker);
    }

    // 受け入れ条件（PRIV-002、P11-1）: ERROR_ACCESS_DENIED によるオープン
    // 失敗は AccessDenied として分類され、ERR-002 の理由が昇格による再試行を
    // 案内する専用の文面になり、対象一覧・イベント payload の access_denied
    // フラグが立つ。
    //
    // `icacls` で自分自身に対する読み取りを明示的に拒否し、実際の ACL 拒否を
    // 再現する（分類ロジック自体の決定的な単体テストは
    // crates/data-source・crates/core-services 側にあり、ここでは Win32
    // 呼び出しを含めた経路全体を統合的に確認する）。所有者は自分自身の
    // ファイルの ACL を（読み取りを拒否されていても）変更できるため、管理者
    // 権限は不要。`icacls` が使えない・拒否 ACE を付与できない環境では
    // テストの前提を満たせないため、`bootstrap::layout`
    // のジャンクションテストと同じ方針でスキップする。
    #[test]
    fn run_open_core_classifies_access_denied_and_marks_target_status() {
        let file = TempFile::create_text(
            "open-core-access-denied",
            "2026/07/28 15:12:23.456 denied\n",
        );

        let username = std::env::var("USERNAME").unwrap_or_default();
        if username.is_empty() {
            eprintln!(
                "USERNAME 環境変数を取得できない環境のため \
                 run_open_core_classifies_access_denied_and_marks_target_status をスキップします"
            );
            return;
        }

        let denied = std::process::Command::new("icacls")
            .arg(&file.path)
            .arg("/deny")
            .arg(format!("{username}:(R)"))
            .status()
            .map(|status| status.success())
            .unwrap_or(false);

        if !denied {
            eprintln!(
                "icacls でアクセス拒否を再現できない環境のため \
                 run_open_core_classifies_access_denied_and_marks_target_status をスキップします"
            );
            return;
        }

        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("denied.log", file.path.clone());

        let events = fixture.run(request(target_id, file.path.clone()));

        // 後始末: 拒否 ACE を解除する（ファイル自体の削除は TempFile::drop へ
        // 任せる。読み取り拒否は削除操作を妨げないはずだが、念のため先に
        // ACL を復元する）。
        let _ = std::process::Command::new("icacls")
            .arg(&file.path)
            .arg("/remove:d")
            .arg(&username)
            .status();

        assert_eq!(events.len(), 1);
        match &events[0] {
            LoadOutcomeEventPayload::Failed {
                access_denied,
                error,
                ..
            } => {
                assert!(*access_denied, "AccessDenied として分類されるはず");
                assert!(
                    error.reason.contains("管理者権限"),
                    "理由が昇格による再試行を案内するはず: {}",
                    error.reason
                );
                assert!(error.continuable);
            }
            other => panic!("Failed を期待しましたが {other:?} でした"),
        }

        match fixture.status_of(target_id) {
            TargetStatusDto::Error { access_denied, .. } => {
                assert!(
                    access_denied,
                    "対象一覧の状態にも access_denied が反映されるはず"
                );
            }
            other => panic!("Error を期待しましたが {other:?} でした"),
        }
    }

    // 受け入れ条件: run_open_core が register_source_with_control
    // （SourceBudget）を経由するようになったこと（P06-5）を、budget の予約が
    // 実際に増減することで確認する。close_source を呼ぶと解放され、
    // `close_target`／`retry_target` が内部で行う処理（active_source_id 経由）
    // と同じ経路になっている。
    #[test]
    fn run_open_core_reserves_budget_and_closing_the_source_frees_it() {
        let contents = "2026/07/28 15:12:23.456 起動しました\n";
        let file = TempFile::create_text("open-core-budget", contents);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("a.log", file.path.clone());

        fixture.run(request(target_id, file.path.clone()));
        assert_eq!(fixture.budget.total_bytes(), contents.len() as u64);

        let source_id = active_source_id(&fixture.internal_status(target_id))
            .expect("Ready のはずなので source_id が取れるはず");

        // close_target が内部で行う処理と同じ（source_id を close_source する）。
        let mut registry_guard = fixture.display_set_registry.lock().unwrap();
        registry_guard.close_source(source_id, &fixture.budget);
        drop(registry_guard);
        assert_eq!(
            fixture.budget.total_bytes(),
            0,
            "close_source で予約が解放されるはず"
        );
    }

    #[test]
    fn run_open_core_reports_progress_through_the_callback() {
        let mut contents = String::new();
        for i in 0..20 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("open-core-progress", &contents);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("progress.log", file.path.clone());

        let progress_calls: Mutex<Vec<(u64, Option<u64>)>> = Mutex::new(Vec::new());
        let outcomes: Mutex<Vec<LoadOutcomeEventPayload>> = Mutex::new(Vec::new());
        run_open_core(
            &fixture.targets,
            &fixture.display_set_registry,
            &fixture.budget,
            &fixture.diagnostics,
            &[],
            &fixture.throttle,
            fixture.chunk_bytes,
            &request(target_id, file.path.clone()),
            &|payload| {
                progress_calls
                    .lock()
                    .unwrap()
                    .push((payload.done_bytes, payload.total_bytes));
            },
            &|outcome| outcomes.lock().unwrap().push(outcome),
        );

        let calls = progress_calls.into_inner().unwrap();
        assert!(!calls.is_empty(), "進捗が少なくとも1回は通知されるはず");
        for (done, total) in &calls {
            assert_eq!(*total, Some(contents.len() as u64));
            assert!(done <= &total.unwrap());
        }
    }

    #[test]
    fn run_open_core_cancellation_marks_cancelled_partial_and_preserves_loaded_items() {
        let mut contents = String::new();
        for i in 0..50 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("open-core-cancel", &contents);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("cancel.log", file.path.clone());

        // 開始前にキャンセル要求を出しておく（1チャンクも読まずに停止する
        // 経路。crates/core-services の同種テストと同じ考え方）。
        assert!(fixture.targets.lock().unwrap().request_cancel(target_id));

        let events = fixture.run(request(target_id, file.path.clone()));

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            LoadOutcomeEventPayload::Cancelled { target_id: t } if t == target_id
        ));
        match fixture.status_of(target_id) {
            TargetStatusDto::CancelledPartial { .. } => {}
            other => panic!("CancelledPartial を期待しましたが {other:?} でした"),
        }

        // CancelledPartial も Ready と同じく source_id を保持し、budget の
        // 予約を解放できる（P06-5 の拡張）。
        let source_id = active_source_id(&fixture.internal_status(target_id))
            .expect("CancelledPartial も source_id を持つはず");
        let mut registry_guard = fixture.display_set_registry.lock().unwrap();
        registry_guard.close_source(source_id, &fixture.budget);
        drop(registry_guard);
        assert_eq!(
            fixture.budget.total_bytes(),
            0,
            "CancelledPartial でも close_source で予約が解放されるはず"
        );

        // 他の対象は影響を受けない（ERR-001）。
        let other = TempFile::create_text("open-core-cancel-other", "2026/07/28 15:12:00.000 別\n");
        let other_id = fixture.register_and_begin("other.log", other.path.clone());
        let other_events = fixture.run(request(other_id, other.path.clone()));
        assert!(matches!(
            other_events[0],
            LoadOutcomeEventPayload::Completed { .. }
        ));
    }

    #[test]
    fn run_open_core_propagates_manual_profile_to_resolution() {
        let contents = "2026/07/28 15:12:23.456 手動プロファイル\n";
        let file = TempFile::create_text("open-core-manual-profile", contents);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("manual.log", file.path.clone());

        let profile = hakutaku_config::LogProfileConfig {
            name: "manual-utf8".to_string(),
            path_pattern: r"C:\Other\Unrelated\*.log".to_string(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Named("utf-8".to_string()),
            ansi_codepage: None,
            datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
        };

        let mut req = request(target_id, file.path.clone());
        req.manual_profile = Some("manual-utf8".to_string());

        let outcomes: Mutex<Vec<LoadOutcomeEventPayload>> = Mutex::new(Vec::new());
        run_open_core(
            &fixture.targets,
            &fixture.display_set_registry,
            &fixture.budget,
            &fixture.diagnostics,
            std::slice::from_ref(&profile),
            &fixture.throttle,
            fixture.chunk_bytes,
            &req,
            &|_progress| {},
            &|outcome| outcomes.lock().unwrap().push(outcome),
        );

        match fixture.status_of(target_id) {
            TargetStatusDto::Ready {
                fell_back_to_raw_display,
                ..
            } => assert!(
                !fell_back_to_raw_display,
                "手動指定した既知のプロファイルなので生表示へは退避しないはず"
            ),
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }
    }

    #[test]
    fn run_open_core_unknown_manual_profile_falls_back_to_raw_display() {
        let contents = "2026/07/28 15:12:23.456 不明なプロファイル\n";
        let file = TempFile::create_text("open-core-manual-profile-unknown", contents);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("manual-unknown.log", file.path.clone());

        let mut req = request(target_id, file.path.clone());
        req.manual_profile = Some("does-not-exist".to_string());

        fixture.run(req);

        match fixture.status_of(target_id) {
            TargetStatusDto::Ready {
                fell_back_to_raw_display,
                ..
            } => assert!(fell_back_to_raw_display),
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }
    }

    /// 自動判定では必ず曖昧になる（＝生表示へ退避する）内容です
    /// （`LOG-DT-004` は常に `LOG-DT-005` とも同時に成立するため。
    /// `crates/core-services/src/loader.rs` の doc コメント参照）。手動書式
    /// 指定が効いているかどうかを、生表示退避の有無だけで判別できます。
    const LOG_DT_004_LINE: &str = "2026/07/28 15:12:23:45 手動書式指定\n";

    // 受け入れ条件（LOG-022）: OpenRequest.manual_datetime_format が
    // LoadControl まで伝わり、設定（log_profiles）が空でも指定した書式で解析
    // される（生表示へ退避しない）。
    #[test]
    fn run_open_core_propagates_manual_datetime_format() {
        let file = TempFile::create_text("open-core-manual-datetime", LOG_DT_004_LINE);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("manual-dt.log", file.path.clone());

        let mut req = request(target_id, file.path.clone());
        req.manual_datetime_format = Some("LOG-DT-004".to_string());

        fixture.run(req);

        match fixture.status_of(target_id) {
            TargetStatusDto::Ready {
                fell_back_to_raw_display,
                ..
            } => assert!(
                !fell_back_to_raw_display,
                "手動指定した書式で確定するため、曖昧判定による生表示退避は起きないはず"
            ),
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }
    }

    // 受け入れ条件（LOG-022）: 既知の6書式にない ID は無視され、
    // 利用者向けエラーにはならない（読み込みは成功し、書式未指定と同じく
    // 自動判定＝この内容では生表示退避になる）。unknown_manual_profile と
    // 同じく「推測でどれかへ寄せない」扱い。
    #[test]
    fn run_open_core_unknown_manual_datetime_format_is_ignored() {
        let file = TempFile::create_text("open-core-manual-datetime-unknown", LOG_DT_004_LINE);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("manual-dt-unknown.log", file.path.clone());

        let mut req = request(target_id, file.path.clone());
        req.manual_datetime_format = Some("LOG-DT-999".to_string());

        let events = fixture.run(req);
        assert!(
            matches!(events[0], LoadOutcomeEventPayload::Completed { .. }),
            "不正な書式 ID でも読み込み自体は成功するはず"
        );

        match fixture.status_of(target_id) {
            TargetStatusDto::Ready {
                fell_back_to_raw_display,
                ..
            } => assert!(
                fell_back_to_raw_display,
                "書式指定が無視され、自動判定の曖昧判定により生表示へ退避するはず"
            ),
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }
    }

    // 受け入れ条件: list_datetime_formats が既知の6書式を、
    // 解析側の要件 ID・表示用パターンのまま返す。フロントエンドはこの応答だけを
    // 選択肢の出所とし、6書式を自前の定数として持たない。
    #[test]
    fn list_datetime_formats_returns_all_six_known_formats() {
        let formats = list_datetime_formats();

        assert_eq!(formats.len(), 6);
        let ids: Vec<&str> = formats.iter().map(|format| format.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "LOG-DT-001",
                "LOG-DT-002",
                "LOG-DT-003",
                "LOG-DT-004",
                "LOG-DT-005",
                "LOG-DT-006",
            ]
        );
        for format in &formats {
            assert!(
                !format.pattern.is_empty(),
                "{} の表示用パターンが空になっている",
                format.id
            );
            assert!(
                hakutaku_core::LogDateTimeFormat::from_id(&format.id).is_some(),
                "{} は再解析要求としてそのまま送り返せるはず",
                format.id
            );
        }
    }

    // --- reload_target が使う内部ヘルパー ---

    #[test]
    fn update_ready_after_reload_updates_generation_and_total_items_when_ready() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );
        registry.set_status(
            target_id,
            TargetStatus::Ready {
                source_id: 7,
                display_set_id: 3,
                generation: 1,
                total_items: 10,
                fell_back_to_raw_display: false,
                update_pending: false,
            },
        );

        let display_set_id =
            update_ready_after_reload(&mut registry, target_id, 7, Some((2, 20)), None, false)
                .expect("Ready な対象なので Some のはず");
        assert_eq!(display_set_id, 3, "display_set_id は変えない");

        let list = registry.list();
        match &list[0].status {
            TargetStatusDto::Ready {
                display_set_id,
                generation,
                total_items,
                fell_back_to_raw_display,
                update_pending,
            } => {
                assert_eq!(*display_set_id, 3);
                assert_eq!(*generation, 2);
                assert_eq!(*total_items, 20);
                assert!(
                    !fell_back_to_raw_display,
                    "raw_display に None を渡した場合は生表示退避の判定を据え置く"
                );
                assert!(!update_pending);
            }
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }
    }

    // 受け入れ条件（ADR-0007）: 上限拒否時は generation_total を渡さず
    // update_pending だけを立てられる（旧世代・旧件数を変更しない）。
    #[test]
    fn update_ready_after_reload_can_set_update_pending_without_changing_generation() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );
        registry.set_status(
            target_id,
            TargetStatus::Ready {
                source_id: 7,
                display_set_id: 3,
                generation: 1,
                total_items: 10,
                fell_back_to_raw_display: false,
                update_pending: false,
            },
        );

        update_ready_after_reload(&mut registry, target_id, 7, None, None, true)
            .expect("Ready な対象なので Some のはず");

        let list = registry.list();
        match &list[0].status {
            TargetStatusDto::Ready {
                generation,
                total_items,
                update_pending,
                ..
            } => {
                assert_eq!(*generation, 1, "旧世代を維持する");
                assert_eq!(*total_items, 10, "旧件数を維持する");
                assert!(update_pending, "更新未反映フラグが立つ");
            }
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }
    }

    #[test]
    fn update_ready_after_reload_returns_none_when_not_ready() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );
        // 既定は Loading。

        let result = update_ready_after_reload(&mut registry, target_id, 7, None, None, true);
        assert!(result.is_none());
    }

    #[test]
    fn update_ready_after_reload_returns_none_for_unknown_target() {
        let mut registry = TargetRegistry::default();
        let result = update_ready_after_reload(&mut registry, 9999, 7, None, None, true);
        assert!(result.is_none());
    }

    // 受け入れ条件（LOG-022）: raw_display に Some を渡した場合は、
    // 据え置きではなく渡された値で fell_back_to_raw_display を更新する
    // （再読み込みが表示集合を作り直した実結果を反映するための土台）。
    #[test]
    fn update_ready_after_reload_overwrites_raw_display_flag_when_given() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );
        registry.set_status(
            target_id,
            TargetStatus::Ready {
                source_id: 7,
                display_set_id: 3,
                generation: 1,
                total_items: 10,
                fell_back_to_raw_display: false,
                update_pending: false,
            },
        );

        update_ready_after_reload(
            &mut registry,
            target_id,
            7,
            Some((2, 20)),
            Some(true),
            false,
        )
        .expect("Ready な対象なので Some のはず");

        let list = registry.list();
        match &list[0].status {
            TargetStatusDto::Ready {
                fell_back_to_raw_display,
                ..
            } => assert!(
                fell_back_to_raw_display,
                "実結果が生表示退避なら、据え置かずに真へ更新する"
            ),
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }
    }

    // 受け入れ条件（LOG-022・LOG-028）: 手動書式で日時付き表示に
    // した対象を再読み込みすると、手動指定は引き継がれない（1回の読み込み要求
    // 限りという既存の設計）ため実際には生表示へ戻る。このとき
    // fell_back_to_raw_display も真へ戻り、表示とフラグが一致する（据え置くと
    // 生表示なのに再解析 UI が出ず、利用者が復旧操作へたどり着けない）。
    #[test]
    fn reload_restores_raw_display_flag_when_manual_datetime_format_is_lost() {
        let file = TempFile::create_text("reload-manual-datetime", LOG_DT_004_LINE);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("manual-dt-reload.log", file.path.clone());

        let mut req = request(target_id, file.path.clone());
        req.manual_datetime_format = Some("LOG-DT-004".to_string());
        fixture.run(req);

        match fixture.status_of(target_id) {
            TargetStatusDto::Ready {
                fell_back_to_raw_display,
                ..
            } => assert!(
                !fell_back_to_raw_display,
                "初回は手動書式が効くので生表示退避しない（この後の対比の前提）"
            ),
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }

        append_line(&file.path, "2026/07/28 15:12:24:99 二行目\n");

        let response = fixture.reload(&[], target_id);
        assert!(
            matches!(
                response,
                ReloadTargetResponse::Reloaded { total_items: 2, .. }
            ),
            "追記1行が反映されるはずですが {response:?} でした"
        );

        match fixture.status_of(target_id) {
            TargetStatusDto::Ready {
                fell_back_to_raw_display,
                ..
            } => assert!(
                fell_back_to_raw_display,
                "再読み込みで手動書式が失われ生表示へ戻るため、フラグも真へ戻るはず"
            ),
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }
    }

    // 受け入れ条件（CFG-008）: 設定のプロファイル由来の書式は
    // path_pattern の一致で再読み込み時にも同じように解決されるため、
    // fell_back_to_raw_display は偽のまま変わらない（実結果で更新する方式に
    // しても、設定で開いた対象の表示が揺れないことの確認）。
    #[test]
    fn reload_keeps_raw_display_flag_false_when_profile_supplies_datetime_format() {
        let file = TempFile::create_text("reload-profile-datetime", LOG_DT_004_LINE);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("profile-dt-reload.log", file.path.clone());

        // 絶対パス完全一致（LOG-021 の第2段階）で必ず解決されるプロファイル。
        let profiles = vec![hakutaku_config::LogProfileConfig {
            name: "exact-dt-004".to_string(),
            path_pattern: file.path.to_string_lossy().into_owned(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Auto,
            ansi_codepage: None,
            datetime_format: hakutaku_config::DateTimeFormatSetting::LogDt004,
        }];

        fixture.run_with_profiles(&profiles, request(target_id, file.path.clone()));

        match fixture.status_of(target_id) {
            TargetStatusDto::Ready {
                fell_back_to_raw_display,
                ..
            } => assert!(
                !fell_back_to_raw_display,
                "設定の書式が効くので生表示退避しない（この後の対比の前提）"
            ),
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }

        append_line(&file.path, "2026/07/28 15:12:24:99 二行目\n");

        let response = fixture.reload(&profiles, target_id);
        assert!(
            matches!(
                response,
                ReloadTargetResponse::Reloaded { total_items: 2, .. }
            ),
            "追記1行が反映されるはずですが {response:?} でした"
        );

        match fixture.status_of(target_id) {
            TargetStatusDto::Ready {
                fell_back_to_raw_display,
                ..
            } => assert!(
                !fell_back_to_raw_display,
                "設定由来の書式は再読み込みでも再適用されるため、フラグは偽のまま"
            ),
            other => panic!("Ready を期待しましたが {other:?} でした"),
        }
    }

    // --- 読み込み中の close / retry / 重複オープン（Issue #31） ---

    // 受け入れ条件（Issue #31）: 既に読み込み中の対象へ begin_loading を
    // 呼んでも、既存のキャンセルトークンを黙って上書きしない。上書きすると
    // 先発ワーカーを cancel_load で止められなくなり、そのワーカーが登録した
    // ソースと SourceBudget の予約が解放不能になる。
    #[test]
    fn begin_loading_does_not_replace_an_existing_token() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );

        let first = registry
            .begin_loading(target_id)
            .expect("最初の begin_loading は新しいトークンを発行するはず");
        assert!(
            registry.begin_loading(target_id).is_none(),
            "読み込み中の対象への begin_loading は None を返すはず"
        );

        // 先発ワーカーが握っているトークンで、引き続きキャンセルできる。
        assert!(registry.request_cancel(target_id));
        assert!(
            first.is_cancelled(),
            "上書きされていなければ、先発ワーカーのトークンがキャンセルされるはず"
        );
    }

    // 受け入れ条件（Issue #31）: 「閉じられた」印は、回収してくれるワーカーが
    // いるときだけ記録し、回収は一度だけできる。
    #[test]
    fn close_pending_mark_is_recorded_only_while_a_worker_is_running() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );

        assert!(
            !registry.mark_close_pending(target_id),
            "begin_loading 前は後始末を代行する相手がいないので記録しない"
        );
        assert!(registry.close_pending_loads.is_empty());

        registry
            .begin_loading(target_id)
            .expect("新規登録した対象なので発行されるはず");
        assert!(registry.mark_close_pending(target_id));
        assert!(registry.take_close_pending(target_id));
        assert!(
            !registry.take_close_pending(target_id),
            "印は一度しか回収できない（二重に close_source を呼ばない）"
        );
    }

    // 受け入れ条件（Issue #31）: 読み込み中の対象は retry_target で再試行
    // できない（reload_target が Ready 以外を拒否するのと対称）。拒否した
    // 場合は先発ワーカーのトークンも書き換えない。存在しない対象は従来どおり
    // NotFound。
    #[test]
    fn begin_retry_rejects_a_loading_target() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );
        let running = registry
            .begin_loading(target_id)
            .expect("新規登録した対象なので発行されるはず");

        assert!(matches!(
            begin_retry(&mut registry, target_id),
            RetryDecision::AlreadyLoading
        ));
        assert!(matches!(
            begin_retry(&mut registry, 9999),
            RetryDecision::NotFound
        ));

        assert!(registry.request_cancel(target_id));
        assert!(
            running.is_cancelled(),
            "拒否された再試行はトークンを差し替えないはず"
        );
    }

    // 受け入れ条件（Issue #31・P06-5）: 読み込み中以外は従来どおり再試行でき、
    // 旧 source_id（`CancelledPartial`）は解放対象として計画へ引き継がれる。
    // 判定を通ると同じロック区間でキャンセル受付が始まる（begin_loading 済み）。
    #[test]
    fn begin_retry_proceeds_for_error_and_cancelled_partial() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "a.log".to_string(),
            TargetOrigin::AdHoc {
                path: PathBuf::from("C:\\logs\\a.log"),
            },
        );

        registry.set_status(
            target_id,
            TargetStatus::Error {
                error: UserFacingError::new("a.log", "失敗", "再試行してください"),
                access_denied: false,
            },
        );
        match begin_retry(&mut registry, target_id) {
            RetryDecision::Proceed(plan) => assert_eq!(plan.previous_source_id, None),
            _ => panic!("Error 状態は再試行できるはず"),
        }
        assert!(
            registry.request_cancel(target_id),
            "判定を通った時点でキャンセル受付が始まっているはず"
        );

        // ワーカーが終了したところ（finish_loading）から、キャンセル済みの
        // 部分読み込みとして再試行する場合。
        registry.finish_loading(target_id);
        registry.set_status(
            target_id,
            TargetStatus::CancelledPartial {
                source_id: 7,
                display_set_id: 1,
                generation: 1,
                total_items: 3,
                fell_back_to_raw_display: false,
            },
        );
        match begin_retry(&mut registry, target_id) {
            RetryDecision::Proceed(plan) => assert_eq!(plan.previous_source_id, Some(7)),
            _ => panic!("CancelledPartial は再試行できるはず"),
        }
    }

    // 受け入れ条件（Issue #31）: 結局ワーカーを起動しない経路（フォルダ未対応）
    // は、begin_loading で始めた受付を取り消し、印もトークンも残さない。
    #[test]
    fn abort_loading_clears_the_token_and_the_close_pending_mark() {
        let mut registry = TargetRegistry::default();
        let target_id = registry.register(
            "端末A".to_string(),
            TargetOrigin::Configured {
                name: "端末A".to_string(),
                path: PathBuf::from("C:\\device"),
            },
        );
        registry
            .begin_loading(target_id)
            .expect("新規登録した対象なので発行されるはず");
        assert!(registry.mark_close_pending(target_id));

        registry.abort_loading(target_id);

        assert!(!registry.request_cancel(target_id));
        assert!(
            registry.close_pending_loads.is_empty(),
            "回収する相手がいない印を残さないはず"
        );
    }

    // 受け入れ条件（Issue #31）: 同じ名前のデータソースを続けて開こうとしても
    // 対象は増えず、2回目は既存の target_id を返す（＝ワーカーを増やさない。
    // 増やすと、どの対象からも参照されないソースと予約が残る）。読み込み中・
    // 読み込み済みのいずれも「既に開いている」とみなす。
    #[test]
    fn reserve_configured_target_reuses_a_loading_or_ready_target() {
        let mut registry = TargetRegistry::default();
        let path = Path::new("C:\\device\\a.log");

        let first = match reserve_configured_target(&mut registry, "端末A", path) {
            ConfiguredTargetSlot::Registered(target_id) => target_id,
            ConfiguredTargetSlot::AlreadyOpen(_) => panic!("最初は新規登録のはず"),
        };
        assert!(
            registry.request_cancel(first),
            "新規登録と同時にキャンセル受付まで済ませるはず（Issue #31）"
        );

        match reserve_configured_target(&mut registry, "端末A", path) {
            ConfiguredTargetSlot::AlreadyOpen(target_id) => assert_eq!(target_id, first),
            ConfiguredTargetSlot::Registered(_) => {
                panic!("読み込み中の同名対象は再利用されるはず")
            }
        }
        assert_eq!(registry.list().len(), 1, "対象は増えないはず");

        registry.set_status(
            first,
            TargetStatus::Ready {
                source_id: 0,
                display_set_id: 1,
                generation: 1,
                total_items: 3,
                fell_back_to_raw_display: false,
                update_pending: false,
            },
        );
        match reserve_configured_target(&mut registry, "端末A", path) {
            ConfiguredTargetSlot::AlreadyOpen(target_id) => assert_eq!(target_id, first),
            ConfiguredTargetSlot::Registered(_) => {
                panic!("読み込み済みの同名対象も再利用されるはず")
            }
        }
        assert_eq!(registry.list().len(), 1, "対象は増えないはず");
    }

    // 受け入れ条件（Issue #31）: 別名、および開き直しの意図が明確な状態
    // （Error）では、新しい対象として登録する。
    #[test]
    fn reserve_configured_target_registers_for_another_name_or_errored_target() {
        let mut registry = TargetRegistry::default();
        let path = Path::new("C:\\device\\a.log");

        let first = match reserve_configured_target(&mut registry, "端末A", path) {
            ConfiguredTargetSlot::Registered(target_id) => target_id,
            ConfiguredTargetSlot::AlreadyOpen(_) => panic!("最初は新規登録のはず"),
        };
        // 読み込みが失敗して終わったところ（ワーカーの終端処理まで済んだ状態）。
        registry.finish_loading(first);
        registry.set_status(
            first,
            TargetStatus::Error {
                error: UserFacingError::new("端末A", "失敗", "再試行してください"),
                access_denied: false,
            },
        );

        match reserve_configured_target(&mut registry, "端末A", path) {
            ConfiguredTargetSlot::Registered(target_id) => assert_ne!(target_id, first),
            ConfiguredTargetSlot::AlreadyOpen(_) => {
                panic!("Error 状態は「既に開いている」とはみなさないはず")
            }
        }
        match reserve_configured_target(&mut registry, "端末B", Path::new("C:\\device\\b.log")) {
            ConfiguredTargetSlot::Registered(_) => {}
            ConfiguredTargetSlot::AlreadyOpen(_) => panic!("別名は新規登録のはず"),
        }
        assert_eq!(registry.list().len(), 3);
    }

    // 受け入れ条件（Issue #31）: 読み込み中に閉じた対象について、ワーカーが
    // その後にコア層へ登録を終えても、ソースも SourceBudget の予約も残らない。
    //
    // 閉じる時点で source_id がまだ払い出されていない経路を確実に通すため、
    // 読み込み開始（register_and_begin）の直後に閉じてから、同じスレッドで
    // ワーカー本体（run_open_core）を走らせる。閉じた時点でキャンセルが要求
    // されるため、コア層は「1件も読めていないソース」を登録して
    // CancelledPartial で終わる。修正前は、この登録が対象一覧のどこからも
    // 参照されないまま残り、上限判定（PERF-004〜006）へ恒久計上され、統合
    // 表示集合（LOG-007・LOG-008）にも閉じたファイルの行が混入していた。
    #[test]
    fn close_target_while_loading_reclaims_a_source_registered_afterwards() {
        let mut contents = String::new();
        for i in 0..50 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("close-during-load-early", &contents);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("closing.log", file.path.clone());

        assert!(
            fixture.close(target_id),
            "読み込み中の対象も一覧から除去できるはず"
        );
        assert!(fixture.targets.lock().unwrap().list().is_empty());

        let events = fixture.run(request(target_id, file.path.clone()));

        assert!(
            events.is_empty(),
            "閉じた対象には完了・失敗イベントの宛先が無いので発行しないはず"
        );
        assert!(
            fixture.display_set_registry.lock().unwrap().is_empty(),
            "閉じた対象のソースがコア側に残っている（統合表示へ混入する）"
        );
        assert_eq!(
            fixture.budget.total_bytes(),
            0,
            "SourceBudget の予約が解放されずに残っている"
        );
        assert!(
            !fixture.targets.lock().unwrap().request_cancel(target_id),
            "active_loads にトークンが残っている"
        );
        assert!(fixture.close_pending_is_empty(), "回収済みの印が残っている");
    }

    // 受け入れ条件（Issue #31）: 既にコア層へ登録済みの読み込み中対象を閉じた
    // 場合も、ソースと予約が残らない。
    //
    // 「コア側に登録済み・かつ読み込み中」という状態を実際に観測してから
    // 閉じるため、run_open_core_keeps_display_set_registry_available_during_load
    // と同じ手法（小さい chunk_bytes と I/O 発行間隔）で読み込みを引き延ばす。
    // 閉じた時点ではまだソースが残る（キャンセルはチャンク境界で確認される
    // ため。cancel_load の doc コメントと同じ性質）が、ワーカーの終端処理が
    // 必ず回収する。
    #[test]
    fn close_target_while_loading_frees_an_already_registered_core_source() {
        const LINE_COUNT: u64 = 600;

        let mut contents = String::new();
        for i in 0..LINE_COUNT {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("close-during-load-registered", &contents);

        let fixture = Arc::new(Fixture::with_throttle_and_chunk_bytes(
            hakutaku_data_source::IoThrottle::new(None, 10),
            512,
        ));
        let target_id = fixture.register_and_begin("closing.log", file.path.clone());
        let req = request(target_id, file.path.clone());

        let runner = Arc::clone(&fixture);
        let loader = std::thread::spawn(move || runner.run(req));

        // 最初のバッチが登録され、かつまだ読み込み中である瞬間を待つ。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut observed = false;
        while std::time::Instant::now() < deadline {
            let registered = !fixture.display_set_registry.lock().unwrap().is_empty();
            let loading = matches!(
                fixture.status_of(target_id),
                TargetStatusDto::Loading { .. }
            );
            if registered && loading {
                observed = true;
                break;
            }
            if loader.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            observed,
            "読み込み中にコア側へ登録済みの状態を観測できなかった（この検査の前提が崩れている）"
        );
        assert!(
            fixture.budget.total_bytes() > 0,
            "読み込み中でも SourceBudget の予約は生きている前提"
        );

        assert!(fixture.close(target_id));
        let events = loader.join().expect("パニックしないはず");

        assert!(events.is_empty(), "閉じた対象のイベントは発行しないはず");
        assert!(
            fixture.display_set_registry.lock().unwrap().is_empty(),
            "閉じた対象のソースがコア側に残っている（統合表示へ混入する）"
        );
        assert_eq!(
            fixture.budget.total_bytes(),
            0,
            "SourceBudget の予約が解放されずに残っている"
        );
        assert!(!fixture.targets.lock().unwrap().request_cancel(target_id));
        assert!(fixture.close_pending_is_empty());
    }

    // 受け入れ条件（P06-5）: 読み込み済みの対象を閉じる従来の経路
    // （active_source_id 経由の close_source）は変わらない。読み込み中では
    // ないため「閉じられた」印も付かない。
    #[test]
    fn close_target_after_load_frees_core_source_and_budget() {
        let contents = "2026/07/28 15:12:23.456 起動しました\n";
        let file = TempFile::create_text("close-after-load", contents);
        let fixture = Fixture::new();
        let target_id = fixture.register_and_begin("a.log", file.path.clone());

        fixture.run(request(target_id, file.path.clone()));
        assert_eq!(fixture.budget.total_bytes(), contents.len() as u64);

        assert!(fixture.close(target_id));

        assert!(fixture.targets.lock().unwrap().list().is_empty());
        assert!(fixture.display_set_registry.lock().unwrap().is_empty());
        assert_eq!(fixture.budget.total_bytes(), 0);
        assert!(
            fixture.close_pending_is_empty(),
            "読み込み中ではないので印は付かないはず"
        );
    }

    // ERR-001: 存在しない対象を閉じても false を返すだけで、何も起きない。
    #[test]
    fn close_target_for_unknown_target_id_returns_false() {
        let fixture = Fixture::new();
        assert!(!fixture.close(9999));
        assert!(fixture.close_pending_is_empty());
    }

    #[test]
    fn change_kind_label_produces_distinct_non_empty_japanese_messages() {
        let labels = [
            change_kind_label(hakutaku_core::ChangeKind::Shrunk),
            change_kind_label(hakutaku_core::ChangeKind::Replaced),
            change_kind_label(hakutaku_core::ChangeKind::Deleted),
        ];
        for label in labels {
            assert!(!label.is_empty());
        }
        assert_ne!(labels[0], labels[1]);
        assert_ne!(labels[1], labels[2]);
        assert_ne!(labels[0], labels[2]);
    }
}
