//! 利用者向けエラーの共通型（[`UserFacingError`]、`ERR-002`）。

use std::fmt;

/// 利用者向けエラーの共通型です。
///
/// `docs/requirements/functional.md` の `ERR-002` は次のとおり定めています。
///
/// > ユーザー向けエラーには、対象を識別できる名称またはパス、発生位置、
/// > 理由、継続可否、必要な次操作を示す。`DIAG-003`／`DIAG-004` により実値の
/// > 表示を制限しないため、原因調査に必要であればフルパスを表示してよい。
///
/// この型は上記5要素（対象・発生位置・理由・継続可否・次操作）をすべて
/// 表現します。`error_code` は `ERR-002` の要素ではなく、
/// `docs/development/error-codes.md` が定める任意の付加情報です。
///
/// # フルパスをマスキングしない
///
/// `ERR-002` は「実値の表示を制限しない」と定めています。この型・
/// `Display` 実装はいずれのフィールドも切り詰め・マスキングしません。
/// 呼び出し側（P06・P07）もマスキングしない前提で扱ってください
/// （`SEC-007` により機密データのマスキング表示は別途提供しません）。
///
/// # エラーコードは基準に該当する場合だけ付与する
///
/// `error_code` は `docs/development/error-codes.md` の「適用範囲と割り当て
/// 基準」（起動できない・処理を継続できない失敗、利用者または導入組織側の
/// 対処が必要な失敗、手順書・FAQ・問い合わせ対応から参照される見込みのある
/// 失敗）に該当する場合だけ設定してください。該当しない失敗は `None`
/// （診断ログでは `code=-`）のままにします。コードの書式・領域割り当て・
/// 採番手順は同文書と各領域の採番台帳が正本です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFacingError {
    /// 対象を識別できる名称またはパス（例: 開こうとしていたログファイルの
    /// フルパス）。何を処理していて失敗したかを示します。
    pub target: String,
    /// 発生位置（対象内のオフセット、行番号、設定ファイルの行・列など）。
    ///
    /// 失敗が対象全体に関わり、対象内のそうした位置が存在しない場合は `None`
    /// にします（`docs/requirements/functional.md` の `ERR-002` の節）。表示側
    /// （`src/error_panel.js`）は欄を消さず「（特定できません）」と示すため、
    /// `None` にしても5要素のうち位置だけが黙って抜けることはありません。
    pub location: Option<String>,
    /// 失敗の理由。
    pub reason: String,
    /// 継続可否。`true` の場合、利用者はこのエラーの後も他の対象の閲覧や
    /// 操作を続行できます（`ERR-001` の「1ファイル、1行の失敗で無関係な
    /// 参照対象の閲覧を停止しない」に対応）。`false` の場合、続行できない
    /// 致命的な失敗です。
    pub continuable: bool,
    /// 利用者が次に取れる操作（例: 「再試行してください」「他の対象を
    /// 閉じてから再読み込みしてください」）。
    pub next_action: String,
    /// アプリ内エラーコード（`docs/development/error-codes.md`）。付与基準に
    /// 該当する場合だけ `Some` にします。
    pub error_code: Option<String>,
}

impl UserFacingError {
    /// 対象・理由・次操作の最小構成で作成します。`continuable` は既定で
    /// `true`（続行可能）、`location` と `error_code` は `None` です。
    /// 必要に応じて [`Self::with_location`]・[`Self::not_continuable`]・
    /// [`Self::with_error_code`] で補ってください。
    #[must_use]
    pub fn new(
        target: impl Into<String>,
        reason: impl Into<String>,
        next_action: impl Into<String>,
    ) -> Self {
        Self {
            target: target.into(),
            location: None,
            reason: reason.into(),
            continuable: true,
            next_action: next_action.into(),
            error_code: None,
        }
    }

