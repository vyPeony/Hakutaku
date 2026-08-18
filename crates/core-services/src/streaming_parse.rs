//! チャンク境界をまたいだ増分解析（日時自動判定＋継続行結合）の状態機械です
//! （P06-2、`tasks/phase-06-large-file-loading.md` 作業項目1・2）。
//!
//! `crate::loader` の登録時ストリーミング解析が、日時書式の自動判定
//! （`LOG-DT-001`〜`006`）と継続行結合（`LOG-014`）の規則を、**1行ずつ届く**
//! 入力に対して、全件一括読み込みと同じ結果になるよう適用します。
//!
//! # 「保留」による継続行結合（チャンク境界をまたぐケース）
//!
//! 継続行は「直前の日時付き項目」へ結合されます（`LOG-014`）。チャンク境界が
//! 継続行の途中に落ちる場合、直前の項目が既に呼び出し側（表示集合）へ
//! 届け済みだと、後から結合できません。これを避けるため、[`StreamingAssembler`]
//! は「まだ継続行を受け取るかもしれない最後の項目」を `held_item` として
//! 常に1件だけ保留し、[`StreamingAssembler::drain_ready`] では返しません。
//! 次のいずれかで確実に完結したと判断できた時点で初めて `ready` へ移します。
//!
//! - 新しい日時付き行が来た（その行が新しい `held_item` になる）
//! - 日時未確定の独立行が来た（直前の保留項目が日時なしだった場合）
//! - [`StreamingAssembler::finish`] が呼ばれた（読み込み終了）
//!
//! # P08-5 本文を保持しない継続行結合
//!
//! `held_item`（[`crate::item::PendingItem`]）はファイルの生バイト範囲
//! （`raw_offset`・`raw_byte_len`）だけを持ち、本文（デコード済みテキスト）を
//! 一切保持しません。継続行を結合する際も、文字列の連結ではなく
//! `raw_byte_len` の再計算（新しい継続行の生バイト範囲の終端まで伸ばすだけ）
//! で済みます。デコード済みテキストは、この構造体へ渡される**前**（1行分の
//! 一時デコード）と、日時自動判定の判定中バッファ（[`BufferedLine`]、最大
//! [`DATETIME_AUTO_SCAN_LIMIT`] 行という有界サイズ）だけに現れる一時データで
//! あり、`held_item`・`ready` には含まれません。
//!
//! # 日時書式の自動判定（チャンク境界をまたぐケース）
//!
//! 判定（[`DATETIME_AUTO_SCAN_LIMIT`] 行までの走査）が完了する前にチャンクが
//! 尽きることがあります。判定が確定するまで、受け取った行を（本文だけの
//! 軽量な形で）`buffer` に保持し、上限に達するか [`StreamingAssembler::finish`]
//! が呼ばれた時点でまとめて判定・再生します（旧 `crate::loader::
//! detect_datetime_format` と同じロジックを、1行ずつ届く入力向けに
//! 再実装したものです）。
//!
//! # 日時書式の明示指定
//!
//! ログ解析プロファイルが日時書式を明示している場合
//! （`hakutaku_config::DateTimeFormatSetting`）、[`StreamingAssembler::new`] へ
//! その書式を渡すと、上記の自動判定を**一度も行わず**最初から
//! [`Mode::Confirmed`] で解析します。自動判定が構造的に曖昧になる
//! `LOG-DT-004`（`YYYY/MM/DD HH:mm:ss:SS`）のログを、生表示へ退避させずに
//! 解析するための経路です（`crate::loader` の doc コメント「日時書式の決め方」
//! 参照）。

use hakutaku_parser::{AutoParseOutcome, LogDateTimeFormat};

use crate::item::PendingItem;

/// ファイル先頭から日時書式の自動判定のために走査する最大行数。
pub(crate) const DATETIME_AUTO_SCAN_LIMIT: usize = 100;

