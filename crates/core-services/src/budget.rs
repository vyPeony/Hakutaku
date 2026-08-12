//! 複数ソースの合計サイズ・ファイル数の上限判定（`PERF-004`〜`006`）と、
//! compare-and-reserve（P06-4、`tasks/phase-06-large-file-loading.md`
//! 「上限の優先関係」、ADR-0007「『原子的』の適用範囲」）。
//!
//! # 上限の優先関係
//!
//! `PERF-006`（合計 2 GiB）が**最上位の拘束条件**です。単一ファイル上限
//! （`PERF-004`、1 GiB）とファイル数上限（`PERF-005`、10 ファイル）はこれと
//! 併用しますが、合計が 2 GiB を超える場合は他の条件を満たしていても拒否し
//! ます。「1 GiB のファイルを2つ」は合計 2 GiB で許容範囲ですが、そこへ 1 KiB
//! を追加することはできません。
//!
//! 上限値は要件（`PERF-004`〜`006`）由来の固定値であり、設定項目にはしません
//! （計画正本の明示方針）。
//!
//! # 「原子的」の適用範囲（ADR-0007）
//!
//! アプリ内の合計サイズの判定と予約（compare-and-reserve）だけが原子的です。
//! [`SourceBudget::reserve`] は `Mutex` で状態を保持し、判定と加算をロックの下
//! でまとめて行うことで、他の追加操作と競合しません。ファイル側の観測
//! （`hakutaku_data_source::FileSnapshot::snapshot_end`）は別物であり、判定した
//! 瞬間のファイルサイズをファイルシステム上のトランザクションにすることは
//! できません。
//!
//! 拒否時は状態を変更しません（既存ソースの表示は維持されます）。

use std::sync::{Mutex, MutexGuard, PoisonError};

/// 合計サイズ上限（`PERF-006`）。2 GiB。
pub const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 単一ファイル上限（`PERF-004`）。1 GiB。
pub const MAX_SINGLE_FILE_BYTES: u64 = 1024 * 1024 * 1024;

/// 同時に開けるファイル数上限（`PERF-005`）。
pub const MAX_SOURCE_COUNT: usize = 10;

/// 追加（または再読み込み）を拒否した理由です。
///
/// 各バリアントは、拒否時点の現在値・要求量・上限値・（合計超過の場合は）
/// 超過量を保持します（利用者への理由・上限値の表示に使う想定。表示そのものは
/// P07 の対象）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetRejection {
    /// 単一ファイルが上限（`PERF-004`）を超える。
    SingleFileTooLarge {
        requested_bytes: u64,
        limit_bytes: u64,
    },
    /// ファイル数が上限（`PERF-005`）に達している。
    TooManySources {
        current_count: usize,
        limit_count: usize,
    },
    /// 合計サイズが上限（`PERF-006`）を超える。最上位の拘束条件。
    TotalTooLarge {
        current_total_bytes: u64,
        requested_bytes: u64,
        limit_bytes: u64,
        /// 追加後の見込み合計が上限を超える量（表示用。計画正本「再読み込み
        /// で合計 2 GB を超える場合」が要求する「超過量」の表示材料）。
        excess_bytes: u64,
    },
}

impl std::fmt::Display for BudgetRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BudgetRejection::SingleFileTooLarge {
                requested_bytes,
                limit_bytes,
            } => write!(
                f,
                "単一ファイルの上限を超えています（要求 {requested_bytes} バイト、\
                 上限 {limit_bytes} バイト）。"
            ),
            BudgetRejection::TooManySources {
                current_count,
                limit_count,
            } => write!(
                f,
                "同時に開けるファイル数の上限に達しています（現在 {current_count} 件、\
                 上限 {limit_count} 件）。"
            ),
            BudgetRejection::TotalTooLarge {
                current_total_bytes,
                requested_bytes,
                limit_bytes,
                excess_bytes,
            } => write!(
                f,
                "開いているファイルの合計サイズの上限を超えます（現在 {current_total_bytes} \
                 バイト、追加要求 {requested_bytes} バイト、上限 {limit_bytes} バイト、\
                 超過量 {excess_bytes} バイト）。"
            ),
        }
    }
}

