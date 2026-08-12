//! クリップボードコピーの内容生成と上限判定（`COPY-001`〜`006`、`CFG-018`、
//! P10、`tasks/phase-10-clipboard-copy.md`、ADR-0009）。
//!
//! GUI 非依存です。`src-tauri` は [`assemble_copy`] の結果を Win32 の
//! クリップボード API へ渡すだけで、上限判定・整形ロジックそのものは
//! 持ちません（計画書「作業項目8: 層境界の確認」と同じ方針）。
//!
//! # 上限判定の順序（COPY-004／COPY-005、ADR-0009）
//!
//! 1. **行数を索引から即時算出します**（本文の読み出しを一切行いません）。
//!    選択の行数が `CFG-018` の `max_copy_lines` を**超える**場合、即座に
//!    拒否します（`selected_bytes` は `None` のまま。バイト数の算出自体を
//!    行わないため「非有界の作業」になりません）。
//! 2. 行数が上限内の場合だけ、本文をオンデマンド読み出しでストリーミングし
//!    ながら、**正準プレーンテキストの UTF-8 換算バイト数**を累積します。
//!    累積が `max_bytes` を**超えた時点で打ち切り**、拒否します
//!    （`selected_bytes` はその時点までの累積値）。
//! 3. **上限ちょうどは許可、超過だけを拒否します**（`COPY-005` は「超える
//!    場合」に拒否と定めているため、境界値は許可側）。
//!
//! 拒否時はクリップボード用バッファを一切生成・予約しません
//! （`COPY-005` の「部分コピーを黙って行わない」）。
//!
//! # 生成規則（ADR-0009）
//!
//! - **本文列のみの選択**（[`CopyColumns::is_body_only`]）: 原文
//!   （`raw_text`。継続行の内部改行を保持）をそのまま、項目間は `\n` で
//!   区切って連結します。
//! - **複数列の選択**（セル範囲）: 可逆な quoted TSV です。列の並びは
//!   「行番号 → 日時 → 本文」の固定順で、選択された列だけを `\t` で
//!   区切ります。タブ・改行・二重引用符を含むセルは二重引用符で囲み、
//!   セル内の二重引用符は重ねます（`""`）。**バックスラッシュによる
//!   `\t`／`\n` への置換は行いません**（元のバックスラッシュ列と区別できず
//!   可逆でないため。ログ本文にはパス等でバックスラッシュが頻出します）。
//!
//! # メモリ会計（`PERF-008`／`PERF-010`）
//!
//! バッファの確保は上限判定が完了した**後**に予約します（`reserve` →
//! 生成 → `mark_allocated`。拒否経路では一切予約しません）。上限判定の
//! ストリーミング中に保持する項目群（[`CopyRow`] の列）は上限（既定 16 MiB・
//! 10万行）の範囲内で有界であり、非有界の作業にはなりません。
//!
//! # 表示外の範囲を含む選択（計画書「リスクと未決事項」）
//!
//! 仮想スクロールは表示されていない行の内容を保持しません（`PERF-012`）。
//! [`assemble_copy`] は `start`／`count`（表示集合内のインデックス範囲）だけを
//! 受け取り、本文は毎回オンデマンドで読み出すため、全選択（Ctrl+A）や
//! 表示外を含む範囲選択でも同じ経路で扱えます。

use std::sync::Arc;

use crate::{DisplaySetRegistry, FetchRangeError, ItemDto, RangeRequest};

/// コピー対象の列の集合です（`COPY-001`。行番号／日時／本文）。
///
/// 列の並び順は常に「行番号 → 日時 → 本文」で固定します（TSV 生成時の
/// 列順、[`CopyRow::write_canonical`] 参照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CopyColumns {
    pub line_number: bool,
    pub timestamp: bool,
    pub raw_text: bool,
}

impl CopyColumns {
    /// 1列も選択されていないか。
    #[must_use]
    pub fn any_selected(&self) -> bool {
        self.line_number || self.timestamp || self.raw_text
    }

    /// 本文列のみの選択か（ADR-0009: 行（論理ログ項目）選択 = 原文そのまま）。
    #[must_use]
    pub fn is_body_only(&self) -> bool {
        self.raw_text && !self.line_number && !self.timestamp
    }

    fn selected_count(&self) -> u32 {
        u32::from(self.line_number) + u32::from(self.timestamp) + u32::from(self.raw_text)
    }
}

/// コピー対象の選択範囲です（`COPY-001`）。
///
/// `start`・`count` は表示集合内のインデックス（0起点）です。仮想スクロール
/// が表示していない範囲を含んでいても構いません（本文はこの関数が改めて
/// オンデマンドで読み出すため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopySelection {
    pub start: u64,
    pub count: u64,
    pub columns: CopyColumns,
}