/// 判定中に溜めておく物理行1件です（本文は判定確定後の再解析にだけ使う一時
/// データ。モジュール doc コメント「本文を保持しない継続行結合」参照）。
struct BufferedLine {
    text: String,
    line_number: u64,
    unconfirmed: bool,
    raw_offset: u64,
    raw_content_len: u32,
}

/// 日時書式の自動判定結果です（`crate::loader::DatetimeDetection` と同じ形）。
enum DatetimeDetection {
    Confirmed(LogDateTimeFormat),
    NoneFound,
    Ambiguous,
}

enum Mode {
    /// 日時書式・プロファイル起因の判定がまだ確定していない。
    Detecting { buffer: Vec<BufferedLine> },
    /// 生表示（1行=1項目、日時なし）。以後ずっとこのモード（`LOG-022`）。
    RawDisplay,
    /// 日時書式が確定した。
    Confirmed(LogDateTimeFormat),
}

/// チャンクをまたいだ増分解析の状態機械です。
pub(crate) struct StreamingAssembler {
    mode: Mode,
    /// まだ継続行を受け取るかもしれない、直近の未確定な項目。
    held_item: Option<PendingItem>,
    /// 確定済みで呼び出し側へ渡してよい項目（[`Self::drain_ready`] で回収）。
    ready: Vec<PendingItem>,
    total_physical_lines: u64,
    detected_datetime_format: Option<LogDateTimeFormat>,
    /// `LOG-022`: 曖昧判定により生表示へ退避したか（プロファイル起因の退避は
    /// [`Self::new`] の `raw_display_due_to_profile` で別途扱う）。
    ambiguous_datetime: bool,
}

impl StreamingAssembler {
    /// `explicit_datetime_format` に明示された書式（プロファイルの
    /// `datetime_format`、または UI での手動選択のどちらか。優先順位の決定は
    /// 呼び出し側 `crate::loader` の責務）を渡すと、日時書式の自動判定を一切
    /// 行わず、その書式で全行を解析します。`None` なら従来どおり内容から
    /// 自動判定します。
    ///
    /// `raw_display_due_to_profile`（`Ambiguous`／`ManualNotFound` によるプロ
    /// ファイル起因の生表示退避）が真の場合は、明示指定があってもそちらを
    /// 優先して生表示にします。プロファイル自体を一意に決められていない以上、
    /// 文字コードと日時解析のよりどころが食い違うため、書式指定だけを採用
    /// しません。設定由来の書式については呼び出し側の
    /// `crate::loader::profile_datetime_format` がこれらの解決結果で常に `None`
    /// を返すため同時には成立しませんが、UI での手動選択は解決結果と独立に
    /// 渡ってくるため、ここでの優先関係が実際に効きます。
    pub(crate) fn new(
        raw_display_due_to_profile: bool,
        explicit_datetime_format: Option<LogDateTimeFormat>,
    ) -> Self {
        let mode = match (raw_display_due_to_profile, explicit_datetime_format) {
            (true, _) => Mode::RawDisplay,
            (false, Some(format)) => Mode::Confirmed(format),
            (false, None) => Mode::Detecting { buffer: Vec::new() },
        };
        StreamingAssembler {
            mode,
            held_item: None,
            ready: Vec::new(),
            total_physical_lines: 0,
            // 明示指定された書式も「このソースで確定した書式」として報告する
            // （`LoadSummary::detected_datetime_format` と、`crate::registry` が
            // 行う `timestamp_display` の再構成が同じ値を使うため）。
            detected_datetime_format: if raw_display_due_to_profile {
                None
            } else {
                explicit_datetime_format
            },
            ambiguous_datetime: false,
        }
    }

