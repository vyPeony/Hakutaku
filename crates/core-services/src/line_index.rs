//! 行索引（P06-2、`tasks/phase-06-large-file-loading.md` 作業項目2、5.2）と、
//! 索引 + オンデマンド読み出し（P08-5）。
//!
//! # 設計制約（5.2）
//!
//! 「行索引は1行あたり24バイト以下」という暫定設計です（要件 ID を持ちません）。
//! 合計2 GB・平均100バイト/行（約2000万行）では索引だけで約480 MBを占める
//! 見積もりであり、この制約を超えるとメモリ予算（`PERF-008`）を圧迫します。
//!
//! # レイアウト（[`LineIndexEntry`]、24バイト）
//!
//! P08-1 は `offset` を「デコード済み全文バッファ内の開始バイト
//! 位置」として定義していました。P08-5 で、
//! **本文（デコード済みテキスト、生バイトのいずれも）を常駐させない**ように
//! 意味を変更しました。`offset`・`byte_len` はいずれも**ファイルの生バイト**の
//! 位置・長さを指します。
//!
//! | フィールド | 型 | バイト数 | 内容 |
//! | --- | --- | --- | --- |
//! | `offset` | `u64` | 8 | ソースファイル先頭からの生バイトオフセット（BOM を除く。後述） |
//! | `comparison_key_millis` | `i64` | 8 | ミリ秒精度へ正規化した比較キー（[`hakutaku_parser::ComparisonKey::as_millis_since_epoch`] と同じ値）。日時未解析の場合は無効（`flags` の [`FLAG_HAS_TIMESTAMP`] で判定） |
//! | `byte_len` | `u32` | 4 | 項目本体の生バイト長（後述） |
//! | `flags` | `u16` | 2 | [`FLAG_HAS_TIMESTAMP`]・[`FLAG_UNCONFIRMED`] |
//! | `continuation_count` | `u16` | 2 | このエントリに結合された継続行（`LOG-014`）の数（0 は継続行なし） |
//!
//! 合計 8 + 8 + 4 + 2 + 2 = 24 バイトです。`#[repr(C)]` を指定し、
//! `size_of::<LineIndexEntry>() == 24` を静的（`const` 評価）・動的（テスト）の
//! 両方で検証します（この制約自体は P08-1 から変わっていません）。
//!
//! ## `offset`・`byte_len` の正確な意味（生バイト、P08-5）
//!
//! 項目（論理ログ行 = 継続行結合済み）単位という粒度は P08-1 から変えていません。
//! `offset` は、その項目の**先頭物理行の生バイト開始位置**（ソースファイル
//! 先頭からのバイトオフセット）です。UTF-8 BOM が存在する場合、`offset` は
//! BOM 分（3バイト）を含みません（先頭項目の `offset` は常に BOM の直後、
//! `crate::loader` の登録時ストリーミング解析がオフセットの起点をあらかじめ
//! BOM 分だけずらして記録するため。オンデマンド読み出し時に BOM の有無を
//! 気にする必要がないようにするための設計判断）。
//!
//! `byte_len` は、項目全体（継続行を含む）の生バイト長です。**項目の最後の
//! 物理行の行末区切り文字（`\n`／`\r\n`）は含みません。** 継続行を結合した
//! 項目の内部（物理行と物理行の間）にある区切り文字は、範囲としては
//! `[offset, offset+byte_len)` の内側に含まれます（デコード時にそのまま
//! 復元され、[`crate::loader`] のオンデマンド読み出し経路が `\r\n` を `\n` へ
//! 正規化します。モジュール doc コメント「デコードと `\r\n` の正規化」参照）。
//!
//! ## 生バイトの行分割の安全性
//!
//! [`hakutaku_data_source::split_raw_lines`] の doc コメントに詳細がありますが、
//! `\n`（`0x0A`）・`\r`（`0x0D`）はいずれの対応文字コード（UTF-8、Windows
//! コードページ群）でもマルチバイト文字の構成バイトとして現れないため、生
//! バイト列を `\n` で分割しても各行の内容は常に単独でデコードできる完全な
//! バイト列です。したがって、項目の `[offset, offset+byte_len)` はファイルの
//! 任意の位置から独立して安全にデコードできます（マルチバイト文字の途中を
//! 割ることはありません）。
//!
//! # 行番号補助配列（[`IndexedText`] の付加情報）
//!
//! `source_line_number`（ソース内の1起点行番号。`ItemDto` の既存フィールド）は、
//! 24バイトの [`LineIndexEntry`] 本体には含めません（5.2 の制約を守るため）。
//! `Vec<u64>` の並列配列として保持します（1行あたり追加8バイト。24バイト本体
//! とは別会計です）。
//!
//! # 本文バッファを保持しない（P08-5）
//!
//! **P08-1 まで存在した `IndexedText::text: String`（デコード済み全文バッファ）
//! は廃止しました。** [`IndexedText`] は索引（[`LineIndexEntry`] の並び）と
//! 行番号の並列配列だけを保持します。常駐メモリは索引 + 補助配列のみです。
//!
//! 本文の取得（範囲取得時の `ItemDto::raw_text` の組み立て）は、
//! `crate::registry::DisplaySetRegistry::fetch_range` が、登録時に記録した
//! ソースのファイルパスへ都度アクセスし、`offset`・`byte_len` の範囲を
//! 読み込んでデコードすることで行います（オンデマンド読み出し）。デコード
//! 済みチャンクの有界キャッシュ（[`crate::chunk_cache`]）が、繰り返しの読み出し
//! コストを抑えます。
//!
//! `timestamp_display`（元の精度・書式を保つ文字列、`LOG-024`）の再構成方式
//! （ソースごとに確定した [`hakutaku_parser::LogDateTimeFormat`] を使い、
//! 読み出した本文の先頭物理行を再解析する）は P08-1 と変えていません。
//! 実装は `crate::registry` へ移りました（オンデマンド読み出しの一部である
//! ため）。
//!
//! # 解放（[`IndexedText::clear`]）
//!
//! `clear` は `entries`・`source_line_numbers` を新しい空のコンテナへ置き換えます
//! （`Vec::clear` は容量を保持したままで実メモリを解放しないため使いません）。

