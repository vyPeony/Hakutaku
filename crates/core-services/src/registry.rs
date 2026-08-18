//! 表示集合レジストリ（`DisplaySetRegistry`）。
//!
//! GUI 非依存です。`src-tauri` は、この型を `Mutex` に包んで Tauri の managed
//! state として保持するだけで、解析・範囲取得ロジックそのものは持ちません
//! （計画書「作業項目8: 層境界の確認」）。
//!
//! # P06 での一般化（複数ソースの登録）
//!
//! 複数ソースを独立に登録・列挙・close できる経路（[`DisplaySetRegistry::
//! insert_source`]・[`list_sources`]・[`close_source`]・[`refresh_source`]）を
//! 提供します（`tasks/phase-06-large-file-loading.md` 「P09 との担当境界」:
//! 複数ソースの登録・各ソース内の安定した順序・`source_id` と来歴・世代・
//! ソースごとの独立した走査は P06 の担当。ソースをまたぐグローバルな全順序は
//! P09）。
//!
//! 「1表示集合 = 1ソース」は維持します。複数ソースはそれぞれ独立の表示集合
//! として保持され、ソースをまたぐ統合表示集合（時系列マージ）は作りません
//! （P09 の対象）。
//!
//! # P08-5 索引 + オンデマンド読み出し
//!
//! [`DisplaySetRegistry::fetch_range`] は、`crate::display_set::DisplaySet` から
//! 索引レベルの応答（`crate::display_set::IndexItemRef`）を取得したあと、この
//! メソッド自身がソースファイルへオンデマンドでアクセスして本文をデコードし、
//! `ItemDto` を組み立てます（「オンデマンド読み出し」の実装本体）。
//!
//! - ソースの再オープンには [`hakutaku_data_source::reopen_for_reload`] を使い、
//!   毎回の範囲取得で開き直します（ファイルハンドルを長期保持しない設計。
//!   Windows の共有可オープンであれば頻繁な開閉のコストは軽微であり、
//!   共有違反・削除の検知が範囲取得のたびに自然に効くという利点があります）。
//! - デコード済みチャンクの有界キャッシュ（[`crate::chunk_cache::
//!   DecodedChunkCache`]）が、繰り返しアクセスのコストを抑えます。要求した
//!   項目群がキャッシュ済みチャンクへ完全に包含される場合もヒットします
//!   （照合の規則と効果の範囲は `crate::chunk_cache` のモジュール
//!   doc コメント参照）。ヒット時はファイルを開き直しません。
//! - 再オープン時にファイルの縮小・置換・削除を検知した場合、`LOG-023` と
//!   同じ経路（[`Self::mark_changed_now`]）で無効化します。共有違反
//!   （`LOG-027`）は [`Self::mark_sharing_violation_now`] で区別します。**この
//!   応答1件の中では、他の項目・他のソースの取得を継続します**（`ERR-001`。
//!   影響を受けたソースの項目は空の本文で返り、次回以降の範囲取得は世代不一致
//!   （フロントエンドが検出して再取得する）または `SourceStatus` の変化で
//!   利用者に伝わります）。ただし**クリップボードコピーだけは、空になった
//!   本文が利用者の手元へ渡ると取り消せない**ため、この既定値の件数を
//!   [`DisplaySetRegistry::hydrate_fallback_items`] で数え、
//!   `crate::copy::assemble_copy` がコピー全体を失敗させます（`COPY-005`、
//!   Issue #37）。
//!
//! # P08-3 → P08-5: しきい値到達時の解放の単純化
//!
//! P08-1 時点は、`IndexedText` がデコード済み全文バッファ
//! （ファイルサイズ相当、最大の常駐コスト）を保持していたため、しきい値
//! 到達時の解放は「索引・項目そのものを破棄し、`SourceStatus::Evicted` へ
//! 遷移させ、再アクセス時に丸ごと再登録する」という重い戦略でした。
//!
//! P08-5 で `IndexedText` が本文を一切保持しなくなり、行数に比例
//! する常駐コストは索引（24+8バイト）と表示集合の項目列（`crate::item::Item`、
//! 24バイト）だけ＝1行あたり56バイト（[`crate::item::RESIDENT_BYTES_PER_ITEM`]）
//! になったため、**索引を解放する意味がなくなりました**（本文を
//! 保持していた頃と比べれば十分小さく、常時保持して問題ありません）。
//! そのため [`Self::evict_inactive_sources`] は、しきい値到達時に非アクティブな
//! ソースの**デコード済みチャンクキャッシュだけをクリアする**単純な操作へ
//! 縮小しました。`SourceStatus`・世代・項目はもはや変更しません。
//!
//! **互換性のために残したもの:** [`SourceStatus::Evicted`]・[`Self::
//! commit_restore`]・`crate::loader::restore_evicted_source`・`src-tauri` 側の
//! `EvictionFlag`／`drain_pending_eviction` の配線は、そのまま残しています。
//! `evict_inactive_sources` がもはや `Evicted` 状態を作らないため、これらは
//! 実行時には到達しなくなりましたが、`restore_evicted_source` 自体は
//! ソース状態に関わらず独立して動作する「強制的な再読み込み＋世代進行」
//! 操作として引き続き有効であり、削除・呼び出し契約の変更には踏み込んで
//! いません（後続課題として報告します）。

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use hakutaku_data_source::{FileSnapshot, SnapshotVerdict};
use hakutaku_format_detection::SelectedEncoding;
use hakutaku_parser::LogDateTimeFormat;

use crate::budget::SourceReservation;
use crate::chunk_cache::{ChunkItemSpan, DecodedChunkCache};
use crate::display_set::{
    DisplaySet, IndexItemRef, ItemDto, RangeFetchError, RangeRequest, RangeResponse,
    MAX_ITEMS_PER_RESPONSE, MAX_RESPONSE_RAW_BYTES,
};
use crate::item::{
    build_items_from_pending_into, ensure_resident_capacity, CapacityEstimate, Item, ItemId,
    PendingItem, SourceInfo,
};
use crate::line_index::IndexedText;
use crate::merge::{self, MergeMember};

// --- 範囲取得経路の軽量カウンタ（キャッシュ照合の計測用） ---

/// デコード済みチャンクキャッシュのヒット回数（プロセス累計）。
static CHUNK_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
/// デコード済みチャンクキャッシュのミス回数（プロセス累計）。
static CHUNK_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
/// 範囲取得のためにソースファイルを開き直した回数（プロセス累計）。
static SOURCE_REOPENS: AtomicU64 = AtomicU64::new(0);

/// 範囲取得（fetch）経路の累計カウンタです（キャッシュ照合の計測用）。
///
/// キャッシュ照合の効き方とソース再オープンの回数を、計測ハーネス
/// （`crates/core-services/examples/scale_verify.rs`）やテストから観測する
/// ためのものです。**利用者向けの挙動には一切影響しません。** 更新は
/// `Ordering::Relaxed` の加算1回だけで、範囲取得の判断にこの値を読むことは
/// ありません（計数の有無で結果も経路も変わりません）。
///
/// ヒット・ミスは「1ソース分の項目群」（[`DisplaySetRegistry::fetch_range`] が
/// 内部で分割する単位）ごとに数えます。単独ソースの表示集合では範囲取得1回に
/// つき1件ですが、統合表示集合では1回の範囲取得が参加ソースの数だけヒット・
/// ミスを生みます。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FetchPathMetrics {
    /// デコード済みチャンクキャッシュのヒット回数。
    pub chunk_cache_hits: u64,
    /// デコード済みチャンクキャッシュのミス回数。
    pub chunk_cache_misses: u64,
    /// 範囲取得のためにソースファイルを開き直した回数
    /// （[`hakutaku_data_source::reopen_for_reload`] の呼び出し回数）。
    /// 読み込み・再読み込み経路の再オープンは含みません。
    pub source_reopens: u64,
}

/// 範囲取得経路のカウンタの現在値を返します（キャッシュ照合の計測用）。
#[must_use]
pub fn fetch_path_metrics() -> FetchPathMetrics {
    FetchPathMetrics {
        chunk_cache_hits: CHUNK_CACHE_HITS.load(Ordering::Relaxed),
        chunk_cache_misses: CHUNK_CACHE_MISSES.load(Ordering::Relaxed),
        source_reopens: SOURCE_REOPENS.load(Ordering::Relaxed),
    }
}

/// 範囲取得経路のカウンタを 0 に戻します（キャッシュ照合の計測用）。
///
/// カウンタはプロセス全体で共有されるため、計測区間の前後で差を取るか、
/// 区間の開始時にこれを呼びます。並行して範囲取得を行うスレッドがある場合、
/// 0 に戻す操作と加算の順序は保証されません（計測用途のみを想定）。
pub fn reset_fetch_path_metrics() {
    CHUNK_CACHE_HITS.store(0, Ordering::Relaxed);
    CHUNK_CACHE_MISSES.store(0, Ordering::Relaxed);
    SOURCE_REOPENS.store(0, Ordering::Relaxed);
}

/// ソースの状態です（読み込み済み／変更済み／エラー）。
///
/// 計画正本「各ソースの状態（読み込み済み／変更済み(検知内容)／エラー）を
/// 保持する」に対応します。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceStatus {
    /// 読み込み済みで利用可能。
    Loaded,
    /// 変更を検知して停止した（`LOG-023`）。手動で `close_source` してから
    /// 再度 `insert_source` するまで再利用しません。
    Changed(ChangeKind),
    /// キャンセル要求（P04-6 の `CancellationToken`）により、読み込みが
    /// チャンク境界で途中終了した（P06-2）。`Changed` と異なり索引・項目は
    /// 無効化されません（読み込み済み範囲は保持されます）。
    CancelledPartial,
    /// 整合性の再確認自体が失敗した（削除・共有違反以外の理由）。既存の表示は
    /// 無効化せず維持します（一時的な事象の可能性があるため。`ERR-001`）。
    Error(String),
    /// 共有を許可しない方法で開かれていて読み取れない（`LOG-027`）。
    /// `Error` と同じく既存の表示は無効化せず維持し、再試行できます。
    SharingViolation,
    /// P08-3 由来の状態です（モジュール doc コメント「しきい値到達時の解放の
    /// 単純化」参照）。P08-5 以降、`evict_inactive_sources` はこの状態を
    /// 作りません（互換性のために型は残しています）。
    Evicted,
}

/// [`SourceStatus::Changed`] の内訳です（`LOG-023`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// 縮小（切り詰め）を検知した。
    Shrunk,
    /// 別ファイルへの置換（識別子変化）を検知した。
    Replaced,
    /// 削除を検知した。
    Deleted,
}

/// [`DisplaySetRegistry::list_sources`] が返す1件の要約です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSummary {
    pub source_id: u32,
    pub display_set_id: u32,
    pub label: String,
    pub status: SourceStatus,
    /// 登録時に観測したサイズ（`snapshot_end`。バイト）。
    pub size_bytes: u64,
    /// 末尾が未確定行（`LOG-026`）で終わっているか。
    pub has_unconfirmed_trailing_line: bool,
    /// 再読み込みが上限超過で拒否され、旧スナップショットの表示を維持した
    /// まま「更新未反映」になっているか（`LOG-028`、ADR-0007）。
    pub update_pending: bool,
}

/// 登録済み1ソースの内部記録です。
///
/// `path` はソース内部の来歴管理・スナップショット再確認・**オンデマンド
/// 読み出しの再オープン**（P08-5）のためだけに保持し、`list_sources` の
/// 戻り値（[`SourceSummary`]）には含めません（`SEC-012`）。
#[derive(Debug)]
struct SourceRecord {
    display_set_id: u32,
    path: PathBuf,
    label: String,
    snapshot: FileSnapshot,
    reservation: SourceReservation,
    status: SourceStatus,
    has_unconfirmed_trailing_line: bool,
    update_pending: bool,
    /// ADR-0008 の順序規則における `source_ordinal`（表示集合の世代ごとに
    /// 不変）。`insert_source` の呼び出し順（= 単調増加のカウンター
    /// `DisplaySetRegistry::next_source_ordinal`）でそのまま割り当てます。
    /// これは ADR-0008 の「後から追加は末尾」を常に満たします。「設定由来は
    /// 記載順」「同一操作でのアドホック複数選択はパスの2キー整列」は、
    /// 呼び出し側（`src-tauri`）が対応する順で `insert_source` を呼ぶことで
    /// 満たされます（`crate::ordering` の doc コメント参照。複数選択 UI は
    /// 本フェーズの対象外）。項目の安定識別子（`ItemId`）はこの値に依存しま
    /// せん（ADR-0008「項目の安定した識別子は source_ordinal に依存させない」）。
    source_ordinal: u32,
    /// このソースで確定した日時書式（`timestamp_display` 再構成用）。
    /// 生表示・日時なしのソースは `None`。
    datetime_format: Option<LogDateTimeFormat>,
    /// このソースで確定した文字コード（P08-5）。オンデマンド
    /// 読み出し時、登録時と同じデコード結果を再現するために保持します
    /// （再判定はしません。ファイルが変化していない前提のため、登録時の
    /// 判定を信頼します）。
    selected_encoding: SelectedEncoding,
    /// このソースのために、行数に比例して常駐する構造（索引本体・行番号配列・
    /// 項目列）へ**実確保として振り替えた**バイト数の累計です。
    ///
    /// 項目数 × [`crate::item::RESIDENT_BYTES_PER_ITEM`] とは一致しません。
    /// 事前確保（[`ensure_resident_capacity`]）で確保した余剰容量を含むため
    /// です。`crate::loader::LoadSummary::reserved_bytes` が示すのはこの値で
    /// あり、会計上の実確保量（`allocated_bytes`）と突き合わせられる量として
    /// 意味を持ちます。
    resident_committed_bytes: usize,
}

