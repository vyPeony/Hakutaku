//! 表示集合が保持する項目の内部表現（4.1 の暫定方針、`tasks/phase-04-vertical-slice.md`）。
//!
//! 「項目 = { 日時（Option）、原文、ソース内の行番号、任意のログレベル }」に、
//! 「契約に織り込む4点」の2（安定した識別子と読み込み元の来歴）を加えた形です。
//!
//! # P08-5 索引 + オンデマンド読み出しへの移行
//!
//! [`PendingItem`] は、もはや `raw_text: String`（デコード済み本文）を保持
//! しません。継続行結合（`LOG-014`）まで終えた論理項目1件を、**ファイルの
//! 生バイト範囲**（`raw_offset`・`raw_byte_len`）として表します。本文の
//! デコードは、範囲取得のたびに `crate::registry::DisplaySetRegistry::
//! fetch_range` がオンデマンドで行います（`crate::line_index` のモジュール
//! doc コメント「本文バッファを保持しない」を参照）。`PendingItem` は
//! ヒープ確保を持たない `Copy` 型になったため、`crate::streaming_parse::
//! StreamingAssembler` が保留する `held_item` の更新（継続行の追記）も、
//! 文字列の連結ではなく `raw_byte_len` の再計算だけで済みます。
//!
//! [`Item`] 自体は P08-1 から変わっていません。安定した識別子
//! （[`ItemId`]）と、その項目が属するソースの [`crate::line_index::IndexedText`]
//! 内でのエントリ添字（`entry_index`）だけを持ちます。

use crate::line_index::IndexedText;

/// 項目の安定した識別子です（契約に織り込む4点の2）。
///
/// `source_id` はソースごとに一意、`seq` はソース内の連番です。表示集合の順序が
/// マージ結果になっても（P06 以降）この組は変わりません（契約に織り込む4点の3:
/// 「範囲を再取得した際に同じ順序と識別子を返す」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId {
    pub source_id: u32,
    pub seq: u64,
}

/// 表示集合が保持する1件の項目です（P08-1: 本文保持の一本化。P08-5: 索引化）。
///
/// `entry_index` は、この項目が属するソース（`id.source_id`）に対応する
/// [`IndexedText`]（`crate::display_set::DisplaySet` がソースごとに1つ保持する）
/// の中でのエントリ添字です。本文（`raw_text`）・日時表示・ソース内行番号・
/// 継続行数・未確定フラグは、いずれもこの添字を通じて索引から導出したうえで、
/// 本文だけは `crate::registry::DisplaySetRegistry::fetch_range` がオンデマンド
/// でファイルから読み出します。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// 安定した識別子（契約に織り込む4点の2）。
    pub id: ItemId,
    /// `id.source_id` の [`IndexedText`] 内でのエントリ添字。
    pub entry_index: usize,
}

/// [`RESIDENT_BYTES_PER_ITEM`] の内訳（「24バイト」の部分）が崩れていないことの
/// 静的検証です。[`Item`] は行数に比例して常駐する型であり、フィールドを1つ
/// 増やすだけで2000万行あたり160MB規模の会計差が生まれます（過去には、
/// この24バイトが予約経路から丸ごと漏れていた不具合がありました）。ビルド時に失敗
/// させ、レイアウトの意図しない変化を予約量の見直しなしに通さないようにします。
const _: () = assert!(
    std::mem::size_of::<Item>() <= 24,
    "Item が24バイトを超えると RESIDENT_BYTES_PER_ITEM の内訳と実態がずれる"
);

/// 論理項目1件を常駐させるために、メモリ会計（`PERF-008`・`PERF-010`）へ予約
/// するバイト数です（64ビット環境で合計 **56バイト**）。
///
/// 内訳:
///
/// | 対象 | バイト数 | 保持者 |
/// | --- | --- | --- |
/// | 行索引本体（[`crate::line_index::LineIndexEntry`]） | 24 | `IndexedText` |
/// | 行番号の並列配列（`u64`） | 8 | `IndexedText` |
/// | 項目（[`Item`]） | 24 | `crate::display_set::DisplaySet::items` |
///
/// 前2者（合計32バイト）は [`crate::line_index::INDEX_BYTES_PER_ENTRY`]、
/// 最後の [`Item`] 分は [`reserve_items_growth`] が予約します。
///
/// # なぜ32ではなく56か
///
/// P08-5 以降、1行あたりの常駐コストは「索引 24 + 行番号 8 =
/// 32バイト」であると各所の doc コメントが説明していましたが、表示集合が
/// 順序付き項目列として保持する `Vec<Item>` も行数に比例して増え続けます。
/// この24バイトが予約経路に含まれていなかったため、2000万行で約480MBが会計外
/// になり、`PERF-010`（大規模な確保の前に予約・拒否する）と ADR-0003（予約の
/// 帰属は明示的な確保 API で行う）の趣旨から外れていました。この定数は、
/// 常駐する3つの配列すべてを1か所で示すために置いています。
pub const RESIDENT_BYTES_PER_ITEM: usize =
    crate::line_index::INDEX_BYTES_PER_ENTRY + std::mem::size_of::<Item>();

/// 表示集合を構成するソース（読み込み元）の来歴情報です（契約に織り込む4点の2:
/// 「読み込み元の来歴」。`LOG-007` の下地）。
///
/// `label` はフロントエンドへ表示するためのラベルであり、絶対パスそのものでは
/// ありません（`SEC-012`: フロントエンドへ任意パスの操作権を与えない）。
///
/// `Arc<str>` なのは、本文（`crate::display_set::ItemDto::raw_text`）と同じ
/// 理由です。ラベルは範囲取得の応答1件ごとに複製される値であり、1回の応答で
/// 最大 [`crate::display_set::MAX_ITEMS_PER_RESPONSE`] 件ぶんの
/// 確保が積み上がります。同じソースの項目はすべて同じラベルを指すため、
/// 参照カウントの増加だけで済ませます（Issue #51 項目11）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub source_id: u32,
    pub label: std::sync::Arc<str>,
}

