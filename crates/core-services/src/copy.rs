//! クリップボードコピーの内容生成と上限判定（`COPY-001`〜`006`、`CFG-018`、
//! P10、ADR-0011）。
//!
//! GUI 非依存です。`src-tauri` は [`assemble_copy`] の結果を Win32 の
//! クリップボード API へ渡すだけで、上限判定・整形ロジックそのものは
//! 持ちません（計画書「作業項目8: 層境界の確認」と同じ方針）。
//!
//! # 生成規則（ADR-0011、Issue #85）
//!
//! **コピー内容は常に選択行の原文（`raw_text`）そのままです。** 選択された
//! 範囲を `start` 昇順にたどり、項目の原文を `\n` で区切って連結します
//! （末尾に改行は付けません）。継続行を含む項目の内部改行はそのまま保ちます
//! （`LOG-014`／`LOG-024`）。
//!
//! 列の選択（行番号・日時・本文の組）と quoted TSV は Issue #85 で廃止しま
//! した。ADR-0009 が定めていた形式のうち、行（論理ログ項目）選択の規則だけを
//! そのまま引き継いでいます（連結規則そのものは変わっていません）。
//!
//! # 選択範囲の受け入れ条件（Issue #85）
//!
//! [`CopySelection::ranges`] は次をすべて満たす必要があります。満たさない
//! 場合は [`CopyError::InvalidSelection`] で拒否し、クリップボードには一切
//! 触れません（フロントエンドの `src/selection.js` が同じ形へ正規化して
//! 送りますが、IPC 境界の防御としてここでも検証します）。
//!
//! 1. 1つ以上の範囲があること（空の選択でクリップボードを空にしない。
//!    `COPY-006`）
//! 2. 各範囲の `count` が1以上であること
//! 3. `start` の昇順に並び、互いに重ならないこと（隣接は許容します。連結
//!    結果が1つの範囲にまとめた場合と同一になるため）
//! 4. すべての範囲が表示集合の項目数の内側にあること
//!
//! # 上限判定の順序（COPY-004／COPY-005）
//!
//! 上限は**選択したすべての範囲の合計**に対して判定します（範囲ごとではあり
//! ません）。
//!
//! 1. **行数を索引から即時算出します**（本文の読み出しを一切行いません）。
//!    選択の合計行数が `CFG-018` の `max_copy_lines` を**超える**場合、即座に
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
//! # 本文を読み出せなかった場合（`COPY-005`、Issue #37）
//!
//! 範囲取得は、コピーの最中にソースが削除・置換された場合も応答自体は
//! 成功させ、そのソースの項目を**空の本文**で返します（`ERR-001`。
//! `crate::registry` のモジュール doc コメント参照）。表示ならば次の取得で
//! 追いつけますが、コピーでは中身の抜けた内容がそのままクリップボードへ渡り、
//! 利用者は「コピーできた」と受け取ります。そこで [`assemble_copy`] は
//! [`DisplaySetRegistry::hydrate_fallback_items`] を範囲取得の前後で比べ、
//! 既定値で返された項目が1件でもあれば [`CopyError::SourceUnavailable`] として
//! コピー全体を失敗させます（クリップボードには一切触れません）。
//!
//! # 統合表示集合（P09-1）のコピー
//!
//! 表示集合の世代・件数の解決には
//! [`DisplaySetRegistry::display_set_state`] を使い、範囲取得には
//! [`DisplaySetRegistry::fetch_range`] を使います。どちらも単独ソースと統合
//! 表示集合を同じ入口で扱うため、この関数に統合表示集合の分岐はありません
//! （以前は `source_id` の逆引きで表示集合を解決しており、`source_id` を持たない
//! 統合表示集合では必ず失敗していました。Issue #37）。
//!
//! 統合表示の画面にだけ現れる読み込み元ラベル列（`LOG-007`）はコピーへ含め
//! ません（コピーは原文そのままのため、そもそも列という概念がありません）。
//!
//! # メモリ会計（`PERF-008`／`PERF-010`）
//!
//! バッファの確保は上限判定が完了した**後**に予約します（`reserve` →
//! 生成 → `mark_allocated`。拒否経路では一切予約しません）。上限判定の
//! ストリーミング中に保持するのは項目ごとの本文の共有ハンドル
//! （`Arc<str>`）だけで、上限（既定 16 MiB・10万行）の範囲内で有界であり、
//! 非有界の作業にはなりません。
//!
//! # 表示外の範囲を含む選択（計画書「リスクと未決事項」）
//!
//! 仮想スクロールは表示されていない行の内容を保持しません（`PERF-012`）。
//! [`assemble_copy`] は表示集合内のインデックス範囲だけを受け取り、本文は
//! 毎回オンデマンドで読み出すため、全選択（Ctrl+A）や表示外を含む範囲選択
//! でも同じ経路で扱えます。

