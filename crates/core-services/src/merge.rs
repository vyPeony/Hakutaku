//! ソースをまたぐ時系列マージ（P09-1、`tasks/phase-09-timeline-merge.md`、
//! `docs/architecture/decisions/0008-merge-order-rules.md`）。
//!
//! # 複製しない設計
//!
//! [`build_merged_order`] の戻り値は、各項目を指す [`ItemId`]
//! （`source_id` と `seq` の組）の並びだけです。本文（`raw_text`）はもちろん、
//! 各ソースの索引（[`LineIndexEntry`]）そのものも複製しません。参照先は
//! `crate::registry::DisplaySetRegistry` が既に保持している各ソース単独の
//! [`crate::line_index::IndexedText`]（範囲取得時にオンデマンドで本文を読み出す
//! 際にも使う、同一の索引）であり、[`MergeMember::entries`] はそれを借用する
//! だけです。
//!
//! `ItemId` 1件のメモリ上のサイズは [`std::mem::size_of::<ItemId>()`]
//! （`source_id`（`u32`）と `seq`（`u64`）のフィールド構成上、8バイト境界への
//! 整列により実測16バイト）であり、本文・索引の複製と比べて無視できるほど
//! 小さい「参照列」だけを保持します（計画正本 4.2「統合結果全体の複製は可能な
//! 限り避ける」、`PERF-008`）。
//!
//! ただし**構築中のピークはこれより大きくなります**。[`build_merged_order`] は
//! 並べ替え用の一時列（[`MergeSortKey`]）を全項目分作り、結果列へ詰め替える間
//! この2本が同時に生存するためです。予約はこのピークで取ります
//! （[`reserve_merged_order`]、Issue #32）。
//!
//! # 順序規則（ADR-0008）
//!
//! 1. 比較キー（ミリ秒精度に正規化済み、`LOG-024`・`LOG-025`）昇順
//! 2. 同一キーは `source_ordinal`（表示集合の世代ごとに不変。`crate::registry`
//!    が各ソースへ割り当てる）昇順
//! 3. さらに同一ソース内では `seq`（ソース内出現順）昇順
//!
//! 時刻補正は一切行いません（`LOG-016`）。取得元の端末間で時計がずれていても、
//! 記録された比較キーをそのまま使います。
//!
//! # 日時なし項目の位置づけ（`LOG-014` との関係）
//!
//! 継続行（日時を持たない物理行が、直前の日時付き物理行へ結合される場合）は
//! `crate::streaming_parse` の時点で1つの論理項目へ結合済みであり、この
//! モジュールが扱う「日時なし項目」とは無関係です（結合済みの項目は
//! `has_timestamp() == true` として扱われます）。
//!
//! ここで言う「日時なし項目」は、日時解析に失敗し独立した項目のまま残った
//! もの（`raw_display`／生データ項目、[`LineIndexEntry::has_timestamp`] が
//! `false`）です。この種の項目は、比較キーとして**ソース内で直前に出現した
//! 日時付き項目のキーを引き継ぎます**。ファイル先頭からその項目まで日時付き
//! 項目が1つも出現していない場合は `i64::MIN`（比較キー最小）を使い、その
//! ソースの最初の日時付き項目より前に位置づけます。
//!
//! この設計により、日時なし項目は常に「直前の日時付き項目のすぐ後ろ」
//! （同一ソース、`seq` 昇順で連続）に位置します。同一 `(comparison_key,
//! source_ordinal)` の組の中では `seq` だけで整列するため、他ソースの項目が
//! この2項目の間へ入り込むことはありません。

use crate::item::ItemId;
use crate::line_index::LineIndexEntry;

/// マージ対象1ソース分の入力です。`entries` はそのソースの
/// `IndexedText::entries()` をそのまま借用します（複製しません）。
pub(crate) struct MergeMember<'a> {
    pub source_id: u32,
    /// ADR-0008 の順序規則における同順位解決キー（表示集合の世代ごとに不変）。
    pub source_ordinal: u32,
    pub entries: &'a [LineIndexEntry],
}

