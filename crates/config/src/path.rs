//! パスの種別判定と正規化（ADR-0005: 設定の読み込みパスは絶対ローカルパス以外を
//! 検証エラーとする）。
//!
//! # 判定規則
//!
//! - 許可: ドライブレター付きの絶対パス（例: `C:\Device\Logs`、`C:/Device/Logs`）と、
//!   ローカルの verbatim パス（例: `\\?\C:\Device\Logs`）
//! - エラー: 相対パス（例: `logs\a.log`）、ドライブ相対パス（例: `C:logs`）、
//!   ルート相対パス（例: `\logs`）、ネットワーク共有パス（UNC。例: `\\server\share`、
//!   `\\?\UNC\...`）、デバイス名前空間のパス（例: `\\.\PhysicalDrive0`、
//!   `\\?\Volume{...}`）
//!
//! 判定は**文字列の形式判定のみ**で行い、ファイルシステムへは一切アクセスしない
//! （ADR-0005）。ログ解析プロファイルの `path_pattern`（glob）にも同じ規則を適用する。
//! パターンにワイルドカード文字（`*` などの glob 記号）が含まれていても、判定は
//! 文字列の**先頭部分**だけを見るため、そのまま同じ関数を適用できる。
//!
//! # 正規化規則
//!
//! [`normalize_path_separators`] は区切り文字を `\` へ統一する（大文字・小文字は
//! 変更しない）。[`paths_equivalent`] は、この正規化を適用したうえで、Windows の
//! ファイルシステム既定（大文字・小文字を区別しない）に合わせた比較を行う。
//!
//! この2つの関数は、P05 のログ解析プロファイル照合が同じ規則で再利用できるように、
//! `crates/config` の公開 API として置いている（`tasks/phase-03-configuration.md`
//! 作業項目3）。

/// 読み込み対象パスの種別（ADR-0005 の判定表）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    /// ドライブレター付きの絶対パス（例: `C:\Device\Logs`、`C:/Device/Logs`）。許可。
    DriveAbsolute,
    /// ローカルの verbatim パス（例: `\\?\C:\Device\Logs`）。許可。
    LocalVerbatim,
    /// ドライブ相対パス（例: `C:logs`）。エラー。
    DriveRelative,
    /// ルート相対パス（例: `\logs`）。エラー。
    RootRelative,
    /// ネットワーク共有パス（UNC。例: `\\server\share`、`\\?\UNC\...`）。エラー。
    Unc,
    /// デバイス名前空間など、ファイルシステム上の場所ではない特殊な名前空間の
    /// パス（例: `\\.\PhysicalDrive0`、`\\.\COM1`、`\\?\Volume{...}`）。エラー。
    ///
    /// UNC と別の種別として持つのは、利用者へ示す理由を取り違えないためである
    /// （Issue #39）。`\\.\` で始まるパスはネットワーク共有ではないため、
    /// 「ネットワーク共有パス（UNC）が指定されています」と表示すると、利用者は
    /// 実際の誤りとは別の原因を探すことになる。
    DeviceNamespace,
    /// 相対パス（例: `logs\a.log`）。エラー。
    Relative,
}

impl PathKind {
    /// ADR-0005 の判定表で許可される種別（絶対ローカルパス）かどうか。
    #[must_use]
    pub fn is_allowed(self) -> bool {
        matches!(self, PathKind::DriveAbsolute | PathKind::LocalVerbatim)
    }
}

/// パス文字列の先頭部分から [`PathKind`] を判定する（ADR-0005）。
///
/// ファイルシステムへは一切アクセスせず、文字列の形式だけで判定する。
#[must_use]
pub fn classify_path(path: &str) -> PathKind {
    if let Some(rest) = strip_prefix_case_insensitive(path, r"\\?\") {
        // `\\?\UNC\...` はローカル verbatim ではなく UNC verbatim（ネットワーク共有）。
        if starts_with_case_insensitive(rest, r"UNC\") || rest.eq_ignore_ascii_case("UNC") {
            return PathKind::Unc;
        }
        // `\\?\C:\...` の形だけをローカル verbatim として許可する。
        return if is_drive_absolute(rest) {
            PathKind::LocalVerbatim
        } else {
            // `\\?\` に続く形式がドライブ絶対でも UNC でもない場合（例:
            // `\\?\Volume{...}`）は、絶対ローカルパスとして認められないため
            // 安全側（エラー）へ倒す。ネットワーク共有ではないので、理由は
            // デバイス名前空間側で示す。
            PathKind::DeviceNamespace
        };
    }

    // デバイス名前空間（`\\.\`）の判定は UNC より前に置く。どちらも `\\` で
    // 始まるため、順序を入れ替えるとネットワーク共有として報告してしまう。
    // 拒否する点は同じでも、利用者が探すべき誤りは別である（Issue #39）。
    if starts_with_case_insensitive(path, r"\\.\") || starts_with_case_insensitive(path, "//./") {
        return PathKind::DeviceNamespace;
    }

    // UNC の判定は、後段のルート相対（`\logs`）の判定より前に置く必要がある。
    // `\\server\share` も先頭が `\` であり、順序を入れ替えるとルート相対として
    // 報告してしまう。どちらも同じくエラーだが、UNC は意図しないネットワーク
    // アクセスを起動前に遮断するという別の意味を持つため（ADR-0005 のセキュリティ
    // 影響、`SEC-001`）、利用者へ示す理由を取り違えない。
    if path.starts_with(r"\\") || path.starts_with("//") {
        return PathKind::Unc;
    }

    if is_drive_absolute(path) {
        return PathKind::DriveAbsolute;
    }

    if is_drive_relative(path) {
        return PathKind::DriveRelative;
    }

    if path.starts_with('\\') || path.starts_with('/') {
        return PathKind::RootRelative;
    }

    PathKind::Relative
}

