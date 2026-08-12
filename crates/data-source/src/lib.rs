#![deny(unsafe_op_in_unsafe_fn)]

//! データソース層（GUI 非依存。`crates/core-services` から呼ばれる）。
//!
//! P04（`tasks/phase-04-vertical-slice.md`）のファイル読み込みを実装します。
//!
//! # このクレートが行うこと
//!
//! - ファイルを読み取り専用・共有可（他プロセスの書き込みを妨げない）で開く
//!   （`ENV-010`: 同じ端末で稼働する他の業務ソフトウェアが書き込み中のログを
//!   開くことを標準ケースとする）。
//! - `PERF-010` に従い、読み込みバッファの確保**前**に
//!   [`hakutaku_memory_accounting::MemoryBudget::reserve`] で予約し、確保後に
//!   [`hakutaku_memory_accounting::ReservationToken::mark_allocated`] で実確保へ
//!   振り替える。予約が拒否された場合はエラーを返し、読み込みを行わない。
//! - `\r\n` と `\n` の両方に対応した行分割を行い、1 起点の行番号を付ける
//!   （[`split_lines`]。文字コード判定後の文字列にも適用できるよう `pub` に
//!   しています）。
//!
//! # P05（`tasks/phase-05-log-parsing-core.md`）での拡張
//!
//! P04 は UTF-8 固定でバイト列を文字列へ変換していました（不正なバイト列は
//! 置換文字 `U+FFFD` で許容する割り切り）。P05 では文字コード判定
//! （`crates/format-detection`）がプロファイルの `encoding`／`ansi_codepage` に
//! 応じて判定・デコードを行うため、このクレートは**デコード前の生バイト列**を
//! 返す入口（[`read_file_bytes`]／[`read_file_bytes_with_budget`]）を追加で
//! 提供します。[`read_file`]／[`read_file_with_budget`]（UTF-8 固定・置換文字
//! 許容）は既存呼び出し側との互換のためフィールド・挙動を変えずに残しており、
//! 内部では [`read_file_bytes_with_budget`] を呼んでからデコードする実装に
//! 揃えています（ファイルを開く・予約する処理の二重実装を避けるため）。
//!
//! # P06（`tasks/phase-06-large-file-loading.md`）での拡張
//!
//! 複数ファイル・変更検知・書き込み中ログの扱いを実装します。
//!
//! - [`FileSnapshot`][]・[`capture_snapshot`][]・[`open_and_snapshot`][]:
//!   ファイルを開いた時点でファイル識別子（ボリューム連番＋ファイルインデック
//!   ス）・サイズ・最終更新時刻を記録します。開いた時点で観測した長さは
//!   `snapshot_end` として固定します（ADR-0007）。
//! - [`read_snapshotted_bytes`][]: `snapshot_end` を上限に読み込みます。
//!   判定後に追記された分はその回では読みません（`LOG-010`、`LOG-028`）。
//! - [`verify_snapshot`][]: 未読み込み範囲を読む前に整合性を再確認し、
//!   同一（追記のみ含む）／縮小／置換／削除を区別します（[`SnapshotVerdict`]、
//!   `LOG-023`）。
//! - [`RawLine::confirmed`][]: `snapshot_end` 時点の末尾が改行で終わらない場合、
//!   最終行を未確定行としてマークします（`LOG-026`）。解析エラーにはしません。
//!
//! # 共有違反の区別（`LOG-027`、P06-5）
//!
//! Windows でファイルを開く際、対象が「共有を許可しない方法」（例:
//! `FileShare.None`）で既に開かれている場合、`ERROR_SHARING_VIOLATION`
//! （Win32 エラーコード 32）で失敗します。[`is_sharing_violation`] はこれを
//! 他の I/O エラーと区別する判定関数です。[`ReadFileError::SharingViolation`]・
//! [`snapshot::ReopenForReloadError::SharingViolation`] がこの判定を使います。
//!
//! **ロックの強制解除やコピーによる迂回は実装しません。** 対象と理由を
//! 示し、利用者が再試行できる経路（同じパスでの再オープン）だけを提供します。
//!
//! # アクセス拒否の区別（`PRIV-002`、P11-1）
//!
//! ファイルを開く際に Windows が `ERROR_ACCESS_DENIED`（Win32 エラーコード 5）
//! を返した場合、[`ReadFileError::AccessDenied`] として共有違反・その他の
//! I/O エラーと区別します。[`is_access_denied`] が判定関数です。
//!
//! 共有違反（他プロセスが排他的に開いている、再試行すれば解消し得る）とは異なり、
//! アクセス拒否は権限不足が原因であり、`crates/core-services`・`src-tauri` 側が
//! 「管理者権限で開き直す」導線（昇格プロセスの起動、`PRIV-002`〜`004`）を
//! 提示するために、この区別を独立した分類として公開します。本クレート自身は
//! 昇格や権限変更を一切行いません（分類だけを提供する下位層のままです）。
//!
//! ファイル識別子の取得は Windows 専用です（`GetFileInformationByHandle`。
//! `std::os::windows::fs::MetadataExt::volume_serial_number`／`file_index` は
//! 現行の安定版 Rust では unstable な `windows_by_handle` の機能ゲート下にあり
//! 使えないため、`windows` クレート経由で直接呼び出します。実装は
//! `crate::win32` に分離し、Windows 以外でもコンパイルが通るようにしています
//! （`crates/format-detection/src/win32.rs` と同じ分離方針）。
//!
//! 実際の日時解析（`crates/parser`）や表示集合の構築（`crates/core-services`）は
//! このクレートの対象外です。

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

