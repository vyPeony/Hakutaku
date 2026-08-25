//! チャンク単位の読み込みと、資源抑制の接続点（`IoThrottle`）です
//! （P06-2、`tasks/phase-06-large-file-loading.md` 作業項目1・9）。
//!
//! # 位置づけ
//!
//! [`crate::read_snapshotted_bytes`]（P06-1）は `snapshot_end` までを一度に
//! `read_to_end` する実装でした。本モジュールは同じ境界（`snapshot_end`。
//! ADR-0007）を守りつつ、既定 [`DEFAULT_CHUNK_BYTES`]（8 MiB。**暫定値であり
//! 要件 ID を持ちません。値の最終決定は P11、`CFG-024`**）単位で逐次読み込み、
//! チャンク境界ごとに次を行います。
//!
//! 1. **整合性の再確認**（後述の2層構成）。縮小・置換・削除を検知した場合、
//!    それ以上読み進めず [`ChunkReadError::ChangeDetected`] を返します
//!    （`LOG-023`）。
//! 2. **キャンセルの確認。** 呼び出し側が渡す `is_cancelled` クロージャで判定
//!    します。ここでは P04-6 の `CancellationToken` 型を直接扱いません
//!    （`crates/data-source` は `crates/core-services` に依存しない下位層の
//!    ため）。呼び出し側（`crates/core-services`）が `CancellationToken` を
//!    このクロージャへ橋渡しします。
//! 3. **進捗の通知。** `on_chunk` コールバックへ、このチャンクで読んだバイト列と
//!    累計バイト数・総バイト数（`snapshot_end`）を渡します。P04-6 の
//!    `ProgressSink`／`ProgressThrottle` への変換も呼び出し側の責務です。
//! 4. **抑制の適用（`PERF-014` の接続点）。** [`IoThrottle`] が持つ同時実行数の
//!    上限（Semaphore 相当。std のみで実装）と I/O 発行間隔（チャンク間の
//!    待機）を適用します。
//! 5. **先読み抑制。** `eager_bytes` までは「要求済み範囲」として必ず読みます。
//!    それを超える範囲は「先読み」とみなし、`budget.prefetch_paused()`
//!    （`crates/memory-accounting`）が真の間は発行しません
//!    （[`ChunkReadOutcome::prefetch_stopped`]）。
//!
//! チャンク境界を跨ぐデコード（マルチバイト文字・継続行の安全な分割点の判断）は
//! `crates/core-services` の責務です。本モジュールは常に生バイト列のチャンクを
//! 返すだけで、文字コードやログの行構造を一切解釈しません。
//!
//! # 整合性再確認の2層構成
//!
//! 手順1の再確認は、検知したい事象ごとに手段と頻度を変えます。判定結果
//! （[`crate::SnapshotVerdict`]）とその後の扱いは、どちらの手段で気づいた場合も
//! 同一です。
//!
//! | 検知したい事象 | 手段 | 頻度 |
//! | --- | --- | --- |
//! | 縮小（切り詰め） | [`crate::verify_snapshot_by_handle`]（オープンなし） | **毎チャンク** |
//! | 置換・削除 | [`crate::verify_snapshot`]（パス再オープン） | [`PATH_VERIFY_CHUNK_INTERVAL`] チャンクごと＋読み切った直後 |
//!
//! **縮小の頻度を落とさないのは、それが読み取りの整合性に直結するためです。**
//! 読もうとしている範囲が消えた状態で読み進めると、途中まで登録した内容と
//! ファイルの実体が食い違います。この検知は既に開いているハンドルへの
//! 問い合わせだけで済み、ファイルオープンを伴わないため、毎チャンク行っても
//! 費用はほとんど増えません。
//!
//! **置換・削除の頻度を落とせるのは、これらがパスの再オープンでしか観測
//! できない（Windows の仕様。`crate::snapshot` のモジュール doc コメント参照）
//! 一方で、気づくのが遅れても読み取り中のデータが壊れないためです。**
//! `FILE_SHARE_DELETE` で開いたハンドルは、置換・削除の後も開いた時点の
//! ファイル実体を指し続けるので、読み取っているバイト列自体は一貫しています。
//! 再オープンはセキュリティソフトのフィルタードライバが介入する分だけ実機で
//! 増幅されるため、2層構成ではここを削ります。
//!
//! ## 挙動として変わること
//!
//! **置換・削除に気づくまでが最大 [`PATH_VERIFY_CHUNK_INTERVAL`] チャンク分
//! （既定のチャンクサイズなら 64 MiB 分の読み込み）遅れます。** 遅れて気づいた
//! 場合も、返す [`ChunkReadError::ChangeDetected`] とその判定は毎チャンク確認
//! していたときと同じであり、呼び出し側（`crates/core-services::loader`）の
//! 経路は変わりません。
//!
//! ## 不変条件
//!
//! **1バイトでも読んだ読み込みが最後まで完了する場合、パス再オープンによる
//! 再確認が必ず1回以上（最初のチャンクの前と、読み切った直後）行われます。**
//! 最後の確認を読み切った後に置くことで、「置換・削除に一度も気づかないまま
//! 読み込みを成功として報告する」ことがなくなります。キャンセル・先読み停止に
//! よる途中終了ではこの最終確認を行いません（呼び出し側が結果を未完了として
//! 扱うため、また、これらの打ち切りをエラーへ変えないためです）。

use std::fs::File;
use std::io::Read;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use hakutaku_memory_accounting::MemoryBudget;

use crate::snapshot::{
    verify_snapshot, verify_snapshot_by_handle, FileSnapshot, SnapshotVerdict, VerifySnapshotError,
};
use crate::{LoadedBytes, ReadFileError};

