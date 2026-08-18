//! 予算と予約（`PERF-008`、`PERF-010`）。
//!
//! [`MemoryBudget`] は、グローバルアロケータの計装値（[`crate::allocated_bytes`]）
//! と、まだ実確保に振り替えられていない予約量（`outstanding_reserved_bytes`）を
//! 分けて保持し、[`MemoryBudget::reserve`] で新たな予約の可否を原子的に判定
//! します。
//!
//! # 会計契約（ADR-0003）
//!
//! - `reserve(要求量)` は `allocated_bytes + outstanding_reserved_bytes +
//!   要求量 <= budget_bytes` を、`outstanding_reserved_bytes` に対する CAS
//!   ループで原子的に判定します。加算はすべて `checked_add` で行い、
//!   オーバーフローする要求は拒否します。
//! - 帰属（振り替え）は、予約トークンを所有するコードが実確保の直後に
//!   [`ReservationToken::mark_allocated`] を明示的に呼ぶことで行います。これに
//!   より `outstanding_reserved_bytes` が減り、実確保そのものはアロケータが
//!   自動的に `allocated_bytes` へ計上します（二重計上の回避）。
//! - 未消費の予約は [`ReservationToken`] の `Drop` で自動的に解放されます。
//!
//! # 予約と実確保が二重に数えられる時間窓（既知の性質）
//!
//! [`MemoryBudget::reserve`] が成功した時点で要求量が
//! `outstanding_reserved_bytes` に載り、その予約の下で実際に確保を行うと、同じ
//! メモリが `allocated_bytes`（アロケータの計装）にも載ります。
//! [`ReservationToken::mark_allocated`] を呼ぶまでの間、**この2つは同じメモリを
//! 二重に数えます**。判定式は両者の和を使うため、この時間窓の中では使用量を
//! 最大で確保済みの量だけ過大に見積もり、予約が本来より早く拒否されたり、
//! ソフトしきい値へ本来より早く到達したりし得ます。
//!
//! これは安全側（過大評価）へ倒す設計であり、予算超過を見逃す方向には働きま
//! せん。時間窓を短く保つのは予約トークンを所有する呼び出し側の責務で、実確保の
//! **直後**に [`ReservationToken::mark_allocated`] を呼んでください（ADR-0003
//! 「帰属（振り替え）」）。確保に失敗した場合は呼ばずにトークンを破棄すると、
//! 予約全量が戻ります。
//!
//! # ソフトしきい値との関係（P02-3）
//!
//! [`MemoryBudget`] はソフトしきい値の状態（[`crate::threshold::ThresholdState`]）
//! も保持します。判定は [`MemoryBudget::reserve`] と
//! [`ReservationToken::mark_allocated`] の操作時、および明示的な確認関数
//! [`MemoryBudget::check_soft_threshold`] でだけ行います。詳細な設計判断
//! （エッジ検出、会計イベントとの関係）は [`crate::threshold`] の
//! doc コメントを参照してください。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use crate::private_usage::{PrivateUsageSample, REFERENCE_INDICATOR_MARGIN_BYTES};
use crate::threshold::{
    AccountingEvent, InvalidSoftThresholdPercent, ThresholdEdge, ThresholdState,
};

/// 既定のメモリ予算（バイト）。2 GiB（`CFG-007` の初期値）。
///
/// `PERF-008` の対象は Rust コアプロセスのヒープ確保量の合計であり、WebView2
/// プロセス群は含みません。
pub const DEFAULT_BUDGET_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// プロセス全体で共有する、唯一のグローバル予算インスタンスです。
static GLOBAL_BUDGET: OnceLock<MemoryBudget> = OnceLock::new();

/// プロセス全体で共有するグローバル予算を返します。
///
/// 初回呼び出し時に [`DEFAULT_BUDGET_BYTES`]（2 GiB）で初期化されます。設定
/// ファイルの値で上書きする場合は [`set_global_budget_bytes`] を使ってください。
#[must_use]
pub fn global_budget() -> &'static MemoryBudget {
    GLOBAL_BUDGET.get_or_init(|| MemoryBudget::new(DEFAULT_BUDGET_BYTES))
}

/// グローバル予算の予算値を上書きします（`CFG-007`）。
///
/// # 契約
///
/// **起動シーケンスの初期化フェーズで一度だけ呼び出してください**（P03 が
/// 設定ファイルを読み込んだ直後を想定しています）。既に [`MemoryBudget::reserve`]
/// による予約が行われた後に呼び出すと、進行中の予約は変更前の予算のまま判定
/// されているため、新しい予算に対する整合性は保証されません。
///
/// P03（設定読み込み）より前に着手する本フェーズでは、既定値
/// [`DEFAULT_BUDGET_BYTES`] を組み込み値として持ち、この関数を設定値からの
/// 上書きの入口としてだけ用意します。
pub fn set_global_budget_bytes(budget_bytes: usize) {
    global_budget().set_budget_bytes(budget_bytes);
}

/// 予約が拒否された理由です。
///
/// `requested_bytes`・`allocated_bytes`・`outstanding_reserved_bytes`・
/// `budget_bytes` の4値が、そのまま拒否理由になります。
/// `allocated_bytes + outstanding_reserved_bytes + requested_bytes` が
/// `budget_bytes` を超える場合、またはこの加算自体が `usize` の上限を超える
/// 場合に拒否されます。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservationRejected {
    /// 拒否された要求量（バイト）。
    pub requested_bytes: usize,
    /// 判定時点の未解放確保量のスナップショット（バイト）。
    pub allocated_bytes: usize,
    /// 判定時点の未消費予約量（バイト）。
    pub outstanding_reserved_bytes: usize,
    /// 判定に使った予算（バイト）。
    pub budget_bytes: usize,
}

impl std::fmt::Display for ReservationRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "メモリ予約を拒否しました（要求 {} バイト、確保済み {} バイト、\
             予約済み {} バイト、予算 {} バイト）",
            self.requested_bytes,
            self.allocated_bytes,
            self.outstanding_reserved_bytes,
            self.budget_bytes
        )
    }
}

impl std::error::Error for ReservationRejected {}

/// [`ReservationToken::mark_allocated`] が残量を超えて呼ばれた場合のエラーです。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkAllocatedError {
    /// 振り替えを要求したバイト数。
    pub requested_bytes: usize,
    /// 呼び出し時点でトークンに残っていたバイト数。
    pub remaining_bytes: usize,
}

impl std::fmt::Display for MarkAllocatedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "予約トークンの残量（{} バイト）を超える振り替え（{} バイト）が要求されました",
            self.remaining_bytes, self.requested_bytes
        )
    }
}

