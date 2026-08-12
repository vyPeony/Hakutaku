//! 表示集合（`DisplaySet`）と範囲取得契約（`tasks/phase-04-vertical-slice.md`
//! 「契約に織り込む4点」「作業項目1」）。
//!
//! # 契約に織り込む4点との対応
//!
//! 1. 範囲取得の要求（[`RangeRequest`]）は `start`（表示集合内でのオフセット）
//!    だけを持ち、単一ファイルの物理オフセットへは一切結合しません。表示集合が
//!    将来マージ結果になっても（P06・P09）、この要求の形は変わりません。
//! 2. 応答の各項目（[`ItemDto`]）は安定した識別子（`item_id`）と読み込み元の
//!    来歴（`source_label`）を持ちます。
//! 3. 同一の要求（`start`・`expected_generation` が同じ）を繰り返しても、
//!    表示集合が変わらない限り同じ順序・識別子を返します（[`DisplaySet`] は
//!    構築後に項目順を変更しないため）。
//! 4. [`DisplaySet::generation`] は再構築（[`DisplaySet::rebuild`]）のたびに
//!    増え、要求の `expected_generation` と一致しない場合は
//!    [`RangeFetchError::GenerationMismatch`] を返します（`LOG-023`・`LOG-028`
//!    の下地）。
//!
//! # P08-5 索引 + オンデマンド読み出しへの移行
//!
//! P08-1 まで、`DisplaySet` は `ItemDto`（本文込みの最終応答形）を
//! 自ら組み立てていました。P08-5 で `IndexedText` が本文を一切保持しなく
//! なったため（`crate::line_index` のモジュール doc コメント参照）、
//! `DisplaySet` はもはや本文をデコードできません（ファイルへのアクセス手段を
//! 持たないため）。
//!
//! 範囲取得の**索引レベル**の実装（契約の4点そのもの: 順序・識別子・世代・
//! 転送上限の判定）はこのモジュールに残し、[`DisplaySet::fetch_range_index`]
//! （`pub(crate)`）が [`IndexRangeResponse`]（本文なし、生バイト範囲だけを
//! 持つ [`IndexItemRef`] の並び）を返します。本文のオンデマンド読み出し・
//! デコードは `crate::registry::DisplaySetRegistry::fetch_range` が行い、
//! 最終的な `ItemDto`（型はここで定義したまま、`raw_text`・
//! `timestamp_display` は不変）を組み立てます。

use std::collections::HashMap;
use std::sync::Arc;

use crate::item::{Item, ItemId, SourceInfo};
use crate::line_index::IndexedText;

/// 1回の範囲取得応答で返す項目数の上限（暫定値）。
///
/// `PERF-009`・`PERF-012` の実測（P04-3）が済むまでの暫定値です。転送コストの
/// 実測結果によって見直される可能性があります（計画書「作業項目3」）。
pub const MAX_ITEMS_PER_RESPONSE: u32 = 512;

/// 1回の範囲取得応答で返す原文（`raw_text`）合計バイト数の上限（暫定値）。
///
/// [`MAX_ITEMS_PER_RESPONSE`] と同じく、P04-3 の実測が済むまでの暫定値です。
/// P08-5 以降、この上限の判定には索引の `byte_len`（生バイト長）を使います。
/// デコード後の文字数とは厳密には一致しませんが（UTF-8・Windows コードページの
/// いずれも1バイト〜数バイト/文字であり、生バイト長はおおむね妥当な近似で
/// あるため）、転送コストの見積もりとしては十分な精度です。
pub const MAX_RESPONSE_RAW_BYTES: usize = 2 * 1024 * 1024;

/// 表示集合に対する範囲取得の要求です（契約に織り込む4点の1）。
///
/// `start` は表示集合内の項目インデックス（0起点）であり、ファイルの物理
/// オフセットではありません。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeRequest {
    pub start: u64,
    pub max_items: u32,
    /// 呼び出し側が最後に観測した世代。[`DisplaySet::generation`] と一致しない
    /// 場合、要求は [`RangeFetchError::GenerationMismatch`] で拒否されます。
    pub expected_generation: u64,
}