/// チャンクサイズの既定値（8 MiB）です。
///
/// **暫定値であり要件 ID を持ちません。** 値の最終決定は P11（`CFG-024`）の
/// 対象です。
pub const DEFAULT_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// パス再オープンによる整合性再確認を行う間隔（チャンク数）です
/// （モジュール doc コメントの「整合性再確認の2層構成」）。
///
/// 8 を選んだ理由は、既定のチャンクサイズ（[`DEFAULT_CHUNK_BYTES`] = 8 MiB）と
/// 掛け合わせて **64 MiB ごとに1回** という頻度になるためです。この値は次の
/// 2つの釣り合いで決めました。
///
/// - 小さすぎる（=1 に近い）と、削りたかったファイルオープンが残ります
/// - 大きすぎると、置換・削除に気づくまでの遅延が実用上無視できなくなります
///   （遅延の上限はチャンクサイズ × この値）
///
/// **暫定値であり要件 ID を持ちません。** チャンクサイズ自体が P11（`CFG-024`）
/// で見直される暫定値であり、この値もそれに合わせて見直す対象です。
pub const PATH_VERIFY_CHUNK_INTERVAL: u64 = 8;

/// チャンク境界での整合性再確認（`LOG-023`）を担う内部状態です。
///
/// モジュール doc コメントの「整合性再確認の2層構成」を実装します。使い手は
/// チャンク読み込みループを持つ [`stream_snapshotted_bytes_chunked`] だけです
/// （[`read_snapshotted_bytes_chunked`] はそこへ委譲するため、同じ判定と同じ
/// 周期がプロセス全体で1系統に保たれます）。
struct ChunkVerifier<'a> {
    path: &'a Path,
    snapshot: &'a FileSnapshot,
    /// これまでに再確認を行ったチャンクの数。パス再オープンの周期
    /// （[`PATH_VERIFY_CHUNK_INTERVAL`]）を決めるためだけに数えます。
    verified_chunks: u64,
}

impl<'a> ChunkVerifier<'a> {
    fn new(path: &'a Path, snapshot: &'a FileSnapshot) -> Self {
        ChunkVerifier {
            path,
            snapshot,
            verified_chunks: 0,
        }
    }

    /// 1チャンクを読む**前**の再確認です。
    ///
    /// 縮小は毎回（オープンを伴わないハンドルへの問い合わせで）確認し、
    /// 置換・削除は [`PATH_VERIFY_CHUNK_INTERVAL`] チャンクごとに確認します。
    fn before_chunk(&mut self, file: &File) -> Result<(), ChunkReadError> {
        // 周期の先頭でパス再オープンを行う。最初のチャンク（0番目）が必ず
        // ここに当たるため、読み込みの開始時点では従来どおり置換・削除も
        // 確認したうえで1バイト目を読む。
        let verdict = if self
            .verified_chunks
            .is_multiple_of(PATH_VERIFY_CHUNK_INTERVAL)
        {
            verify_snapshot(self.path, self.snapshot)
        } else {
            verify_snapshot_by_handle(file, self.snapshot)
        };
        self.verified_chunks += 1;
        interpret_verdict(verdict)
    }

    /// 最後のチャンクを読み**切った直後**の再確認です（パス再オープン）。
    ///
    /// モジュール doc コメントの「不変条件」を満たすための確認です。周期の
    /// 都合で最後のチャンクの前がハンドルへの問い合わせだった場合、この確認が
    /// なければ、読み込みの後半に起きた置換・削除へ一度も気づかないまま成功を
    /// 報告してしまいます。
    fn after_last_chunk(&self) -> Result<(), ChunkReadError> {
        interpret_verdict(verify_snapshot(self.path, self.snapshot))
    }
}

/// 再確認の結果を、チャンク読み込みループの継続可否へ翻訳します。
///
/// 追記（[`SnapshotVerdict::Appended`]）で読み込みを止めないのは、`snapshot_end`
/// を上限に読む限り追記分を読まないため（ADR-0007）、読み取りの整合性が保たれる
/// からです。追記分は「更新未反映」として次の再読み込みで反映します（`LOG-010`）。
fn interpret_verdict(
    result: Result<SnapshotVerdict, VerifySnapshotError>,
) -> Result<(), ChunkReadError> {
    match result {
        Ok(SnapshotVerdict::Unchanged | SnapshotVerdict::Appended { .. }) => Ok(()),
        Ok(other) => Err(ChunkReadError::ChangeDetected(other)),
        // 共有違反（LOG-027）は、既に開いているこの読み込みそのものを止める
        // ほどの事象ではないが、再確認のための再オープンが一時的に共有違反へ
        // 転じる可能性があるため、他の I/O エラーと区別して呼び出し側
        // （`crates/core-services`）が再試行可能として扱えるようにする。
        Err(error) if error.is_sharing_violation() => {
            Err(ChunkReadError::Read(ReadFileError::SharingViolation {
                reason: format!("整合性の再確認に失敗しました: {error}"),
            }))
        }
        Err(error) => Err(ChunkReadError::Read(ReadFileError::Io {
            reason: format!("整合性の再確認に失敗しました: {error}"),
        })),
    }
}

/// 同時実行数の上限（Semaphore 相当）と I/O 発行間隔を外部から与えるための
/// 抑制の接続点です（`PERF-014`）。
///
/// 既定は [`IoThrottle::unlimited`]（同時実行数無制限・待機なし）です。実際の
/// 既定値・設定項目化は P11 の対象であり、本モジュールは「外部から与えられる
/// 構造体」という接続点だけを提供します。
///
/// # 同時実行数の上限（Semaphore 相当）
///
/// 標準ライブラリの `Mutex` と `Condvar` だけで実装したカウンティング
/// セマフォです（外部クレートを追加しない方針のため、`tokio::sync::Semaphore`
/// 等は使いません）。現在の読み込みパイプラインは単一ファイルを1スレッドで
/// 逐次読むだけのため、実際に複数の並行読み込みが起きるのは複数ソースを
/// 別スレッドから同時に読む将来の利用（P09 以降が想定する複数ソース同時読み
/// 込みの高速化など）です。本フェーズでは「接続点を用意する」ことが目的であり、
/// 実際に複数スレッドから使う配線は対象外です。
#[derive(Debug, Clone)]
pub struct IoThrottle {
    max_concurrent: Option<NonZeroUsize>,
    io_interval: Duration,
    state: Arc<(Mutex<usize>, Condvar)>,
}

impl IoThrottle {
    /// 同時実行数の上限なし・I/O 発行間隔なし（待機なし）の抑制です。
    #[must_use]
    pub fn unlimited() -> Self {
        Self::new(None, 0)
    }