use std::sync::Arc;

use crate::{DisplaySetRegistry, FetchRangeError, RangeRequest};

/// コピー対象の連続範囲です（`COPY-001`）。
///
/// `start`・`count` は表示集合内のインデックス（0起点、`start` を含む半開
/// 区間の長さが `count`）です。仮想スクロールが表示していない範囲を含んで
/// いても構いません（本文は [`assemble_copy`] が改めてオンデマンドで読み
/// 出すため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyRange {
    pub start: u64,
    pub count: u64,
}

/// コピー対象の選択です（`COPY-001`、Issue #85）。
///
/// 飛び飛びの選択（Ctrl+クリック）を表せるよう、互いに素な範囲の集合として
/// 受け取ります。満たすべき条件はモジュール doc コメント「選択範囲の受け入れ
/// 条件」を参照してください。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopySelection {
    pub ranges: Vec<CopyRange>,
}

/// 選択範囲が受け入れ条件を満たさなかった理由です（Issue #85）。
///
/// 呼び出し側（`src-tauri/src/clipboard.rs`）はこれをそのまま利用者向けの
/// 通知へ添えます。通常の UI 操作では発生せず、発生した場合は
/// フロントエンドの正規化（`src/selection.js` の `toCopyRanges`）と IPC の
/// 受け渡しのどちらかが壊れていることを意味します。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidSelectionReason {
    /// 範囲が1つも無い（`COPY-006`: 選択が無いときはクリップボードへ触れない
    /// ため、空の内容で上書きせずに拒否します）。
    NoRanges,
    /// `count` が0の範囲が含まれている。
    EmptyRange,
    /// `start` の昇順に並んでいない、または範囲同士が重なっている。
    OverlappingOrUnordered,
    /// 表示集合の項目数の外側を指す範囲が含まれている。
    OutOfBounds,
}

impl std::fmt::Display for InvalidSelectionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvalidSelectionReason::NoRanges => write!(f, "選択範囲が1つも指定されていません"),
            InvalidSelectionReason::EmptyRange => write!(f, "行数が0の選択範囲が含まれています"),
            InvalidSelectionReason::OverlappingOrUnordered => {
                write!(f, "選択範囲が昇順に並んでいないか、重なっています")
            }
            InvalidSelectionReason::OutOfBounds => {
                write!(f, "選択範囲が表示中のログの範囲を超えています")
            }
        }
    }
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
    /// 行数（論理ログ項目数。選択したすべての範囲の合計）。継続行を含む項目は
    /// 内部に改行を持ちますが、行数としては1件です（`LOG-014`）。
    pub lines: u64,
}

/// 上限超過により拒否したことを表します（`COPY-005`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyRejection {
    pub limit_bytes: u64,
    pub limit_lines: u64,
    /// 選択の合計行数（索引から即時算出した、常に判明している値）。
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
    /// 選択範囲が受け入れ条件を満たさない（UI 側では通常発生しない防御的
    /// エラー。モジュール doc コメント「選択範囲の受け入れ条件」参照）。
    ///
    /// 呼び出し側はクリップボードを変更してはいけません。
    InvalidSelection(InvalidSelectionReason),
    /// メモリ予約が拒否された（`PERF-008`）。`CFG-018` の上限内でも、他の
    /// 用途でメモリ予算が逼迫している場合に発生し得る。
    MemoryReservationRejected(hakutaku_memory_accounting::ReservationRejected),
    /// 選択範囲の本文を読み出せなかった（コピーの最中にソースが削除・置換・
    /// 共有違反になった等。`LOG-023`／`LOG-027`）。
    ///
    /// 呼び出し側はクリップボードを変更してはいけません。中身の抜けた内容を
    /// 「コピーできた」として渡さないための失敗であり（`COPY-005`）、利用者へは
    /// 対象の状態を確かめて再試行するよう案内します。
    SourceUnavailable,
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CopyError::Fetch(error) => error.fmt(f),
            CopyError::InvalidSelection(reason) => {
                write!(f, "選択範囲が正しくありません（{reason}）。")
            }
            CopyError::MemoryReservationRejected(error) => error.fmt(f),
            CopyError::SourceUnavailable => {
                write!(
                    f,
                    "選択範囲の本文を読み出せなかったため、コピーを中止しました。"
                )
            }
        }
    }
}

