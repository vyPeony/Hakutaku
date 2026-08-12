//! 進捗の契約（[`Progress`]、[`ProgressSink`]）と通知単位の規約
//! （[`ProgressThrottle`]）。

use std::time::{Duration, Instant};

use super::task_id::TaskId;

/// 進捗の量が表す単位です。呼び出し側が単位を取り違えないよう、
/// [`Progress`] へ明示的に持たせます。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressUnit {
    /// バイト数（チャンク読み込みなど、量が連続的に増える対象）。
    Bytes,
    /// 件数（行数、ファイル数など、離散的に数える対象）。
    Items,
}

/// 処理単位の進捗です。総量が判明している**確定的**な進捗と、総量が不明な
/// **不確定**な進捗の両方を表現します。
///
/// 「単位」を [`ProgressUnit`] として型に明示し、`done` と `total`（確定的な
/// 場合）が何を数えているのかを呼び出し側が取り違えないようにします。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// 総量が判明している進捗（例: ファイルサイズが既知のチャンク読み込み）。
    Determinate {
        /// 処理済みの量（`unit` 単位）。
        done: u64,
        /// 総量（`unit` 単位）。`done` を上回ることは呼び出し側の責務であり、
        /// この型自身は実行時に検証しません。
        total: u64,
        /// `done` と `total` の単位。
        unit: ProgressUnit,
    },
    /// 総量が不明な進捗（例: 総サイズが分からない走査の途中経過）。
    Indeterminate {
        /// 処理済みの量（`unit` 単位）。総量が不明でも、経過の目安として
        /// 示せる場合に使います。
        done: u64,
        /// `done` の単位。
        unit: ProgressUnit,
    },
}

impl Progress {
    /// 処理済みの量を返します（確定・不確定のどちらでも取得できます）。
    #[must_use]
    pub const fn done(&self) -> u64 {
        match self {
            Progress::Determinate { done, .. } | Progress::Indeterminate { done, .. } => *done,
        }
    }

    /// 単位を返します。
    #[must_use]
    pub const fn unit(&self) -> ProgressUnit {
        match self {
            Progress::Determinate { unit, .. } | Progress::Indeterminate { unit, .. } => *unit,
        }
    }

    /// 確定的な進捗の場合、完了比率（0.0〜1.0 の目安）を返します。総量が
    /// `0` の確定的な進捗、および不確定な進捗では `None` を返します。
    #[must_use]
    pub fn ratio(&self) -> Option<f64> {
        match *self {
            Progress::Determinate { done, total, .. } if total > 0 => {
                #[allow(clippy::cast_precision_loss)]
                let ratio = done as f64 / total as f64;
                Some(ratio)
            }
            _ => None,
        }
    }
}

/// 既定の最小通知間隔です。**暫定設計であり要件 ID を持ちません**（P04-6 の
/// 技術検討における暫定値）。実測（P06・P08）に基づいて調整され得ます。
pub const DEFAULT_MIN_NOTIFY_INTERVAL: Duration = Duration::from_millis(100);

/// 既定の最小通知量（バイト単位。8 MiB）です。**暫定設計であり要件 ID を
/// 持ちません。**
pub const DEFAULT_MIN_NOTIFY_AMOUNT_BYTES: u64 = 8 * 1024 * 1024;

/// 進捗通知の受け口です。
///
/// P06（読み込み・索引構築の実行側）が発行側として、[`ProgressThrottle`]
/// による間引きを経てから [`Self::report`] を呼び出します。P07 は
/// `src-tauri` 経由でこのトレイトを実装し、受け取った通知を Tauri イベント
/// へ変換します（変換そのものはこのクレートの対象外です）。
///
/// # 通知単位の規約
///
/// **発行側は、処理単位（1バイト、1行など）ごとに無条件で呼び出しては
/// いけません。** 「最後の通知から一定間隔（時間または処理量）を超えた
/// とき」だけ呼び出してください。目安は [`DEFAULT_MIN_NOTIFY_INTERVAL`]
/// （100ミリ秒）または [`DEFAULT_MIN_NOTIFY_AMOUNT_BYTES`]（8 MiB）ごとで、
/// いずれか早く条件を満たした方を採用します。これらは**暫定設計であり
/// 要件 ID を持ちません**。実測（P06・P08）に基づいて調整され得ます。
///
/// この規約を守らないと、高頻度呼び出しが Rust ↔ WebView 間の転送コストを
/// 圧迫します（`tasks/phase-04-vertical-slice.md` が実測する懸念）。
/// [`ProgressThrottle`] は、この判定を発行側が自前で再実装しなくて済む
/// ようにする純粋なヘルパーです。
pub trait ProgressSink: Send + Sync {
    /// `task_id` が識別する処理の進捗を通知します。
    fn report(&self, task_id: TaskId, progress: Progress);
}