mod chunk;
mod snapshot;
#[cfg(windows)]
mod win32;

pub use chunk::{
    read_snapshotted_bytes_chunked, stream_snapshotted_bytes_chunked, ChunkReadError,
    ChunkReadOutcome, ChunkReadSummary, ChunkedReadRequest, IoThrottle, DEFAULT_CHUNK_BYTES,
    PATH_VERIFY_CHUNK_INTERVAL,
};
pub use snapshot::{
    capture_snapshot, compare_snapshots, open_and_snapshot, read_snapshotted_bytes,
    reopen_for_reload, reset_snapshot_verify_metrics, snapshot_verify_metrics, verify_snapshot,
    verify_snapshot_by_handle, FileIdentity, FileSnapshot, ReopenForReloadError, SnapshotVerdict,
    SnapshotVerifyMetrics, VerifySnapshotError,
};

/// データソース層が担う責務の表示名です。
pub const RESPONSIBILITY: &str = "データソース";

/// Hakutaku のデータソース境界で許可するアクセス方法です。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// 参照元を変更しない読み取り専用アクセスです。
    ReadOnly,
}

/// データソース層で許可するアクセス方法を返します。
pub const fn access_mode() -> AccessMode {
    AccessMode::ReadOnly
}

/// 行分割後の1行です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLine {
    /// 1起点の行番号。
    pub line_number: u64,
    /// UTF-8 固定で読み込んだ行本文（改行文字は含まない）。不正なバイト列は
    /// 置換文字（`U+FFFD`）で許容する（P04 の割り切り。正式な文字コード処理は
    /// P05）。
    pub text: String,
    /// 改行で確定した行かどうか（`LOG-026`）。
    ///
    /// `false` は、読み込んだ範囲（`snapshot_end`）の末尾がこの行の途中で
    /// 終わっている「未確定行」であることを示します。書き込み途中のログを
    /// 開いた場合の標準的な状態であり、断片を破棄せず `text` にそのまま保持
    /// します。解析エラーにはしません。
    ///
    /// 末尾に改行を持たない行は必ず最後の1行だけであり、それ以外の行は常に
    /// `true` です（`split_lines` の実装上の性質）。
    pub confirmed: bool,
}

/// ファイルを読み込み、行分割まで終えた結果です。
#[derive(Debug, Clone)]
pub struct LoadedFile {
    /// 読み込み時点のファイルサイズ（バイト）。予約に使った量と一致します。
    pub file_size_bytes: u64,
    /// `PERF-010` に従い、実確保へ振り替えた量（バイト）。診断ログ
    /// （行数・バイト数・予約量）の記録に使います。
    pub reserved_bytes: usize,
    /// 行（`\r\n`・`\n` の両方に対応して分割済み）。
    pub lines: Vec<RawLine>,
}

/// ファイルを読み込んだ、デコード**前**の生バイト列です（P05）。
///
/// 文字コード判定（`crates/format-detection::detect_encoding`）はデコード前の
/// バイト列（BOM・先頭バイトパターン）を見る必要があるため、[`read_file`]の
/// ような即時 UTF-8 変換を行わずに公開します。
#[derive(Debug, Clone)]
pub struct LoadedBytes {
    /// 読み込み時点のファイルサイズ（バイト）。予約に使った量と一致します。
    pub file_size_bytes: u64,
    /// `PERF-010` に従い、実確保へ振り替えた量（バイト）。
    pub reserved_bytes: usize,
    /// 読み込んだ生バイト列（デコード前）。
    pub bytes: Vec<u8>,
}

/// ファイル読み込みが失敗した理由です。
#[derive(Debug)]
pub enum ReadFileError {
    /// ファイルを開く・メタデータ取得・読み込みのいずれかで I/O エラーが発生した。
    Io { reason: String },
    /// `PERF-010` の予約が拒否された（メモリ予算超過）。読み込みは行っていない。
    ReservationRejected(hakutaku_memory_accounting::ReservationRejected),
    /// 共有を許可しない方法で既に開かれていて読み取れない
    /// （`ERROR_SHARING_VIOLATION`、`LOG-027`）。ロックの強制解除やコピーに
    /// よる迂回は行わず、対象と理由を示して再試行を促す。
    SharingViolation { reason: String },
    /// アクセスが拒否された（`ERROR_ACCESS_DENIED`、`PRIV-002`、P11-1）。
    /// 管理者権限で開き直すことで解消し得るため、共有違反（再試行すれば
    /// 解消し得る一時的な競合）とは別に区別する。
    AccessDenied { reason: String },
}

impl std::fmt::Display for ReadFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadFileError::Io { reason } => write!(f, "ファイルを読み込めません: {reason}"),
            ReadFileError::ReservationRejected(rejected) => write!(f, "{rejected}"),
            ReadFileError::SharingViolation { reason } => write!(
                f,
                "他のプロセスが共有を許可せずに開いているため読み取れません（LOG-027）: {reason}"
            ),
            ReadFileError::AccessDenied { reason } => write!(
                f,
                "アクセスが拒否されました。管理者権限で開き直すことができます（PRIV-002）: {reason}"
            ),
        }
    }
}

impl std::error::Error for ReadFileError {}

impl ReadFileError {
    /// 共有違反（`LOG-027`）による失敗かどうかを返します。呼び出し側
    /// （`crates/core-services`）が利用者向けエラーの理由・次操作を
    /// 使い分けるために使います。
    #[must_use]
    pub fn is_sharing_violation(&self) -> bool {
        matches!(self, ReadFileError::SharingViolation { .. })
    }