/// `DisplaySetRegistry::reload_context` が返す、明示的な再読み込み
/// （`LOG-028`）に必要な情報です（`crate::loader::reload_source` が使う
/// 内部部品）。
pub(crate) struct SourceReloadContext {
    pub path: PathBuf,
    pub label: String,
    pub old_snapshot: FileSnapshot,
    pub old_reservation: SourceReservation,
}

/// 表示集合を新規登録・再構築した結果です。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplaySetHandle {
    pub display_set_id: u32,
    /// 登録したソースの ID をそのまま返します。
    pub source_id: u32,
    pub generation: u64,
    pub total_items: u64,
}

/// [`DisplaySetRegistry::enable_merged_view`] が返す、統合表示集合の識別子・
/// 世代・件数です（P09-1）。単一ソースの [`DisplaySetHandle`] と異なり
/// `source_id` は持ちません（複数ソースを横断するため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergedViewHandle {
    pub display_set_id: u32,
    pub generation: u64,
    pub total_items: u64,
}

/// 表示集合の現在の世代と件数です（[`DisplaySetRegistry::display_set_state`]）。
///
/// 単独ソースの表示集合と統合表示集合（P09-1）を、呼び出し側が区別せずに
/// 扱えるようにするための共通形です。[`DisplaySetHandle`]・
/// [`MergedViewHandle`] と違い、`source_id`（単独ソースにしかない）も
/// `display_set_id`（呼び出し側が既に持っている）も含みません。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplaySetState {
    pub generation: u64,
    pub total_items: u64,
}

/// 統合表示集合（P09-1）の内部記録です。
///
/// `order` は各項目を指す [`ItemId`]（`source_id` + `seq`）の並びだけを保持し、
/// 本文・各ソースの索引そのものは複製しません（`crate::merge` のモジュール
/// doc コメント「複製しない設計」参照）。範囲取得時は、この並びが指す項目ごとに
/// 参加ソース自身の単独表示集合（`DisplaySetRegistry::display_sets`）から
/// オンデマンドで索引情報・本文を読み出します。
#[derive(Debug)]
struct MergedViewRecord {
    display_set_id: u32,
    generation: u64,
    order: Vec<ItemId>,
}

/// 表示集合を再構築した結果です（`LOG-023`・`LOG-028` の下地）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebuildOutcome {
    pub generation: u64,
    pub total_items: u64,
}

/// レジストリの操作で発生し得る失敗です。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchRangeError {
    /// 指定した `display_set_id` の表示集合が存在しない
    /// （未登録、または破棄済み）。
    UnknownDisplaySet,
    /// 世代不一致（[`RangeFetchError::GenerationMismatch`] を参照）。
    GenerationMismatch { expected: u64, current: u64 },
}

impl std::fmt::Display for FetchRangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchRangeError::UnknownDisplaySet => {
                write!(f, "指定された表示集合が見つかりません。")
            }
            FetchRangeError::GenerationMismatch { expected, current } => {
                RangeFetchError::GenerationMismatch {
                    expected: *expected,
                    current: *current,
                }
                .fmt(f)
            }
        }
    }
}

impl std::error::Error for FetchRangeError {}

impl From<RangeFetchError> for FetchRangeError {
    fn from(error: RangeFetchError) -> Self {
        match error {
            RangeFetchError::GenerationMismatch { expected, current } => {
                FetchRangeError::GenerationMismatch { expected, current }
            }
        }
    }
}

/// [`DisplaySetRegistry::grow_source_items`] の失敗です（P08-5）。
#[derive(Debug, Clone, Copy)]
pub(crate) enum IndexGrowError {
    /// 未登録の `source_id`。
    UnknownSource,
    /// 索引の伸長分のメモリ予約が拒否された（`PERF-008`）。
    ReservationRejected(hakutaku_memory_accounting::ReservationRejected),
}

/// 表示集合を丸ごと作り直す経路（[`DisplaySetRegistry::commit_reload`]・
/// [`DisplaySetRegistry::commit_restore`]）向けに、索引と項目列を構築します。
///
/// 戻り値は `(索引, 項目列, 会計へ振り替えたバイト数)` です。
///
/// これらの経路では `pending_items` が既に完成しているため、読み込み中のような
/// 外挿ではなく [`CapacityEstimate::Exact`] で**ちょうどの容量**を先に確保でき
/// ます。ここで倍々成長に任せると、2000万件規模では最終容量が
/// 必要量の1.0〜2.0倍に振れ、再確保のたびに全量コピーが発生します。
fn build_rebuilt_source_containers(
    source_id: u32,
    pending_items: &[PendingItem],
) -> Result<(IndexedText, Vec<Item>, usize), hakutaku_memory_accounting::ReservationRejected> {
    let mut text = IndexedText::new();
    let mut items = Vec::new();
    let mut committed_bytes = ensure_resident_capacity(
        &mut text,
        &mut items,
        CapacityEstimate::Exact(pending_items.len()),
    );
    committed_bytes +=
        build_items_from_pending_into(source_id, 0, pending_items, &mut text, &mut items)?;
    Ok((text, items, committed_bytes))
}

/// 表示集合レジストリです。`display_set_id`・`source_id` はレジストリ内で一意な
/// 連番で払い出します。
#[derive(Debug, Default)]
pub struct DisplaySetRegistry {
    next_source_id: u32,
    next_display_set_id: u32,
    /// ADR-0008 の `source_ordinal` を払い出す単調増加カウンター
    /// （`SourceRecord::source_ordinal` の doc コメント参照）。
    next_source_ordinal: u32,
    display_sets: HashMap<u32, DisplaySet>,
    sources: HashMap<u32, SourceRecord>,
    /// P08-3: 現在アクティブ（表示中）のソース。`src-tauri` が
    /// `set_active_source` 経由で伝えます。
    active_source_id: Option<u32>,
    /// デコード済みチャンクの有界キャッシュ（P08-5）。
    decoded_cache: DecodedChunkCache,
    /// 統合表示集合（P09-1）。ON のときだけ `Some`。
    merged_view: Option<MergedViewRecord>,
    /// 本文のオンデマンド読み出しが成立せず、既定値（空の本文）で応答した
    /// 項目数の累計です（[`Self::hydrate_fallback_items`]）。
    hydrate_fallback_items: u64,
}

impl DisplaySetRegistry {
    #[must_use]
    pub fn new() -> Self {
        DisplaySetRegistry {
            next_source_id: 0,
            next_display_set_id: 0,
            next_source_ordinal: 0,
            display_sets: HashMap::new(),
            sources: HashMap::new(),
            active_source_id: None,
            decoded_cache: DecodedChunkCache::new(),
            merged_view: None,
            hydrate_fallback_items: 0,
        }
    }

    /// 本文のオンデマンド読み出しが成立せず、既定値（空の本文）で応答した
    /// 項目数の累計です（プロセス起動からの単調増加）。
    ///
    /// [`Self::fetch_range`] は、削除・置換・共有違反・読み出し失敗を検知しても
    /// **その応答1件の中では他の項目・他のソースの取得を継続します**
    /// （`ERR-001`。モジュール doc コメント参照）。表示は次回以降の範囲取得か
    /// `SourceStatus` の変化で追いつけますが、**クリップボードコピーでは
    /// 空になった本文がそのまま利用者の手元へ渡り、取り消せません**。
    ///
    /// そこで `crate::copy::assemble_copy` は、範囲取得の前後でこの値を比べ、
    /// 増えていればコピー全体を失敗させます（`COPY-005` の「部分コピーを黙って
    /// 行わない」）。呼び出しの前後で差を取る用途にだけ使ってください
    /// （絶対値そのものには意味がありません）。
    #[must_use]
    pub fn hydrate_fallback_items(&self) -> u64 {
        self.hydrate_fallback_items
    }

    /// 既存の表示集合を再構築し、世代を1つ進めます（`LOG-023`・`LOG-028` の
    /// 下地。この API を呼び出す Tauri コマンドは未実装で、コア層の契約として
    /// テストで検証するに留めます）。
    ///
    /// `display_set_id` が未登録の場合 `None` を返します。
    pub fn rebuild(
        &mut self,
        display_set_id: u32,
        sources: Vec<SourceInfo>,
        items: Vec<Item>,
        texts: HashMap<u32, IndexedText>,
    ) -> Option<RebuildOutcome> {
        let display_set = self.display_sets.get_mut(&display_set_id)?;
        *display_set = display_set.rebuild(sources, items, texts);
        let outcome = RebuildOutcome {
            generation: display_set.generation(),
            total_items: display_set.total_items(),
        };

        // 再構築は項目の切れ目（継続行のまとめ方）を変え得るため、古い世代の
        // 本文がデコード済みチャンクキャッシュに残っていると、次の範囲取得が
        // それを再利用してしまう。再読み込み経路（[`Self::commit_reload`]）と
        // 同じく、ここでも該当ソースのキャッシュを捨てる。
        if let Some(source_id) = self.source_id_for_display_set(display_set_id) {
            self.decoded_cache.invalidate_source(source_id);
        }

        Some(outcome)
    }