/// `CFG-018` の上限値です（呼び出し側が MiB → バイトへ変換して渡します）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyLimits {
    pub max_bytes: u64,
    pub max_lines: u64,
}

/// 生成に成功したコピー内容です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyBuffer {
    /// 正準プレーンテキスト（CF_UNICODETEXT として書き込む内容そのもの）。
    pub text: String,
    /// `text` の UTF-8 バイト数。
    pub bytes: u64,
    /// 行数（論理ログ項目数、または TSV の行数）。
    pub lines: u64,
}

/// 上限超過により拒否したことを表します（`COPY-005`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyRejection {
    pub limit_bytes: u64,
    pub limit_lines: u64,
    /// 選択の行数（索引から即時算出した、常に判明している値）。
    pub selected_lines: u64,
    /// 判明している範囲のバイト数。行数超過で即拒否した場合は `None`
    /// （バイト数の算出自体を行っていないため）。バイト数超過で拒否した
    /// 場合は、打ち切り時点までの累積値。
    pub selected_bytes: Option<u64>,
}

/// [`assemble_copy`] の正常系の結果です（`Rejected` も異常ではなく、
/// `COPY-005` が定める正規の応答です）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied(CopyBuffer),
    Rejected(CopyRejection),
}

/// [`assemble_copy`] が失敗した理由です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyError {
    /// 未知の表示集合・世代不一致（既存のエラー経路の再利用。
    /// `crate::registry::DisplaySetRegistry::fetch_range` と同じ意味）。
    Fetch(FetchRangeError),
    /// 列が1つも選択されていない（UI 側では通常発生しない防御的エラー）。
    NoColumnsSelected,
    /// メモリ予約が拒否された（`PERF-008`）。`CFG-018` の上限内でも、他の
    /// 用途でメモリ予算が逼迫している場合に発生し得る。
    MemoryReservationRejected(hakutaku_memory_accounting::ReservationRejected),
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CopyError::Fetch(error) => error.fmt(f),
            CopyError::NoColumnsSelected => {
                write!(f, "コピーする列が1つも選択されていません。")
            }
            CopyError::MemoryReservationRejected(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CopyError {}

/// 選択範囲からクリップボードコピーの内容を組み立てます（`COPY-001`〜
/// `COPY-005`、`PERF-008`／`PERF-010`）。
///
/// `expected_generation` は呼び出し側が最後に観測した世代です。表示集合が
/// 再構築されていた場合、[`CopyError::Fetch`]（`GenerationMismatch`）を返し
/// ます（既存の範囲取得と同じ経路）。
///
/// 上限超過は `Err` ではなく `Ok(CopyOutcome::Rejected(..))` を返します
/// （`COPY-005` が定める、部分コピーなしの正規応答）。
pub fn assemble_copy(
    registry: &mut DisplaySetRegistry,
    display_set_id: u32,
    expected_generation: u64,
    selection: CopySelection,
    limits: CopyLimits,
    budget: &hakutaku_memory_accounting::MemoryBudget,
) -> Result<CopyOutcome, CopyError> {
    if !selection.columns.any_selected() {
        return Err(CopyError::NoColumnsSelected);
    }

    let source_id = registry
        .source_id_for_display_set(display_set_id)
        .ok_or(CopyError::Fetch(FetchRangeError::UnknownDisplaySet))?;
    let handle = registry
        .current_handle(source_id)
        .ok_or(CopyError::Fetch(FetchRangeError::UnknownDisplaySet))?;
    if handle.generation != expected_generation {
        return Err(CopyError::Fetch(FetchRangeError::GenerationMismatch {
            expected: expected_generation,
            current: handle.generation,
        }));
    }

    // 行数は索引（total_items）から即時算出できる。本文の読み出しは
    // まだ一切行わない（表示外の範囲を含む選択でも同じ経路で扱える。
    // モジュール doc コメント「表示外の範囲を含む選択」参照）。
    let start = selection.start.min(handle.total_items);
    let effective_lines = selection.count.min(handle.total_items - start);

    if effective_lines > limits.max_lines {
        return Ok(CopyOutcome::Rejected(CopyRejection {
            limit_bytes: limits.max_bytes,
            limit_lines: limits.max_lines,
            selected_lines: effective_lines,
            selected_bytes: None,
        }));
    }

    if effective_lines == 0 {
        return Ok(CopyOutcome::Copied(CopyBuffer {
            text: String::new(),
            bytes: 0,
            lines: 0,
        }));
    }

    let is_body_only = selection.columns.is_body_only();
    let mut rows: Vec<CopyRow> = Vec::new();
    let mut accumulated_bytes: u64 = 0;
    let mut cursor = start;
    let target_end = start + effective_lines;

    while cursor < target_end {
        let remaining = target_end - cursor;
        let max_items = u32::try_from(remaining).unwrap_or(u32::MAX);
        let response = registry
            .fetch_range(
                display_set_id,
                RangeRequest {
                    start: cursor,
                    max_items,
                    expected_generation,
                },
            )
            .map_err(CopyError::Fetch)?;

        if response.items.is_empty() {
            // 索引から算出した件数より実際の項目が少ない（通常発生しない
            // 防御的な打ち切り）。これ以上進めない。
            break;
        }

        let next_cursor = response.start + response.items.len() as u64;

        for item in response.items {
            let row = CopyRow::from_item(item);
            let row_bytes = row.canonical_len(selection.columns, is_body_only);
            let separator_bytes = u64::from(!rows.is_empty());
            accumulated_bytes = accumulated_bytes
                .saturating_add(separator_bytes)
                .saturating_add(row_bytes);
            rows.push(row);

            if accumulated_bytes > limits.max_bytes {
                // COPY-005: 打ち切って拒否する（非有界の作業を作らない）。
                return Ok(CopyOutcome::Rejected(CopyRejection {
                    limit_bytes: limits.max_bytes,
                    limit_lines: limits.max_lines,
                    selected_lines: effective_lines,
                    selected_bytes: Some(accumulated_bytes),
                }));
            }
        }

        cursor = next_cursor;
    }

    // PERF-010: 上限判定が完了した後に予約してから生成する。拒否経路は
    // 上の return でここへ到達しない。
    let total_bytes = usize::try_from(accumulated_bytes).unwrap_or(usize::MAX);
    let token = budget
        .reserve(total_bytes)
        .map_err(CopyError::MemoryReservationRejected)?;

    let mut text = String::with_capacity(total_bytes);
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        row.write_canonical(selection.columns, is_body_only, &mut text);
    }

    let actual_bytes = text.len();
    token.mark_allocated(actual_bytes).expect(
        "上限判定で積算したバイト数と生成したバッファの実バイト数は常に一致するはず\
         （CopyRow::canonical_len と CopyRow::write_canonical は同じ規則で計算している）",
    );

    Ok(CopyOutcome::Copied(CopyBuffer {
        text,
        bytes: actual_bytes as u64,
        lines: rows.len() as u64,
    }))
}