impl std::error::Error for CopyError {}

/// 選択範囲からクリップボードコピーの内容を組み立てます（`COPY-001`〜
/// `COPY-005`、`PERF-008`／`PERF-010`、ADR-0011）。
///
/// `display_set_id` は単独ソースの表示集合でも統合表示集合（P09-1）でも
/// 構いません（モジュール doc コメント「統合表示集合のコピー」参照）。
///
/// `expected_generation` は呼び出し側が最後に観測した世代です。表示集合が
/// 再構築されていた場合、[`CopyError::Fetch`]（`GenerationMismatch`）を返し
/// ます（既存の範囲取得と同じ経路）。
///
/// `selection` の受け入れ条件はモジュール doc コメント「選択範囲の受け入れ
/// 条件」のとおりで、満たさない場合は [`CopyError::InvalidSelection`] を
/// 返します。
///
/// 上限超過は `Err` ではなく `Ok(CopyOutcome::Rejected(..))` を返します
/// （`COPY-005` が定める、部分コピーなしの正規応答）。本文を読み出せなかった
/// 場合は [`CopyError::SourceUnavailable`] で全体を失敗させます（同じく
/// `COPY-005`。モジュール doc コメント参照）。
pub fn assemble_copy(
    registry: &mut DisplaySetRegistry,
    display_set_id: u32,
    expected_generation: u64,
    selection: &CopySelection,
    limits: CopyLimits,
    budget: &hakutaku_memory_accounting::MemoryBudget,
) -> Result<CopyOutcome, CopyError> {
    // 表示集合の解決は、範囲取得（`fetch_range`）と同じ入口を使う。単独ソース
    // から `source_id` を逆引きする方法では、`source_id` を持たない統合表示集合
    // （P09-1）が常に「未知の表示集合」になってしまう（Issue #37）。
    let state = registry
        .display_set_state(display_set_id)
        .ok_or(CopyError::Fetch(FetchRangeError::UnknownDisplaySet))?;
    if state.generation != expected_generation {
        return Err(CopyError::Fetch(FetchRangeError::GenerationMismatch {
            expected: expected_generation,
            current: state.generation,
        }));
    }

    // 表示集合を解決した後に検証するのは、範囲の妥当性が総項目数に依存する
    // ため（`OutOfBounds`）。未知の表示集合・世代不一致は選択の内容に関わらず
    // 先に返す（利用者への案内が「選び直す」ではなく「開き直す」になる）。
    let total_lines =
        validate_selection(selection, state.total_items).map_err(CopyError::InvalidSelection)?;

    // 行数は索引（total_items）から即時算出できる。本文の読み出しは
    // まだ一切行わない（表示外の範囲を含む選択でも同じ経路で扱える。
    // モジュール doc コメント「表示外の範囲を含む選択」参照）。
    if total_lines > limits.max_lines {
        return Ok(CopyOutcome::Rejected(CopyRejection {
            limit_bytes: limits.max_bytes,
            limit_lines: limits.max_lines,
            selected_lines: total_lines,
            selected_bytes: None,
        }));
    }

    // 本文は共有ハンドルのまま集める（コピーは読み出して連結するだけなので、
    // 所有権のために複製する必要がない）。
    let mut bodies: Vec<Arc<str>> = Vec::new();
    let mut accumulated_bytes: u64 = 0;
    // COPY-005: 範囲取得が本文を空の既定値で埋めた項目を検出するための基準値
    // （モジュール doc コメント「本文を読み出せなかった場合」）。取得のたびに
    // 比べ、増えていたらその場で全体を失敗させる（読めなかった項目より後ろを
    // 読み進めても、結果は捨てるため無駄になる）。
    let fallback_items_before = registry.hydrate_fallback_items();

    for range in &selection.ranges {
        let target_end = range.start + range.count;
        let mut cursor = range.start;

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

            if registry.hydrate_fallback_items() != fallback_items_before {
                return Err(CopyError::SourceUnavailable);
            }

            if response.items.is_empty() {
                // 索引から算出した件数より実際の項目が少ない（検証済みの範囲
                // では通常発生しない防御的な打ち切り）。この範囲はこれ以上
                // 進めないため、次の範囲へ移る。
                break;
            }

            let next_cursor = response.start + response.items.len() as u64;

            for item in response.items {
                // 項目間の区切りは `\n` 1バイト（先頭の項目には付かない）。
                let separator_bytes = u64::from(!bodies.is_empty());
                accumulated_bytes = accumulated_bytes
                    .saturating_add(separator_bytes)
                    .saturating_add(item.raw_text.len() as u64);
                bodies.push(item.raw_text);

                if accumulated_bytes > limits.max_bytes {
                    // COPY-005: 打ち切って拒否する（非有界の作業を作らない）。
                    return Ok(CopyOutcome::Rejected(CopyRejection {
                        limit_bytes: limits.max_bytes,
                        limit_lines: limits.max_lines,
                        selected_lines: total_lines,
                        selected_bytes: Some(accumulated_bytes),
                    }));
                }
            }

            cursor = next_cursor;
        }
    }

    // PERF-010: 上限判定が完了した後に予約してから生成する。拒否経路は
    // 上の return でここへ到達しない。
    let total_bytes = usize::try_from(accumulated_bytes).unwrap_or(usize::MAX);
    let token = budget
        .reserve(total_bytes)
        .map_err(CopyError::MemoryReservationRejected)?;

    let mut text = String::with_capacity(total_bytes);
    for (index, body) in bodies.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        text.push_str(body);
    }

    let actual_bytes = text.len();
    token.mark_allocated(actual_bytes).expect(
        "上限判定で積算したバイト数と生成したバッファの実バイト数は常に一致するはず\
         （どちらも原文のバイト数と項目間の改行1バイトだけを数えている）",
    );

    Ok(CopyOutcome::Copied(CopyBuffer {
        text,
        bytes: actual_bytes as u64,
        lines: bodies.len() as u64,
    }))
}

