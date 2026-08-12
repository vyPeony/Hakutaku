#![forbid(unsafe_code)]

//! パーサー層（GUI 非依存、`crates/core-services` から呼ばれる）。
//!
//! P04（`tasks/phase-04-vertical-slice.md`）では `LOG-DT-001` 固定の最小解析
//! （`parse_line`／`ParsedTimestamp`）だけを持っていましたが、P05
//! （`tasks/phase-05-log-parsing-core.md`）・P05-6（読み込み経路への統合）で
//! [`mod@datetime`] の6書式 API（[`parse_datetime_auto`]／
//! [`parse_datetime_with_format`]／[`DateTimeMatch`]）へ一本化しました。P04 の
//! 呼び出し側（`crates/core-services/src/loader.rs`）は P05-6 でこの新 API へ
//! 差し替え済みのため、桁数固定の旧 API は残していません（同じ役割の実装を
//! 二か所に残さない）。
//!
//! すべての解析関数は副作用のない純関数で、GUI（Tauri）なしで単体テストできます。

mod datetime;

pub use datetime::{
    parse_datetime_auto, parse_datetime_with_format, AutoParseOutcome, ComparisonKey,
    DateTimeMatch, LogDateTimeFormat, Precision,
};

/// パーサー層が担う責務の表示名です。
pub const RESPONSIBILITY: &str = "パーサー";

/// `year`/`month`/`day` が暦として成立するかを判定します（うるう年を含む）。
/// Win32 API を呼ばない純粋な計算です。
fn is_valid_calendar_date(year: u32, month: u32, day: u32) -> bool {
    if !(1..=12).contains(&month) {
        return false;
    }
    if day == 0 {
        return false;
    }
    day <= days_in_month(year, month)
}

/// 指定した年月の日数を返します。`month` は `1..=12` の範囲である前提です
/// （呼び出し元 [`is_valid_calendar_date`] が事前に検証済み）。
fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// グレゴリオ暦のうるう年判定です。
fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responsibility_is_explicit() {
        assert_eq!(RESPONSIBILITY, "パーサー");
    }

    // is_valid_calendar_date（`crate::datetime` の6書式すべてが共有する暦検証）
    // の単体テスト。`parse_datetime_with_format`／`parse_datetime_auto` 側の
    // 受け入れ条件テストは `crate::datetime` に既にあるため、ここでは
    // 境界値そのものを直接確認する。

    // 受け入れ条件: 不正な日付（暦として存在しない日時）は不正と判定される。
    #[test]
    fn invalid_calendar_date_is_rejected() {
        assert!(
            !is_valid_calendar_date(2026, 2, 29),
            "2026年は平年なので2月29日は存在しない"
        );
        assert!(!is_valid_calendar_date(2026, 13, 1), "13月は不正");
        assert!(!is_valid_calendar_date(2026, 0, 1), "0月は不正");
        assert!(
            !is_valid_calendar_date(2026, 4, 31),
            "4月31日は不正（4月は30日まで）"
        );
    }

    // 受け入れ条件: うるう年の2月29日は正しく成立する
    // （is_valid_calendar_date のうるう年判定が正しいことの確認）。
    #[test]
    fn leap_year_february_29_is_valid() {
        assert!(is_valid_calendar_date(2028, 2, 29));
        // 世紀年は400で割り切れる場合だけうるう年（西暦2000年はうるう年）。
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(1900));
        assert!(is_leap_year(2028));
        assert!(!is_leap_year(2026));
    }

    #[test]
    fn valid_calendar_date_is_accepted() {
        assert!(is_valid_calendar_date(2026, 7, 28));
        assert!(is_valid_calendar_date(2026, 12, 31));
        assert!(is_valid_calendar_date(2026, 1, 1));
    }
}