/// 範囲取得応答の1項目です（契約に織り込む4点の2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDto {
    pub item_id: ItemId,
    /// ISO 8601 風の表示文字列。日時が解析できなかった場合は `None`
    /// （フロントエンドは null として扱う）。
    pub timestamp_display: Option<String>,
    /// 原文（継続行の内部改行を含む）。
    ///
    /// `Arc<str>` なのは、範囲取得のデコード済みキャッシュ
    /// （`crate::chunk_cache::DecodedChunkCache`）と応答が同じ本文を共有する
    /// ためです。キャッシュへ格納するときも応答を組み立てるときも
    /// 参照カウントを増やすだけで、1応答あたり最大 2 MiB の本文を複製しません。
    /// 読み取りは `&str` へ自動的に参照外しされるため、利用側は `String` だった
    /// ときと同じように扱えます（JSON へ直列化した結果も変わりません）。
    pub raw_text: Arc<str>,
    /// 読み込み元ラベル（来歴。`LOG-007` の下地）。
    pub source_label: String,
    pub source_line_number: u64,
    /// 未確定行（書き込み途中の可能性がある末尾断片）ではないか（`LOG-026`）。
    /// 解析エラーとは区別する表示メタデータです（P08-1 追加）。
    pub confirmed: bool,
    /// 結合された継続行（`LOG-014`）の数。0 は継続行なし。表示側の行高導出に
    /// 使います（P08-1 追加）。
    pub continuation_count: u32,
    /// 日時未解析の生データ項目か（`timestamp_display.is_none()` と同値。
    /// フロントエンドが `null` 判定に頼らず明示的に分岐できるように追加した
    /// 表示メタデータです。P08-1 追加）。
    pub raw_display: bool,
}

/// 範囲取得の応答です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeResponse {
    pub generation: u64,
    pub total_items: u64,
    /// 実際に返した先頭項目のインデックス（要求の `start` を `total_items` で
    /// 切り詰めた値。要求の `start` をそのまま反映するため、要求値と一致する
    /// のが通常）。
    pub start: u64,
    pub items: Vec<ItemDto>,
    /// [`MAX_ITEMS_PER_RESPONSE`]・[`MAX_RESPONSE_RAW_BYTES`] のいずれかで
    /// 打ち切られ、`start + items.len()` が `total_items` に届いていない場合
    /// `true`。
    pub truncated: bool,
}

/// 範囲取得が失敗した理由です。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeFetchError {
    /// 要求した世代が現在の世代と一致しない（契約に織り込む4点の4。`LOG-023`・
    /// `LOG-028` の下地）。
    GenerationMismatch { expected: u64, current: u64 },
}

impl std::fmt::Display for RangeFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RangeFetchError::GenerationMismatch { expected, current } => write!(
                f,
                "表示集合の世代が一致しません（要求 {expected}、現在 {current}）。\
                 表示集合が再構築されています。最新の世代で範囲を取得し直してください。"
            ),
        }
    }
}

impl std::error::Error for RangeFetchError {}

/// 索引レベルの範囲取得応答です（P08-5）。本文はまだ読み出していません。
/// [`crate::registry::DisplaySetRegistry::fetch_range`] がこれを受け取り、
/// オンデマンドで本文を読み出して最終的な [`RangeResponse`] を組み立てます。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexRangeResponse {
    pub generation: u64,
    pub total_items: u64,
    pub start: u64,
    pub items: Vec<IndexItemRef>,
    pub truncated: bool,
}