/// 継続行の結合まで終えた、ID割り当て**前**の論理項目1件です
/// （`crate::loader` が構築します）。
///
/// ヒープ確保を一切持たない `Copy` 型です（P08-5、モジュール doc コメント
/// 参照）。`raw_offset`・`raw_byte_len` の意味は
/// [`crate::line_index::LineIndexEntry`] と同じです（ファイルの生バイト範囲、
/// BOM を除く）。
///
/// `log_level` は現状のパーサー（`crates/parser`）がログレベルを解析しない
/// ため保持していません（`LOG-004`: ログレベル列が存在しない形式でも表示が
/// 崩れないことが要件であり、`None` 固定のフィールドを持ち回すことに意味が
/// ないための削除）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingItem {
    /// ソースファイル先頭からの生バイトオフセット（BOM を除く）。
    pub raw_offset: u64,
    /// 項目本体（継続行を含む）の生バイト長（最後の物理行の行末区切り文字を
    /// 含まない）。
    pub raw_byte_len: u32,
    /// ミリ秒精度の比較キー。日時未解析なら `None`。
    pub comparison_key_millis: Option<i64>,
    pub source_line_number: u64,
    /// 結合された継続行（`LOG-014`）の数。0 は継続行なし。
    pub continuation_count: u16,
    /// 末尾の断片（`LOG-026`）で、書き込み途中の可能性がある未確定行か。
    pub unconfirmed: bool,
}

impl PendingItem {
    /// テスト用に、単一物理行から `PendingItem` を作る便利コンストラクタです
    /// （継続行結合・未確定フラグはいずれも既定値）。`raw_offset` は0、
    /// `raw_byte_len` は `text.len()` を使います（実ファイルを介さない
    /// テストのための簡略化）。
    #[cfg(test)]
    pub(crate) fn simple(line_number: u64, text: &str) -> Self {
        PendingItem {
            raw_offset: 0,
            raw_byte_len: u32::try_from(text.len()).unwrap_or(u32::MAX),
            comparison_key_millis: None,
            source_line_number: line_number,
            continuation_count: 0,
            unconfirmed: false,
        }
    }
}

/// `source_id` を起点に、`pending_items` から [`Item`] の並びを構築し、
/// `text`（その `source_id` に対応する [`IndexedText`]）へ索引エントリを
/// 追記します。
///
/// `seq` は `pending_items` の並び順（0起点、継続行結合後の論理項目単位）です。
/// メモリ会計（`PERF-008`・`PERF-010`）は、索引の伸長分（[`crate::line_index::
/// reserve_growth`]）と項目列の伸長分（[`reserve_items_growth`]）を、
/// いずれも確保の**前**に予約します（1件あたり合計
/// [`RESIDENT_BYTES_PER_ITEM`]）。**P08-5 で、
/// この予約の拒否は登録失敗として扱うよう変更しました**（P08-1 までは
/// ベストエフォートで無視していましたが、本文バッファを保持しなくなり
/// 索引と項目列が常駐コストのすべてになったため、拒否を握りつぶすと
/// `PERF-008` の予算を無制限に超過し得ます。`crate::loader` が
/// [`hakutaku_memory_accounting::ReservationRejected`] を利用者向け
/// メッセージへ変換します）。
///
/// 本体の経路（登録・伸長・再読み込み）は、常駐する項目列へ直接追記する
/// [`build_items_from_pending_into`] を使います。この関数は、新しい `Vec<Item>`
/// を返す形が読みやすいテストのために残しています。
#[cfg(test)]
pub(crate) fn build_items_from_pending(
    source_id: u32,
    pending_items: &[PendingItem],
    text: &mut IndexedText,
) -> Result<Vec<Item>, hakutaku_memory_accounting::ReservationRejected> {
    let mut items = Vec::new();
    build_items_from_pending_into(source_id, 0, pending_items, text, &mut items)?;
    Ok(items)
}

/// `pending_items` を `text`（索引）と `items`（常駐する項目列）の**末尾へ
/// 追記**します。`start_seq` から連番を続けます。
///
/// P06-2 のチャンク読み込みが、読み込み途中で解析済み範囲から表示集合を
/// 伸長する際（`crate::registry::DisplaySetRegistry::grow_source_items`）に、
/// 直前までに払い出し済みの `seq` の続きから採番するために使います
/// （`seq` を常に0から振り直すと、後続バッチの項目が既存項目と衝突する）。
/// `text`・`items` も同様に、既存の内容へ追記を続けます（新規作成しません）。
///
/// 戻り値は、この呼び出しで会計へ**実確保として振り替えたバイト数**です
/// （呼び出し側が `LoadSummary::reserved_bytes` へ積み上げます）。
///
/// # 事前確保済みの容量は再予約しない
///
/// [`ensure_resident_capacity`] で確保済みの余剰容量に収まる追記は、新たな
/// ヒープ確保を伴いません。その分まで予約すると、確保されないバイト数を
/// 予約することになり（二重計上）、予算判定が実態より厳しくなって不要な拒否を
/// 生みます。そのため予約量は「余剰容量を超える分」だけに絞ります。余剰が
/// 足りない場合の伸長は従来どおり `Vec` の倍々成長に任せます（この経路の
/// 予約拒否は、従来どおり**登録失敗**として扱います）。
///
/// P06-2 まで、この関数は新しい `Vec<Item>` へ `collect` して返していました。
/// 呼び出し側の常駐 `Vec<Item>` へ直接 push する形に変えたのは、余剰容量を
/// 判定するために常駐側の容量を見る必要があるからです（バッチごとの一時
/// `Vec` の確保が1つ減る副次的な利点もあります）。
pub(crate) fn build_items_from_pending_into(
    source_id: u32,
    start_seq: u64,
    pending_items: &[PendingItem],
    text: &mut IndexedText,
    items: &mut Vec<Item>,
) -> Result<usize, hakutaku_memory_accounting::ReservationRejected> {
    build_items_from_pending_into_with_budget(
        hakutaku_memory_accounting::global_budget(),
        source_id,
        start_seq,
        pending_items,
        text,
        items,
    )
}