impl std::error::Error for BudgetRejection {}

/// [`SourceBudget::reserve`] が成功したときに返す予約ハンドルです。
///
/// [`SourceBudget::release`] へそのまま渡すことで、予約したサイズを合計から
/// 除外できます（`close_source` 用）。値そのものにドロップ時の自動返却は
/// 実装していません（`MemoryBudget::ReservationToken` と異なり、こちらは
/// 「登録されているソースの数」という粗粒度な状態であり、明示的に
/// `close_source` を呼んだときにだけ返却されるべきという設計のため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceReservation {
    pub reserved_bytes: u64,
}

#[derive(Debug, Default)]
struct BudgetState {
    total_bytes: u64,
    count: usize,
}

/// 複数ソースの合計サイズ・ファイル数を管理する予算です。
///
/// `Mutex<BudgetState>` により、判定（`PERF-004`〜`006`）と加算を1つの
/// クリティカルセクションにまとめ、compare-and-reserve を原子的にします。
#[derive(Debug)]
pub struct SourceBudget {
    state: Mutex<BudgetState>,
    max_total_bytes: u64,
    max_single_file_bytes: u64,
    max_source_count: usize,
}

impl SourceBudget {
    /// 要件由来の既定上限（2 GiB／1 GiB／10 ファイル）で新規作成します。
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(MAX_TOTAL_BYTES, MAX_SINGLE_FILE_BYTES, MAX_SOURCE_COUNT)
    }

    /// 上限値を注入できるコンストラクタです。
    ///
    /// 実運用は既定の要件由来上限を使う [`Self::new`] を使ってください。
    /// テストで境界値を小さくして検証したい場合にこちらを使います（1 GiB 級の
    /// 実ファイルを用意せずに上限判定ロジックを検証するための設計。作業指示の
    /// 「上限値は定数注入可能にしてテストでは小さい値を使う設計も可」に対応）。
    #[must_use]
    pub fn with_limits(
        max_total_bytes: u64,
        max_single_file_bytes: u64,
        max_source_count: usize,
    ) -> Self {
        SourceBudget {
            state: Mutex::new(BudgetState::default()),
            max_total_bytes,
            max_single_file_bytes,
            max_source_count,
        }
    }

    fn lock(&self) -> MutexGuard<'_, BudgetState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 現在の合計サイズ（バイト）を返します。
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.lock().total_bytes
    }

    /// 現在登録されているファイル数を返します。
    #[must_use]
    pub fn count(&self) -> usize {
        self.lock().count
    }

    /// `size_bytes` のファイル追加を試みます（compare-and-reserve）。
    ///
    /// 判定は `PERF-004`（単一ファイル）→ `PERF-005`（ファイル数）→
    /// `PERF-006`（合計サイズ）の順に行いますが、いずれか1つでも満たさない
    /// 場合は拒否するAND条件です（`PERF-006` が最上位という位置づけは、
    /// 「ファイル数・単体サイズを満たしていても合計超過なら拒否する」という
    /// 優先関係であり、判定順序そのものではありません）。
    ///
    /// 成功した場合だけ状態（合計サイズ・件数）を更新します。拒否時は状態を
    /// 一切変更しません（既存ソースに影響しないことの実装）。
    pub fn reserve(&self, size_bytes: u64) -> Result<SourceReservation, BudgetRejection> {
        let mut state = self.lock();

        if size_bytes > self.max_single_file_bytes {
            return Err(BudgetRejection::SingleFileTooLarge {
                requested_bytes: size_bytes,
                limit_bytes: self.max_single_file_bytes,
            });
        }
        if state.count >= self.max_source_count {
            return Err(BudgetRejection::TooManySources {
                current_count: state.count,
                limit_count: self.max_source_count,
            });
        }
        // size_bytes が u64::MAX 付近の非現実的な値でオーバーフローする場合も、
        // 安全側（上限超過扱い）に倒すため saturating_add を使う。
        let new_total = state.total_bytes.saturating_add(size_bytes);
        if new_total > self.max_total_bytes {
            return Err(BudgetRejection::TotalTooLarge {
                current_total_bytes: state.total_bytes,
                requested_bytes: size_bytes,
                limit_bytes: self.max_total_bytes,
                excess_bytes: new_total.saturating_sub(self.max_total_bytes),
            });
        }

        state.total_bytes = new_total;
        state.count += 1;
        Ok(SourceReservation {
            reserved_bytes: size_bytes,
        })
    }

    /// `reservation` を返却し、合計サイズ・件数の判定対象から除外します
    /// （`close_source` 用）。
    pub fn release(&self, reservation: SourceReservation) {
        let mut state = self.lock();
        state.total_bytes = state.total_bytes.saturating_sub(reservation.reserved_bytes);
        state.count = state.count.saturating_sub(1);
    }

    /// 既存の予約を新しいサイズへ置き換えます（明示的な再読み込みで合計が
    /// 変わる場合の compare-and-reserve。ADR-0007「再読み込みで合計 2 GB を
    /// 超える場合」の判定に使う想定の部品です。**再読み込みの実際のオーケスト
    /// レーション（LOG-028 のフロー全体）は P06-5 の対象であり、本メソッドは
    /// その下で使われる原子的な判定・予約の部品を提供するだけです。**
    ///
    /// `old` を一旦除いた状態で `new_size_bytes` を判定し、成功した場合だけ
    /// 状態を更新します（`old` を含めたまま判定すると、同じファイルの
    /// サイズ変化を「既存分＋新規分」として二重計上してしまうため）。拒否時は
    /// 元の予約（`old`）がそのまま有効です（呼び出し側は何もする必要が
    /// ありません）。
    pub fn try_replace(
        &self,
        old: SourceReservation,
        new_size_bytes: u64,
    ) -> Result<SourceReservation, BudgetRejection> {
        let mut state = self.lock();

        if new_size_bytes > self.max_single_file_bytes {
            return Err(BudgetRejection::SingleFileTooLarge {
                requested_bytes: new_size_bytes,
                limit_bytes: self.max_single_file_bytes,
            });
        }
        // 件数は変わらない（置き換えなので PERF-005 の再判定は不要）。

        let total_without_old = state.total_bytes.saturating_sub(old.reserved_bytes);
        let new_total = total_without_old.saturating_add(new_size_bytes);
        if new_total > self.max_total_bytes {
            return Err(BudgetRejection::TotalTooLarge {
                current_total_bytes: state.total_bytes,
                requested_bytes: new_size_bytes,
                limit_bytes: self.max_total_bytes,
                excess_bytes: new_total.saturating_sub(self.max_total_bytes),
            });
        }

        state.total_bytes = new_total;
        Ok(SourceReservation {
            reserved_bytes: new_size_bytes,
        })
    }
}

