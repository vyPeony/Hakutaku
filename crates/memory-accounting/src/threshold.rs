//! ソフトしきい値の枠組み（P02-3、`tasks/phase-02-memory-accounting.md`
//! 「5. ソフトしきい値の枠組み」）。
//!
//! # 位置づけと暫定性
//!
//! しきい値の具体的な割合（計画正本 5.2 が挙げる 90%）は**暫定設計であり、
//! 要件 ID を持ちません**。[`DEFAULT_SOFT_THRESHOLD_PERCENT`] は実測に基づいて
//! 将来調整され得る値として扱ってください。割合は
//! [`super::MemoryBudget::set_soft_threshold_percent`] で変更できます。
//!
//! **実際に解放する対象（索引、ログ本文バッファ、表示範囲）の登録は本クレートの
//! 対象外です**（P06・P08）。ここが持つのは、登録された解放処理を呼び出す
//! 枠組みと、消費側が読む先読み停止フラグだけです。
//!
//! # アロケータ経路では判定しない（ADR-0003）
//!
//! しきい値判定は `crate::allocator`（`alloc` / `dealloc` 経路）では一切行い
//! ません。[`super::MemoryBudget::reserve`]・[`super::ReservationToken::
//! mark_allocated`]・[`super::MemoryBudget::check_soft_threshold`] の各操作時に
//! だけ判定します。アロケータ内で確保・ロック取得・ログ出力を伴う判定を行うと
//! 再入・デッドロックの危険があるため（ADR-0003「ソフトしきい値の検知と通知は
//! アロケータの alloc/dealloc 経路では行わない」）、判定はすべて会計サービス側
//! （このモジュールと [`super::budget`]）に置きます。
//!
//! # エッジ検出（設計判断）
//!
//! しきい値到達は**エッジ検出**で扱います。しきい値を超えている間、判定の
//! たびに登録された解放処理を呼び直すことはしません。呼び出し側の解放処理は
//! コストの大きい処理（索引の破棄など）を行う可能性があり、超過が続く間に
//! 操作のたびへ繰り返し呼ぶと悪影響の方が大きいためです。一度発火したら
//! 「armed」フラグを倒し、値がしきい値を**下回った**ときにだけ再武装します。
//! 再武装後、再びしきい値へ到達すると改めて発火します。
//!
//! 先読み停止フラグ（[`super::MemoryBudget::prefetch_paused`]）は、エッジでは
//! なく**判定のたびにその時点の判定結果そのもの**で更新します（Issue #40）。
//! エッジ検出は「解放処理を呼ぶかどうか」だけに使い、フラグの値には使いません。
//! 両者を連動させると、2スレッドが同時に [`ThresholdState::evaluate`] を呼んだ
//! ときに「再武装側の解除 → 到達側の設定」の順で確定し、しきい値未満なのに
//! フラグが `true` のまま固着し得るためです（詳細は
//! [`ThresholdState::evaluate`] の doc コメント）。
//!
//! # 解放処理はロックの外で呼ぶ
//!
//! 登録された解放処理は、登録簿（`Mutex`）から複製を取り出して**ロックを
//! 手放してから**呼びます（Issue #40）。ロックを保持したまま呼ぶと、解放処理の
//! 内部から会計 API へ再入したときに同じ `Mutex` を取り直して自ロックします
//! （`std::sync::Mutex` は非再入）。
//!
//! # 会計イベントと診断ログの分離
//!
//! `crates/memory-accounting` は `hakutaku-diagnostics` に依存しません（コアの
//! 層を薄く保つ設計判断）。代わりに [`AccountingEvent`] という中立な型を定義し、
//! [`super::MemoryBudget::set_event_sink`] の登録口経由で、呼び出し側
//! （`src-tauri`）が受け取って診断ログ（`DIAG-005`）へ記録します。

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::budget::ReservationRejected;