/// コピー1行分の中間表現です（列生成前）。
struct CopyRow {
    source_line_number: u64,
    timestamp_display: Option<String>,
    /// 本文は [`ItemDto::raw_text`] から共有ハンドルごと受け取ります。
    /// コピーは本文を読み出して連結するだけなので、ここで
    /// 所有権のために複製する必要がありません。
    raw_text: Arc<str>,
}

impl CopyRow {
    fn from_item(item: ItemDto) -> Self {
        CopyRow {
            source_line_number: item.source_line_number,
            timestamp_display: item.timestamp_display,
            raw_text: item.raw_text,
        }
    }

    /// この行を正準表現へ変換した場合のバイト数を、実際には生成せずに
    /// 計算します（COPY-004 の判定を、上限に達するまで安価に行うための
    /// 長さ計算専用の経路。[`Self::write_canonical`] と必ず同じ規則を
    /// 保ちます）。
    fn canonical_len(&self, columns: CopyColumns, is_body_only: bool) -> u64 {
        if is_body_only {
            return self.raw_text.len() as u64;
        }
        let mut total = 0u64;
        if columns.line_number {
            total += quoted_cell_len(&self.source_line_number.to_string()) as u64;
        }
        if columns.timestamp {
            total += quoted_cell_len(self.timestamp_display.as_deref().unwrap_or("")) as u64;
        }
        if columns.raw_text {
            total += quoted_cell_len(&self.raw_text) as u64;
        }
        total + u64::from(columns.selected_count().saturating_sub(1))
    }

    /// [`Self::canonical_len`] と同じ規則で実際の文字列を生成し、`out` へ
    /// 追記します。
    fn write_canonical(&self, columns: CopyColumns, is_body_only: bool, out: &mut String) {
        if is_body_only {
            out.push_str(&self.raw_text);
            return;
        }
        let mut wrote_any = false;
        if columns.line_number {
            push_quoted_cell(out, &self.source_line_number.to_string());
            wrote_any = true;
        }
        if columns.timestamp {
            if wrote_any {
                out.push('\t');
            }
            push_quoted_cell(out, self.timestamp_display.as_deref().unwrap_or(""));
            wrote_any = true;
        }
        if columns.raw_text {
            if wrote_any {
                out.push('\t');
            }
            push_quoted_cell(out, &self.raw_text);
        }
    }
}