impl std::error::Error for MarkAllocatedError {}

/// [`evaluate_reservation`] が予約を拒否した理由です（内部専用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReservationDenial {
    /// `checked_add` が `usize` の上限を超えた。
    Overflow,
    /// 加算自体は成功したが、予算を超えた。
    BudgetExceeded,
}

/// 予約の可否判定だけを行う、副作用のない内部関数です。
///
/// グローバルな計装値を読まず、引数で渡された値だけで判定するため、決定的な
/// 単体テストが書けます（[`MemoryBudget::reserve`] はグローバルな計装値を読んで
/// これを呼び出します）。成功時は新しい `outstanding_reserved_bytes` の値を
/// 返します。
fn evaluate_reservation(
    allocated_snapshot: usize,
    current_reserved: usize,
    request_bytes: usize,
    budget_bytes: usize,
) -> Result<usize, ReservationDenial> {
    let new_reserved = current_reserved
        .checked_add(request_bytes)
        .ok_or(ReservationDenial::Overflow)?;
    let total = allocated_snapshot
        .checked_add(new_reserved)
        .ok_or(ReservationDenial::Overflow)?;
    if total > budget_bytes {
        return Err(ReservationDenial::BudgetExceeded);
    }
    Ok(new_reserved)
}

/// メモリ予算と、未消費の予約量を保持します。
///
/// `budget_bytes` と `outstanding_reserved_bytes` はそれぞれ独立した
/// `AtomicUsize` で保持し、[`Self::reserve`] はロックなしの CAS ループで原子的に
/// 判定・更新します（`PERF-010`）。ソフトしきい値の状態（[`ThresholdState`]）も
/// 1つずつ保持します（P02-3）。
///
/// `Box<dyn Fn>` を含む [`ThresholdState`] は `#[derive(Debug)]` できないため、
/// このフィールドを追加した際、[`std::fmt::Debug`] は手動実装に変更しています。
pub struct MemoryBudget {
    budget_bytes: AtomicUsize,
    outstanding_reserved_bytes: AtomicUsize,
    threshold: ThresholdState,
}

impl std::fmt::Debug for MemoryBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryBudget")
            .field("budget_bytes", &self.budget_bytes())
            .field(
                "outstanding_reserved_bytes",
                &self.outstanding_reserved_bytes(),
            )
            .field("threshold", &self.threshold)
            .finish()
    }
}

impl MemoryBudget {
    /// 指定した予算で新しい `MemoryBudget` を作ります。
    ///
    /// プロセス全体で共有する予算は [`global_budget`] を使ってください。この
    /// コンストラクタは、テストや、将来複数の独立した予算インスタンスが必要に
    /// なった場合のために公開しています。ソフトしきい値は既定割合
    /// （[`crate::DEFAULT_SOFT_THRESHOLD_PERCENT`]）で初期化されます。
    #[must_use]
    pub const fn new(budget_bytes: usize) -> Self {
        MemoryBudget {
            budget_bytes: AtomicUsize::new(budget_bytes),
            outstanding_reserved_bytes: AtomicUsize::new(0),
            threshold: ThresholdState::new(),
        }
    }

