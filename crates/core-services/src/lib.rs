#![forbid(unsafe_code)]

//! 共通サービス層（GUI 非依存）。
//!
//! P04（`tasks/phase-04-vertical-slice.md`）の範囲取得契約（表示集合・世代・
//! 転送上限）と、ファイル読み込みから表示集合登録までのオーケストレーションを
//! 実装します。`src-tauri` はこのクレートが公開する型・関数を呼ぶだけで、
//! 解析ロジックそのものを持ちません（層境界の維持。計画書「作業項目8」）。
//!
//! P05（`tasks/phase-05-log-parsing-core.md`）では、[`resolve_profile`] が
//! ログ解析プロファイルの4段階解決（`LOG-021`）を実装します。パス照合の下請け
//! （正規化・大文字小文字不区別・glob）は `hakutaku_config` を再利用し、
//! 依存の向きは `core-services → config`（逆向きはなし）です。

mod budget;
mod chunk_cache;
mod copy;
mod display_set;
mod item;
mod line_index;
mod loader;
mod merge;
mod ordering;
mod profile_resolution;
mod registry;
mod streaming_parse;

pub use budget::{
    BudgetRejection, SourceBudget, SourceReservation, MAX_SINGLE_FILE_BYTES, MAX_SOURCE_COUNT,
    MAX_TOTAL_BYTES,
};
pub use copy::{
    assemble_copy, CopyBuffer, CopyColumns, CopyError, CopyLimits, CopyOutcome, CopyRejection,
    CopySelection,
};
pub use display_set::{
    DisplaySet, ItemDto, RangeFetchError, RangeRequest, RangeResponse, MAX_ITEMS_PER_RESPONSE,
    MAX_RESPONSE_RAW_BYTES,
};
/// コードページの存在確認（`ENC-007`）の再公開です（Issue #39）。
///
/// `src-tauri` が起動時の設定検証へこの判定を注入します
/// （`hakutaku_config::load_config_with_codepage_check`）。下の
/// [`LogDateTimeFormat`] と同じ理由で、呼び出し側へ
/// `hakutaku-format-detection` への直接依存を1本増やさせる代わりに、共通
/// サービス層が公開する関数として通します。
pub use hakutaku_format_detection::codepage_available;
/// 日時書式（`LOG-DT-001`〜`006`）の再公開です（P07）。
///
/// [`LoadControl::manual_datetime_format`] が公開 API でこの型を使うため、
/// 呼び出し側（`src-tauri`）が型名を書けないと値を構築できません。呼び出し側へ
/// `hakutaku-parser` への直接依存を1本増やさせる代わりに、共通サービス層が
/// 公開する型として通します。
pub use hakutaku_parser::LogDateTimeFormat;
pub use item::{reserve_items_growth, Item, ItemId, SourceInfo, RESIDENT_BYTES_PER_ITEM};
pub use line_index::{
    reserve_growth, IndexedText, LineIndexEntry, FLAG_HAS_TIMESTAMP, FLAG_UNCONFIRMED,
    INDEX_BYTES_PER_ENTRY,
};
pub use loader::{
    load_file_into_registry, register_source, register_source_with_access,
    register_source_with_control, reload_source, restore_evicted_source, DatetimeFormatRoute,
    DirectRegistryAccess, LoadControl, LoadFileError, LoadStageTimings, LoadSummary,
    RegisterSourceError, RegisterSourceOutcome, RegistryAccess, ReloadOutcome,
};
pub use ordering::{is_already_open, plan_adhoc_batch_order};
pub use profile_resolution::{resolve_profile, ResolutionOutcome};
pub use registry::{
    fetch_path_metrics, reset_fetch_path_metrics, ChangeKind, DisplaySetHandle, DisplaySetRegistry,
    DisplaySetState, FetchPathMetrics, FetchRangeError, MergedViewHandle, RebuildOutcome,
    SourceStatus, SourceSummary,
};

/// 共通サービス層が担う責務の表示名です。
pub const RESPONSIBILITY: &str = "共通サービス";

/// [`RegisterSourceError`] がアクセス拒否（`ERROR_ACCESS_DENIED`）による失敗
/// かどうかを判定します（`PRIV-002`、P11-1）。
///
/// `src-tauri` がこれを使って「管理者権限で開き直す」導線（アクセス拒否時だけ
/// 「管理者として新しいウィンドウで開く」ボタンを表示する判定）を組み立てます。
///
/// # なぜ `loader.rs` ではなくここに置くか
///
/// 本来は [`RegisterSourceError`]／[`LoadFileError`] を定義する `loader`
/// モジュールにこの判定を置くのが自然ですが、`loader.rs` は別セッション
/// （P08-5、`tasks/phase-08-large-log-viewing.md` 系統の並行作業）が同時に
/// 変更中のため、この P11-1 実装では触れずに温存しています。
/// [`RegisterSourceError::Load`]・[`LoadFileError::ReadFile`] はいずれも
/// 公開enumの公開フィールドであり（`hakutaku_core::ReloadOutcome` 等、他の
/// 箇所でも同様にモジュール外から直接パターンマッチしている設計）、ここから
/// 直接パターンマッチするだけで足りるため、`loader.rs` 自体の変更は不要です。
#[must_use]
pub fn is_access_denied(error: &RegisterSourceError) -> bool {
    matches!(
        error,
        RegisterSourceError::Load(LoadFileError::ReadFile(
            hakutaku_data_source::ReadFileError::AccessDenied { .. }
        ))
    )
}

/// 進捗・エラー・キャンセルの共通契約です（P04-6）。
///
/// P06（大容量読み込み）が発行側、P07（共通シェル UI。`src-tauri` 経由で
/// Tauri イベントへ変換）が受信側として、この契約の型だけに依存すれば
/// 互いの完成を待たずに並行着手できます。型の定義と設計判断はモジュール
/// doc コメントを参照してください（`tasks/phase-04-vertical-slice.md` の
/// P04-6）。
pub mod notification;

/// 現在分離されている GUI 非依存層の責務を返します。
pub const fn responsibilities() -> [&'static str; 4] {
    [
        hakutaku_data_source::RESPONSIBILITY,
        hakutaku_format_detection::RESPONSIBILITY,
        hakutaku_parser::RESPONSIBILITY,
        RESPONSIBILITY,
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{is_access_denied, responsibilities, LoadFileError, RegisterSourceError};

    #[test]
    fn all_core_responsibilities_are_present_and_unique() {
        let responsibilities = responsibilities();
        assert_eq!(responsibilities.len(), 4);
        assert_eq!(
            responsibilities
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len(),
            4
        );
    }

    // --- is_access_denied（PRIV-002、P11-1） ---

    #[test]
    fn is_access_denied_is_true_for_access_denied_read_errors() {
        let error = RegisterSourceError::Load(LoadFileError::ReadFile(
            hakutaku_data_source::ReadFileError::AccessDenied {
                reason: "C:\\example\\a.log".to_string(),
            },
        ));
        assert!(is_access_denied(&error));
    }

    #[test]
    fn is_access_denied_is_false_for_sharing_violation_and_budget_rejection() {
        let sharing = RegisterSourceError::Load(LoadFileError::ReadFile(
            hakutaku_data_source::ReadFileError::SharingViolation {
                reason: "locked".to_string(),
            },
        ));
        assert!(!is_access_denied(&sharing));

        let io_error = RegisterSourceError::Load(LoadFileError::ReadFile(
            hakutaku_data_source::ReadFileError::Io {
                reason: "other".to_string(),
            },
        ));
        assert!(!is_access_denied(&io_error));
    }
}