/// 既定のソフトしきい値（予算に対する割合、パーセント）です。
///
/// **暫定設計であり、要件 ID を持ちません**（`tasks/phase-02-memory-accounting.md`
/// 5.2 の 90% を暫定的に採用したものです）。実測に基づいて調整される可能性が
/// あります。変更する場合は [`super::MemoryBudget::set_soft_threshold_percent`]
/// を使ってください。
pub const DEFAULT_SOFT_THRESHOLD_PERCENT: u8 = 90;

/// [`super::MemoryBudget::set_soft_threshold_percent`] に不正な割合（`0` または
/// `101` 以上）を渡した場合のエラーです。有効範囲は 1〜100（パーセント）です。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidSoftThresholdPercent {
    /// 呼び出し側が指定した割合。
    pub requested_percent: u8,
}

impl fmt::Display for InvalidSoftThresholdPercent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ソフトしきい値の割合は1〜100（パーセント）で指定してください（指定値: {}）",
            self.requested_percent
        )
    }
}

impl std::error::Error for InvalidSoftThresholdPercent {}

/// 会計イベント（`DIAG-005` 出力用）です。
///
/// このクレートは診断ログクレートへ依存しないため、実際のログ出力は
/// [`super::MemoryBudget::set_event_sink`] で登録した通知先（呼び出し側）の
/// 責務です。イベント発火は [`super::MemoryBudget::reserve`]・しきい値判定・
/// 参考指標の超過判定の経路からのみ行い、アロケータ内からは発火しません。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountingEvent {
    /// [`super::MemoryBudget::reserve`] が予算超過で予約を拒否した。
    ReservationRejected(ReservationRejected),
    /// ソフトしきい値へ到達した（エッジ検出。連続した超過中は再送しない）。
    SoftThresholdReached {
        /// 判定時点の未解放確保量（バイト、現在値）。
        allocated_bytes: usize,
        /// 判定時点の未消費予約量（バイト）。
        outstanding_reserved_bytes: usize,
        /// 判定に使った予算（バイト）。
        budget_bytes: usize,
        /// 判定時点の観測ピーク値（バイト）。
        peak_bytes: usize,
    },
    /// 参考指標（`PERF-011`、`PrivateUsage` 合計）が予算値 + 1 GiB
    /// （[`crate::REFERENCE_INDICATOR_MARGIN_BYTES`]、暫定値）を超えた。
    ///
    /// [`super::MemoryBudget::check_reference_indicator`] が呼ばれるたびに
    /// 判定します。ソフトしきい値と異なりエッジ検出は行わないため、超過が
    /// 続く間は呼び出しのたびに発火し得ます（P02-3 の doc コメントとの違いに
    /// 注意）。**合否判定には使いません**（参考指標）。
    ReferenceIndicatorExceeded {
        /// 判定時点の `PrivateUsage` 合計（バイト）。
        total_private_usage_bytes: usize,
        /// 判定に使った予算（バイト、`PERF-008`）。
        budget_bytes: usize,
        /// 判定に使ったしきい値（バイト。`budget_bytes` +
        /// [`crate::REFERENCE_INDICATOR_MARGIN_BYTES`]）。
        limit_bytes: usize,
    },
}

/// [`ThresholdState::evaluate`] が検出した状態遷移（エッジ）です。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThresholdEdge {
    /// 下から上（未到達 → 到達）へ跨いだ。
    Reached,
    /// 上から下（到達 → 未到達）へ跨ぎ、再武装した。
    Rearmed,
}

