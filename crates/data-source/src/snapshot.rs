//! ファイルスナップショット（ファイル識別子・サイズ・最終更新時刻）と、未読み
//! 込み範囲を読む前の整合性再確認です（P06-1、`tasks/phase-06-large-file-loading.md`
//! 作業項目5、`LOG-023`）。
//!
//! # `snapshot_end`（ADR-0007）
//!
//! ファイルを開いた時点で観測した長さを [`FileSnapshot::snapshot_end`] として
//! 固定します。[`read_snapshotted_bytes`] はこれを上限に読み込み、判定した瞬間
//! より先に追記された分はその回では読みません。判定後の追記分は「更新未反映」
//! として扱い、次の明示的な再読み込みで反映します（`LOG-010`、`LOG-028`）。
//!
//! 「原子的」の範囲は、アプリ内の合計サイズの判定と予約
//! （`crates/core-services::budget::SourceBudget`）に限られます。ファイル側の
//! サイズ取得と後続の読み込みそのものをファイルシステム上のトランザクションに
//! することはできません（排他ロックを取らない限り、観測した瞬間にサイズは
//! 変わり得ます）。`snapshot_end` を固定することで、判定した量と実際に読む量を
//! 一致させます。
//!
//! # ファイル識別子
//!
//! Windows のボリューム連番（`dwVolumeSerialNumber`）とファイルインデックス
//! （`nFileIndexHigh`・`nFileIndexLow` を結合した64ビット値）の組を
//! [`FileIdentity`] として使います。同一パスへの再オープンでこの組が変われば
//! 「別ファイルへの置換」と判定します（[`SnapshotVerdict::Replaced`]）。
//!
//! # 再確認の2層構成
//!
//! 整合性の再確認には、検知できる事象と費用が異なる2つの手段があります。
//!
//! 1. [`verify_snapshot`]（**パス再オープン**）。縮小・置換・削除のすべてを
//!    検知できますが、呼び出しのたびにファイルを開き直します
//! 2. [`verify_snapshot_by_handle`]（**既存ハンドルへの再問い合わせ**）。
//!    追加のファイルオープンを伴いませんが、検知できるのはサイズの変化
//!    （縮小・追記）だけです
//!
//! 2 が置換・削除を検知できないのは Windows の仕様によります。
//! `FILE_SHARE_DELETE` で開いたハンドルは、ファイルが削除されても、また同じ
//! パスへ別のファイルが作られても有効なまま「開いた時点のファイル実体」を
//! 指し続けます。したがって、削除・置換は「パスを開き直したときに何が見えるか」
//! でしか観測できません（[`verify_snapshot`] の doc コメント参照）。
//!
//! チャンク読み込み（[`crate::stream_snapshotted_bytes_chunked`]）は、この
//! 費用差を踏まえて2つを使い分けます。使い分けの方針と、それによって生じる
//! 検知の遅延は `crate::chunk` のモジュール doc コメントを正本とします。
//!
//! # 既知の限界: 同一サイズでの上書きは検知できない
//!
//! [`compare_snapshots`] が判定に使う手がかりは、[`FileIdentity`] と
//! `snapshot_end`（開いた時点で観測した長さ）の2つだけです。
//! [`FileSnapshot::last_write_time`] は診断・表示用の参考情報として保持する
//! だけで、判定には使いません。
//!
//! したがって、**同じファイル実体（識別子が変わらない）へ、同じバイト数の
//! 内容が上書きされた場合は [`SnapshotVerdict::Unchanged`] になります。**
//! 内容が入れ替わっていても「変化なし」と判定されるため、`LOG-023` の変更
//! 検知は働かず、表示は上書き前に読み込んだ内容のまま残ります。
//!
//! 最終更新時刻を判定へ加えていないのは、更新時刻が書き込み側の都合で元の値
//! のまま保存されることがあり（上書きしても変わらない）、逆に内容が変わらな
//! くても更新され得るため、変更の手がかりとして信頼できないからです。内容
//! そのものの照合（ハッシュ等）は、再確認のたびにファイル全量を読み直すこと
//! になり、[`verify_snapshot`] の doc コメントが述べる再オープンの費用より
//! さらに大きな費用を、追記の追従（`LOG-010`）のたびに払うことになります。
//!
//! この限界は、追記されていくログを読むという想定用途では実害が小さいと判断
//! して受け入れています。同一サイズでの上書きが疑われる場合は、明示的な
//! 再読み込み（`LOG-028`）で読み直してください。

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{is_sharing_violation, open_read_only_shared, LoadedBytes, ReadFileError};