    /// 同時実行数の上限（`None` は無制限）と I/O 発行間隔（ミリ秒）を指定して
    /// 作成します。
    #[must_use]
    pub fn new(max_concurrent: Option<NonZeroUsize>, io_interval_ms: u64) -> Self {
        IoThrottle {
            max_concurrent,
            io_interval: Duration::from_millis(io_interval_ms),
            state: Arc::new((Mutex::new(0), Condvar::new())),
        }
    }

    /// I/O 発行間隔（チャンク間の待機）を返します。
    #[must_use]
    pub fn io_interval(&self) -> Duration {
        self.io_interval
    }

    /// 同時実行数の許可を1つ取得します。上限に達している間は待機します
    /// （Semaphore 相当）。上限が `None`（無制限）の場合は即座に許可を返します。
    ///
    /// 戻り値の [`IoPermit`] を破棄（drop）すると許可を返却します。
    fn acquire(&self) -> IoPermit<'_> {
        let Some(limit) = self.max_concurrent else {
            return IoPermit { throttle: None };
        };

        let (lock, condvar) = &*self.state;
        let mut in_flight = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *in_flight >= limit.get() {
            in_flight = condvar
                .wait(in_flight)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *in_flight += 1;
        IoPermit {
            throttle: Some(self),
        }
    }

    fn release(&self) {
        let (lock, condvar) = &*self.state;
        let mut in_flight = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *in_flight = in_flight.saturating_sub(1);
        drop(in_flight);
        condvar.notify_one();
    }
}

impl Default for IoThrottle {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// [`IoThrottle::acquire`] が返す許可です。破棄すると許可を返却します。
struct IoPermit<'a> {
    throttle: Option<&'a IoThrottle>,
}

impl Drop for IoPermit<'_> {
    fn drop(&mut self) {
        if let Some(throttle) = self.throttle {
            throttle.release();
        }
    }
}

/// [`read_snapshotted_bytes_chunked`] の入力をまとめた構造体です。
pub struct ChunkedReadRequest<'a> {
    pub file: File,
    /// 置換・削除の再確認（[`crate::verify_snapshot`]）のためのパスです。
    /// 開き直してこそ観測できる事象があるため、`file` とは別に受け取ります
    /// （`crate::snapshot` の doc コメント参照）。縮小の再確認は `file` から
    /// 行うため、このパスを使いません。
    pub path: &'a Path,
    pub snapshot: &'a FileSnapshot,
    pub budget: &'a MemoryBudget,
    /// 1チャンクあたりのバイト数（既定は [`DEFAULT_CHUNK_BYTES`]）。
    pub chunk_bytes: u64,
    pub throttle: &'a IoThrottle,
    /// この量までは「要求済み範囲」として、`budget.prefetch_paused()` に
    /// 関わらず必ず読みます。これを超える範囲は「先読み」として扱い、
    /// `prefetch_paused()` が真の間は発行しません。`snapshot_end` 以上を
    /// 指定すると、事実上すべての範囲が「要求済み」として扱われます
    /// （抑制なしの互換経路が使う既定値）。
    pub eager_bytes: u64,
    /// キャンセル要求の判定です。`crates/core-services` の
    /// `CancellationToken::is_cancelled` を橋渡しする想定です
    /// （本クレートは `core-services` に依存しないため、具象型ではなく
    /// クロージャで受け取ります）。
    pub is_cancelled: &'a dyn Fn() -> bool,
}

/// [`read_snapshotted_bytes_chunked`] の結果です。
#[derive(Debug, Clone)]
pub struct ChunkReadOutcome {
    /// `snapshot.snapshot_end` と同じ値（上限）。
    pub file_size_bytes: u64,
    /// `PERF-010` に従い実確保へ振り替えた量（バイト）。
    pub reserved_bytes: usize,
    /// 読み込んだ生バイト列（デコード前）。キャンセル・先読み停止により
    /// 途中で終わった場合は、読み込み済みの範囲だけを保持します（破棄しない）。
    pub bytes: Vec<u8>,
    /// 実際に読み込んだバイト数（`bytes.len()` と同じ）。
    pub bytes_read: u64,
    /// キャンセルにより途中で終わったか。
    pub cancelled: bool,
    /// `prefetch_paused()` により、`eager_bytes` を超える範囲の読み込みを
    /// 発行しなかったか（キャンセルとは別の理由による打ち切り）。
    pub prefetch_stopped: bool,
}

/// [`read_snapshotted_bytes_chunked`] の失敗です。
#[derive(Debug)]
pub enum ChunkReadError {
    /// データソース層の読み込み失敗（`PERF-010` の予約拒否・I/O エラー）。
    Read(ReadFileError),
    /// チャンク読み込み前の整合性再確認で変更を検知した（`LOG-023`）。
    /// このチャンクより前に読み込み済みの範囲は呼び出し側の責務で保持できる
    /// よう、検知時点では読み込み済みバイト列を返しません（呼び出し側は
    /// この変更を「そのソースを停止する」トリガーとして扱ってください）。
    ChangeDetected(SnapshotVerdict),
}

impl std::fmt::Display for ChunkReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChunkReadError::Read(error) => write!(f, "{error}"),
            ChunkReadError::ChangeDetected(verdict) => {
                write!(f, "読み込み中にファイルの変更を検知しました: {verdict:?}")
            }
        }
    }
}

impl std::error::Error for ChunkReadError {}

impl From<ReadFileError> for ChunkReadError {
    fn from(error: ReadFileError) -> Self {
        ChunkReadError::Read(error)
    }
}