/// ソフトしきい値の状態です。[`super::MemoryBudget`] が1つずつ保持します。
///
/// テスト容易性のため、通知先（[`AccountingEvent`] の送り先）と解放処理の
/// 登録口は、クレート全体で共有するグローバルな状態ではなく、この
/// `MemoryBudget` インスタンス単位で保持します（`OnceLock` を使ってはいますが、
/// スコープはプロセス全体ではなく個々のインスタンスです）。プロセス全体で
/// 共有するグローバル予算（[`super::global_budget`]）に対しては、`src-tauri`
/// 側がそのグローバルインスタンス1つに対して起動時に一度だけ配線することを
/// 想定しています。
pub(crate) struct ThresholdState {
    /// 予算に対する割合（パーセント、1〜100）。
    percent: AtomicU8,
    /// 解放処理を発火させるエッジ検出の武装状態。`true` = 未到達（次に到達
    /// すると発火する）。先読み停止フラグの値には使いません（Issue #40。
    /// 理由は [`Self::evaluate`] の doc コメント）。
    armed: AtomicBool,
    /// 消費側が読む先読み停止フラグ。[`Self::evaluate`] が判定のたびに、その
    /// 時点の判定結果そのもので上書きします。
    prefetch_paused: AtomicBool,
    /// 到達時に呼び出す解放処理（P06・P08 が実対象を登録する。ここでは
    /// 呼び出す枠組みだけを持つ）。
    ///
    /// `Box` ではなく `Arc` で保持するのは、発火時にロックの外へ持ち出せる
    /// 複製を安価に作るためです（Issue #40。[`Self::fire_release_handlers`]）。
    release_handlers: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
    /// 会計イベントの通知先。一度設定したら変更しない（`OnceLock`）。
    event_sink: OnceLock<Box<dyn Fn(AccountingEvent) + Send + Sync>>,
}

impl fmt::Debug for ThresholdState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let handlers_len = self
            .release_handlers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        f.debug_struct("ThresholdState")
            .field("percent", &self.percent.load(Ordering::Relaxed))
            .field("armed", &self.armed.load(Ordering::Relaxed))
            .field(
                "prefetch_paused",
                &self.prefetch_paused.load(Ordering::Relaxed),
            )
            .field("release_handlers_len", &handlers_len)
            .field("event_sink_set", &self.event_sink.get().is_some())
            .finish()
    }
}

impl ThresholdState {
    pub(crate) const fn new() -> Self {
        ThresholdState {
            percent: AtomicU8::new(DEFAULT_SOFT_THRESHOLD_PERCENT),
            armed: AtomicBool::new(true),
            prefetch_paused: AtomicBool::new(false),
            release_handlers: Mutex::new(Vec::new()),
            event_sink: OnceLock::new(),
        }
    }

    pub(crate) fn percent(&self) -> u8 {
        self.percent.load(Ordering::Relaxed)
    }