    /// 1物理行を投入します。`unconfirmed` はファイル末尾の未確定行
    /// （`LOG-026`）だけ `true` にしてください（それ以外は常に `false`）。
    ///
    /// `raw_offset`・`raw_content_len` は、この行（区切り文字を含まない本文）
    /// のファイル先頭からの生バイト範囲です（`text` を得るために一時デコード
    /// した元の生バイト範囲。`crate::line_index::LineIndexEntry` と同じ意味）。
    pub(crate) fn feed_line(
        &mut self,
        text: &str,
        raw_offset: u64,
        raw_content_len: u32,
        unconfirmed: bool,
    ) {
        self.total_physical_lines += 1;
        let line_number = self.total_physical_lines;

        match &mut self.mode {
            Mode::RawDisplay => {
                self.push_raw_item(raw_offset, raw_content_len, line_number, unconfirmed);
            }
            Mode::Confirmed(format) => {
                let format = *format;
                self.feed_confirmed_line(
                    format,
                    text,
                    raw_offset,
                    raw_content_len,
                    line_number,
                    unconfirmed,
                );
            }
            Mode::Detecting { buffer } => {
                // 「最初に見つかった Matched／Ambiguous の行で確定する」規則を
                // 1行ずつ届く入力に対しても即座に適用する。100行溜まるまで
                // 判定を先延ばしにすると、実際には1行目で確定するはずの
                // ファイルでも100行分の遅延・保留が生じてしまうため、行が
                // 届くたびに判定を試みる。
                let detection = match hakutaku_parser::parse_datetime_auto(text) {
                    AutoParseOutcome::Matched(matched) => {
                        Some(DatetimeDetection::Confirmed(matched.format))
                    }
                    AutoParseOutcome::Ambiguous(_) => Some(DatetimeDetection::Ambiguous),
                    AutoParseOutcome::NoMatch => None,
                };

                buffer.push(BufferedLine {
                    text: text.to_string(),
                    line_number,
                    unconfirmed,
                    raw_offset,
                    raw_content_len,
                });

                if let Some(detection) = detection {
                    self.resolve_detection(detection);
                } else if buffer.len() >= DATETIME_AUTO_SCAN_LIMIT {
                    self.resolve_detection(DatetimeDetection::NoneFound);
                }
            }
        }
    }

    /// これ以上データが来ないことを通知します。判定中であればこの時点までの
    /// 内容で確定させ、保留中の項目（`held_item`）があれば `ready` へ移します。
    pub(crate) fn finish(&mut self) {
        if matches!(self.mode, Mode::Detecting { .. }) {
            // 100行に満たないままファイル末尾に達した場合、それまでの内容に
            // 日時付き行が1つも見つからなかったことになる（見つかっていれば
            // feed_line 側で既に resolve_detection 済みのはず）。
            self.resolve_detection(DatetimeDetection::NoneFound);
        }
        self.flush_held();
    }

    /// `detection` に確定した結果で、`buffer` に溜めていた行をまとめて
    /// 再生します。
    fn resolve_detection(&mut self, detection: DatetimeDetection) {
        let Mode::Detecting { buffer } = std::mem::replace(&mut self.mode, Mode::RawDisplay) else {
            unreachable!("resolve_detection は Detecting モードでのみ呼ばれる");
        };

        match detection {
            DatetimeDetection::Confirmed(format) => {
                self.detected_datetime_format = Some(format);
                self.mode = Mode::Confirmed(format);
                for line in buffer {
                    self.feed_confirmed_line(
                        format,
                        &line.text,
                        line.raw_offset,
                        line.raw_content_len,
                        line.line_number,
                        line.unconfirmed,
                    );
                }
            }
            DatetimeDetection::Ambiguous => {
                self.ambiguous_datetime = true;
                self.mode = Mode::RawDisplay;
                for line in buffer {
                    self.push_raw_item(
                        line.raw_offset,
                        line.raw_content_len,
                        line.line_number,
                        line.unconfirmed,
                    );
                }
            }
            DatetimeDetection::NoneFound => {
                self.mode = Mode::RawDisplay;
                for line in buffer {
                    self.push_raw_item(
                        line.raw_offset,
                        line.raw_content_len,
                        line.line_number,
                        line.unconfirmed,
                    );
                }
            }
        }
    }