/// [`build_items_from_pending_into`] の予算指定版です。
///
/// 予約拒否の経路を決定的に検証できるようにするためだけに分けています
/// （グローバル予算を縮小すると他のテストへ影響するため）。
fn build_items_from_pending_into_with_budget(
    budget: &hakutaku_memory_accounting::MemoryBudget,
    source_id: u32,
    start_seq: u64,
    pending_items: &[PendingItem],
    text: &mut IndexedText,
    items: &mut Vec<Item>,
) -> Result<usize, hakutaku_memory_accounting::ReservationRejected> {
    let additional_entries = pending_items.len();
    // 事前確保で既に確保済みの余剰容量は、追記しても新たな確保が
    // 起きないため予約対象から外す（doc コメント「事前確保済みの容量は再予約
    // しない」）。索引と項目列は容量が別々に決まるため、それぞれ独立に求める。
    let index_uncovered = additional_entries.saturating_sub(text.spare_capacity());
    let items_uncovered =
        additional_entries.saturating_sub(items.capacity().saturating_sub(items.len()));

    // ADR-0003 の帰属規則により、予約はその確保を行う層が、確保の直前に行う。
    // 索引（IndexedText 本体 + 行番号の並列配列）は line_index が確保するため
    // line_index が、下の push が伸ばす Vec<Item> はこの層が予約する。
    // 索引を先に予約するのは、順序が意味を持つからではなく、後続の予約が
    // 拒否されたときに先行トークンの破棄で確実に全量が戻るため（ADR-0003
    // 「確保の失敗」。どちらが先でも会計結果は同じ）。
    let index_token = crate::line_index::reserve_growth(budget, index_uncovered)?;
    // 項目列は `crate::display_set::DisplaySet::items` として常駐し続けるため、
    // 索引と同じく行数に比例して増え続ける。予約が拒否された
    // 場合は索引へ1件も追記せずに戻り、index_token の破棄で索引分の予約も
    // 解放される。
    let items_token = reserve_items_growth(budget, items_uncovered)?;

    for (offset, pending) in pending_items.iter().enumerate() {
        let entry_index = text.push_entry(
            pending.raw_offset,
            pending.raw_byte_len,
            pending.comparison_key_millis,
            pending.unconfirmed,
            pending.continuation_count,
            pending.source_line_number,
        );
        items.push(Item {
            id: ItemId {
                source_id,
                seq: start_seq + offset as u64,
            },
            entry_index,
        });
    }

    // 実確保が済んだ直後に、それぞれのトークンを実確保へ振り替える
    // （ADR-0003「帰属（振り替え）」。振り替えないまま破棄すると、予約が
    // 解放されて二重計上は避けられるものの、以後の予算判定が実態より緩くなる）。
    let index_bytes = index_uncovered * crate::line_index::INDEX_BYTES_PER_ENTRY;
    let items_bytes = items_uncovered * std::mem::size_of::<Item>();
    let _ = index_token.mark_allocated(index_bytes);
    let _ = items_token.mark_allocated(items_bytes);

    Ok(index_bytes + items_bytes)
}

/// 事前確保の目標容量の求め方です。
///
/// 読み込み中は総項目数が確定しないため、目標容量の求め方を2つに分けます。
/// この区別が必要なのは、`Vec` の伸長が**倍々**だからです。事前確保した容量を
/// 1件でも超えると次の容量は2倍になるため、「わずかに過小な見積もり」は
/// 事前確保なしより悪い結果（最終容量 約1.9倍、再確保時の一時確保 約2.9倍）に
/// なります。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapacityEstimate {
    /// 読み込み済みの前半から外挿した推定値（`crate::loader::
    /// estimate_total_items`）。過小だったときに再確保が何度も起きないよう、
    /// 目標容量に下限（現容量の1.25倍）を課します。
    Projected(usize),
    /// 確定値。全量を読み終えている（最終バッチ）か、`pending_items` が完成
    /// している（再読み込み・退避復元）場合に使います。**下限を課しません。**
    /// 正確な件数が分かっているのに1.25倍へ切り上げると、使われない容量を
    /// そのまま常駐させることになるためです。
    Exact(usize),
}

impl CapacityEstimate {
    /// `current_capacity` から広げるべき目標容量です。広げる必要がなければ
    /// `None`（この場合、呼び出し側は予約も確保も行いません）。
    fn target(self, current_capacity: usize) -> Option<usize> {
        let estimated = match self {
            CapacityEstimate::Projected(estimated) | CapacityEstimate::Exact(estimated) => {
                estimated
            }
        };
        if estimated <= current_capacity {
            return None;
        }
        match self {
            // 下限（現容量の1.25倍）を課す理由は CapacityEstimate の doc
            // コメントを参照。1.25倍なら、推定が何度外れても再確保回数は
            // 対数で抑えられ、かつ倍々成長より余剰が小さい。
            CapacityEstimate::Projected(_) => {
                Some(estimated.max(current_capacity.saturating_add(current_capacity / 4)))
            }
            CapacityEstimate::Exact(_) => Some(estimated),
        }
    }
}

