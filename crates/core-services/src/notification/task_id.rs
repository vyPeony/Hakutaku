//! 処理単位の識別子（[`TaskId`]）。

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// プロセス内で一意な採番用カウンタです。`1` から始めます（`0` は将来
/// 「未設定」の番兵として使う余地を残すため、割り当てません）。
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

/// 進捗・キャンセルの対象となる処理単位（例: 1ファイルの読み込み、1回の
/// 索引構築）を一意に識別する ID です。
///
/// 生成は [`TaskId::generate`] によるアトミックカウンタの採番です。
/// **プロセス内でだけ一意**であり、プロセスをまたいだ一意性・永続性は
/// 保証しません。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(u64);

impl TaskId {
    /// 新しい一意な `TaskId` を採番します。
    ///
    /// 呼び出しごとに異なる値を返します（複数スレッドから同時に呼んでも
    /// 採番は重複しません。`AtomicU64::fetch_add` による原子的な採番です）。
    ///
    /// 採番のたびに異なる値を返すという性質上、引数なしで固定の値を返す
    /// `Default` 実装は意図的に用意していません（`new` ではなく `generate`
    /// と命名しているのも、これが「既定値の構築」ではなく「新規採番」で
    /// あることを明確にするためです）。
    #[must_use]
    pub fn generate() -> Self {
        TaskId(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// 採番された内部値を返します。診断ログへの記録など、比較以外の用途
    /// 向けです。
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task-{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::TaskId;

    #[test]
    fn generated_ids_are_unique_and_increasing() {
        let first = TaskId::generate();
        let second = TaskId::generate();
        assert_ne!(first, second, "連続して採番した ID は一意のはず");
        assert!(second.get() > first.get(), "採番は単調増加のはず");
    }

    #[test]
    fn display_includes_prefix_and_value() {
        let id = TaskId::generate();
        assert_eq!(format!("{id}"), format!("task-{}", id.get()));
    }

    // 受け入れ条件: TaskId の採番が複数スレッドから同時に呼ばれても重複しない。
    #[test]
    fn generate_is_unique_across_threads() {
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};
        use std::thread;

        let seen = Arc::new(Mutex::new(HashSet::new()));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let seen = Arc::clone(&seen);
            handles.push(thread::spawn(move || {
                let id = TaskId::generate();
                seen.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id);
            }));
        }
        for handle in handles {
            handle.join().expect("スレッドが panic しないはず");
        }
        assert_eq!(
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            8,
            "8スレッド分すべて一意のはず"
        );
    }
}