    /// 日時書式が確定している状態で1行を処理します（`LOG-014` の継続行結合
    /// 規則。旧 `crate::loader::merge_continuation_lines` を1行ずつ届く入力
    /// 向けに再実装したものです）。
    #[allow(clippy::too_many_arguments)]
    fn feed_confirmed_line(
        &mut self,
        format: LogDateTimeFormat,
        text: &str,
        raw_offset: u64,
        raw_content_len: u32,
        line_number: u64,
        unconfirmed: bool,
    ) {
        let matched = hakutaku_parser::parse_datetime_with_format(format, text);

        match matched {
            Some(matched) => {
                self.flush_held();
                self.held_item = Some(PendingItem {
                    raw_offset,
                    raw_byte_len: raw_content_len,
                    comparison_key_millis: Some(matched.comparison_key.as_millis_since_epoch()),
                    source_line_number: line_number,
                    continuation_count: 0,
                    unconfirmed,
                });
            }
            None => {
                // held_item が「直前の“日時付き”行」である場合だけ結合する
                // （crate::loader のドキュメント「継続行の結合」と同じ判断）。
                let merge =
                    matches!(&self.held_item, Some(item) if item.comparison_key_millis.is_some());
                if merge {
                    let held = self
                        .held_item
                        .as_mut()
                        .expect("直前の matches! で Some を確認済み");
                    // 継続行の末尾まで範囲を伸ばす（文字列連結ではなく生バイト
                    // 範囲の再計算だけで済む。P08-5）。
                    let new_end = raw_offset.saturating_add(u64::from(raw_content_len));
                    let extended_len = new_end.saturating_sub(held.raw_offset);
                    held.raw_byte_len = u32::try_from(extended_len).unwrap_or(u32::MAX);
                    held.continuation_count = held.continuation_count.saturating_add(1);
                    // 末尾の断片（LOG-026）は必ずファイル全体の最終物理行にしか
                    // 現れないため、継続行として結合された場合はこの項目全体が
                    // 未確定行になる。
                    held.unconfirmed = unconfirmed;
                } else {
                    self.flush_held();
                    self.held_item = Some(PendingItem {
                        raw_offset,
                        raw_byte_len: raw_content_len,
                        comparison_key_millis: None,
                        source_line_number: line_number,
                        continuation_count: 0,
                        unconfirmed,
                    });
                }
            }
        }
    }

    /// 生表示（1行=1項目、日時 `None`）として即座に確定させます。継続行結合を
    /// 行わないため `held_item` を経由せず、直接 `ready` へ積みます。
    fn push_raw_item(
        &mut self,
        raw_offset: u64,
        raw_content_len: u32,
        line_number: u64,
        unconfirmed: bool,
    ) {
        self.ready.push(PendingItem {
            raw_offset,
            raw_byte_len: raw_content_len,
            comparison_key_millis: None,
            source_line_number: line_number,
            continuation_count: 0,
            unconfirmed,
        });
    }

    fn flush_held(&mut self) {
        if let Some(item) = self.held_item.take() {
            self.ready.push(item);
        }
    }

    /// 確定済みで即座に流してよい項目を取り出します（`held_item` は含まれ
    /// ません）。空の場合は空の `Vec` を返します。
    pub(crate) fn drain_ready(&mut self) -> Vec<PendingItem> {
        std::mem::take(&mut self.ready)
    }

    pub(crate) fn total_physical_lines(&self) -> u64 {
        self.total_physical_lines
    }

    pub(crate) fn detected_datetime_format(&self) -> Option<LogDateTimeFormat> {
        self.detected_datetime_format
    }

