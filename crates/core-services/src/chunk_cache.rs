//! デコード済みチャンクの有界キャッシュ（P08-5）。
//!
//! オンデマンド読み出し（`crate::registry::DisplaySetRegistry::fetch_range`）は、
//! 範囲取得のたびにソースファイルから生バイトを読み、デコードします。同一
//! 範囲への繰り返しアクセス（例: 同じ表示位置への再取得、複数 UI コンポーネント
//! からの重複要求）のコストを抑えるため、直近デコードした結果を有界（LRU、
//! 最大 [`MAX_CACHED_CHUNKS`] 件）でキャッシュします。
//!
//! # 暫定値
//!
//! - 1件の「チャンク」は、1回の範囲取得応答が対象とする項目群（1ソース分）を
//!   単位とします。応答1件あたりの原文合計は
//!   `crate::display_set::MAX_RESPONSE_RAW_BYTES`（2 MiB）で上限が掛かっている
//!   ため、通常はこの単位が「2 MiB 程度のチャンク」に自然と対応します。
//! - [`MAX_CACHED_CHUNKS`]（8件）は暫定値です。値の最終決定は将来の実測に
//!   委ねます。
//!
//! # 照合の規則
//!
//! v1 の照合は「同一ソース・同一の先頭オフセット・同一件数」の
//! **完全一致**だけでした。現在は**包含判定**へ広げ、要求した項目群が
//! キャッシュ済みエントリの項目列に**連続部分列として完全に含まれる**場合も
//! ヒットとします。完全一致は、その部分列がエントリ全体と一致する特別な場合
//! として引き続きヒットします。
//!
//! 部分的な重なり（要求範囲の一部だけがエントリに含まれる場合）を複数
//! エントリやファイル読み出しと**合成して1件の応答を組み立てることはしません**。
//! 合成には「どこまでをどのエントリから取ったか」を追う状態が増え、境界の
//! 取り違えが混入する余地が広がる一方、包含判定だけでも本来の受益者
//! （下記「効果の範囲」）は救えるためです。
//!
//! ## 項目単位の対応付けを取り違えない方法
//!
//! 包含判定は、項目の [`ChunkItemSpan`]（生バイトの開始オフセットと長さ）の
//! 組を要素ごとに突き合わせて行います。要求した項目群の並びが、エントリの
//! 並びのある位置から**1件ずつ完全に一致**した場合だけヒットとするため、
//! 「ずれた位置の本文を返す」ことが起こりません。
//!
//! 開始オフセットだけで対応付けないのは、表示集合の再構築などで項目の切れ目
//! （継続行のまとめ方）が変わったとき、開始位置が同じでも中身の異なる項目に
//! なり得るためです。長さも突き合わせることで、その場合はヒットしません。
//! この点で、先頭オフセットと件数だけを見ていた v1 の完全一致キーよりも
//! 照合は厳しくなっています（v1 は先頭が一致して件数が同じなら、2件目以降の
//! 切れ目が変わっていてもヒットしていました）。
//!
//! ## 効果の範囲
//!
//! GUI の仮想スクロール経路（`src/log_view.js`）は `CHUNK_SIZE = 512` を
//! `crate::display_set::MAX_ITEMS_PER_RESPONSE` と一致させており、範囲取得は
//! 常に512件境界へ整列しています。そのため同じチャンクの取り直しは v1 の
//! 完全一致でも既にヒットしており、包含判定がヒット率を動かすのは境界が
//! 整列しない呼び出し側です。具体的には `crate::copy`（利用者が選んだ任意の
//! 開始位置・件数でコピーする経路）、統合表示集合（ソースごとに分割された
//! グループの先頭と件数が要求ごとに変わる）、および任意の位置を要求する
//! 計測・診断経路です。
//!
//! # メモリ会計
//!
//! 追加時に `hakutaku_memory_accounting::global_budget()` へ予約し
//! （`PERF-008`・`PERF-010`）、拒否された場合はキャッシュへ追加しません
//! （キャッシュは最適化であり、失敗しても範囲取得そのものは継続します。
//! 呼び出し側は既にデコード済みの結果をそのまま応答として使い、キャッシュ
//! できなかっただけです）。成功時は直ちに実確保へ振り替えます。実際の
//! ヒープ確保・解放は `hakutaku_memory_accounting::CountingAllocator` が
//! 自動的に計上・減算するため、追い出し（evict）・クリア時に明示的な会計
//! 操作は不要です（最後の `Arc` が落ちた時点で自動的に `allocated_bytes` から
//! 減ります）。
//!
//! 予約量には本文の実バイト数に加えて [`ChunkItemSpan`] の列の分を含めます。
//! この列は包含判定のためにキャッシュ側が新たに確保して保持し続けるもので、
//! 応答とは共有しないためです（1エントリあたり最大512件 = 8 KiB 程度）。
//!
//! # 応答との本文共有
//!
//! 本文（`Arc<str>`）は範囲取得の応答（`crate::display_set::ItemDto::raw_text`）
//! と共有し、キャッシュへの格納・取り出しのどちらでも複製しません。以前は
//! 格納時と取り出し時にそれぞれ全体（1応答あたり最大 2 MiB・512 件）を複製して
//! いました。共有しても、応答が解放されたあと本文を生かし続けるのはこの
//! キャッシュなので、上記の予約量の意味は変わりません。
//!
//! 包含ヒットでも本文は複製しません。[`ChunkCacheHit`] はエントリ全体の共有
//! ハンドルと開始位置を返すだけで、呼び出し側は必要な範囲の `Arc<str>` の
//! 参照カウントを増やすだけで応答を組み立てられます。

