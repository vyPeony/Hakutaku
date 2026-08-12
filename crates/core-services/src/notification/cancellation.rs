//! キャンセルの契約（[`CancellationToken`]）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// キャンセル要求を共有するためのトークンです（`Arc<AtomicBool>` 相当）。
///
/// `clone()` で複製したトークンは同じ内部状態を共有します。要求側（例: UI
/// の「キャンセル」操作を受けた P07）が [`Self::request_cancel`] を呼ぶと、
/// 共有している全てのクローンから [`Self::is_cancelled`] が `true` を返す
/// ようになります。
///
/// # 規約
///
/// 実行側（P06）は、**意味のある処理単位ごと**（例: チャンク読み込みごと、
/// N 行の解析ごと）に [`Self::is_cancelled`] を確認してください。バイト
/// 単位・行単位のような極小な粒度で毎回確認すると、確認そのものが
/// オーバーヘッドになります。キャンセルを検出した場合、実行側は途中結果を
/// 破棄するか、安全に確定できる範囲までを確定し、
/// [`super::TaskOutcome::Cancelled`] で処理を終えてください。
///
/// # 一度要求したら取り消せない
///
/// [`Self::request_cancel`] は一方向です。要求を撤回する API は意図的に
/// 用意していません。撤回したい場合は、新しい `CancellationToken` を発行
/// し、新しい処理単位として再実行する設計を想定します。
#[derive(Debug, Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// キャンセルされていない新しいトークンを作成します。
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// キャンセルを要求します。共有している全てのクローンへ、以後の
    /// [`Self::is_cancelled`] 呼び出しで反映されます（`Release` 順序で
    /// 書き込みます）。
    pub fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// キャンセルが要求されているかを返します（`Acquire` 順序で読み取り
    /// ます）。
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::CancellationToken;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn request_cancel_is_visible_through_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());

        token.request_cancel();

        assert!(token.is_cancelled());
        assert!(clone.is_cancelled(), "clone() は状態を共有するはず");
    }

    // 受け入れ条件: CancellationToken の共有と検出（複数スレッド）。
    #[test]
    fn cancellation_is_observed_across_threads() {
        let token = CancellationToken::new();
        let worker_count = 8usize;
        let barrier = Arc::new(Barrier::new(worker_count + 1));
        let observed_cancelled = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..worker_count {
            let token = token.clone();
            let barrier = Arc::clone(&barrier);
            let observed_cancelled = Arc::clone(&observed_cancelled);
            handles.push(thread::spawn(move || {
                barrier.wait();
                // キャンセル要求が伝搬するまで、意味のある処理単位ごとの
                // 確認を模してポーリングする。
                while !token.is_cancelled() {
                    thread::yield_now();
                }
                observed_cancelled.fetch_add(1, Ordering::Relaxed);
            }));
        }

        barrier.wait();
        token.request_cancel();

        for handle in handles {
            handle.join().expect("スレッドが panic しないはず");
        }

        assert_eq!(
            observed_cancelled.load(Ordering::Relaxed),
            worker_count,
            "全スレッドがキャンセルを検出できたはず"
        );
    }

    #[test]
    fn default_token_is_not_cancelled() {
        assert!(!CancellationToken::default().is_cancelled());
    }
}