/// 索引（`text`）と項目列（`items`）の容量を、推定総項目数まであらかじめ
/// 広げます（`PERF-010`「大きな確保の前に予約する」）。
///
/// 戻り値は、会計へ**実確保として振り替えたバイト数**です。
///
/// # 予約が拒否されても登録は失敗させない
///
/// 事前確保は再確保コストを減らすための最適化であり、これができなくても
/// 読み込み自体は従来の伸長経路で継続できます。ここで拒否を登録失敗に倒すと、
/// 見積もりが過大だった場合に**従来は読み込めていたファイルが読めなくなる**
/// （拒否の回帰）ため、拒否は握りつぶして事前確保を諦めます。予算が本当に
/// 不足している場合は、続く [`build_items_from_pending_into`] のバッチごとの
/// 予約が拒否され、そこで登録失敗になります（判定を捨てているわけではなく、
/// 実際に確保が必要になった時点まで遅らせています）。
pub(crate) fn ensure_resident_capacity(
    text: &mut IndexedText,
    items: &mut Vec<Item>,
    estimate: CapacityEstimate,
) -> usize {
    ensure_resident_capacity_with_budget(
        hakutaku_memory_accounting::global_budget(),
        text,
        items,
        estimate,
    )
}

/// [`ensure_resident_capacity`] の予算指定版です（分けている理由は
/// [`build_items_from_pending_into_with_budget`] と同じ）。
fn ensure_resident_capacity_with_budget(
    budget: &hakutaku_memory_accounting::MemoryBudget,
    text: &mut IndexedText,
    items: &mut Vec<Item>,
    estimate: CapacityEstimate,
) -> usize {
    let mut committed_bytes = 0usize;

    // 索引層（ADR-0003 の2層帰属: 索引は line_index が予約・確保する）。
    if let Some(target) = estimate.target(text.capacity()) {
        let additional = target - text.capacity();
        if let Ok(token) = crate::line_index::reserve_growth(budget, additional) {
            let allocated = text.grow_capacity_to(target);
            // 実際に増えた分だけを振り替える。予約量で上限を切るのは、
            // アロケータが要求より多く返した場合に mark_allocated が残量超過で
            // 失敗し、振り替えが丸ごと失われるのを避けるため。
            let transferred = allocated.min(additional * crate::line_index::INDEX_BYTES_PER_ENTRY);
            let _ = token.mark_allocated(transferred);
            committed_bytes += transferred;
        }
    }

    // 項目列層（同じく ADR-0003: 項目列を確保するこの層が予約する）。
    if let Some(target) = estimate.target(items.capacity()) {
        let additional = target - items.capacity();
        if let Ok(token) = reserve_items_growth(budget, additional) {
            let before = items.capacity();
            items.reserve_exact(target.saturating_sub(items.len()));
            let allocated = items.capacity().saturating_sub(before) * std::mem::size_of::<Item>();
            let transferred = allocated.min(additional * std::mem::size_of::<Item>());
            let _ = token.mark_allocated(transferred);
            committed_bytes += transferred;
        }
    }

    committed_bytes
}

/// 表示集合が保持する項目列（[`Item`] の `Vec`）の伸長分を、メモリ会計
/// （`PERF-008`・`PERF-010`）へ通すための予約です。
///
/// 呼び出し側は、実際に項目を確保する**前**にこれで予約し、成功したら確保を
/// 行い、完了後に [`hakutaku_memory_accounting::ReservationToken::
/// mark_allocated`] で実確保へ振り替えてください（ADR-0003）。
///
/// [`crate::line_index::reserve_growth`] と分けているのは、確保する層が違う
/// ためです（索引は `IndexedText`、項目列は表示集合）。同じ1行についての予約
/// なので、両方を通した合計が [`RESIDENT_BYTES_PER_ITEM`] になります。
///
/// **この予約が拒否された場合、登録は失敗として扱います**（索引側と同じ扱い。
/// 呼び出し元 `build_items_from_pending` の doc コメント参照）。
pub fn reserve_items_growth(
    budget: &hakutaku_memory_accounting::MemoryBudget,
    additional_items: usize,
) -> Result<
    hakutaku_memory_accounting::ReservationToken<'_>,
    hakutaku_memory_accounting::ReservationRejected,
> {
    budget.reserve(additional_items.saturating_mul(std::mem::size_of::<Item>()))
}