use std::sync::Arc;

/// 有界キャッシュに保持するチャンク数の上限（暫定値）。
pub(crate) const MAX_CACHED_CHUNKS: usize = 8;

/// キャッシュ済みチャンクの1項目が、ソースファイル上で占める生バイト範囲です。
///
/// 包含判定はこの組の完全一致で項目単位の対応付けを行います（モジュール doc
/// コメント「項目単位の対応付けを取り違えない方法」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChunkItemSpan {
    pub(crate) raw_offset: u64,
    pub(crate) raw_byte_len: u32,
}

/// 包含ヒットの結果です。
///
/// 要求した項目群の本文は `items[offset..offset + 要求件数]` に、要求したのと
/// 同じ並びで入っています。要求分だけを切り出した新しい `Arc<[_]>` を返さない
/// のは、切り出しにヒープ確保と本文ハンドル全件の複製が必要になり、参照
/// カウントの増加だけで済ませるという応答との本文共有の利点を失うためです。
#[derive(Debug)]
pub(crate) struct ChunkCacheHit {
    /// キャッシュ済みエントリ全体の共有ハンドル。
    pub(crate) items: Arc<[Arc<str>]>,
    /// 要求した項目群が `items` の中で始まる位置。
    pub(crate) offset: usize,
}

#[derive(Debug)]
struct CachedChunk {
    source_id: u32,
    /// 各項目の生バイト範囲（`items` と同じ並び・同じ長さ）。包含判定の
    /// 突き合わせに使います。
    spans: Box<[ChunkItemSpan]>,
    /// デコード済み・`\r\n` 正規化済みの本文（格納時に要求された並び順）。
    ///
    /// 本文は応答（`crate::display_set::ItemDto::raw_text`）と共有します。
    /// 外側の `Arc<[_]>` で並び全体を、内側の `Arc<str>` で
    /// 各項目の本文を共有するため、格納にも取り出しにも複製が起きません。
    items: Arc<[Arc<str>]>,
}

/// デコード済みチャンクの有界 LRU キャッシュです。
#[derive(Debug, Default)]
pub(crate) struct DecodedChunkCache {
    /// 先頭が最も新しく使われたエントリ（MRU）、末尾が最も古い（LRU）。
    entries: Vec<CachedChunk>,
}