    /// 現在の予算（バイト）を返します。
    #[must_use]
    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes.load(Ordering::Relaxed)
    }

    /// 未消費の予約量（バイト）を返します。
    #[must_use]
    pub fn outstanding_reserved_bytes(&self) -> usize {
        self.outstanding_reserved_bytes.load(Ordering::Relaxed)
    }

    /// 予算を上書きします。呼び出し契約は [`set_global_budget_bytes`] の
    /// doc コメントを参照してください。
    ///
    /// **この関数はソフトしきい値を再評価しません。** 新しい予算が判定へ反映
    /// されるのは次回の判定（[`Self::reserve`]・
    /// [`ReservationToken::mark_allocated`]・[`Self::check_soft_threshold`]）
    /// からで、それまで [`Self::prefetch_paused`] は変更前の予算に基づく古い
    /// 判定結果のままです。予算を下げた直後など、その場で反映したい場合は
    /// [`Self::check_soft_threshold`] を明示的に呼んでください。
    pub fn set_budget_bytes(&self, budget_bytes: usize) {
        self.budget_bytes.store(budget_bytes, Ordering::Relaxed);
    }

    /// ソフトしきい値の割合（パーセント）を返します。既定は
    /// [`crate::DEFAULT_SOFT_THRESHOLD_PERCENT`] です（暫定設計）。
    #[must_use]
    pub fn soft_threshold_percent(&self) -> u8 {
        self.threshold.percent()
    }

    /// ソフトしきい値の割合（パーセント）を変更します。
    ///
    /// 有効範囲は 1〜100 です。範囲外（`0` または `101` 以上）は
    /// [`InvalidSoftThresholdPercent`] で拒否し、値は変更しません。
    ///
    /// [`Self::set_budget_bytes`] と同じく、**この関数はソフトしきい値を再評価
    /// しません。** 新しい割合が反映されるのは次回の判定からで、それまで
    /// [`Self::prefetch_paused`] は変更前の割合に基づく古い判定結果のままです。
    /// その場で反映したい場合は [`Self::check_soft_threshold`] を呼んでください。
    pub fn set_soft_threshold_percent(
        &self,
        percent: u8,
    ) -> Result<(), InvalidSoftThresholdPercent> {
        self.threshold.set_percent(percent)
    }

    /// 先読み停止フラグです。消費側（P06・P08）はこれを読んで、しきい値超過中は
    /// 新規の先読みを止めてください。
    ///
    /// 値は**直近に完了したしきい値判定の結果**そのものです（Issue #40）。
    /// しきい値を下回れば解除されますが、それには判定の実行が必要です
    /// （[`Self::reserve`]・[`ReservationToken::mark_allocated`]・
    /// [`Self::check_soft_threshold`] のいずれか）。トークンの `Drop` や
    /// 予算・割合の変更は、それ自体では再評価しません。
    #[must_use]
    pub fn prefetch_paused(&self) -> bool {
        self.threshold.prefetch_paused()
    }

    /// しきい値到達時に呼ぶ解放処理を登録します。
    ///
    /// **実際の解放対象（索引、ログ本文バッファ、表示範囲）の登録は P06・P08 の
    /// 担当です。** ここは呼び出す枠組みだけを提供します。複数回呼ぶと、登録
    /// された全ての処理が到達のたびに（エッジ検出で1回だけ）呼ばれます。
    ///
    /// # 解放処理から会計 API を呼ぶ場合
    ///
    /// 解放処理は、登録簿のロックを手放した状態で呼ばれます（Issue #40）。
    /// そのため、解放処理の内部からこの関数や [`Self::check_soft_threshold`]、
    /// [`Self::reserve`] を呼び直しても、この型の内部ロックで自ロックすることは
    /// ありません。ただし解放処理の実行中に追加した処理は、その回の発火では
    /// 呼ばれず、次回の到達から呼ばれます。
    ///
    /// **呼び出し側自身のロックについては、依然として注意が必要です。** 解放
    /// 処理は [`Self::reserve`] / [`ReservationToken::mark_allocated`] の内部から
    /// 呼ばれるため、呼び出し側がロックを保持したままメモリ予約を行う経路が
    /// あるなら、解放処理からそのロックを取ると再入してデッドロックします
    /// （`src-tauri` はフラグを立てるだけにして、実際の解放を後から安全な地点で
    /// 行う遅延方式でこれを避けています）。
    pub fn register_release_handler(&self, handler: Box<dyn Fn() + Send + Sync>) {
        self.threshold.register_release_handler(handler);
    }

    /// 会計イベント（[`AccountingEvent`]、予約拒否・しきい値到達）の通知先を
    /// 設定します。
    ///
    /// `OnceLock` により、このインスタンスに対して一度だけ設定できます。2回目
    /// 以降の呼び出しは無視され `false` を返します（`true` は設定できたことを
    /// 示します）。
    ///
    /// プロセス全体のグローバル予算（[`global_budget`]）に対しては、
    /// `src-tauri` 側が起動時（ブートストラップで診断ログを開いた後）に一度だけ
    /// 配線し、診断ログ（`DIAG-005`）へ記録します。このクレートは
    /// `hakutaku-diagnostics` へ依存しないため（コア層を薄く保つ設計判断）、
    /// 実際のログ出力は呼び出し側の責務です。
    pub fn set_event_sink(&self, sink: Box<dyn Fn(AccountingEvent) + Send + Sync>) -> bool {
        self.threshold.set_event_sink(sink)
    }

    /// 現在の会計値からソフトしきい値を明示的に確認します。
    ///
    /// [`Self::reserve`] / [`ReservationToken::mark_allocated`] の操作時に加え、
    /// 定期的な確認（例: P02-4 の参考指標計測タイミング）からもこれを呼べます。
    /// 返り値は呼び出し後の [`Self::prefetch_paused`] と同じです（呼び出し側が
    /// 結果をそのままログへ残せるようにするための利便性）。
    #[must_use]
    pub fn check_soft_threshold(&self) -> bool {
        let allocated_snapshot = crate::allocator::allocated_bytes();
        self.check_soft_threshold_with_allocated_snapshot(allocated_snapshot)
    }

    /// [`Self::check_soft_threshold`] の内部実装です。`allocated_snapshot` を
    /// 引数として受け取るため、グローバル計装値に依存しない決定的な単体テストが
    /// 書けます（[`Self::reserve_with_allocated_snapshot`] と同じ切り出し方）。
    ///
    /// 到達エッジを検出した場合は [`AccountingEvent::SoftThresholdReached`] を
    /// 通知先へ送ります（現在値・予約量・予算・ピーク値を含む）。解放処理の
    /// 呼び出しと先読み停止フラグの設定は [`ThresholdState::evaluate`] が
    /// 内部で行います。
    fn check_soft_threshold_with_allocated_snapshot(&self, allocated_snapshot: usize) -> bool {
        let reserved = self.outstanding_reserved_bytes();
        let budget_bytes = self.budget_bytes();
        let current_usage = allocated_snapshot.saturating_add(reserved);

        if let Some(ThresholdEdge::Reached) = self.threshold.evaluate(current_usage, budget_bytes) {
            self.threshold.emit(AccountingEvent::SoftThresholdReached {
                allocated_bytes: allocated_snapshot,
                outstanding_reserved_bytes: reserved,
                budget_bytes,
                peak_bytes: crate::allocator::peak_bytes(),
            });
        }

        self.threshold.prefetch_paused()
    }

    /// 参考指標（`PERF-011`、`PrivateUsage` 合計）の超過を判定します。
    ///
    /// **参考指標であり、合否判定には使いません。** しきい値は
    /// `budget_bytes()` + [`REFERENCE_INDICATOR_MARGIN_BYTES`]（予算値 +
    /// 1 GiB、暫定値。詳細は `private_usage` モジュールの doc コメントを
    /// 参照）です。`sample.total_private_usage_bytes` がこのしきい値を**超えた**
    /// （ちょうどは含まない）場合、[`AccountingEvent::ReferenceIndicatorExceeded`]
    /// を通知先へ送り `true` を返します。超えていなければ何もせず `false` を
    /// 返します。
    ///
    /// [`Self::check_soft_threshold`] と異なりエッジ検出は行いません。この
    /// 参考指標に対する自動対処（進行中の追加読み込みのキャンセル）は P06
    /// 以降の対象外であり、高コストな解放処理を繰り返し呼んでしまう心配が
    /// ないためです。呼び出し側（P04 以降が想定する定期計測）が計測のたびに
    /// これを呼ぶことを想定しています。
    #[must_use]
    pub fn check_reference_indicator(&self, sample: &PrivateUsageSample) -> bool {
        let budget_bytes = self.budget_bytes();
        let limit_bytes = budget_bytes.saturating_add(REFERENCE_INDICATOR_MARGIN_BYTES);
        let exceeded = sample.total_private_usage_bytes > limit_bytes;

        if exceeded {
            self.threshold
                .emit(AccountingEvent::ReferenceIndicatorExceeded {
                    total_private_usage_bytes: sample.total_private_usage_bytes,
                    budget_bytes,
                    limit_bytes,
                });
        }

        exceeded
    }

    /// `request_bytes` の予約を試みます。
    ///
    /// 判定式は `allocated_bytes()`（グローバル計装値） +
    /// `outstanding_reserved_bytes` + `request_bytes` <= `budget_bytes` です。
    /// 成功すると [`ReservationToken`] を返し、`outstanding_reserved_bytes` が
    /// `request_bytes` だけ増えます。予約したまま使わなかった分は、返した
    /// トークンの破棄（`Drop`）で自動的に解放されます。
    pub fn reserve(
        &self,
        request_bytes: usize,
    ) -> Result<ReservationToken<'_>, ReservationRejected> {
        let allocated_snapshot = crate::allocator::allocated_bytes();
        self.reserve_with_allocated_snapshot(request_bytes, allocated_snapshot)
    }

    /// [`Self::reserve`] の内部実装です。`allocated_snapshot` を引数として
    /// 受け取るため、グローバル計装値に依存しない決定的な単体テストが書けます
    /// （テスト容易性のための切り出し）。
    fn reserve_with_allocated_snapshot(
        &self,
        request_bytes: usize,
        allocated_snapshot: usize,
    ) -> Result<ReservationToken<'_>, ReservationRejected> {
        let budget_bytes = self.budget_bytes();
        let mut current_reserved = self.outstanding_reserved_bytes.load(Ordering::Acquire);

        loop {
            let new_reserved = match evaluate_reservation(
                allocated_snapshot,
                current_reserved,
                request_bytes,
                budget_bytes,
            ) {
                Ok(new_reserved) => new_reserved,
                Err(_denial) => {
                    let rejected = ReservationRejected {
                        requested_bytes: request_bytes,
                        allocated_bytes: allocated_snapshot,
                        outstanding_reserved_bytes: current_reserved,
                        budget_bytes,
                    };
                    // 会計イベント: 予約拒否を通知先へ届ける（DIAG-005）。
                    // イベント発火はこの reserve 経路からのみ行い、アロケータ内
                    // からは行わない。
                    self.threshold
                        .emit(AccountingEvent::ReservationRejected(rejected));
                    // 拒否時点の会計値でもしきい値を判定する（ADR-0003: 予約
                    // 操作時に判定する）。拒否されるほど逼迫している場合、既に
                    // しきい値へ到達済みであることが多い。
                    self.check_soft_threshold_with_allocated_snapshot(allocated_snapshot);
                    return Err(rejected);
                }
            };

            match self.outstanding_reserved_bytes.compare_exchange_weak(
                current_reserved,
                new_reserved,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // 会計イベント: 予約成功時点の会計値でしきい値を判定する
                    // （ADR-0003: 予約操作時に判定する。アロケータ内では行わない）。
                    self.check_soft_threshold_with_allocated_snapshot(allocated_snapshot);
                    return Ok(ReservationToken {
                        budget: self,
                        remaining_bytes: AtomicUsize::new(request_bytes),
                    });
                }
                Err(observed) => current_reserved = observed,
            }
        }
    }

    /// `bytes` だけ `outstanding_reserved_bytes` を減らします（下限 0）。
    ///
    /// [`ReservationToken::mark_allocated`] と `Drop` から呼ばれる内部専用の
    /// 解放処理です。
    fn release_reserved(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        // fetch_update は常に Some を返すクロージャを渡しているため Err にはならない。
        // 万一の不整合（本来起きないはず）でも saturating_sub で下限 0 に留め、
        // パニックや巻き戻り（wrap）を起こさない防御的な実装にする。
        let _ = self.outstanding_reserved_bytes.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| Some(current.saturating_sub(bytes)),
        );
    }
}