/// ADR-0005 の判定表に従い、絶対ローカルパスとして許可される形式かどうかを判定する。
#[must_use]
pub fn is_absolute_local_path(path: &str) -> bool {
    classify_path(path).is_allowed()
}

/// パスの区切り文字を `\` へ統一する。大文字・小文字は変更しない。
///
/// P05 のログ解析プロファイル照合は、この関数で区切りを統一してから比較する
/// （作業項目3の「正規化の実装を P03 に置く」に対応する）。
#[must_use]
pub fn normalize_path_separators(path: &str) -> String {
    path.replace('/', "\\")
}

/// OS 慣例（Windows のファイルシステム既定である、大文字・小文字を区別しない比較）
/// に従って、2つのパス文字列が同じ場所を指すかを比較する。
///
/// 内部で [`normalize_path_separators`] を適用してから、`str::to_uppercase` で
/// 大文字小文字を畳み込んで比較する。**注意:** これは NTFS 自体の大文字小文字畳み込み
/// 規則（コードポイントごとの例外を含む）を完全に再現するものではない近似実装であり、
/// 通常の ASCII 主体のパスでは実用上問題にならない。
///
/// # 近似の帰結（Issue #39 で明文化）
///
/// この比較は文字列だけを見る（ADR-0005 のとおりファイルシステムへ問い合わせ
/// ない）ため、次は同じ場所を指していても**別のパス**として扱う。起動時検証の
/// 重複検出（`log_profiles` の同一パターン検出）も同じ限界を持ち、これらの
/// 書き分けは重複として検出されない。
///
/// - `.` / `..` を含むパス（例: `C:\a\..\a\x.log` と `C:\a\x.log`）
/// - 末尾の区切り文字の有無（例: `C:\a\` と `C:\a`）
/// - 短縮名（8.3 形式。例: `C:\PROGRA~1`）と長い名前
/// - シンボリックリンク・ジャンクション経由のパスと実体のパス
///
/// また、畳み込みに `str::to_uppercase` を使うため、1文字が複数文字へ変わる
/// コードポイント（例: `ß` → `SS`）では**文字数が変わる**。完全一致の比較では
/// 両辺に同じ変換を適用するため問題にならないが、`?`（1文字一致）を使う
/// [`crate::glob_match`] の照合では、そうした文字を含むパスで一致する文字数が
/// 直感と食い違い得る。
#[must_use]
pub fn paths_equivalent(a: &str, b: &str) -> bool {
    normalize_path_separators(a).to_uppercase() == normalize_path_separators(b).to_uppercase()
}

/// 大文字・小文字を区別せずに `needle` で始まるかを判定する。
///
/// `haystack` の該当箇所が UTF-8 の文字境界でない場合は一致しないものとして扱う
/// （`str::get` を使うためパニックしない）。
fn starts_with_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack
        .get(..needle.len())
        .is_some_and(|slice| slice.eq_ignore_ascii_case(needle))
}

/// [`starts_with_case_insensitive`] が真の場合に、`needle` を取り除いた残りを返す。
fn strip_prefix_case_insensitive<'a>(haystack: &'a str, needle: &str) -> Option<&'a str> {
    if starts_with_case_insensitive(haystack, needle) {
        Some(&haystack[needle.len()..])
    } else {
        None
    }
}

/// 先頭3文字が `<ドライブレター>:\` または `<ドライブレター>:/` の形かどうか。
fn is_drive_absolute(path: &str) -> bool {
    let chars: Vec<char> = path.chars().take(3).collect();
    chars.len() == 3
        && is_drive_letter(chars[0])
        && chars[1] == ':'
        && matches!(chars[2], '\\' | '/')
}

/// `<ドライブレター>:` で始まるが、その直後が区切り文字ではない（＝絶対パスでない）形。
fn is_drive_relative(path: &str) -> bool {
    let chars: Vec<char> = path.chars().take(3).collect();
    chars.len() >= 2
        && is_drive_letter(chars[0])
        && chars[1] == ':'
        && !(chars.len() == 3 && matches!(chars[2], '\\' | '/'))
}