impl DecodedChunkCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// `wanted` を包含するキャッシュ済みチャンクがあれば返します（見つかれば
    /// そのエントリを MRU 側へ移動します）。
    ///
    /// 返すのは共有ハンドルの複製（参照カウントの増加）だけで、本文は
    /// 複製しません。呼び出し側が保持している間もキャッシュ側の
    /// エントリは有効なままで、追い出されても `Arc` が生きているので本文は
    /// 解放されません。
    pub(crate) fn get(
        &mut self,
        source_id: u32,
        wanted: &[ChunkItemSpan],
    ) -> Option<ChunkCacheHit> {
        if wanted.is_empty() {
            return None;
        }

        // MRU 側から順に見るため、複数のエントリが同じ範囲を包含する場合は
        // 最も新しく使われたものが選ばれる。
        let (position, offset) =
            self.entries
                .iter()
                .enumerate()
                .find_map(|(position, entry)| {
                    if entry.source_id != source_id {
                        return None;
                    }
                    containment_offset(&entry.spans, wanted).map(|offset| (position, offset))
                })?;

        let entry = self.entries.remove(position);
        let items = Arc::clone(&entry.items);
        self.entries.insert(0, entry);
        Some(ChunkCacheHit { items, offset })
    }

    /// デコード結果を追加します（メモリ予約に失敗した場合は何もしません）。
    ///
    /// `spans` は `items` と同じ並び・同じ長さの生バイト範囲で、包含判定に
    /// 使います。`items` は呼び出し側（範囲取得の応答）と共有されます。
    /// 予約する本文の量は実バイト数のままで構いません。応答が解放された
    /// あとも本文を生かし続けるのはこのキャッシュであり、「キャッシュが保持し
    /// 続ける量」という会計の意味は共有化の前後で変わらないためです。
    pub(crate) fn insert(
        &mut self,
        source_id: u32,
        spans: Box<[ChunkItemSpan]>,
        items: Arc<[Arc<str>]>,
    ) {
        // 包含判定は `spans` と `items` の位置が1対1で対応していることに依存
        // する。長さが食い違うエントリを入れると包含ヒットが別項目の本文を
        // 返し得るため、`debug_assert` で開発時に気付けるようにするのではなく、
        // 常に破棄する（キャッシュは最適化であり、疑わしい入力は捨てても
        // 範囲取得そのものは続くため、これが最も安全な失敗の仕方になる）。
        if items.is_empty() || spans.len() != items.len() {
            return;
        }

        let text_bytes: usize = items.iter().map(|item| item.len()).sum();
        let span_bytes = spans.len() * std::mem::size_of::<ChunkItemSpan>();
        let total_bytes = text_bytes.saturating_add(span_bytes);
        let Ok(token) = hakutaku_memory_accounting::global_budget().reserve(total_bytes) else {
            return;
        };
        let _ = token.mark_allocated(total_bytes);
        drop(token);

        // 同一内容のエントリを重複させない（同じ範囲を二重に会計しない）。
        self.entries
            .retain(|entry| entry.source_id != source_id || *entry.spans != *spans);
        self.entries.insert(
            0,
            CachedChunk {
                source_id,
                spans,
                items,
            },
        );
        while self.entries.len() > MAX_CACHED_CHUNKS {
            self.entries.pop();
        }
    }

    /// 指定ソースのキャッシュ済みチャンクをすべて取り除きます（close・変更検知・
    /// 再読み込み・再構築・P08-3 の解放要求で呼びます）。
    pub(crate) fn invalidate_source(&mut self, source_id: u32) {
        self.entries.retain(|entry| entry.source_id != source_id);
    }
}