    /// 複数ソースの1つとしてソースを登録します（P06）。
    ///
    /// `path`・`snapshot`・`reservation`・`selected_encoding` は、以後の変更
    /// 検知（[`Self::refresh_source`]）・close（[`Self::close_source`]）・
    /// オンデマンド読み出し（[`Self::fetch_range`]、P08-5）のために保持します。
    /// `datetime_format` は `timestamp_display` 再構成用です（生表示・日時
    /// なしのソースは `None`）。
    ///
    /// 索引の伸長分のメモリ予約（`PERF-008`）が拒否された場合、`Err` を返し
    /// `registry` の状態を変更しません（P08-5、[`crate::item::
    /// build_items_from_pending_into`] のドキュメント参照）。
    ///
    /// `estimated_total_items` は、このソースが最終的に持つ論理項目数の見積もり
    /// です。索引・項目列の容量をあらかじめ確保して倍々成長を
    /// 避けるために使います。**見積もりが外れても登録の成否は変わりません**
    /// （過小なら従来どおり伸長し、事前確保の予約が拒否されても事前確保を
    /// 諦めるだけです。[`ensure_resident_capacity`] の doc コメント参照）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_source(
        &mut self,
        path: PathBuf,
        label: String,
        pending_items: &[PendingItem],
        snapshot: FileSnapshot,
        reservation: SourceReservation,
        has_unconfirmed_trailing_line: bool,
        datetime_format: Option<LogDateTimeFormat>,
        selected_encoding: SelectedEncoding,
        estimated_total_items: CapacityEstimate,
    ) -> Result<DisplaySetHandle, hakutaku_memory_accounting::ReservationRejected> {
        let source_id = self.next_source_id;
        let display_set_id = self.next_display_set_id;

        // 先に容量を確保してから追記する。順序が重要で、逆にすると
        // 最初のバッチ分だけが倍々成長の起点になってしまい、事前確保の意味が
        // 薄れる（`ensure_resident_capacity` は既存の容量を超える分だけを
        // 予約・確保するため、空の状態で呼ぶのが最も無駄がない）。
        let mut text = IndexedText::new();
        let mut items = Vec::new();
        let mut resident_committed_bytes =
            ensure_resident_capacity(&mut text, &mut items, estimated_total_items);
        resident_committed_bytes +=
            build_items_from_pending_into(source_id, 0, pending_items, &mut text, &mut items)?;

        self.next_source_id += 1;
        self.next_display_set_id += 1;

        let sources_info = vec![SourceInfo {
            source_id,
            label: label.clone(),
        }];
        let mut texts = HashMap::new();
        texts.insert(source_id, text);
        let display_set = DisplaySet::new(sources_info, items, texts);
        let generation = display_set.generation();
        let total_items = display_set.total_items();

        let source_ordinal = self.next_source_ordinal;
        self.next_source_ordinal += 1;

        self.display_sets.insert(display_set_id, display_set);
        self.sources.insert(
            source_id,
            SourceRecord {
                display_set_id,
                path,
                label,
                snapshot,
                reservation,
                status: SourceStatus::Loaded,
                has_unconfirmed_trailing_line,
                update_pending: false,
                source_ordinal,
                datetime_format,
                selected_encoding,
                resident_committed_bytes,
            },
        );

        // P09-1: 統合表示集合が有効なら、新しいソースを含めて再構築する
        // （「対象の追加を再読み込みなしで行える」構成。世代を1つ進める）。
        self.sync_merged_view();

        Ok(DisplaySetHandle {
            display_set_id,
            source_id,
            generation,
            total_items,
        })
    }

    /// [`insert_source`] で登録済みの全ソースを列挙します（`source_id` 昇順。
    /// 決定的な順序にすることで、呼び出し側の表示・テストを安定させます）。
    ///
    /// [`insert_source`]: Self::insert_source
    #[must_use]
    pub fn list_sources(&self) -> Vec<SourceSummary> {
        let mut summaries: Vec<SourceSummary> = self
            .sources
            .iter()
            .map(|(source_id, record)| SourceSummary {
                source_id: *source_id,
                display_set_id: record.display_set_id,
                label: record.label.clone(),
                status: record.status.clone(),
                size_bytes: record.snapshot.snapshot_end,
                has_unconfirmed_trailing_line: record.has_unconfirmed_trailing_line,
                update_pending: record.update_pending,
            })
            .collect();
        summaries.sort_by_key(|summary| summary.source_id);
        summaries
    }

    /// 指定ソースの現在の状態を返します（未登録なら `None`）。
    #[must_use]
    pub fn source_status(&self, source_id: u32) -> Option<SourceStatus> {
        self.sources
            .get(&source_id)
            .map(|record| record.status.clone())
    }

    /// `display_set_id` に対応する `source_id` を返します（未登録なら `None`）。
    ///
    /// **統合表示集合（P09-1）は複数ソースを横断するため、常に `None` になり
    /// ます。** 「単独ソースの表示集合か」を判定する用途にはそのまま使えます
    /// が、表示集合の世代・件数を知りたいだけの用途には
    /// [`Self::display_set_state`] を使ってください（統合表示集合を
    /// 「存在しない表示集合」として扱ってしまわないため。Issue #37）。
    #[must_use]
    pub fn source_id_for_display_set(&self, display_set_id: u32) -> Option<u32> {
        self.sources
            .iter()
            .find(|(_, record)| record.display_set_id == display_set_id)
            .map(|(source_id, _)| *source_id)
    }

    /// `display_set_id` の現在の世代・件数を返します（未登録なら `None`）。
    ///
    /// 単独ソースの表示集合と統合表示集合（P09-1）のどちらでも同じ意味の値を
    /// 返します。世代・件数の出所は [`Self::fetch_range`] が範囲取得に使うもの
    /// と同一であり、同じ借用の中で読めば両者は必ず一致します
    /// （`crate::copy::assemble_copy` が、範囲取得の前に上限判定へ使います）。
    #[must_use]
    pub fn display_set_state(&self, display_set_id: u32) -> Option<DisplaySetState> {
        // 統合表示集合は `display_sets` には入っておらず（`merged_view` が単独で
        // 保持する）、`fetch_range` も専用の分岐で扱う。判定の順序と条件を
        // `fetch_range` と揃え、同じ ID が両者で別の表示集合を指すことがない
        // ようにする。
        if let Some(merged) = &self.merged_view {
            if merged.display_set_id == display_set_id {
                return Some(DisplaySetState {
                    generation: merged.generation,
                    total_items: merged.order.len() as u64,
                });
            }
        }
        let display_set = self.display_sets.get(&display_set_id)?;
        Some(DisplaySetState {
            generation: display_set.generation(),
            total_items: display_set.total_items(),
        })
    }

    /// 現在アクティブ（表示中）のソースを設定します（P08-3）。
    pub fn set_active_source(&mut self, source_id: Option<u32>) {
        self.active_source_id = source_id;
    }

    /// 現在アクティブなソースを返します。
    #[must_use]
    pub fn active_source_id(&self) -> Option<u32> {
        self.active_source_id
    }

    /// ソースを閉じます。予約サイズを `budget` へ返却して上限計算から除外し、
    /// 対応する表示集合も破棄します（close 後の `fetch_range` は
    /// [`FetchRangeError::UnknownDisplaySet`] になります）。
    ///
    /// 未登録の `source_id` の場合 `None` を返し、何も変更しません。
    pub fn close_source(
        &mut self,
        source_id: u32,
        budget: &crate::budget::SourceBudget,
    ) -> Option<()> {
        let record = self.sources.remove(&source_id)?;
        self.display_sets.remove(&record.display_set_id);
        self.decoded_cache.invalidate_source(source_id);
        budget.release(record.reservation);
        if self.active_source_id == Some(source_id) {
            self.active_source_id = None;
        }
        // P09-1: 統合表示集合が有効なら、閉じたソースを除いて再構築する
        // （「対象の除外を再読み込みなしで行える」構成。世代を1つ進める）。
        self.sync_merged_view();
        Some(())
    }

    /// 読み込み途中の解析済み範囲から表示集合を伸長します（P06-2）。
    ///
    /// 索引の伸長分のメモリ予約が拒否された場合 [`IndexGrowError::
    /// ReservationRejected`] を返し、既存の項目・索引は変更しません。
    ///
    /// `estimated_total_items` の意味は [`Self::insert_source`] と同じです。
    /// 事前確保済みの容量に収まっている間は、この呼び出しで
    /// 新たな確保も予約も発生しません。容量を使い切ったときだけ、**その時点
    /// までの実測で更新された見積もり**まで容量を広げ直します。
    ///
    /// **統合表示集合（P09-1）はここでは同期しません。** 伸長は
    /// 読み込み中に何度も起こるため、そのたびに全体再マージを走らせないための
    /// 意図的な選択です。読み込みの完了時に呼び出し側が
    /// [`Self::sync_merged_view_after_load`] を1回呼ぶことで揃います（理由は
    /// そちらの doc コメント参照）。
    pub(crate) fn grow_source_items(
        &mut self,
        source_id: u32,
        additional_pending: &[PendingItem],
        estimated_total_items: CapacityEstimate,
    ) -> Result<u64, IndexGrowError> {
        let display_set_id = self
            .sources
            .get(&source_id)
            .ok_or(IndexGrowError::UnknownSource)?
            .display_set_id;
        let display_set = self
            .display_sets
            .get_mut(&display_set_id)
            .ok_or(IndexGrowError::UnknownSource)?;
        let start_seq = display_set.total_items();
        let (items, text) = display_set
            .items_and_text_mut(source_id)
            .ok_or(IndexGrowError::UnknownSource)?;
        let mut committed_bytes = ensure_resident_capacity(text, items, estimated_total_items);
        committed_bytes +=
            build_items_from_pending_into(source_id, start_seq, additional_pending, text, items)
                .map_err(IndexGrowError::ReservationRejected)?;
        let total_items = display_set.total_items();

        // 会計値の累計はソース記録側で持つ（`SourceRecord::
        // resident_committed_bytes`）。直前で存在を確認済みだが、防御的に
        // `if let` で受ける（存在しなければ会計値を積まないだけで、表示集合の
        // 伸長そのものは成立している）。
        if let Some(record) = self.sources.get_mut(&source_id) {
            record.resident_committed_bytes += committed_bytes;
        }
        Ok(total_items)
    }

    /// 指定ソースが、行数に比例して常駐する構造へ実確保として振り替えた
    /// バイト数の累計です（`SourceRecord::resident_committed_bytes`
    /// の doc コメント参照）。未登録の `source_id` では 0 を返します。
    #[must_use]
    pub(crate) fn resident_committed_bytes(&self, source_id: u32) -> usize {
        self.sources
            .get(&source_id)
            .map_or(0, |record| record.resident_committed_bytes)
    }

    /// キャンセル要求（P04-6 の `CancellationToken`）により、読み込みが
    /// チャンク境界で部分読み込みのまま停止したことを記録します（P06-2）。
    ///
    /// 未登録の `source_id` の場合 `None` を返します。
    pub(crate) fn mark_cancelled_partial(&mut self, source_id: u32) -> Option<()> {
        let record = self.sources.get_mut(&source_id)?;
        record.status = SourceStatus::CancelledPartial;
        Some(())
    }

    /// 読み込み完了後に、末尾が未確定行（`LOG-026`）だったかを反映します。
    ///
    /// 未登録の `source_id` の場合 `None` を返します。
    pub(crate) fn set_unconfirmed_trailing_line(
        &mut self,
        source_id: u32,
        has_unconfirmed_trailing_line: bool,
    ) -> Option<()> {
        let record = self.sources.get_mut(&source_id)?;
        record.has_unconfirmed_trailing_line = has_unconfirmed_trailing_line;
        Some(())
    }

    /// 読み込み中に検知した変更を即座に反映します（`LOG-023`）。
    ///
    /// 未登録の `source_id` の場合 `None` を返します。
    pub(crate) fn mark_changed_now(&mut self, source_id: u32, kind: ChangeKind) -> Option<()> {
        let record = self.sources.get_mut(&source_id)?;
        record.status = SourceStatus::Changed(kind);
        record.update_pending = false;
        let display_set_id = record.display_set_id;
        let label = record.label.clone();
        self.decoded_cache.invalidate_source(source_id);

        if let Some(display_set) = self.display_sets.get_mut(&display_set_id) {
            let sources_info = vec![SourceInfo { source_id, label }];
            // LOG-023: 索引を無効化する。世代を進めたうえで項目を空にし、
            // 従来の索引を有効扱いで維持しない。
            *display_set = display_set.rebuild(sources_info, Vec::new(), HashMap::new());
        }
        // P09-1: 統合表示集合が有効なら、参加ソースの無効化に合わせて世代を
        // 進める（計画正本「参加ソースのいずれかが無効化されたら統合表示集合の
        // 世代も進める」）。
        self.sync_merged_view();
        Some(())
    }

    /// 読み込み中に検知した、削除以外の一時的な失敗を記録します（`ERR-001`）。
    ///
    /// 未登録の `source_id` の場合 `None` を返します。
    pub(crate) fn mark_error_now(&mut self, source_id: u32, message: String) -> Option<()> {
        let record = self.sources.get_mut(&source_id)?;
        record.status = SourceStatus::Error(message);
        record.update_pending = false;
        Some(())
    }

    /// 共有違反（`LOG-027`）により読み取れなかったことを記録します。
    /// `Error` と同じく既存の項目・世代は変更しません（`ERR-001`）。
    ///
    /// 未登録の `source_id` の場合 `None` を返します。
    pub(crate) fn mark_sharing_violation_now(&mut self, source_id: u32) -> Option<()> {
        let record = self.sources.get_mut(&source_id)?;
        record.status = SourceStatus::SharingViolation;
        record.update_pending = false;
        Some(())
    }

    /// 再読み込みが上限超過で拒否され、旧スナップショットの表示を維持した
    /// まま「更新未反映」になったことを記録します（`LOG-028`、ADR-0007）。
    ///
    /// 未登録の `source_id` の場合 `None` を返します。
    pub(crate) fn mark_update_pending(&mut self, source_id: u32) -> Option<()> {
        let record = self.sources.get_mut(&source_id)?;
        record.update_pending = true;
        Some(())
    }

    /// 「更新未反映」フラグを解除します。
    ///
    /// 未登録の `source_id` の場合 `None` を返します。
    pub(crate) fn clear_update_pending(&mut self, source_id: u32) -> Option<()> {
        let record = self.sources.get_mut(&source_id)?;
        record.update_pending = false;
        Some(())
    }

    /// 再読み込み対象の情報（パス・来歴ラベル・旧スナップショット・旧予約）を
    /// 取得します。
    ///
    /// 未登録の `source_id` の場合 `None` を返します。
    #[must_use]
    pub(crate) fn reload_context(&self, source_id: u32) -> Option<SourceReloadContext> {
        let record = self.sources.get(&source_id)?;
        Some(SourceReloadContext {
            path: record.path.clone(),
            label: record.label.clone(),
            old_snapshot: record.snapshot,
            old_reservation: record.reservation,
        })
    }

    /// 明示的な再読み込み（`LOG-028`）が上限内で成功した内容を確定させます。
    /// 表示集合を新しい項目列で再構築し（世代を1つ進める）、ソース記録の
    /// スナップショット・予約・状態・末尾未確定フラグ・更新未反映フラグ・
    /// 日時書式・文字コードを最新化します。
    ///
    /// 索引の伸長分のメモリ予約が拒否された場合 `Err` を返し、`registry` の
    /// 状態を変更しません（P08-5）。未登録の `source_id` の場合
    /// `Ok(None)` を返します。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_reload(
        &mut self,
        source_id: u32,
        pending_items: &[PendingItem],
        new_snapshot: FileSnapshot,
        new_reservation: SourceReservation,
        has_unconfirmed_trailing_line: bool,
        datetime_format: Option<LogDateTimeFormat>,
        selected_encoding: SelectedEncoding,
    ) -> Result<Option<RebuildOutcome>, hakutaku_memory_accounting::ReservationRejected> {
        let Some(record) = self.sources.get(&source_id) else {
            return Ok(None);
        };
        let display_set_id = record.display_set_id;
        let label = record.label.clone();

        // 再読み込みは `pending_items` が完成しているため、推定では
        // なく確定値で事前確保できる（読み込み中の外挿と違い、過大にも過小にも
        // ならない）。
        let (text, items, resident_committed_bytes) =
            build_rebuilt_source_containers(source_id, pending_items)?;
        let sources_info = vec![SourceInfo { source_id, label }];
        let mut texts = HashMap::new();
        texts.insert(source_id, text);

        let Some(display_set) = self.display_sets.get_mut(&display_set_id) else {
            return Ok(None);
        };
        *display_set = display_set.rebuild(sources_info, items, texts);
        let generation = display_set.generation();
        let total_items = display_set.total_items();
        self.decoded_cache.invalidate_source(source_id);

        let record = self
            .sources
            .get_mut(&source_id)
            .expect("直前の get で存在確認済み");
        record.snapshot = new_snapshot;
        record.reservation = new_reservation;
        record.status = SourceStatus::Loaded;
        // 旧世代の索引・項目列は rebuild で丸ごと置き換わって解放されるため、
        // 累計ではなく新しい世代の分で置き換える。
        record.resident_committed_bytes = resident_committed_bytes;
        record.has_unconfirmed_trailing_line = has_unconfirmed_trailing_line;
        record.update_pending = false;
        record.datetime_format = datetime_format;
        record.selected_encoding = selected_encoding;

        // P09-1: 統合表示集合が有効なら、再読み込み後の内容へ追従させる
        // （LOG-028 の反映と同じ考え方で世代を進める）。
        self.sync_merged_view();

        Ok(Some(RebuildOutcome {
            generation,
            total_items,
        }))
    }

    /// [`SourceStatus::Evicted`] ソースを、スナップショット整合性確認後に
    /// 再読み込みした内容で復元します（`crate::loader::
    /// restore_evicted_source` が使う内部部品）。**世代は必ず1つ進めます**
    /// （同一内容へ復元しても識別子の安定性より安全側を優先する設計判断）。
    ///
    /// 索引の伸長分のメモリ予約が拒否された場合 `Err` を返します。未登録の
    /// `source_id` の場合 `Ok(None)` を返します。
    pub(crate) fn commit_restore(
        &mut self,
        source_id: u32,
        pending_items: &[PendingItem],
        new_snapshot: FileSnapshot,
        has_unconfirmed_trailing_line: bool,
    ) -> Result<Option<RebuildOutcome>, hakutaku_memory_accounting::ReservationRejected> {
        let Some(record) = self.sources.get(&source_id) else {
            return Ok(None);
        };
        let display_set_id = record.display_set_id;
        let label = record.label.clone();

        // 再読み込み（`commit_reload`）と同じく、退避復元も `pending_items` が
        // 完成しているため確定値で事前確保できる。
        let (text, items, resident_committed_bytes) =
            build_rebuilt_source_containers(source_id, pending_items)?;
        let sources_info = vec![SourceInfo { source_id, label }];
        let mut texts = HashMap::new();
        texts.insert(source_id, text);

        let Some(display_set) = self.display_sets.get_mut(&display_set_id) else {
            return Ok(None);
        };
        *display_set = display_set.rebuild(sources_info, items, texts);
        let generation = display_set.generation();
        let total_items = display_set.total_items();
        self.decoded_cache.invalidate_source(source_id);

        let record = self
            .sources
            .get_mut(&source_id)
            .expect("直前の get で存在確認済み");
        record.snapshot = new_snapshot;
        record.status = SourceStatus::Loaded;
        record.resident_committed_bytes = resident_committed_bytes;
        record.has_unconfirmed_trailing_line = has_unconfirmed_trailing_line;

        // P09-1: 統合表示集合が有効なら、復元後の内容へ追従させる。
        self.sync_merged_view();

        Ok(Some(RebuildOutcome {
            generation,
            total_items,
        }))
    }

    /// 現在の表示集合の状態から [`DisplaySetHandle`] を再構築します。
    ///
    /// 未登録の `source_id` の場合 `None` を返します。
    #[must_use]
    pub(crate) fn current_handle(&self, source_id: u32) -> Option<DisplaySetHandle> {
        let record = self.sources.get(&source_id)?;
        let display_set = self.display_sets.get(&record.display_set_id)?;
        Some(DisplaySetHandle {
            display_set_id: record.display_set_id,
            source_id,
            generation: display_set.generation(),
            total_items: display_set.total_items(),
        })
    }

    /// ソースの整合性を再確認します（`LOG-023`）。
    ///
    /// 未登録の `source_id` の場合 `None` を返します。
    pub fn refresh_source(&mut self, source_id: u32) -> Option<SourceStatus> {
        let record = self.sources.get(&source_id)?;
        if let SourceStatus::Changed(_) = &record.status {
            return Some(record.status.clone());
        }

        let verdict = hakutaku_data_source::verify_snapshot(&record.path, &record.snapshot);
        let change_kind = match verdict {
            Ok(SnapshotVerdict::Unchanged) | Ok(SnapshotVerdict::Appended { .. }) => None,
            Ok(SnapshotVerdict::Shrunk { .. }) => Some(ChangeKind::Shrunk),
            Ok(SnapshotVerdict::Replaced) => Some(ChangeKind::Replaced),
            Ok(SnapshotVerdict::Deleted) => Some(ChangeKind::Deleted),
            Err(error) => {
                let record = self
                    .sources
                    .get_mut(&source_id)
                    .expect("直前の get で存在確認済み");
                record.status = if error.is_sharing_violation() {
                    SourceStatus::SharingViolation
                } else {
                    SourceStatus::Error(error.to_string())
                };
                record.update_pending = false;
                return Some(record.status.clone());
            }
        };

        match change_kind {
            Some(kind) => {
                self.mark_changed_now(source_id, kind)
                    .expect("直前の get で存在確認済み");
            }
            None => {
                let record = self
                    .sources
                    .get_mut(&source_id)
                    .expect("直前の get で存在確認済み");
                record.status = SourceStatus::Loaded;
            }
        }

        self.source_status(source_id)
    }

    /// しきい値到達時、非アクティブな [`SourceStatus::Loaded`] ソースの
    /// デコード済みチャンクキャッシュをクリアします（P08-5、モジュール
    /// doc コメント「しきい値到達時の解放の単純化」参照）。
    ///
    /// P08-1〜P08-3 と異なり、索引・項目・世代・`SourceStatus` は一切
    /// 変更しません（索引はもはや解放する意味のある大きさではないため）。
    ///
    /// # 呼び出しタイミング（デッドロック回避、遅延方式）
    ///
    /// P08-3 から変わっていません。`register_release_handler` へ登録する
    /// クロージャは `Registry` のロックを取らずフラグを立てるだけにし、実際の
    /// 解放（このメソッドの呼び出し）は Tauri コマンド処理の入口で行います
    /// （`src-tauri::log_view::drain_pending_eviction` を参照）。
    ///
    /// 戻り値はキャッシュをクリアした `source_id` の一覧です（診断ログ用）。
    pub fn evict_inactive_sources(&mut self) -> Vec<u32> {
        let active = self.active_source_id;
        let candidates: Vec<u32> = self
            .sources
            .iter()
            .filter(|(id, record)| {
                Some(**id) != active && matches!(record.status, SourceStatus::Loaded)
            })
            .map(|(id, _)| *id)
            .collect();

        for source_id in &candidates {
            self.decoded_cache.invalidate_source(*source_id);
        }
        candidates
    }

    // --- 統合表示集合（P09-1） ---

    /// 現在開いている全ソースを横断する統合表示集合を構築し、ON にします
    /// （`LOG-007`〜`LOG-008`）。既に ON だった場合は、その時点の内容で作り
    /// 直します（世代は1から振り直します。既存の統合表示集合の
    /// `display_set_id` はこの呼び出し以降 [`FetchRangeError::
    /// UnknownDisplaySet`] になります）。
    ///
    /// **参加ソースの索引の再読み込み・再解析は一切行いません**（計画正本
    /// 「再読み込みなしの切り替え」）。既に登録済みの各ソースの
    /// [`IndexedText`] を読むだけです。
    ///
    /// 参照列（`(source_id, seq)` の並び）の確保が [`hakutaku_memory_accounting::
    /// global_budget`] の予約（`PERF-008`）で拒否された場合、統合表示集合を
    /// 開始せず `Err` を返します。
    pub fn enable_merged_view(
        &mut self,
    ) -> Result<MergedViewHandle, hakutaku_memory_accounting::ReservationRejected> {
        let order = self.compute_merged_order()?;
        let display_set_id = self.next_display_set_id;
        self.next_display_set_id += 1;
        let total_items = order.len() as u64;

        self.merged_view = Some(MergedViewRecord {
            display_set_id,
            generation: 1,
            order,
        });

        Ok(MergedViewHandle {
            display_set_id,
            generation: 1,
            total_items,
        })
    }

    /// 統合表示集合を破棄し、OFF にします（`LOG-008`、`LOG-015`）。参加して
    /// いた各ソースの索引・状態には一切触れません（ファイル別タブ表示へ、
    /// 参照対象ファイルを変更せずに戻せます）。
    ///
    /// 既に OFF の場合は何もしません。
    pub fn disable_merged_view(&mut self) {
        self.merged_view = None;
    }

    /// 統合表示集合が現在 ON かどうかを返します。
    #[must_use]
    pub fn is_merged_view_enabled(&self) -> bool {
        self.merged_view.is_some()
    }

    /// 現在登録されている全ソースから、ADR-0008 の順序規則に従う参照列
    /// （`ItemId` の並び）を構築します。予約が拒否された場合は統合表示を
    /// 開始（または継続）せず `Err` を返します（`crate::merge` のモジュール
    /// doc コメント参照）。
    fn compute_merged_order(
        &self,
    ) -> Result<Vec<ItemId>, hakutaku_memory_accounting::ReservationRejected> {
        // texts は一時的に借用するだけで、戻り値（Vec<ItemId>）には複製を
        // 含めない（crate::merge のモジュール doc コメント「複製しない設計」）。
        let mut members: Vec<MergeMember<'_>> = Vec::with_capacity(self.sources.len());
        for (source_id, record) in &self.sources {
            let Some(display_set) = self.display_sets.get(&record.display_set_id) else {
                continue;
            };
            let Some(text) = display_set.text(*source_id) else {
                continue;
            };
            members.push(MergeMember {
                source_id: *source_id,
                source_ordinal: record.source_ordinal,
                entries: text.entries(),
            });
        }

        let total_items: usize = members.iter().map(|member| member.entries.len()).sum();
        let token =
            merge::reserve_merged_order(hakutaku_memory_accounting::global_budget(), total_items)?;

        let order = merge::build_merged_order(&members);

        let actual_bytes = order.len() * std::mem::size_of::<ItemId>();
        let _ = token.mark_allocated(actual_bytes.min(token.remaining_bytes()));

        Ok(order)
    }

    /// 統合表示集合が有効な場合、現在のソース群から作り直し、世代を1つ進めます。
    /// 参加ソースの状態変更（追加・除外・`LOG-023` の無効化・`LOG-028` の
    /// 再読み込み・復元）のたびに呼びます。読み込み途中の伸長
    /// （[`Self::grow_source_items`]）だけは例外で、読み込みの完了時に
    /// [`Self::sync_merged_view_after_load`] からまとめて1回呼びます。
    ///
    /// 予約が拒否された場合（他の読み込みでメモリ予算が逼迫している等）は、
    /// 安全側に倒して統合表示集合そのものを無効化します（OFF）。以後の
    /// `fetch_range` はその `display_set_id` に対して [`FetchRangeError::
    /// UnknownDisplaySet`] を返すため、フロントエンドは通常の「表示集合が
    /// 見つからない」経路で気づけます。
    fn sync_merged_view(&mut self) {
        let Some(merged) = &self.merged_view else {
            return;
        };
        let display_set_id = merged.display_set_id;
        let next_generation = merged.generation + 1;

        match self.compute_merged_order() {
            Ok(order) => {
                self.merged_view = Some(MergedViewRecord {
                    display_set_id,
                    generation: next_generation,
                    order,
                });
            }
            Err(_rejected) => {
                self.merged_view = None;
            }
        }
    }

    /// 1ソース分の読み込み（[`Self::insert_source`] に続く
    /// [`Self::grow_source_items`] の繰り返し）が終わったことを、統合表示集合へ
    /// 反映します。統合表示が OFF のときは何もしません
    /// （[`Self::sync_merged_view`] が即座に戻ります）。
    ///
    /// キャンセルによる部分読み込みの確定（[`SourceStatus::CancelledPartial`]）
    /// も、「その時点までに読み込めた項目で完了した」ものとして同じく同期
    /// します。読み込み済み範囲は保持されるため、統合表示から除く理由がない
    /// ためです。
    ///
    /// # なぜ伸長のたびではなく完了時に1回だけ同期するのか
    ///
    /// 同期の実体は参加ソース全体の再マージ（[`Self::compute_merged_order`]。
    /// 全ソースの全項目を集めて ADR-0008 の順序へ並べ替える）であり、費用は
    /// **そのとき開いている全項目数**に比例します。読み込み中の伸長は
    /// チャンク（既定 [`hakutaku_data_source::DEFAULT_CHUNK_BYTES`]）ごとに
    /// 発生するため、伸長のたびに同期すると1回の読み込みで「バッチ数 × 全項目
    /// 数の並べ替え」を繰り返すことになります。GB 級のファイルではバッチが
    /// 百回単位になり、読み込み時間もレジストリの借用時間も現実的ではなく
    /// なります。
    ///
    /// そこで**読み込み中の統合表示集合は「完了後に揃う」**という挙動を選び
    /// ます。読み込み中に統合表示へ見えるのは最初のバッチまで（`insert_source`
    /// が同期した時点の内容）ですが、完了時にこのメソッドが1回呼ばれることで
    /// 全項目が揃います。読み込み途中の経過は、伸長のたびに `total_items` が
    /// 増える個別ソースの表示集合の側で見えます（`crate::loader::
    /// register_source_with_access`）。
    ///
    /// 順序規則（ADR-0008）は再マージし直しても保たれます。`source_ordinal` は
    /// ソースの登録時に確定して以後変わらず（[`SourceRecord::source_ordinal`]）、
    /// 伸長は既存項目の後ろへ `seq` を増やしながら追記するだけだからです。
    pub(crate) fn sync_merged_view_after_load(&mut self) {
        self.sync_merged_view();
    }

    /// 範囲を取得します（契約に織り込む4点の実装は
    /// [`DisplaySet::fetch_range_index`] に委譲し、その後この関数が本文を
    /// オンデマンドで読み出してデコードします。P08-5）。
    ///
    /// `display_set_id` が統合表示集合（P09-1）のものである場合は
    /// [`Self::fetch_merged_range`] へ委譲します。範囲取得契約
    /// （[`RangeRequest`]／[`RangeResponse`]）はいずれの場合も変わりません。
    pub fn fetch_range(
        &mut self,
        display_set_id: u32,
        request: RangeRequest,
    ) -> Result<RangeResponse, FetchRangeError> {
        if self
            .merged_view
            .as_ref()
            .is_some_and(|merged| merged.display_set_id == display_set_id)
        {
            return self.fetch_merged_range(request);
        }

        let index_response = {
            let display_set = self
                .display_sets
                .get(&display_set_id)
                .ok_or(FetchRangeError::UnknownDisplaySet)?;
            display_set.fetch_range_index(request)?
        };

        let items = self.hydrate_items(index_response.items);

        Ok(RangeResponse {
            generation: index_response.generation,
            total_items: index_response.total_items,
            start: index_response.start,
            items,
            truncated: index_response.truncated,
        })
    }

    /// [`Self::fetch_range`] のうち、統合表示集合向けの実装です（P09-1）。
    ///
    /// 索引レベルの判定（転送上限・打ち切り・世代不一致）は
    /// [`DisplaySet::fetch_range_index`] と同じ規則を、統合表示集合の参照列
    /// （`Vec<ItemId>`）に対して実装します。各項目の索引情報は、その項目が
    /// 属する**単独ソースの `DisplaySet`**（`crate::display_set::DisplaySet::
    /// to_index_ref`）からオンデマンドで取得します（統合表示集合自身は索引を
    /// 複製しません）。
    fn fetch_merged_range(
        &mut self,
        request: RangeRequest,
    ) -> Result<RangeResponse, FetchRangeError> {
        let (generation, total_items, start, index_refs, truncated) = {
            let merged = self
                .merged_view
                .as_ref()
                .ok_or(FetchRangeError::UnknownDisplaySet)?;

            if request.expected_generation != merged.generation {
                return Err(FetchRangeError::GenerationMismatch {
                    expected: request.expected_generation,
                    current: merged.generation,
                });
            }

            let total_items = merged.order.len() as u64;
            let start = request.start.min(total_items);
            let effective_max_items = request.max_items.min(MAX_ITEMS_PER_RESPONSE);

            let mut refs = Vec::new();
            let mut raw_bytes_total: usize = 0;
            for item_id in merged
                .order
                .iter()
                .skip(usize::try_from(start).unwrap_or(usize::MAX))
                .take(effective_max_items as usize)
            {
                // 参照先が消えている場合（通常は起こらない防御的経路。
                // sync_merged_view が状態変更のたびに再構築するため）は
                // その項目を静かに読み飛ばす。
                let Some(index_ref) = self.index_ref_for_merged_item(*item_id) else {
                    continue;
                };
                let item_bytes = index_ref.raw_byte_len as usize;
                if !refs.is_empty()
                    && raw_bytes_total.saturating_add(item_bytes) > MAX_RESPONSE_RAW_BYTES
                {
                    break;
                }
                raw_bytes_total = raw_bytes_total.saturating_add(item_bytes);
                refs.push(index_ref);
            }

            let truncated = start.saturating_add(refs.len() as u64) < total_items;
            (merged.generation, total_items, start, refs, truncated)
        };

        let items = self.hydrate_items(index_refs);

        Ok(RangeResponse {
            generation,
            total_items,
            start,
            items,
            truncated,
        })
    }

    /// 統合表示集合の1項目（`ItemId`）を、その項目が属する単独ソースの
    /// `DisplaySet` から [`IndexItemRef`] へ変換します。参照先のソース・
    /// 表示集合が既に存在しない場合は `None`（防御的経路。呼び出し側は
    /// その項目を読み飛ばします）。
    fn index_ref_for_merged_item(&self, item_id: ItemId) -> Option<IndexItemRef> {
        let record = self.sources.get(&item_id.source_id)?;
        let owning = self.display_sets.get(&record.display_set_id)?;
        // seq は常にそのソースの IndexedText 内での entry_index と一致する
        // （`crate::item::build_items_from_pending_starting_at` が常に seq の
        // 昇順で追記するため。`crate::merge` のモジュール doc コメント参照）。
        let synthetic_item = Item {
            id: item_id,
            entry_index: usize::try_from(item_id.seq).ok()?,
        };
        Some(owning.to_index_ref(&synthetic_item))
    }

    /// [`Self::fetch_range`] が索引レベルの応答から得た項目群の本文を、
    /// ソースごとにまとめてオンデマンドで読み出します（P08-5）。
    fn hydrate_items(&mut self, items: Vec<IndexItemRef>) -> Vec<ItemDto> {
        let mut by_source: HashMap<u32, Vec<usize>> = HashMap::new();
        for (index, item) in items.iter().enumerate() {
            by_source.entry(item.source_id).or_default().push(index);
        }

        let mut results: Vec<Option<ItemDto>> = (0..items.len()).map(|_| None).collect();
        for (source_id, indices) in by_source {
            let group: Vec<IndexItemRef> = indices.iter().map(|&i| items[i].clone()).collect();
            let dtos = self.hydrate_source_group(source_id, group);
            for (local_index, dto) in indices.into_iter().zip(dtos) {
                results[local_index] = Some(dto);
            }
        }

        results
            .into_iter()
            .map(|dto| dto.expect("すべての添字を埋めたはず"))
            .collect()
    }

    /// 1ソース分の項目群の本文をオンデマンドで読み出します（P08-5）。
    ///
    /// キャッシュヒット時はファイルへ一切アクセスしません。ミス時は、
    /// このグループが覆う範囲（最小オフセット〜最大終端）をまとめて1回の
    /// 読み込みで取得し、項目ごとにデコードします。
    fn hydrate_source_group(&mut self, source_id: u32, group: Vec<IndexItemRef>) -> Vec<ItemDto> {
        if group.is_empty() {
            return Vec::new();
        }

        // 未登録の `source_id`（close との競合など）。本文を読み出す手立てが
        // 無いため、この項目群は既定値（空の本文）で返す。
        let Some(datetime_format) = self
            .sources
            .get(&source_id)
            .map(|record| record.datetime_format)
        else {
            return self.fallback_group(&group);
        };

        // 包含判定の突き合わせに使う、要求した項目群の生バイト範囲
        // （`crate::chunk_cache` のモジュール doc コメント「項目単位の対応付けを
        // 取り違えない方法」）。ミス時はそのままキャッシュへの格納にも使う。
        let wanted_spans: Vec<ChunkItemSpan> = group
            .iter()
            .map(|item| ChunkItemSpan {
                raw_offset: item.raw_offset,
                raw_byte_len: item.raw_byte_len,
            })
            .collect();

        if let Some(hit) = self.decoded_cache.get(source_id, &wanted_spans) {
            CHUNK_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            // キャッシュヒット時は本文を複製せず、参照カウントを増やすだけで
            // 応答を組み立てる。要求した項目群はキャッシュ側の
            // 並びの `hit.offset` から連続して入っている（包含判定が要素ごとの
            // 完全一致で位置を決めているため、ここで並びがずれることはない）。
            return group
                .iter()
                .zip(hit.items[hit.offset..].iter())
                .map(|(item, text)| item_dto_from_text(item, Arc::clone(text), datetime_format))
                .collect();
        }
        CHUNK_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);

        let Some((path, selected_encoding, recorded_snapshot)) =
            self.sources.get(&source_id).map(|record| {
                (
                    record.path.clone(),
                    record.selected_encoding,
                    record.snapshot,
                )
            })
        else {
            return self.fallback_group(&group);
        };

        SOURCE_REOPENS.fetch_add(1, Ordering::Relaxed);
        let (mut file, new_snapshot) = match hakutaku_data_source::reopen_for_reload(&path) {
            Ok(pair) => pair,
            Err(hakutaku_data_source::ReopenForReloadError::Deleted) => {
                self.mark_changed_now(source_id, ChangeKind::Deleted);
                return self.fallback_group(&group);
            }
            Err(hakutaku_data_source::ReopenForReloadError::SharingViolation { .. }) => {
                self.mark_sharing_violation_now(source_id);
                return self.fallback_group(&group);
            }
            Err(hakutaku_data_source::ReopenForReloadError::Io { reason }) => {
                self.mark_error_now(source_id, reason);
                return self.fallback_group(&group);
            }
        };

        // 索引に記録した生バイト範囲が、現在のファイルにまだ収まっているかを
        // 確認する（LOG-023。識別子が変わっていたり、記録済みの snapshot_end
        // より縮んでいたりする場合、索引がもはや有効ではない）。
        if new_snapshot.identity != recorded_snapshot.identity {
            self.mark_changed_now(source_id, ChangeKind::Replaced);
            return self.fallback_group(&group);
        }
        if new_snapshot.snapshot_end < recorded_snapshot.snapshot_end {
            self.mark_changed_now(source_id, ChangeKind::Shrunk);
            return self.fallback_group(&group);
        }

        let span_start = group.iter().map(|item| item.raw_offset).min().unwrap_or(0);
        let span_end = group
            .iter()
            .map(|item| item.raw_offset.saturating_add(u64::from(item.raw_byte_len)))
            .max()
            .unwrap_or(span_start);
        let span_len = usize::try_from(span_end.saturating_sub(span_start)).unwrap_or(usize::MAX);

        if file.seek(SeekFrom::Start(span_start)).is_err() {
            self.mark_error_now(
                source_id,
                "本文の読み出しに失敗しました（seek）。".to_string(),
            );
            return self.fallback_group(&group);
        }
        let mut buffer = vec![0u8; span_len];
        if file.read_exact(&mut buffer).is_err() {
            self.mark_error_now(
                source_id,
                "本文の読み出しに失敗しました（read）。".to_string(),
            );
            return self.fallback_group(&group);
        }

        let decided = hakutaku_format_detection::DecidedEncoding {
            encoding: selected_encoding,
            // route・warnings は decode() が参照しないため、オンデマンド
            // 読み出し専用のプレースホルダーで構いません。
            route: hakutaku_format_detection::DetectionRoute::EnvironmentAnsi,
            bom_len: 0,
            warnings: Vec::new(),
        };

        let mut decoded_items: Vec<Arc<str>> = Vec::with_capacity(group.len());
        for item in &group {
            let rel_start =
                usize::try_from(item.raw_offset.saturating_sub(span_start)).unwrap_or(0);
            let rel_end = rel_start.saturating_add(item.raw_byte_len as usize);
            let slice = buffer.get(rel_start..rel_end).unwrap_or(&[]);
            let text: Arc<str> = match hakutaku_format_detection::decode(slice, &decided) {
                Ok(outcome) => normalize_newlines(outcome.text),
                Err(_) => Arc::from(""),
            };
            decoded_items.push(text);
        }

        // 応答とキャッシュで本文を共有する。以前はキャッシュ用に
        // 全体を複製していたが、`Arc` の複製は参照カウントの増加だけで済む。
        // キャッシュが保持し続けるため、応答が解放されても本文は生きたまま。
        let decoded_items: Arc<[Arc<str>]> = Arc::from(decoded_items);
        self.decoded_cache.insert(
            source_id,
            wanted_spans.into_boxed_slice(),
            Arc::clone(&decoded_items),
        );

        group
            .iter()
            .zip(decoded_items.iter())
            .map(|(item, text)| item_dto_from_text(item, Arc::clone(text), datetime_format))
            .collect()
    }

    /// 本文を読み出せなかった項目群を、既定値（空の本文）の [`ItemDto`] として
    /// 返し、その件数を [`Self::hydrate_fallback_items`] へ数え上げます。
    ///
    /// `ERR-001` に従い、この応答自体は失敗させません（同じ応答に含まれる他の
    /// 項目・他のソースの取得は継続します）。数え上げるのは、空の本文が黙って
    /// 結果へ混ざると取り消せない呼び出し側——クリップボードコピー——が、
    /// 後から気づいて全体を失敗させられるようにするためです（`COPY-005`、
    /// Issue #37）。
    fn fallback_group(&mut self, group: &[IndexItemRef]) -> Vec<ItemDto> {
        self.hydrate_fallback_items = self
            .hydrate_fallback_items
            .saturating_add(group.len() as u64);
        group.iter().map(item_dto_fallback).collect()
    }

    /// 現在登録されている表示集合の件数（テスト・診断用）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.display_sets.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.display_sets.is_empty()
    }
}