    /// `LOG-022` により日時未解析の生表示へ退避したか（プロファイル起因の
    /// 退避、または曖昧判定による退避のいずれか）。
    pub(crate) fn fell_back_to_raw_display(&self, raw_display_due_to_profile: bool) -> bool {
        raw_display_due_to_profile || self.ambiguous_datetime
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // テスト用に、連続した行を「1文字1バイト」の前提で仮想的な生バイト範囲を
    // 割り当てながら feed_line する（ASCII のみの試験データなので安全）。
    fn feed_all(assembler: &mut StreamingAssembler, lines: &[(&str, bool)]) {
        let mut offset = 0u64;
        for (text, unconfirmed) in lines {
            let len = u32::try_from(text.len()).unwrap();
            assembler.feed_line(text, offset, len, *unconfirmed);
            offset += u64::from(len) + 1; // +1 は仮想的な区切り文字1バイト分。
        }
    }

    fn spans(items: &[PendingItem]) -> Vec<(u64, u32)> {
        items
            .iter()
            .map(|item| (item.raw_offset, item.raw_byte_len))
            .collect()
    }

    // 受け入れ条件: 日時付き行と継続行が結合される（LOG-014）。範囲が継続行の
    // 終端まで正しく伸びる。
    #[test]
    fn continuation_lines_are_merged_into_the_preceding_dated_item() {
        let mut assembler = StreamingAssembler::new(false, None);
        feed_all(
            &mut assembler,
            &[
                ("2026/07/28 15:12:23.456 起動しました", false),
                ("継続行1", false),
                ("継続行2", false),
                ("2026/07/28 15:12:24.000 次の項目", false),
            ],
        );
        assembler.finish();

        let items = assembler.drain_ready();
        assert_eq!(items.len(), 2);
        // 1件目は「起動しました」〜「継続行2」の末尾までの範囲を覆う。
        let line0_len = "2026/07/28 15:12:23.456 起動しました".len() as u64;
        let line1_len = "継続行1".len() as u64;
        let line2_len = "継続行2".len() as u64;
        let expected_len = line0_len + 1 + line1_len + 1 + line2_len;
        assert_eq!(items[0].raw_offset, 0);
        assert_eq!(items[0].raw_byte_len as u64, expected_len);
        assert_eq!(items[0].continuation_count, 2);
        assert_eq!(items[1].continuation_count, 0);
    }

    // 受け入れ条件: 継続行の直前で drain_ready しても、継続行はまだ届いていない
    // 保留項目（held_item）へ正しく結合される（チャンク境界が継続行の直前に
    // 落ちるケース）。
    #[test]
    fn held_item_still_receives_continuations_arriving_in_a_later_batch() {
        let mut assembler = StreamingAssembler::new(false, None);
        assembler.feed_line("2026/07/28 15:12:23.456 起動しました", 0, 40, false);
        let first_batch = assembler.drain_ready();
        assert!(
            first_batch.is_empty(),
            "held_item はまだ ready へ移っていないはず"
        );

        assembler.feed_line("継続行1", 41, 12, false);
        assembler.finish();
        let items = assembler.drain_ready();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].raw_offset, 0);
        assert_eq!(items[0].raw_byte_len, 41 + 12);
        assert_eq!(items[0].continuation_count, 1);
    }

    // 受け入れ条件: 先頭の日時なし行が破棄されず、それぞれ独立した項目になる
    // （crate::loader の同名テストと同じ規則）。
    #[test]
    fn leading_lines_without_datetime_stay_independent() {
        let mut assembler = StreamingAssembler::new(false, None);
        feed_all(
            &mut assembler,
            &[
                ("起動準備中", false),
                ("2026/07/28 15:12:23.456 起動しました", false),
            ],
        );
        assembler.finish();

        let items = assembler.drain_ready();
        assert_eq!(items.len(), 2);
        assert!(items[0].comparison_key_millis.is_none());
        assert!(items[1].comparison_key_millis.is_some());
    }

    // 受け入れ条件: 100行を超えても日時が見つからなければ全行が独立した生
    // データ項目になる（DATETIME_AUTO_SCAN_LIMIT を跨ぐケース）。
    #[test]
    fn no_datetime_within_scan_limit_falls_back_to_raw_display_for_all_lines() {
        let mut assembler = StreamingAssembler::new(false, None);
        let lines: Vec<(String, bool)> = (0..150)
            .map(|i| (format!("日時なし行{i}"), false))
            .collect();
        let borrowed: Vec<(&str, bool)> = lines.iter().map(|(s, b)| (s.as_str(), *b)).collect();
        feed_all(&mut assembler, &borrowed);
        assembler.finish();

        let items = assembler.drain_ready();
        assert_eq!(items.len(), 150);
        assert!(items
            .iter()
            .all(|item| item.comparison_key_millis.is_none()));
        assert!(assembler.detected_datetime_format().is_none());
    }

    // 受け入れ条件: 曖昧な日時は生表示へ退避する（LOG-022）。
    #[test]
    fn ambiguous_datetime_falls_back_to_raw_display() {
        let mut assembler = StreamingAssembler::new(false, None);
        feed_all(
            &mut assembler,
            &[
                ("2026/07/28 15:12:23:45 一行目", false),
                ("2026/07/28 15:12:24:99 二行目", false),
            ],
        );
        assembler.finish();

        assert!(assembler.fell_back_to_raw_display(false));
        let items = assembler.drain_ready();
        assert_eq!(items.len(), 2);
        assert_eq!(spans(&items)[0].0, 0);
    }

    // 受け入れ条件: プロファイル起因の生表示退避では、1行=1項目のまま日時判定を
    // 一切行わない。
    #[test]
    fn raw_display_due_to_profile_skips_detection_entirely() {
        let mut assembler = StreamingAssembler::new(true, None);
        feed_all(
            &mut assembler,
            &[
                ("2026/07/28 15:12:23.456 起動しました", false),
                ("2026/07/28 15:12:24.000 次の行", false),
            ],
        );
        assembler.finish();

        let items = assembler.drain_ready();
        assert_eq!(items.len(), 2, "生表示では継続行結合を行わない");
        assert!(items
            .iter()
            .all(|item| item.comparison_key_millis.is_none()));
    }

    // 受け入れ条件（LOG-026）: 末尾が改行で終わらない断片は unconfirmed になり、
    // 継続行として結合された場合は結合先の項目全体が未確定になる。
    #[test]
    fn trailing_unconfirmed_fragment_marks_the_merged_item_as_unconfirmed() {
        let mut assembler = StreamingAssembler::new(false, None);
        feed_all(
            &mut assembler,
            &[
                ("2026/07/28 15:12:23.456 起動しました", false),
                ("未確定の断片", true),
            ],
        );
        assembler.finish();

        let items = assembler.drain_ready();
        assert_eq!(items.len(), 1);
        assert!(items[0].unconfirmed);
        assert_eq!(items[0].continuation_count, 1);
    }

    // 受け入れ条件: 書式を明示指定すると、自動判定では曖昧になる
    // LOG-DT-004 のログでも解析され、継続行結合まで通常どおり働く。
    #[test]
    fn explicit_datetime_format_parses_log_dt_004_without_ambiguity_fallback() {
        let mut assembler = StreamingAssembler::new(false, Some(LogDateTimeFormat::LogDt004));
        feed_all(
            &mut assembler,
            &[
                ("2026/07/28 15:12:23:45 一行目", false),
                ("継続行", false),
                ("2026/07/28 15:12:24:99 二行目", false),
            ],
        );
        assembler.finish();

        assert!(
            !assembler.fell_back_to_raw_display(false),
            "明示指定した書式では曖昧判定による退避が起きない"
        );
        assert_eq!(
            assembler.detected_datetime_format(),
            Some(LogDateTimeFormat::LogDt004)
        );
        let items = assembler.drain_ready();
        assert_eq!(items.len(), 2, "継続行が直前の項目へ結合される（LOG-014）");
        assert!(items
            .iter()
            .all(|item| item.comparison_key_millis.is_some()));
    }

    // 受け入れ条件: 明示指定した書式に一致しない行は、生表示へ
    // 切り替えずに継続行として扱う（指定した書式だけで解析し続ける）。
    #[test]
    fn explicit_datetime_format_never_falls_back_to_auto_detection() {
        let mut assembler = StreamingAssembler::new(false, Some(LogDateTimeFormat::LogDt004));
        feed_all(
            &mut assembler,
            &[
                ("2026/07/28 15:12:23:45 一行目", false),
                // LOG-DT-001 の書式。自動判定なら 001 で確定するが、明示指定
                // された 004 に一致しないため継続行として結合される。
                ("2026/07/28 15:12:24.456 別書式の行", false),
            ],
        );
        assembler.finish();

        let items = assembler.drain_ready();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].continuation_count, 1);
        assert_eq!(
            assembler.detected_datetime_format(),
            Some(LogDateTimeFormat::LogDt004)
        );
    }

    // 受け入れ条件: プロファイル起因の生表示退避
    // （Ambiguous／ManualNotFound）は、書式の明示指定より優先される。
    #[test]
    fn raw_display_due_to_profile_wins_over_explicit_datetime_format() {
        let mut assembler = StreamingAssembler::new(true, Some(LogDateTimeFormat::LogDt004));
        feed_all(
            &mut assembler,
            &[
                ("2026/07/28 15:12:23:45 一行目", false),
                ("2026/07/28 15:12:24:99 二行目", false),
            ],
        );
        assembler.finish();

        let items = assembler.drain_ready();
        assert_eq!(items.len(), 2, "生表示では1行=1項目のまま");
        assert!(items
            .iter()
            .all(|item| item.comparison_key_millis.is_none()));
        assert_eq!(assembler.detected_datetime_format(), None);
    }

    // 受け入れ条件: 独立した生データ項目としての末尾断片も unconfirmed になる。
    #[test]
    fn trailing_unconfirmed_independent_line_is_marked_unconfirmed() {
        let mut assembler = StreamingAssembler::new(false, None);
        assembler.feed_line("未確定の断片", 0, 18, true);
        assembler.finish();

        let items = assembler.drain_ready();
        assert_eq!(items.len(), 1);
        assert!(items[0].unconfirmed);
    }

    // 受け入れ条件（Issue #36、LOG-014・LOG-024）: 既知の6書式より細かい小数秒
    // （マイクロ秒など）を持つ行は、ミリ秒へ切り詰めて解析せず日時なしとして
    // 扱う。直前に日時付き行があれば継続行として結合される。
    #[test]
    fn sub_millisecond_line_is_absorbed_as_a_continuation_line() {
        let mut assembler = StreamingAssembler::new(false, None);
        feed_all(
            &mut assembler,
            &[
                ("2026/07/28 15:12:23.456 一行目", false),
                ("2026/07/28 15:12:24.4567 マイクロ秒の行", false),
            ],
        );
        assembler.finish();

        assert_eq!(
            assembler.detected_datetime_format(),
            Some(LogDateTimeFormat::LogDt001)
        );
        let items = assembler.drain_ready();
        assert_eq!(items.len(), 1, "小数4桁の行は継続行として結合される");
        assert_eq!(items[0].continuation_count, 1);
    }

    // 受け入れ条件（Issue #36、LOG-022）: マイクロ秒精度だけで構成された
    // ファイルは、先頭行で書式を確定できないため「日時を持たないログ」として
    // 1行=1項目で扱う（曖昧判定による生表示退避ではない）。
    #[test]
    fn file_of_only_sub_millisecond_lines_has_no_detected_format() {
        let mut assembler = StreamingAssembler::new(false, None);
        feed_all(
            &mut assembler,
            &[
                ("2026/07/28 15:12:23.4567 一行目", false),
                ("2026/07/28 15:12:24.4567 二行目", false),
            ],
        );
        assembler.finish();

        assert!(assembler.detected_datetime_format().is_none());
        assert!(
            !assembler.fell_back_to_raw_display(false),
            "曖昧判定による退避ではなく、単に日時書式を持たないログとして扱う"
        );
        let items = assembler.drain_ready();
        assert_eq!(items.len(), 2, "日時なし行はそれぞれ独立した項目のまま");
        assert!(items
            .iter()
            .all(|item| item.comparison_key_millis.is_none()));
    }
}