    /// アクセス拒否（`PRIV-002`）による失敗かどうかを返します。呼び出し側
    /// （`crates/core-services`・`src-tauri`）が「管理者権限で開き直す」導線
    /// を表示するかどうかの判定に使います。
    #[must_use]
    pub fn is_access_denied(&self) -> bool {
        matches!(self, ReadFileError::AccessDenied { .. })
    }
}

/// Windows の `ERROR_SHARING_VIOLATION`（対象が共有を許可しない方法で既に
/// 開かれているため、要求した共有モードでは開けない）の Win32 エラーコード
/// です。
///
/// 新規に `windows` クレートへの依存を増やさず、`io::Error::raw_os_error()`
/// との比較にだけ使う定数値です（禁止事項: 外部クレートの追加をしない）。
pub const ERROR_SHARING_VIOLATION: i32 = 32;

/// Windows の `ERROR_ACCESS_DENIED`（要求したアクセス方法でファイル・
/// フォルダを開けない）の Win32 エラーコードです（`PRIV-002`、P11-1）。
///
/// [`ERROR_SHARING_VIOLATION`] と同じ理由で、`windows` クレートへは依存せず
/// 定数値だけを持ちます。
pub const ERROR_ACCESS_DENIED: i32 = 5;

/// `error` が共有違反（[`ERROR_SHARING_VIOLATION`]）かどうかを判定します
/// （`LOG-027`）。
#[must_use]
pub fn is_sharing_violation(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
}

/// `error` がアクセス拒否（[`ERROR_ACCESS_DENIED`]）かどうかを判定します
/// （`PRIV-002`、P11-1）。
#[must_use]
pub fn is_access_denied(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_ACCESS_DENIED)
}

/// `open_read_only_shared` の失敗を、共有違反・アクセス拒否・それ以外に分類して
/// [`ReadFileError`] へ変換します（`open_and_snapshot`・
/// `read_file_bytes_with_budget` が共有する分類ロジック）。
fn classify_open_error(error: io::Error, path: &Path) -> ReadFileError {
    let reason = format!("{}（{error}）", path.display());
    if is_sharing_violation(&error) {
        ReadFileError::SharingViolation { reason }
    } else if is_access_denied(&error) {
        ReadFileError::AccessDenied { reason }
    } else {
        ReadFileError::Io { reason }
    }
}

/// ファイルを読み取り専用・共有可（他プロセスの書き込みを妨げない）で開きます。
///
/// Windows では `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE` を明示
/// 指定します（`ENV-010`: 同じ端末で稼働する他の業務ソフトウェアが書き込み中の
/// ログを開くことを標準ケースとするため、他プロセスの追記や削除操作を
/// ブロックしない）。
///
/// `pub(crate)` なのは `crate::snapshot`（[`verify_snapshot`]・[`open_and_snapshot`]）
/// が同じ開き方を再利用するためです（開き方を二か所で実装し直さない）。
pub(crate) fn open_read_only_shared(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        // Win32 の FILE_SHARE_* 定数値。この呼び出しのためだけに `windows` クレートへ
        // 依存する必要はないため、値をそのまま定数化する（禁止事項: 新規外部
        // クレートの追加をしない）。
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }

    options.open(path)
}

/// ファイルを読み込みます。グローバル予算
/// （[`hakutaku_memory_accounting::global_budget`]）へ予約します。
///
/// テストでグローバル予算を汚さずに予約拒否経路を確認したい場合は
/// [`read_file_with_budget`] を使ってください。
pub fn read_file(path: &Path) -> Result<LoadedFile, ReadFileError> {
    read_file_with_budget(path, hakutaku_memory_accounting::global_budget())
}

/// [`read_file`] の内部実装です。予算を引数として受け取るため、
/// グローバル予算に依存しない決定的な単体テストが書けます（予約拒否経路の
/// テスト用の切り出し）。
///
/// UTF-8 固定・置換文字許容で即座に文字列化します（P04 の割り切り）。文字コード
/// 判定（P05）を行いたい場合は [`read_file_bytes_with_budget`] を使ってくだ
/// さい。
pub fn read_file_with_budget(
    path: &Path,
    budget: &hakutaku_memory_accounting::MemoryBudget,
) -> Result<LoadedFile, ReadFileError> {
    let loaded = read_file_bytes_with_budget(path, budget)?;
    let text = String::from_utf8_lossy(&loaded.bytes).into_owned();
    let lines = split_lines(&text);

    Ok(LoadedFile {
        file_size_bytes: loaded.file_size_bytes,
        reserved_bytes: loaded.reserved_bytes,
        lines,
    })
}

/// ファイルをデコードせずに生バイト列で読み込みます（P05）。グローバル予算
/// （[`hakutaku_memory_accounting::global_budget`]）へ予約します。
///
/// テストでグローバル予算を汚さずに予約拒否経路を確認したい場合は
/// [`read_file_bytes_with_budget`] を使ってください。
pub fn read_file_bytes(path: &Path) -> Result<LoadedBytes, ReadFileError> {
    read_file_bytes_with_budget(path, hakutaku_memory_accounting::global_budget())
}