/// `request.snapshot.snapshot_end` を上限に、チャンク単位で逐次読み込み、
/// 読み込んだ生バイト列を1つの `Vec<u8>` へ蓄積して返します。
///
/// 各チャンクを読む**前**に整合性を再確認します（`LOG-023`）。変更を検知した
/// 場合は [`ChunkReadError::ChangeDetected`] を返し、それ以上読み進めません。
/// 縮小は毎チャンク、置換・削除は [`PATH_VERIFY_CHUNK_INTERVAL`] チャンクごと
/// と読み切った直後に確認します（モジュール doc コメントの
/// 「整合性再確認の2層構成」）。
///
/// `on_chunk` は、新たに読み込んだチャンクのバイト列・累計読み込み量・
/// `snapshot_end` を受け取ります（進捗通知・逐次デコードに使う想定）。
///
/// `PERF-010` の予約は、[`crate::read_snapshotted_bytes`] と同じく
/// `snapshot_end` 全量を読み込み開始前に一括で予約し、完了時点（キャンセル・
/// 先読み停止による途中終了を含む）で実際に読んだ量へ振り替えます。
///
/// # [`stream_snapshotted_bytes_chunked`] との関係
///
/// **チャンク読み込みループそのものは持たず、
/// [`stream_snapshotted_bytes_chunked`] へ委譲します。** この関数が追加するのは
/// 「全量の予約（`PERF-010`）」と「チャンクの蓄積」の2点だけです。
///
/// ループを二重に持たない理由は、そこに載っている不変条件——整合性再確認の
/// 周期（縮小は毎チャンク／置換・削除は周期的）、読み切った直後の最終確認、
/// キャンセル・先読み停止では最終確認を行わないこと（モジュール doc コメントの
/// 「不変条件」）——を1か所でだけ保守するためです。2系統に分かれていると、
/// 片方だけを直したときに検知の抜けが生まれます（Issue #51 項目10）。
pub fn read_snapshotted_bytes_chunked(
    request: ChunkedReadRequest<'_>,
    mut on_chunk: impl FnMut(&[u8], u64, u64),
) -> Result<ChunkReadOutcome, ChunkReadError> {
    // `request` を委譲先へ渡す前に、予約に必要な値だけを取り出す（どちらも
    // `Copy` なので `request` はそのまま渡せる）。
    let snapshot_end = request.snapshot.snapshot_end;
    let budget = request.budget;

    // `PERF-010`: 蓄積用バッファの確保「前」に全量を予約する。拒否されたら
    // 1バイトも読まずに戻る（委譲先は蓄積しないため予約を行わない。両者の
    // 違いはこの予約と蓄積だけ）。
    let reserve_amount = usize::try_from(snapshot_end).unwrap_or(usize::MAX);
    let token = budget
        .reserve(reserve_amount)
        .map_err(ReadFileError::ReservationRejected)?;

    let mut buffer = Vec::with_capacity(reserve_amount);
    let summary = stream_snapshotted_bytes_chunked(request, |chunk, bytes_read, total_bytes| {
        buffer.extend_from_slice(chunk);
        on_chunk(chunk, bytes_read, total_bytes);
    })?;

    // 実確保（バッファの容量）を予約から実確保へ振り替える（ADR-0003）。
    let actual_bytes = buffer.capacity();
    let reserved_bytes = actual_bytes.min(token.remaining_bytes());
    let _ = token.mark_allocated(reserved_bytes);

    Ok(ChunkReadOutcome {
        file_size_bytes: summary.file_size_bytes,
        reserved_bytes,
        bytes: buffer,
        bytes_read: summary.bytes_read,
        cancelled: summary.cancelled,
        prefetch_stopped: summary.prefetch_stopped,
    })
}

/// [`stream_snapshotted_bytes_chunked`] の結果です。[`ChunkReadOutcome`] と
/// 異なり、読み込んだ生バイト列そのものは保持しません（`bytes` フィールドが
/// ない）。P08-5（索引 + オンデマンド読み出しへの移行）で、登録時に
/// ファイル全量を `Vec<u8>` として蓄積しない経路のために追加しました。
#[derive(Debug, Clone)]
pub struct ChunkReadSummary {
    /// `snapshot.snapshot_end` と同じ値（上限）。
    pub file_size_bytes: u64,
    /// 実際に読み込んだバイト数。
    pub bytes_read: u64,
    /// キャンセルにより途中で終わったか。
    pub cancelled: bool,
    /// `prefetch_paused()` により、`eager_bytes` を超える範囲の読み込みを
    /// 発行しなかったか。
    pub prefetch_stopped: bool,
    /// `File::read_exact` そのものに費やした時間の累計。
    ///
    /// # なぜこの区間だけを計るか
    ///
    /// 読み込み1回の所要時間を段階別に分けるとき、**呼び出し側
    /// （`crates/core-services::loader`）から観測できない唯一の段階が I/O
    /// です。** `on_chunk` の中で起きること（デコード・解析・登録）は呼び出し側が
    /// 自分で計れますが、`read_exact` は本関数の内側にしかありません。抑制の
    /// 待機（[`IoThrottle`] の許可待ちと発行間隔）・整合性の再確認・キャンセル
    /// 判定は I/O そのものではないため、意図的に含めません（それらは呼び出し
    /// 側の内訳では「その他」に落ちます）。
    ///
    /// # オーバーヘッドが無視できる根拠
    ///
    /// [`Instant::now`] の対はチャンクごとに1組だけ増えます。既定のチャンク
    /// （[`DEFAULT_CHUNK_BYTES`] = 8 MiB）なら 1 GiB のファイルで128組であり、
    /// 1組あたり数十ナノ秒（Windows では `QueryPerformanceCounter`）の合計は
    /// 数マイクロ秒です。同じ 1 GiB の読み込みが秒の桁である以上、実測へ現れ
    /// ません。**行ごとには決して計らない**（数百万回になり、この前提が崩れる）
    /// という境界の置き方が、この見積もりの根拠です。
    pub read_elapsed: Duration,
}