/// 索引レベルの1項目です（P08-5）。本文（`raw_text`）・表示用日時文字列
/// （`timestamp_display`）はまだ持たず、それらを導出するために必要な生バイト
/// 範囲（`raw_offset`・`raw_byte_len`）と、それ以外の表示メタデータ（索引だけ
/// から導出できるもの）を持ちます。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexItemRef {
    pub item_id: ItemId,
    pub source_id: u32,
    pub source_label: String,
    pub source_line_number: u64,
    pub confirmed: bool,
    pub continuation_count: u32,
    pub has_timestamp: bool,
    /// ソースファイル先頭からの生バイトオフセット（BOM を除く）。
    pub raw_offset: u64,
    /// 項目本体（継続行を含む）の生バイト長。
    pub raw_byte_len: u32,
}

/// 表示集合です。順序付きの項目列であり、順序がどう作られたか（単一ファイル
/// 順か、将来のマージ結果か）に依存しません。P04 では単一ソースのファイル順で
/// 構築します（`crate::loader`）。
///
/// `texts` はソース単位（`source_id` 起点）で保持します。`items` はどのソース
/// の項目が混在してもよい順序付き列であり、各 `Item::entry_index` は
/// `texts[item.id.source_id]` の中での添字です。
///
/// P08-5 より前は、ソースごとに確定した日時書式
/// （`timestamp_display` 再構成用）もここで保持していましたが、本文の
/// オンデマンド読み出し（`crate::registry::DisplaySetRegistry::fetch_range`）
/// が `crate::registry` 内の `SourceRecord::datetime_format` を直接使うように
/// なったため、`DisplaySet` からは削除しました（索引レベルでは
/// `IndexItemRef::has_timestamp`（真偽値）だけが必要です）。
#[derive(Debug)]
pub struct DisplaySet {
    generation: u64,
    items: Vec<Item>,
    sources: HashMap<u32, SourceInfo>,
    /// ソースごとの行索引（P08-5: 本文は保持しません。`crate::line_index` の
    /// モジュール doc コメント参照）。
    texts: HashMap<u32, IndexedText>,
}

impl DisplaySet {
    /// 新規に表示集合を構築します。世代は 1 から始まります（契約に織り込む
    /// 4点の4）。
    #[must_use]
    pub fn new(
        sources: Vec<SourceInfo>,
        items: Vec<Item>,
        texts: HashMap<u32, IndexedText>,
    ) -> Self {
        Self::at_generation(1, sources, items, texts)
    }

    /// 表示集合を再構築し、世代を1つ進めた新しいインスタンスを返します
    /// （`LOG-023`「索引を無効化」・`LOG-028`「明示的な再読み込み」の下地）。
    /// 元のインスタンス（古い世代）は変更されません。
    #[must_use]
    pub fn rebuild(
        &self,
        sources: Vec<SourceInfo>,
        items: Vec<Item>,
        texts: HashMap<u32, IndexedText>,
    ) -> Self {
        Self::at_generation(self.generation + 1, sources, items, texts)
    }