    /// 発生位置を設定します。
    ///
    /// 現在の呼び出し側（`src-tauri`）はこれを一度も使っていません。今の失敗は
    /// すべて対象全体に関わるもの（ファイルを開けない、選択範囲全体が上限を
    /// 超える、表示集合を構築できない）で、対象内の位置が存在しないためです
    /// （`location` フィールドの doc コメント）。行や設定ファイルの行・列を伴う
    /// 失敗を利用者へ返すようになった時点で使います。
    #[must_use]
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    /// 続行不可能な致命的エラーとして印付けます。
    #[must_use]
    pub fn not_continuable(mut self) -> Self {
        self.continuable = false;
        self
    }

    /// アプリ内エラーコードを設定します。付与基準は型の doc コメントを
    /// 参照してください。
    #[must_use]
    pub fn with_error_code(mut self, error_code: impl Into<String>) -> Self {
        self.error_code = Some(error_code.into());
        self
    }
}

impl fmt::Display for UserFacingError {
    /// 対象・理由・次操作を分かりやすい日本語で結合します（`location`・
    /// `continuable`・`error_code` を保持している場合は付記します）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.target)?;
        if let Some(location) = &self.location {
            write!(f, "（{location}）")?;
        }
        write!(f, ": {}", self.reason)?;
        if !self.continuable {
            write!(f, "。この操作は続行できません")?;
        }
        write!(f, "。次の操作: {}", self.next_action)?;
        if let Some(error_code) = &self.error_code {
            write!(f, "（コード: {error_code}）")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UserFacingError;

    #[test]
    fn new_defaults_to_continuable_without_location_or_code() {
        let error = UserFacingError::new("対象", "理由", "次の操作");
        assert!(error.continuable);
        assert_eq!(error.location, None);
        assert_eq!(error.error_code, None);
    }

    // 受け入れ条件: UserFacingError の Display（対象・理由・次操作の結合）。
    #[test]
    fn display_combines_target_reason_and_next_action() {
        let error = UserFacingError::new(
            "C:\\logs\\app.log",
            "共有違反で読み取れません",
            "他のプロセスを終了してから再試行してください",
        );
        assert_eq!(
            format!("{error}"),
            "C:\\logs\\app.log: 共有違反で読み取れません。\
             次の操作: 他のプロセスを終了してから再試行してください"
        );
    }

    #[test]
    fn display_includes_location_when_present() {
        let error = UserFacingError::new(
            "hakutaku.yaml",
            "構文が不正です",
            "該当行を修正してください",
        )
        .with_location("3行目5列目");
        assert_eq!(
            format!("{error}"),
            "hakutaku.yaml（3行目5列目）: 構文が不正です。次の操作: 該当行を修正してください"
        );
    }

    #[test]
    fn display_marks_not_continuable_errors() {
        let error = UserFacingError::new(
            "設定ファイル",
            "読み込みに失敗しました",
            "アプリを再起動してください",
        )
        .not_continuable();
        assert_eq!(
            format!("{error}"),
            "設定ファイル: 読み込みに失敗しました。この操作は続行できません。\
             次の操作: アプリを再起動してください"
        );
    }

    #[test]
    fn display_includes_error_code_when_present() {
        let error = UserFacingError::new(
            "WebView2 Runtime",
            "検出できません",
            "Runtime を導入してください",
        )
        .with_error_code("HKT-W2-0001");
        assert_eq!(
            format!("{error}"),
            "WebView2 Runtime: 検出できません。次の操作: Runtime を導入してください\
             （コード: HKT-W2-0001）"
        );
    }

    #[test]
    fn location_and_error_code_can_combine_with_not_continuable() {
        let error = UserFacingError::new("対象X", "致命的な内部エラー", "サポートへ連絡")
            .with_location("処理ステップ3")
            .not_continuable()
            .with_error_code("HKT-CORE-0001");
        assert_eq!(
            format!("{error}"),
            "対象X（処理ステップ3）: 致命的な内部エラー。この操作は続行できません。\
             次の操作: サポートへ連絡（コード: HKT-CORE-0001）"
        );
    }
}