// --- 整合性再確認の軽量カウンタ（再オープン回数の計測用） ---

/// パス再オープンによる整合性再確認の回数（プロセス累計）。
static PATH_VERIFICATIONS: AtomicU64 = AtomicU64::new(0);
/// 既存ハンドルへの再問い合わせによる整合性再確認の回数（プロセス累計）。
static HANDLE_VERIFICATIONS: AtomicU64 = AtomicU64::new(0);

/// 整合性再確認の累計カウンタです（再オープン回数の計測用）。
///
/// 読み込み経路が再確認のためにファイルを開き直した回数を、計測ハーネス
/// （`crates/core-services/examples/scale_verify.rs`）やテストから観測する
/// ためのものです。**利用者向けの挙動には一切影響しません。** 更新は
/// `Ordering::Relaxed` で行い、カウンタのためにスレッド間の順序づけを
/// 増やしません（計測用途に厳密な同期は不要なため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SnapshotVerifyMetrics {
    /// [`verify_snapshot`] の呼び出し回数（＝再確認のためのファイルオープン
    /// 回数）。再オープン削減の主指標です。
    pub path_verifications: u64,
    /// [`verify_snapshot_by_handle`] の呼び出し回数。ファイルオープンを
    /// 伴わないため、この値が増えても再確認のためのオープン回数は増えません。
    pub handle_verifications: u64,
}

/// 整合性再確認のカウンタの現在値を返します（再オープン回数の計測用）。
#[must_use]
pub fn snapshot_verify_metrics() -> SnapshotVerifyMetrics {
    SnapshotVerifyMetrics {
        path_verifications: PATH_VERIFICATIONS.load(Ordering::Relaxed),
        handle_verifications: HANDLE_VERIFICATIONS.load(Ordering::Relaxed),
    }
}

/// 整合性再確認のカウンタを 0 に戻します（再オープン回数の計測用）。
///
/// カウンタはプロセス全体で共有されるため、計測区間の前後で差を取るか、
/// 区間の開始時にこれを呼びます。並行して読み込みを行うスレッドがある場合、
/// 0 に戻す操作と加算の順序は保証されません（計測用途のみを想定）。
pub fn reset_snapshot_verify_metrics() {
    PATH_VERIFICATIONS.store(0, Ordering::Relaxed);
    HANDLE_VERIFICATIONS.store(0, Ordering::Relaxed);
}

/// ファイルの識別子です（ボリューム連番＋ファイルインデックス）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    /// `GetFileInformationByHandle` の `dwVolumeSerialNumber`。
    pub volume_serial_number: u32,
    /// `nFileIndexHigh`・`nFileIndexLow` を結合した64ビットのファイル
    /// インデックス。
    pub file_index: u64,
}

/// ファイルを開いた時点のスナップショットです。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSnapshot {
    /// 開いた時点のファイル識別子。
    pub identity: FileIdentity,
    /// 開いた時点で観測した長さ（バイト）。これより先は読みません
    /// （`snapshot_end`。ADR-0007）。
    pub snapshot_end: u64,
    /// 開いた時点の最終更新時刻（Windows FILETIME。1601-01-01 UTC からの
    /// 100ナノ秒単位を64ビットへ結合した値）。診断・表示用の参考情報であり、
    /// [`SnapshotVerdict`] の判定には使いません（同一サイズでの上書きは
    /// 検知できない既知の限界。本モジュール doc コメント末尾を参照）。
    pub last_write_time: u64,
}

/// 整合性再確認の結果です（`LOG-023`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotVerdict {
    /// 完全に同一（識別子・サイズとも変化なし）。
    Unchanged,
    /// 同一ファイル（識別子が一致）でサイズが増加した。追記であり、
    /// `snapshot_end` は従来のまま固定して「更新未反映」として扱います
    /// （ADR-0007）。`LOG-023` の変更検知の対象にはしません。
    Appended { current_size_bytes: u64 },
    /// 同一ファイルだがサイズが縮小した（切り詰め。`LOG-023` の対象）。
    Shrunk { current_size_bytes: u64 },
    /// ファイル識別子が変化した（別ファイルへの置換。`LOG-023` の対象）。
    Replaced,
    /// ファイルを再度開けなかった（削除、またはパスが存在しない。`LOG-023`
    /// の対象）。
    Deleted,
}