    fn at_generation(
        generation: u64,
        sources: Vec<SourceInfo>,
        items: Vec<Item>,
        texts: HashMap<u32, IndexedText>,
    ) -> Self {
        DisplaySet {
            generation,
            items,
            sources: sources
                .into_iter()
                .map(|source| (source.source_id, source))
                .collect(),
            texts,
        }
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn total_items(&self) -> u64 {
        self.items.len() as u64
    }

    /// 読み込み途中の解析済み範囲から表示集合を伸長する（P06-2、作業項目1
    /// 「読み込み途中でも解析済み範囲から表示集合を伸長できる」）ために、
    /// 項目列と `source_id` の索引を**同時に**可変で借ります。
    ///
    /// [`Self::rebuild`] と異なり、世代（[`Self::generation`]）は変えません。
    ///
    /// 2つをまとめて返すのは、伸長時の予約量が「項目列の余剰容量」と
    /// 「索引の余剰容量」の両方に依存するためです
    /// （`crate::item::build_items_from_pending_into`）。別々の取得メソッドに
    /// 分けると、同じ `DisplaySet` から2つの可変参照を得られません。
    pub(crate) fn items_and_text_mut(
        &mut self,
        source_id: u32,
    ) -> Option<(&mut Vec<Item>, &mut IndexedText)> {
        let text = self.texts.get_mut(&source_id)?;
        Some((&mut self.items, text))
    }

    /// `source_id` の [`IndexedText`] への不変参照です（P09-1: 統合表示集合の
    /// 構築時、各ソースの索引を複製せず読むために使います。
    /// `crate::registry::DisplaySetRegistry::compute_merged_order` 参照）。
    pub(crate) fn text(&self, source_id: u32) -> Option<&IndexedText> {
        self.texts.get(&source_id)
    }

    /// 索引レベルで範囲を取得します（契約に織り込む4点の1〜4を実装する中心
    /// 関数。P08-5: 本文はまだ読み出しません。`crate::registry::
    /// DisplaySetRegistry::fetch_range` が本文のオンデマンド読み出しを担い、
    /// 最終的な [`RangeResponse`]／[`ItemDto`] を組み立てます）。
    ///
    /// # 転送上限（暫定値。[`MAX_ITEMS_PER_RESPONSE`]・[`MAX_RESPONSE_RAW_BYTES`]）
    ///
    /// - 項目数は [`MAX_ITEMS_PER_RESPONSE`] と `request.max_items` の小さい方まで。
    /// - 原文合計バイト数（索引の `byte_len`、生バイト）が [`MAX_RESPONSE_RAW_BYTES`]
    ///   を超える**手前**で打ち切ります。ただし、1件も返せないまま打ち切ると
    ///   呼び出し側が永久に前進できなくなるため、**少なくとも1件は必ず返します**。
    /// - 打ち切った場合 `truncated = true` を返します。
    pub(crate) fn fetch_range_index(
        &self,
        request: RangeRequest,
    ) -> Result<IndexRangeResponse, RangeFetchError> {
        if request.expected_generation != self.generation {
            return Err(RangeFetchError::GenerationMismatch {
                expected: request.expected_generation,
                current: self.generation,
            });
        }

        let total_items = self.total_items();
        let start = request.start.min(total_items);
        let effective_max_items = request.max_items.min(MAX_ITEMS_PER_RESPONSE);

        let mut items = Vec::new();
        let mut raw_bytes_total: usize = 0;

        for item in self
            .items
            .iter()
            .skip(usize::try_from(start).unwrap_or(usize::MAX))
            .take(effective_max_items as usize)
        {
            let item_bytes = self
                .texts
                .get(&item.id.source_id)
                .and_then(|text| text.entries().get(item.entry_index))
                .map(|entry| entry.byte_len as usize)
                .unwrap_or(0);
            if !items.is_empty()
                && raw_bytes_total.saturating_add(item_bytes) > MAX_RESPONSE_RAW_BYTES
            {
                break;
            }
            items.push(self.to_index_ref(item));
            raw_bytes_total = raw_bytes_total.saturating_add(item_bytes);
        }

        let truncated = start.saturating_add(items.len() as u64) < total_items;

        Ok(IndexRangeResponse {
            generation: self.generation,
            total_items,
            start,
            items,
            truncated,
        })
    }

    /// `item` を [`IndexItemRef`] へ変換します。参照先のソース・エントリが
    /// 存在しない（未知のソース ID など）場合は panic せず既定値
    /// （空文字列・`has_timestamp: false`・`confirmed: true`）へ
    /// フォールバックします。
    ///
    /// `pub(crate)` なのは、統合表示集合（P09-1）の範囲取得
    /// （`crate::registry::DisplaySetRegistry::fetch_merged_range`）が、
    /// 統合結果の参照列（`ItemId` の並び）が指す項目それぞれについて、
    /// **その項目が属する単独ソースの `DisplaySet`** からこのメソッドを直接
    /// 呼び出すためです（統合表示集合自身は各ソースの索引を複製しないため、
    /// 常に単独ソース側の `DisplaySet` を経由します）。
    pub(crate) fn to_index_ref(&self, item: &Item) -> IndexItemRef {
        let source_id = item.id.source_id;
        let source_label = self
            .sources
            .get(&source_id)
            .map(|source| source.label.clone())
            .unwrap_or_default();

        let text = self.texts.get(&source_id);
        let entry = text.and_then(|text| text.entries().get(item.entry_index));
        let source_line_number = text
            .and_then(|text| text.source_line_number(item.entry_index))
            .unwrap_or(0);
        let confirmed = entry.map(|entry| !entry.is_unconfirmed()).unwrap_or(true);
        let continuation_count = entry
            .map(|entry| u32::from(entry.continuation_count))
            .unwrap_or(0);
        let has_timestamp = entry.map(|entry| entry.has_timestamp()).unwrap_or(false);
        let (raw_offset, raw_byte_len) = entry.map(|entry| entry.raw_range()).unwrap_or((0, 0));

        IndexItemRef {
            item_id: item.id,
            source_id,
            source_label,
            source_line_number,
            confirmed,
            continuation_count,
            has_timestamp,
            raw_offset,
            raw_byte_len,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用に、1ソース分の `SourceInfo`・`IndexedText`・`Item` 列をまとめて
    /// 構築します。`lines` は `(source_line_number, raw_offset, raw_byte_len)`
    /// の並びです。日時は付けません（生データ項目）。
    fn build_raw_source(
        source_id: u32,
        label: &str,
        lines: &[(u64, u64, u32)],
    ) -> (SourceInfo, IndexedText, Vec<Item>) {
        let mut text = IndexedText::new();
        let mut items = Vec::new();
        for (seq, (line_number, raw_offset, raw_byte_len)) in lines.iter().enumerate() {
            let entry_index =
                text.push_entry(*raw_offset, *raw_byte_len, None, false, 0, *line_number);
            items.push(Item {
                id: ItemId {
                    source_id,
                    seq: seq as u64,
                },
                entry_index,
            });
        }
        (
            SourceInfo {
                source_id,
                label: label.to_string(),
            },
            text,
            items,
        )
    }

    fn single_source_display_set(
        source_id: u32,
        label: &str,
        lines: &[(u64, u64, u32)],
    ) -> DisplaySet {
        let (info, text, items) = build_raw_source(source_id, label, lines);
        let mut texts = HashMap::new();
        texts.insert(source_id, text);
        DisplaySet::new(vec![info], items, texts)
    }

    // 受け入れ条件: 同一要求の反復で同一の応答（順序・識別子）が返る。
    #[test]
    fn repeated_fetch_with_same_request_returns_identical_response() {
        let display_set =
            single_source_display_set(1, "a.log", &[(1, 0, 8), (2, 9, 8), (3, 18, 10)]);
        let request = RangeRequest {
            start: 0,
            max_items: 10,
            expected_generation: 1,
        };

        let first = display_set
            .fetch_range_index(request)
            .expect("成功するはず");
        let second = display_set
            .fetch_range_index(request)
            .expect("成功するはず");

        assert_eq!(first, second);
        assert_eq!(first.items.len(), 3);
        assert_eq!(first.total_items, 3);
        assert!(!first.truncated);
    }

    // 受け入れ条件: 複数ソース相当の試験データで、範囲取得・再取得の順序・
    // 識別子・来歴が決定的（マージ実装そのものは作らず、順序は試験データ側で
    // 明示的に与える）。
    #[test]
    fn multi_source_like_fixture_preserves_deterministic_order_ids_and_provenance() {
        let mut text_10 = IndexedText::new();
        let mut text_20 = IndexedText::new();

        let entry_10_0 = text_10.push_entry(0, 30, Some(1000), false, 0, 1);
        let entry_20_0 = text_20.push_entry(0, 33, Some(1000), false, 0, 1);
        let entry_10_1 = text_10.push_entry(31, 30, Some(2000), false, 0, 2);

        let items = vec![
            Item {
                id: ItemId {
                    source_id: 10,
                    seq: 0,
                },
                entry_index: entry_10_0,
            },
            Item {
                id: ItemId {
                    source_id: 20,
                    seq: 0,
                },
                entry_index: entry_20_0,
            },
            Item {
                id: ItemId {
                    source_id: 10,
                    seq: 1,
                },
                entry_index: entry_10_1,
            },
        ];

        let sources = vec![
            SourceInfo {
                source_id: 10,
                label: "app.log".to_string(),
            },
            SourceInfo {
                source_id: 20,
                label: "worker.log".to_string(),
            },
        ];
        let mut texts = HashMap::new();
        texts.insert(10, text_10);
        texts.insert(20, text_20);

        let display_set = DisplaySet::new(sources, items, texts);
        let request = RangeRequest {
            start: 0,
            max_items: 10,
            expected_generation: 1,
        };

        let first = display_set
            .fetch_range_index(request)
            .expect("成功するはず");
        let second = display_set
            .fetch_range_index(request)
            .expect("再取得も成功するはず");

        assert_eq!(first, second, "再取得しても同じ応答になるはず");
        assert_eq!(first.items.len(), 3);

        assert_eq!(
            first.items[0].item_id,
            ItemId {
                source_id: 10,
                seq: 0
            }
        );
        assert_eq!(
            first.items[1].item_id,
            ItemId {
                source_id: 20,
                seq: 0
            }
        );
        assert_eq!(
            first.items[2].item_id,
            ItemId {
                source_id: 10,
                seq: 1
            }
        );

        assert!(first.items[0].has_timestamp);
        assert!(first.items[1].has_timestamp);
        assert_ne!(first.items[0].item_id, first.items[1].item_id);

        assert_eq!(first.items[0].source_label, "app.log");
        assert_eq!(first.items[1].source_label, "worker.log");
        assert_eq!(first.items[2].source_label, "app.log");
    }

    // 受け入れ条件: 世代不一致の検出（再構築後に古い世代の要求がエラーになる）。
    #[test]
    fn stale_generation_request_is_rejected_after_rebuild() {
        let (info, text, items) = build_raw_source(1, "a.log", &[(1, 0, 2)]);
        let mut texts = HashMap::new();
        texts.insert(1, text);
        let original = DisplaySet::new(vec![info.clone()], items, texts);
        assert_eq!(original.generation(), 1);

        let (_info2, text2, items2) = build_raw_source(1, "a.log", &[(1, 0, 2), (2, 3, 3)]);
        let mut texts2 = HashMap::new();
        texts2.insert(1, text2);
        let rebuilt = original.rebuild(vec![info], items2, texts2);
        assert_eq!(rebuilt.generation(), 2, "再構築で世代が1つ進むはず");

        let stale_request = RangeRequest {
            start: 0,
            max_items: 10,
            expected_generation: 1,
        };
        let error = rebuilt
            .fetch_range_index(stale_request)
            .expect_err("古い世代の要求は拒否されるはず");
        assert_eq!(
            error,
            RangeFetchError::GenerationMismatch {
                expected: 1,
                current: 2
            }
        );

        let fresh_request = RangeRequest {
            start: 0,
            max_items: 10,
            expected_generation: 2,
        };
        let response = rebuilt
            .fetch_range_index(fresh_request)
            .expect("最新世代なら成功するはず");
        assert_eq!(response.items.len(), 2);
        assert_eq!(response.items[0].raw_byte_len, 2);
    }

    // 受け入れ条件: 転送上限（513項目要求で512に切られる）。
    #[test]
    fn item_count_is_capped_at_max_items_per_response() {
        let lines: Vec<(u64, u64, u32)> = (0..600).map(|seq| (seq + 1, seq * 2, 1)).collect();
        let display_set = single_source_display_set(1, "a.log", &lines);

        let request = RangeRequest {
            start: 0,
            max_items: MAX_ITEMS_PER_RESPONSE + 1, // 513
            expected_generation: 1,
        };
        let response = display_set
            .fetch_range_index(request)
            .expect("成功するはず");

        assert_eq!(response.items.len(), MAX_ITEMS_PER_RESPONSE as usize);
        assert!(
            response.truncated,
            "600件中512件しか返していないので打ち切りのはず"
        );
    }

    // 受け入れ条件: 巨大な行で2 MiB上限が効きtruncatedになる。
    #[test]
    fn raw_bytes_cap_truncates_before_hitting_item_count_cap() {
        // 1件あたり 700 KiB の索引長を4件用意する。2件（約1.4 MiB）までは
        // 2 MiB 以内に収まるが、3件目（約2.05 MiB）で上限を超える。
        let big_len = 700 * 1024u32;
        let lines: Vec<(u64, u64, u32)> = (0..4u64)
            .map(|seq| (seq + 1, seq * u64::from(big_len), big_len))
            .collect();
        let display_set = single_source_display_set(1, "a.log", &lines);

        let request = RangeRequest {
            start: 0,
            max_items: 10,
            expected_generation: 1,
        };
        let response = display_set
            .fetch_range_index(request)
            .expect("成功するはず");

        assert_eq!(response.items.len(), 2);
        assert!(response.truncated);
        let total_bytes: usize = response
            .items
            .iter()
            .map(|item| item.raw_byte_len as usize)
            .sum();
        assert!(total_bytes <= MAX_RESPONSE_RAW_BYTES);
    }

    // 受け入れ条件（進行保証）: 1件だけで2 MiB上限を超える巨大な行でも、
    // 少なくとも1件は返す。
    #[test]
    fn single_item_larger_than_cap_is_still_returned_alone() {
        let huge_len = u32::try_from(MAX_RESPONSE_RAW_BYTES + 100).unwrap();
        let display_set =
            single_source_display_set(1, "a.log", &[(1, 0, huge_len), (2, u64::from(huge_len), 5)]);

        let request = RangeRequest {
            start: 0,
            max_items: 10,
            expected_generation: 1,
        };
        let response = display_set
            .fetch_range_index(request)
            .expect("成功するはず");

        assert_eq!(response.items.len(), 1, "巨大な1件目だけは必ず返す");
        assert_eq!(response.items[0].raw_byte_len, huge_len);
        assert!(response.truncated, "2件目が残っているので打ち切り扱い");
    }

    // 境界値: start が total_items 以上のときは空の応答（エラーにはしない）。
    #[test]
    fn start_beyond_total_items_yields_empty_not_truncated_response() {
        let display_set = single_source_display_set(1, "a.log", &[(1, 0, 4)]);

        let request = RangeRequest {
            start: 100,
            max_items: 10,
            expected_generation: 1,
        };
        let response = display_set
            .fetch_range_index(request)
            .expect("成功するはず");

        assert_eq!(response.start, 1, "total_items(1)まで切り詰められるはず");
        assert!(response.items.is_empty());
        assert!(
            !response.truncated,
            "これ以上残っていないので打ち切りではない"
        );
    }

    // 未知のソース ID を参照する項目があっても panic せず、来歴は空文字列になる
    // （通常発生しない防御的な経路の確認）。
    #[test]
    fn unknown_source_id_falls_back_to_empty_label_without_panicking() {
        let display_set = single_source_display_set(1, "a.log", &[(1, 0, 4)]);
        let mut display_set = display_set;
        display_set.items.push(Item {
            id: ItemId {
                source_id: 999,
                seq: 0,
            },
            entry_index: 0,
        });

        let request = RangeRequest {
            start: 0,
            max_items: 10,
            expected_generation: 1,
        };
        let response = display_set
            .fetch_range_index(request)
            .expect("成功するはず");

        assert_eq!(response.items[1].source_label, "");
        assert_eq!(response.items[1].raw_byte_len, 0);
        assert!(!response.items[1].has_timestamp, "既定はraw_display扱い");
        assert!(response.items[1].confirmed, "既定はconfirmed扱い");
    }

    // 受け入れ条件（P08-1 追加メタデータ）: 未確定行・生表示・継続行数が索引
    // レベルの応答に正しく載る。
    #[test]
    fn index_ref_carries_confirmed_raw_display_and_continuation_count() {
        let mut text = IndexedText::new();
        let dated_with_continuation = text.push_entry(0, 40, Some(1), false, 2, 1);
        let raw_unconfirmed = text.push_entry(41, 10, None, true, 0, 4);

        let items = vec![
            Item {
                id: ItemId {
                    source_id: 1,
                    seq: 0,
                },
                entry_index: dated_with_continuation,
            },
            Item {
                id: ItemId {
                    source_id: 1,
                    seq: 1,
                },
                entry_index: raw_unconfirmed,
            },
        ];
        let mut texts = HashMap::new();
        texts.insert(1, text);
        let display_set = DisplaySet::new(
            vec![SourceInfo {
                source_id: 1,
                label: "a.log".to_string(),
            }],
            items,
            texts,
        );

        let response = display_set
            .fetch_range_index(RangeRequest {
                start: 0,
                max_items: 10,
                expected_generation: 1,
            })
            .expect("成功するはず");

        assert_eq!(response.items[0].continuation_count, 2);
        assert!(response.items[0].confirmed);
        assert!(response.items[0].has_timestamp);

        assert_eq!(response.items[1].continuation_count, 0);
        assert!(
            !response.items[1].confirmed,
            "未確定行はconfirmed=falseのはず"
        );
        assert!(
            !response.items[1].has_timestamp,
            "日時未解析はhas_timestamp=falseのはず"
        );
    }

    // --- メモリ実測比較（P08-1 作業項目4。P08-5 で更新） ---

    // 受け入れ条件: N行のファイルで「索引 + 付加情報」の合計が、旧構造
    // （P08-1 より前の時点。行ごとに `Item { raw_text: String, timestamp: Option<
    // DateTimeMatch>, comparison_key: Option<ComparisonKey>,
    // source_line_number: u64, log_level: Option<String> }` を所有する構造）
    // より小さいことを実測で確認する。P08-5 では本文バッファすら持たないため、
    // P08-1 時点よりさらに削減されていることも確認する。
    #[test]
    fn indexed_text_uses_less_memory_than_the_old_per_item_owned_string_estimate() {
        const LINE_COUNT: usize = 20_000;
        let lines: Vec<String> = (0..LINE_COUNT)
            .map(|i| format!("2026/07/28 15:12:23.{i:03} 行番号{i}のテスト本文です"))
            .collect();

        // --- 旧構造の概算（doc コメントの内訳、P08-1 時点） ---
        const OLD_ITEM_FIXED_OVERHEAD_BYTES: usize = 150;
        const OLD_MATCHED_TEXT_HEAP_BYTES: usize = 24;
        let old_total: usize = lines
            .iter()
            .map(|line| OLD_ITEM_FIXED_OVERHEAD_BYTES + OLD_MATCHED_TEXT_HEAP_BYTES + line.len())
            .sum();

        // --- 新構造（IndexedText、P08-5: 本文を保持しない）の実測 ---
        let mut indexed = IndexedText::new();
        let mut offset = 0u64;
        for (i, line) in lines.iter().enumerate() {
            let len = u32::try_from(line.len()).unwrap();
            let millis = 1_753_679_543_000i64 + i as i64;
            indexed.push_entry(offset, len, Some(millis), false, 0, i as u64 + 1);
            offset += u64::from(len) + 1;
        }
        let new_total = indexed.index_bytes() + indexed.auxiliary_bytes();

        assert!(
            new_total < old_total,
            "新構造 {new_total} バイトが旧構造の概算 {old_total} バイト以上になっている"
        );
        let ratio = new_total as f64 / old_total as f64;
        assert!(
            ratio < 0.9,
            "新構造が旧構造の90%以上を占めている（削減効果が乏しい）: 比率={ratio:.3}\
             （new={new_total}, old={old_total}）"
        );
        // P08-5 は本文を一切保持しないため、1行あたり32バイト（24+8）固定に
        // なるはず（P08-1 時点は本文サイズに比例して増えていた）。
        assert_eq!(new_total, LINE_COUNT * 32);
    }
}