use hakutaku_memory_accounting::{MemoryBudget, ReservationRejected, ReservationToken};

/// 日時が解析できた行であることを示すフラグです。
pub const FLAG_HAS_TIMESTAMP: u16 = 0b0000_0001;

/// 末尾が改行で終わらない未確定行（`LOG-026`）であることを示すフラグです。
/// 解析エラーではなく、表示上の区別のための付随情報です。
pub const FLAG_UNCONFIRMED: u16 = 0b0000_0010;

/// 行索引の1エントリです（24バイト以下という設計制約。5.2）。
///
/// `#[repr(C)]` によりフィールド順を固定し、`size_of` の予測可能性を保ちます
/// （テストの静的検証・実測の前提）。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineIndexEntry {
    /// ソースファイル先頭からの生バイトオフセット（BOM を除く。モジュール
    /// doc コメント「`offset`・`byte_len` の正確な意味」参照）。
    pub offset: u64,
    /// ミリ秒精度の比較キー（[`FLAG_HAS_TIMESTAMP`] が立っている場合だけ有効）。
    pub comparison_key_millis: i64,
    /// 項目全体（継続行を含む）の生バイト長（最後の物理行の行末区切り文字を
    /// 含まない）。
    pub byte_len: u32,
    /// [`FLAG_HAS_TIMESTAMP`]・[`FLAG_UNCONFIRMED`] のビット和。
    pub flags: u16,
    /// 結合された継続行（`LOG-014`）の数。0 は継続行なし（単独行）。
    pub continuation_count: u16,
}