/// [`read_file_bytes`] の内部実装です。予算を引数として受け取るため、
/// グローバル予算に依存しない決定的な単体テストが書けます。
///
/// ファイルを開く・サイズ取得・`PERF-010` の予約・読み込みという共通の処理は
/// ここに一本化されており、[`read_file_with_budget`] はこの関数を呼んでから
/// UTF-8 固定でデコードするだけの薄い実装です（二重実装を避ける）。
pub fn read_file_bytes_with_budget(
    path: &Path,
    budget: &hakutaku_memory_accounting::MemoryBudget,
) -> Result<LoadedBytes, ReadFileError> {
    let mut file = open_read_only_shared(path).map_err(|error| classify_open_error(error, path))?;

    let file_size_bytes = file
        .metadata()
        .map_err(|error| ReadFileError::Io {
            reason: format!(
                "ファイルサイズを取得できません: {}（{error}）",
                path.display()
            ),
        })?
        .len();

    // PERF-010: 読み込みバッファの確保「前」に予約する。拒否されたら読み込みを
    // 行わずエラーを返す。
    let reserve_amount = usize::try_from(file_size_bytes).unwrap_or(usize::MAX);
    let token = budget
        .reserve(reserve_amount)
        .map_err(ReadFileError::ReservationRejected)?;

    // 予約が通った量だけ、あらかじめ容量を確保しておく（読み込み中の再確保を
    // 避けるための最適化。実際の確保量は buffer.capacity() で振り替える）。
    let mut buffer = Vec::with_capacity(reserve_amount);
    file.read_to_end(&mut buffer)
        .map_err(|error| ReadFileError::Io {
            reason: format!("ファイルを読み込めません: {}（{error}）", path.display()),
        })?;

    // 実確保（バッファの容量）を予約から実確保へ振り替える（ADR-0003）。
    // 読み込み中にファイルサイズが変化した場合（他プロセスが同時に書き込み中
    // など、ENV-010 により通常のケース）、実際の確保量が予約量と食い違うことが
    // あるため、トークンの残量を超えない範囲で振り替える。振り替えられなかった
    // 差分は、トークンの Drop で自動的に解放される（過小な予約分の対処は
    // アロケータ計装（allocated_bytes）側の会計に委ねる。予算超過そのものの
    // 防止は P04 の対象外の再確保リトライではなく、次回読み込み時の予約判定で
    // 反映される）。
    let actual_bytes = buffer.capacity();
    let reserved_bytes = actual_bytes.min(token.remaining_bytes());
    let _ = token.mark_allocated(reserved_bytes);

    Ok(LoadedBytes {
        file_size_bytes,
        reserved_bytes,
        bytes: buffer,
    })
}

/// 生バイト列の1行分の範囲です（[`split_raw_lines`]。P08-5）。
///
/// [`RawLine`] のバイト版です。`content_start`・`content_end` は `\r\n`／`\n` の
/// 区切り文字を含まない本文の範囲（`bytes` 内の相対バイト位置）、`full_end` は
/// 区切り文字を含めた次行の開始位置です（区切り文字がない最終未確定行では
/// `content_end == full_end == bytes.len()`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawLineSpan {
    pub content_start: usize,
    pub content_end: usize,
    pub full_end: usize,
    /// 改行で確定した行かどうか（`LOG-026`。[`RawLine::confirmed`] と同じ意味）。
    pub confirmed: bool,
}

impl RawLineSpan {
    /// 区切り文字を含まない本文の範囲（`bytes[content_start..content_end]`）。
    #[must_use]
    pub fn content_len(&self) -> usize {
        self.content_end - self.content_start
    }

    /// この範囲の行本文を、`String` へ複製せず `text` から借用して返します。
    ///
    /// `text` には、この範囲を求めるときに [`split_line_spans_into`] へ渡した
    /// 文字列そのものを渡します（別の文字列に対して使うと範囲が意味を持ち
    /// ません）。添字操作がパニックしない理由は
    /// [`split_line_spans_into`] の doc コメント「文字境界」を参照してください。
    #[must_use]
    pub fn content_str<'a>(&self, text: &'a str) -> &'a str {
        &text[self.content_start..self.content_end]
    }
}

/// デコード済みの文字列を行分割し、各行の範囲を `out` へ追加します
/// （[`split_lines`] の借用版）。
///
/// 行分割規則は [`split_lines`] と完全に同一です（どちらも
/// [`split_raw_lines_into`] へ委譲します）。違いは、行本文を `String` へ
/// 複製せず、`text` 内のバイト範囲として返す点だけです。各行の本文は
/// [`RawLineSpan::content_str`] で借用します。1行ごとの `String` 確保を避けたい
/// ホットパス（登録時のストリーミング解析）はこちらを使います。
///
/// `out` は空にせず追加するだけです（[`split_raw_lines_into`] と同じ）。
///
/// # 文字境界（添字がパニックしない理由）
///
/// 区切りに使う `\n`（`0x0A`）と `\r`（`0x0D`）は ASCII であり、UTF-8 では
/// ASCII バイトがマルチバイト文字の構成バイトとして現れません。`text` は
/// 妥当な UTF-8 である（`&str` の不変条件）ため、求まる区切り位置は必ず文字
/// 境界に一致します。したがって [`RawLineSpan::content_str`] の添字操作は
/// 常に成功します。
pub fn split_line_spans_into(text: &str, out: &mut Vec<RawLineSpan>) {
    split_raw_lines_into(text.as_bytes(), out);
}