    pub(crate) fn set_percent(&self, percent: u8) -> Result<(), InvalidSoftThresholdPercent> {
        if percent == 0 || percent > 100 {
            return Err(InvalidSoftThresholdPercent {
                requested_percent: percent,
            });
        }
        self.percent.store(percent, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn prefetch_paused(&self) -> bool {
        self.prefetch_paused.load(Ordering::Relaxed)
    }

    pub(crate) fn register_release_handler(&self, handler: Box<dyn Fn() + Send + Sync>) {
        // 発火中（fire_release_handlers の実行中）にこの関数が呼ばれても、
        // 発火側は既にロックを手放しているためデッドロックしない（Issue #40）。
        // ただし、その回の発火では複製済みの一覧を呼ぶため、ここで追加した
        // 処理は次回の到達から呼ばれる。
        //
        // 公開 API の引数の型（Box）を保ったまま Arc へ移し替える。呼び出し側
        // （src-tauri）の署名を変えずに、発火時の複製を安価にするため。
        let mut handlers = self
            .release_handlers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        handlers.push(Arc::from(handler));
    }

    pub(crate) fn set_event_sink(&self, sink: Box<dyn Fn(AccountingEvent) + Send + Sync>) -> bool {
        self.event_sink.set(sink).is_ok()
    }

    pub(crate) fn emit(&self, event: AccountingEvent) {
        if let Some(sink) = self.event_sink.get() {
            sink(event);
        }
    }

    /// 登録された解放処理を、**登録簿のロックを手放してから**呼びます。
    ///
    /// ロックを保持したまま呼ぶと、解放処理の内部から会計 API（予約、振り替え、
    /// [`Self::evaluate`] を経由する明示的な確認、解放処理の追加登録）へ再入した
    /// ときに、同じ `Mutex` を取り直して自ロックします（`std::sync::Mutex` は
    /// 非再入。Issue #40）。そのため、ロック内では `Arc` の複製だけを集め、
    /// 呼び出しはロックの外で行います。
    ///
    /// この複製は `Vec` の確保を伴いますが、しきい値判定はアロケータの
    /// `alloc`／`dealloc` 経路では行わない（ADR-0003、このモジュールの
    /// doc コメント）ため、ここでの確保は再入の危険になりません。
    fn fire_release_handlers(&self) {
        let handlers: Vec<Arc<dyn Fn() + Send + Sync>> = {
            let guard = self
                .release_handlers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.iter().map(Arc::clone).collect()
        };
        for handler in handlers {
            handler();
        }
    }

    /// しきい値を判定します。`current_usage_bytes` は呼び出し側が組み立てた
    /// 「確保済み + 予約済み」の合計です。
    ///
    /// - 先読み停止フラグは、**呼び出しのたびにその回の判定結果**（しきい値
    ///   以上かどうか）で上書きします。戻り値がエッジかどうかとは無関係です。
    /// - 未到達 → 到達のエッジでだけ [`ThresholdEdge::Reached`] を返し、登録
    ///   された解放処理を呼びます。
    /// - 到達 → 未到達のエッジでだけ [`ThresholdEdge::Rearmed`] を返し、次回の
    ///   到達で再び発火できるようにします。
    /// - エッジでなければ `None` を返します（超過が続く間に高コストな解放処理を
    ///   呼び直さない、エッジ検出の中心部分）。
    ///
    /// # フラグをエッジに連動させない理由（Issue #40）
    ///
    /// 先読み停止フラグを `armed` の遷移（CAS 成功）に連動させると、2スレッドが
    /// 同時に呼んだときに「しきい値未満なのにフラグが `true` のまま固着する」
    /// 状態が作れてしまいます。再武装側が `armed` を `true` へ戻した後、到達側の
    /// フラグ設定が後から確定すると、`armed = true`（未発火）と
    /// `prefetch_paused = true`（停止中）が同時に成立し、次の「到達 → 再武装」の
    /// 一巡が起きるまで先読みが止まり続けるためです。
    ///
    /// 判定のたびに結果そのものを書けば、重なりのない次の呼び出しが必ずフラグを
    /// 自身の判定結果へ一致させるため、この固着は起こりません。並行して呼ばれて
    /// いる間は、フラグの値は「同時に走ったいずれかの判定結果」になります
    /// （どれもその時点の観測に基づく妥当な値であり、どれか一方が恒久的に
    /// 残ることはありません）。
    ///
    /// `armed` の遷移は `compare_exchange` で行うため、複数スレッドが同時に
    /// 呼んでも、発火・再武装のどちらも高々1回だけ起こります。
    ///
    /// しきい値バイト数は `budget_bytes * percent / 100` を `u128` で計算し、
    /// `usize` の乗算オーバーフローを避けます。
    pub(crate) fn evaluate(
        &self,
        current_usage_bytes: usize,
        budget_bytes: usize,
    ) -> Option<ThresholdEdge> {
        let percent = u128::from(self.percent());
        let threshold_bytes = (budget_bytes as u128 * percent) / 100;
        let current = current_usage_bytes as u128;
        let over = current >= threshold_bytes;

        // 解放処理を呼ぶ前に書く。ハンドラが先読み停止フラグを読む場合、判定
        // 結果が反映済みの状態で読めるようにするため。ハンドラ内から再入した
        // 判定は、このあとで自身の結果を上書きしてよい（より新しい観測のため）。
        self.prefetch_paused.store(over, Ordering::Release);

        if over {
            if self
                .armed
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.fire_release_handlers();
                return Some(ThresholdEdge::Reached);
            }
        } else if self
            .armed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Some(ThresholdEdge::Rearmed);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 受け入れ条件: しきい値の割合変更が判定へ反映される（境界値。低レベルな
    // evaluate を直接確認する。MemoryBudget 経由の確認は budget.rs 側のテスト）。
    #[test]
    fn evaluate_boundary_reaches_exactly_at_threshold_percent() {
        let below = ThresholdState::new();
        below.set_percent(10).expect("10は有効な割合のはず"); // 1000 * 10 / 100 = 100
        assert_eq!(
            below.evaluate(99, 1000),
            None,
            "99バイトはしきい値未満のはず"
        );
        assert!(!below.prefetch_paused());

        let at = ThresholdState::new();
        at.set_percent(10).expect("10は有効な割合のはず");
        assert_eq!(
            at.evaluate(100, 1000),
            Some(ThresholdEdge::Reached),
            "100バイトはちょうどしきい値（10%）のはず"
        );
        assert!(at.prefetch_paused());
    }

    // 到達エッジの設計判断: 超過が続いている間は再発火しない。
    #[test]
    fn evaluate_does_not_refire_while_still_over_threshold() {
        let state = ThresholdState::new();
        state.set_percent(50).expect("50は有効な割合のはず");

        assert_eq!(state.evaluate(600, 1000), Some(ThresholdEdge::Reached));
        // 超過が続いている間、2回目以降は None（再発火しない）。
        assert_eq!(state.evaluate(700, 1000), None);
        assert_eq!(state.evaluate(900, 1000), None);
        assert!(state.prefetch_paused());
    }

    // 受け入れ条件: しきい値を下回ると先読み停止が解除され、イベントが
    // 再武装される（低レベルな evaluate を直接確認する）。
    #[test]
    fn evaluate_rearms_after_dropping_below_and_can_fire_again() {
        let state = ThresholdState::new();
        state.set_percent(50).expect("50は有効な割合のはず");

        assert_eq!(state.evaluate(600, 1000), Some(ThresholdEdge::Reached));
        assert_eq!(state.evaluate(400, 1000), Some(ThresholdEdge::Rearmed));
        assert!(!state.prefetch_paused());

        // 再武装後、再び到達すると改めて発火する。
        assert_eq!(state.evaluate(600, 1000), Some(ThresholdEdge::Reached));
        assert!(state.prefetch_paused());
    }

    // 受け入れ条件（Issue #40）: evaluate を終えた時点で、先読み停止フラグは
    // 必ずその回の判定結果と一致する（戻り値がエッジかどうかに依らない）。
    #[test]
    fn evaluate_always_leaves_prefetch_paused_matching_the_verdict() {
        let state = ThresholdState::new();
        state.set_percent(50).expect("50は有効な割合のはず");

        // 到達エッジ・超過の継続・再武装エッジ・未到達の継続の4通りを順に通す。
        for (usage, expected_paused) in [(600, true), (700, true), (400, false), (300, false)] {
            state.evaluate(usage, 1000);
            assert_eq!(
                state.prefetch_paused(),
                expected_paused,
                "usage={usage} の判定結果とフラグが一致するはず"
            );
        }
    }

    // 受け入れ条件（Issue #40）: 並行実行の交錯が残し得る「armed = true（未発火）
    // かつ prefetch_paused = true（停止中）」の状態からでも、次の判定でフラグが
    // 解除される（しきい値未満のまま固着しない）。
    //
    // 実スレッドの交錯が起きるのを待つ代わりに、交錯が残す状態そのものを直接
    // 作って検証する（決定的なテストにするため）。フラグをエッジ（armed の CAS
    // 成功）に連動させる実装では、この状態から未到達を判定しても CAS が失敗して
    // フラグが true のまま残るため、このテストは失敗する。
    #[test]
    fn evaluate_clears_prefetch_paused_stuck_by_a_concurrent_interleaving() {
        let state = ThresholdState::new();
        state.set_percent(50).expect("50は有効な割合のはず");
        state.armed.store(true, Ordering::Release);
        state.prefetch_paused.store(true, Ordering::Release);

        assert_eq!(
            state.evaluate(400, 1000),
            None,
            "armed は既に再武装済みのため、再武装エッジは立たない"
        );
        assert!(
            !state.prefetch_paused(),
            "しきい値未満と判定した以上、先読み停止は解除されるはず"
        );
    }

    // 受け入れ条件（Issue #40）: 複数スレッドが同時に evaluate を呼んでも、
    // 全スレッドの終了後に単独で判定し直せば、フラグは必ずその判定結果と一致
    // する（並行実行の後にフラグが固着して残らない）。
    #[test]
    fn concurrent_evaluate_leaves_no_stuck_prefetch_paused() {
        let state = Arc::new(ThresholdState::new());
        state.set_percent(50).expect("50は有効な割合のはず");

        // 半数を到達側、半数を未到達側にして、到達と再武装を繰り返し交錯させる。
        // 反復回数は有限のため、各スレッドは必ず終了する。
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    let usage = if index % 2 == 0 { 600 } else { 400 };
                    for _ in 0..1000 {
                        state.evaluate(usage, 1000);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("パニックしないはず");
        }

        state.evaluate(400, 1000);
        assert!(
            !state.prefetch_paused(),
            "並行実行の後でも、単独の未到達判定でフラグが解除されるはず"
        );
        state.evaluate(600, 1000);
        assert!(
            state.prefetch_paused(),
            "並行実行の後でも、単独の到達判定でフラグが立つはず"
        );
    }

    // 受け入れ条件（Issue #40）: 解放処理の内部から会計 API へ再入しても
    // デッドロックしない（ロックを手放してからハンドラを呼ぶ）。
    //
    // ロックを保持したままハンドラを呼ぶ実装では、ハンドラ内の
    // register_release_handler が同じ Mutex を取り直してハングするため、
    // 時間制限つきの待機が発火してこのテストは失敗する。
    #[test]
    fn release_handler_can_reenter_the_accounting_api_without_deadlock() {
        let state = Arc::new(ThresholdState::new());
        state.set_percent(50).expect("50は有効な割合のはず");

        // 解放処理は `'static` である必要があるため、`Arc` の循環参照を作らない
        // よう `Weak` を捕捉する（循環させるとこの状態自体が解放されない）。
        let weak = Arc::downgrade(&state);
        let reentered = Arc::new(AtomicBool::new(false));
        let reentered_in_handler = Arc::clone(&reentered);
        state.register_release_handler(Box::new(move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            // 再入その1: 登録簿のロックを取り直す経路。
            state.register_release_handler(Box::new(|| {}));
            // 再入その2: しきい値判定からハンドラ発火へ至り得る経路。armed は
            // 既に倒れているため、ここから再発火して無限再帰することはない。
            state.evaluate(600, 1000);
            reentered_in_handler.store(true, Ordering::Release);
        }));

        // デッドロックした場合にテストが永久にハングしないよう、別スレッドで
        // 実行して時間制限つきで完了を待つ。
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            worker_state.evaluate(600, 1000);
            let _ = done_tx.send(());
        });
        done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("解放処理からの再入でデッドロックしている（10秒以内に完了しない）");
        worker.join().expect("パニックしないはず");

        assert!(
            reentered.load(Ordering::Acquire),
            "解放処理が呼ばれ、再入も完了しているはず"
        );
        assert!(state.prefetch_paused());
    }

    // 受け入れ条件: 不正な割合（0、101）が拒否される（低レベル set_percent の
    // 確認。公開 API 経由の確認は budget.rs 側のテスト）。
    #[test]
    fn set_percent_rejects_zero_and_values_over_100() {
        let state = ThresholdState::new();
        assert_eq!(
            state.set_percent(0),
            Err(InvalidSoftThresholdPercent {
                requested_percent: 0
            })
        );
        assert_eq!(
            state.set_percent(101),
            Err(InvalidSoftThresholdPercent {
                requested_percent: 101
            })
        );
        assert!(state.set_percent(1).is_ok(), "境界値1は許可されるはず");
        assert!(state.set_percent(100).is_ok(), "境界値100は許可されるはず");
    }
}