/// [`verify_snapshot`] が失敗した理由です。
///
/// 削除（[`SnapshotVerdict::Deleted`]）は正常な判定結果として扱うため、ここに
/// は含まれません。共有違反などの再試行可能な経路（`LOG-027`）は P06-5 の
/// 対象であり、本クレートはその区別をせず一律 [`VerifySnapshotError::Io`] として
/// 伝えます。
#[derive(Debug)]
pub enum VerifySnapshotError {
    /// 削除以外の理由で再オープン、または情報取得に失敗した。
    Io(io::Error),
}

impl std::fmt::Display for VerifySnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifySnapshotError::Io(error) => {
                write!(f, "ファイルの整合性を再確認できません: {error}")
            }
        }
    }
}

impl std::error::Error for VerifySnapshotError {}

impl VerifySnapshotError {
    /// 共有違反（`LOG-027`）による再確認の失敗かどうかを返します。
    #[must_use]
    pub fn is_sharing_violation(&self) -> bool {
        match self {
            VerifySnapshotError::Io(error) => is_sharing_violation(error),
        }
    }
}

/// 開いたファイルハンドルからスナップショットを採取します。
pub fn capture_snapshot(file: &File) -> io::Result<FileSnapshot> {
    let (identity, size_bytes, last_write_time) = raw_file_info(file)?;
    Ok(FileSnapshot {
        identity,
        snapshot_end: size_bytes,
        last_write_time,
    })
}

/// ファイルを読み取り専用・共有可で開き、その時点のスナップショットを取ります。
///
/// 読み込みバッファの確保・実読み込みは行いません。呼び出し側
/// （`crates/core-services`）は、この関数が返す `snapshot_end` を使って上限
/// 判定（`PERF-004`〜`006`）を行った後、[`read_snapshotted_bytes`] で読み込みへ
/// 進むことを想定しています（判定を先に済ませることで、上限超過時に大きな
/// 読み込みバッファを確保せずに済みます）。
pub fn open_and_snapshot(path: &Path) -> Result<(File, FileSnapshot), ReadFileError> {
    let file =
        open_read_only_shared(path).map_err(|error| crate::classify_open_error(error, path))?;
    let snapshot = capture_snapshot(&file).map_err(|error| ReadFileError::Io {
        reason: format!(
            "ファイル情報を取得できません: {}（{error}）",
            path.display()
        ),
    })?;
    Ok((file, snapshot))
}

/// 既に開いたファイルから、`snapshot.snapshot_end` を上限にバイト列を読み込み
/// ます（ADR-0007「観測した長さを固定し、それより先を読まない」）。
///
/// `budget` への予約は [`crate::read_file_bytes_with_budget`] と同じく
/// `PERF-010` に従い、確保**前**に行います。
pub fn read_snapshotted_bytes(
    file: File,
    snapshot: &FileSnapshot,
    budget: &hakutaku_memory_accounting::MemoryBudget,
) -> Result<LoadedBytes, ReadFileError> {
    use std::io::Read;

    let snapshot_end = snapshot.snapshot_end;
    let reserve_amount = usize::try_from(snapshot_end).unwrap_or(usize::MAX);
    let token = budget
        .reserve(reserve_amount)
        .map_err(ReadFileError::ReservationRejected)?;

    let mut buffer = Vec::with_capacity(reserve_amount);
    // `Take` は `snapshot_end` バイトを超えて内部リーダーへ読み込みを発行し
    // ない。これにより、読み込み中に他プロセスが追記しても（`ENV-010` の
    // 標準ケース）、開いた時点で観測した長さより先は読まれない。
    let mut limited = file.take(snapshot_end);
    limited
        .read_to_end(&mut buffer)
        .map_err(|error| ReadFileError::Io {
            reason: format!("ファイルを読み込めません（{error}）"),
        })?;

    // 実確保（バッファの容量）を予約から実確保へ振り替える（ADR-0003）。
    let actual_bytes = buffer.capacity();
    let reserved_bytes = actual_bytes.min(token.remaining_bytes());
    let _ = token.mark_allocated(reserved_bytes);

    Ok(LoadedBytes {
        file_size_bytes: snapshot_end,
        reserved_bytes,
        bytes: buffer,
    })
}