/// `\r\n` と `\n` の両方に対応して、**デコード前の生バイト列**を行分割します
/// （[`split_lines`] のバイト版。P08-5「索引 + オンデマンド読み出し」）。
///
/// # 安全性の前提（マルチバイト文字の途中を割らない）
///
/// この関数は `\n`（`0x0A`）バイトだけを区切りとして使います。本リポジトリが
/// 対応する文字コード（UTF-8、および `crates/format-detection` が対応する
/// Windows コードページ群、CP932 を含む）はいずれも、`0x0A`・`0x0D`
/// （`\r`）が**マルチバイト文字の構成バイト（先頭バイト・トレイルバイトの
/// いずれ）としては現れない**という性質を持ちます（UTF-8 は ASCII 範囲の
/// バイトを他の用途に使わないことが仕様で保証されており、CP932 等の
/// トレイルバイト範囲も `0x0A`・`0x0D` を含みません）。したがって、生バイト列に
/// 対して `\n`／`\r\n` で行分割しても、各行の内容（`bytes[content_start..
/// content_end]`）は常にそのままデコードできる完全な文字列です（先頭・末尾が
/// マルチバイト文字の途中で切れることはありません）。この前提は
/// `crates/core-services::loader` の登録時ストリーミング解析（本文を保持せず、
/// チャンクごとに一時デコードして解析する設計）が、生バイトオフセットを
/// 直接デコード境界として使うための基盤です。
#[must_use]
pub fn split_raw_lines(bytes: &[u8]) -> Vec<RawLineSpan> {
    let mut spans = Vec::new();
    split_raw_lines_into(bytes, &mut spans);
    spans
}

/// [`split_raw_lines`] の結果を、呼び出し側が用意した `out` へ追加します。
///
/// 行分割規則そのものの唯一の実装であり、[`split_raw_lines`] と
/// [`split_lines`] はどちらもこの関数へ委譲します（規則を複数箇所で実装し直すと、
/// チャンク境界の分割点が食い違って `crates/core-services::loader` の
/// 1対1対応が壊れるため）。
///
/// `out` は空にせず追加するだけなので、呼び出し側は1つの `Vec` を確保したまま
/// `clear()` して繰り返し使えます。チャンクごとに読み出す
/// `crates/core-services::loader::DecodeCursor` は、この形でチャンク単位の
/// `Vec` 再確保を避けます。
pub fn split_raw_lines_into(bytes: &[u8], out: &mut Vec<RawLineSpan>) {
    let mut start = 0usize;

    for index in 0..bytes.len() {
        if bytes[index] != b'\n' {
            continue;
        }
        let content_end = if index > start && bytes[index - 1] == b'\r' {
            index - 1
        } else {
            index
        };
        out.push(RawLineSpan {
            content_start: start,
            content_end,
            full_end: index + 1,
            confirmed: true,
        });
        start = index + 1;
    }

    if start < bytes.len() {
        out.push(RawLineSpan {
            content_start: start,
            content_end: bytes.len(),
            full_end: bytes.len(),
            confirmed: false,
        });
    }
}