/// 予約済みで、まだ実確保に振り替えられていない量を表す所有トークンです。
///
/// `Drop` で未消費の残量を自動的に [`MemoryBudget`] へ返却します（`PERF-010`
/// 「予約トークンの破棄で自動的に解放される構造にし、解放漏れを型で防ぐ」）。
#[derive(Debug)]
pub struct ReservationToken<'a> {
    budget: &'a MemoryBudget,
    /// まだ実確保へ振り替えていない残量（バイト）。
    remaining_bytes: AtomicUsize,
}

impl<'a> ReservationToken<'a> {
    /// まだ実確保に振り替えていない残量（バイト）を返します。
    #[must_use]
    pub fn remaining_bytes(&self) -> usize {
        self.remaining_bytes.load(Ordering::Relaxed)
    }

    /// `bytes` を実確保へ振り替えます（ADR-0003「帰属（振り替え）」）。
    ///
    /// 呼び出し側は、予約の下で実際に確保を行った**直後**に、確保したバイト数
    /// でこれを呼んでください。実確保そのものはグローバルアロケータの計装が
    /// 自動的に `allocated_bytes` へ計上するため、ここではトークンの残量と
    /// 予算側の `outstanding_reserved_bytes` を減らすだけです。これにより
    /// 「予約」と「実確保」の二重計上を避けます。
    ///
    /// 振り替え成功時、ソフトしきい値も判定します（ADR-0003: 振り替え操作時に
    /// 判定する。アロケータ内では判定しない）。実確保は既にグローバル計装で
    /// `allocated_bytes` へ計上済みのため、最新の計装値を読み直して判定します。
    ///
    /// # 確保が失敗した場合
    ///
    /// 振り替えは確保の**成功後**に呼んでください。確保が失敗した場合はこの
    /// 関数を呼ばず、トークンをそのまま破棄（drop）すれば、残量全体が予約へ
    /// 戻ります（ADR-0003「確保の失敗」）。
    ///
    /// # エラー
    ///
    /// `bytes` が残量を超える場合は [`MarkAllocatedError`] を返し、状態は
    /// 変更しません。
    pub fn mark_allocated(&self, bytes: usize) -> Result<(), MarkAllocatedError> {
        let mut current_remaining = self.remaining_bytes.load(Ordering::Acquire);

        loop {
            if bytes > current_remaining {
                return Err(MarkAllocatedError {
                    requested_bytes: bytes,
                    remaining_bytes: current_remaining,
                });
            }
            let new_remaining = current_remaining - bytes;

            match self.remaining_bytes.compare_exchange_weak(
                current_remaining,
                new_remaining,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.budget.release_reserved(bytes);
                    // ADR-0003: 振り替え（mark_allocated）操作時にもしきい値を
                    // 判定する。実確保は既に allocated_bytes へ計上済みのため、
                    // ここでは最新の計装値を読み直す。戻り値（prefetch_paused
                    // と同値）はここでは使わない。
                    let _ = self.budget.check_soft_threshold();
                    return Ok(());
                }
                Err(observed) => current_remaining = observed,
            }
        }
    }
}