/// チャンク読み込みループ（整合性の再確認・キャンセル・抑制・先読み停止）の
/// **唯一の実装**です（[`read_snapshotted_bytes_chunked`] もここへ委譲します）。
/// この関数は**読み込んだ生バイト列を蓄積しません**。各チャンクは `on_chunk` へ
/// 一時的に渡されるだけで、この関数の呼び出しが終わるまで生バイト列全体を
/// `Vec<u8>` として保持することはありません（P08-5「本文の全量保持をやめ、
/// 索引 + オンデマンド読み出しへ移行する」）。
///
/// この理由により、`PERF-010` の「読み込みバッファの確保前に予約する」対象が
/// なくなるため（保持するバッファ自体が存在しない）、`request.budget` へは
/// 全量の予約を行いません（`budget.prefetch_paused()` の判定にだけ使います）。
/// 呼び出し側（`crates/core-services::loader`）が、代わりに行数に比例して常駐
/// する構造の伸長分（索引は `crate::line_index::reserve_growth`、表示集合の
/// 項目列は `crate::item::reserve_items_growth`）を予約します。
///
/// `on_chunk` からのエラー伝播は行いません（`crates/data-source` は
/// `crates/core-services` のエラー型に依存しないため）。呼び出し側が
/// `is_cancelled` を通じて次のチャンクの前に停止させてください
/// （`register_source_with_control` の `is_cancelled_combined` と同じ方式）。
pub fn stream_snapshotted_bytes_chunked(
    request: ChunkedReadRequest<'_>,
    mut on_chunk: impl FnMut(&[u8], u64, u64),
) -> Result<ChunkReadSummary, ChunkReadError> {
    let ChunkedReadRequest {
        mut file,
        path,
        snapshot,
        budget,
        chunk_bytes,
        throttle,
        eager_bytes,
        is_cancelled,
    } = request;

    let snapshot_end = snapshot.snapshot_end;
    let chunk_bytes = chunk_bytes.max(1);

    let mut chunk_buffer = vec![0u8; usize::try_from(chunk_bytes).unwrap_or(usize::MAX)];
    let mut offset: u64 = 0;
    let mut cancelled = false;
    let mut prefetch_stopped = false;
    let mut first_chunk = true;
    let mut verifier = ChunkVerifier::new(path, snapshot);
    // 段階別内訳の I/O 分（`ChunkReadSummary::read_elapsed`）。
    let mut read_elapsed = Duration::ZERO;

    while offset < snapshot_end {
        let _permit = throttle.acquire();

        if !first_chunk && !throttle.io_interval().is_zero() {
            std::thread::sleep(throttle.io_interval());
        }
        first_chunk = false;

        if is_cancelled() {
            cancelled = true;
            break;
        }

        if offset >= eager_bytes && budget.prefetch_paused() {
            prefetch_stopped = true;
            break;
        }

        // 縮小は毎回、置換・削除は周期的に確認する（モジュール doc
        //  コメントの「整合性再確認の2層構成」）。
        verifier.before_chunk(&file)?;

        let remaining = snapshot_end - offset;
        let this_chunk_len = remaining.min(chunk_bytes);
        let this_chunk_len_usize = usize::try_from(this_chunk_len).unwrap_or(usize::MAX);
        // 計時は read_exact だけを挟む。抑制の待機・整合性再確認・
        // キャンセル判定を含めると、I/O の内訳が「待たされた時間」で薄まる。
        let read_began = Instant::now();
        let read_result = file.read_exact(&mut chunk_buffer[..this_chunk_len_usize]);
        read_elapsed += read_began.elapsed();
        read_result.map_err(|error| ReadFileError::Io {
            reason: format!("ファイルを読み込めません（{error}）"),
        })?;

        offset += this_chunk_len;
        on_chunk(&chunk_buffer[..this_chunk_len_usize], offset, snapshot_end);
    }

    // 読み切った場合だけ最終確認を行う（モジュール doc コメントの「不変条件」）。
    // キャンセル・先読み停止による途中終了で確認しないのは、それらを整合性の
    // エラーへ変えないためである（呼び出し側は結果を未完了として扱う）。
    // 空ファイル（snapshot_end が 0）はループ本体を一度も通らないため、読んだ
    // ものがない読み込みのためにファイルを開き直すことはしない。
    if !cancelled && !prefetch_stopped && offset > 0 {
        verifier.after_last_chunk()?;
    }

    Ok(ChunkReadSummary {
        file_size_bytes: snapshot_end,
        bytes_read: offset,
        cancelled,
        prefetch_stopped,
        read_elapsed,
    })
}

