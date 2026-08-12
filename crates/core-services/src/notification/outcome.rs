//! 処理単位の最終結果（[`TaskOutcome`]）。

use super::error::UserFacingError;

/// 処理単位（[`super::TaskId`] が識別する1回の実行）の最終結果です。
///
/// P06 は読み込み・索引構築などの処理を、この3値のいずれかで終えます。
/// P07 はこの値を受け取り、対応する表示（成功、エラー表示、キャンセル済み
/// 表示）へ変換します。
///
/// # キャンセルの規約との関係
///
/// [`super::CancellationToken::is_cancelled`] を検出した実行側は、途中結果を
/// 破棄するか安全に確定できる範囲までを確定したうえで、`Failed` ではなく
/// この列挙の [`TaskOutcome::Cancelled`] を返してください。キャンセルは
/// 利用者向けエラーではないため、`UserFacingError` を伴いません。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutcome {
    /// 正常に完了しました。
    Completed,
    /// 利用者向けエラーで失敗しました（`ERR-002`）。
    Failed(UserFacingError),
    /// キャンセル要求（[`super::CancellationToken`]）により中断しました。
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_are_distinguishable() {
        let completed = TaskOutcome::Completed;
        let cancelled = TaskOutcome::Cancelled;
        let failed = TaskOutcome::Failed(UserFacingError::new("対象", "理由", "次の操作"));

        assert_ne!(completed, cancelled);
        assert_ne!(completed, failed);
        assert_ne!(cancelled, failed);
    }

    #[test]
    fn failed_outcomes_compare_by_inner_error() {
        let a = TaskOutcome::Failed(UserFacingError::new("対象", "理由", "次の操作"));
        let b = TaskOutcome::Failed(UserFacingError::new("対象", "理由", "次の操作"));
        let c = TaskOutcome::Failed(UserFacingError::new("別の対象", "理由", "次の操作"));

        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