impl<'a> Drop for ReservationToken<'a> {
    fn drop(&mut self) {
        // 未消費分（振り替えられなかった残量）をまとめて返却する。
        let remaining = self.remaining_bytes.swap(0, Ordering::AcqRel);
        self.budget.release_reserved(remaining);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threshold::DEFAULT_SOFT_THRESHOLD_PERCENT;
    use std::sync::{Arc, Mutex};
    use std::thread;

    // --- evaluate_reservation（純粋関数）の単体テスト ---
    // グローバル状態を一切使わないため、並行実行しても常に決定的。

    // 受け入れ条件: 予測確保量を渡すと、予算を超える場合に予約が拒否される
    // （境界値: ちょうど予算まで可、1 バイト超で拒否）。
    #[test]
    fn evaluate_reservation_accepts_exactly_at_budget_and_rejects_one_byte_over() {
        assert_eq!(evaluate_reservation(0, 0, 1000, 1000), Ok(1000));
        assert_eq!(
            evaluate_reservation(0, 0, 1001, 1000),
            Err(ReservationDenial::BudgetExceeded)
        );
    }

    // 受け入れ条件: 予約量に巨大値を渡しても加算がオーバーフローせず、拒否される
    // （usize 上限付近）。
    #[test]
    fn evaluate_reservation_rejects_overflow_without_panicking() {
        // current_reserved + request_bytes がオーバーフローするケース。
        assert_eq!(
            evaluate_reservation(0, usize::MAX - 10, 100, usize::MAX),
            Err(ReservationDenial::Overflow)
        );
        // allocated_snapshot + new_reserved がオーバーフローするケース。
        assert_eq!(
            evaluate_reservation(usize::MAX - 5, 0, 100, usize::MAX),
            Err(ReservationDenial::Overflow)
        );
    }

    // --- MemoryBudget / ReservationToken の単体テスト ---
    // それぞれ独立した MemoryBudget::new インスタンスを使うため、テスト同士が
    // 干渉せず並行実行しても決定的（global_budget() を使うテストのみ例外）。

    // 受け入れ条件: 予約可否が allocated + reserved + request <= budget で
    // 原子的に判定される（境界値: ちょうど予算まで可、1 バイト超で拒否）。
    // 受け入れ条件: 予約トークンを破棄すると予約量が解放される。
    #[test]
    fn reserve_boundary_accepts_exactly_budget_and_rejects_one_byte_over() {
        let budget = MemoryBudget::new(1000);

        let token = budget
            .reserve_with_allocated_snapshot(1000, 0)
            .expect("予算ちょうどの予約は許可されるはず");
        assert_eq!(budget.outstanding_reserved_bytes(), 1000);
        drop(token);
        assert_eq!(budget.outstanding_reserved_bytes(), 0, "drop で解放される");

        let rejected = budget
            .reserve_with_allocated_snapshot(1001, 0)
            .expect_err("予算を 1 バイト超える予約は拒否されるはず");
        assert_eq!(rejected.requested_bytes, 1001);
        assert_eq!(rejected.allocated_bytes, 0);
        assert_eq!(rejected.outstanding_reserved_bytes, 0);
        assert_eq!(rejected.budget_bytes, 1000);
    }

    // 受け入れ条件: allocated と outstanding_reserved が分けて保持され、
    // それぞれ取得できる（allocated 側のスナップショットが判定に正しく
    // 加算されることを確認する）。
    #[test]
    fn reserve_accounts_for_allocated_snapshot_in_budget_check() {
        let budget = MemoryBudget::new(1000);

        // 既に 400 バイトが実確保済みという想定（allocated_snapshot = 400）。
        // 600 バイトまでの予約は 400 + 600 = 1000 <= 1000 で許可される。
        assert!(budget.reserve_with_allocated_snapshot(600, 400).is_ok());

        // 直前の予約はこの式の終わりで即座に drop され、outstanding は 0 に
        // 戻っている。601 バイトの予約は 400 + 601 = 1001 > 1000 のため拒否
        // される。
        assert!(budget.reserve_with_allocated_snapshot(601, 400).is_err());
    }

    // 受け入れ条件: 予約量に巨大値を渡しても加算がオーバーフローせず、拒否される
    // （usize 上限付近。MemoryBudget::reserve 経由で確認する）。
    #[test]
    fn reserve_rejects_without_overflow_near_usize_max() {
        let budget = MemoryBudget::new(usize::MAX);

        let huge_token = budget
            .reserve_with_allocated_snapshot(usize::MAX - 10, 0)
            .expect("予算が usize::MAX ならこの予約自体は許可される");
        assert_eq!(budget.outstanding_reserved_bytes(), usize::MAX - 10);

        // さらに 100 バイト予約しようとすると outstanding + request が usize の
        // 上限を超える。checked_add で検知し、パニックせず拒否される。
        let rejected = budget
            .reserve_with_allocated_snapshot(100, 0)
            .expect_err("usize の上限を超える予約はオーバーフローせず拒否されるはず");
        assert_eq!(rejected.requested_bytes, 100);
        assert_eq!(rejected.outstanding_reserved_bytes, usize::MAX - 10);

        drop(huge_token);
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }

    // 受け入れ条件: 並行して予約しても、合計が予算を超えない。
    // allocated_snapshot を 0 に固定した内部関数を使うため、実際のグローバル
    // アロケーション量に依存せず決定的に判定できる。
    #[test]
    fn concurrent_reservations_never_exceed_budget() {
        let budget = Arc::new(MemoryBudget::new(1000));
        let per_thread_request = 130usize;
        let thread_count = 16;

        let handles: Vec<_> = (0..thread_count)
            .map(|_| {
                let budget = Arc::clone(&budget);
                thread::spawn(move || {
                    match budget.reserve_with_allocated_snapshot(per_thread_request, 0) {
                        Ok(token) => {
                            // このテストでは CAS ループの正しさ（合計が予算を
                            // 超えないこと）だけを確認したいため、成功した
                            // トークンは意図的に drop させず保持し続けた扱いに
                            // する（mem::forget）。トークンの Drop による解放は
                            // 別テストで検証する。
                            std::mem::forget(token);
                            true
                        }
                        Err(_) => false,
                    }
                })
            })
            .collect();

        let success_count = handles
            .into_iter()
            .map(|handle| handle.join().expect("パニックしないはず"))
            .filter(|succeeded| *succeeded)
            .count();

        // 予算 1000 バイトに対して 130 バイトずつ予約するので、
        // 7 個まで（7 * 130 = 910 <= 1000 < 8 * 130 = 1040）しか成功しない。
        assert_eq!(success_count, 7);
        assert_eq!(budget.outstanding_reserved_bytes(), 7 * per_thread_request);
        assert!(budget.outstanding_reserved_bytes() <= budget.budget_bytes());
    }

    // 受け入れ条件: 実確保時に予約から実確保へ振り替えられ、二重計上されない
    // （reserved が減る）。
    #[test]
    fn mark_allocated_reduces_remaining_and_outstanding_without_double_counting() {
        let budget = MemoryBudget::new(1000);
        let token = budget
            .reserve_with_allocated_snapshot(500, 0)
            .expect("予約できるはず");

        assert_eq!(token.remaining_bytes(), 500);
        assert_eq!(budget.outstanding_reserved_bytes(), 500);

        token
            .mark_allocated(300)
            .expect("残量内の振り替えは成功するはず");

        assert_eq!(token.remaining_bytes(), 200, "振り替えた分だけ残量が減る");
        assert_eq!(
            budget.outstanding_reserved_bytes(),
            200,
            "振り替えた分だけ outstanding_reserved_bytes も減り、二重計上されない"
        );

        drop(token);
        assert_eq!(
            budget.outstanding_reserved_bytes(),
            0,
            "drop で残りの未消費分 200 バイトが解放される"
        );
    }

    // 受け入れ条件: 残量を超える mark_allocated がエラーで検知される。
    #[test]
    fn mark_allocated_beyond_remaining_is_rejected_and_state_unchanged() {
        let budget = MemoryBudget::new(1000);
        let token = budget
            .reserve_with_allocated_snapshot(100, 0)
            .expect("予約できるはず");

        let error = token
            .mark_allocated(101)
            .expect_err("残量を超える振り替えはエラーになるはず");
        assert_eq!(error.requested_bytes, 101);
        assert_eq!(error.remaining_bytes, 100);

        // 状態は変化していない。
        assert_eq!(token.remaining_bytes(), 100);
        assert_eq!(budget.outstanding_reserved_bytes(), 100);
    }

    // 受け入れ条件: 確保が失敗した場合に予約が戻る。
    // ADR-0003「確保の失敗」: 振り替えは確保の成功後に行う契約のため、確保が
    // 失敗した経路は「mark_allocated を一度も呼ばずに token を破棄する」ことで
    // シミュレートできる（トークン破棄で予約全量が戻る）。
    #[test]
    fn token_drop_restores_full_reservation_when_simulated_allocation_fails() {
        let budget = MemoryBudget::new(1000);
        let token = budget
            .reserve_with_allocated_snapshot(400, 0)
            .expect("予約できるはず");
        assert_eq!(budget.outstanding_reserved_bytes(), 400);

        // 実確保が失敗したと仮定し、mark_allocated を呼ばずに破棄する。
        drop(token);

        assert_eq!(
            budget.outstanding_reserved_bytes(),
            0,
            "確保に失敗した場合、予約全量がそのままトークンの破棄で戻る"
        );
    }

    // 受け入れ条件: 予約量より少ない確保で差分が解放され、予約したまま残らない。
    #[test]
    fn token_drop_after_partial_mark_allocated_releases_only_the_difference() {
        let budget = MemoryBudget::new(1000);
        let token = budget
            .reserve_with_allocated_snapshot(500, 0)
            .expect("予約できるはず");

        token.mark_allocated(150).expect("残量内なので成功するはず");
        assert_eq!(budget.outstanding_reserved_bytes(), 350);

        drop(token);
        assert_eq!(
            budget.outstanding_reserved_bytes(),
            0,
            "一部消費後の残り 350 バイトが drop で解放される"
        );
    }

    // 受け入れ条件: 予約後に予算の 100% を超える処理が開始されない。
    // reserve が Err を返すことそのものが「処理を開始しない」を保証する
    // （呼び出し側は Err の場合、確保コードへ進めない）。
    #[test]
    fn reserve_never_allows_starting_work_that_exceeds_full_budget() {
        let budget = MemoryBudget::new(1000);

        let first = budget
            .reserve_with_allocated_snapshot(1000, 0)
            .expect("予算ちょうどまでは許可される");
        // 予算を使い切った状態でさらに 1 バイトでも要求すると拒否される。
        assert!(budget.reserve_with_allocated_snapshot(1, 0).is_err());

        drop(first);
    }

    // 公開 API reserve() が内部関数へ正しく委譲することを確認する（スモーク
    // テスト）。このテストバイナリ（cargo test --lib）にはグローバル
    // アロケータとして CountingAllocator が設置されていないため、
    // allocator::allocated_bytes() は常に 0 を返す。実アロケータと組み合わせた
    // 検証は tests/ 配下の統合テストで行う。
    #[test]
    fn public_reserve_delegates_to_internal_evaluation() {
        let budget = MemoryBudget::new(1000);
        let token = budget.reserve(500).expect("予算内なら許可されるはず");
        assert_eq!(token.remaining_bytes(), 500);
        assert_eq!(budget.outstanding_reserved_bytes(), 500);
    }

    // 受け入れ条件: 既定 2 GiB を予算として扱い、設定値を与えるとその値が予算に
    // なる（global_budget / set_global_budget_bytes の契約確認）。
    //
    // このテストだけがプロセス全体で共有される global_budget() を操作する。
    // 他の単体テストは MemoryBudget::new で独立インスタンスを使うため、この
    // テストと干渉しない。cargo test はテスト関数を並行実行するため、この
    // テスト自身が複数回同時に走ることはない前提で、Mutex による直列化までは
    // 行わないが、念のため最後に既定値へ戻し、他テストに影響を残さないように
    // する。
    #[test]
    fn global_budget_default_and_override() {
        static SERIALIZE: Mutex<()> = Mutex::new(());
        let _guard = SERIALIZE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let original = global_budget().budget_bytes();
        assert_eq!(
            original, DEFAULT_BUDGET_BYTES,
            "既定値は 2 GiB のはず（他テストが変更していないこと前提）"
        );

        set_global_budget_bytes(123_456);
        assert_eq!(global_budget().budget_bytes(), 123_456);

        // 他テストへの影響を避けるため、既定値へ戻す。
        set_global_budget_bytes(DEFAULT_BUDGET_BYTES);
    }

    // --- ソフトしきい値（P02-3）の単体テスト ---
    // それぞれ独立した MemoryBudget::new インスタンスを使うため、テスト同士が
    // 干渉せず並行実行しても決定的。allocated_snapshot は
    // reserve_with_allocated_snapshot 経由で 0 に固定し、グローバルな
    // アロケーション量に依存しない（このテストバイナリには CountingAllocator が
    // 設置されていないため crate::allocator::allocated_bytes() は常に 0 を返す。
    // check_soft_threshold() を直接呼ぶテストはこの事実に依拠している）。

    // 受け入れ条件: しきい値到達を検知し、登録された解放処理と先読み停止が
    // 呼ばれる。
    #[test]
    fn soft_threshold_reached_calls_release_handlers_and_pauses_prefetch() {
        let budget = MemoryBudget::new(1000);
        budget
            .set_soft_threshold_percent(50)
            .expect("50は有効な割合のはず");
        assert!(!budget.prefetch_paused());

        let call_count = Arc::new(Mutex::new(0u32));
        let call_count_handler = Arc::clone(&call_count);
        budget.register_release_handler(Box::new(move || {
            *call_count_handler.lock().unwrap() += 1;
        }));

        // 予算1000、しきい値50% = 500バイト。600バイトの予約でしきい値を跨ぐ。
        let token = budget
            .reserve_with_allocated_snapshot(600, 0)
            .expect("予算内なので予約は成功するはず");

        assert_eq!(
            *call_count.lock().unwrap(),
            1,
            "到達時に1回だけ呼ばれるはず"
        );
        assert!(budget.prefetch_paused(), "先読み停止フラグが立つはず");

        drop(token);
    }

    // 受け入れ条件: しきい値未満では解放処理が呼ばれない。
    #[test]
    fn soft_threshold_not_reached_does_not_call_release_handlers() {
        let budget = MemoryBudget::new(1000);
        budget
            .set_soft_threshold_percent(50)
            .expect("50は有効な割合のはず");

        let call_count = Arc::new(Mutex::new(0u32));
        let call_count_handler = Arc::clone(&call_count);
        budget.register_release_handler(Box::new(move || {
            *call_count_handler.lock().unwrap() += 1;
        }));

        // しきい値500バイトに対して400バイトの予約は未到達。
        let token = budget
            .reserve_with_allocated_snapshot(400, 0)
            .expect("予算内なので予約は成功するはず");

        assert_eq!(
            *call_count.lock().unwrap(),
            0,
            "しきい値未満では呼ばれないはず"
        );
        assert!(!budget.prefetch_paused());

        drop(token);
    }

    // 受け入れ条件: しきい値を下回ると先読み停止が解除され、イベントが
    // 再武装される（再び到達すると改めて発火する）。
    #[test]
    fn prefetch_paused_clears_and_rearms_after_dropping_below_threshold() {
        let budget = MemoryBudget::new(1000);
        budget
            .set_soft_threshold_percent(50)
            .expect("50は有効な割合のはず");

        let call_count = Arc::new(Mutex::new(0u32));
        let call_count_handler = Arc::clone(&call_count);
        budget.register_release_handler(Box::new(move || {
            *call_count_handler.lock().unwrap() += 1;
        }));

        let first_token = budget
            .reserve_with_allocated_snapshot(600, 0)
            .expect("予算内なので予約は成功するはず");
        assert_eq!(*call_count.lock().unwrap(), 1);
        assert!(budget.prefetch_paused());

        // 予約を解放する（Drop は自動では再評価しないため、明示的な確認関数
        // check_soft_threshold() で再評価する）。
        drop(first_token);
        assert!(
            !budget.check_soft_threshold(),
            "予約解放後は現在値0がしきい値未満のため、しきい値は未到達のはず"
        );
        assert!(
            !budget.prefetch_paused(),
            "しきい値を下回ったら先読み停止が解除されるはず"
        );

        // 再武装後、再び到達すると改めて発火する。
        let second_token = budget
            .reserve_with_allocated_snapshot(600, 0)
            .expect("予算内なので予約は成功するはず");
        assert_eq!(
            *call_count.lock().unwrap(),
            2,
            "再武装後に再び到達したら、もう一度発火するはず"
        );
        assert!(budget.prefetch_paused());

        drop(second_token);
    }

    // 受け入れ条件（Issue #40）: しきい値到達で呼ばれた解放処理の内部から
    // 公開 API（解放処理の追加登録、明示的なしきい値確認、予約）へ再入しても
    // デッドロックしない。
    //
    // 解放処理を登録簿のロック保持中に呼ぶ実装では、ハンドラ内の
    // register_release_handler が同じ Mutex を取り直してハングするため、
    // 時間制限つきの待機が発火してこのテストは失敗する。
    #[test]
    fn release_handler_reentering_public_api_does_not_deadlock() {
        let budget = Arc::new(MemoryBudget::new(1000));
        budget
            .set_soft_threshold_percent(50)
            .expect("50は有効な割合のはず");

        // 解放処理は `'static` である必要があるため、`Arc` の循環参照（予算
        // 自身が解放されなくなる）を避けて `Weak` を捕捉する。
        let weak = Arc::downgrade(&budget);
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_handler = Arc::clone(&call_count);
        budget.register_release_handler(Box::new(move || {
            let Some(budget) = weak.upgrade() else {
                return;
            };
            // 実配線（src-tauri）はフラグを立てるだけだが、ここでは会計 API へ
            // 再入する最悪の形を意図的に作る。いずれも既に armed が倒れている
            // ため、ここから解放処理が再発火して無限再帰することはない。
            budget.register_release_handler(Box::new(|| {}));
            let _ = budget.check_soft_threshold();
            drop(budget.reserve(1));
            call_count_handler.fetch_add(1, Ordering::Relaxed);
        }));

        // デッドロックした場合にテストが永久にハングしないよう、別スレッドで
        // 実行して時間制限つきで完了を待つ。
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker_budget = Arc::clone(&budget);
        let worker = thread::spawn(move || {
            // 予算1000、しきい値50% = 500バイト。600バイトの予約で跨ぐ。
            let token = worker_budget
                .reserve_with_allocated_snapshot(600, 0)
                .expect("予算内なので予約は成功するはず");
            drop(token);
            let _ = done_tx.send(());
        });
        done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("解放処理からの再入でデッドロックしている（10秒以内に完了しない）");
        worker.join().expect("パニックしないはず");

        assert_eq!(
            call_count.load(Ordering::Relaxed),
            1,
            "到達エッジで解放処理が1回だけ呼ばれ、再入も完了しているはず"
        );
    }

    // 受け入れ条件: 予約の拒否イベントが通知先へ届く（テスト用の通知先を
    // 登録して検証する）。
    #[test]
    fn reservation_rejected_event_delivered_to_event_sink() {
        let budget = MemoryBudget::new(1000);
        let events: Arc<Mutex<Vec<AccountingEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_sink = Arc::clone(&events);
        let set_ok = budget.set_event_sink(Box::new(move |event| {
            events_sink.lock().unwrap().push(event);
        }));
        assert!(set_ok, "初回の設定は成功するはず");

        let rejected = budget
            .reserve_with_allocated_snapshot(1001, 0)
            .expect_err("予算1000に対して1001バイトの予約は拒否されるはず");

        let recorded = events.lock().unwrap();
        assert_eq!(recorded.len(), 1, "拒否イベントが1件届くはず");
        match recorded[0] {
            AccountingEvent::ReservationRejected(actual) => {
                assert_eq!(actual, rejected, "届いたイベントの内容が一致するはず");
            }
            AccountingEvent::SoftThresholdReached { .. } => {
                panic!("ReservationRejected イベントが届くはずが、しきい値到達イベントだった")
            }
            AccountingEvent::ReferenceIndicatorExceeded { .. } => {
                panic!("ReservationRejected イベントが届くはずが、参考指標超過イベントだった")
            }
        }
    }

    // 2回目の set_event_sink は無視され、最初に登録した通知先だけが使われる
    // ことを確認する（OnceLock による一度だけの設定という契約の確認）。
    #[test]
    fn set_event_sink_can_only_be_set_once() {
        let budget = MemoryBudget::new(1000);
        assert!(budget.set_event_sink(Box::new(|_event| {})));
        assert!(
            !budget.set_event_sink(Box::new(|_event| {})),
            "2回目の設定は false を返すはず"
        );
    }

    // 受け入れ条件: しきい値の割合変更が判定へ反映される（境界値。公開 API
    // MemoryBudget::set_soft_threshold_percent 経由での確認）。
    #[test]
    fn set_soft_threshold_percent_changes_the_evaluation_boundary() {
        let budget = MemoryBudget::new(1000);
        budget
            .set_soft_threshold_percent(10)
            .expect("10は有効な割合のはず"); // しきい値 = 100バイト

        let call_count = Arc::new(Mutex::new(0u32));
        let call_count_handler = Arc::clone(&call_count);
        budget.register_release_handler(Box::new(move || {
            *call_count_handler.lock().unwrap() += 1;
        }));

        // 99バイトはしきい値未満（境界の1つ下）。
        let below = budget
            .reserve_with_allocated_snapshot(99, 0)
            .expect("予算内なので予約は成功するはず");
        assert_eq!(
            *call_count.lock().unwrap(),
            0,
            "境界の1つ下では到達しないはず"
        );
        drop(below);
        assert!(!budget.check_soft_threshold());

        // 100バイトはちょうどしきい値（10%）。
        let at_threshold = budget
            .reserve_with_allocated_snapshot(100, 0)
            .expect("予算内なので予約は成功するはず");
        assert_eq!(
            *call_count.lock().unwrap(),
            1,
            "ちょうどしきい値で到達するはず"
        );
        assert!(budget.prefetch_paused());

        drop(at_threshold);
    }

    // 受け入れ条件: 不正な割合（0、101）が拒否される（公開 API
    // MemoryBudget::set_soft_threshold_percent 経由での確認）。
    #[test]
    fn set_soft_threshold_percent_rejects_out_of_range_values() {
        let budget = MemoryBudget::new(1000);
        assert_eq!(
            budget.soft_threshold_percent(),
            DEFAULT_SOFT_THRESHOLD_PERCENT
        );

        let error_zero = budget
            .set_soft_threshold_percent(0)
            .expect_err("0は不正な割合のはず");
        assert_eq!(error_zero.requested_percent, 0);
        assert_eq!(
            budget.soft_threshold_percent(),
            DEFAULT_SOFT_THRESHOLD_PERCENT,
            "拒否された場合は値が変わらないはず"
        );

        let error_over = budget
            .set_soft_threshold_percent(101)
            .expect_err("101は不正な割合のはず");
        assert_eq!(error_over.requested_percent, 101);

        // 境界値の1と100は許可される。
        budget
            .set_soft_threshold_percent(1)
            .expect("1は有効な割合のはず");
        assert_eq!(budget.soft_threshold_percent(), 1);
        budget
            .set_soft_threshold_percent(100)
            .expect("100は有効な割合のはず");
        assert_eq!(budget.soft_threshold_percent(), 100);
    }

    // --- 参考指標（PERF-011、P02-4）の単体テスト ---
    // それぞれ独立した MemoryBudget::new インスタンスを使うため、テスト同士が
    // 干渉せず並行実行しても決定的。

    fn sample_with_total(total_private_usage_bytes: usize) -> PrivateUsageSample {
        PrivateUsageSample {
            total_private_usage_bytes,
            processes: Vec::new(),
            skipped_count: 0,
        }
    }

    // 受け入れ条件: 参考指標の超過検知（しきい値比較とイベント発火）。
    // しきい値は budget_bytes + REFERENCE_INDICATOR_MARGIN_BYTES（境界値:
    // ちょうどしきい値は超過扱いにせず、1 バイト超えで超過扱いにする）。
    #[test]
    fn check_reference_indicator_fires_event_when_exceeding_budget_plus_margin() {
        let budget = MemoryBudget::new(1000);
        let events: Arc<Mutex<Vec<AccountingEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_sink = Arc::clone(&events);
        budget.set_event_sink(Box::new(move |event| {
            events_sink.lock().unwrap().push(event);
        }));

        let limit_bytes = 1000usize.saturating_add(REFERENCE_INDICATOR_MARGIN_BYTES);

        // ちょうどしきい値は超過扱いにしない。
        let at_limit = sample_with_total(limit_bytes);
        assert!(
            !budget.check_reference_indicator(&at_limit),
            "ちょうどしきい値は超過扱いにしないはず"
        );
        assert!(
            events.lock().unwrap().is_empty(),
            "超過しない場合はイベントが発火しないはず"
        );

        // 1 バイト超えると超過扱いになる。
        let over_limit = sample_with_total(limit_bytes + 1);
        assert!(
            budget.check_reference_indicator(&over_limit),
            "しきい値を1バイト超えたら超過扱いになるはず"
        );

        let recorded = events.lock().unwrap();
        assert_eq!(recorded.len(), 1, "超過イベントが1件届くはず");
        match recorded[0] {
            AccountingEvent::ReferenceIndicatorExceeded {
                total_private_usage_bytes,
                budget_bytes,
                limit_bytes: actual_limit_bytes,
            } => {
                assert_eq!(total_private_usage_bytes, limit_bytes + 1);
                assert_eq!(budget_bytes, 1000);
                assert_eq!(actual_limit_bytes, limit_bytes);
            }
            _ => panic!("ReferenceIndicatorExceeded イベントが届くはず"),
        }
    }

    // 受け入れ条件: 超過しない場合はイベントが発火しないこと。
    #[test]
    fn check_reference_indicator_does_not_fire_when_below_limit() {
        let budget = MemoryBudget::new(1000);
        let events: Arc<Mutex<Vec<AccountingEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_sink = Arc::clone(&events);
        budget.set_event_sink(Box::new(move |event| {
            events_sink.lock().unwrap().push(event);
        }));

        let well_below_limit = sample_with_total(500);
        assert!(!budget.check_reference_indicator(&well_below_limit));
        assert!(events.lock().unwrap().is_empty());
    }
}