/// 索引情報から本文なしの既定値 `ItemDto` を組み立てます（未知のソース・
/// 読み出し失敗時の防御的フォールバック。P08-5）。
fn item_dto_fallback(item: &IndexItemRef) -> ItemDto {
    item_dto_from_text(item, Arc::from(""), None)
}

/// 継続行の内部区切り文字を `\n` へ正規化し、共有本文（`Arc<str>`）にします。
///
/// 継続行の内部区切り文字は生バイトのまま `\r\n` を含み得ます。索引化前
/// （P08-1）は常に `\n` へ正規化していたため、オンデマンド読み出しでも同じ
/// 表示になるよう正規化します（`crate::line_index` のモジュール doc コメント
/// 参照）。
///
/// `\r` を含むかどうかで分岐するのは、置換が不要な場合に `String` をもう1つ
/// 確保しないためです。`str::replace` は該当箇所がなくても必ず
/// 新しい `String` を確保します。`\r` を含まない項目（`\n` 改行のログ、および
/// 継続行を持たない大多数の項目）が支配的なため、この分岐はホットパスの確保を
/// 1件分減らします。判定は「`\r\n`」ではなく「`\r`」で行いますが、`\r` を含む
/// のに `\r\n` を含まない項目では `replace` が何も置換せずに同じ内容を返すため、
/// 結果は変わりません。
fn normalize_newlines(text: String) -> Arc<str> {
    if text.contains('\r') {
        Arc::from(text.replace("\r\n", "\n"))
    } else {
        Arc::from(text)
    }
}