/// `path` を読み取り専用・共有可で再度開き、`snapshot` との整合性を確認します
/// （`LOG-023`）。
///
/// 元のハンドルではなく `path` を再オープンして確認するのは、`FILE_SHARE_DELETE`
/// で開いているとファイルが削除されても元のハンドルは有効なまま読み続けられて
/// しまい、削除を検知できないためです（Windows の仕様）。パスを再オープンする
/// ことで、削除は「再オープンの失敗（`NotFound`）」として観測できます。
///
/// この再オープンこそが本関数の費用です。エンタープライズ向け
/// セキュリティソフトの環境では、ファイルオープンのたびにフィルタードライバが
/// 介入するため、呼び出し回数がそのまま実機の所要時間へ効きます。サイズの
/// 変化だけを見れば足りる場合は、オープンを伴わない
/// [`verify_snapshot_by_handle`] を使ってください。
pub fn verify_snapshot(
    path: &Path,
    snapshot: &FileSnapshot,
) -> Result<SnapshotVerdict, VerifySnapshotError> {
    PATH_VERIFICATIONS.fetch_add(1, Ordering::Relaxed);
    let file = match open_read_only_shared(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(SnapshotVerdict::Deleted);
        }
        Err(error) => return Err(VerifySnapshotError::Io(error)),
    };

    let current = capture_snapshot(&file).map_err(VerifySnapshotError::Io)?;
    Ok(compare_snapshots(snapshot, &current))
}

/// **既に開いているハンドル**から現在の状態を問い合わせ、`snapshot` との
/// 整合性を確認します（`LOG-023`）。
///
/// [`verify_snapshot`] と違い、**ファイルを開き直しません**。読み込みに使って
/// いるハンドルへ `GetFileInformationByHandle` を発行するだけなので、
/// セキュリティソフトのフィルタードライバがオープンごとに介入する環境でも
/// 費用がほとんど増えません。チャンクごとの縮小検知はこちらで行います。
///
/// # 検知できないこと
///
/// **置換と削除は検知できません。** `FILE_SHARE_DELETE` で開いたハンドルは、
/// ファイルが削除されても、同じパスへ別のファイルが作られても、開いた時点の
/// ファイル実体を指し続けるためです（Windows の仕様。本モジュール doc
/// コメントの「再確認の2層構成」を参照）。したがって本関数が返し得る判定は
/// 実質的に [`SnapshotVerdict::Unchanged`]・[`SnapshotVerdict::Appended`]・
/// [`SnapshotVerdict::Shrunk`] の3つです（`file` と `snapshot` が別のファイル
/// 由来という呼び出し側の誤りがあれば [`SnapshotVerdict::Replaced`] になり
/// ますが、これは想定外の使い方です）。置換・削除を検知する必要がある場合は
/// [`verify_snapshot`] を使ってください。
///
/// # 失敗の意味
///
/// 失敗はハンドルへの問い合わせ自体が失敗したこと（[`VerifySnapshotError::Io`]）
/// を意味します。オープンを行わないため、共有違反（`LOG-027`）はここでは
/// 起きません。
pub fn verify_snapshot_by_handle(
    file: &File,
    snapshot: &FileSnapshot,
) -> Result<SnapshotVerdict, VerifySnapshotError> {
    HANDLE_VERIFICATIONS.fetch_add(1, Ordering::Relaxed);
    let current = capture_snapshot(file).map_err(VerifySnapshotError::Io)?;
    Ok(compare_snapshots(snapshot, &current))
}