/// 設計制約「1行あたり24バイト以下」の静的検証です。ビルド時（`const` 評価）に
/// 失敗するため、レイアウトの意図しない変化（フィールド追加など）を
/// コンパイルエラーで検出します。
const _: () = assert!(
    std::mem::size_of::<LineIndexEntry>() <= 24,
    "LineIndexEntry は5.2の設計制約により24バイト以下でなければならない"
);

impl LineIndexEntry {
    /// 日時が解析できた行か。
    #[must_use]
    pub const fn has_timestamp(&self) -> bool {
        self.flags & FLAG_HAS_TIMESTAMP != 0
    }

    /// 未確定行（`LOG-026`）か。DTO の `confirmed` フィールドはこの否定です。
    #[must_use]
    pub const fn is_unconfirmed(&self) -> bool {
        self.flags & FLAG_UNCONFIRMED != 0
    }

    /// [`Self::has_timestamp`] が真の場合だけ比較キー（ミリ秒）を返します。
    #[must_use]
    pub const fn comparison_key_millis(&self) -> Option<i64> {
        if self.has_timestamp() {
            Some(self.comparison_key_millis)
        } else {
            None
        }
    }

    /// この項目の生バイト範囲（`[offset, offset+byte_len)`）です。
    #[must_use]
    pub const fn raw_range(&self) -> (u64, u32) {
        (self.offset, self.byte_len)
    }
}

/// 行索引（[`LineIndexEntry`] の並び）と行番号の並列配列だけを保持します
/// （モジュール doc コメント「本文バッファを保持しない」参照）。P08-1 まで
/// 保持していたデコード済み全文バッファは廃止しました。
#[derive(Debug, Default)]
pub struct IndexedText {
    entries: Vec<LineIndexEntry>,
    /// 行番号の並列配列（モジュール doc コメント「行番号補助配列」）。
    source_line_numbers: Vec<u64>,
}

impl IndexedText {
    /// 空の状態で作成します。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// あらかじめ容量を確保して作成します（再確保の回数を減らすための
    /// ヒント。呼び出し側が `hakutaku_memory_accounting` へ予約した後に
    /// 使う想定です。[`reserve_growth`] を参照）。
    ///
    /// 予約なしで呼ぶと `PERF-010`（大きな確保の前に予約する）に反します。
    /// 予約と確保をまとめて行う入口は [`crate::item::ensure_resident_capacity`]
    /// です。
    #[must_use]
    pub fn with_capacity(entry_count: usize) -> Self {
        IndexedText {
            entries: Vec::with_capacity(entry_count),
            source_line_numbers: Vec::with_capacity(entry_count),
        }
    }

