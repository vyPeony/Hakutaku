//! ログ解析プロファイルの glob 照合（`LOG-021` の第3段階）。
//!
//! [`crate::path`] の正規化（[`crate::path::normalize_path_separators`]）と
//! 大文字小文字不区別比較の考え方を再利用し、`*`・`?`・`**` を解釈する自前実装
//! です。外部 glob クレートは追加しません（`tasks/phase-05-log-parsing-core.md`
//! 作業項目3の禁止事項、外部依存を増やさない方針）。
//!
//! # 階層規則（暫定設計。要件 ID を持たない。4.1 の暫定方針）
//!
//! - `*` と `?` は**1階層内**だけを対象とし、パス区切り `\` をまたがない。
//!   `*` は0文字以上の任意の文字列、`?` は任意の1文字に一致する
//! - `**` は、パスの**1階層（セグメント）全体を占める場合に限り**特別扱いし、
//!   0階層以上の任意の階層数に一致する。例えば `C:\a\**\b.log` は次のいずれにも
//!   一致する。
//!     - `C:\a\b.log`（0階層。`**` が何も消費しない場合を許す設計）
//!     - `C:\a\x\b.log`（1階層）
//!     - `C:\a\x\y\b.log`（2階層以上）
//!
//!   0階層一致を許すのは、「`**` の間に何段あってもよい」という利用者の直感
//!   （`x\**\y.log` は `x` 直下の `y.log` にも一致してほしい）に合わせるためで
//!   あり、`tasks/phase-05-log-parsing-core.md` が「0階層一致を許す設計を推奨」
//!   としている暫定方針にも合致する
//! - `a**b` のように `**` が他の文字と同じセグメントに混在する場合は、階層を
//!   またがない通常の `*` の連続として扱う（`**` は単一の `*` を2つ並べたのと
//!   同じ結果になり、複数階層への特別扱いは**セグメント全体が `**` の場合だけ**
//!   である）
//! - エスケープ機構は無い。Windows のファイル名には `*` や `?` がそもそも
//!   使えない（Windows の予約文字）ため、リテラルの `*`・`?` を表現する必要が
//!   ないという前提である（`tasks/phase-05-log-parsing-core.md` 作業項目2）
//!
//! # 大文字・小文字
//!
//! [`crate::path::paths_equivalent`] と同じ近似実装（`str::to_uppercase` に
//! よる畳み込み）で大文字・小文字を区別しない。NTFS 自体の畳み込み規則を完全に
//! 再現するものではない近似である（[`crate::path::paths_equivalent`] の doc
//! コメントを参照）。

use crate::path::normalize_path_separators;