/// セルがタブ・改行（LF／CR）・二重引用符のいずれかを含み、quoted TSV の
/// 引用囲みが必要か（ADR-0009）。
fn cell_needs_quoting(cell: &str) -> bool {
    cell.contains(['\t', '\n', '\r', '"'])
}

/// [`push_quoted_cell`] が生成する文字列のバイト数を、実際には生成せずに
/// 計算します。
fn quoted_cell_len(cell: &str) -> usize {
    if cell_needs_quoting(cell) {
        let quote_count = cell.bytes().filter(|&b| b == b'"').count();
        // 前後の引用符2バイト + 内部の二重引用符を1つずつ重ねる分。
        cell.len() + 2 + quote_count
    } else {
        cell.len()
    }
}

/// セルを quoted TSV のセルとして `out` へ書き込みます。引用が必要な場合は
/// 二重引用符で囲み、セル内の二重引用符を重ねます（ADR-0009）。
/// **バックスラッシュには一切触れません**（`\t`／`\n` への置換をしない設計
/// 判断。モジュール doc コメント参照）。
fn push_quoted_cell(out: &mut String, cell: &str) {
    if !cell_needs_quoting(cell) {
        out.push_str(cell);
        return;
    }
    out.push('"');
    for ch in cell.chars() {
        if ch == '"' {
            out.push('"');
            out.push('"');
        } else {
            out.push(ch);
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::SourceBudget;
    use crate::item::PendingItem;
    use hakutaku_format_detection::SelectedEncoding;
    use hakutaku_memory_accounting::MemoryBudget;

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
                "hakutaku-core-services-copy-test-{label}-{}-{count}-{nanos}.log",
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

    /// `lines` を `\n` 区切りの単独物理行として並べ、それぞれ1件の
    /// `PendingItem`（継続行なし）にしたテスト用ソースを登録します。
    fn insert_simple_lines(
        registry: &mut DisplaySetRegistry,
        budget: &SourceBudget,
        file: &TempFile,
        label: &str,
        lines: &[&str],
    ) -> crate::DisplaySetHandle {
        let mut content = Vec::new();
        let mut pending = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let raw_offset = content.len() as u64;
            content.extend_from_slice(line.as_bytes());
            pending.push(PendingItem {
                raw_offset,
                raw_byte_len: u32::try_from(line.len()).unwrap(),
                comparison_key_millis: None,
                source_line_number: index as u64 + 1,
                continuation_count: 0,
                unconfirmed: false,
            });
            content.push(b'\n');
        }
        std::fs::write(&file.path, &content).expect("内容を書き込めるはず");

        let (opened, snapshot) =
            hakutaku_data_source::open_and_snapshot(&file.path).expect("開けるはず");
        drop(opened);
        let reservation = budget
            .reserve(snapshot.snapshot_end)
            .expect("テストの上限は十分大きいはず");
        registry
            .insert_source(
                file.path.clone(),
                label.to_string(),
                &pending,
                snapshot,
                reservation,
                false,
                None,
                SelectedEncoding::Utf8,
                crate::item::CapacityEstimate::Exact(pending.len()),
            )
            .expect("索引予約は十分な予算内のはず")
    }

    /// 継続行（内部改行）を持つ単一項目1件だけのソースを登録します。
    /// `physical_lines` は継続行結合前の物理行の並びで、項目全体の
    /// `raw_byte_len` はそれらを `\n` で結合した長さになります。
    fn insert_continuation_item(
        registry: &mut DisplaySetRegistry,
        budget: &SourceBudget,
        file: &TempFile,
        label: &str,
        physical_lines: &[&str],
    ) -> crate::DisplaySetHandle {
        let joined = physical_lines.join("\n");
        let content = format!("{joined}\n");
        std::fs::write(&file.path, content.as_bytes()).expect("内容を書き込めるはず");

        let pending = vec![PendingItem {
            raw_offset: 0,
            raw_byte_len: u32::try_from(joined.len()).unwrap(),
            comparison_key_millis: None,
            source_line_number: 1,
            continuation_count: u16::try_from(physical_lines.len() - 1).unwrap(),
            unconfirmed: false,
        }];

        let (opened, snapshot) =
            hakutaku_data_source::open_and_snapshot(&file.path).expect("開けるはず");
        drop(opened);
        let reservation = budget
            .reserve(snapshot.snapshot_end)
            .expect("テストの上限は十分大きいはず");
        registry
            .insert_source(
                file.path.clone(),
                label.to_string(),
                &pending,
                snapshot,
                reservation,
                false,
                None,
                SelectedEncoding::Utf8,
                crate::item::CapacityEstimate::Exact(pending.len()),
            )
            .expect("索引予約は十分な予算内のはず")
    }

    fn body_only_columns() -> CopyColumns {
        CopyColumns {
            line_number: false,
            timestamp: false,
            raw_text: true,
        }
    }

    fn generous_limits() -> CopyLimits {
        CopyLimits {
            max_bytes: 1_000_000,
            max_lines: 1_000_000,
        }
    }

    // 受け入れ条件: 行（論理ログ項目）選択では原文をそのまま、項目間は \n で
    // 連結する（ADR-0009）。継続行の内部改行も保持する（LOG-024）。
    #[test]
    fn row_selection_joins_raw_text_with_newline_and_preserves_internal_newlines() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("row-selection", b"placeholder");

        let handle = insert_continuation_item(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &["first", "second"],
        );

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 1,
                columns: body_only_columns(),
            },
            generous_limits(),
            &memory_budget,
        )
        .expect("成功するはず");

        match outcome {
            CopyOutcome::Copied(buffer) => {
                assert_eq!(buffer.text, "first\nsecond");
                assert_eq!(buffer.lines, 1);
                assert_eq!(buffer.bytes, "first\nsecond".len() as u64);
            }
            other => panic!("Copied を期待したが {other:?} だった"),
        }
    }

    // 受け入れ条件: 複数行の row 選択は項目間が \n 区切りになる。
    #[test]
    fn row_selection_of_multiple_items_separates_with_newline() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("row-multi", b"placeholder");

        let handle = insert_simple_lines(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &["alpha", "beta", "gamma"],
        );

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 3,
                columns: body_only_columns(),
            },
            generous_limits(),
            &memory_budget,
        )
        .expect("成功するはず");

        match outcome {
            CopyOutcome::Copied(buffer) => {
                assert_eq!(buffer.text, "alpha\nbeta\ngamma");
                assert_eq!(buffer.lines, 3);
            }
            other => panic!("Copied を期待したが {other:?} だった"),
        }
    }

    // 受け入れ条件: セル範囲（複数列）は quoted TSV になり、タブ・改行・
    // 二重引用符を含むセルは引用囲みされ、往復変換できる。バックスラッシュは
    // 置換されない。
    #[test]
    fn cell_range_selection_produces_reversible_quoted_tsv_without_backslash_replacement() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("cell-range", b"placeholder");

        // 生の raw_text には \t・\n・" を直接埋め込めない（改行は継続行結合
        // でしか作れない）ため、\t と " を含む単独物理行で検証する（改行を
        // 含むケースは継続行を使った別テストで確認する）。
        let raw_with_special_chars = "a\\path\tb\"c";
        let handle = insert_simple_lines(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &[raw_with_special_chars],
        );

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 1,
                columns: CopyColumns {
                    line_number: true,
                    timestamp: false,
                    raw_text: true,
                },
            },
            generous_limits(),
            &memory_budget,
        )
        .expect("成功するはず");

        let text = match outcome {
            CopyOutcome::Copied(buffer) => buffer.text,
            other => panic!("Copied を期待したが {other:?} だった"),
        };

        let cells = parse_single_row_quoted_tsv(&text);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0], "1", "行番号セルは引用不要のはず");
        assert_eq!(
            cells[1], raw_with_special_chars,
            "タブ・二重引用符・バックスラッシュを含むセルが可逆に復元されるはず"
        );
        assert!(
            !cells[1].contains("\\t") && !cells[1].contains("\\n"),
            "バックスラッシュによる \\t/\\n への置換が行われていないはず"
        );
    }

    // 受け入れ条件: セル内の改行（継続行の内部改行）も quoted TSV で往復できる。
    #[test]
    fn cell_range_selection_preserves_internal_newline_in_quoted_cell() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("cell-range-newline", b"placeholder");

        let handle = insert_continuation_item(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &["line1", "line2"],
        );

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 1,
                columns: CopyColumns {
                    line_number: true,
                    timestamp: false,
                    raw_text: true,
                },
            },
            generous_limits(),
            &memory_budget,
        )
        .expect("成功するはず");

        let text = match outcome {
            CopyOutcome::Copied(buffer) => buffer.text,
            other => panic!("Copied を期待したが {other:?} だった"),
        };

        let cells = parse_single_row_quoted_tsv(&text);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0], "1");
        assert_eq!(cells[1], "line1\nline2", "セル内改行が可逆に復元されるはず");
    }

    /// 単一行（改行を含み得る quoted セルを含む）の quoted TSV をセルへ分解する
    /// テスト専用のパーサーです。複数行にまたがる TSV 全体の解釈は行わず、
    /// 単一項目のみを検証する本テストの範囲に限定しています。
    fn parse_single_row_quoted_tsv(row: &str) -> Vec<String> {
        let mut cells = Vec::new();
        let mut chars = row.chars().peekable();
        loop {
            let mut cell = String::new();
            if chars.peek() == Some(&'"') {
                chars.next();
                loop {
                    match chars.next() {
                        Some('"') => {
                            if chars.peek() == Some(&'"') {
                                chars.next();
                                cell.push('"');
                            } else {
                                break;
                            }
                        }
                        Some(other) => cell.push(other),
                        None => break,
                    }
                }
            } else {
                while let Some(&next) = chars.peek() {
                    if next == '\t' {
                        break;
                    }
                    cell.push(next);
                    chars.next();
                }
            }
            cells.push(cell);
            match chars.next() {
                Some('\t') => continue,
                _ => break,
            }
        }
        cells
    }

    // 受け入れ条件（境界値）: 行数がちょうど上限なら許可される。
    #[test]
    fn line_count_exactly_at_limit_is_copied() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("line-boundary-ok", b"placeholder");

        let handle = insert_simple_lines(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &["l1", "l2", "l3", "l4"],
        );

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 3,
                columns: body_only_columns(),
            },
            CopyLimits {
                max_bytes: 1_000_000,
                max_lines: 3,
            },
            &memory_budget,
        )
        .expect("成功するはず");

        match outcome {
            CopyOutcome::Copied(buffer) => assert_eq!(buffer.lines, 3),
            other => panic!("行数ちょうどは許可されるはず: {other:?}"),
        }
    }

    // 受け入れ条件（境界値）: 行数が上限を1行超えると拒否され、
    // selected_bytes は算出されない（非有界の作業を避けるため）。
    #[test]
    fn line_count_one_over_limit_is_rejected_without_computing_bytes() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("line-boundary-over", b"placeholder");

        let handle = insert_simple_lines(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &["l1", "l2", "l3", "l4"],
        );

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 4,
                columns: body_only_columns(),
            },
            CopyLimits {
                max_bytes: 1_000_000,
                max_lines: 3,
            },
            &memory_budget,
        )
        .expect("成功するはず（拒否も正常応答）");

        match outcome {
            CopyOutcome::Rejected(rejection) => {
                assert_eq!(rejection.limit_lines, 3);
                assert_eq!(rejection.selected_lines, 4);
                assert_eq!(rejection.selected_bytes, None);
            }
            other => panic!("Rejected を期待したが {other:?} だった"),
        }
        assert_eq!(
            memory_budget.outstanding_reserved_bytes(),
            0,
            "拒否経路では予約が行われないはず（PERF-010）"
        );
    }

    // 受け入れ条件（境界値）: バイト数がちょうど上限なら許可される。
    #[test]
    fn byte_count_exactly_at_limit_is_copied() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("byte-boundary-ok", b"placeholder");

        let content = "x".repeat(10);
        let handle =
            insert_simple_lines(&mut registry, &source_budget, &file, "a.log", &[&content]);

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 1,
                columns: body_only_columns(),
            },
            CopyLimits {
                max_bytes: 10,
                max_lines: 1_000_000,
            },
            &memory_budget,
        )
        .expect("成功するはず");

        match outcome {
            CopyOutcome::Copied(buffer) => {
                assert_eq!(buffer.bytes, 10);
                assert_eq!(buffer.text, content);
            }
            other => panic!("バイト数ちょうどは許可されるはず: {other:?}"),
        }
        assert_eq!(
            memory_budget.outstanding_reserved_bytes(),
            0,
            "mark_allocated 後は予約が実確保へ振り替わり残らないはず"
        );
    }

    // 受け入れ条件（境界値）: バイト数が上限を1バイト超えると拒否され、
    // クリップボード用バッファは生成・予約されない。
    #[test]
    fn byte_count_one_over_limit_is_rejected_and_nothing_is_reserved() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("byte-boundary-over", b"placeholder");

        let content = "x".repeat(11);
        let handle =
            insert_simple_lines(&mut registry, &source_budget, &file, "a.log", &[&content]);

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 1,
                columns: body_only_columns(),
            },
            CopyLimits {
                max_bytes: 10,
                max_lines: 1_000_000,
            },
            &memory_budget,
        )
        .expect("成功するはず（拒否も正常応答）");

        match outcome {
            CopyOutcome::Rejected(rejection) => {
                assert_eq!(rejection.limit_bytes, 10);
                assert_eq!(rejection.selected_lines, 1);
                assert_eq!(rejection.selected_bytes, Some(11));
            }
            other => panic!("Rejected を期待したが {other:?} だった"),
        }
        assert_eq!(
            memory_budget.outstanding_reserved_bytes(),
            0,
            "拒否経路では予約が行われないはず（PERF-010、部分コピーなし）"
        );
    }

    // 受け入れ条件（PERF-008／PERF-010）: 上限内のコピーで、生成したバイト数
    // ぶんの予約が会計へ計上され、mark_allocated 後は予約が残らない。
    #[test]
    fn successful_copy_reserves_and_settles_the_generated_byte_count() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("reservation", b"placeholder");

        let handle = insert_simple_lines(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &["abc", "de"],
        );

        assert_eq!(memory_budget.outstanding_reserved_bytes(), 0);

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 2,
                columns: body_only_columns(),
            },
            generous_limits(),
            &memory_budget,
        )
        .expect("成功するはず");

        match outcome {
            CopyOutcome::Copied(buffer) => assert_eq!(buffer.bytes, "abc\nde".len() as u64),
            other => panic!("Copied を期待したが {other:?} だった"),
        }
        assert_eq!(
            memory_budget.outstanding_reserved_bytes(),
            0,
            "生成完了後は mark_allocated により予約が実確保へ振り替わるはず"
        );
    }

    // 受け入れ条件（CFG-018）: 上限値を変えると判定結果が変わる（設定値変更の
    // 反映）。
    #[test]
    fn changing_limits_changes_the_judgment() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("limits-change", b"placeholder");

        let handle = insert_simple_lines(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &["l1", "l2", "l3"],
        );

        let selection = CopySelection {
            start: 0,
            count: 3,
            columns: body_only_columns(),
        };

        let rejected = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            selection,
            CopyLimits {
                max_bytes: 1_000_000,
                max_lines: 2,
            },
            &memory_budget,
        )
        .expect("成功するはず");
        assert!(matches!(rejected, CopyOutcome::Rejected(_)));

        let copied = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            selection,
            CopyLimits {
                max_bytes: 1_000_000,
                max_lines: 3,
            },
            &memory_budget,
        )
        .expect("成功するはず");
        assert!(matches!(copied, CopyOutcome::Copied(_)));
    }

    // 受け入れ条件（COPY-004）: 上限判定は正準プレーンテキストの UTF-8 換算で
    // 行う。ANSI（Windows-1252）ソースでは、ディスク上のバイト数と UTF-8
    // 換算後のバイト数が異なることを確認する（"é" は cp1252 で1バイト、
    // UTF-8 で2バイト）。
    #[test]
    fn byte_accounting_uses_utf8_length_not_raw_source_bytes_for_ansi_source() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("ansi-utf8", b"placeholder");

        // "café" の cp1252 表現（é = 0xE9、1バイト）。UTF-8 では "café" は
        // 5バイト（c,a,f が1バイトずつ + é が2バイト）。
        let raw_bytes: &[u8] = b"caf\xE9";
        std::fs::write(&file.path, raw_bytes).expect("書き込めるはず");

        let pending = vec![PendingItem {
            raw_offset: 0,
            raw_byte_len: u32::try_from(raw_bytes.len()).unwrap(),
            comparison_key_millis: None,
            source_line_number: 1,
            continuation_count: 0,
            unconfirmed: false,
        }];
        let (opened, snapshot) =
            hakutaku_data_source::open_and_snapshot(&file.path).expect("開けるはず");
        drop(opened);
        let reservation = source_budget
            .reserve(snapshot.snapshot_end)
            .expect("十分な予算内のはず");
        let handle = registry
            .insert_source(
                file.path.clone(),
                "ansi.log".to_string(),
                &pending,
                snapshot,
                reservation,
                false,
                None,
                SelectedEncoding::Windows(1252),
                crate::item::CapacityEstimate::Exact(pending.len()),
            )
            .expect("索引予約は成功するはず");

        // UTF-8 換算のちょうど5バイトは許可される（ディスク上は4バイトだが
        // UTF-8 換算では5バイトのため、max_bytes=4 のままでは本来拒否される
        // べき境界を、誤ってディスク上のバイト数で判定していないかを
        // このテストの後半で確認する）。
        let copied = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 1,
                columns: body_only_columns(),
            },
            CopyLimits {
                max_bytes: 5,
                max_lines: 1_000_000,
            },
            &memory_budget,
        )
        .expect("成功するはず");
        match copied {
            CopyOutcome::Copied(buffer) => {
                assert_eq!(buffer.text, "café");
                assert_eq!(buffer.bytes, 5, "UTF-8 換算で5バイトのはず");
            }
            other => panic!("Copied を期待したが {other:?} だった"),
        }

        // ディスク上のバイト数（4）を上限にすると、UTF-8 換算（5）が上限を
        // 超えるため拒否されるはず（誤ってディスク上のバイト数で判定して
        // いれば、この境界は誤って許可されてしまう）。
        let rejected = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 1,
                columns: body_only_columns(),
            },
            CopyLimits {
                max_bytes: 4,
                max_lines: 1_000_000,
            },
            &memory_budget,
        )
        .expect("成功するはず（拒否も正常応答）");
        assert!(
            matches!(rejected, CopyOutcome::Rejected(_)),
            "UTF-8 換算バイト数（5）が上限（4）を超えるので拒否されるはず"
        );
    }

    // 受け入れ条件: 列が1つも選択されていない場合は防御的エラーになる。
    #[test]
    fn no_columns_selected_is_an_error() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("no-columns", b"placeholder");

        let handle = insert_simple_lines(&mut registry, &source_budget, &file, "a.log", &["l1"]);

        let error = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 1,
                columns: CopyColumns::default(),
            },
            generous_limits(),
            &memory_budget,
        )
        .expect_err("列なしはエラーになるはず");
        assert_eq!(error, CopyError::NoColumnsSelected);
    }

    // 受け入れ条件: 未知の表示集合は既存のエラー経路（FetchRangeError）を
    // そのまま使う。
    #[test]
    fn unknown_display_set_reuses_the_existing_fetch_error() {
        let mut registry = DisplaySetRegistry::new();
        let memory_budget = MemoryBudget::new(10_000_000);

        let error = assemble_copy(
            &mut registry,
            999,
            1,
            CopySelection {
                start: 0,
                count: 1,
                columns: body_only_columns(),
            },
            generous_limits(),
            &memory_budget,
        )
        .expect_err("未登録のIDはエラーになるはず");
        assert_eq!(error, CopyError::Fetch(FetchRangeError::UnknownDisplaySet));
    }

    // 受け入れ条件: 世代不一致は既存のエラー経路（FetchRangeError）をそのまま
    // 使う。
    #[test]
    fn generation_mismatch_reuses_the_existing_fetch_error() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("gen-mismatch", b"placeholder");

        let handle = insert_simple_lines(&mut registry, &source_budget, &file, "a.log", &["l1"]);

        let error = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation + 1,
            CopySelection {
                start: 0,
                count: 1,
                columns: body_only_columns(),
            },
            generous_limits(),
            &memory_budget,
        )
        .expect_err("世代不一致はエラーになるはず");
        assert_eq!(
            error,
            CopyError::Fetch(FetchRangeError::GenerationMismatch {
                expected: handle.generation + 1,
                current: handle.generation,
            })
        );
    }

    // 受け入れ条件: 空選択（count=0）は拒否ではなく、0バイト・0行の Copied
    // として扱う（クリップボードを変更する呼び出し側の判断はこの関数の外）。
    #[test]
    fn empty_selection_yields_an_empty_copied_buffer() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("empty-selection", b"placeholder");

        let handle = insert_simple_lines(&mut registry, &source_budget, &file, "a.log", &["l1"]);

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 0,
                columns: body_only_columns(),
            },
            generous_limits(),
            &memory_budget,
        )
        .expect("成功するはず");
        match outcome {
            CopyOutcome::Copied(buffer) => {
                assert_eq!(buffer.bytes, 0);
                assert_eq!(buffer.lines, 0);
                assert_eq!(buffer.text, "");
            }
            other => panic!("Copied（空）を期待したが {other:?} だった"),
        }
    }

    // 受け入れ条件: 選択が総項目数を超えて延びていても（表示外を含む選択）、
    // 総項目数で自動的にクランプされる。
    #[test]
    fn selection_beyond_total_items_is_clamped() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("clamp", b"placeholder");

        let handle =
            insert_simple_lines(&mut registry, &source_budget, &file, "a.log", &["l1", "l2"]);

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            CopySelection {
                start: 0,
                count: 100,
                columns: body_only_columns(),
            },
            generous_limits(),
            &memory_budget,
        )
        .expect("成功するはず");
        match outcome {
            CopyOutcome::Copied(buffer) => assert_eq!(buffer.lines, 2),
            other => panic!("Copied を期待したが {other:?} だった"),
        }
    }
}