    /// 現在確保済みの容量（エントリ数）です。
    ///
    /// 2本の並列配列（[`LineIndexEntry`] の列と行番号の列）は常に同じ長さまで
    /// 追記されますが、容量は別々に決まります。**小さい方**を返すのは、この値を
    /// 「あと何件なら再確保なしで追記できるか」の判定に使うためです
    /// （[`Self::spare_capacity`]）。大きい方を返すと、片方だけが再確保を起こす
    /// 場合を見落とします。
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.entries
            .capacity()
            .min(self.source_line_numbers.capacity())
    }

    /// 再確保なしで追記できる残りのエントリ数です。
    ///
    /// メモリ会計（`PERF-008`・`PERF-010`）の二重計上を避けるために使います。
    /// この残量に収まる追記は新たなヒープ確保を伴わないため、[`reserve_growth`]
    /// で改めて予約すると、確保されないバイト数を予約したことになります
    /// （`crate::item::build_items_from_pending_into` の実装コメント参照）。
    #[must_use]
    pub fn spare_capacity(&self) -> usize {
        self.capacity().saturating_sub(self.len())
    }

    /// 容量を `target_entries` まで広げます（事前確保）。
    ///
    /// 既に十分な容量がある場合は何もしません。`Vec::reserve` ではなく
    /// `Vec::reserve_exact` を使うのは、呼び出し側が会計へ予約した量と実際の
    /// 確保量を一致させるためです。`reserve` は倍々成長（要求量より大きく確保
    /// する）ため、予約より多く確保してしまい、`PERF-010` の「確保の前に予約
    /// する」が崩れます。
    ///
    /// **予約なしで呼ばないでください。** 呼び出し側は
    /// [`crate::item::ensure_resident_capacity`] を通してください（ADR-0003 の
    /// 帰属規則により、予約・振り替えと確保を同じ場所で行うため）。
    ///
    /// 戻り値は、この呼び出しで実際に増えたバイト数です（2本の配列の合計）。
    /// 予約量ではなく実測値を返すのは、呼び出し側が
    /// [`hakutaku_memory_accounting::ReservationToken::mark_allocated`] へ
    /// 「実際に確保した量」を渡せるようにするためです。
    pub fn grow_capacity_to(&mut self, target_entries: usize) -> usize {
        let additional = target_entries.saturating_sub(self.len());
        let before = self.allocated_capacity_bytes();
        self.entries.reserve_exact(additional);
        self.source_line_numbers.reserve_exact(additional);
        self.allocated_capacity_bytes().saturating_sub(before)
    }

    /// 2本の並列配列が確保している容量の合計バイト数です（長さではなく容量。
    /// [`Self::grow_capacity_to`] の実測用）。
    fn allocated_capacity_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<LineIndexEntry>()
            + self.source_line_numbers.capacity() * std::mem::size_of::<u64>()
    }

    /// 1エントリ（1論理項目）を追加します。
    ///
    /// `raw_offset`・`raw_byte_len` は、ファイル先頭からの生バイトオフセットと
    /// 項目本体の生バイト長です（モジュール doc コメント「`offset`・
    /// `byte_len` の正確な意味」を参照。継続行を含む場合はその全体を覆う範囲）。
    ///
    /// `source_line_number` はソース内の1起点行番号（継続行を含む場合は
    /// 先頭物理行の行番号）です。
    ///
    /// 戻り値はエントリの添字（範囲取得時に `crate::registry` が本文を
    /// 読み出す際の索引）です。
    pub fn push_entry(
        &mut self,
        raw_offset: u64,
        raw_byte_len: u32,
        comparison_key_millis: Option<i64>,
        unconfirmed: bool,
        continuation_count: u16,
        source_line_number: u64,
    ) -> usize {
        let mut flags = 0u16;
        let key = match comparison_key_millis {
            Some(key) => {
                flags |= FLAG_HAS_TIMESTAMP;
                key
            }
            None => 0,
        };
        if unconfirmed {
            flags |= FLAG_UNCONFIRMED;
        }

        self.entries.push(LineIndexEntry {
            offset: raw_offset,
            comparison_key_millis: key,
            byte_len: raw_byte_len,
            flags,
            continuation_count,
        });
        self.source_line_numbers.push(source_line_number);
        self.entries.len() - 1
    }

    /// エントリ数（=論理項目数、継続行を結合した単位）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 索引本体を返します。
    #[must_use]
    pub fn entries(&self) -> &[LineIndexEntry] {
        &self.entries
    }

    /// 索引が占めるバイト数の実測値（`entries.len() * size_of::<LineIndexEntry>()`）。
    /// 「実測で索引バイト数 / 行数 <= 24」を確認するテストが使います。5.2 の
    /// 制約対象は本体（この値）だけであり、[`Self::auxiliary_bytes`]（行番号の
    /// 並列配列）は含みません。
    #[must_use]
    pub fn index_bytes(&self) -> usize {
        self.entries.len() * std::mem::size_of::<LineIndexEntry>()
    }

    /// 24バイト本体とは別に保持する付加情報（行番号の並列配列）が占める
    /// バイト数の実測値です（モジュール doc コメント「行番号補助配列」）。
    #[must_use]
    pub fn auxiliary_bytes(&self) -> usize {
        self.source_line_numbers.len() * std::mem::size_of::<u64>()
    }

    /// `index` 番目のエントリの比較キー（ミリ秒）。日時未解析なら `None`。
    #[must_use]
    pub fn comparison_key_millis(&self, index: usize) -> Option<i64> {
        self.entries.get(index)?.comparison_key_millis()
    }

    /// `index` 番目のエントリのソース内行番号（1起点）。
    #[must_use]
    pub fn source_line_number(&self, index: usize) -> Option<u64> {
        self.source_line_numbers.get(index).copied()
    }

    /// 保持している索引を解放します（P08-3、しきい値到達時の解放対象登録の
    /// 名残。P08-5 以降は索引自体が小さいため、しきい値到達時の主要な解放対象
    /// ではなくなりました。`crate::chunk_cache` の doc コメント参照）。
    /// 解放後は `len() == 0` になります。
    pub fn clear(&mut self) {
        self.entries = Vec::new();
        self.source_line_numbers = Vec::new();
    }
}