impl Default for SourceBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    // 受け入れ条件: 合計 2 GB 境界（ちょうど可、1 バイト超過で拒否）。既定の
    // 上限値そのもの（PERF-006 の既定値 2 GiB、PERF-004 の既定値 1 GiB）を
    // 確認する。「1 GiB のファイルを2つ」が合計 2 GiB ちょうどで許容される
    // ことを、計画正本の例（作業指示「上限の優先関係」）のとおりに確認する。
    #[test]
    fn reserve_accepts_exactly_default_total_limit_and_rejects_one_byte_over() {
        let budget = SourceBudget::new();
        assert_eq!(budget.total_bytes(), 0);

        // 1 GiB のファイルを2つ（単一上限ちょうど×2）で合計 2 GiB ちょうど。
        let first = budget
            .reserve(MAX_SINGLE_FILE_BYTES)
            .expect("単一上限ちょうどは許可されるはず");
        let second = budget
            .reserve(MAX_SINGLE_FILE_BYTES)
            .expect("合計上限ちょうどまでは許可されるはず");
        assert_eq!(budget.total_bytes(), MAX_TOTAL_BYTES);

        // そこへ 1 バイトでも追加すると合計上限を超えて拒否される
        // （計画正本「そこに 1 KB を追加することはできません」に対応）。
        let rejected = budget
            .reserve(1)
            .expect_err("合計上限を1バイト超えるので拒否されるはず");
        assert!(matches!(rejected, BudgetRejection::TotalTooLarge { .. }));

        budget.release(first);
        budget.release(second);
        assert_eq!(budget.total_bytes(), 0);
    }

    // 受け入れ条件: 合計 2 GB 境界（テスト用の小さい上限値で、ちょうど可・
    // 1 バイト超過で拒否を確認する。実ファイルを用意せずに判定ロジックだけを
    // 検証する）。
    #[test]
    fn reserve_boundary_with_injected_small_limits() {
        let budget = SourceBudget::with_limits(1000, 1000, 10);

        let first = budget.reserve(600).expect("上限内なので許可されるはず");
        let second = budget
            .reserve(400)
            .expect("合計ちょうど1000なので許可されるはず");
        assert_eq!(budget.total_bytes(), 1000);

        let rejected = budget
            .reserve(1)
            .expect_err("合計1001は上限超過で拒否されるはず");
        match rejected {
            BudgetRejection::TotalTooLarge {
                current_total_bytes,
                requested_bytes,
                limit_bytes,
                excess_bytes,
            } => {
                assert_eq!(current_total_bytes, 1000);
                assert_eq!(requested_bytes, 1);
                assert_eq!(limit_bytes, 1000);
                assert_eq!(excess_bytes, 1);
            }
            other => panic!("TotalTooLarge を期待したが {other:?} だった"),
        }

        // 拒否しても既存の予約（合計）に影響しない。
        assert_eq!(budget.total_bytes(), 1000);
        assert_eq!(budget.count(), 2);

        budget.release(first);
        budget.release(second);
        assert_eq!(budget.total_bytes(), 0);
        assert_eq!(budget.count(), 0);
    }

    // 受け入れ条件: 単一 1 GB 超の拒否（既定の PERF-004 上限値そのもので確認）。
    #[test]
    fn reserve_rejects_single_file_over_default_limit() {
        let budget = SourceBudget::new();
        let rejected = budget
            .reserve(MAX_SINGLE_FILE_BYTES + 1)
            .expect_err("単一ファイル上限を1バイト超えるので拒否されるはず");
        assert_eq!(
            rejected,
            BudgetRejection::SingleFileTooLarge {
                requested_bytes: MAX_SINGLE_FILE_BYTES + 1,
                limit_bytes: MAX_SINGLE_FILE_BYTES,
            }
        );
        // 拒否時は状態を変更しない。
        assert_eq!(budget.total_bytes(), 0);
        assert_eq!(budget.count(), 0);
    }

    // 受け入れ条件: 11ファイル目の拒否（PERF-005）。
    #[test]
    fn reserve_rejects_the_eleventh_file() {
        let budget = SourceBudget::with_limits(u64::MAX, u64::MAX, 10);

        let mut reservations = Vec::new();
        for _ in 0..10 {
            reservations.push(budget.reserve(1).expect("10件目までは許可されるはず"));
        }
        assert_eq!(budget.count(), 10);

        let rejected = budget
            .reserve(1)
            .expect_err("11件目はファイル数上限で拒否されるはず");
        assert_eq!(
            rejected,
            BudgetRejection::TooManySources {
                current_count: 10,
                limit_count: 10,
            }
        );
        // 拒否後も既存の10件はそのまま。
        assert_eq!(budget.count(), 10);

        for reservation in reservations {
            budget.release(reservation);
        }
        assert_eq!(budget.count(), 0);
    }

    // 受け入れ条件: close 後に再追加できる（合計・件数から除外される）。
    #[test]
    fn release_excludes_reservation_and_allows_re_adding() {
        let budget = SourceBudget::with_limits(1000, 1000, 1);

        let first = budget
            .reserve(1000)
            .expect("上限ちょうどなので許可されるはず");
        assert!(
            budget.reserve(1).is_err(),
            "件数上限（1件）に達しているので拒否されるはず"
        );

        budget.release(first);
        assert_eq!(budget.total_bytes(), 0);
        assert_eq!(budget.count(), 0);

        // close 後は同じサイズで再度追加できる。
        let second = budget
            .reserve(1000)
            .expect("close 後は合計から除外されているので許可されるはず");
        assert_eq!(budget.total_bytes(), 1000);
        budget.release(second);
    }

    // 受け入れ条件: 並行追加テスト（複数スレッドの同時 reserve で合計が上限を
    // 超えない）。memory-accounting::MemoryBudget の並行テストと同じ手法。
    #[test]
    fn concurrent_reservations_never_exceed_total_limit() {
        let budget = Arc::new(SourceBudget::with_limits(1000, 1000, 100));
        let per_thread_request = 130u64;
        let thread_count = 16;

        let handles: Vec<_> = (0..thread_count)
            .map(|_| {
                let budget = Arc::clone(&budget);
                thread::spawn(move || budget.reserve(per_thread_request).is_ok())
            })
            .collect();

        let success_count = handles
            .into_iter()
            .map(|handle| handle.join().expect("パニックしないはず"))
            .filter(|succeeded| *succeeded)
            .count();

        // 上限1000バイトに対して130バイトずつ予約するので、7個まで
        // （7 * 130 = 910 <= 1000 < 8 * 130 = 1040）しか成功しない。
        assert_eq!(success_count, 7);
        assert_eq!(budget.total_bytes(), 7 * per_thread_request);
        assert!(budget.total_bytes() <= 1000);
    }

    // 受け入れ条件: try_replace は旧予約分を除いた上で新サイズを判定する
    // （再読み込みでの二重計上を避ける）。単一ファイル上限は合計上限より
    // 大きく取り、合計上限だけが判定に効くようにする（TotalTooLarge を
    // ピンポイントで確認するため）。
    #[test]
    fn try_replace_judges_without_double_counting_the_old_reservation() {
        let budget = SourceBudget::with_limits(1000, 2000, 10);
        let reservation = budget.reserve(600).expect("許可されるはず");
        assert_eq!(budget.total_bytes(), 600);

        // 600 -> 900 への置き換え（900 <= 1000 なので許可されるはず）。
        let replaced = budget
            .try_replace(reservation, 900)
            .expect("合計1000以内なので許可されるはず");
        assert_eq!(budget.total_bytes(), 900);

        // 900 -> 1100 は上限超過で拒否され、元の予約が保たれる。
        let rejected = budget
            .try_replace(replaced, 1100)
            .expect_err("合計上限を超えるので拒否されるはず");
        assert!(matches!(rejected, BudgetRejection::TotalTooLarge { .. }));
        assert_eq!(
            budget.total_bytes(),
            900,
            "拒否時は置き換え前の予約がそのまま有効なはず"
        );
    }

    // 受け入れ条件: BudgetRejection の日本語メッセージに主要な数値が含まれる
    // （利用者向け表示の材料として最低限の確認）。
    #[test]
    fn budget_rejection_messages_are_japanese_and_include_key_numbers() {
        let single = BudgetRejection::SingleFileTooLarge {
            requested_bytes: 2000,
            limit_bytes: 1000,
        };
        let message = single.to_string();
        assert!(message.contains("2000"));
        assert!(message.contains("1000"));

        let total = BudgetRejection::TotalTooLarge {
            current_total_bytes: 900,
            requested_bytes: 200,
            limit_bytes: 1000,
            excess_bytes: 100,
        };
        let message = total.to_string();
        assert!(message.contains("900"));
        assert!(message.contains("200"));
        assert!(message.contains("1000"));
        assert!(message.contains("100"));
    }
}