/// `\r\n` と `\n` の両方に対応して行分割します。1起点の行番号を付けます。
///
/// ファイルが最終行の直後に改行を持つ場合（多くのテキストファイルの慣例）、
/// その改行の後ろに空行を追加しません。`\r` 単独（旧 Mac 形式）は区切りとして
/// 扱いません（対象外。`\r\n` の `\r` は `\n` の直前にある場合だけ取り除きます）。
///
/// `pub` にしているのは、P05 の文字コード判定後の文字列（`crates/format-detection`
/// がデコードした結果）にも同じ行分割規則を適用できるようにするためです
/// （`crates/core-services` が呼び出します。行分割規則を二か所で実装し直さない）。
///
/// # 未確定行（`LOG-026`）
///
/// `text` の末尾が改行で終わらない場合（＝渡された範囲の末尾が行の途中で
/// 切れている場合）、その断片を最後の [`RawLine`] として保持しつつ
/// `confirmed: false` を付けます（`RawLine::confirmed` の doc コメント参照）。
/// `text` に何を渡すかは呼び出し側の責務で、`crates/core-services` は
/// `snapshot_end` までしか読まないため、この判定はそのまま「書き込み時点の
/// 末尾が未確定か」の判定になります。
#[must_use]
pub fn split_lines(text: &str) -> Vec<RawLine> {
    // 行分割規則そのものは split_raw_lines_into が唯一の実装であり、ここでは
    // その範囲を所有文字列へ写し取るだけにする（規則を二重に実装すると、
    // 借用版 split_line_spans_into との分割点が食い違う恐れがあるため）。
    let mut spans = Vec::new();
    split_line_spans_into(text, &mut spans);

    // 行番号は1起点（RawLine::line_number の規約）。
    // 末尾断片（改行で終わらない行）は span.confirmed が false になる。
    // LOG-026: 破棄せず未確定行として保持する。
    (1_u64..)
        .zip(spans.iter())
        .map(|(line_number, span)| RawLine {
            line_number,
            text: span.content_str(text).to_string(),
            confirmed: span.confirmed,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn data_source_boundary_is_read_only() {
        assert_eq!(access_mode(), AccessMode::ReadOnly);
        assert_eq!(RESPONSIBILITY, "データソース");
    }

    // --- split_lines（純粋関数）の単体テスト ---

    #[test]
    fn split_lines_handles_lf_only() {
        let lines = split_lines("a\nb\nc");
        assert_eq!(
            lines,
            vec![
                RawLine {
                    line_number: 1,
                    text: "a".to_string(),
                    confirmed: true
                },
                RawLine {
                    line_number: 2,
                    text: "b".to_string(),
                    confirmed: true
                },
                // 末尾に改行がないため、最終行は未確定行になる（LOG-026）。
                RawLine {
                    line_number: 3,
                    text: "c".to_string(),
                    confirmed: false
                },
            ]
        );
    }

    #[test]
    fn split_lines_handles_crlf() {
        let lines = split_lines("a\r\nb\r\nc");
        assert_eq!(
            lines,
            vec![
                RawLine {
                    line_number: 1,
                    text: "a".to_string(),
                    confirmed: true
                },
                RawLine {
                    line_number: 2,
                    text: "b".to_string(),
                    confirmed: true
                },
                RawLine {
                    line_number: 3,
                    text: "c".to_string(),
                    confirmed: false
                },
            ]
        );
    }

    #[test]
    fn split_lines_handles_mixed_crlf_and_lf() {
        let lines = split_lines("a\r\nb\nc");
        assert_eq!(
            lines,
            vec![
                RawLine {
                    line_number: 1,
                    text: "a".to_string(),
                    confirmed: true
                },
                RawLine {
                    line_number: 2,
                    text: "b".to_string(),
                    confirmed: true
                },
                RawLine {
                    line_number: 3,
                    text: "c".to_string(),
                    confirmed: false
                },
            ]
        );
    }

    #[test]
    fn split_lines_does_not_add_phantom_line_for_trailing_newline() {
        let lines = split_lines("a\nb\n");
        assert_eq!(
            lines,
            vec![
                RawLine {
                    line_number: 1,
                    text: "a".to_string(),
                    confirmed: true
                },
                RawLine {
                    line_number: 2,
                    text: "b".to_string(),
                    confirmed: true
                },
            ]
        );
    }

    #[test]
    fn split_lines_keeps_empty_lines() {
        let lines = split_lines("a\n\nb");
        assert_eq!(
            lines,
            vec![
                RawLine {
                    line_number: 1,
                    text: "a".to_string(),
                    confirmed: true
                },
                RawLine {
                    line_number: 2,
                    text: String::new(),
                    confirmed: true
                },
                RawLine {
                    line_number: 3,
                    text: "b".to_string(),
                    confirmed: false
                },
            ]
        );
    }

    #[test]
    fn split_lines_of_empty_text_yields_no_lines() {
        assert_eq!(split_lines(""), Vec::new());
    }

    // 受け入れ条件（LOG-026）: 末尾が改行で終わらない場合、最終行だけが
    // 未確定行としてマークされ、断片が破棄されず保持される。
    #[test]
    fn split_lines_marks_only_trailing_fragment_without_newline_as_unconfirmed() {
        let lines = split_lines("complete line\nunconfirmed tail");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].confirmed, "改行で終わる行は確定行のはず");
        assert!(
            !lines[1].confirmed,
            "改行で終わらない末尾断片は未確定行になるはず"
        );
        assert_eq!(
            lines[1].text, "unconfirmed tail",
            "断片は破棄されず保持されるはず"
        );
    }

    // 受け入れ条件（LOG-026）: 末尾が改行で終わる場合、全行が確定行になる。
    #[test]
    fn split_lines_marks_all_lines_confirmed_when_trailing_newline_present() {
        let lines = split_lines("a\nb\n");
        assert!(
            lines.iter().all(|line| line.confirmed),
            "末尾に改行があれば未確定行は生じないはず"
        );
    }

    // --- split_raw_lines（純粋関数、P08-5）の単体テスト ---
    // split_lines と同じ受け入れ条件を、生バイト列に対して確認する。

    #[test]
    fn split_raw_lines_handles_lf_only() {
        let spans = split_raw_lines(b"a\nb\nc");
        assert_eq!(spans.len(), 3);
        assert_eq!((spans[0].content_start, spans[0].content_end), (0, 1));
        assert!(spans[0].confirmed);
        assert_eq!((spans[1].content_start, spans[1].content_end), (2, 3));
        assert!(spans[1].confirmed);
        // 末尾に改行がないため、最終行は未確定行になる（LOG-026）。
        assert_eq!((spans[2].content_start, spans[2].content_end), (4, 5));
        assert!(!spans[2].confirmed);
    }

    #[test]
    fn split_raw_lines_handles_crlf() {
        let spans = split_raw_lines(b"a\r\nb\r\nc");
        assert_eq!(spans.len(), 3);
        // "a\r\n" -> content は "a" (index 0..1)、区切りを除く。
        assert_eq!((spans[0].content_start, spans[0].content_end), (0, 1));
        assert_eq!(spans[0].full_end, 3);
        assert_eq!((spans[1].content_start, spans[1].content_end), (3, 4));
        assert_eq!(spans[1].full_end, 6);
        assert_eq!((spans[2].content_start, spans[2].content_end), (6, 7));
        assert!(!spans[2].confirmed);
    }

    #[test]
    fn split_raw_lines_does_not_add_phantom_line_for_trailing_newline() {
        let spans = split_raw_lines(b"a\nb\n");
        assert_eq!(spans.len(), 2);
        assert!(spans.iter().all(|span| span.confirmed));
    }

    #[test]
    fn split_raw_lines_of_empty_bytes_yields_no_lines() {
        assert!(split_raw_lines(b"").is_empty());
    }

    // 受け入れ条件: split_raw_lines と split_lines（デコード後の文字列版）が、
    // 同じ入力（バイト列とそれを UTF-8 として解釈した文字列）に対して同じ件数・
    // 同じ confirmed 判定を返す（登録時ストリーミング解析が、生バイトの分割と
    // デコード後の分割を1対1で対応付けられることの裏付け）。
    #[test]
    fn split_raw_lines_matches_split_lines_line_count_and_confirmed_flags() {
        let text = "先頭行\r\n2行目\n3行目（未確定）";
        let raw_spans = split_raw_lines(text.as_bytes());
        let decoded_lines = split_lines(text);

        assert_eq!(raw_spans.len(), decoded_lines.len());
        for (span, line) in raw_spans.iter().zip(decoded_lines.iter()) {
            assert_eq!(span.confirmed, line.confirmed);
        }
    }

    // --- read_file_with_budget の単体テスト（実ファイルシステムを使う） ---
    // それぞれ独立した一意な一時ファイルを使い、テスト同士が干渉しない。

    struct TempFile {
        path: std::path::PathBuf,
    }

    impl TempFile {
        fn create(label: &str, contents: &[u8]) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let count = COUNTER.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "hakutaku-data-source-test-{label}-{}-{count}-{nanos}.log",
                std::process::id()
            ));
            std::fs::write(&path, contents).expect("テスト用ファイルを作成できません");
            TempFile { path }
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    // 受け入れ条件: 読み込みバッファの確保が P02 の予約を通り、会計に計上される
    // （PERF-008、PERF-010）。
    #[test]
    fn read_file_reserves_and_marks_allocated_on_success() {
        let contents = b"2026/07/28 15:12:23.456 line one\nline two\n";
        let file = TempFile::create("reserve-success", contents);
        let budget = hakutaku_memory_accounting::MemoryBudget::new(1024 * 1024);

        let loaded = read_file_with_budget(&file.path, &budget).expect("読み込みは成功するはず");

        assert_eq!(loaded.file_size_bytes, contents.len() as u64);
        assert_eq!(loaded.lines.len(), 2);
        assert_eq!(loaded.lines[0].line_number, 1);
        assert_eq!(loaded.lines[0].text, "2026/07/28 15:12:23.456 line one");
        assert_eq!(loaded.lines[1].line_number, 2);
        assert_eq!(loaded.lines[1].text, "line two");

        // mark_allocated 済みなので、予約の未消費残量（outstanding）はゼロに戻る。
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }

    // 受け入れ条件: 予約の拒否（予算を小さくした MemoryBudget での読み込み
    // 失敗経路）。グローバル予算を汚さずテストできる。
    #[test]
    fn read_file_returns_reservation_rejected_when_budget_too_small() {
        let contents = b"0123456789";
        let file = TempFile::create("reserve-rejected", contents);
        // ファイルサイズ（10バイト）未満の予算にして、確実に拒否させる。
        let budget = hakutaku_memory_accounting::MemoryBudget::new(5);

        let error = read_file_with_budget(&file.path, &budget)
            .expect_err("予算不足のため読み込みは失敗するはず");

        match error {
            ReadFileError::ReservationRejected(rejected) => {
                assert_eq!(rejected.requested_bytes, contents.len());
                assert_eq!(rejected.budget_bytes, 5);
            }
            other => panic!("予約拒否エラーが返るはず: {other:?}"),
        }
        // 拒否されたので予約は残らない。
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }

    // 受け入れ条件: 不正なバイト列は置換文字で許容する（P04 の割り切り）。
    #[test]
    fn read_file_replaces_invalid_utf8_bytes() {
        // 0xFF は単独では不正な UTF-8 バイト列。
        let contents: Vec<u8> = vec![b'a', 0xFF, b'\n', b'b'];
        let file = TempFile::create("invalid-utf8", &contents);
        let budget = hakutaku_memory_accounting::MemoryBudget::new(1024);

        let loaded = read_file_with_budget(&file.path, &budget).expect("読み込みは成功するはず");

        assert_eq!(loaded.lines.len(), 2);
        assert!(
            loaded.lines[0].text.contains('\u{FFFD}'),
            "不正なバイト列は置換文字になるはず: {:?}",
            loaded.lines[0].text
        );
        assert_eq!(loaded.lines[1].text, "b");
    }

    // 受け入れ条件: 存在しないファイルは I/O エラーとして返す（予約は行わない）。
    #[test]
    fn read_file_returns_io_error_for_missing_file() {
        let missing =
            std::env::temp_dir().join("hakutaku-data-source-test-does-not-exist-3f9c2a.log");
        let budget = hakutaku_memory_accounting::MemoryBudget::new(1024);

        let error =
            read_file_with_budget(&missing, &budget).expect_err("存在しないファイルは失敗するはず");
        assert!(matches!(error, ReadFileError::Io { .. }));
        assert_eq!(
            budget.outstanding_reserved_bytes(),
            0,
            "開く前に失敗した場合は予約していないはず"
        );
    }

    // --- read_file_bytes_with_budget の単体テスト（P05: デコード前の生バイト） ---

    // 受け入れ条件: デコードせず生バイト列のまま返す（不正な UTF-8 バイト列や
    // CP932 のバイト列でも変換・置換せずそのまま保持する）。
    #[test]
    fn read_file_bytes_returns_undecoded_bytes_as_is() {
        // 0x93 0xFA は CP932 の「日」だが、UTF-8 としては不正なバイト列。
        // read_file_bytes はデコードしないため、そのまま保持されるはず。
        let contents: Vec<u8> = vec![b'a', 0x93, 0xFA, b'\n', b'b'];
        let file = TempFile::create("bytes-undecoded", &contents);
        let budget = hakutaku_memory_accounting::MemoryBudget::new(1024);

        let loaded =
            read_file_bytes_with_budget(&file.path, &budget).expect("読み込みは成功するはず");

        assert_eq!(
            loaded.bytes, contents,
            "デコードせず生バイト列のまま返すはず"
        );
        assert_eq!(loaded.file_size_bytes, contents.len() as u64);
    }

    // 受け入れ条件: read_file_bytes も PERF-010 の予約・実確保への振り替えを行う
    // （read_file_with_budget と共通の実装を経由するため）。
    #[test]
    fn read_file_bytes_reserves_and_marks_allocated_on_success() {
        let contents = b"2026/07/28 15:12:23.456 line one\n";
        let file = TempFile::create("bytes-reserve-success", contents);
        let budget = hakutaku_memory_accounting::MemoryBudget::new(1024 * 1024);

        let loaded =
            read_file_bytes_with_budget(&file.path, &budget).expect("読み込みは成功するはず");

        assert_eq!(loaded.file_size_bytes, contents.len() as u64);
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }

    // 受け入れ条件: read_file_bytes も予約拒否経路を持つ（read_file_with_budget
    // と共通の実装を経由するため）。
    #[test]
    fn read_file_bytes_returns_reservation_rejected_when_budget_too_small() {
        let contents = b"0123456789";
        let file = TempFile::create("bytes-reserve-rejected", contents);
        let budget = hakutaku_memory_accounting::MemoryBudget::new(5);

        let error = read_file_bytes_with_budget(&file.path, &budget)
            .expect_err("予算不足のため読み込みは失敗するはず");
        assert!(matches!(error, ReadFileError::ReservationRejected(_)));
    }

    // --- 共有違反の区別（LOG-027、P06-5） ---

    // 受け入れ条件: FileShare.None 相当（share_mode(0)）で開かれたファイルへの
    // 読み込みは、他の I/O エラーとは別の SharingViolation として区別される。
    // ロックの強制解除やコピーによる迂回は行わず、対象（パス）を含む理由を
    // 返すだけであることも確認する。
    #[test]
    fn read_file_bytes_returns_sharing_violation_when_locked_exclusively() {
        use std::os::windows::fs::OpenOptionsExt;

        let file = TempFile::create("sharing-violation", b"locked content");
        // 共有を一切許可しない方法で開く（他の業務ソフトウェアが FileShare.None
        // でログを開いている状況の再現）。このハンドルを保持したままにする。
        let _locker = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file.path)
            .expect("排他的に開けるはず（LOG-027 再現用の前提）");

        let budget = hakutaku_memory_accounting::MemoryBudget::new(1024 * 1024);
        let error = read_file_bytes_with_budget(&file.path, &budget)
            .expect_err("共有違反のため読み込みは失敗するはず");

        assert!(
            error.is_sharing_violation(),
            "SharingViolation として区別されるはず: {error:?}"
        );
        assert!(
            !matches!(error, ReadFileError::Io { .. }),
            "他の I/O エラー（Io）とは区別されるはず"
        );
        if let ReadFileError::SharingViolation { reason } = &error {
            assert!(
                reason.contains(&file.path.display().to_string()),
                "対象（パス）を理由に含むはず: {reason}"
            );
        }
        // 予約は行われていない（開く前に失敗したため）。
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }

    // 通常の（共有違反ではない）I/O エラーは引き続き Io のまま区別されない
    // （is_sharing_violation が false を返す）ことを確認する。
    #[test]
    fn is_sharing_violation_is_false_for_ordinary_errors() {
        let missing =
            std::env::temp_dir().join("hakutaku-data-source-test-does-not-exist-sv-check.log");
        let budget = hakutaku_memory_accounting::MemoryBudget::new(1024);

        let error = read_file_bytes_with_budget(&missing, &budget)
            .expect_err("存在しないファイルは失敗するはず");
        assert!(!error.is_sharing_violation());
        assert!(matches!(error, ReadFileError::Io { .. }));
    }

    // --- アクセス拒否の区別（PRIV-002、P11-1） ---

    // 受け入れ条件: classify_open_error は raw_os_error() が ERROR_ACCESS_DENIED
    // （5）の io::Error を AccessDenied として分類し、共有違反・通常の I/O
    // エラーとは区別する。実際に ACL でアクセス拒否を再現するのはテスト環境の
    // 権限に依存し不安定なため、io::Error を直接構築した決定的な単体テストで
    // 分類ロジックを検証する（手動確認手順は本フェーズの報告に記録する）。
    #[test]
    fn classify_open_error_detects_access_denied_by_os_error_code() {
        let os_error = io::Error::from_raw_os_error(ERROR_ACCESS_DENIED);
        let path = Path::new(r"C:\example\locked-by-acl.log");

        let error = classify_open_error(os_error, path);

        assert!(
            error.is_access_denied(),
            "AccessDenied として分類されるはず: {error:?}"
        );
        assert!(!error.is_sharing_violation());
        assert!(!matches!(error, ReadFileError::Io { .. }));
        if let ReadFileError::AccessDenied { reason } = &error {
            assert!(
                reason.contains(&path.display().to_string()),
                "対象（パス）を理由に含むはず: {reason}"
            );
        } else {
            panic!("AccessDenied を期待しましたが {error:?} でした");
        }
    }

    #[test]
    fn access_denied_display_mentions_privilege_002_and_elevation() {
        let error = ReadFileError::AccessDenied {
            reason: "C:\\example\\a.log（アクセスが拒否されました。）".to_string(),
        };
        let message = error.to_string();
        assert!(message.contains("PRIV-002"));
        assert!(message.contains("管理者権限"));
    }

    #[test]
    fn is_access_denied_is_false_for_sharing_violation_and_ordinary_io_errors() {
        let sharing = ReadFileError::SharingViolation {
            reason: "locked".to_string(),
        };
        assert!(!sharing.is_access_denied());

        let io_error = ReadFileError::Io {
            reason: "other".to_string(),
        };
        assert!(!io_error.is_access_denied());
    }

    #[test]
    fn is_access_denied_helper_matches_only_the_access_denied_os_error_code() {
        assert!(is_access_denied(&io::Error::from_raw_os_error(
            ERROR_ACCESS_DENIED
        )));
        assert!(!is_access_denied(&io::Error::from_raw_os_error(
            ERROR_SHARING_VIOLATION
        )));
    }
}