/// `wanted` が `cached` の連続部分列として現れる先頭位置を返します。
///
/// 全開始位置を試す総当たりですが、スライス比較は最初の不一致で打ち切られ、
/// かつ同一ソース内で項目の開始オフセットは重複しない（各項目は異なるバイト
/// 位置から始まる）ため、先頭要素まで一致する開始位置は実際には高々1つです。
/// したがって比較回数は `cached` の長さ程度に収まります。1エントリの項目数は
/// `crate::display_set::MAX_ITEMS_PER_RESPONSE`（512件）で頭打ちであり、
/// 部分列探索の専用アルゴリズムを持ち込む価値はありません。
fn containment_offset(cached: &[ChunkItemSpan], wanted: &[ChunkItemSpan]) -> Option<usize> {
    // 要求のほうが長ければ包含され得ない（checked_sub で下回りを弾く）。
    let last_start = cached.len().checked_sub(wanted.len())?;
    (0..=last_start).find(|&start| cached[start..start + wanted.len()] == *wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(raw_offset: u64, raw_byte_len: u32) -> ChunkItemSpan {
        ChunkItemSpan {
            raw_offset,
            raw_byte_len,
        }
    }

    fn texts(values: &[&str]) -> Arc<[Arc<str>]> {
        Arc::from(
            values
                .iter()
                .map(|value| Arc::<str>::from(*value))
                .collect::<Vec<_>>(),
        )
    }

    /// 包含ヒットの結果を、要求件数分の文字列として取り出す。
    fn hit_texts(hit: &ChunkCacheHit, count: usize) -> Vec<&str> {
        hit.items[hit.offset..hit.offset + count]
            .iter()
            .map(|item| &**item)
            .collect()
    }

    /// 3件（オフセット 0/10/20）のエントリを1件だけ持つキャッシュ。
    fn cache_with_three_items() -> DecodedChunkCache {
        let mut cache = DecodedChunkCache::new();
        cache.insert(
            1,
            vec![span(0, 10), span(10, 10), span(20, 10)].into_boxed_slice(),
            texts(&["a", "b", "c"]),
        );
        cache
    }

    // 受け入れ条件: 追加した内容が、同じ範囲の要求で取り出せる（完全一致は
    // 包含の特別な場合としてヒットする）。
    #[test]
    fn insert_then_get_with_identical_range_returns_shared_items() {
        let mut cache = cache_with_three_items();

        let wanted = [span(0, 10), span(10, 10), span(20, 10)];
        let hit = cache.get(1, &wanted).expect("ヒットするはず");

        assert_eq!(hit.offset, 0);
        assert_eq!(hit_texts(&hit, wanted.len()), vec!["a", "b", "c"]);
    }

    // 受け入れ条件: 要求がエントリの前方（先頭側）に包含される場合にヒットし、
    // その位置の本文が返る。
    #[test]
    fn get_hits_when_wanted_is_contained_at_the_front() {
        let mut cache = cache_with_three_items();

        let wanted = [span(0, 10), span(10, 10)];
        let hit = cache.get(1, &wanted).expect("前方の包含はヒットするはず");

        assert_eq!(hit.offset, 0);
        assert_eq!(hit_texts(&hit, wanted.len()), vec!["a", "b"]);
    }

    // 受け入れ条件: 要求がエントリの後方（末尾側）に包含される場合にヒットし、
    // 開始位置がずれていても正しい本文が返る。
    #[test]
    fn get_hits_when_wanted_is_contained_at_the_back() {
        let mut cache = cache_with_three_items();

        let wanted = [span(10, 10), span(20, 10)];
        let hit = cache.get(1, &wanted).expect("後方の包含はヒットするはず");

        assert_eq!(hit.offset, 1);
        assert_eq!(hit_texts(&hit, wanted.len()), vec!["b", "c"]);
    }

    // 受け入れ条件: エントリの端（先頭1件・末尾1件）だけの要求でもヒットし、
    // 位置の取り違えが起きない（包含判定の境界条件）。
    #[test]
    fn get_hits_at_entry_boundaries_for_single_item_requests() {
        let mut cache = cache_with_three_items();

        let first = cache.get(1, &[span(0, 10)]).expect("先頭1件はヒットする");
        assert_eq!(first.offset, 0);
        assert_eq!(hit_texts(&first, 1), vec!["a"]);

        let last = cache.get(1, &[span(20, 10)]).expect("末尾1件はヒットする");
        assert_eq!(last.offset, 2);
        assert_eq!(hit_texts(&last, 1), vec!["c"]);
    }

    // 受け入れ条件: 包含されない要求はミスになる（部分的な重なり・範囲外・
    // 要求のほうが長い場合・並びが不連続な場合）。
    #[test]
    fn get_misses_when_wanted_is_not_fully_contained() {
        let mut cache = cache_with_three_items();

        assert!(
            cache.get(1, &[span(20, 10), span(30, 10)]).is_none(),
            "末尾からはみ出す部分的な重なりはミス"
        );
        assert!(
            cache.get(1, &[span(100, 10)]).is_none(),
            "エントリの範囲外はミス"
        );
        assert!(
            cache
                .get(1, &[span(0, 10), span(10, 10), span(20, 10), span(30, 10)])
                .is_none(),
            "要求のほうが長ければ包含され得ない"
        );
        assert!(
            cache.get(1, &[span(0, 10), span(20, 10)]).is_none(),
            "連続部分列でない（間を飛ばした）要求はミス"
        );
    }

    // 受け入れ条件: 開始オフセットが同じでも長さが違えばミスになる（項目の
    // 切れ目が変わった場合に別項目の本文を返さない）。
    #[test]
    fn get_misses_when_raw_byte_len_differs_at_the_same_offset() {
        let mut cache = cache_with_three_items();

        assert!(
            cache.get(1, &[span(10, 12)]).is_none(),
            "長さが違えば別の項目とみなしてミスにする"
        );
    }

    // 受け入れ条件: ソースが違えばキャッシュミス。
    #[test]
    fn get_misses_when_source_id_does_not_match() {
        let mut cache = cache_with_three_items();

        assert!(cache.get(2, &[span(0, 10)]).is_none());
    }

    // 受け入れ条件: 空の要求はミス（0件の応答をキャッシュ由来と誤認しない）。
    #[test]
    fn get_with_empty_request_misses() {
        let mut cache = cache_with_three_items();

        assert!(cache.get(1, &[]).is_none());
    }

    // 受け入れ条件: 上限件数を超えると最も使われていない（LRU）ものから
    // 追い出される。包含ヒットも「使った」と数える。
    #[test]
    fn insert_evicts_least_recently_used_entry_beyond_capacity() {
        let mut cache = DecodedChunkCache::new();
        for i in 0..MAX_CACHED_CHUNKS as u64 {
            cache.insert(
                1,
                vec![span(i * 10, 10)].into_boxed_slice(),
                texts(&[&format!("chunk{i}")]),
            );
        }
        // 最初に追加した(offset=0)を包含ヒットで再アクセスして MRU にする。
        assert!(cache.get(1, &[span(0, 10)]).is_some());

        // 上限を1件超える新規追加で、直近アクセスされていない最古のエントリが
        // 追い出される（offset=0 は直前にアクセス済みなので残るはず）。
        cache.insert(1, vec![span(9990, 10)].into_boxed_slice(), texts(&["new"]));

        assert!(
            cache.get(1, &[span(0, 10)]).is_some(),
            "直近アクセスしたエントリは残るはず"
        );
        assert!(
            cache.get(1, &[span(9990, 10)]).is_some(),
            "新規追加したエントリは残るはず"
        );
        assert!(
            cache.get(1, &[span(10, 10)]).is_none(),
            "最も古いエントリが追い出されるはず"
        );
    }

    // 受け入れ条件: 同じ範囲を追加し直してもエントリは重複しない。
    #[test]
    fn insert_with_identical_spans_replaces_the_existing_entry() {
        let mut cache = DecodedChunkCache::new();
        let spans = || vec![span(0, 10)].into_boxed_slice();
        cache.insert(1, spans(), texts(&["old"]));
        cache.insert(1, spans(), texts(&["new"]));

        let hit = cache.get(1, &[span(0, 10)]).expect("ヒットするはず");
        assert_eq!(hit_texts(&hit, 1), vec!["new"]);
        assert_eq!(cache.entries.len(), 1, "同じ範囲のエントリは1件だけ");
    }

    // 受け入れ条件: invalidate_source で指定ソースのエントリだけが消える。
    #[test]
    fn invalidate_source_removes_only_that_source_entries() {
        let mut cache = DecodedChunkCache::new();
        cache.insert(1, vec![span(0, 1)].into_boxed_slice(), texts(&["a"]));
        cache.insert(2, vec![span(0, 1)].into_boxed_slice(), texts(&["b"]));

        cache.invalidate_source(1);

        assert!(cache.get(1, &[span(0, 1)]).is_none());
        let hit = cache.get(2, &[span(0, 1)]).expect("ヒットするはず");
        assert_eq!(hit_texts(&hit, 1), vec!["b"]);
    }

    // 空の items は追加されない（キャッシュを汚さない）。
    #[test]
    fn insert_with_empty_items_does_nothing() {
        let mut cache = DecodedChunkCache::new();
        cache.insert(1, Vec::new().into_boxed_slice(), texts(&[]));
        assert!(cache.entries.is_empty());
    }

    // spans と items の長さが食い違う組み立てミスは追加しない（包含ヒットが
    // 別項目の本文を返さないための防御）。
    #[test]
    fn insert_with_mismatched_span_count_does_nothing() {
        let mut cache = DecodedChunkCache::new();
        cache.insert(1, vec![span(0, 10)].into_boxed_slice(), texts(&["a", "b"]));
        assert!(cache.entries.is_empty());
    }
}