/// [`build_merged_order`] が並べ替えに使う一時列の1要素です
/// （`(実効比較キー, source_ordinal, ソース内 seq, source_id)`）。
///
/// 型として名前を付けているのは、[`reserve_merged_order`] の予約量がこの型の
/// 大きさに依存するためです（係数をハードコードせず
/// [`std::mem::size_of`] で参照し、要素が増減したら予約量も自動的に追従
/// させます）。
type MergeSortKey = (i64, u32, u64, u32);

/// 統合表示の並べ替えで、項目1件あたり**同時に生存し得る**最悪バイト数です
/// （[`reserve_merged_order`] の係数。導出根拠は同関数の doc コメント）。
pub(crate) const MERGED_ORDER_PEAK_BYTES_PER_ITEM: usize =
    std::mem::size_of::<MergeSortKey>() + std::mem::size_of::<ItemId>();

/// ADR-0008 の順序規則に従い、複数ソースの索引を横断する順序付き参照列
/// （`(source_id, seq)` の並び）を構築します。
///
/// 決定的です。同じ `members`（内容・並び）を渡せば、何度呼んでも同じ結果を
/// 返します（呼び出し順は結果に影響しません。ソートキーに `source_ordinal` を
/// 含むため）。
///
/// 実行中のピークは、並べ替え用の一時列（[`MergeSortKey`]）と結果列
/// （[`ItemId`]）が変換の間だけ同時に生存する形です
/// （[`reserve_merged_order`] の予約モデルと一致させています）。
#[must_use]
pub(crate) fn build_merged_order(members: &[MergeMember<'_>]) -> Vec<ItemId> {
    let total: usize = members.iter().map(|member| member.entries.len()).sum();
    // ソートキー: (実効比較キー, source_ordinal, ソース内 seq, source_id)。
    // 先頭3要素の組は全項目で一意（source_ordinal はソースごとに一意、seq は
    // ソース内で一意なため）であり、安定ソートである必要はない
    // （sort_unstable_by で十分。かつ決定的）。
    let mut combined: Vec<MergeSortKey> = Vec::with_capacity(total);

    for member in members {
        // ソース内で直前に出現した日時付き項目のキー（モジュール doc コメント
        // 「日時なし項目の位置づけ」）。ファイル先頭で未出現の間は i64::MIN
        // （比較キー最小）を使う。
        let mut last_key = i64::MIN;
        for (seq, entry) in member.entries.iter().enumerate() {
            let key = match entry.comparison_key_millis() {
                Some(k) => {
                    last_key = k;
                    k
                }
                None => last_key,
            };
            combined.push((key, member.source_ordinal, seq as u64, member.source_id));
        }
    }

    combined.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

    // `into_iter().map().collect()` ではなく、結果列を明示的に確保して詰め替
    // える。`collect` は条件が揃うと一時列の確保をそのまま作り替えて再利用する
    // （in-place 特殊化）ことがあり、実確保の形が標準ライブラリの内部実装に
    // 左右される。ここでは `reserve_merged_order` の予約モデル（一時列と結果列
    // が変換の間だけ同時に生存する）と実装を決定的に一致させたいので、確保の
    // 形をこちらで固定する。
    let mut order: Vec<ItemId> = Vec::with_capacity(total);
    for &(_, _, seq, source_id) in &combined {
        order.push(ItemId { source_id, seq });
    }
    // 予約は「一時列と結果列が同時に生存する」ピークで取っているため、一時列は
    // 呼び出し側へ戻る前にここで解放する。
    drop(combined);
    order
}

/// [`build_merged_order`] の実行中に同時に生存する確保量を計算し、`budget` へ
/// 予約します（`PERF-008`・`PERF-010`、Issue #32）。
///
/// # ピークのモデル
///
/// 予約量は `total_items` × [`MERGED_ORDER_PEAK_BYTES_PER_ITEM`]
/// （= [`MergeSortKey`] 1件 + [`ItemId`] 1件。64ビット環境で 24 + 16 =
/// 40バイト）です。[`build_merged_order`] は、まず並べ替え用の一時列
/// （`Vec<MergeSortKey>`）を全項目分作って整列し、そのあと結果列
/// （`Vec<ItemId>`）へ詰め替えます。**詰め替えの間、この2本は同時に生存する**
/// ため、結果列だけを数えると実際のピークの半分以下しか予約しないことに
/// なります（Issue #32 の経路2。以前は `ItemId` 1件分＝16バイトだけを予約して
/// いました）。
///
/// 一時列を先に捨ててから結果列を作ることはできません。詰め替えの入力が
/// 一時列そのものだからです。したがって「同時に生存する2本」がこの関数の
/// 予約対象であり、[`build_merged_order`] 側も `collect` の最適化任せにせず
/// この形になるよう明示的に確保しています。
///
/// # 呼び出し側の契約
///
/// 予約が成功した後に [`build_merged_order`] を実行し、完了後にトークンの
/// [`hakutaku_memory_accounting::ReservationToken::mark_allocated`] で、
/// **呼び出しから戻ったあとも残る分**（結果列＝`ItemId` × 件数）だけを実確保へ
/// 振り替えてください。一時列は既に解放されているため、振り替えずに残った予約
/// はトークンの破棄で戻ります（ADR-0003）。予約が拒否された場合、統合表示集合の
/// 構築を開始してはいけません（計画正本「予約が拒否されたら統合表示を開始せず
/// エラー」）。
pub(crate) fn reserve_merged_order(
    budget: &hakutaku_memory_accounting::MemoryBudget,
    total_items: usize,
) -> Result<
    hakutaku_memory_accounting::ReservationToken<'_>,
    hakutaku_memory_accounting::ReservationRejected,
> {
    let bytes = total_items.saturating_mul(MERGED_ORDER_PEAK_BYTES_PER_ITEM);
    budget.reserve(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hakutaku_memory_accounting::MemoryBudget;

    /// テスト用に、1ソース分の `LineIndexEntry` 列を組み立てます。
    /// `entries` は `(comparison_key_millis, has_timestamp)` の並びです
    /// （`raw_offset`・`byte_len`・`continuation_count`・`unconfirmed` は
    /// マージの順序決定に無関係なため固定値で埋めます）。
    fn entries_from(pairs: &[(i64, bool)]) -> Vec<LineIndexEntry> {
        pairs
            .iter()
            .map(|&(key, has_timestamp)| LineIndexEntry {
                offset: 0,
                comparison_key_millis: key,
                byte_len: 1,
                flags: if has_timestamp {
                    crate::line_index::FLAG_HAS_TIMESTAMP
                } else {
                    0
                },
                continuation_count: 0,
            })
            .collect()
    }

    // 受け入れ条件（LOG-007 の例）: A-1 -> A-2 -> B-1 -> A-3 の順で表示される。
    #[test]
    fn merges_two_sources_in_ascending_timestamp_order_per_log_007_example() {
        // A: 15:00:00.000, 15:00:01.000, 15:00:02.000（A-1, A-2, A-3）。
        // 起点を 0 ミリ秒とした相対値（0, 1000, 2000）を使う。
        // B: 15:00:01.500（B-1、A-1 から 1500 ミリ秒後）
        let a_entries = entries_from(&[(0, true), (1000, true), (2000, true)]);
        let b_entries = entries_from(&[(1500, true)]);

        let members = vec![
            MergeMember {
                source_id: 10,
                source_ordinal: 0,
                entries: &a_entries,
            },
            MergeMember {
                source_id: 20,
                source_ordinal: 1,
                entries: &b_entries,
            },
        ];

        let order = build_merged_order(&members);

        assert_eq!(
            order,
            vec![
                ItemId {
                    source_id: 10,
                    seq: 0
                }, // A-1
                ItemId {
                    source_id: 10,
                    seq: 1
                }, // A-2
                ItemId {
                    source_id: 20,
                    seq: 0
                }, // B-1
                ItemId {
                    source_id: 10,
                    seq: 2
                }, // A-3
            ]
        );
    }

    // 受け入れ条件: 同一比較キーの行は source_ordinal -> seq の順で並ぶ。
    #[test]
    fn ties_on_comparison_key_are_broken_by_source_ordinal_then_seq() {
        let a_entries = entries_from(&[(1000, true), (1000, true)]);
        let b_entries = entries_from(&[(1000, true)]);

        // source_ordinal を意図的に B(0) < A(1) にする（挿入順とは無関係に
        // 決定されることの確認）。
        let members = vec![
            MergeMember {
                source_id: 20,
                source_ordinal: 0,
                entries: &b_entries,
            },
            MergeMember {
                source_id: 10,
                source_ordinal: 1,
                entries: &a_entries,
            },
        ];

        let order = build_merged_order(&members);

        assert_eq!(
            order,
            vec![
                ItemId {
                    source_id: 20,
                    seq: 0
                }, // source_ordinal 0 が先
                ItemId {
                    source_id: 10,
                    seq: 0
                }, // source_ordinal 1、seq 0
                ItemId {
                    source_id: 10,
                    seq: 1
                }, // source_ordinal 1、seq 1
            ]
        );
    }

    // 受け入れ条件: 繰り返し呼んでも順序が再現する（決定性）。
    #[test]
    fn repeated_builds_with_the_same_input_produce_identical_order() {
        let a_entries = entries_from(&[(3000, true), (1000, true), (2000, true)]);
        let members = vec![MergeMember {
            source_id: 1,
            source_ordinal: 0,
            entries: &a_entries,
        }];

        let first = build_merged_order(&members);
        let second = build_merged_order(&members);
        assert_eq!(first, second);
        // 入力自体は比較キー順ではない（3000, 1000, 2000）が、出力は昇順になる。
        assert_eq!(
            first,
            vec![
                ItemId {
                    source_id: 1,
                    seq: 1
                }, // 1000
                ItemId {
                    source_id: 1,
                    seq: 2
                }, // 2000
                ItemId {
                    source_id: 1,
                    seq: 0
                }, // 3000
            ]
        );
    }

    // 受け入れ条件（LOG-016）: 時刻補正を行わないため、取得元の端末間で時計が
    // ずれていても記録された比較キーのまま並ぶ（source_ordinal が小さいソースの方が
    // 後ろに来ることもある、という確認）。
    #[test]
    fn no_time_correction_is_applied_even_when_ordinal_and_key_order_disagree() {
        // source_ordinal 0 (先に開いたソース) の時計が進んでいる想定。
        let fast_clock = entries_from(&[(5000, true)]);
        let slow_clock = entries_from(&[(1000, true)]);

        let members = vec![
            MergeMember {
                source_id: 1,
                source_ordinal: 0,
                entries: &fast_clock,
            },
            MergeMember {
                source_id: 2,
                source_ordinal: 1,
                entries: &slow_clock,
            },
        ];

        let order = build_merged_order(&members);

        // 時刻補正されないため、source_ordinal に関わらず比較キー昇順のまま。
        assert_eq!(
            order,
            vec![
                ItemId {
                    source_id: 2,
                    seq: 0
                },
                ItemId {
                    source_id: 1,
                    seq: 0
                },
            ]
        );
    }

    // 受け入れ条件: 日時なし項目は直前の日時付き項目のすぐ後ろに位置する
    // （ソース内 seq 順、比較キーは直前項目のキーを引き継ぐ）。他ソースの
    // 項目が2項目の間へ割り込まないことも確認する。
    #[test]
    fn undated_item_follows_immediately_after_preceding_dated_item() {
        // ソース A: 日時付き(1000) -> 日時なし -> 日時付き(3000)
        let a_entries = entries_from(&[(1000, true), (0, false), (3000, true)]);
        // ソース B: 日時付き(2000) だけ。もし日時なし項目の実効キーが正しく
        // 1000 を引き継がなければ、B が A の日時なし項目より前に来てしまう
        // （0 < 2000 になるため）。
        let b_entries = entries_from(&[(2000, true)]);

        let members = vec![
            MergeMember {
                source_id: 1,
                source_ordinal: 0,
                entries: &a_entries,
            },
            MergeMember {
                source_id: 2,
                source_ordinal: 1,
                entries: &b_entries,
            },
        ];

        let order = build_merged_order(&members);

        assert_eq!(
            order,
            vec![
                ItemId {
                    source_id: 1,
                    seq: 0
                }, // A: 1000
                ItemId {
                    source_id: 1,
                    seq: 1
                }, // A: 日時なし（1000 を引き継ぎ、直後に位置する）
                ItemId {
                    source_id: 2,
                    seq: 0
                }, // B: 2000
                ItemId {
                    source_id: 1,
                    seq: 2
                }, // A: 3000
            ]
        );
    }

    // 受け入れ条件: ファイル先頭の日時なし項目は、そのソースの最初の日時付き
    // 項目より前（比較キー最小扱い）に位置する。
    #[test]
    fn leading_undated_items_are_treated_as_minimum_comparison_key() {
        let a_entries = entries_from(&[(0, false), (0, false), (5000, true)]);
        let members = vec![MergeMember {
            source_id: 1,
            source_ordinal: 0,
            entries: &a_entries,
        }];

        let order = build_merged_order(&members);

        assert_eq!(
            order,
            vec![
                ItemId {
                    source_id: 1,
                    seq: 0
                },
                ItemId {
                    source_id: 1,
                    seq: 1
                },
                ItemId {
                    source_id: 1,
                    seq: 2
                },
            ]
        );
    }

    // 受け入れ条件（LOG-025）: `15:12:23.45`（1/100秒展開で450ミリ秒）と
    // `15:12:23.450` が同一の比較キーとなり、マージでも同一キー扱い（同順位
    // 解決は source_ordinal -> seq）になる。実際の日時解析（crates/parser）を
    // 通した値を使う。
    #[test]
    fn centisecond_and_millisecond_precision_share_the_same_comparison_key_in_merge() {
        let centisecond = hakutaku_parser::parse_datetime_with_format(
            hakutaku_parser::LogDateTimeFormat::LogDt003,
            "2026/07/28 15:12:23.45",
        )
        .expect("解析できるはず");
        let millisecond = hakutaku_parser::parse_datetime_with_format(
            hakutaku_parser::LogDateTimeFormat::LogDt001,
            "2026/07/28 15:12:23.450",
        )
        .expect("解析できるはず");
        let key_centi = centisecond.comparison_key.as_millis_since_epoch();
        let key_milli = millisecond.comparison_key.as_millis_since_epoch();
        assert_eq!(
            key_centi, key_milli,
            "LOG-025: .45 は 450 ミリ秒に展開される"
        );

        let a_entries = entries_from(&[(key_centi, true)]);
        let b_entries = entries_from(&[(key_milli, true)]);
        let members = vec![
            MergeMember {
                source_id: 1,
                source_ordinal: 0,
                entries: &a_entries,
            },
            MergeMember {
                source_id: 2,
                source_ordinal: 1,
                entries: &b_entries,
            },
        ];

        let order = build_merged_order(&members);
        // 同一キーのため source_ordinal 昇順（source_id 1 が先）。
        assert_eq!(
            order,
            vec![
                ItemId {
                    source_id: 1,
                    seq: 0
                },
                ItemId {
                    source_id: 2,
                    seq: 0
                },
            ]
        );
    }

    // 受け入れ条件（LOG-024）: `LOG-DT-006` の `15:12` は `15:12:00.000` として
    // 並ぶ（秒・秒未満の0補完）。実際の日時解析を通した値で確認する。
    #[test]
    fn log_dt_006_without_seconds_sorts_as_if_zero_padded_to_milliseconds() {
        let zero_padded = hakutaku_parser::parse_datetime_with_format(
            hakutaku_parser::LogDateTimeFormat::LogDt006,
            "2026/07/28 15:12",
        )
        .expect("解析できるはず");
        let explicit = hakutaku_parser::parse_datetime_with_format(
            hakutaku_parser::LogDateTimeFormat::LogDt001,
            "2026/07/28 15:12:00.000",
        )
        .expect("解析できるはず");
        assert_eq!(
            zero_padded.comparison_key.as_millis_since_epoch(),
            explicit.comparison_key.as_millis_since_epoch(),
            "LOG-024: 秒・秒未満の欠落は0補完されるはず"
        );

        // 15:12:00.000（0補完済み） と 15:12:30.000 をマージし、前者が先に
        // 並ぶことを確認する。
        let later = hakutaku_parser::parse_datetime_with_format(
            hakutaku_parser::LogDateTimeFormat::LogDt001,
            "2026/07/28 15:12:30.000",
        )
        .expect("解析できるはず");

        let entries = entries_from(&[
            (later.comparison_key.as_millis_since_epoch(), true),
            (zero_padded.comparison_key.as_millis_since_epoch(), true),
        ]);
        let members = vec![MergeMember {
            source_id: 1,
            source_ordinal: 0,
            entries: &entries,
        }];

        let order = build_merged_order(&members);
        assert_eq!(
            order,
            vec![
                ItemId {
                    source_id: 1,
                    seq: 1
                }, // 15:12:00.000（0補完された 15:12）
                ItemId {
                    source_id: 1,
                    seq: 0
                }, // 15:12:30.000
            ]
        );
    }

    // --- reserve_merged_order（PERF-008・PERF-010） ---

    // 受け入れ条件: 参照列の予約が P02 の予約経路を通る（会計値の観測）。
    // 受け入れ条件（Issue #32 の経路2）: 予約量が実ピーク（並べ替え用の一時列と
    // 結果列が同時に生存する分＝1件40バイト）と一致する。結果列だけを数えた
    // 16バイト/件では、実際の確保の半分以下しか予約していなかった。
    #[test]
    fn reserve_merged_order_reserves_sort_key_and_item_id_bytes_per_item() {
        let budget = MemoryBudget::new(1_000_000);
        assert_eq!(budget.outstanding_reserved_bytes(), 0);

        // 係数はハードコードせず型の大きさから導くが、64ビット環境での実値
        // （24 + 16 = 40バイト）が変わっていないことも合わせて固定する。
        assert_eq!(std::mem::size_of::<MergeSortKey>(), 24);
        assert_eq!(std::mem::size_of::<ItemId>(), 16);
        assert_eq!(MERGED_ORDER_PEAK_BYTES_PER_ITEM, 40);

        let token = reserve_merged_order(&budget, 100).expect("予算内なので成功するはず");
        let expected_bytes = 100 * MERGED_ORDER_PEAK_BYTES_PER_ITEM;
        assert_eq!(budget.outstanding_reserved_bytes(), expected_bytes);

        // 呼び出しから戻ったあとも残るのは結果列だけなので、振り替えるのは
        // その分（16バイト/件）で、残りはトークンの破棄で戻る。
        token
            .mark_allocated(100 * std::mem::size_of::<ItemId>())
            .expect("予約量以内の振り替えは成功するはず");
        drop(token);
        assert_eq!(
            budget.outstanding_reserved_bytes(),
            0,
            "実確保へ振り替えた後は予約が残らないはず"
        );
    }

    // 受け入れ条件（Issue #32 の経路2）: 予算がちょうど 40n バイトなら予約でき、
    // 1バイト足りなければ拒否される（境界値。16n を前提にしていた頃は、この
    // 予算帯でも予約が通ってしまい実確保が予算を超えていた）。
    #[test]
    fn reserve_merged_order_boundary_accepts_exact_peak_and_rejects_one_byte_less() {
        let items = 100usize;
        let peak_bytes = items * MERGED_ORDER_PEAK_BYTES_PER_ITEM;

        let exact = MemoryBudget::new(peak_bytes);
        let token = reserve_merged_order(&exact, items).expect("ピークちょうどなら成功するはず");
        assert_eq!(exact.outstanding_reserved_bytes(), peak_bytes);
        drop(token);

        let one_short = MemoryBudget::new(peak_bytes - 1);
        let rejected = reserve_merged_order(&one_short, items)
            .expect_err("ピークに1バイト足りなければ拒否されるはず");
        assert_eq!(rejected.requested_bytes, peak_bytes);
        assert_eq!(one_short.outstanding_reserved_bytes(), 0);
    }

    // 受け入れ条件: 予約が拒否されたら統合表示を開始せずエラーになる
    // （PERF-008 の文言。呼び出し側は Err を見て build_merged_order を呼ばない
    // という使い方の確認）。
    #[test]
    fn reserve_merged_order_is_rejected_when_budget_is_insufficient() {
        let budget = MemoryBudget::new(10);
        let rejected =
            reserve_merged_order(&budget, 1000).expect_err("予算を超えるので拒否されるはず");
        assert_eq!(rejected.budget_bytes, 10);
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }
}