/// 索引情報とデコード済み本文から `ItemDto` を組み立てます（P08-5）。
///
/// `timestamp_display` は、本文の先頭物理行を `format` で再解析して決定的に
/// 再構成します（`LOG-024`。旧 `IndexedText::timestamp_display` と同じ規則）。
fn item_dto_from_text(
    item: &IndexItemRef,
    raw_text: Arc<str>,
    format: Option<LogDateTimeFormat>,
) -> ItemDto {
    let timestamp_display = if item.has_timestamp {
        format.and_then(|format| {
            let first_line = match raw_text.find('\n') {
                Some(pos) => &raw_text[..pos],
                None => &*raw_text,
            };
            hakutaku_parser::parse_datetime_with_format(format, first_line)
                .map(|matched| matched.to_display_string())
        })
    } else {
        None
    };

    ItemDto {
        item_id: item.item_id,
        timestamp_display,
        raw_text,
        source_label: item.source_label.clone(),
        source_line_number: item.source_line_number,
        confirmed: item.confirmed,
        continuation_count: item.continuation_count,
        raw_display: !item.has_timestamp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::PendingItem;

    fn pending(line_number: u64, raw_offset: u64, text: &str) -> PendingItem {
        PendingItem {
            raw_offset,
            raw_byte_len: u32::try_from(text.len()).unwrap(),
            comparison_key_millis: None,
            source_line_number: line_number,
            continuation_count: 0,
            unconfirmed: false,
        }
    }

    struct TempFile {
        path: std::path::PathBuf,
    }

    impl TempFile {
        fn create(label: &str, contents: &[u8]) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let count = COUNTER.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "hakutaku-core-services-registry-test-{label}-{}-{count}-{nanos}.log",
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

    /// テスト用に、実ファイルをスナップショットして `insert_source` します。
    /// `contents` の先頭1行分を1件の PendingItem として登録します。
    fn insert_from_file(
        registry: &mut DisplaySetRegistry,
        budget: &crate::budget::SourceBudget,
        path: &std::path::Path,
        label: &str,
        first_line: &str,
    ) -> DisplaySetHandle {
        let (file, snapshot) = hakutaku_data_source::open_and_snapshot(path).expect("開けるはず");
        drop(file);
        let reservation = budget
            .reserve(snapshot.snapshot_end)
            .expect("テストの上限は十分大きいはず");
        registry
            .insert_source(
                path.to_path_buf(),
                label.to_string(),
                &[pending(1, 0, first_line)],
                snapshot,
                reservation,
                false,
                None,
                SelectedEncoding::Utf8,
                CapacityEstimate::Exact(1),
            )
            .expect("索引予約は十分な予算内のはず")
    }

    // 受け入れ条件: 複数のソースを登録でき、各ソースに source_id と来歴が付く。
    // ソースごとに独立して走査でき、他ソースの状態に影響されない。
    #[test]
    fn insert_source_registers_multiple_independent_sources() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file_a = TempFile::create("multi-a", b"a content");
        let file_b = TempFile::create("multi-b", b"b content");

        let handle_a = insert_from_file(&mut registry, &budget, &file_a.path, "a.log", "a content");
        let handle_b = insert_from_file(&mut registry, &budget, &file_b.path, "b.log", "b content");

        assert_ne!(handle_a.source_id, handle_b.source_id);
        assert_ne!(handle_a.display_set_id, handle_b.display_set_id);

        let summaries = registry.list_sources();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].source_id, handle_a.source_id);
        assert_eq!(summaries[0].label, "a.log");
        assert_eq!(summaries[0].status, SourceStatus::Loaded);
        assert_eq!(summaries[1].source_id, handle_b.source_id);
        assert_eq!(summaries[1].label, "b.log");

        // 各ソースは独立した表示集合として範囲取得でき、本文も正しく復元される
        // （オンデマンド読み出し、P08-5）。
        let response_a = registry
            .fetch_range(
                handle_a.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle_a.generation,
                },
            )
            .expect("成功するはず");
        assert_eq!(response_a.items[0].source_label, "a.log");
        assert_eq!(&*response_a.items[0].raw_text, "a content");
    }

    // 受け入れ条件: close 後に再追加できる（合計から除外される）。
    #[test]
    fn close_source_releases_budget_and_removes_display_set() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create("close-reopen", b"content");

        let handle = insert_from_file(&mut registry, &budget, &file.path, "a.log", "content");
        assert!(budget.total_bytes() > 0);
        assert_eq!(registry.list_sources().len(), 1);

        let closed = registry.close_source(handle.source_id, &budget);
        assert!(closed.is_some());
        assert_eq!(budget.total_bytes(), 0, "予約が返却されるはず");
        assert!(registry.list_sources().is_empty());

        let error = registry
            .fetch_range(
                handle.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect_err("close 済みなのでエラーになるはず");
        assert_eq!(error, FetchRangeError::UnknownDisplaySet);

        let reopened = insert_from_file(&mut registry, &budget, &file.path, "a.log", "content");
        assert_ne!(reopened.source_id, handle.source_id);
        assert_eq!(registry.list_sources().len(), 1);
    }

    // 未登録の source_id に対する close_source は None を返し、何も変更しない。
    #[test]
    fn close_source_for_unknown_id_returns_none() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        assert!(registry.close_source(999, &budget).is_none());
    }

    /// 1項目分の生バイトを持つファイルを登録し、範囲取得の本文を返します。
    /// `fetch_count` 回取得して最後の結果を返すので、2回目以降はキャッシュ
    /// 経由の経路（`decoded_cache`）を通ります。
    fn fetch_single_item_text(label: &str, contents: &[u8], fetch_count: usize) -> String {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create(label, contents);
        let text = std::str::from_utf8(contents).expect("テストデータは UTF-8");
        let handle = insert_from_file(&mut registry, &budget, &file.path, "crlf.log", text);

        let mut last = String::new();
        for _ in 0..fetch_count {
            let response = registry
                .fetch_range(
                    handle.display_set_id,
                    RangeRequest {
                        start: 0,
                        max_items: 10,
                        expected_generation: handle.generation,
                    },
                )
                .expect("成功するはず");
            last = response.items[0].raw_text.to_string();
        }
        last
    }

    // 受け入れ条件: 継続行の内部区切り文字 `\r\n` は、オンデマンド読み出しでも
    // `\n` へ正規化される（`LOG-024`。索引化前の表示と同じ結果になること）。
    // 無条件の replace を「`\r` を含む場合だけ」の分岐へ変えたため、
    // 正規化結果が変わっていないことを固定する。
    #[test]
    fn hydrate_normalizes_crlf_inside_item_to_lf() {
        assert_eq!(
            fetch_single_item_text("crlf-normalize", b"line one\r\nline two", 1),
            "line one\nline two",
            "`\\r\\n` は `\\n` へ正規化されるはず"
        );
    }

    // 受け入れ条件: `\r` を含まない本文は、分岐を通しても内容が変わらない
    // （`\r` 判定の分岐追加による退行がないこと）。
    #[test]
    fn hydrate_keeps_text_without_carriage_return_unchanged() {
        assert_eq!(
            fetch_single_item_text("lf-only", b"line one\nline two", 1),
            "line one\nline two"
        );
    }

    // 受け入れ条件: 同じ範囲を再取得しても本文は変わらない（キャッシュと応答で
    // 本文を共有するようにした変更で、2回目が壊れないこと）。
    #[test]
    fn hydrate_returns_same_text_on_cached_refetch() {
        assert_eq!(
            fetch_single_item_text("crlf-cached", b"line one\r\nline two", 2),
            "line one\nline two"
        );
    }

    // --- デコード済みチャンクキャッシュの包含ヒット ---

    /// 包含ヒットの検証で使う6行のファイル（各行4バイト + 改行）。
    const CONTAINMENT_LINES: [(u64, &str, Option<i64>); 6] = [
        (1, "aaaa", None),
        (2, "bbbb", None),
        (3, "cccc", None),
        (4, "dddd", None),
        (5, "eeee", None),
        (6, "ffff", None),
    ];

    /// 範囲取得の本文を、要求した並びの `Vec<String>` として取り出す。
    fn fetch_texts(
        registry: &mut DisplaySetRegistry,
        handle: &DisplaySetHandle,
        start: u64,
        max_items: u32,
    ) -> Vec<String> {
        registry
            .fetch_range(
                handle.display_set_id,
                RangeRequest {
                    start,
                    max_items,
                    expected_generation: handle.generation,
                },
            )
            .expect("成功するはず")
            .items
            .into_iter()
            .map(|item| item.raw_text.to_string())
            .collect()
    }

    // 受け入れ条件: 先に取得した範囲へ完全に包含される部分範囲は、ファイルへ
    // 一切アクセスせずにキャッシュから返る。
    //
    // 「ファイルへアクセスしていないこと」は、1回目の取得後にファイルを削除
    // してから部分範囲を取得することで確かめる。もし再オープンしていれば
    // `reopen_for_reload` が削除を検知し（`LOG-023`）、本文は空になり状態は
    // `Changed(Deleted)` へ変わるため、この2つが変わらないことがそのまま
    // 「開き直していない」ことの証拠になる。
    #[test]
    fn fetch_hits_cache_for_contained_subrange_without_reopening_the_file() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let (handle, file) =
            insert_multiline_file(&mut registry, &budget, "contain-hit", &CONTAINMENT_LINES);

        let all = fetch_texts(&mut registry, &handle, 0, 6);
        assert_eq!(all, vec!["aaaa", "bbbb", "cccc", "dddd", "eeee", "ffff"]);

        std::fs::remove_file(&file.path).expect("削除できるはず");

        // 中ほど（前後どちらの端でもない位置）を要求する。位置がずれても
        // 取り違えないことを見るため、先頭からの部分ではなく途中を選ぶ。
        let middle = fetch_texts(&mut registry, &handle, 2, 3);
        assert_eq!(
            middle,
            vec!["cccc", "dddd", "eeee"],
            "包含される部分範囲はキャッシュから同じ本文が返るはず"
        );
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(SourceStatus::Loaded),
            "キャッシュヒット時は開き直さないので削除は検知されないはず"
        );

        // エントリの両端も同じく包含ヒットになる（境界の取り違えがないこと）。
        assert_eq!(fetch_texts(&mut registry, &handle, 0, 1), vec!["aaaa"]);
        assert_eq!(fetch_texts(&mut registry, &handle, 5, 1), vec!["ffff"]);
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(SourceStatus::Loaded)
        );
    }

    // 受け入れ条件: キャッシュ済みの範囲に包含されない要求は、包含判定を
    // 入れてもヒットせずファイルを読みに行く（過剰にヒットさせて
    // いないことの確認）。
    #[test]
    fn fetch_misses_cache_for_range_not_contained_in_a_cached_chunk() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let (handle, file) =
            insert_multiline_file(&mut registry, &budget, "contain-miss", &CONTAINMENT_LINES);

        // 先頭3件だけをキャッシュへ載せる。
        assert_eq!(
            fetch_texts(&mut registry, &handle, 0, 3),
            vec!["aaaa", "bbbb", "cccc"]
        );

        std::fs::remove_file(&file.path).expect("削除できるはず");

        // 後半3件はキャッシュ済みの範囲と重ならないため、開き直して削除を
        // 検知する（`LOG-023` の既存挙動が変わっていないこと）。
        let tail = fetch_texts(&mut registry, &handle, 3, 3);
        assert_eq!(
            tail,
            vec!["", "", ""],
            "包含されない範囲は読み出しに失敗して空の本文になるはず"
        );
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(SourceStatus::Changed(ChangeKind::Deleted)),
            "開き直した結果として削除を検知するはず"
        );
    }

    // 受け入れ条件: 部分的に重なるだけ（一部がキャッシュ済み範囲からはみ出す）
    // の要求はヒットしない（照合は包含だけを扱い、重なりの合成はしない）。
    #[test]
    fn fetch_misses_cache_for_partially_overlapping_range() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let (handle, file) = insert_multiline_file(
            &mut registry,
            &budget,
            "contain-overlap",
            &CONTAINMENT_LINES,
        );

        assert_eq!(
            fetch_texts(&mut registry, &handle, 0, 3),
            vec!["aaaa", "bbbb", "cccc"]
        );

        std::fs::remove_file(&file.path).expect("削除できるはず");

        // 先頭2件はキャッシュ済みだが3件目がはみ出すため、全体としてミス。
        let overlapping = fetch_texts(&mut registry, &handle, 1, 3);
        assert_eq!(overlapping, vec!["", "", ""]);
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(SourceStatus::Changed(ChangeKind::Deleted))
        );
    }

    // 受け入れ条件: 再構築で世代が進んだあとの範囲取得は、前の世代の
    // デコード済みチャンクを再利用しない（包含判定で照合が広がる
    // ぶん、世代を跨いだ再利用が起きないことを明示的に固定する）。
    #[test]
    fn rebuild_invalidates_decoded_chunk_cache_across_generations() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create("rebuild-cache", b"v2Xb");
        let handle = insert_from_file(&mut registry, &budget, &file.path, "a.log", "v2");

        // 世代1の本文をキャッシュへ載せる。
        assert_eq!(fetch_texts(&mut registry, &handle, 0, 10), vec!["v2"]);

        let sources = vec![SourceInfo {
            source_id: handle.source_id,
            label: "a.log".to_string(),
        }];
        let mut text = IndexedText::new();
        let items = crate::item::build_items_from_pending(
            handle.source_id,
            &[pending(1, 0, "v2"), pending(2, 3, "b")],
            &mut text,
        )
        .expect("予約は成功するはず");
        let mut texts = HashMap::new();
        texts.insert(handle.source_id, text);
        let outcome = registry
            .rebuild(handle.display_set_id, sources, items, texts)
            .expect("既存IDなので成功するはず");
        assert_eq!(outcome.generation, 2);

        std::fs::remove_file(&file.path).expect("削除できるはず");

        // 世代2の1件目は、世代1でキャッシュした項目と生バイト範囲
        // （オフセット0・長さ2）が完全に一致する。キャッシュを捨てていなければ
        // ここで古い本文が返ってしまう。
        let rebuilt = registry
            .fetch_range(
                handle.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 1,
                    expected_generation: 2,
                },
            )
            .expect("最新世代なら成功するはず");
        assert_eq!(
            rebuilt.items[0].raw_text.as_ref(),
            "",
            "再構築でキャッシュを捨てるため、古い世代の本文は再利用されないはず"
        );
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(SourceStatus::Changed(ChangeKind::Deleted)),
            "キャッシュを使わずに開き直した結果、削除を検知するはず"
        );
    }

    // 受け入れ条件: 縮小を検知すると変更済みになり、表示集合の世代が進んで
    // 無効化される（LOG-023）。他ソースには影響しない。
    #[test]
    fn refresh_source_detects_shrink_and_invalidates_only_that_source() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let shrinking = TempFile::create("refresh-shrink", b"0123456789");
        let stable = TempFile::create("refresh-stable", b"stable content");

        let handle_shrink = insert_from_file(
            &mut registry,
            &budget,
            &shrinking.path,
            "shrink.log",
            "0123456789",
        );
        let handle_stable = insert_from_file(
            &mut registry,
            &budget,
            &stable.path,
            "stable.log",
            "stable content",
        );

        {
            let writer = std::fs::OpenOptions::new()
                .write(true)
                .open(&shrinking.path)
                .expect("書き込み用に開けるはず");
            writer.set_len(3).expect("切り詰めできるはず");
        }

        let status = registry
            .refresh_source(handle_shrink.source_id)
            .expect("登録済みのはず");
        assert_eq!(status, SourceStatus::Changed(ChangeKind::Shrunk));
        assert_eq!(
            registry.source_status(handle_shrink.source_id),
            Some(SourceStatus::Changed(ChangeKind::Shrunk))
        );

        let stale = registry.fetch_range(
            handle_shrink.display_set_id,
            RangeRequest {
                start: 0,
                max_items: 10,
                expected_generation: handle_shrink.generation,
            },
        );
        assert!(matches!(
            stale,
            Err(FetchRangeError::GenerationMismatch { .. })
        ));

        let fresh = registry
            .fetch_range(
                handle_shrink.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle_shrink.generation + 1,
                },
            )
            .expect("新しい世代では成功するはず");
        assert_eq!(fresh.items.len(), 0);

        assert_eq!(
            registry.source_status(handle_stable.source_id),
            Some(SourceStatus::Loaded)
        );
        let stable_response = registry
            .fetch_range(
                handle_stable.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle_stable.generation,
                },
            )
            .expect("stable は影響を受けず成功するはず");
        assert_eq!(stable_response.items.len(), 1);
    }

    // 受け入れ条件: 変更済みソースが再利用されない。
    #[test]
    fn refresh_source_does_not_reuse_a_changed_source() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create("refresh-no-reuse", b"gone soon");
        let handle = insert_from_file(&mut registry, &budget, &file.path, "a.log", "gone soon");

        std::fs::remove_file(&file.path).expect("削除できるはず");
        let first = registry
            .refresh_source(handle.source_id)
            .expect("登録済みのはず");
        assert_eq!(first, SourceStatus::Changed(ChangeKind::Deleted));

        std::fs::write(&file.path, b"restored").expect("復元できるはず");
        let second = registry
            .refresh_source(handle.source_id)
            .expect("登録済みのはず");
        assert_eq!(
            second,
            SourceStatus::Changed(ChangeKind::Deleted),
            "close するまで再検証されないはず"
        );
    }

    // 受け入れ条件: 追記（サイズ増）は Changed にならず、Loaded のまま
    // （ADR-0007: 同一だが更新未反映）。
    #[test]
    fn refresh_source_treats_append_as_still_loaded() {
        use std::io::Write;

        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create("refresh-append", b"hello");
        let handle = insert_from_file(&mut registry, &budget, &file.path, "a.log", "hello");

        {
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記用に開けるはず");
            writer.write_all(b" world").expect("追記できるはず");
        }

        let status = registry
            .refresh_source(handle.source_id)
            .expect("登録済みのはず");
        assert_eq!(status, SourceStatus::Loaded, "追記は Changed にしないはず");

        let response = registry
            .fetch_range(
                handle.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("世代が変わっていないので成功するはず");
        assert_eq!(response.items.len(), 1);
    }

    // 未登録の source_id に対する refresh_source は None を返す。
    #[test]
    fn refresh_source_for_unknown_id_returns_none() {
        let mut registry = DisplaySetRegistry::new();
        assert!(registry.refresh_source(999).is_none());
    }

    // 受け入れ条件（LOG-027）: 共有違反は Error とは別の SourceStatus::
    // SharingViolation として区別される。
    #[test]
    fn refresh_source_distinguishes_sharing_violation_and_allows_retry_after_unlock() {
        use std::os::windows::fs::OpenOptionsExt;

        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let locked = TempFile::create("refresh-sharing-violation", b"locked content");
        let stable = TempFile::create("refresh-sharing-violation-stable", b"stable content");

        let handle_locked = insert_from_file(
            &mut registry,
            &budget,
            &locked.path,
            "locked.log",
            "locked content",
        );
        let handle_stable = insert_from_file(
            &mut registry,
            &budget,
            &stable.path,
            "stable.log",
            "stable content",
        );

        let locker = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&locked.path)
            .expect("排他的に開けるはず（LOG-027 再現用の前提）");

        let status = registry
            .refresh_source(handle_locked.source_id)
            .expect("登録済みのはず");
        assert_eq!(status, SourceStatus::SharingViolation);
        assert_eq!(
            registry.source_status(handle_locked.source_id),
            Some(SourceStatus::SharingViolation)
        );

        let response = registry
            .fetch_range(
                handle_locked.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle_locked.generation,
                },
            )
            .expect("世代は変わっていないので成功するはず");
        assert_eq!(response.items.len(), 1);

        assert_eq!(
            registry.source_status(handle_stable.source_id),
            Some(SourceStatus::Loaded)
        );

        drop(locker);
        let retried = registry
            .refresh_source(handle_locked.source_id)
            .expect("登録済みのはず");
        assert_eq!(
            retried,
            SourceStatus::Loaded,
            "ロック解除後は再試行できるはず（LOG-027）"
        );
    }

    // 受け入れ条件: レジストリ経由の再構築で世代が進み、範囲取得の世代不一致
    // 判定に反映される。
    #[test]
    fn rebuild_advances_generation_and_is_reflected_in_fetch_range() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create("rebuild", b"v2Xb");
        let handle = insert_from_file(&mut registry, &budget, &file.path, "a.log", "v2");
        assert_eq!(handle.generation, 1);

        let sources = vec![SourceInfo {
            source_id: handle.source_id,
            label: "a.log".to_string(),
        }];
        let mut text = IndexedText::new();
        let items = crate::item::build_items_from_pending(
            handle.source_id,
            &[pending(1, 0, "v2"), pending(2, 3, "b")],
            &mut text,
        )
        .expect("予約は成功するはず");
        let mut texts = HashMap::new();
        texts.insert(handle.source_id, text);
        let outcome = registry
            .rebuild(handle.display_set_id, sources, items, texts)
            .expect("既存IDなので成功するはず");
        assert_eq!(outcome.generation, 2);
        assert_eq!(outcome.total_items, 2);

        let stale = registry.fetch_range(
            handle.display_set_id,
            RangeRequest {
                start: 0,
                max_items: 10,
                expected_generation: 1,
            },
        );
        assert_eq!(
            stale,
            Err(FetchRangeError::GenerationMismatch {
                expected: 1,
                current: 2
            })
        );

        let fresh = registry
            .fetch_range(
                handle.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: 2,
                },
            )
            .expect("最新世代なら成功するはず");
        assert_eq!(fresh.items.len(), 2);
    }

    #[test]
    fn rebuild_for_unknown_display_set_returns_none() {
        let mut registry = DisplaySetRegistry::new();
        let result = registry.rebuild(123, Vec::new(), Vec::new(), HashMap::new());
        assert!(result.is_none());
    }

    // --- P08-5: evict_inactive_sources（キャッシュのクリアへ単純化） ---

    // 受け入れ条件: evict_inactive_sources は非アクティブな Loaded ソースを
    // 対象に返すが、索引・項目・世代・ステータスは変更しない（P08-5）。
    // 呼び出し後も本文が正しく取得できることで、キャッシュのクリアだけで
    // あることを確認する。
    #[test]
    fn evict_inactive_sources_only_clears_cache_and_preserves_items_and_status() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let active_file = TempFile::create("evict-active", b"active content");
        let inactive_file = TempFile::create("evict-inactive", b"inactive content");

        let active = insert_from_file(
            &mut registry,
            &budget,
            &active_file.path,
            "active.log",
            "active content",
        );
        let inactive = insert_from_file(
            &mut registry,
            &budget,
            &inactive_file.path,
            "inactive.log",
            "inactive content",
        );

        registry.set_active_source(Some(active.source_id));

        let evicted = registry.evict_inactive_sources();
        assert_eq!(evicted, vec![inactive.source_id]);

        // ステータス・世代は変わらない（P08-5: 索引を破棄しない）。
        assert_eq!(
            registry.source_status(inactive.source_id),
            Some(SourceStatus::Loaded),
            "P08-5 以降、キャッシュのクリアだけなのでステータスは変わらない"
        );

        // 同一世代のまま、本文もそのまま取得できる。
        let inactive_response = registry
            .fetch_range(
                inactive.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: inactive.generation,
                },
            )
            .expect("世代は変わっていないので成功するはず");
        assert_eq!(&*inactive_response.items[0].raw_text, "inactive content");

        let active_response = registry
            .fetch_range(
                active.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: active.generation,
                },
            )
            .expect("アクティブなソースは影響を受けないはず");
        assert_eq!(active_response.items.len(), 1);
    }

    // 受け入れ条件: 繰り返し呼んでも安全（キャッシュを毎回クリアするだけで
    // エラーにならない）。
    #[test]
    fn evict_inactive_sources_is_safe_to_call_repeatedly() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create("evict-repeat", b"content");
        let handle = insert_from_file(&mut registry, &budget, &file.path, "a.log", "content");

        let first = registry.evict_inactive_sources();
        assert_eq!(first, vec![handle.source_id]);
        let second = registry.evict_inactive_sources();
        assert_eq!(
            second,
            vec![handle.source_id],
            "ステータスを変えないため、繰り返し対象になり続ける"
        );

        let response = registry
            .fetch_range(
                handle.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("繰り返しキャッシュをクリアしても本文取得は継続できるはず");
        assert_eq!(&*response.items[0].raw_text, "content");
    }

    // 受け入れ条件: source_id_for_display_set が display_set_id から source_id を
    // 正しく逆引きできる。
    #[test]
    fn source_id_for_display_set_resolves_correctly() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create("lookup", b"content");
        let handle = insert_from_file(&mut registry, &budget, &file.path, "a.log", "content");

        assert_eq!(
            registry.source_id_for_display_set(handle.display_set_id),
            Some(handle.source_id)
        );
        assert_eq!(registry.source_id_for_display_set(999), None);
    }

    // 受け入れ条件（Issue #37）: display_set_state は単独ソースでも統合表示集合
    // でも現在の世代・件数を返し、未知の ID では None を返す（source_id を
    // 持たない統合表示集合を「存在しない表示集合」として扱わない）。
    #[test]
    fn display_set_state_resolves_both_single_source_and_merged_view() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create("state", b"content");
        let handle = insert_from_file(&mut registry, &budget, &file.path, "a.log", "content");

        assert_eq!(
            registry.display_set_state(handle.display_set_id),
            Some(DisplaySetState {
                generation: handle.generation,
                total_items: handle.total_items,
            })
        );
        assert_eq!(registry.display_set_state(999), None);

        let merged = registry.enable_merged_view().expect("成功するはず");
        assert_eq!(
            registry.display_set_state(merged.display_set_id),
            Some(DisplaySetState {
                generation: merged.generation,
                total_items: merged.total_items,
            })
        );

        // OFF にした後の識別子は、他の表示集合と取り違えず未知として扱われる。
        registry.disable_merged_view();
        assert_eq!(registry.display_set_state(merged.display_set_id), None);
    }

    // 受け入れ条件: 未知の display_set_id は UnknownDisplaySet になる。
    #[test]
    fn fetch_range_for_unknown_display_set_returns_unknown_display_set_error() {
        let mut registry = DisplaySetRegistry::new();
        let request = RangeRequest {
            start: 0,
            max_items: 10,
            expected_generation: 1,
        };

        let error = registry
            .fetch_range(999, request)
            .expect_err("未登録のIDはエラーになるはず");
        assert_eq!(error, FetchRangeError::UnknownDisplaySet);
    }

    // 受け入れ条件: 未知のソース ID を参照する項目があっても panic せず、
    // 来歴・本文は既定値になる（防御的な経路の確認）。
    #[test]
    fn hydrate_falls_back_to_defaults_for_orphaned_items() {
        let dto = item_dto_fallback(&IndexItemRef {
            item_id: crate::item::ItemId {
                source_id: 999,
                seq: 0,
            },
            source_id: 999,
            source_label: String::new(),
            source_line_number: 0,
            confirmed: true,
            continuation_count: 0,
            has_timestamp: false,
            raw_offset: 0,
            raw_byte_len: 0,
        });
        assert_eq!(&*dto.raw_text, "");
        assert!(dto.raw_display);
        assert!(dto.confirmed);
    }

    // --- 統合表示集合（P09-1） ---

    /// テスト用に、複数行（日時つき／日時なし混在可）を持つ実ファイルを登録
    /// する。`lines` は `(source_line_number, text, comparison_key_millis)` の
    /// 並び。ファイル内容は `text` を `\n` 区切りで連結したものになる
    /// （`fetch_range` がオンデマンドで実ファイルから本文を読み出すため、
    /// オフセット・長さは実際のファイルレイアウトと一致させる）。
    fn insert_multiline_file(
        registry: &mut DisplaySetRegistry,
        budget: &crate::budget::SourceBudget,
        label: &str,
        lines: &[(u64, &str, Option<i64>)],
    ) -> (DisplaySetHandle, TempFile) {
        let contents = lines
            .iter()
            .map(|(_, text, _)| *text)
            .collect::<Vec<_>>()
            .join("\n");
        let file = TempFile::create(label, contents.as_bytes());

        let (opened, snapshot) =
            hakutaku_data_source::open_and_snapshot(&file.path).expect("開けるはず");
        drop(opened);
        let reservation = budget
            .reserve(snapshot.snapshot_end)
            .expect("テストの上限は十分大きいはず");

        let mut offset = 0u64;
        let pending_items: Vec<PendingItem> = lines
            .iter()
            .map(|(line_number, text, key)| {
                let item = PendingItem {
                    raw_offset: offset,
                    raw_byte_len: u32::try_from(text.len()).unwrap(),
                    comparison_key_millis: *key,
                    source_line_number: *line_number,
                    continuation_count: 0,
                    unconfirmed: false,
                };
                offset += text.len() as u64 + 1;
                item
            })
            .collect();

        let handle = registry
            .insert_source(
                file.path.clone(),
                label.to_string(),
                &pending_items,
                snapshot,
                reservation,
                false,
                None,
                SelectedEncoding::Utf8,
                CapacityEstimate::Exact(pending_items.len()),
            )
            .expect("索引予約は十分な予算内のはず");
        (handle, file)
    }

    // 受け入れ条件（LOG-007 の例）: 異なる2ファイル以上の行が、解析された
    // 日時の昇順で表示される。読み込み元も識別できる。
    #[test]
    fn enable_merged_view_merges_two_sources_in_ascending_timestamp_order() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        // A: 15:00:00.000, 15:00:01.000, 15:00:02.000（A-1, A-2, A-3）。
        // 起点を 0 ミリ秒とした相対値（0, 1000, 2000）を使う。
        let (_handle_a, _file_a) = insert_multiline_file(
            &mut registry,
            &budget,
            "a.log",
            &[
                (1, "A-1", Some(0)),
                (2, "A-2", Some(1000)),
                (3, "A-3", Some(2000)),
            ],
        );
        // B: 15:00:01.500（B-1、A-1 から 1500 ミリ秒後）
        let (_handle_b, _file_b) =
            insert_multiline_file(&mut registry, &budget, "b.log", &[(1, "B-1", Some(1500))]);

        let merged = registry
            .enable_merged_view()
            .expect("統合表示を開始できるはず");
        assert_eq!(merged.generation, 1);
        assert_eq!(merged.total_items, 4);

        let response = registry
            .fetch_range(
                merged.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: merged.generation,
                },
            )
            .expect("成功するはず");

        let texts: Vec<&str> = response.items.iter().map(|item| &*item.raw_text).collect();
        assert_eq!(texts, vec!["A-1", "A-2", "B-1", "A-3"]);

        let labels: Vec<&str> = response
            .items
            .iter()
            .map(|item| item.source_label.as_str())
            .collect();
        assert_eq!(labels, vec!["a.log", "a.log", "b.log", "a.log"]);
    }

    // 受け入れ条件: 同一比較キーの行が、ファイルの表示順（source_ordinal =
    // insert_source の呼び出し順）→ ファイル内の出現順で並ぶ。
    #[test]
    fn enable_merged_view_orders_ties_by_insertion_order_then_seq() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        // 先に登録した a.log の source_ordinal が小さくなるはず。
        let (_handle_a, _file_a) = insert_multiline_file(
            &mut registry,
            &budget,
            "a.log",
            &[(1, "A-same", Some(1000))],
        );
        let (_handle_b, _file_b) = insert_multiline_file(
            &mut registry,
            &budget,
            "b.log",
            &[(1, "B-same", Some(1000))],
        );

        let merged = registry.enable_merged_view().expect("成功するはず");
        let response = registry
            .fetch_range(
                merged.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: merged.generation,
                },
            )
            .expect("成功するはず");

        let texts: Vec<&str> = response.items.iter().map(|item| &*item.raw_text).collect();
        assert_eq!(
            texts,
            vec!["A-same", "B-same"],
            "先に開いた a.log が同一キーで先に並ぶはず"
        );
    }

    // 受け入れ条件: 統合結果から範囲を再取得しても、順序・識別子・来歴が
    // 変わらない（繰り返し実行での順序再現）。
    #[test]
    fn repeated_fetch_on_merged_view_returns_identical_response() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let (_handle_a, _file_a) = insert_multiline_file(
            &mut registry,
            &budget,
            "a.log",
            &[(1, "one", Some(1000)), (2, "two", Some(2000))],
        );
        let (_handle_b, _file_b) =
            insert_multiline_file(&mut registry, &budget, "b.log", &[(1, "mid", Some(1500))]);

        let merged = registry.enable_merged_view().expect("成功するはず");
        let request = RangeRequest {
            start: 0,
            max_items: 10,
            expected_generation: merged.generation,
        };

        let first = registry
            .fetch_range(merged.display_set_id, request)
            .unwrap();
        let second = registry
            .fetch_range(merged.display_set_id, request)
            .unwrap();
        assert_eq!(first, second, "再取得しても同じ応答になるはず");
    }

    // 受け入れ条件: 参加ソースの1つが無効化（LOG-023）されたら、統合表示集合の
    // 世代も進む。
    #[test]
    fn source_invalidation_advances_merged_view_generation() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let shrinking = TempFile::create("merged-shrink", b"0123456789");
        let handle_shrink = insert_from_file(
            &mut registry,
            &budget,
            &shrinking.path,
            "shrink.log",
            "0123456789",
        );
        let (_handle_stable, _file_stable) = insert_multiline_file(
            &mut registry,
            &budget,
            "stable.log",
            &[(1, "stable", Some(1000))],
        );

        let merged = registry.enable_merged_view().expect("成功するはず");
        assert_eq!(merged.total_items, 2);

        {
            let writer = std::fs::OpenOptions::new()
                .write(true)
                .open(&shrinking.path)
                .expect("書き込み用に開けるはず");
            writer.set_len(3).expect("切り詰めできるはず");
        }
        registry.refresh_source(handle_shrink.source_id);

        // 統合表示集合の世代が進んでいるため、古い世代での取得は拒否される。
        let stale = registry.fetch_range(
            merged.display_set_id,
            RangeRequest {
                start: 0,
                max_items: 10,
                expected_generation: merged.generation,
            },
        );
        assert!(matches!(
            stale,
            Err(FetchRangeError::GenerationMismatch { .. })
        ));

        let fresh = registry
            .fetch_range(
                merged.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: merged.generation + 1,
                },
            )
            .expect("新しい世代なら成功するはず");
        // shrink.log 側は LOG-023 により項目が空になるため、stable.log の
        // 1件だけが残る。
        assert_eq!(fresh.items.len(), 1);
        assert_eq!(fresh.items[0].source_label, "stable.log");
    }

    // 受け入れ条件: ソースを閉じると、統合表示集合の対象から除外され世代が
    // 進む（再読み込みなしでの対象の除外）。
    #[test]
    fn closing_a_member_source_removes_it_from_merged_view_and_advances_generation() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let (handle_a, _file_a) = insert_multiline_file(
            &mut registry,
            &budget,
            "a.log",
            &[(1, "a-line", Some(1000))],
        );
        let (_handle_b, _file_b) = insert_multiline_file(
            &mut registry,
            &budget,
            "b.log",
            &[(1, "b-line", Some(2000))],
        );

        let merged = registry.enable_merged_view().expect("成功するはず");
        assert_eq!(merged.total_items, 2);

        registry.close_source(handle_a.source_id, &budget);

        let response = registry
            .fetch_range(
                merged.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: merged.generation + 1,
                },
            )
            .expect("close 後の新しい世代では成功するはず");
        assert_eq!(response.items.len(), 1);
        assert_eq!(response.items[0].source_label, "b.log");
    }

    // 受け入れ条件: 対象ファイルの追加を再読み込みなしで行える。新しいソースを
    // 開くと、既存ソースを再解析せずに統合表示集合が拡張される。
    #[test]
    fn opening_a_new_source_while_merged_view_enabled_extends_it_without_reloading_existing() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let (_handle_a, _file_a) = insert_multiline_file(
            &mut registry,
            &budget,
            "a.log",
            &[(1, "a-line", Some(1000))],
        );
        let merged = registry.enable_merged_view().expect("成功するはず");
        assert_eq!(merged.total_items, 1);

        let (_handle_b, _file_b) = insert_multiline_file(
            &mut registry,
            &budget,
            "b.log",
            &[(1, "b-line", Some(2000))],
        );

        let response = registry
            .fetch_range(
                merged.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: merged.generation + 1,
                },
            )
            .expect("新しいソース追加後の世代では成功するはず");
        assert_eq!(response.items.len(), 2);
    }

    // 受け入れ条件: 統合表示を OFF にすると、ファイル単位の表示へ戻せる。
    // OFF・ON の切り替えで参照対象ファイル（各ソースの状態・世代）は変更
    // されない（ERR-003）。
    #[test]
    fn disable_merged_view_does_not_touch_individual_sources() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let (handle_a, _file_a) = insert_multiline_file(
            &mut registry,
            &budget,
            "a.log",
            &[(1, "a-line", Some(1000))],
        );

        let before = registry.current_handle(handle_a.source_id).unwrap();

        let merged = registry.enable_merged_view().expect("成功するはず");
        assert!(registry.is_merged_view_enabled());
        registry.disable_merged_view();
        assert!(!registry.is_merged_view_enabled());

        let after = registry.current_handle(handle_a.source_id).unwrap();
        assert_eq!(before, after, "個別ソースの状態は変わらないはず");

        // OFF にした統合表示集合の display_set_id はもう使えない。
        let error = registry
            .fetch_range(
                merged.display_set_id,
                RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: merged.generation,
                },
            )
            .expect_err("OFF 後は UnknownDisplaySet になるはず");
        assert_eq!(error, FetchRangeError::UnknownDisplaySet);
    }

    // 受け入れ条件: 開いているソースが無くてもエラーにはならず、空の統合
    // 表示集合になる。
    #[test]
    fn enable_merged_view_with_no_sources_yields_empty_result() {
        let mut registry = DisplaySetRegistry::new();
        let merged = registry
            .enable_merged_view()
            .expect("ソースが無くてもエラーにはならないはず");
        assert_eq!(merged.total_items, 0);
    }
}