/// 2つのスナップショットを比較し、[`SnapshotVerdict`] を判定します（I/O を
/// 伴わない純粋関数）。
///
/// [`verify_snapshot`]（`path` を再オープンして現在の状態と比較する）、
/// [`verify_snapshot_by_handle`]（既存ハンドルへ問い合わせて比較する）、
/// [`reopen_for_reload`] の呼び出し側（`crates/core-services::loader::
/// reload_source`。明示的な再読み込みで既に得た新しいスナップショットと
/// 比較する）が、この同じ判定ロジックを共有します（`LOG-023`・`LOG-028`
/// の判定を二重実装しない）。どの手段で現在の状態を得たかによらず、同じ
/// 差分には同じ [`SnapshotVerdict`] が対応します。`old` が「削除」だったかどうかはこの関数の
/// 対象外です（削除は再オープンの失敗そのものとして観測されるため。
/// `verify_snapshot`・[`reopen_for_reload`] の doc コメント参照）。
#[must_use]
pub fn compare_snapshots(old: &FileSnapshot, new: &FileSnapshot) -> SnapshotVerdict {
    if new.identity != old.identity {
        return SnapshotVerdict::Replaced;
    }

    if new.snapshot_end < old.snapshot_end {
        SnapshotVerdict::Shrunk {
            current_size_bytes: new.snapshot_end,
        }
    } else if new.snapshot_end > old.snapshot_end {
        SnapshotVerdict::Appended {
            current_size_bytes: new.snapshot_end,
        }
    } else {
        SnapshotVerdict::Unchanged
    }
}

/// [`reopen_for_reload`] の失敗です（`crates/core-services::loader::reload_source`、
/// `LOG-023`・`LOG-027`・`LOG-028`）。
#[derive(Debug)]
pub enum ReopenForReloadError {
    /// ファイルを再度開けなかった（削除、またはパスが存在しない。`LOG-023`）。
    Deleted,
    /// 共有を許可しない方法で開かれていて読み取れない（`LOG-027`。再試行可能）。
    SharingViolation { reason: String },
    /// 削除・共有違反以外の理由で再オープン、または情報取得に失敗した。
    Io { reason: String },
}

impl std::fmt::Display for ReopenForReloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReopenForReloadError::Deleted => write!(f, "ファイルが見つかりません（削除済み）。"),
            ReopenForReloadError::SharingViolation { reason } => write!(
                f,
                "他のプロセスが共有を許可せずに開いているため読み取れません（LOG-027）: {reason}"
            ),
            ReopenForReloadError::Io { reason } => {
                write!(f, "ファイルを再度開けません: {reason}")
            }
        }
    }
}

impl std::error::Error for ReopenForReloadError {}

/// 明示的な再読み込み（`LOG-028`）のために `path` を再度開き、新しい
/// スナップショットを取ります。
///
/// [`open_and_snapshot`] とほぼ同じですが、削除（`NotFound`）を
/// [`ReopenForReloadError::Deleted`] として区別する点が異なります
/// （`verify_snapshot` と同じ「再オープンの失敗＝削除」という扱い。
/// `crates/core-services::loader::reload_source` は、この区別を使って
/// `LOG-023`（削除の検知）と `LOG-027`（共有違反）を作り分けます）。
pub fn reopen_for_reload(path: &Path) -> Result<(File, FileSnapshot), ReopenForReloadError> {
    let file = match open_read_only_shared(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ReopenForReloadError::Deleted);
        }
        Err(error) if is_sharing_violation(&error) => {
            return Err(ReopenForReloadError::SharingViolation {
                reason: format!("{}（{error}）", path.display()),
            });
        }
        Err(error) => {
            return Err(ReopenForReloadError::Io {
                reason: format!("{}（{error}）", path.display()),
            })
        }
    };

    let snapshot = capture_snapshot(&file).map_err(|error| ReopenForReloadError::Io {
        reason: format!(
            "ファイル情報を取得できません: {}（{error}）",
            path.display()
        ),
    })?;
    Ok((file, snapshot))
}

#[cfg(windows)]
fn raw_file_info(file: &File) -> io::Result<(FileIdentity, u64, u64)> {
    crate::win32::query_file_information(file)
}