/// [`IndexedText`] が1エントリあたり確保するバイト数です（32 = 24 + 8）。
///
/// 内訳は索引本体（[`LineIndexEntry`]、24バイト）と行番号の並列配列
/// （`u64`、8バイト）です。[`reserve_growth`] の予約量はこの値に比例します。
///
/// **これは `IndexedText` の分だけであり、1行あたりの常駐コスト全体ではありま
/// せん。** 表示集合が別に保持する項目列（`crate::item::Item`）を含めた合計は
/// [`crate::item::RESIDENT_BYTES_PER_ITEM`] を参照してください。
pub const INDEX_BYTES_PER_ENTRY: usize =
    std::mem::size_of::<LineIndexEntry>() + std::mem::size_of::<u64>();

/// [`IndexedText`] の確保をメモリ会計（`PERF-008`・`PERF-010`）へ通すための
/// 予約です。呼び出し側は、実際に [`IndexedText::push_entry`] を呼ぶ**前**に
/// これで予約し、成功したら構築・追記を行い、完了後に
/// [`hakutaku_memory_accounting::ReservationToken::mark_allocated`] で実確保へ
/// 振り替えてください。
///
/// P08-5 で本文バッファを保持しなくなったため、予約対象は索引
/// 本体（24バイト/エントリ）と付加情報（行番号の並列配列、8バイト/エントリ）
/// だけです（本文バイト数の予約は不要になりました）。
///
/// **この関数は `IndexedText` の分しか予約しません。** 同じ1行につき表示集合が
/// 保持する `crate::item::Item`（24バイト）は、それを確保する層が
/// [`crate::item::reserve_items_growth`] で別に予約します（ADR-0003 の帰属規則:
/// 予約と振り替えは、その確保を行う場所で行う）。両者を合わせた1行あたりの
/// 予約量が [`crate::item::RESIDENT_BYTES_PER_ITEM`] です。
///
/// `crate::loader` は、読み込み中のバッチごとに、この関数と
/// [`crate::item::reserve_items_growth`] で成長分を都度予約します
/// （`crate::item` の doc コメント参照）。**この予約が拒否された場合、登録は
/// 失敗として扱います**（P08-1 まではベストエフォートで無視していましたが、
/// P08-5 で必須化しました。`PERF-008` の利用者向けメッセージで返します）。
pub fn reserve_growth(
    budget: &MemoryBudget,
    additional_entries: usize,
) -> Result<ReservationToken<'_>, ReservationRejected> {
    budget.reserve(additional_entries.saturating_mul(INDEX_BYTES_PER_ENTRY))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 受け入れ条件: LineIndexEntry のサイズが24バイト以下である（静的検証）。
    #[test]
    fn line_index_entry_size_is_at_most_24_bytes() {
        assert!(std::mem::size_of::<LineIndexEntry>() <= 24);
        // 5.2 の設計どおり、実際にちょうど24バイトに収まっていることも確認する
        // （余裕を持たせすぎて索引が肥大化していないことの確認）。
        assert_eq!(std::mem::size_of::<LineIndexEntry>(), 24);
    }

    // 受け入れ条件: 実測で索引バイト数 / 行数 <= 24（N行のファイル相当の索引）。
    #[test]
    fn measured_index_bytes_per_line_is_at_most_24() {
        let mut indexed = IndexedText::new();
        let mut offset = 0u64;
        for i in 0..20_000u32 {
            let len = 40u32;
            indexed.push_entry(
                offset,
                len,
                Some(1_753_679_543_000 + i64::from(i)),
                false,
                0,
                u64::from(i) + 1,
            );
            offset += u64::from(len) + 1;
        }
        assert_eq!(indexed.len(), 20_000);
        let bytes_per_line = indexed.index_bytes() / indexed.len();
        assert!(
            bytes_per_line <= 24,
            "索引バイト数/行数が24を超えている: {bytes_per_line}"
        );
        assert_eq!(indexed.index_bytes(), 20_000 * 24);
        // 付加情報（行番号の並列配列）は本体とは別会計で8バイト/行。
        assert_eq!(indexed.auxiliary_bytes(), 20_000 * 8);
    }

    // 受け入れ条件: push_entry で記録した生バイトオフセット・長さ・比較キー・
    // 行番号・フラグがそのまま往復できる。
    #[test]
    fn push_entry_round_trips_offset_len_key_and_flags() {
        let mut indexed = IndexedText::new();
        let idx0 = indexed.push_entry(0, 6, Some(1000), false, 0, 1);
        let idx1 = indexed.push_entry(7, 9, None, false, 0, 2);
        let idx2 = indexed.push_entry(17, 21, None, true, 0, 3);

        assert_eq!(indexed.entries()[idx0].raw_range(), (0, 6));
        assert_eq!(indexed.entries()[idx1].raw_range(), (7, 9));
        assert_eq!(indexed.entries()[idx2].raw_range(), (17, 21));

        assert_eq!(indexed.comparison_key_millis(idx0), Some(1000));
        assert_eq!(indexed.comparison_key_millis(idx1), None);
        assert!(!indexed.entries()[idx0].is_unconfirmed());
        assert!(indexed.entries()[idx2].is_unconfirmed());

        assert_eq!(indexed.source_line_number(idx0), Some(1));
        assert_eq!(indexed.source_line_number(idx1), Some(2));
        assert_eq!(indexed.source_line_number(idx2), Some(3));
    }

    // 受け入れ条件: 継続行を結合した1論理項目の継続行数・範囲が正しく記録される。
    #[test]
    fn push_entry_stores_continuation_count_for_merged_items() {
        let mut indexed = IndexedText::new();
        let idx = indexed.push_entry(0, 80, Some(1), false, 2, 1);

        assert_eq!(indexed.len(), 1);
        assert_eq!(indexed.entries()[idx].raw_range(), (0, 80));
        assert_eq!(indexed.entries()[idx].continuation_count, 2);
        assert_eq!(indexed.source_line_number(idx), Some(1));
    }

    // 存在しない添字は None を返し panic しない。
    #[test]
    fn accessors_for_out_of_range_index_return_none() {
        let indexed = IndexedText::new();
        assert_eq!(indexed.comparison_key_millis(0), None);
        assert_eq!(indexed.source_line_number(0), None);
        assert!(indexed.entries().is_empty());
    }

    // 受け入れ条件: clear で解放され、以後は空の索引として振る舞う。
    #[test]
    fn clear_releases_index_and_leaves_indexed_text_empty() {
        let mut indexed = IndexedText::new();
        indexed.push_entry(0, 6, Some(1), false, 0, 1);
        indexed.push_entry(7, 6, None, false, 0, 2);
        assert_eq!(indexed.len(), 2);

        indexed.clear();

        assert_eq!(indexed.len(), 0);
        assert!(indexed.is_empty());
        assert_eq!(indexed.index_bytes(), 0);
        assert_eq!(indexed.auxiliary_bytes(), 0);
    }

    // 受け入れ条件: 確保が P02 の予約を通る（会計値の観測）。索引本体・行番号の
    // 並列配列の両方を含めて予約する（本文バイト数は含まない、P08-5。項目列
    // （crate::item::Item）はこの関数の対象外で、item 側が別に予約する）。
    #[test]
    fn reserve_growth_reserves_index_and_auxiliary_bytes_together() {
        let budget = MemoryBudget::new(1_000_000);
        assert_eq!(budget.outstanding_reserved_bytes(), 0);

        let token = reserve_growth(&budget, 10).expect("予算内なので成功するはず");
        // 10エントリ * 24バイト(索引) + 10エントリ * 8バイト(行番号の並列配列)
        // = 240 + 80 = 320 バイト。
        assert_eq!(INDEX_BYTES_PER_ENTRY, 32);
        assert_eq!(budget.outstanding_reserved_bytes(), 320);

        let mut indexed = IndexedText::with_capacity(10);
        for i in 0..10u32 {
            indexed.push_entry(u64::from(i) * 10, 8, None, false, 0, u64::from(i) + 1);
        }
        token
            .mark_allocated(320)
            .expect("予約量以内の振り替えは成功するはず");
        drop(token);
        assert_eq!(
            budget.outstanding_reserved_bytes(),
            0,
            "実確保へ振り替えた後は予約が残らないはず"
        );
    }

    // 受け入れ条件: 事前確保した容量が capacity / spare_capacity
    // に反映され、その範囲の追記では容量が変わらない（＝再確保が起きない）。
    #[test]
    fn grow_capacity_to_sets_capacity_and_spare_capacity_for_appends() {
        let mut indexed = IndexedText::new();
        assert_eq!(indexed.capacity(), 0);
        assert_eq!(indexed.spare_capacity(), 0);

        let allocated = indexed.grow_capacity_to(100);

        assert_eq!(indexed.capacity(), 100);
        assert_eq!(indexed.spare_capacity(), 100);
        // 索引本体（24バイト）と行番号の並列配列（8バイト）の合計。
        assert_eq!(allocated, 100 * INDEX_BYTES_PER_ENTRY);

        for i in 0..100u32 {
            indexed.push_entry(u64::from(i) * 10, 8, None, false, 0, u64::from(i) + 1);
        }
        assert_eq!(indexed.len(), 100);
        assert_eq!(
            indexed.capacity(),
            100,
            "事前確保した容量に収まる追記では再確保が起きないはず"
        );
        assert_eq!(indexed.spare_capacity(), 0);
    }

    // 受け入れ条件: 既に十分な容量がある場合、grow_capacity_to は
    // 何も確保せず縮小もしない（読み終わりに余っていても shrink しない）。
    #[test]
    fn grow_capacity_to_is_a_no_op_when_capacity_already_suffices() {
        let mut indexed = IndexedText::new();
        indexed.grow_capacity_to(100);

        let allocated = indexed.grow_capacity_to(50);

        assert_eq!(allocated, 0);
        assert_eq!(indexed.capacity(), 100, "縮小しないはず");
    }

    // 受け入れ条件: 予算不足の場合は拒否され、索引を構築しない（呼び出し側が
    // reserve_growth の Err を見て構築を諦める、という使い方の確認）。
    #[test]
    fn reserve_growth_is_rejected_when_budget_is_insufficient() {
        let budget = MemoryBudget::new(100);
        let rejected = reserve_growth(&budget, 10).expect_err("予算を超えるので拒否されるはず");
        assert_eq!(rejected.budget_bytes, 100);
        assert_eq!(budget.outstanding_reserved_bytes(), 0);
    }
}