/// 選択範囲がモジュール doc コメント「選択範囲の受け入れ条件」を満たすかを
/// 検証し、満たす場合は合計行数を返します（Issue #85）。
///
/// 合計行数を同時に返すのは、検証と同じ1回の走査で求まるうえ、`u64` の
/// 加算が溢れないこと（各範囲が `total_items` 以内で互いに素なので、合計も
/// `total_items` 以下）をこの関数の中で言い切れるためです。
fn validate_selection(
    selection: &CopySelection,
    total_items: u64,
) -> Result<u64, InvalidSelectionReason> {
    if selection.ranges.is_empty() {
        return Err(InvalidSelectionReason::NoRanges);
    }

    let mut total_lines: u64 = 0;
    let mut previous_end: u64 = 0;
    for range in &selection.ranges {
        if range.count == 0 {
            return Err(InvalidSelectionReason::EmptyRange);
        }
        // 先頭の範囲は `previous_end` が0のため、この判定は常に通る。2つ目
        // 以降は「前の範囲の終端以降から始まる」ことだけを要求する（隣接は
        // 許容。連結結果が1つの範囲にまとめた場合と同一になるため）。
        if range.start < previous_end {
            return Err(InvalidSelectionReason::OverlappingOrUnordered);
        }
        // 終端の算出そのものが溢れないよう、加算の前に確かめる。
        if range.start > total_items || range.count > total_items - range.start {
            return Err(InvalidSelectionReason::OutOfBounds);
        }
        previous_end = range.start + range.count;
        total_lines += range.count;
    }

    Ok(total_lines)
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

    /// [`insert_simple_lines`] と同じ構成で、各項目に比較キー（ミリ秒。
    /// `LOG-024`）を持たせたソースを登録します。統合表示集合（P09-1）の
    /// 並び順（ADR-0008）をまたいだコピーを検証するために使います。
    fn insert_lines_with_keys(
        registry: &mut DisplaySetRegistry,
        budget: &SourceBudget,
        file: &TempFile,
        label: &str,
        lines: &[(&str, i64)],
    ) -> crate::DisplaySetHandle {
        let mut content = Vec::new();
        let mut pending = Vec::new();
        for (index, (line, key)) in lines.iter().enumerate() {
            let raw_offset = content.len() as u64;
            content.extend_from_slice(line.as_bytes());
            pending.push(PendingItem {
                raw_offset,
                raw_byte_len: u32::try_from(line.len()).unwrap(),
                comparison_key_millis: Some(*key),
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

    /// `(start, count)` の並びから [`CopySelection`] を作ります（テストの
    /// 見通しのため。正規化は一切しないので、検証の拒否経路も書けます）。
    fn selection(ranges: &[(u64, u64)]) -> CopySelection {
        CopySelection {
            ranges: ranges
                .iter()
                .map(|&(start, count)| CopyRange { start, count })
                .collect(),
        }
    }

    fn generous_limits() -> CopyLimits {
        CopyLimits {
            max_bytes: 1_000_000,
            max_lines: 1_000_000,
        }
    }

    // 受け入れ条件: 行（論理ログ項目）選択では原文をそのまま、項目間は \n で
    // 連結する（ADR-0011。連結規則は ADR-0009 の行選択から変えていない）。
    // 継続行の内部改行も保持する（LOG-014、LOG-024）。
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
            &selection(&[(0, 1)]),
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
            &selection(&[(0, 3)]),
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

    // 受け入れ条件（COPY-001、Issue #85）: 飛び飛びの選択（複数の範囲）は、
    // 範囲を start 昇順にたどった順で連結される（範囲の切れ目でも区切りは
    // 項目間と同じ \n 1つだけで、余分な空行が入らない）。
    #[test]
    fn multiple_ranges_are_joined_in_ascending_order_without_extra_separators() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("multi-range", b"placeholder");

        let handle = insert_simple_lines(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &["l0", "l1", "l2", "l3", "l4"],
        );

        // 先頭1行と、間を空けた末尾2行（Ctrl+クリックで作れる形）。
        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            &selection(&[(0, 1), (3, 2)]),
            generous_limits(),
            &memory_budget,
        )
        .expect("成功するはず");

        match outcome {
            CopyOutcome::Copied(buffer) => {
                assert_eq!(buffer.text, "l0\nl3\nl4");
                assert_eq!(buffer.lines, 3, "行数は全範囲の合計のはず");
                assert_eq!(buffer.bytes, "l0\nl3\nl4".len() as u64);
            }
            other => panic!("Copied を期待したが {other:?} だった"),
        }
    }

    // 受け入れ条件（Issue #85）: 隣接した2範囲（前の終端 = 次の開始）は、
    // 1つにまとめた範囲と同じ内容になる（フロントエンドの正規化が働かなかった
    // 場合でも、貼り付け結果が変わらないことの確認）。
    #[test]
    fn adjacent_ranges_produce_the_same_text_as_one_merged_range() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("adjacent-range", b"placeholder");

        let handle = insert_simple_lines(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &["l0", "l1", "l2"],
        );

        let split = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            &selection(&[(0, 1), (1, 2)]),
            generous_limits(),
            &memory_budget,
        )
        .expect("隣接は受け入れるはず");
        let merged = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            &selection(&[(0, 3)]),
            generous_limits(),
            &memory_budget,
        )
        .expect("成功するはず");

        match (split, merged) {
            (CopyOutcome::Copied(split), CopyOutcome::Copied(merged)) => {
                assert_eq!(split.text, "l0\nl1\nl2");
                assert_eq!(split.text, merged.text);
                assert_eq!(split.lines, merged.lines);
                assert_eq!(split.bytes, merged.bytes);
            }
            other => panic!("どちらも Copied を期待したが {other:?} だった"),
        }
    }

    // 受け入れ条件（境界値、Issue #85）: 表示集合の末尾ちょうどで終わる範囲は
    // 受け入れる（範囲外の判定が1件ぶん厳しすぎないこと）。
    #[test]
    fn range_ending_exactly_at_total_items_is_accepted() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("range-boundary", b"placeholder");

        let handle =
            insert_simple_lines(&mut registry, &source_budget, &file, "a.log", &["l0", "l1"]);

        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            &selection(&[(1, 1)]),
            generous_limits(),
            &memory_budget,
        )
        .expect("末尾ちょうどは受け入れるはず");
        match outcome {
            CopyOutcome::Copied(buffer) => {
                assert_eq!(buffer.text, "l1");
                assert_eq!(buffer.lines, 1);
            }
            other => panic!("Copied を期待したが {other:?} だった"),
        }
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
            &selection(&[(0, 3)]),
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
            &["l1", "l2", "l3", "l4", "l5"],
        );

        // 飛び飛びの2範囲（2行 + 2行）。どちらも単独では上限（3行）以内だが、
        // 合計は4行で超える（Issue #85: 上限は全範囲の合計に対して判定する）。
        let outcome = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            &selection(&[(0, 2), (3, 2)]),
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
            &selection(&[(0, 1)]),
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
            &selection(&[(0, 1)]),
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
            &selection(&[(0, 2)]),
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

        let three_lines = selection(&[(0, 3)]);

        let rejected = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            &three_lines,
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
            &three_lines,
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
            &selection(&[(0, 1)]),
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
            &selection(&[(0, 1)]),
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

    // --- 選択範囲の検証（Issue #85） ---

    /// 検証だけを見るテストの共通手順。`ranges` を2行のソースへ当て、返った
    /// 失敗理由を照合します。
    fn expect_invalid_selection(
        label: &str,
        ranges: &[(u64, u64)],
        expected: InvalidSelectionReason,
    ) {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create(label, b"placeholder");

        let handle =
            insert_simple_lines(&mut registry, &source_budget, &file, "a.log", &["l0", "l1"]);

        let error = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            &selection(ranges),
            generous_limits(),
            &memory_budget,
        )
        .expect_err("不正な選択範囲はエラーになるはず");
        assert_eq!(error, CopyError::InvalidSelection(expected));
        assert_eq!(
            memory_budget.outstanding_reserved_bytes(),
            0,
            "検証で拒否した経路では予約が行われないはず（PERF-010）"
        );
    }

    // 受け入れ条件（COPY-006、Issue #85）: 範囲が1つも無い選択は拒否する
    // （空文字列でクリップボードを上書きしない）。
    #[test]
    fn selection_without_any_range_is_rejected() {
        expect_invalid_selection("invalid-empty", &[], InvalidSelectionReason::NoRanges);
    }

    // 受け入れ条件（Issue #85）: 行数が0の範囲は拒否する。
    #[test]
    fn zero_count_range_is_rejected() {
        expect_invalid_selection(
            "invalid-zero-count",
            &[(0, 0)],
            InvalidSelectionReason::EmptyRange,
        );
    }

    // 受け入れ条件（Issue #85）: start が昇順でない範囲列は拒否する。
    #[test]
    fn descending_ranges_are_rejected() {
        expect_invalid_selection(
            "invalid-descending",
            &[(1, 1), (0, 1)],
            InvalidSelectionReason::OverlappingOrUnordered,
        );
    }

    // 受け入れ条件（Issue #85）: 重なり合う範囲列は拒否する（同じ行を2回
    // コピーしない）。
    #[test]
    fn overlapping_ranges_are_rejected() {
        expect_invalid_selection(
            "invalid-overlapping",
            &[(0, 2), (1, 1)],
            InvalidSelectionReason::OverlappingOrUnordered,
        );
    }

    // 受け入れ条件（Issue #85）: 表示集合の外へ出る範囲は、黙って切り詰めず
    // 拒否する（フロントエンド側のクランプが働かなかったことを表面化させる）。
    #[test]
    fn range_beyond_total_items_is_rejected() {
        expect_invalid_selection(
            "invalid-out-of-bounds",
            &[(0, 100)],
            InvalidSelectionReason::OutOfBounds,
        );
    }

    // 受け入れ条件（Issue #85）: 開始位置そのものが表示集合の外にある範囲も
    // 拒否する。
    #[test]
    fn range_starting_beyond_total_items_is_rejected() {
        expect_invalid_selection(
            "invalid-start-out-of-bounds",
            &[(5, 1)],
            InvalidSelectionReason::OutOfBounds,
        );
    }

    // 受け入れ条件（Issue #85）: 未知の表示集合・世代不一致は、選択範囲の
    // 検証より先に返る（利用者への案内が「選び直す」ではなく「開き直す」に
    // なるため、種別を取り違えない）。
    #[test]
    fn unknown_display_set_is_reported_before_selection_validation() {
        let mut registry = DisplaySetRegistry::new();
        let memory_budget = MemoryBudget::new(10_000_000);

        let error = assemble_copy(
            &mut registry,
            999,
            1,
            &selection(&[]),
            generous_limits(),
            &memory_budget,
        )
        .expect_err("未登録のIDはエラーになるはず");
        assert_eq!(error, CopyError::Fetch(FetchRangeError::UnknownDisplaySet));
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
            &selection(&[(0, 1)]),
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
            &selection(&[(0, 1)]),
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

    // --- 統合表示集合（P09-1）のコピー（Issue #37） ---

    // 受け入れ条件（COPY-001／COPY-002、`LOG-007`）: 統合表示集合の行選択を
    // コピーでき、内容はソースをまたいで ADR-0008 の並び順になる（統合表示は
    // source_id を持たないため、以前は必ず UnknownDisplaySet で失敗していた）。
    #[test]
    fn merged_display_set_row_selection_is_copied_in_merged_order() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file_a = TempFile::create("merged-row-a", b"placeholder");
        let file_b = TempFile::create("merged-row-b", b"placeholder");

        insert_lines_with_keys(
            &mut registry,
            &source_budget,
            &file_a,
            "a.log",
            &[("a-10", 10), ("a-30", 30)],
        );
        insert_lines_with_keys(
            &mut registry,
            &source_budget,
            &file_b,
            "b.log",
            &[("b-20", 20)],
        );

        let merged = registry.enable_merged_view().expect("成功するはず");
        assert_eq!(merged.total_items, 3);

        let outcome = assemble_copy(
            &mut registry,
            merged.display_set_id,
            merged.generation,
            &selection(&[(0, 3)]),
            generous_limits(),
            &memory_budget,
        )
        .expect("統合表示集合でも成功するはず");

        match outcome {
            CopyOutcome::Copied(buffer) => {
                assert_eq!(
                    buffer.text, "a-10\nb-20\na-30",
                    "比較キー昇順（ソースをまたぐ並び）でコピーされるはず"
                );
                assert_eq!(buffer.lines, 3);
                assert!(
                    !buffer.text.contains("a.log") && !buffer.text.contains("b.log"),
                    "統合表示の画面にだけある読み込み元ラベルはコピーへ含めない（LOG-007）"
                );
            }
            other => panic!("Copied を期待したが {other:?} だった"),
        }
    }

    // 受け入れ条件（COPY-001、Issue #85）: 統合表示集合でも飛び飛びの選択が
    // でき、範囲の切れ目をまたいでも統合順（ADR-0008）のまま連結される。
    #[test]
    fn merged_display_set_multiple_ranges_follow_the_merged_order() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file_a = TempFile::create("merged-multi-a", b"placeholder");
        let file_b = TempFile::create("merged-multi-b", b"placeholder");

        insert_lines_with_keys(
            &mut registry,
            &source_budget,
            &file_a,
            "a.log",
            &[("a-10", 10), ("a-30", 30)],
        );
        insert_lines_with_keys(
            &mut registry,
            &source_budget,
            &file_b,
            "b.log",
            &[("b-20", 20)],
        );

        let merged = registry.enable_merged_view().expect("成功するはず");

        // 統合順は a-10 / b-20 / a-30。真ん中（別ソース）を外して両端だけを選ぶ。
        let outcome = assemble_copy(
            &mut registry,
            merged.display_set_id,
            merged.generation,
            &selection(&[(0, 1), (2, 1)]),
            generous_limits(),
            &memory_budget,
        )
        .expect("統合表示集合でも成功するはず");

        match outcome {
            CopyOutcome::Copied(buffer) => {
                assert_eq!(buffer.text, "a-10\na-30");
                assert_eq!(buffer.lines, 2);
            }
            other => panic!("Copied を期待したが {other:?} だった"),
        }
    }

    // 受け入れ条件: 統合表示集合の世代不一致（ON のまま対象を開いた・閉じた
    // 場合に起きる）も、単独ソースと同じ既存のエラー経路で返る。
    #[test]
    fn merged_display_set_generation_mismatch_reuses_the_existing_fetch_error() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file_a = TempFile::create("merged-gen-a", b"placeholder");
        let file_b = TempFile::create("merged-gen-b", b"placeholder");

        insert_lines_with_keys(
            &mut registry,
            &source_budget,
            &file_a,
            "a.log",
            &[("a-10", 10)],
        );
        let merged = registry.enable_merged_view().expect("成功するはず");

        // 統合表示 ON のまま別の対象を開くと、統合表示集合は作り直されて世代が
        // 1つ進む（`sync_merged_view`）。フロントエンドが持つ古い世代での
        // コピー要求はここで弾かれる。
        insert_lines_with_keys(
            &mut registry,
            &source_budget,
            &file_b,
            "b.log",
            &[("b-20", 20)],
        );

        let error = assemble_copy(
            &mut registry,
            merged.display_set_id,
            merged.generation,
            &selection(&[(0, 1)]),
            generous_limits(),
            &memory_budget,
        )
        .expect_err("世代不一致はエラーになるはず");

        assert_eq!(
            error,
            CopyError::Fetch(FetchRangeError::GenerationMismatch {
                expected: merged.generation,
                current: merged.generation + 1,
            })
        );
    }

    // 受け入れ条件: 統合表示を OFF にした後の古い display_set_id は、既存の
    // 「未知の表示集合」経路になる（統合表示の識別子を無条件に受理しない）。
    #[test]
    fn disabled_merged_display_set_is_an_unknown_display_set() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("merged-disabled", b"placeholder");

        insert_lines_with_keys(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &[("a-10", 10)],
        );
        let merged = registry.enable_merged_view().expect("成功するはず");
        registry.disable_merged_view();

        let error = assemble_copy(
            &mut registry,
            merged.display_set_id,
            merged.generation,
            &selection(&[(0, 1)]),
            generous_limits(),
            &memory_budget,
        )
        .expect_err("破棄済みの表示集合はエラーになるはず");
        assert_eq!(error, CopyError::Fetch(FetchRangeError::UnknownDisplaySet));
    }

    // --- 本文を読み出せなかった場合（COPY-005、Issue #37） ---

    // 受け入れ条件（COPY-005）: コピーの最中にソースが削除されると、範囲取得の
    // 既定値（空の本文）が黙ってクリップボードへ渡らず、コピー全体が失敗する。
    #[test]
    fn copy_fails_when_the_body_cannot_be_read_instead_of_copying_empty_text() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file = TempFile::create("body-unavailable", b"placeholder");

        let handle = insert_simple_lines(
            &mut registry,
            &source_budget,
            &file,
            "a.log",
            &["alpha", "beta"],
        );

        // 索引は登録済みのまま、実ファイルだけが消える（LOG-023 の削除検知が
        // 範囲取得の中で起きる状況をそのまま作る）。
        std::fs::remove_file(&file.path).expect("削除できるはず");

        let error = assemble_copy(
            &mut registry,
            handle.display_set_id,
            handle.generation,
            &selection(&[(0, 2)]),
            generous_limits(),
            &memory_budget,
        )
        .expect_err("本文を読み出せない場合はエラーになるはず");

        assert_eq!(error, CopyError::SourceUnavailable);
        assert_eq!(
            memory_budget.outstanding_reserved_bytes(),
            0,
            "失敗経路では予約が行われないはず（PERF-010、部分コピーなし）"
        );
    }

    // 受け入れ条件（COPY-005）: 統合表示集合でも、参加ソースの1つが読み出せなく
    // なった時点でコピー全体が失敗する（読める側のソースの内容だけを黙って
    // コピーしない）。
    #[test]
    fn merged_copy_fails_when_one_member_source_cannot_be_read() {
        let mut registry = DisplaySetRegistry::new();
        let source_budget = SourceBudget::new();
        let memory_budget = MemoryBudget::new(10_000_000);
        let file_a = TempFile::create("merged-unavailable-a", b"placeholder");
        let file_b = TempFile::create("merged-unavailable-b", b"placeholder");

        insert_lines_with_keys(
            &mut registry,
            &source_budget,
            &file_a,
            "a.log",
            &[("a-10", 10)],
        );
        insert_lines_with_keys(
            &mut registry,
            &source_budget,
            &file_b,
            "b.log",
            &[("b-20", 20)],
        );
        let merged = registry.enable_merged_view().expect("成功するはず");

        std::fs::remove_file(&file_b.path).expect("削除できるはず");

        let error = assemble_copy(
            &mut registry,
            merged.display_set_id,
            merged.generation,
            &selection(&[(0, 2)]),
            generous_limits(),
            &memory_budget,
        )
        .expect_err("参加ソースを読み出せない場合はエラーになるはず");

        assert_eq!(error, CopyError::SourceUnavailable);
        assert_eq!(memory_budget.outstanding_reserved_bytes(), 0);
    }
}