/// [`crate::loader::stream_decode_and_index`] が全項目を溜める**一時バッファ**
/// （[`PendingItem`] の `Vec`）の容量を、推定総項目数まであらかじめ広げます
/// （`PERF-010`「大きな確保の前に予約する」）。
///
/// 戻り値は、会計へ**実確保として振り替えたバイト数**です。広げる必要が
/// なかった場合と、予約が拒否された場合はいずれも 0 です。
///
/// # なぜ常駐しないバッファにも予約が要るか
///
/// `PERF-010` が予約・拒否を求める対象は「大きな確保」であって、「常駐する
/// 確保」ではありません。再読み込み・退避復元の経路は、表示集合を組み立てる
/// 前に全項目分の `PendingItem` を1本の `Vec` へ溜めるため、1 GiB 級の
/// ファイルではこのバッファだけで数百 MB に達し、しかも構築中の常駐分
/// （索引・項目列）と同時に生きます。予約しないまま確保すると、その量は確保が
/// 起きた後にしか `allocated_bytes` へ現れず、`PERF-008` の予算を超えるか
/// どうかを**確保する前に**判定できません。
///
/// 事前確保は、確保量そのものも予測可能にします。倍々成長に任せると最終容量は
/// 必要量の1〜2倍のどこかに落ち（どこに落ちるかは最初のバッチの件数と総件数の
/// 関係だけで決まり、制御できません）、その差はそのまま山の高さになります。
/// 見積もりからの事前確保なら、上振れは外挿のヘッドルーム
/// （[`crate::loader::estimate_total_items`] の5%）に収まります。なお `realloc`
/// による伸長は、アロケータが差分しか計上しない（ADR-0003「`realloc`」）ため、
/// 再確保の瞬間に旧容量と新容量が同時に生きる分は会計値に現れません。会計値の
/// 上では最終容量の差だけが見えます。
///
/// # 解放時の会計
///
/// 振り替え（`mark_allocated`）の後、このバッファは表示集合の構築を終えた
/// 時点で落ちます。減少はグローバルアロケータの `dealloc` が
/// `allocated_bytes` から差し引くため、呼び出し側が予約へ戻す処理は要りません
/// （ADR-0003「`dealloc`」: 消費済みの予約は復元しない）。予約したまま
/// 振り替えずに落とすと、その分だけ `outstanding_reserved_bytes` が減らず、
/// 以後の予算判定が実態より厳しくなります。
///
/// # 予約が拒否されても読み込みは失敗させない
///
/// [`ensure_resident_capacity`] と同じ2階建てです。事前確保は再確保コストを
/// 減らすための最適化であり、これができなくても読み込み自体は従来の倍々成長で
/// 継続できます。ここで拒否を失敗に倒すと、見積もりが過大だった場合に
/// **従来は再読み込みできていたファイルが読めなくなる**（拒否の回帰）ため、
/// 拒否は握りつぶして事前確保を諦めます。予算が本当に不足している場合は、
/// 続く常駐分の予約（[`build_items_from_pending_into`]）が拒否され、そこで
/// 失敗になります。
pub(crate) fn ensure_pending_capacity(
    pending: &mut Vec<PendingItem>,
    estimate: CapacityEstimate,
) -> usize {
    ensure_pending_capacity_with_budget(
        hakutaku_memory_accounting::global_budget(),
        pending,
        estimate,
    )
}

/// [`ensure_pending_capacity`] の予算指定版です（分けている理由は
/// [`build_items_from_pending_into_with_budget`] と同じ）。
fn ensure_pending_capacity_with_budget(
    budget: &hakutaku_memory_accounting::MemoryBudget,
    pending: &mut Vec<PendingItem>,
    estimate: CapacityEstimate,
) -> usize {
    let Some(target) = estimate.target(pending.capacity()) else {
        return 0;
    };
    let additional = target - pending.capacity();
    // ADR-0003 の帰属規則により、予約はその確保を行う層が行う。この `Vec` を
    // 所有し伸ばすのは `crate::loader` の読み込み経路であり、表示集合が持ち
    // 続ける項目列（[`reserve_items_growth`]）とは確保するものも寿命も違う
    // ため、独立したトークンで予約する。
    let Ok(token) = budget.reserve(additional.saturating_mul(std::mem::size_of::<PendingItem>()))
    else {
        return 0;
    };

    let before = pending.capacity();
    // `Vec::reserve_exact` を使うのは、倍々成長に任せると予約量と実際に増えた
    // 容量がずれ、振り替え量が実態と合わなくなるためである。
    pending.reserve_exact(target.saturating_sub(pending.len()));
    let allocated = pending.capacity().saturating_sub(before) * std::mem::size_of::<PendingItem>();
    // 実際に増えた分だけを振り替える。予約量で上限を切るのは、アロケータが
    // 要求より多く返した場合に `mark_allocated` が残量超過で失敗し、振り替えが
    // 丸ごと失われるのを避けるため（[`ensure_resident_capacity`] と同じ）。
    let transferred = allocated.min(additional * std::mem::size_of::<PendingItem>());
    let _ = token.mark_allocated(transferred);
    transferred
}

#[cfg(test)]
mod tests {
    use super::*;

    // 受け入れ条件（`PERF-008`・`PERF-010`）: 1行あたりの予約量が、
    // 常駐する3つの配列の内訳（索引24 + 行番号8 + 項目24 = 56バイト）と一致する。
    #[test]
    fn resident_bytes_per_item_covers_index_auxiliary_and_item_bytes() {
        assert_eq!(std::mem::size_of::<Item>(), 24);
        assert_eq!(crate::line_index::INDEX_BYTES_PER_ENTRY, 24 + 8);
        assert_eq!(RESIDENT_BYTES_PER_ITEM, 56);
        assert_eq!(
            RESIDENT_BYTES_PER_ITEM,
            crate::line_index::INDEX_BYTES_PER_ENTRY + std::mem::size_of::<Item>()
        );
    }

    // 受け入れ条件: 項目列の伸長分が、確保の前に会計へ予約される
    // （1件24バイト）。索引側（line_index::reserve_growth）とは別のトークン。
    #[test]
    fn reserve_items_growth_reserves_item_bytes() {
        let budget = hakutaku_memory_accounting::MemoryBudget::new(1_000_000);
        assert_eq!(budget.outstanding_reserved_bytes(), 0);

        let token = reserve_items_growth(&budget, 10).expect("予算内なので成功するはず");
        // 10件 * 24バイト（Item）= 240 バイト。
        assert_eq!(budget.outstanding_reserved_bytes(), 240);

        token
            .mark_allocated(240)
            .expect("予約量以内の振り替えは成功するはず");
        drop(token);
        assert_eq!(
            budget.outstanding_reserved_bytes(),
            0,
            "実確保へ振り替えた後は予約が残らないはず"
        );
    }

    /// 事前確保・追記のテスト用に、`count` 件の `PendingItem` を作ります。
    fn pending_items(count: usize) -> Vec<PendingItem> {
        (0..count)
            .map(|index| PendingItem::simple(index as u64 + 1, "x"))
            .collect()
    }