/// Windows 以外でのビルド用の代替実装です。
///
/// 本リポジトリのビルド対象は `.cargo/config.toml` で `x86_64-pc-windows-msvc`
/// に固定されているため、この関数が実際に呼ばれることはありません。型として
/// コンパイルが通るようにするための最小限の代替値です
/// （`crates/format-detection/src/decision.rs::environment_ansi_codepage` と
/// 同じ方針）。
#[cfg(not(windows))]
fn raw_file_info(_file: &File) -> io::Result<(FileIdentity, u64, u64)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "ファイル識別子の取得は Windows 専用です",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

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
                "hakutaku-data-source-snapshot-test-{label}-{}-{count}-{nanos}.log",
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

    fn snapshot_of(path: &Path) -> FileSnapshot {
        let (file, snapshot) = open_and_snapshot(path).expect("開けるはず");
        drop(file);
        snapshot
    }

    // 受け入れ条件: 開いた時点のサイズが snapshot_end として固定される。
    #[test]
    fn open_and_snapshot_captures_size_as_snapshot_end() {
        let file = TempFile::create("capture", b"0123456789");
        let snapshot = snapshot_of(&file.path);
        assert_eq!(snapshot.snapshot_end, 10);
    }

    // 受け入れ条件: 同一ファイルを変更しなければ Unchanged になる。
    #[test]
    fn verify_snapshot_detects_unchanged_file() {
        let file = TempFile::create("unchanged", b"hello");
        let snapshot = snapshot_of(&file.path);

        let verdict = verify_snapshot(&file.path, &snapshot).expect("確認は成功するはず");
        assert_eq!(verdict, SnapshotVerdict::Unchanged);
    }

    // 受け入れ条件: 追記（サイズ増）は同一ファイルとして Appended になる
    // （LOG-010・LOG-028: 更新未反映として扱う。ADR-0007）。
    #[test]
    fn verify_snapshot_detects_append_as_same_file_with_growth() {
        let file = TempFile::create("appended", b"hello");
        let snapshot = snapshot_of(&file.path);

        {
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記用に開けるはず");
            writer.write_all(b" world").expect("追記できるはず");
        }

        let verdict = verify_snapshot(&file.path, &snapshot).expect("確認は成功するはず");
        assert_eq!(
            verdict,
            SnapshotVerdict::Appended {
                current_size_bytes: 11
            }
        );
    }

    // 受け入れ条件: 縮小（切り詰め）を Shrunk として検知する（LOG-023）。
    #[test]
    fn verify_snapshot_detects_truncation_as_shrunk() {
        let file = TempFile::create("shrunk", b"0123456789");
        let snapshot = snapshot_of(&file.path);
        assert_eq!(snapshot.snapshot_end, 10);

        {
            let writer = std::fs::OpenOptions::new()
                .write(true)
                .open(&file.path)
                .expect("書き込み用に開けるはず");
            writer.set_len(3).expect("切り詰めできるはず");
        }

        let verdict = verify_snapshot(&file.path, &snapshot).expect("確認は成功するはず");
        assert_eq!(
            verdict,
            SnapshotVerdict::Shrunk {
                current_size_bytes: 3
            }
        );
    }

    // 受け入れ条件: 削除・再作成（別ファイルへの置換）を Replaced として検知する
    // （LOG-023）。std::fs::remove_file の後に同じパスへ書き込むことで、識別子
    // （ファイルインデックス）が変わる新しいファイルを確実に作る。
    #[test]
    fn verify_snapshot_detects_replacement_as_replaced() {
        let file = TempFile::create("replaced", b"original");
        let snapshot = snapshot_of(&file.path);

        std::fs::remove_file(&file.path).expect("削除できるはず");
        std::fs::write(&file.path, b"different content").expect("再作成できるはず");

        let verdict = verify_snapshot(&file.path, &snapshot).expect("確認は成功するはず");
        assert_eq!(verdict, SnapshotVerdict::Replaced);
    }

    // 受け入れ条件: 削除（再作成なし）を Deleted として検知する（LOG-023）。
    #[test]
    fn verify_snapshot_detects_deletion_as_deleted() {
        let file = TempFile::create("deleted", b"gone soon");
        let snapshot = snapshot_of(&file.path);

        std::fs::remove_file(&file.path).expect("削除できるはず");

        let verdict = verify_snapshot(&file.path, &snapshot).expect("確認は成功するはず");
        assert_eq!(verdict, SnapshotVerdict::Deleted);
        // TempFile::drop は remove_file の失敗を無視するため、テスト終了時に
        // 既に削除済みのパスへ再度 remove_file を試みても問題ない。
    }

    // 受け入れ条件: read_snapshotted_bytes は snapshot_end を超えて読まない
    // （ADR-0007「観測した長さを固定し、それより先を読まない」）。スナップ
    // ショット取得後にファイルへ追記してから読み込み、追記分が含まれないこと
    // を確認する。
    #[test]
    fn read_snapshotted_bytes_does_not_read_past_snapshot_end() {
        let file = TempFile::create("bounded-read", b"0123456789");
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        assert_eq!(snapshot.snapshot_end, 10);

        // スナップショット取得後に追記する（判定後の追記はその回では読まない）。
        {
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記用に開けるはず");
            writer
                .write_all(b"appended-after-snapshot")
                .expect("追記できるはず");
        }

        let budget = hakutaku_memory_accounting::MemoryBudget::new(1024 * 1024);
        let loaded =
            read_snapshotted_bytes(handle, &snapshot, &budget).expect("読み込みは成功するはず");

        assert_eq!(loaded.bytes, b"0123456789");
        assert_eq!(loaded.file_size_bytes, 10);
    }

    // --- compare_snapshots（純粋関数） ---

    fn identity(n: u64) -> FileIdentity {
        FileIdentity {
            volume_serial_number: 1,
            file_index: n,
        }
    }

    fn snap(identity: FileIdentity, snapshot_end: u64) -> FileSnapshot {
        FileSnapshot {
            identity,
            snapshot_end,
            last_write_time: 0,
        }
    }

    #[test]
    fn compare_snapshots_detects_unchanged_appended_and_shrunk_for_same_identity() {
        let old = snap(identity(1), 10);

        assert_eq!(
            compare_snapshots(&old, &snap(identity(1), 10)),
            SnapshotVerdict::Unchanged
        );
        assert_eq!(
            compare_snapshots(&old, &snap(identity(1), 15)),
            SnapshotVerdict::Appended {
                current_size_bytes: 15
            }
        );
        assert_eq!(
            compare_snapshots(&old, &snap(identity(1), 3)),
            SnapshotVerdict::Shrunk {
                current_size_bytes: 3
            }
        );
    }

    #[test]
    fn compare_snapshots_detects_replacement_by_identity_change_even_if_size_unchanged() {
        let old = snap(identity(1), 10);
        let new = snap(identity(2), 10);
        assert_eq!(compare_snapshots(&old, &new), SnapshotVerdict::Replaced);
    }

    // --- reopen_for_reload（LOG-028 の再読み込みが使う再オープン経路） ---

    // 受け入れ条件: 通常の再オープンは新しいスナップショットを返す（追記後の
    // 再読み込みで使う経路）。
    #[test]
    fn reopen_for_reload_succeeds_and_observes_growth() {
        let file = TempFile::create("reopen-ok", b"hello");
        let (first_handle, first_snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        drop(first_handle);

        {
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記用に開けるはず");
            writer.write_all(b" world").expect("追記できるはず");
        }

        let (_second_handle, second_snapshot) =
            reopen_for_reload(&file.path).expect("再オープンは成功するはず");
        assert_eq!(
            compare_snapshots(&first_snapshot, &second_snapshot),
            SnapshotVerdict::Appended {
                current_size_bytes: 11
            }
        );
    }

    // 受け入れ条件: 削除済みファイルの再読み込みは Deleted として区別される
    // （LOG-023。共有違反とは別の分類）。
    #[test]
    fn reopen_for_reload_returns_deleted_for_missing_file() {
        let file = TempFile::create("reopen-deleted", b"gone soon");
        std::fs::remove_file(&file.path).expect("削除できるはず");

        let error = reopen_for_reload(&file.path).expect_err("削除済みなので失敗するはず");
        assert!(matches!(error, ReopenForReloadError::Deleted));
    }

    // 受け入れ条件: 共有違反（FileShare.None 相当）の再読み込みは
    // SharingViolation として区別される（LOG-027。再試行可能）。
    #[test]
    fn reopen_for_reload_returns_sharing_violation_when_locked_exclusively() {
        use std::os::windows::fs::OpenOptionsExt;

        let file = TempFile::create("reopen-sharing-violation", b"locked content");
        let _locker = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file.path)
            .expect("排他的に開けるはず（LOG-027 再現用の前提）");

        let error =
            reopen_for_reload(&file.path).expect_err("共有違反のため再オープンは失敗するはず");
        assert!(matches!(
            error,
            ReopenForReloadError::SharingViolation { .. }
        ));
    }
}