/// 進捗通知の間引き判定を行う、副作用のない純粋なユーティリティです。
///
/// 発行側は、実際に通知する直前に [`Self::should_notify`] を呼び、`true` が
/// 返ったときだけ [`ProgressSink::report`] を呼び出す運用を想定しています。
/// 時刻の取得（`Instant::now()`）は呼び出し側が行い、この構造体へ渡します
/// （このモジュール自身はテスト容易性のため実時間そのものを扱いません）。
#[derive(Debug, Clone)]
pub struct ProgressThrottle {
    min_interval: Duration,
    min_progress_amount: u64,
    last_notified: Option<(Instant, u64)>,
}

impl ProgressThrottle {
    /// 通知間隔と通知量のしきい値を指定して作成します。
    #[must_use]
    pub const fn new(min_interval: Duration, min_progress_amount: u64) -> Self {
        Self {
            min_interval,
            min_progress_amount,
            last_notified: None,
        }
    }

    /// 既定値（[`DEFAULT_MIN_NOTIFY_INTERVAL`]、
    /// [`DEFAULT_MIN_NOTIFY_AMOUNT_BYTES`]）で作成します。
    #[must_use]
    pub const fn with_defaults() -> Self {
        Self::new(DEFAULT_MIN_NOTIFY_INTERVAL, DEFAULT_MIN_NOTIFY_AMOUNT_BYTES)
    }