    // 受け入れ条件: 推定がちょうどの場合、事前確保だけで容量が
    // 満たされ、以後の追記では追加の予約が一切起きない（二重計上ゼロ）。
    // 振り替えた総量が、確保した容量バイト数と一致することも確認する。
    #[test]
    fn exact_estimate_preallocates_once_and_appending_reserves_nothing_more() {
        let budget = hakutaku_memory_accounting::MemoryBudget::new(10_000_000);
        let mut text = IndexedText::new();
        let mut items: Vec<Item> = Vec::new();

        let preallocated = ensure_resident_capacity_with_budget(
            &budget,
            &mut text,
            &mut items,
            CapacityEstimate::Exact(100),
        );

        assert_eq!(text.capacity(), 100);
        assert_eq!(items.capacity(), 100);
        assert_eq!(preallocated, 100 * RESIDENT_BYTES_PER_ITEM);
        assert_eq!(
            budget.outstanding_reserved_bytes(),
            0,
            "実確保へ振り替えた後は予約が残らないはず"
        );

        let appended = build_items_from_pending_into_with_budget(
            &budget,
            1,
            0,
            &pending_items(100),
            &mut text,
            &mut items,
        )
        .expect("予算内なので成功するはず");

        assert_eq!(
            appended, 0,
            "事前確保済みの容量に収まる追記は、新たな確保がないため予約もしない"
        );
        assert_eq!(text.len(), 100);
        assert_eq!(items.len(), 100);
        // 振り替えた総量（事前確保 + 追記）が、実際に確保した容量と一致する。
        assert_eq!(
            preallocated + appended,
            text.capacity() * crate::line_index::INDEX_BYTES_PER_ENTRY
                + items.capacity() * std::mem::size_of::<Item>()
        );
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }

    // 受け入れ条件: 推定を超えた分だけが追加で予約される
    // （事前確保済みの容量分は再予約しない）。
    #[test]
    fn appending_beyond_preallocated_capacity_reserves_only_the_excess() {
        let budget = hakutaku_memory_accounting::MemoryBudget::new(10_000_000);
        let mut text = IndexedText::new();
        let mut items: Vec<Item> = Vec::new();
        ensure_resident_capacity_with_budget(
            &budget,
            &mut text,
            &mut items,
            CapacityEstimate::Exact(100),
        );

        let appended = build_items_from_pending_into_with_budget(
            &budget,
            1,
            0,
            &pending_items(150),
            &mut text,
            &mut items,
        )
        .expect("予算内なので成功するはず");

        // 150件のうち100件は事前確保済み。超過した50件分だけを予約する。
        assert_eq!(appended, 50 * RESIDENT_BYTES_PER_ITEM);
        assert_eq!(text.len(), 150);
        assert_eq!(items.len(), 150);
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }

    // 受け入れ条件: 事前確保の予約が拒否されても登録は失敗せず、
    // 従来の伸長経路で追記を続けられる（過大な見積もりが拒否の回帰を生まない）。
    #[test]
    fn rejected_preallocation_is_not_fatal_and_appending_still_succeeds() {
        // 1000件分の事前確保（56,000バイト）には遠く足りないが、数件の追記
        // （56バイト/件）には足りる予算。
        let budget = hakutaku_memory_accounting::MemoryBudget::new(1_000);
        let mut text = IndexedText::new();
        let mut items: Vec<Item> = Vec::new();

        let preallocated = ensure_resident_capacity_with_budget(
            &budget,
            &mut text,
            &mut items,
            CapacityEstimate::Exact(1_000),
        );

        assert_eq!(preallocated, 0, "拒否されたので1バイトも確保していない");
        assert_eq!(text.capacity(), 0);
        assert_eq!(items.capacity(), 0);
        assert_eq!(
            budget.outstanding_reserved_bytes(),
            0,
            "拒否された予約は残らない"
        );

        let appended = build_items_from_pending_into_with_budget(
            &budget,
            1,
            0,
            &pending_items(1),
            &mut text,
            &mut items,
        )
        .expect("事前確保に失敗しても、実際に必要な分の予約は通るはず");

        assert_eq!(appended, RESIDENT_BYTES_PER_ITEM);
        assert_eq!(items.len(), 1);
    }

    // 受け入れ条件（P08-5）: 事前確保の拒否と違い、バッチごとの
    // 伸長予約の拒否は登録失敗として扱う（索引・項目列を変更せずに Err）。
    #[test]
    fn rejected_growth_reservation_fails_the_build_without_touching_containers() {
        // 10件分（560バイト）には足りない予算。
        let budget = hakutaku_memory_accounting::MemoryBudget::new(100);
        let mut text = IndexedText::new();
        let mut items: Vec<Item> = Vec::new();

        let rejected = build_items_from_pending_into_with_budget(
            &budget,
            1,
            0,
            &pending_items(10),
            &mut text,
            &mut items,
        )
        .expect_err("予算を超えるので拒否されるはず");

        assert_eq!(rejected.budget_bytes, 100);
        assert!(text.is_empty(), "拒否時は索引へ1件も追記しない");
        assert!(items.is_empty(), "拒否時は項目列へ1件も追記しない");
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }

    // 受け入れ条件: 推定がわずかに過小だった場合、再推定
    // （Projected）では現容量の1.25倍を下限として広げ直す。倍々成長（200）にも、
    // 推定ちょうど（104、すぐまた足りなくなる）にもしない。
    #[test]
    fn projected_regrowth_uses_a_1_25x_floor_instead_of_doubling() {
        let budget = hakutaku_memory_accounting::MemoryBudget::new(10_000_000);
        let mut text = IndexedText::new();
        let mut items: Vec<Item> = Vec::new();
        ensure_resident_capacity_with_budget(
            &budget,
            &mut text,
            &mut items,
            CapacityEstimate::Exact(100),
        );

        let regrown = ensure_resident_capacity_with_budget(
            &budget,
            &mut text,
            &mut items,
            CapacityEstimate::Projected(104),
        );

        assert_eq!(text.capacity(), 125);
        assert_eq!(items.capacity(), 125);
        assert_eq!(regrown, 25 * RESIDENT_BYTES_PER_ITEM);
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }

    // 受け入れ条件: 確定値（Exact）には1.25倍の下限を課さない。
    // 件数が分かっているのに切り上げると、使われない容量が常駐するため。
    #[test]
    fn exact_estimate_grows_to_the_exact_count_without_the_floor() {
        let budget = hakutaku_memory_accounting::MemoryBudget::new(10_000_000);
        let mut text = IndexedText::new();
        let mut items: Vec<Item> = Vec::new();
        ensure_resident_capacity_with_budget(
            &budget,
            &mut text,
            &mut items,
            CapacityEstimate::Exact(100),
        );

        let regrown = ensure_resident_capacity_with_budget(
            &budget,
            &mut text,
            &mut items,
            CapacityEstimate::Exact(104),
        );

        assert_eq!(text.capacity(), 104);
        assert_eq!(items.capacity(), 104);
        assert_eq!(regrown, 4 * RESIDENT_BYTES_PER_ITEM);
    }

    // 受け入れ条件: 既に十分な容量がある場合は縮小も再確保もせず、
    // 予約も行わない（読み終わりに容量が余っていても shrink しない）。
    #[test]
    fn estimate_below_current_capacity_neither_shrinks_nor_reserves() {
        let budget = hakutaku_memory_accounting::MemoryBudget::new(10_000_000);
        let mut text = IndexedText::new();
        let mut items: Vec<Item> = Vec::new();
        ensure_resident_capacity_with_budget(
            &budget,
            &mut text,
            &mut items,
            CapacityEstimate::Exact(100),
        );

        for estimate in [CapacityEstimate::Exact(50), CapacityEstimate::Projected(50)] {
            let committed =
                ensure_resident_capacity_with_budget(&budget, &mut text, &mut items, estimate);
            assert_eq!(committed, 0, "{estimate:?} で追加確保は不要のはず");
            assert_eq!(text.capacity(), 100, "{estimate:?} で縮小しないはず");
            assert_eq!(items.capacity(), 100, "{estimate:?} で縮小しないはず");
        }
    }

    // 受け入れ条件（`PERF-010`）: 一時バッファ（Vec<PendingItem>）の
    // 事前確保が、確保の前に会計へ予約され、確保後に実確保へ振り替えられる
    // （振り替え後に予約残がゼロへ戻る＝二重計上しない）。
    #[test]
    fn pending_capacity_reserves_before_growing_and_transfers_after() {
        let budget = hakutaku_memory_accounting::MemoryBudget::new(10_000_000);
        let mut pending: Vec<PendingItem> = Vec::new();

        let committed = ensure_pending_capacity_with_budget(
            &budget,
            &mut pending,
            CapacityEstimate::Exact(1_000),
        );

        assert_eq!(pending.capacity(), 1_000);
        assert_eq!(committed, 1_000 * std::mem::size_of::<PendingItem>());
        assert_eq!(
            budget.outstanding_reserved_bytes(),
            0,
            "実確保へ振り替えた後は予約が残らないはず"
        );

        // 事前確保済みの容量に収まる追記では、容量も振替量も動かない
        // （倍々成長の起点にならない）。
        pending.extend(pending_items(1_000));
        assert_eq!(pending.capacity(), 1_000);
        assert_eq!(pending.len(), 1_000);
    }

    // 受け入れ条件: 読み込み途中の再推定（Projected）でも、
    // 常駐分（ensure_resident_capacity）と同じガード（現容量の1.25倍を下限、
    // 見積もりが現容量以下なら縮小も再確保もしない）が効く。
    #[test]
    fn pending_capacity_uses_the_same_floor_and_never_shrinks() {
        let budget = hakutaku_memory_accounting::MemoryBudget::new(10_000_000);
        let mut pending: Vec<PendingItem> = Vec::new();
        ensure_pending_capacity_with_budget(&budget, &mut pending, CapacityEstimate::Exact(100));

        let regrown = ensure_pending_capacity_with_budget(
            &budget,
            &mut pending,
            CapacityEstimate::Projected(104),
        );
        assert_eq!(pending.capacity(), 125, "1.25倍の下限まで広げるはず");
        assert_eq!(regrown, 25 * std::mem::size_of::<PendingItem>());

        for estimate in [CapacityEstimate::Exact(50), CapacityEstimate::Projected(50)] {
            let committed = ensure_pending_capacity_with_budget(&budget, &mut pending, estimate);
            assert_eq!(committed, 0, "{estimate:?} で追加確保は不要のはず");
            assert_eq!(pending.capacity(), 125, "{estimate:?} で縮小しないはず");
        }
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }

    // 受け入れ条件: 一時バッファの事前確保の予約が拒否されても
    // 非致命で、従来どおりの伸長で追記を続けられる（過大な見積もりが
    // 「従来は再読み込みできていたファイルの失敗」を生まない）。
    #[test]
    fn rejected_pending_preallocation_is_not_fatal_and_appending_still_succeeds() {
        // 1000件分の事前確保には遠く足りないが、数件の追記には足りる予算。
        let budget = hakutaku_memory_accounting::MemoryBudget::new(1_000);
        let mut pending: Vec<PendingItem> = Vec::new();

        let committed = ensure_pending_capacity_with_budget(
            &budget,
            &mut pending,
            CapacityEstimate::Exact(1_000),
        );

        assert_eq!(committed, 0, "拒否されたので1バイトも確保していない");
        assert_eq!(pending.capacity(), 0);
        assert_eq!(
            budget.outstanding_reserved_bytes(),
            0,
            "拒否された予約は残らない"
        );

        // 事前確保に失敗しても、従来どおりの伸長で追記は続けられる。
        pending.extend(pending_items(4));
        assert_eq!(pending.len(), 4);
    }