/// [`LoadedBytes`] へ変換します（互換経路が既存の型をそのまま使えるように
/// するための変換）。`cancelled`・`prefetch_stopped`・`bytes_read` は失われる
/// ため、これらの情報が必要な呼び出し側は [`ChunkReadOutcome`] を直接使って
/// ください。
impl From<ChunkReadOutcome> for LoadedBytes {
    fn from(outcome: ChunkReadOutcome) -> Self {
        LoadedBytes {
            file_size_bytes: outcome.file_size_bytes,
            reserved_bytes: outcome.reserved_bytes,
            bytes: outcome.bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_and_snapshot;
    use std::sync::atomic::AtomicUsize;
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
                "hakutaku-data-source-chunk-test-{label}-{}-{count}-{nanos}.log",
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

    // 受け入れ条件: チャンク読み込みが全件一括読み込みと同一のバイト列になる
    // （境界がちょうど・半端どちらでも）。
    #[test]
    fn chunked_read_matches_whole_file_read_for_various_chunk_sizes() {
        let contents: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        for chunk_bytes in [1u64, 3, 7, 64, 4096, 1_000_000] {
            let file = TempFile::create(&format!("match-{chunk_bytes}"), &contents);
            let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
            let budget = MemoryBudget::new(usize::MAX);
            let throttle = IoThrottle::unlimited();

            let outcome = read_snapshotted_bytes_chunked(
                ChunkedReadRequest {
                    file: handle,
                    path: &file.path,
                    snapshot: &snapshot,
                    budget: &budget,
                    chunk_bytes,
                    throttle: &throttle,
                    eager_bytes: snapshot.snapshot_end,
                    is_cancelled: &|| false,
                },
                |_, _, _| {},
            )
            .expect("読み込みは成功するはず");

            assert_eq!(
                outcome.bytes, contents,
                "chunk_bytes={chunk_bytes} で不一致"
            );
            assert!(!outcome.cancelled);
            assert!(!outcome.prefetch_stopped);
            assert_eq!(outcome.bytes_read, contents.len() as u64);
        }
    }

    // 受け入れ条件: on_chunk が累計バイト数・総バイト数を正しく通知する。
    #[test]
    fn on_chunk_reports_cumulative_progress() {
        let contents = vec![b'x'; 25];
        let file = TempFile::create("progress", &contents);
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        let budget = MemoryBudget::new(usize::MAX);
        let throttle = IoThrottle::unlimited();

        let mut observed = Vec::new();
        let outcome = read_snapshotted_bytes_chunked(
            ChunkedReadRequest {
                file: handle,
                path: &file.path,
                snapshot: &snapshot,
                budget: &budget,
                chunk_bytes: 10,
                throttle: &throttle,
                eager_bytes: snapshot.snapshot_end,
                is_cancelled: &|| false,
            },
            |chunk, done, total| observed.push((chunk.len(), done, total)),
        )
        .expect("読み込みは成功するはず");

        assert_eq!(observed, vec![(10, 10, 25), (10, 20, 25), (5, 25, 25)]);
        assert_eq!(outcome.bytes_read, 25);
    }

    // 受け入れ条件: チャンク境界でキャンセルを検出すると、読み込み済み範囲を
    // 保持したまま停止する。
    #[test]
    fn cancellation_stops_at_chunk_boundary_and_keeps_partial_bytes() {
        let contents = vec![b'y'; 100];
        let file = TempFile::create("cancel", &contents);
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        let budget = MemoryBudget::new(usize::MAX);
        let throttle = IoThrottle::unlimited();

        let chunk_count = AtomicUsize::new(0);
        let is_cancelled = || chunk_count.load(Ordering::SeqCst) >= 3;

        let outcome = read_snapshotted_bytes_chunked(
            ChunkedReadRequest {
                file: handle,
                path: &file.path,
                snapshot: &snapshot,
                budget: &budget,
                chunk_bytes: 10,
                throttle: &throttle,
                eager_bytes: snapshot.snapshot_end,
                is_cancelled: &is_cancelled,
            },
            |_, _, _| {
                chunk_count.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect("キャンセルはエラーではなく正常応答扱い");

        assert!(outcome.cancelled);
        assert_eq!(outcome.bytes_read, 30, "3チャンク分だけ読み込み済みのはず");
        assert_eq!(outcome.bytes, &contents[..30]);
    }

    // 受け入れ条件: 各チャンクの読み込み前に整合性を再確認し、縮小を検知したら
    // それ以上読み進めない。
    #[test]
    fn change_detection_stops_further_reading() {
        let contents = vec![b'z'; 100];
        let file = TempFile::create("change", &contents);
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        let budget = MemoryBudget::new(usize::MAX);
        let throttle = IoThrottle::unlimited();

        let chunk_count = AtomicUsize::new(0);
        let path = file.path.clone();
        let error = read_snapshotted_bytes_chunked(
            ChunkedReadRequest {
                file: handle,
                path: &file.path,
                snapshot: &snapshot,
                budget: &budget,
                chunk_bytes: 10,
                throttle: &throttle,
                eager_bytes: snapshot.snapshot_end,
                is_cancelled: &|| false,
            },
            move |_, _, _| {
                let count = chunk_count.fetch_add(1, Ordering::SeqCst);
                if count == 1 {
                    // 2チャンク目の直後にファイルを縮小させる。次のチャンクの
                    // 読み込み前に検知されるはず。
                    let writer = std::fs::OpenOptions::new()
                        .write(true)
                        .open(&path)
                        .expect("書き込み用に開けるはず");
                    writer.set_len(5).expect("切り詰めできるはず");
                }
            },
        )
        .expect_err("縮小を検知して失敗するはず");

        assert!(matches!(
            error,
            ChunkReadError::ChangeDetected(SnapshotVerdict::Shrunk { .. })
        ));
    }

    // 受け入れ条件: I/O 発行間隔（io_interval_ms）が待機を発生させる
    // （時間検証は緩め。何らかの計測可能な遅延が生じることだけを確認する）。
    #[test]
    fn io_interval_causes_measurable_delay() {
        let contents = vec![b'w'; 40];
        let file = TempFile::create("interval", &contents);
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        let budget = MemoryBudget::new(usize::MAX);
        let throttle = IoThrottle::new(None, 20);

        let started = std::time::Instant::now();
        let outcome = read_snapshotted_bytes_chunked(
            ChunkedReadRequest {
                file: handle,
                path: &file.path,
                snapshot: &snapshot,
                budget: &budget,
                chunk_bytes: 10,
                throttle: &throttle,
                eager_bytes: snapshot.snapshot_end,
                is_cancelled: &|| false,
            },
            |_, _, _| {},
        )
        .expect("読み込みは成功するはず");

        // 4チャンク、初回を除く3回の待機（20ms）で、緩めに60ms以上を期待する
        // ところを、環境差を考慮してさらに緩め（30ms以上）で確認する。
        assert!(
            started.elapsed() >= Duration::from_millis(30),
            "io_interval_ms による待機が発生しているはず: {:?}",
            started.elapsed()
        );
        assert_eq!(outcome.bytes_read, 40);
    }

    // 受け入れ条件: prefetch_paused() が真の間、eager_bytes を超える範囲
    // （先読み）は発行されない。要求済み範囲（eager_bytes まで）は読み込まれる。
    #[test]
    fn prefetch_paused_stops_reading_beyond_eager_bytes() {
        let contents = vec![b'p'; 50];
        let file = TempFile::create("prefetch", &contents);
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        // 予約自体（50バイト）は許可されるだけの予算を持たせつつ、しきい値の
        // 割合を極端に低くして、その予約だけで即座に prefetch_paused() が
        // 真になるようにする（しきい値 = 1000 * 1% = 10 バイト < 50 バイト）。
        let budget = MemoryBudget::new(1000);
        budget
            .set_soft_threshold_percent(1)
            .expect("1は有効な割合のはず");
        let throttle = IoThrottle::unlimited();

        let outcome = read_snapshotted_bytes_chunked(
            ChunkedReadRequest {
                file: handle,
                path: &file.path,
                snapshot: &snapshot,
                budget: &budget,
                chunk_bytes: 10,
                throttle: &throttle,
                eager_bytes: 20, // 最初の20バイトだけは要求済みとして必ず読む。
                is_cancelled: &|| false,
            },
            |_, _, _| {},
        )
        .expect("読み込みは成功するはず（打ち切りはエラーではない）");

        assert!(outcome.prefetch_stopped);
        assert_eq!(
            outcome.bytes_read, 20,
            "要求済み範囲(eager_bytes=20)までは読むが、それ以降の先読みは発行しないはず"
        );
        assert_eq!(outcome.bytes, &contents[..20]);
    }

    // --- stream_snapshotted_bytes_chunked（P08-5） ---

    // 受け入れ条件: on_chunk へ渡すバイト列を外部で連結すると、全件一括読み込みと
    // 同一のバイト列になる（read_snapshotted_bytes_chunked と同じ結果）。
    #[test]
    fn stream_matches_whole_file_read_for_various_chunk_sizes() {
        let contents: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        for chunk_bytes in [1u64, 3, 7, 64, 4096, 1_000_000] {
            let file = TempFile::create(&format!("stream-match-{chunk_bytes}"), &contents);
            let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
            let budget = MemoryBudget::new(usize::MAX);
            let throttle = IoThrottle::unlimited();

            let mut reconstructed = Vec::new();
            let summary = stream_snapshotted_bytes_chunked(
                ChunkedReadRequest {
                    file: handle,
                    path: &file.path,
                    snapshot: &snapshot,
                    budget: &budget,
                    chunk_bytes,
                    throttle: &throttle,
                    eager_bytes: snapshot.snapshot_end,
                    is_cancelled: &|| false,
                },
                |chunk, _, _| reconstructed.extend_from_slice(chunk),
            )
            .expect("読み込みは成功するはず");

            assert_eq!(
                reconstructed, contents,
                "chunk_bytes={chunk_bytes} で不一致"
            );
            assert!(!summary.cancelled);
            assert!(!summary.prefetch_stopped);
            assert_eq!(summary.bytes_read, contents.len() as u64);
        }
    }

    // 受け入れ条件: 大きな予算予約を一切行わなくても読み込める（読み込み
    // バッファを保持しないため、予算はほぼ消費されない）。
    #[test]
    fn stream_does_not_reserve_the_full_file_size_upfront() {
        let contents = vec![b'q'; 1_000];
        let file = TempFile::create("stream-no-reserve", &contents);
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        // 予算をファイルサイズ未満にしても、全量バッファを確保しないため
        // 成功するはず（read_snapshotted_bytes_chunked ならここで拒否される）。
        let budget = MemoryBudget::new(10);
        let throttle = IoThrottle::unlimited();

        let summary = stream_snapshotted_bytes_chunked(
            ChunkedReadRequest {
                file: handle,
                path: &file.path,
                snapshot: &snapshot,
                budget: &budget,
                chunk_bytes: 100,
                throttle: &throttle,
                eager_bytes: snapshot.snapshot_end,
                is_cancelled: &|| false,
            },
            |_, _, _| {},
        )
        .expect("読み込みバッファを保持しないため小さい予算でも成功するはず");

        assert_eq!(summary.bytes_read, 1_000);
    }

    // 受け入れ条件: チャンク境界でキャンセルを検出すると停止する。
    #[test]
    fn stream_cancellation_stops_at_chunk_boundary() {
        let contents = vec![b'y'; 100];
        let file = TempFile::create("stream-cancel", &contents);
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        let budget = MemoryBudget::new(usize::MAX);
        let throttle = IoThrottle::unlimited();

        let chunk_count = AtomicUsize::new(0);
        let is_cancelled = || chunk_count.load(Ordering::SeqCst) >= 3;

        let summary = stream_snapshotted_bytes_chunked(
            ChunkedReadRequest {
                file: handle,
                path: &file.path,
                snapshot: &snapshot,
                budget: &budget,
                chunk_bytes: 10,
                throttle: &throttle,
                eager_bytes: snapshot.snapshot_end,
                is_cancelled: &is_cancelled,
            },
            |_, _, _| {
                chunk_count.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect("キャンセルはエラーではなく正常応答扱い");

        assert!(summary.cancelled);
        assert_eq!(summary.bytes_read, 30, "3チャンク分だけ読み込み済みのはず");
    }

    // 受け入れ条件: 各チャンクの読み込み前に整合性を再確認し、縮小を検知したら
    // それ以上読み進めない。
    #[test]
    fn stream_change_detection_stops_further_reading() {
        let contents = vec![b'z'; 100];
        let file = TempFile::create("stream-change", &contents);
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        let budget = MemoryBudget::new(usize::MAX);
        let throttle = IoThrottle::unlimited();

        let chunk_count = AtomicUsize::new(0);
        let path = file.path.clone();
        let error = stream_snapshotted_bytes_chunked(
            ChunkedReadRequest {
                file: handle,
                path: &file.path,
                snapshot: &snapshot,
                budget: &budget,
                chunk_bytes: 10,
                throttle: &throttle,
                eager_bytes: snapshot.snapshot_end,
                is_cancelled: &|| false,
            },
            move |_, _, _| {
                let count = chunk_count.fetch_add(1, Ordering::SeqCst);
                if count == 1 {
                    let writer = std::fs::OpenOptions::new()
                        .write(true)
                        .open(&path)
                        .expect("書き込み用に開けるはず");
                    writer.set_len(5).expect("切り詰めできるはず");
                }
            },
        )
        .expect_err("縮小を検知して失敗するはず");

        assert!(matches!(
            error,
            ChunkReadError::ChangeDetected(SnapshotVerdict::Shrunk { .. })
        ));
    }

    // 受け入れ条件: 縮小（切り詰め）は、パス再オープンの周期
    // （PATH_VERIFY_CHUNK_INTERVAL）に当たらないチャンクでも、その直後の
    // チャンク境界で検知される（LOG-023。2層構成のうち「縮小は
    // 毎チャンク」の層）。
    //
    // 4チャンク目の直後に切り詰めると、次に確認するのは5チャンク目
    // （0起点で添字4）である。8の倍数ではないためパス再オープンは行われず、
    // 既に開いているハンドルへの問い合わせだけで検知できることを、配信された
    // チャンク数がちょうど4であることで確かめる。
    #[test]
    fn stream_detects_shrink_at_a_chunk_without_path_reopen() {
        let contents = vec![b'z'; 100];
        let file = TempFile::create("stream-shrink-no-reopen", &contents);
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        let budget = MemoryBudget::new(usize::MAX);
        let throttle = IoThrottle::unlimited();

        let delivered = AtomicUsize::new(0);
        let path = file.path.clone();
        let error = stream_snapshotted_bytes_chunked(
            ChunkedReadRequest {
                file: handle,
                path: &file.path,
                snapshot: &snapshot,
                budget: &budget,
                chunk_bytes: 10,
                throttle: &throttle,
                eager_bytes: snapshot.snapshot_end,
                is_cancelled: &|| false,
            },
            |_, _, _| {
                let count = delivered.fetch_add(1, Ordering::SeqCst);
                if count == 3 {
                    let writer = std::fs::OpenOptions::new()
                        .write(true)
                        .open(&path)
                        .expect("書き込み用に開けるはず");
                    writer.set_len(5).expect("切り詰めできるはず");
                }
            },
        )
        .expect_err("縮小を検知して失敗するはず");

        assert!(matches!(
            error,
            ChunkReadError::ChangeDetected(SnapshotVerdict::Shrunk { .. })
        ));
        assert_eq!(
            delivered.load(Ordering::SeqCst),
            4,
            "切り詰めの直後のチャンク境界で検知し、それ以上読み進めないはず"
        );
    }

    // 受け入れ条件: 置換は、パス再オープンの周期に当たらない間は検知されず、
    // 読み切った直後の最終確認で検知される（検知の遅延はあっても
    // 従来と同じ ChangeDetected の経路へ入ることの確認）。
    //
    // 置換は元のファイルを別名へ退避してから同じパスへ新しいファイルを作って
    // 作る（ログの世代交代と同じ形）。FILE_SHARE_DELETE で開いた読み取り
    // ハンドルは退避後も元の実体を指し続けるため、残りのチャンクはそのまま
    // 読み切れる。毎チャンクのパス再確認を行っていた頃はこの試験で2チャンク
    // 目の前に失敗していたので、配信されたチャンク数が全数であることが、
    // 検知が最終確認まで遅れたことの証拠になる。
    #[test]
    fn stream_defers_replacement_detection_to_the_final_verification() {
        let contents = vec![b'r'; 50];
        let file = TempFile::create("stream-replaced-final", &contents);
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        let budget = MemoryBudget::new(usize::MAX);
        let throttle = IoThrottle::unlimited();

        let rotated = file.path.with_extension("rotated");
        let delivered = AtomicUsize::new(0);
        let path = file.path.clone();
        let rotated_for_chunk = rotated.clone();
        let error = stream_snapshotted_bytes_chunked(
            ChunkedReadRequest {
                file: handle,
                path: &file.path,
                snapshot: &snapshot,
                budget: &budget,
                chunk_bytes: 10,
                throttle: &throttle,
                eager_bytes: snapshot.snapshot_end,
                is_cancelled: &|| false,
            },
            |_, _, _| {
                let count = delivered.fetch_add(1, Ordering::SeqCst);
                if count == 1 {
                    std::fs::rename(&path, &rotated_for_chunk).expect("退避できるはず");
                    // 退避元がまだ存在するため、同じパスへ作る新しいファイルは
                    // 必ず別のファイル識別子を持つ（＝Replaced になる）。
                    std::fs::write(&path, vec![b'n'; 50]).expect("作り直せるはず");
                }
            },
        )
        .expect_err("置換を検知して失敗するはず");
        let _ = std::fs::remove_file(&rotated);

        assert!(matches!(
            error,
            ChunkReadError::ChangeDetected(SnapshotVerdict::Replaced)
        ));
        assert_eq!(
            delivered.load(Ordering::SeqCst),
            5,
            "置換に気づくのは最終確認のときであり、それまでは読み切るはず"
        );
    }

    // 受け入れ条件: キャンセルによる途中終了では最終確認を行わない（打ち切りを
    // 整合性エラーへ変えない。モジュール doc コメントの「不変条件」）。読み込みを
    // 止めた後にファイルを消しても、結果はキャンセルの成功応答のままである。
    #[test]
    fn stream_cancellation_does_not_run_the_final_path_verification() {
        let contents = vec![b'c'; 100];
        let file = TempFile::create("stream-cancel-no-final", &contents);
        let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
        let budget = MemoryBudget::new(usize::MAX);
        let throttle = IoThrottle::unlimited();

        let delivered = AtomicUsize::new(0);
        let path = file.path.clone();
        let is_cancelled = || {
            if delivered.load(Ordering::SeqCst) >= 2 {
                // 打ち切りを決めた時点でファイルを消す。最終確認を行う実装なら
                // ここで Deleted としてエラーになる。
                let _ = std::fs::remove_file(&path);
                true
            } else {
                false
            }
        };

        let summary = stream_snapshotted_bytes_chunked(
            ChunkedReadRequest {
                file: handle,
                path: &file.path,
                snapshot: &snapshot,
                budget: &budget,
                chunk_bytes: 10,
                throttle: &throttle,
                eager_bytes: snapshot.snapshot_end,
                is_cancelled: &is_cancelled,
            },
            |_, _, _| {
                delivered.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect("キャンセルはエラーではなく正常応答扱い");

        assert!(summary.cancelled);
        assert_eq!(summary.bytes_read, 20);
    }

    // 受け入れ条件: 同時実行数の上限（Semaphore 相当）が機能する。上限1で
    // 2スレッドが同時に許可を取得しようとすると、片方は解放を待つ。
    #[test]
    fn io_throttle_limits_concurrent_permits() {
        let throttle = Arc::new(IoThrottle::new(NonZeroUsize::new(1), 0));
        let entered = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..4 {
            let throttle = Arc::clone(&throttle);
            let entered = Arc::clone(&entered);
            let max_observed = Arc::clone(&max_observed);
            handles.push(std::thread::spawn(move || {
                let _permit = throttle.acquire();
                let current = entered.fetch_add(1, Ordering::SeqCst) + 1;
                max_observed.fetch_max(current, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                entered.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for handle in handles {
            handle.join().expect("パニックしないはず");
        }

        assert_eq!(
            max_observed.load(Ordering::SeqCst),
            1,
            "同時実行数の上限(1)を超えて同時に許可が出てはいけない"
        );
    }
}