    /// 現在時刻 `now` と現在の処理済み量 `done_amount` を渡し、通知すべきか
    /// 判定します。
    ///
    /// - 初回呼び出しは基準点がまだないため、常に通知すべきと判定します。
    /// - 前回の通知から `now` までの経過時間が [`Self::new`] で指定した
    ///   `min_interval` **以上**、または処理済み量の増分が
    ///   `min_progress_amount` **以上**であれば、通知すべきと判定します
    ///   （境界値は「以上」を通知側とします。`ThresholdState::evaluate`
    ///   の到達判定と同じ扱いです）。
    /// - `true` を返した場合、次回判定の基準点として `now` と `done_amount`
    ///   を内部へ記録します。**呼び出し側が実際には通知しない場合、この
    ///   関数を呼ばないでください**（呼ぶと基準点だけが進んでしまいます）。
    pub fn should_notify(&mut self, now: Instant, done_amount: u64) -> bool {
        let should = match self.last_notified {
            None => true,
            Some((last_time, last_amount)) => {
                let elapsed = now.saturating_duration_since(last_time);
                let advanced = done_amount.saturating_sub(last_amount);
                elapsed >= self.min_interval || advanced >= self.min_progress_amount
            }
        };
        if should {
            self.last_notified = Some((now, done_amount));
        }
        should
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn ratio_for_determinate_progress() {
        let progress = Progress::Determinate {
            done: 25,
            total: 100,
            unit: ProgressUnit::Bytes,
        };
        assert_eq!(progress.ratio(), Some(0.25));
        assert_eq!(progress.done(), 25);
        assert_eq!(progress.unit(), ProgressUnit::Bytes);
    }

    #[test]
    fn ratio_is_none_for_indeterminate_and_zero_total() {
        let indeterminate = Progress::Indeterminate {
            done: 10,
            unit: ProgressUnit::Items,
        };
        assert_eq!(indeterminate.ratio(), None);
        assert_eq!(indeterminate.done(), 10);

        let zero_total = Progress::Determinate {
            done: 0,
            total: 0,
            unit: ProgressUnit::Bytes,
        };
        assert_eq!(zero_total.ratio(), None);
    }

    #[test]
    fn should_notify_always_true_on_first_call() {
        let mut throttle = ProgressThrottle::new(Duration::from_millis(100), 8);
        assert!(
            throttle.should_notify(Instant::now(), 0),
            "初回は基準点がないため常に通知すべきはず"
        );
    }

    // 受け入れ条件: 通知間隔規約の境界テスト（時間側）。
    #[test]
    fn should_notify_boundary_on_elapsed_time() {
        // 処理量側の条件が絶対に成立しないようにして、時間側だけを検証する。
        let mut throttle = ProgressThrottle::new(Duration::from_millis(100), u64::MAX);
        let base = Instant::now();
        assert!(throttle.should_notify(base, 0));

        let just_before = base + Duration::from_millis(99);
        assert!(
            !throttle.should_notify(just_before, 0),
            "99ms は間隔未満のため通知しないはず"
        );

        let at_boundary = base + Duration::from_millis(100);
        assert!(
            throttle.should_notify(at_boundary, 0),
            "ちょうど100msは境界（以上）で通知すべきはず"
        );
    }

    // 受け入れ条件: 通知間隔規約の境界テスト（処理量側）。
    #[test]
    fn should_notify_boundary_on_progress_amount() {
        // 時間側の条件が絶対に成立しないようにして、処理量側だけを検証する。
        let mut throttle = ProgressThrottle::new(Duration::from_secs(3600), 8);
        let base = Instant::now();
        assert!(throttle.should_notify(base, 0));

        assert!(
            !throttle.should_notify(base, 7),
            "増分7は8未満のため通知しないはず"
        );
        assert!(
            throttle.should_notify(base, 8),
            "増分8はちょうど境界（以上）で通知すべきはず"
        );
    }

    #[test]
    fn should_notify_does_not_advance_baseline_when_it_returns_false() {
        let mut throttle = ProgressThrottle::new(Duration::from_millis(100), 1000);
        let base = Instant::now();
        assert!(throttle.should_notify(base, 0));

        // 通知しなかった判定は基準点を進めない。
        let mid = base + Duration::from_millis(50);
        assert!(!throttle.should_notify(mid, 100));

        // 基準点はまだ base のまま。base + 90ms 時点でも 100ms を超えないので false。
        let still_within = base + Duration::from_millis(90);
        assert!(!throttle.should_notify(still_within, 100));

        // base + 100ms で境界に到達し true。
        let boundary = base + Duration::from_millis(100);
        assert!(throttle.should_notify(boundary, 100));
    }

    #[test]
    fn with_defaults_uses_documented_constants() {
        let throttle = ProgressThrottle::with_defaults();
        assert_eq!(throttle.min_interval, DEFAULT_MIN_NOTIFY_INTERVAL);
        assert_eq!(
            throttle.min_progress_amount,
            DEFAULT_MIN_NOTIFY_AMOUNT_BYTES
        );
    }

    /// `ProgressSink` が実際にトレイトオブジェクトとして使える（P07 側の
    /// 想定利用形態）ことを確認する、契約の性質そのものの検証。
    struct RecordingSink {
        received: Mutex<Vec<(TaskId, Progress)>>,
    }

    impl ProgressSink for RecordingSink {
        fn report(&self, task_id: TaskId, progress: Progress) {
            self.received
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((task_id, progress));
        }
    }

    #[test]
    fn progress_sink_is_usable_as_trait_object() {
        let sink = RecordingSink {
            received: Mutex::new(Vec::new()),
        };
        let boxed: Box<dyn ProgressSink> = Box::new(sink);

        let task_id = TaskId::generate();
        let progress = Progress::Determinate {
            done: 1,
            total: 2,
            unit: ProgressUnit::Items,
        };
        boxed.report(task_id, progress);

        // Box<dyn ProgressSink> からは中身を直接検査できないため、ここでは
        // panic せずに呼び出せること（オブジェクト安全であること）自体を
        // 確認する。内容の検証は RecordingSink を直接使う次のテストで行う。
    }

    #[test]
    fn progress_sink_receives_reported_values() {
        let sink = RecordingSink {
            received: Mutex::new(Vec::new()),
        };
        let task_id = TaskId::generate();
        let progress = Progress::Indeterminate {
            done: 42,
            unit: ProgressUnit::Bytes,
        };

        sink.report(task_id, progress);

        let received = sink
            .received
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(received.len(), 1);
        assert_eq!(received[0], (task_id, progress));
    }
}