/// パターン文字列が glob 記号（`*` または `?`）を含むかどうか。
///
/// 含まない場合は絶対パス完全一致の指定として扱う
/// （[`crate::schema::LogProfileConfig::path_pattern`] のドキュメント、
/// `LOG-021` の第2段階）。
#[must_use]
pub fn is_glob_pattern(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

/// 正規化前のパターンと候補パスを、大文字・小文字を区別せず glob 照合する。
///
/// 呼び出し側で `pattern` の基点が絶対ローカルパスであることを検証済みで
/// あることを前提とする（この関数自体は ADR-0005 の絶対性検証を行わない。
/// 起動時検証は [`crate::load::load_config`] が別途行う）。
#[must_use]
pub fn glob_match(pattern: &str, candidate: &str) -> bool {
    // 照合前に区切り統一と大文字化を済ませる。畳み込みを
    // [`crate::path::paths_equivalent`] と揃えることで、`LOG-021` の第2段階
    // （絶対パス完全一致）と第3段階（glob）で大文字・小文字の扱いが食い違わない
    // （`tasks/phase-05-log-parsing-core.md` の「完全一致と glob は、正規化した
    // ローカル絶対パスに対して Windows と同様に大文字・小文字を区別せず評価」）。
    let pattern = normalize_path_separators(pattern).to_uppercase();
    let candidate = normalize_path_separators(candidate).to_uppercase();
    let pattern_segments: Vec<&str> = pattern.split('\\').collect();
    let candidate_segments: Vec<&str> = candidate.split('\\').collect();
    match_segments(&pattern_segments, &candidate_segments)
}

/// パスをパス区切りで分割した「セグメント（階層）」の並びどうしを再帰的に
/// 照合する。
///
/// `**` セグメントだけを特別扱いし（0階層以上への展開）、それ以外の
/// セグメントは1つずつ [`segment_matches`] へ委ねる。
fn match_segments(pattern: &[&str], candidate: &[&str]) -> bool {
    let Some(&first) = pattern.first() else {
        return candidate.is_empty();
    };
    let rest = &pattern[1..];

    if first == "**" {
        // `**` は0階層以上の任意のセグメント数を消費し得る。消費数
        // （0..=candidate.len()）を総当たりし、残りが一致すれば真とする。
        return (0..=candidate.len()).any(|skip| match_segments(rest, &candidate[skip..]));
    }

    let Some(&candidate_first) = candidate.first() else {
        return false;
    };
    segment_matches(first, candidate_first) && match_segments(rest, &candidate[1..])
}

/// 1階層（セグメント）分の文字列を `*`・`?` で照合する。
///
/// 古典的な2ポインタ + バックトラックによるワイルドカード一致判定
/// （最悪計算量 O(パターン長 × 対象長)）。パスの1階層分の文字数は小さいため、
/// 実用上のパフォーマンス上の懸念はない。文字単位（`char`）で扱うため、
/// 日本語ファイル名などマルチバイト文字が含まれていても境界を壊さない。
fn segment_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();

    let (mut p, mut t) = (0usize, 0usize);
    // 直近に見た `*` の位置と、そのときの対象側の位置（バックトラック用）。
    let mut star_pattern: Option<usize> = None;
    let mut star_text = 0usize;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star_pattern = Some(p);
            star_text = t;
            p += 1;
        } else if let Some(star_p) = star_pattern {
            // 直前の `*` に1文字多く飲み込ませてやり直す。
            p = star_p + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }

    // パターン末尾に残った `*` は0文字にも一致するため読み飛ばす。
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::{glob_match, is_glob_pattern};

    #[test]
    fn is_glob_pattern_detects_asterisk_and_question_mark() {
        assert!(is_glob_pattern(r"C:\a\*.log"));
        assert!(is_glob_pattern(r"C:\a\?.log"));
        assert!(!is_glob_pattern(r"C:\a\b.log"));
    }

    // 受け入れ条件: `*` は1階層内（パス区切りをまたがない）。
    #[test]
    fn single_star_does_not_cross_directory_boundary() {
        assert!(glob_match(r"C:\a\*.log", r"C:\a\b.log"));
        assert!(!glob_match(r"C:\a\*.log", r"C:\a\b\c.log"));
    }

    #[test]
    fn question_mark_matches_exactly_one_character() {
        assert!(glob_match(r"C:\a\?.log", r"C:\a\b.log"));
        assert!(!glob_match(r"C:\a\?.log", r"C:\a\bb.log"));
        assert!(!glob_match(r"C:\a\?.log", r"C:\a\.log"));
    }

    // 受け入れ条件: `**` は複数階層（0階層以上）に一致する。
    #[test]
    fn double_star_matches_multiple_directories_including_zero() {
        assert!(glob_match(r"C:\a\**\b.log", r"C:\a\b.log")); // 0階層
        assert!(glob_match(r"C:\a\**\b.log", r"C:\a\x\b.log")); // 1階層
        assert!(glob_match(r"C:\a\**\b.log", r"C:\a\x\y\b.log")); // 2階層以上
        assert!(!glob_match(r"C:\a\**\b.log", r"C:\a\x\c.log"));
    }

    #[test]
    fn double_star_with_trailing_star_pattern_matches_nested_files() {
        assert!(glob_match(r"C:\a\**\*.log", r"C:\a\b\c.log"));
        assert!(glob_match(r"C:\a\**\*.log", r"C:\a\file.log")); // 0階層
        assert!(!glob_match(r"C:\a\**\*.log", r"C:\a\b\c.txt"));
    }

    // 大文字・小文字を区別しない（Windows のファイルシステム既定）。
    #[test]
    fn matching_is_case_insensitive() {
        assert!(glob_match(r"C:\LOGS\*.log", r"c:\logs\A.LOG"));
    }

    // 区切り文字の正規化（`/` と `\` を同一視する）。
    #[test]
    fn forward_and_back_slashes_are_treated_as_equivalent() {
        assert!(glob_match("C:/a/*.log", r"C:\a\b.log"));
    }

    #[test]
    fn mixed_double_star_within_a_segment_does_not_cross_boundary() {
        // "a**b" は「1階層内の `*` の連続」として扱う（複数階層への特別扱いは
        // セグメント全体が "**" の場合だけ）。
        assert!(glob_match(r"C:\a\pre**post.log", r"C:\a\preXpost.log"));
        assert!(!glob_match(r"C:\a\pre**post.log", r"C:\a\x\preXpost.log"));
    }

    #[test]
    fn non_matching_pattern_returns_false() {
        assert!(!glob_match(r"C:\a\*.log", r"C:\b\c.log"));
    }

    #[test]
    fn exact_pattern_without_glob_symbols_matches_only_itself() {
        assert!(glob_match(r"C:\a\b.log", r"C:\a\b.log"));
        assert!(!glob_match(r"C:\a\b.log", r"C:\a\c.log"));
    }
}