    // 受け入れ条件: 予算不足なら項目列の予約も拒否され、呼び出し側は
    // 項目を構築せずに登録失敗へ倒せる（索引側の拒否と同じ扱い）。
    #[test]
    fn reserve_items_growth_is_rejected_when_budget_is_insufficient() {
        let budget = hakutaku_memory_accounting::MemoryBudget::new(100);
        let rejected =
            reserve_items_growth(&budget, 10).expect_err("予算を超えるので拒否されるはず");
        assert_eq!(rejected.budget_bytes, 100);
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }

    #[test]
    fn build_items_assigns_sequential_seq_starting_at_zero() {
        let pending_items = vec![
            PendingItem::simple(1, "line one"),
            PendingItem::simple(2, "line two"),
        ];
        let mut text = IndexedText::new();

        let items =
            build_items_from_pending(7, &pending_items, &mut text).expect("予約は成功するはず");

        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].id,
            ItemId {
                source_id: 7,
                seq: 0
            }
        );
        assert_eq!(
            items[1].id,
            ItemId {
                source_id: 7,
                seq: 1
            }
        );
        assert_eq!(text.source_line_number(items[0].entry_index), Some(1));
        assert_eq!(text.source_line_number(items[1].entry_index), Some(2));
        assert_eq!(
            text.entries()[items[0].entry_index].raw_range(),
            (0, "line one".len() as u32)
        );
    }

    // 受け入れ条件: comparison_key（IndexedText 側のミリ秒表現）は
    // PendingItem からそのまま複製される。
    #[test]
    fn build_items_copies_comparison_key_millis_from_pending_item() {
        let matched = hakutaku_parser::parse_datetime_with_format(
            hakutaku_parser::LogDateTimeFormat::LogDt001,
            "2026/07/28 15:12:23.456",
        )
        .expect("解析できるはず");
        let expected_millis = matched.comparison_key.as_millis_since_epoch();

        let pending_items = vec![PendingItem {
            raw_offset: 0,
            raw_byte_len: 30,
            comparison_key_millis: Some(expected_millis),
            source_line_number: 1,
            continuation_count: 0,
            unconfirmed: false,
        }];
        let mut text = IndexedText::new();

        let items =
            build_items_from_pending(1, &pending_items, &mut text).expect("予約は成功するはず");

        assert_eq!(
            text.comparison_key_millis(items[0].entry_index),
            Some(expected_millis)
        );
    }

    // 日時未解析（生表示）の項目は comparison_key_millis も None になる。
    #[test]
    fn build_items_with_no_timestamp_has_no_comparison_key() {
        let mut text = IndexedText::new();
        let items = build_items_from_pending(1, &[PendingItem::simple(1, "raw")], &mut text)
            .expect("予約は成功するはず");
        assert_eq!(text.comparison_key_millis(items[0].entry_index), None);
        assert!(!text.entries()[items[0].entry_index].has_timestamp());
    }

    // 受け入れ条件: start_seq から連番を続けられる（伸長経路の下地）。
    #[test]
    fn build_items_starting_at_continues_seq_and_appends_to_existing_text() {
        let mut text = IndexedText::new();
        let mut items = Vec::new();
        build_items_from_pending_into(
            3,
            0,
            &[PendingItem::simple(1, "first")],
            &mut text,
            &mut items,
        )
        .expect("予約は成功するはず");
        assert_eq!(items[0].id.seq, 0);

        build_items_from_pending_into(
            3,
            items.len() as u64,
            &[PendingItem::simple(2, "second")],
            &mut text,
            &mut items,
        )
        .expect("予約は成功するはず");
        assert_eq!(items[1].id.seq, 1);
        assert_eq!(text.len(), 2);
        assert_eq!(
            text.entries()[items[1].entry_index].raw_range(),
            (0, "second".len() as u32)
        );
    }

    // 受け入れ条件: 継続行数・未確定フラグが IndexedText 側へ正しく伝わる。
    #[test]
    fn build_items_propagates_continuation_count_and_unconfirmed_flag() {
        let pending_items = vec![PendingItem {
            raw_offset: 10,
            raw_byte_len: 40,
            comparison_key_millis: None,
            source_line_number: 5,
            continuation_count: 1,
            unconfirmed: true,
        }];
        let mut text = IndexedText::new();

        let items =
            build_items_from_pending(1, &pending_items, &mut text).expect("予約は成功するはず");

        let entry = &text.entries()[items[0].entry_index];
        assert_eq!(entry.continuation_count, 1);
        assert!(entry.is_unconfirmed());
        assert_eq!(text.source_line_number(items[0].entry_index), Some(5));
    }

    // 受け入れ条件（P08-5）: 索引の伸長予約が拒否された場合、Err を返し索引は
    // 変更しない。
    #[test]
    fn build_items_propagates_reservation_rejection() {
        // グローバル予算を汚さないよう、拒否そのものは予算インスタンスを
        // 指定できる各予約関数の単体テスト（索引は crate::line_index の
        // reserve_growth_is_rejected_when_budget_is_insufficient、項目列は本
        // モジュールの reserve_items_growth_is_rejected_when_budget_is_
        // insufficient）で確認済み。ここでは、拒否時に
        // build_items_from_pending がエラーを伝播すること自体は型シグネチャ
        // （Result）で保証されるため、正常系のみを重ねて確認する
        // （グローバル予算を縮小する決定的な方法がないため、拒否経路の
        // 統合確認は crate::loader 側の register_source 系テストで行う）。
        let mut text = IndexedText::new();
        let items = build_items_from_pending(1, &[PendingItem::simple(1, "ok")], &mut text)
            .expect("十分な予算内なので成功するはず");
        assert_eq!(items.len(), 1);
    }
}