fn is_drive_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

#[cfg(test)]
mod tests {
    use super::{
        classify_path, is_absolute_local_path, normalize_path_separators, paths_equivalent,
        PathKind,
    };

    // ADR-0005 の判定表: 許可される形式。

    #[test]
    fn drive_absolute_backslash_is_allowed() {
        assert_eq!(classify_path(r"C:\Device\Logs"), PathKind::DriveAbsolute);
        assert!(is_absolute_local_path(r"C:\Device\Logs"));
    }

    #[test]
    fn drive_absolute_forward_slash_is_allowed() {
        assert_eq!(classify_path("C:/Device/Logs"), PathKind::DriveAbsolute);
        assert!(is_absolute_local_path("C:/Device/Logs"));
    }

    #[test]
    fn local_verbatim_is_allowed() {
        assert_eq!(
            classify_path(r"\\?\C:\Device\Logs"),
            PathKind::LocalVerbatim
        );
        assert!(is_absolute_local_path(r"\\?\C:\Device\Logs"));
    }

    // ADR-0005 の判定表: エラーになる形式。

    #[test]
    fn relative_path_is_denied() {
        assert_eq!(classify_path(r"logs\a.log"), PathKind::Relative);
        assert!(!is_absolute_local_path(r"logs\a.log"));
    }

    #[test]
    fn drive_relative_path_is_denied() {
        assert_eq!(classify_path("C:logs"), PathKind::DriveRelative);
        assert!(!is_absolute_local_path("C:logs"));
    }

    #[test]
    fn bare_drive_and_colon_is_drive_relative() {
        // "C:" だけ（カレントディレクトリ相対）もドライブ相対として扱う。
        assert_eq!(classify_path("C:"), PathKind::DriveRelative);
    }

    #[test]
    fn root_relative_path_is_denied() {
        assert_eq!(classify_path(r"\logs"), PathKind::RootRelative);
        assert!(!is_absolute_local_path(r"\logs"));
        assert_eq!(classify_path("/logs"), PathKind::RootRelative);
    }

    #[test]
    fn unc_path_is_denied() {
        assert_eq!(classify_path(r"\\server\share"), PathKind::Unc);
        assert!(!is_absolute_local_path(r"\\server\share"));
    }

    #[test]
    fn unc_verbatim_path_is_denied() {
        assert_eq!(classify_path(r"\\?\UNC\server\share"), PathKind::Unc);
        assert!(!is_absolute_local_path(r"\\?\UNC\server\share"));
    }

    #[test]
    fn forward_slash_unc_path_is_denied() {
        assert_eq!(classify_path("//server/share"), PathKind::Unc);
    }

    // 受け入れ条件: デバイス名前空間のパスは、UNC ではなく専用の種別として
    // 拒否する（利用者へ示す理由を取り違えない。Issue #39）。
    #[test]
    fn device_namespace_path_is_denied_as_its_own_kind() {
        assert_eq!(
            classify_path(r"\\.\PhysicalDrive0"),
            PathKind::DeviceNamespace
        );
        assert_eq!(classify_path(r"\\.\COM1"), PathKind::DeviceNamespace);
        assert!(!is_absolute_local_path(r"\\.\PhysicalDrive0"));
        // 区切り文字を `/` で書いた形も同じ扱いにする。
        assert_eq!(
            classify_path("//./PhysicalDrive0"),
            PathKind::DeviceNamespace
        );
    }

    // 受け入れ条件: `\\?\` に続く形式がドライブ絶対でも UNC でもない場合
    // （例: ボリューム GUID）も、ネットワーク共有ではないため UNC とは
    // 区別して拒否する。
    #[test]
    fn verbatim_volume_guid_path_is_denied_as_device_namespace() {
        assert_eq!(
            classify_path(r"\\?\Volume{12345678-1234-1234-1234-123456789abc}\Logs"),
            PathKind::DeviceNamespace
        );
    }

    // glob パターンへの適用（基点の判定。P05 が意味解釈を行う対象）。

    #[test]
    fn glob_pattern_with_absolute_base_is_allowed() {
        assert!(is_absolute_local_path(r"C:\Device\Logs\*.log"));
    }

    #[test]
    fn glob_pattern_with_relative_base_is_denied() {
        assert!(!is_absolute_local_path("*.log"));
    }

    // 正規化。

    #[test]
    fn normalize_path_separators_unifies_to_backslash() {
        assert_eq!(
            normalize_path_separators("C:/Device/Logs"),
            r"C:\Device\Logs"
        );
        // 既に `\` の場合はそのまま。
        assert_eq!(
            normalize_path_separators(r"C:\Device\Logs"),
            r"C:\Device\Logs"
        );
    }

    #[test]
    fn paths_equivalent_ignores_separator_and_case() {
        assert!(paths_equivalent(r"C:\Device\Logs", "c:/device/logs"));
        assert!(!paths_equivalent(r"C:\Device\Logs", r"C:\Device\Other"));
    }
}
