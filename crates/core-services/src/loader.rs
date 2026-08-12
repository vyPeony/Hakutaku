//! ファイル読み込み〜表示集合登録までのオーケストレーション（P05-6、
//! `tasks/phase-05-log-parsing-core.md` 作業項目5〜8）。
//!
//! P05 の各部品（6書式日時解析・プロファイル4段階解決・文字コード判定）を、
//! 実際の読み込みパイプラインへ結線します。P08-5 以降、この
//! パイプラインは**索引 + オンデマンド読み出し**方式です（本文をファイル全量
//! 分蓄積しません。下記「P08-5: 索引 + オンデマンド読み出しへの移行」参照）。
//!
//! # パイプライン
//!
//! 1. **チャンク読み込み**（[`hakutaku_data_source::stream_snapshotted_bytes_chunked`]、
//!    `PERF-010` の接続点。読み込んだ生バイト列全体は保持しません）。
//! 2. **プロファイル解決**（`crate::profile_resolution::resolve_profile`、
//!    `LOG-021` の4段階）。手動指定は [`LoadControl::manual_profile`] 経由。
//! 3. **文字コード判定・チャンクごとの一時デコード**（`crates/format-detection`、
//!    `ENC-005`。[`DecodeCursor`]）。
//! 4. **生バイトの行分割**（[`hakutaku_data_source::split_raw_lines`]）と
//!    **デコード後の行分割**（[`hakutaku_data_source::split_lines`]）を
//!    1対1に対応付け、生バイトオフセットを記録します（[`DecodedLine`]）。
//! 5. **日時の自動判定＋継続行の結合**（[`crate::streaming_parse::
//!    StreamingAssembler`]）。生バイト範囲だけを持つ [`PendingItem`] を生成し、
//!    デコード済みテキストはここで破棄します。
//! 6. **索引への登録**（`crate::registry::DisplaySetRegistry::insert_source`／
//!    `grow_source_items`）。本文は登録しません。
//!
//! レジストリを触るのは 6 だけです。読み込み中に別スレッドから範囲取得できる
//! ようにするため、6 の実行中だけレジストリを借りる形も用意しています
//! （[`register_source_with_access`]。ロックを分割する設計の記録は
//! その doc コメントにあります）。
//!
//! # P08-5: 索引 + オンデマンド読み出しへの移行
//!
//! P06-4 の実測で、複数ファイル合計 2 GB の PERF-006 が不成立と判明
//! しました（本文の全量保持が原因）。本フェーズで、登録経路（本ファイル）を
//! **チャンクごとに一時デコードして解析するだけ**（`crate::line_index` の
//! モジュール doc コメント「本文バッファを保持しない」）に変更し、範囲取得
//! 経路（`crate::registry::DisplaySetRegistry::fetch_range`）を**オンデマンド
//! 読み出し**（ファイルへ都度アクセスしてデコード、有界キャッシュ付き）へ
//! 変更しました。[`load_file_into_registry`]・[`register_source`] は
//! いずれも [`register_source_with_control`] の薄いラッパーです（進捗・
//! キャンセル・伸長のための独自ループは `register_source_with_control` だけが
//! 持ち、`reload_source`・`restore_evicted_source` は一括コミット用の
//! [`stream_decode_and_index`] を共有します）。
//!
//! # プロファイル解決結果と生表示退避（`LOG-022`）の対応
//!
//! | `ResolutionOutcome` | 文字コード | 日時解析 |
//! | --- | --- | --- |
//! | `Manual`／`ExactMatch`／`Glob` | プロファイルの `encoding`／`ansi_codepage` | プロファイルの `datetime_format`。未指定なら自動判定（下記） |
//! | `NoMatch`（該当プロファイルなし） | 自動判定 | 自動判定（下記） |
//! | `Ambiguous`／`ManualNotFound` | 自動判定 | **行わない（生表示へ退避）** |
//!
//! 日時解析の列は、UI での手動書式選択（[`LoadControl::manual_datetime_format`]）
//! が無い場合の挙動です。手動選択があるときの優先順位は下記「日時書式の
//! 決め方」を参照してください。
//!
//! `Ambiguous`（同一優先度の複数 glob が同時に一致）・`ManualNotFound`
//! （指定されたプロファイル名が見つからない）は、プロファイル自体を一意に
//! 決められない状態です。`LOG-022`（貪欲マッチで推測せず、プロファイル選択を
//! 求めるか日時未解析の生表示にする）と同じ思想を、ログ解析プロファイルの
//! 解決そのものにも適用し、**日時解析を試みずに全行を生表示（1行 = 1項目、
//! 日時 `None`）へ退避**します。プロファイル選択 UI（利用者に候補を提示して
//! 選ばせる経路）は P07 の担当です。文字コードだけは読める文字列を返すために
//! 自動判定を使います（真っ白な文字化けより、既定のベストエフォート表示の方が
//! 有用なため）。
//!
//! # 日時書式の決め方（`LOG-DT-001`〜`006`）
//!
//! 書式は次の優先順位で決めます。
//!
//! 1. **UI での手動選択**（[`LoadControl::manual_datetime_format`]、P07 の
//!    「日時書式を選んで再解析」）。設定ファイルを書かずに開いたファイルを、
//!    その場の操作だけで解析させるための経路です。設定より弱いと目的を
//!    果たせないため、2 より優先します
//! 2. **一意に決まったプロファイルの明示指定**
//!    （`hakutaku_config::LogProfileConfig::datetime_format`。`CFG-008`）
//! 3. **自動判定**（下記）
//!
//! ただし 1・2 のいずれよりも、`Ambiguous`／`ManualNotFound` による生表示退避
//! （上表）が優先されます。プロファイルを一意に決められない状態では設定全体を
//! 採用できないためです（`crate::streaming_parse::StreamingAssembler::new` の
//! doc コメント）。
//!
//! どの経路で決まったかは [`LoadSummary::datetime_format_route`] に記録し、
//! 確定した書式と併せて診断ログ（`DIAG-005`）から読み取れるようにしています。
//! 書式の値だけでは、明示指定を誤った状態（指定した書式に
//! 一致しない日時行がすべて継続行へ結合される。`LOG-014`）と自動判定の結果を
//! 切り分けられないためです。書式と経路は
//! [`resolve_datetime_format_and_route`] が同時に決めるため、この優先順位と
//! 診断ログの経路表示が食い違うことはありません。
//!
//! 1 または 2 で書式が決まった場合は、その書式でファイル全体を解析し、
//! **自動判定を一切行いません**（[`register_source_with_control`] が
//! `crate::streaming_parse::StreamingAssembler` へ渡します。2 の取り出しは
//! [`profile_datetime_format`]）。明示指定に一致しない行は、従来どおり直前の
//! 日時付き行の継続行として結合します（`LOG-014`）。
//!
//! 2 は初回登録（[`register_source_with_control`]）と再読み込み・退避復元
//! （[`stream_decode_and_index`]）の両方で同じように効きます。1 は初回登録
//! だけです（[`LoadControl`] を受け取るのがこの経路だけであり、手動選択は
//! `manual_profile` と同じく1回の読み込み要求限りの指定として扱うためです。
//! 再読み込み後も同じ書式で見たい場合は、再解析 UI から選び直すか、
//! プロファイルへ `datetime_format` を書きます）。
//!
//! いずれの明示指定もない場合は、日時書式をファイルの内容から自動判定します。
//!
//! 方式（`crate::streaming_parse::StreamingAssembler`）: ファイル先頭から
//! 最大 [`crate::streaming_parse::DATETIME_AUTO_SCAN_LIMIT`] 行までを順に見て、
//! `parse_datetime_auto` が最初に `NoMatch` 以外を返した行で判定を確定します。
//!
//! - その行が `Matched` なら、その書式（`LogDateTimeFormat`）をファイル全体の
//!   書式として確定し、以降は全行を `parse_datetime_with_format` で解析します
//!   （日時に一致しない行は直前の日時付き行の継続行として結合します。
//!   `LOG-014`）。
//! - その行が `Ambiguous`（`LOG-DT-004` と `LOG-DT-005` の同時成立など）なら、
//!   貪欲に長い方を選ばず**生表示へ退避**します（全行を1行=1項目、日時
//!   `None` として扱う）。
//! - 走査した範囲に日時付き行が1つも見つからなければ（全行 `NoMatch`）、
//!   ファイル全体を「日時なし」として扱います（全行を1行=1項目、日時
//!   `None`。生表示退避とは異なり `LOG-022` の異常系ではなく、単に日時書式を
//!   持たないログとして正常に扱います）。
//!
//! **自動判定だけでは解けない場合:** `LOG-DT-004`
//! （`YYYY/MM/DD HH:mm:ss:SS`）だけで構成されるファイルは、自動判定では必ず
//! `LOG-DT-005` とも同時に成立するため（`crates/parser/src/datetime.rs` の設計。
//! `004` の小数点区切りが常に `:` であり `005` の除外条件に該当しないため）、
//! 常に曖昧判定＝生表示退避になります。どちらの書式で記録されたログなのかは
//! 内容から決められないため、**プロファイルで
//! `datetime_format: LOG-DT-004` を明示するか、生表示になった対象の再解析 UI
//! から `LOG-DT-004` を選べば解析できます**（上記「日時書式の決め方」の 1・2）。
//! 曖昧性検出（`LOG-022`）そのものは変更していません。どちらも指定していない
//! ファイルは、従来どおり生表示へ退避します。
//!
//! # 継続行の結合（`LOG-014`）
//!
//! 日時に一致しない行は、直前に確定した日時付き項目の `raw_text` へ `\n` で
//! 連結します（元の改行を保持したまま1つの論理項目にする）。**直前の日時付き
//! 項目が存在しない場合（ファイル先頭など）は破棄せず、日時未確定の生データ
//! として独立した項目にします。** 直前の項目自体が日時未確定（さらにその前に
//! 日時付き行がない場合）は、それも「直前の日時付き行」ではないため、連続する
//! 日時なし行はそれぞれ独立した項目のままです（結合しません）。
//!
//! # 後続課題（P06／P07／P08 への引き継ぎ）
//!
//! - **`LOG-DT-004` 単独ファイル**（上記「自動判定だけでは解けない場合」）は、
//!   プロファイルの `datetime_format` と、UI からの手動書式選択
//!   （[`LoadControl::manual_datetime_format`]）の両方で解決済みです。
//!   残る制約は、手動選択が再読み込み・退避復元では引き継がれ
//!   ない点だけです（`manual_profile` と同じ扱い。上記「日時書式の決め方」）
//! - **プロファイル選択 UI**（`Ambiguous`／`ManualNotFound` の場合に利用者へ
//!   候補を提示して選ばせる経路）は P07 の担当です。開く際の手動プロファイル
//!   指定そのものを受け取る経路は [`LoadControl::manual_profile`]（P07-2）で
//!   [`register_source_with_control`] へ渡せますが、`load_file_into_registry`・
//!   `register_source`（[`LoadControl::none`] 経由）は引き続き常に生表示へ
//!   退避するだけです
//! - **不正バイト位置の利用者向け表示**（`LoadSummary::decode_invalid_positions`
//!   を実際の UI 表示へつなぐ経路）と、**元バイト列を保持し続けるバッファ設計**
//!   （`crates/format-detection` の doc コメント「後続課題」が P05-6 へ引き継いだ
//!   項目）は、診断ログへの記録までに留め、UI 表示・長期保持は P08 以降の対象
//!   としています
//! - **時系列マージ**（`comparison_key` を用いた複数ソースの統合）は P09 の対象
//!   です。本実装は `Item::comparison_key` を用意するところまでです
//!
//! # P06 での拡張（複数ソースの登録）
//!
//! [`register_source`] は、[`load_file_into_registry`]（単一ソース経路。
//! `src-tauri` の既存呼び出し契約を変えないためそのまま残しています）と同じ
//! 文字コード判定・日時自動判定・継続行結合のロジックを、
//! `crate::budget::SourceBudget` による上限判定（`PERF-004`〜`006`）と
//! [`hakutaku_data_source::FileSnapshot`]（`snapshot_end`。ADR-0007）を通した
//! 読み込みに差し替えて提供します（いずれも [`register_source_with_control`]
//! を呼ぶ薄いラッパーであり、二重実装を避けています）。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hakutaku_format_detection::SelectedEncoding;
use hakutaku_parser::LogDateTimeFormat;

use crate::item::{CapacityEstimate, PendingItem};
use crate::profile_resolution::{resolve_profile, ResolutionOutcome};
use crate::registry::{ChangeKind, DisplaySetHandle, DisplaySetRegistry};

/// 読み込み結果の概要です。`src-tauri` が診断ログ（`DIAG-005`）へ Info／Warn で
/// 記録する材料として使います（`ENC-005`・`LOG-022` の受け入れ条件「判定経路を
/// 診断情報で確認できる」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadSummary {
    pub file_size_bytes: u64,
    pub line_count: u64,
    /// `PERF-010` に従い、行数に比例して常駐する構造（索引本体・行番号配列・
    /// 表示集合の項目列）へ実確保として振り替えた総量（バイト）。
    ///
    /// P08-5 で、読み込みバッファ（生バイト・デコード済み文字列）
    /// を保持しなくなったため、意味を「読み込みバッファの実確保量」から
    /// 「索引の実確保量」へ変更しました。その後の見直しで、会計から漏れていた
    /// 項目列（24バイト/行）を対象へ加えたため、同じ行数でも従来（32バイト/行）
    /// より大きい値になります。
    ///
    /// **事前確保の導入以降、行数 × [`crate::RESIDENT_BYTES_PER_ITEM`] とは一致
    /// しません。** 事前確保（`crate::item::ensure_resident_capacity`）で確保
    /// した容量には、見積もりの誤差分だけ使われない余剰が含まれ、それも実確保
    /// である以上この値に含めるためです。行数から逆算できる値ではなく、実際に
    /// 確保した量（`allocated_bytes` と突き合わせられる量）を示します。
    pub reserved_bytes: usize,
    /// 選択された文字コード判定経路の日本語ラベル（`ENC-005`）。
    pub encoding_route: &'static str,
    /// 実際に選択された文字コード（例: `"utf-8"`、`"windows-932"`）。
    pub selected_encoding: String,
    /// プロファイル解決経路の日本語ラベル（`LOG-021`）。
    pub profile_resolution_route: &'static str,
    /// 確定した日時書式の要件ID（例: `"LOG-DT-001"`）。日時なし、または生表示
    /// 退避の場合は `None`。
    pub detected_datetime_format: Option<&'static str>,
    /// 日時書式の決定経路。`detected_datetime_format` の値が
    /// **どの入力で決まったか**を、`encoding_route` と同じ粒度で表します。
    pub datetime_format_route: DatetimeFormatRoute,
    /// デコードできなかったバイト列の位置一覧（`bytes` の先頭からの絶対
    /// オフセット。上限 `hakutaku_format_detection::MAX_INVALID_POSITIONS` 件）。
    /// 空なら不正バイトなし。
    pub decode_invalid_positions: Vec<usize>,
    /// `decode_invalid_positions` が上限に達し、以降の位置を打ち切ったか。
    pub decode_invalid_positions_truncated: bool,
    /// 文字コード判定中に検出した警告（BOM と明示指定の矛盾など）の日本語
    /// メッセージ一覧。空なら警告なし。
    pub encoding_warnings: Vec<String>,
    /// `LOG-022` により日時未解析の生表示へ退避したか（プロファイル解決の
    /// 曖昧性、または日時書式自動判定の曖昧性のいずれか）。
    pub fell_back_to_raw_display: bool,
    /// 読み込んだ範囲（`snapshot_end`）の末尾が改行で終わらず、最終行が未確定
    /// 行になっているか（`LOG-026`）。断片は破棄されず通常の項目として保持
    /// されており、これは表示上の区別のための付随情報です（解析エラーでは
    /// ない）。
    pub has_unconfirmed_trailing_line: bool,
    /// この読み込みの段階別所要時間。[`LoadStageTimings`] の
    /// doc コメントに、境界の置き方と読み方があります。
    ///
    /// **診断ログ（`DIAG-005`）へは出しません。** 実測用の値であり、利用者向けの
    /// 記録ではないためです（`src-tauri` の読み込みサマリー出力は文字コード・
    /// プロファイル・日時書式の判定経路だけを扱います）。
    pub stage_timings: LoadStageTimings,
}

/// 読み込み1回を段階へ分けた累計時間です（`PERF-009` の分析用）。
///
/// # なぜこの4段階なのか
///
/// 読み込みが遅いときに次の一手を決めるには、「どの段階が支配的か」だけ分かれば
/// 足ります。段階の切れ目は、**別々の最適化手段が対応する境界**へ置いています。
///
/// | 段階 | 対応する手段 |
/// | --- | --- |
/// | [`Self::io_read`] | チャンクサイズ、読み取り方式、先読み |
/// | [`Self::decode`] | 文字コード判定・デコード実装（`crates/format-detection`） |
/// | [`Self::parse`] | 行分割・日時解析・継続行結合（`crates/parser`、`crate::streaming_parse`） |
/// | [`Self::deliver`] | 索引・項目列の構築とメモリ会計（`crate::registry`、`crate::item`） |
///
/// これより細かく割っても、対応する手段が同じなら判断は変わりません。逆に
/// まとめてしまうと、たとえば「デコードが重いのか日時解析が重いのか」が分から
/// ず、次の一手を選べません。
///
/// # オーバーヘッドが無視できる根拠
///
/// [`Instant::now`] の対は**チャンク境界にだけ**置いています（既定の 8 MiB
/// チャンクなら 1 GiB のファイルで128回のループ、1ループあたり4組程度）。
/// 1組が数十ナノ秒なので、1 GiB の読み込み全体で数十マイクロ秒にしかなりません。
/// 秒の桁である読み込み時間に対して10万分の1未満であり、release ビルドで常時
/// 有効にしても実測に現れません。
///
/// **行ごとには決して計りません。** 300万行のファイルで行ごとに計ると、それだけ
/// で数百ミリ秒（計測対象そのものと同じ桁）を足すことになり、内訳が計測行為に
/// よって歪みます。そのため、行単位で回る処理（`feed_line` 群）は**ループ全体を
/// 1回で囲みます**。
///
/// # 合計との関係
///
/// [`Self::total`] は登録関数の入口から出口までの実時間で、4段階の合計とは
/// 一致しません。差分（[`Self::other`]）には、ファイルのオープンとスナップ
/// ショット取得、上限判定（`PERF-004`〜`006`）、プロファイル解決、チャンク
/// ごとの整合性再確認（`LOG-023`）、抑制の待機（`PERF-014`）、進捗通知、
/// 要約の組み立てが入ります。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadStageTimings {
    /// 登録関数の入口から出口まで（段階の合計ではなく実時間）。
    pub total: Duration,
    /// I/O。チャンクの読み取り（`File::read_exact`）だけの累計
    /// （[`hakutaku_data_source::ChunkReadSummary::read_elapsed`]）。
    ///
    /// 途中で失敗・変更検知により打ち切った場合は `0` になります（打ち切りの
    /// 経路では [`hakutaku_data_source::ChunkReadSummary`] が返らないため）。
    /// 段階別内訳は成功した読み込みを分析するためのものであり、この欠落を
    /// 補うために計測点を増やすことはしていません。
    pub io_read: Duration,
    /// デコード。文字コード判定（`ENC-005`）と、チャンクごとの確定デコード
    /// （`DecodeCursor::consume_and_decode`）の累計。
    pub decode: Duration,
    /// 解析。生バイト・デコード後の行分割と、日時解析・継続行結合
    /// （`crate::streaming_parse::StreamingAssembler`）の累計。
    pub parse: Duration,
    /// バッチ登録。**レジストリを借りている区間すべて**の累計です（チャンク
    /// ごとの `deliver_batch` と、読み込み終了時の最終確定）。
    ///
    /// 途中のバッチ登録だけに限らないのは、[`register_source_with_access`] で
    /// この値が「GUI 層のロックが実際に保持された時間の合計」と一致するように
    /// するためです（バッチ境界でロックを取り直す経路。最終確定には統合表示の
    /// 同期が入り、実測ではそこだけで数百ミリ秒に達します）。借用の外で起きる
    /// ことを混ぜない限り、この一致は保たれます。
    pub deliver: Duration,
}

impl LoadStageTimings {
    /// [`Self::total`] から4段階の合計を引いた残りです（doc コメント「合計との
    /// 関係」の内訳）。
    ///
    /// 引き算が負になり得ないよう飽和減算します。段階の計測は互いに重ならない
    /// 区間なので理論上は負になりませんが、計時の丸めで下回った場合に
    /// パニックさせる価値はありません。
    #[must_use]
    pub fn other(&self) -> Duration {
        self.total
            .saturating_sub(self.io_read)
            .saturating_sub(self.decode)
            .saturating_sub(self.parse)
            .saturating_sub(self.deliver)
    }
}

/// 日時書式が**どの入力で決まったか**を表します。
///
/// [`LoadSummary::detected_datetime_format`]（確定した書式そのもの）だけでは、
/// 明示指定を誤った状態（指定した書式に一致しない日時行がすべて継続行へ結合
/// される。`LOG-014`）と、内容からの自動判定の結果を切り分けられません。
/// 文字コードの [`LoadSummary::encoding_route`] と同じ粒度で経路も残し、
/// 診断ログ（`DIAG-005`）だけで切り分けられるようにします。
///
/// 各値はモジュール doc コメント「日時書式の決め方」の優先順位に対応します。
/// 書式と同じ場所（`resolve_datetime_format_and_route`）で決めるため、優先順位
/// の実装と食い違いません。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatetimeFormatRoute {
    /// UI での手動選択（[`LoadControl::manual_datetime_format`]。優先順位1）で
    /// 決まった。この経路は初回登録でだけ現れます（再読み込み・退避復元は
    /// [`LoadControl`] を受け取らないため。モジュール doc コメント「日時書式の
    /// 決め方」）。
    Manual,
    /// 一意に決まったプロファイルの `datetime_format` 明示指定（`CFG-008`。
    /// 優先順位2）で決まった。
    Profile,
    /// 明示指定がなく、ファイルの内容から自動判定した（優先順位3）。
    ///
    /// 自動判定が曖昧（`LOG-022`）だった場合や、走査範囲に日時付き行が1つも
    /// 無かった場合もこの経路です（いずれも
    /// [`LoadSummary::detected_datetime_format`] が `None` になり、両者は
    /// [`LoadSummary::fell_back_to_raw_display`] で区別できます）。
    Auto,
    /// プロファイルを一意に決められず（`Ambiguous`／`ManualNotFound`）生表示へ
    /// 退避したため、書式の決定そのものを行わなかった。
    ///
    /// 明示指定があっても採用しません（モジュール doc コメント「日時書式の
    /// 決め方」の「ただし」）。読み手にとっては「書式が決まらなかった」ではなく
    /// 「決めに行っていない」ことを意味し、次の一手はプロファイルの解決
    /// （[`LoadSummary::profile_resolution_route`]）の是正です。
    RawDisplayFallback,
}

impl DatetimeFormatRoute {
    /// 診断ログ（`DIAG-005`）表示用に、決定経路を短い日本語ラベルで返します
    /// （`encoding_route_label`・
    /// [`ResolutionOutcome::route_label`](crate::profile_resolution::ResolutionOutcome::route_label)
    /// と同じ文体）。
    #[must_use]
    pub fn route_label(&self) -> &'static str {
        match self {
            DatetimeFormatRoute::Manual => "UI での手動選択",
            DatetimeFormatRoute::Profile => "プロファイル指定（datetime_format）",
            DatetimeFormatRoute::Auto => "内容からの自動判定",
            DatetimeFormatRoute::RawDisplayFallback => "判定なし（生表示退避）",
        }
    }
}

/// ファイル読み込みから表示集合登録までの失敗です。
#[derive(Debug)]
pub enum LoadFileError {
    /// データソース層の読み込み失敗（`PERF-010` の予約拒否を含む）。
    ReadFile(hakutaku_data_source::ReadFileError),
    /// UTF-16 LE/BE の BOM を検出した（`ENC-006`。初期リリースでは未対応）。
    UnsupportedEncoding(hakutaku_format_detection::UnsupportedEncoding),
    /// プロファイルの `encoding` 名前指定を解釈できなかった
    /// （`'utf-8'`／`'windows-<コードページ番号>'` 以外）。
    InvalidEncodingName(hakutaku_format_detection::InvalidEncodingNameError),
    /// 指定された Windows コードページが実行環境に存在しない。
    Decode(hakutaku_format_detection::DecodeError),
    /// P06-2: チャンク読み込みの途中（まだ表示集合へ何も登録していない時点）で
    /// ファイルの変更を検知した（`LOG-023`）。既に登録済みの表示集合が存在する
    /// 場合はこのエラーにはならず、[`RegisterSourceOutcome`] を通じて
    /// [`crate::notification::TaskOutcome::Failed`] として報告されます
    /// （`register_source_with_control` の doc コメント参照）。
    ChangedDuringLoad(hakutaku_data_source::SnapshotVerdict),
    /// P08-5: 索引の伸長分のメモリ予約が拒否された（`PERF-008`）。
    /// `crate::budget::SourceBudget`（`PERF-006`、開いているファイルの合計
    /// サイズ）の拒否とは別の理由・メッセージで区別します。
    IndexMemoryBudgetExceeded(hakutaku_memory_accounting::ReservationRejected),
}

impl std::fmt::Display for LoadFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadFileError::ReadFile(error) => write!(f, "ログファイルを読み込めません: {error}"),
            LoadFileError::UnsupportedEncoding(unsupported) => {
                let bom_label = match unsupported.bom {
                    hakutaku_format_detection::Utf16BomKind::Le => "UTF-16 LE",
                    hakutaku_format_detection::Utf16BomKind::Be => "UTF-16 BE",
                };
                write!(
                    f,
                    "{bom_label} の BOM を検出しました。UTF-16 は初期リリースでは未対応の\
                     文字コードです（ENC-006）。"
                )
            }
            LoadFileError::InvalidEncodingName(error) => {
                write!(f, "プロファイルの文字コード指定を解釈できません: {error}")
            }
            LoadFileError::Decode(error) => {
                write!(f, "文字コードのデコードに失敗しました: {error}")
            }
            LoadFileError::ChangedDuringLoad(verdict) => {
                write!(f, "読み込み中にファイルの変更を検知しました: {verdict:?}")
            }
            LoadFileError::IndexMemoryBudgetExceeded(rejected) => {
                write!(
                    f,
                    "索引のためのメモリ予約に失敗しました（メモリ予算の上限に達しています。\
                     PERF-008）: {rejected}"
                )
            }
        }
    }
}

impl std::error::Error for LoadFileError {}

impl From<hakutaku_data_source::ReadFileError> for LoadFileError {
    fn from(error: hakutaku_data_source::ReadFileError) -> Self {
        LoadFileError::ReadFile(error)
    }
}

impl LoadFileError {
    /// 共有違反（`LOG-027`）による失敗かどうかを返します。呼び出し側
    /// （`src-tauri`）が利用者向けエラーの理由・次操作を使い分けるために
    /// 使います。
    #[must_use]
    pub fn is_sharing_violation(&self) -> bool {
        matches!(self, LoadFileError::ReadFile(inner) if inner.is_sharing_violation())
    }
}

/// [`register_source`] の失敗です（P06）。
#[derive(Debug)]
pub enum RegisterSourceError {
    /// 上限判定（compare-and-reserve）で拒否された（`PERF-004`〜`006`）。
    /// `registry`・`budget` の状態は変更されていません。
    BudgetRejected(crate::budget::BudgetRejection),
    /// 上限判定を通過した後の読み込み・解析エラー（[`LoadFileError`] と同じ
    /// 内訳）。この場合も上限判定で確保した予約は返却済みです。
    Load(LoadFileError),
}

impl std::fmt::Display for RegisterSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterSourceError::BudgetRejected(rejection) => write!(f, "{rejection}"),
            RegisterSourceError::Load(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RegisterSourceError {}

impl RegisterSourceError {
    /// 共有違反（`LOG-027`）による失敗かどうかを返します。
    #[must_use]
    pub fn is_sharing_violation(&self) -> bool {
        matches!(self, RegisterSourceError::Load(inner) if inner.is_sharing_violation())
    }
}

/// ファイルを読み込み、`LOG-021` のプロファイル解決・`ENC-005` の文字コード
/// 判定・6書式の日時自動判定・`LOG-014` の継続行結合を経て、単一ソースの表示
/// 集合として `registry` へ登録します（P04-1 の単一ファイル・単一ソースという
/// 前提は変えていません。複数ファイルの同時読み込みは P06 の対象）。
///
/// `source_label` はフロントエンドへ表示する来歴ラベルです（`SEC-012` により、
/// 呼び出し側はここへ絶対パスではなくファイル名などの表示用文字列を渡して
/// ください）。
///
/// `log_profiles` は設定（`hakutaku.yaml` の `log_profiles`）から読み込んだ
/// プロファイル一覧です。手動プロファイル選択は現段階では未実装のため、
/// 常に `resolve_profile(None, path, log_profiles)` で解決します（開く際に
/// ユーザーがプロファイルを選ぶ UI は P07 の担当）。
pub fn load_file_into_registry(
    registry: &mut DisplaySetRegistry,
    path: &Path,
    source_label: String,
    log_profiles: &[hakutaku_config::LogProfileConfig],
) -> Result<(DisplaySetHandle, LoadSummary), LoadFileError> {
    // P08-5: 本文を保持しないストリーミング登録
    // （`register_source_with_control`）へ一本化しました。この経路は
    // `PERF-004`〜`006`（ファイル数・サイズ上限）を課さない P04 時代の契約を
    // 保つため、実質無制限の `SourceBudget` を一時的に使います
    // （`crate::budget::SourceBudget` は「開いているファイルの合計サイズ」の
    // 判定用であり、このソースの寿命を追跡するものではないため、呼び出しの
    // 内側だけで使い捨てても問題ありません）。
    let budget = crate::budget::SourceBudget::with_limits(u64::MAX, u64::MAX, usize::MAX);
    let control = LoadControl::none();
    match register_source_with_control(
        registry,
        &budget,
        path,
        source_label,
        log_profiles,
        &control,
    ) {
        Ok(outcome) => Ok((outcome.handle, outcome.summary)),
        Err(RegisterSourceError::BudgetRejected(rejection)) => {
            // 実質無制限の上限のため到達しないはずだが、型として
            // LoadFileError へ変換する必要がある（防御的な経路）。
            Err(LoadFileError::ReadFile(
                hakutaku_data_source::ReadFileError::Io {
                    reason: format!("内部エラー: 想定外の上限拒否が発生しました: {rejection}"),
                },
            ))
        }
        Err(RegisterSourceError::Load(error)) => Err(error),
    }
}

/// 複数ソースの1つとしてファイルを読み込み、`registry` へ登録します（P06。
/// `tasks/phase-06-large-file-loading.md` 作業項目3「複数ソースの登録」）。
///
/// [`load_file_into_registry`] との違いは次の2点です。
///
/// 1. 登録前に `budget`（[`crate::budget::SourceBudget`]）で上限判定
///    （compare-and-reserve、`PERF-004`〜`006`）を行います。拒否された場合は
///    `registry` の状態を一切変更しません（既に開いているソースの表示は
///    維持されます）。
/// 2. ファイルは [`hakutaku_data_source::FileSnapshot`] を取り、
///    `snapshot_end` を上限に読み込みます（ADR-0007）。このスナップショットは
///    以後の変更検知（[`DisplaySetRegistry::refresh_source`]）にも使われます。
///
/// # 実装順序について（計画正本との対応）
///
/// 計画正本は上限判定を「上限判定 → スナップショット → 読み込み」の順で
/// 説明していますが、本実装は「スナップショット取得 → 上限判定 → 読み込み」
/// の順です。ファイル識別子を含むスナップショットの取得自体がファイルを開く
/// ことを要求するため（`GetFileInformationByHandle` はハンドルが必要）、
/// 上限判定に使うサイズを別途 stat で先に得る設計にすると、2回の観測（先行の
/// stat と後続のスナップショット）が食い違う余地（TOCTOU）が生まれます。
/// **上限判定に使うサイズと読み込み境界（`snapshot_end`）を同じ1回の観測に
/// 揃えるため**、ファイルを開く操作そのものを先に行い、その1回の観測結果を
/// 両方の目的に使っています。上限超過時は読み込み（バイト列の確保）へ進まない
/// ため、「そのファイルを開かず」という意図（大きな読み込みバッファを確保
/// しない、ソースとして登録・解析しない）は保たれています。
pub fn register_source(
    registry: &mut DisplaySetRegistry,
    budget: &crate::budget::SourceBudget,
    path: &Path,
    source_label: String,
    log_profiles: &[hakutaku_config::LogProfileConfig],
) -> Result<(DisplaySetHandle, LoadSummary), RegisterSourceError> {
    // P08-5: 本文を保持しないストリーミング登録
    // （`register_source_with_control`）を、進捗・キャンセル・抑制のいずれも
    // 使わない設定で呼び出す薄いラッパーです。
    //
    // `register_source_with_control` は、既に一部を登録済みの状態で読み込み
    // 中に失敗した場合、`Err` ではなく `Ok(RegisterSourceOutcome{ outcome:
    // TaskOutcome::Failed(_), .. })` を返します（登録済みの表示を消さない
    // `ERR-001` の方針）。この関数の戻り値の型（`Result<(handle, summary),
    // RegisterSourceError>`）はその区別を表現できないため、`Failed` は
    // `Err(RegisterSourceError::Load(_))` へ変換します。**この場合でも
    // `registry` には（壊れた状態としてマークされた）ソースが残る**という、
    // 全件一括読み込み時代の「拒否時は状態を一切変更しない」契約からの
    // 差分があります（GB級ファイルの読み込み中に変更を検知した場合など、
    // まれな経路です。`crate::loader` の後続課題として報告します）。
    let control = LoadControl::none();
    let outcome =
        register_source_with_control(registry, budget, path, source_label, log_profiles, &control)?;

    match outcome.outcome {
        crate::notification::TaskOutcome::Completed
        | crate::notification::TaskOutcome::Cancelled => Ok((outcome.handle, outcome.summary)),
        crate::notification::TaskOutcome::Failed(user_facing_error) => {
            Err(RegisterSourceError::Load(LoadFileError::ReadFile(
                hakutaku_data_source::ReadFileError::Io {
                    reason: user_facing_error.to_string(),
                },
            )))
        }
    }
}

/// [`reload_source`] の結果です（`LOG-028`、ADR-0007。
/// `tasks/phase-06-large-file-loading.md` 作業項目8）。
#[derive(Debug, Clone)]
pub enum ReloadOutcome {
    /// 最新状態を反映しました（純粋な追記の反映、または変化なしの再確認）。
    /// 追記があった場合は世代が1つ進み、変化がなければ元の世代のままです。
    Reloaded {
        generation: u64,
        total_items: u64,
        /// `LOG-022`: この再読み込みで表示集合を作り直した場合の、**作り直した
        /// 結果**の生表示退避の有無です。
        ///
        /// 再読み込みは `resolve_profile(None, ..)` から解析をやり直すため、
        /// 初回オープン時の手動指定（`LoadControl::manual_profile`／
        /// `manual_datetime_format`）は引き継がれず、生表示退避の有無が初回と
        /// 変わることがあります。呼び出し側が実際の表示に合わせて状態を更新
        /// できるよう、判定し直した値をここで返します。
        ///
        /// `None` は「表示集合を作り直していないため判定し直していない」
        /// （変化なしの再確認）を意味し、呼び出し側は直前の値を据え置きます。
        fell_back_to_raw_display: Option<bool>,
    },
    /// 再読み込み後の見込み合計が上限（`PERF-004`〜`006`）を超えるため、
    /// **再読み込み全体を拒否しました**（ADR-0007）。旧スナップショットの
    /// 表示は維持され、対象のソース状態には「更新未反映」フラグが立ちます
    /// （[`crate::registry::SourceSummary::update_pending`]）。
    RejectedOverLimit(crate::budget::BudgetRejection),
    /// 削除・縮小・置換を検知し、`LOG-023` どおり索引を無効化しました
    /// （従来の索引を有効扱いで維持しません）。
    Changed(ChangeKind),
    /// 共有を許可しない方法で開かれていて読み取れませんでした（`LOG-027`。
    /// 再試行可能）。旧スナップショットの表示は維持されます。
    SharingViolation,
    /// 上記以外の読み込み・解析エラーです。旧スナップショットの表示は維持
    /// されます。
    Failed(crate::notification::UserFacingError),
}

/// 利用者の明示的な指示による再読み込みです（`LOG-028`。
/// `tasks/phase-06-large-file-loading.md` 作業項目8、ADR-0007）。
///
/// `source_id` が未登録（未登録・close 済み）の場合は `None` を返します。
///
/// # 手順（ADR-0007「決定」の表と対応）
///
/// 1. `path` を再オープンし、新しいスナップショットを取ります
///    （[`hakutaku_data_source::reopen_for_reload`]）。削除は
///    [`ReloadOutcome::Changed`]`(`[`ChangeKind::Deleted`]`)`、共有違反は
///    [`ReloadOutcome::SharingViolation`] として区別します（`LOG-023`・
///    `LOG-027`）。
/// 2. 旧スナップショットと比較し（[`hakutaku_data_source::compare_snapshots`]）、
///    縮小・置換を検知した場合は `LOG-023` どおり索引を無効化します
///    （従来の索引を有効扱いで維持しません。ADR-0007「3番目の行」の注意）。
/// 3. 変化なしなら、何も読み直さず現在の世代・件数をそのまま返します。
/// 4. 追記なら、上限判定（[`crate::budget::SourceBudget::try_replace`]。
///    compare-and-reserve）を行います。拒否された場合は
///    [`ReloadOutcome::RejectedOverLimit`] を返し、**旧スナップショットの
///    表示を維持したまま**「更新未反映」フラグを立てます（部分読み込みは
///    採りません。ADR-0007）。
/// 5. 上限内なら、`snapshot_end`（新しいスナップショット）を上限に開き直し、
///    新しい表示集合を構築して世代を進めます（`LOG-010`：ここでも
///    `snapshot_end` より先には追記されても読みません）。読み込み・解析の
///    途中で失敗した場合は、上限判定で確保した予約を旧サイズへ戻し、旧
///    スナップショットの表示を維持したまま [`ReloadOutcome::Failed`] を
///    返します。
///
/// リアルタイム追従は行いません（`LOG-010`）。呼び出し元が明示的にこの
/// 関数を呼んだときだけ再読み込みします。
pub fn reload_source(
    registry: &mut DisplaySetRegistry,
    budget: &crate::budget::SourceBudget,
    source_id: u32,
    log_profiles: &[hakutaku_config::LogProfileConfig],
) -> Option<ReloadOutcome> {
    let ctx = registry.reload_context(source_id)?;

    // 1. 再オープンして新しいスナップショットを取る（削除・共有違反はここで
    //    区別する）。
    let (file, new_snapshot) = match hakutaku_data_source::reopen_for_reload(&ctx.path) {
        Ok(pair) => pair,
        Err(hakutaku_data_source::ReopenForReloadError::Deleted) => {
            registry.mark_changed_now(source_id, ChangeKind::Deleted);
            return Some(ReloadOutcome::Changed(ChangeKind::Deleted));
        }
        Err(hakutaku_data_source::ReopenForReloadError::SharingViolation { .. }) => {
            registry.mark_sharing_violation_now(source_id);
            return Some(ReloadOutcome::SharingViolation);
        }
        Err(hakutaku_data_source::ReopenForReloadError::Io { reason }) => {
            registry.mark_error_now(source_id, reason.clone());
            return Some(ReloadOutcome::Failed(reload_user_facing_error(
                &ctx.label, reason,
            )));
        }
    };

    // 2. 旧スナップショットと比較する。
    match hakutaku_data_source::compare_snapshots(&ctx.old_snapshot, &new_snapshot) {
        hakutaku_data_source::SnapshotVerdict::Replaced => {
            registry.mark_changed_now(source_id, ChangeKind::Replaced);
            Some(ReloadOutcome::Changed(ChangeKind::Replaced))
        }
        hakutaku_data_source::SnapshotVerdict::Shrunk { .. } => {
            registry.mark_changed_now(source_id, ChangeKind::Shrunk);
            Some(ReloadOutcome::Changed(ChangeKind::Shrunk))
        }
        // reopen_for_reload が成功した直後の比較のため、通常は到達しない
        // （削除は reopen 失敗として既に処理済み）。安全側（索引無効化）に
        // 倒す（`change_kind_from_verdict` と同じ防御方針）。
        hakutaku_data_source::SnapshotVerdict::Deleted => {
            registry.mark_changed_now(source_id, ChangeKind::Deleted);
            Some(ReloadOutcome::Changed(ChangeKind::Deleted))
        }
        // 3. 変化なし: 何も読み直さない（LOG-010: リアルタイム追従はしないが、
        //    明示的な再読み込みで「変化がない」ことを確認できるのは自然）。
        hakutaku_data_source::SnapshotVerdict::Unchanged => {
            registry.clear_update_pending(source_id);
            let handle = registry
                .current_handle(source_id)
                .expect("直前の reload_context で存在確認済み");
            Some(ReloadOutcome::Reloaded {
                generation: handle.generation,
                total_items: handle.total_items,
                // 何も読み直していない（表示集合はそのまま）ため、生表示退避の
                // 判定もやり直していない。呼び出し側は直前の値を据え置く。
                fell_back_to_raw_display: None,
            })
        }
        // 4〜5. 追記: 上限判定してから開き直す。
        hakutaku_data_source::SnapshotVerdict::Appended { .. } => Some(reload_appended(
            registry,
            budget,
            source_id,
            &ctx,
            file,
            new_snapshot,
            log_profiles,
        )),
    }
}

/// [`reload_source`] の「追記」分岐（手順4〜5）です。関数を分け、
/// `reload_source` 本体の分岐を読みやすくしています。
fn reload_appended(
    registry: &mut DisplaySetRegistry,
    budget: &crate::budget::SourceBudget,
    source_id: u32,
    ctx: &crate::registry::SourceReloadContext,
    file: std::fs::File,
    new_snapshot: hakutaku_data_source::FileSnapshot,
    log_profiles: &[hakutaku_config::LogProfileConfig],
) -> ReloadOutcome {
    // 4. 上限判定（compare-and-reserve。ADR-0007「アプリ内の合計サイズの
    //    判定と予約」だけが原子的）。
    let new_reservation = match budget.try_replace(ctx.old_reservation, new_snapshot.snapshot_end) {
        Ok(reservation) => reservation,
        Err(rejection) => {
            // 部分読み込みは採らない。旧スナップショットの表示を維持したまま
            // 「更新未反映」フラグを立てるだけで、registry の snapshot・
            // reservation・items はいずれも変更しない。
            registry.mark_update_pending(source_id);
            return ReloadOutcome::RejectedOverLimit(rejection);
        }
    };

    // 5. 上限内。開き直して最新状態を反映する（P08-5、本文を保持しない
    //    ストリーミング解析。`stream_decode_and_index` に共通化）。
    let streamed = match stream_decode_and_index(file, &ctx.path, &new_snapshot, log_profiles) {
        Ok(streamed) => streamed,
        Err(error) => {
            return reload_revert_and_fail(
                registry,
                budget,
                source_id,
                ctx,
                new_reservation,
                error,
            );
        }
    };

    let outcome = match registry.commit_reload(
        source_id,
        &streamed.pending_items,
        new_snapshot,
        new_reservation,
        streamed.has_unconfirmed_trailing_line,
        streamed.datetime_format,
        streamed.selected_encoding,
    ) {
        Ok(Some(outcome)) => outcome,
        Ok(None) => unreachable!("直前の reload_context で存在確認済み"),
        Err(rejected) => {
            return reload_revert_and_fail(
                registry,
                budget,
                source_id,
                ctx,
                new_reservation,
                LoadFileError::IndexMemoryBudgetExceeded(rejected),
            );
        }
    };

    ReloadOutcome::Reloaded {
        generation: outcome.generation,
        total_items: outcome.total_items,
        // 表示集合を作り直したので、生表示退避の有無も作り直した結果で返す。
        fell_back_to_raw_display: Some(streamed.fell_back_to_raw_display),
    }
}

/// [`reload_appended`] の読み込み・解析失敗を共通処理します。上限判定で
/// 確保した新しい予約を旧サイズへ戻し（旧スナップショットの表示を維持する
/// ため。ADR-0007）、ソース状態を更新して [`ReloadOutcome::Changed`]／
/// [`ReloadOutcome::SharingViolation`]／[`ReloadOutcome::Failed`] のいずれかを
/// 返します。
fn reload_revert_and_fail(
    registry: &mut DisplaySetRegistry,
    budget: &crate::budget::SourceBudget,
    source_id: u32,
    ctx: &crate::registry::SourceReloadContext,
    new_reservation: crate::budget::SourceReservation,
    error: LoadFileError,
) -> ReloadOutcome {
    // 旧サイズへ縮小する置き換えは、旧予約が既に有効だった以上ほぼ必ず
    // 成功する（合計は減る方向のため）。万一失敗しても、読み込み自体は
    // 破棄する（新しい内容を反映しない）ことに変わりはない。
    let _ = budget.try_replace(new_reservation, ctx.old_reservation.reserved_bytes);

    if let LoadFileError::ChangedDuringLoad(verdict) = error {
        let kind = change_kind_from_verdict(verdict);
        registry.mark_changed_now(source_id, kind);
        return ReloadOutcome::Changed(kind);
    }

    if error.is_sharing_violation() {
        registry.mark_sharing_violation_now(source_id);
        return ReloadOutcome::SharingViolation;
    }

    let message = error.to_string();
    registry.mark_error_now(source_id, message.clone());
    ReloadOutcome::Failed(reload_user_facing_error(&ctx.label, message))
}

/// [`reload_source`] が失敗を [`crate::notification::UserFacingError`]
/// （`ERR-002` の5要素）へ変換する共通処理です。
fn reload_user_facing_error(label: &str, reason: String) -> crate::notification::UserFacingError {
    crate::notification::UserFacingError::new(
        label.to_string(),
        reason,
        "対象を閉じてから再試行してください。",
    )
}

/// [`crate::registry::DisplaySetRegistry::evict_inactive_sources`] が解放
/// （[`crate::registry::SourceStatus::Evicted`]）したソースを、再アクセス時に
/// 透過的に復元します（P08-3）。
///
/// # 呼び出しタイミング
///
/// `src-tauri` 側が、`fetch_log_range`（またはタブ切り替え相当の操作）の入口
/// で、対象ソースの状態が `Evicted` であることを確認した場合に呼び出す想定
/// です。しきい値到達時の解放ハンドラそのものからは**呼びません**（デッド
/// ロック回避のための遅延方式。`crate::registry::DisplaySetRegistry::
/// evict_inactive_sources` の doc コメント「呼び出しタイミング」を参照）。
///
/// # 手順（`crate::loader::reload_source` と対になる設計）
///
/// 1. `path` を再オープンし、新しいスナップショットを取ります
///    （[`hakutaku_data_source::reopen_for_reload`]）。
/// 2. 解放前に記録済みの旧スナップショットと比較します
///    （[`hakutaku_data_source::compare_snapshots`]）。
///    - **変化なし（`Unchanged`）の場合だけ**「復元」として扱います。ファイル
///      を再読み込みし、[`crate::registry::DisplaySetRegistry::commit_restore`]
///      で表示集合を再構築します（世代は必ず1つ進みます。同メソッドの doc
///      コメント「世代は必ず1つ進めます」を参照）。
///    - それ以外（追記・縮小・置換）は、安全側に倒して `LOG-023` の無効化
///      経路（`registry.mark_changed_now`）を使います。解放中に発生した追記
///      分を「復元」の名目で静かに取り込むと、`ADR-0007` が定める
///      compare-and-reserve（`SourceBudget` の上限判定）を経由しない容量増加
///      を許してしまうため、本セッションでは意図的に対象外としています
///      （利用者は `LOG-023` と同じ「対象を閉じてから開き直す」操作で追記分を
///      反映できます。P08 の後続課題として、追記だけは `reload_source` と
///      同じ compare-and-reserve 経路を踏んでから復元する拡張が考えられます）。
/// 3. 削除・共有違反・その他の I/O エラーは、`reload_source` と同じ経路
///    （`mark_changed_now`／`mark_sharing_violation_now`／`mark_error_now`）で
///    扱います。
///
/// `source_id` が未登録（未登録・close 済み）の場合は `None` を返します。
pub fn restore_evicted_source(
    registry: &mut DisplaySetRegistry,
    source_id: u32,
    log_profiles: &[hakutaku_config::LogProfileConfig],
) -> Option<ReloadOutcome> {
    let ctx = registry.reload_context(source_id)?;

    let (file, new_snapshot) = match hakutaku_data_source::reopen_for_reload(&ctx.path) {
        Ok(pair) => pair,
        Err(hakutaku_data_source::ReopenForReloadError::Deleted) => {
            registry.mark_changed_now(source_id, ChangeKind::Deleted);
            return Some(ReloadOutcome::Changed(ChangeKind::Deleted));
        }
        Err(hakutaku_data_source::ReopenForReloadError::SharingViolation { .. }) => {
            registry.mark_sharing_violation_now(source_id);
            return Some(ReloadOutcome::SharingViolation);
        }
        Err(hakutaku_data_source::ReopenForReloadError::Io { reason }) => {
            registry.mark_error_now(source_id, reason.clone());
            return Some(ReloadOutcome::Failed(reload_user_facing_error(
                &ctx.label, reason,
            )));
        }
    };

    match hakutaku_data_source::compare_snapshots(&ctx.old_snapshot, &new_snapshot) {
        hakutaku_data_source::SnapshotVerdict::Unchanged => {
            let streamed =
                match stream_decode_and_index(file, &ctx.path, &new_snapshot, log_profiles) {
                    Ok(streamed) => streamed,
                    Err(error) => return Some(restore_fail(registry, source_id, &ctx, error)),
                };

            let outcome = match registry.commit_restore(
                source_id,
                &streamed.pending_items,
                new_snapshot,
                streamed.has_unconfirmed_trailing_line,
            ) {
                Ok(Some(outcome)) => outcome,
                Ok(None) => unreachable!("直前の reload_context で存在確認済み"),
                Err(rejected) => {
                    return Some(restore_fail(
                        registry,
                        source_id,
                        &ctx,
                        LoadFileError::IndexMemoryBudgetExceeded(rejected),
                    ));
                }
            };

            Some(ReloadOutcome::Reloaded {
                generation: outcome.generation,
                total_items: outcome.total_items,
                // 復元でも表示集合を作り直している（reload_source と同じく
                // resolve_profile(None, ..) からやり直す）ため、生表示退避の
                // 有無も作り直した結果で返す。
                fell_back_to_raw_display: Some(streamed.fell_back_to_raw_display),
            })
        }
        verdict => {
            // 追記・縮小・置換はいずれも安全側（索引無効化）で扱う（doc
            // コメント「手順」参照）。
            let kind = change_kind_from_verdict_for_restore(verdict);
            registry.mark_changed_now(source_id, kind);
            Some(ReloadOutcome::Changed(kind))
        }
    }
}

/// [`restore_evicted_source`] の読み込み・解析失敗を共通処理します。
fn restore_fail(
    registry: &mut DisplaySetRegistry,
    source_id: u32,
    ctx: &crate::registry::SourceReloadContext,
    error: LoadFileError,
) -> ReloadOutcome {
    if let LoadFileError::ChangedDuringLoad(verdict) = error {
        let kind = change_kind_from_verdict(verdict);
        registry.mark_changed_now(source_id, kind);
        return ReloadOutcome::Changed(kind);
    }
    if error.is_sharing_violation() {
        registry.mark_sharing_violation_now(source_id);
        return ReloadOutcome::SharingViolation;
    }
    let message = error.to_string();
    registry.mark_error_now(source_id, message.clone());
    ReloadOutcome::Failed(reload_user_facing_error(&ctx.label, message))
}

/// [`hakutaku_data_source::SnapshotVerdict`] を復元経路用の [`ChangeKind`] へ
/// 変換します。`Appended` も含め、`Unchanged` 以外はすべて安全側（索引無効化）
/// に倒します（[`restore_evicted_source`] の doc コメント「手順」参照）。
fn change_kind_from_verdict_for_restore(
    verdict: hakutaku_data_source::SnapshotVerdict,
) -> ChangeKind {
    match verdict {
        hakutaku_data_source::SnapshotVerdict::Shrunk { .. } => ChangeKind::Shrunk,
        hakutaku_data_source::SnapshotVerdict::Replaced => ChangeKind::Replaced,
        hakutaku_data_source::SnapshotVerdict::Deleted => ChangeKind::Deleted,
        // 復元経路では、解放中の追記を静かに取り込まない（doc コメント参照）。
        // 保守的に「置換」と同じ無効化ラベルへ倒す。
        hakutaku_data_source::SnapshotVerdict::Appended { .. } => ChangeKind::Replaced,
        // Unchanged は呼び出し元で別処理のため到達しない。防御的に安全側。
        hakutaku_data_source::SnapshotVerdict::Unchanged => ChangeKind::Replaced,
    }
}

/// P06-2: 進捗・キャンセル・抑制の制御をまとめて [`register_source_with_control`]
/// へ渡すための入れ物です。
///
/// P04-6 の共通契約（[`crate::notification`]）と、
/// [`hakutaku_data_source::IoThrottle`]（`PERF-014` の接続点）をまとめています。
pub struct LoadControl<'a> {
    pub task_id: crate::notification::TaskId,
    /// 進捗の通知先（P04-6）。`None` なら通知しません。
    pub progress: Option<&'a dyn crate::notification::ProgressSink>,
    /// キャンセル要求（P04-6）。`None` なら常に「キャンセルされていない」
    /// として扱います。
    pub cancellation: Option<&'a crate::notification::CancellationToken>,
    /// 同時実行数の上限・I/O 発行間隔（`PERF-014` の接続点）。
    pub throttle: hakutaku_data_source::IoThrottle,
    /// 1チャンクあたりのバイト数。
    pub chunk_bytes: u64,
    /// この量までは「要求済み範囲」として、先読み抑制（`prefetch_paused`）に
    /// 関わらず必ず読みます。既定は `u64::MAX`（実質的に全範囲を要求済み扱いに
    /// し、先読み抑制を無効化する）。
    pub eager_bytes: u64,
    /// 開く際にユーザーが明示的に指定したログ解析プロファイル名（`LOG-022`、
    /// P07-2）。`None` なら手動指定なし（従来どおり `crate::profile_resolution`
    /// の第2段階以降で自動解決します）。`Some` の場合は
    /// [`resolve_profile`](crate::profile_resolution::resolve_profile) の第1段階
    /// （手動指定）へそのまま渡され、`profiles` に同名のものが無ければ
    /// [`ResolutionOutcome::ManualNotFound`] となり生表示へ退避します
    /// （本ファイル doc コメント「プロファイル解決結果と生表示退避の対応」）。
    pub manual_profile: Option<&'a str>,
    /// 開く際にユーザーが UI で明示的に選んだ日時書式（`LOG-022`、
    /// P07 の「日時書式を選んで再解析」）。`None` なら手動指定なしです。
    ///
    /// `Some` の場合、プロファイルの `datetime_format` 設定より優先します
    /// （本ファイル doc コメント「日時書式の決め方」の優先順位）。設定ファイル
    /// を書かずに開いたファイルを、その場の操作だけで解析させるための経路
    /// であり、設定より弱いと目的を果たせないためです。
    ///
    /// ただし `Ambiguous`／`ManualNotFound` によるプロファイル起因の生表示退避
    /// には勝てません（生表示のままになります）。プロファイルを一意に決められ
    /// ない状態では設定全体を採用できず、日時書式だけを採用すると文字コードと
    /// 日時解析でよりどころが食い違うためです。この場合、利用者は同じ再解析
    /// UI でプロファイルも併せて選べば解消できます。
    pub manual_datetime_format: Option<LogDateTimeFormat>,
}

impl<'a> LoadControl<'a> {
    /// 進捗・キャンセル・抑制・手動プロファイル指定・手動書式指定のいずれも
    /// 使わない既定値です（[`register_source`] が内部で使います）。
    #[must_use]
    pub fn none() -> Self {
        LoadControl {
            task_id: crate::notification::TaskId::generate(),
            progress: None,
            cancellation: None,
            throttle: hakutaku_data_source::IoThrottle::unlimited(),
            chunk_bytes: hakutaku_data_source::DEFAULT_CHUNK_BYTES,
            eager_bytes: u64::MAX,
            manual_profile: None,
            manual_datetime_format: None,
        }
    }
}

/// [`register_source_with_control`] の結果です。
#[derive(Debug, Clone)]
pub struct RegisterSourceOutcome {
    pub handle: DisplaySetHandle,
    pub summary: LoadSummary,
    /// 処理単位の最終結果（P04-6）。`Completed` 以外（`Cancelled`・`Failed`）
    /// の場合でも、`handle` は読み込み済み範囲の状態を指します（読み込み
    /// 済み範囲は保持される。作業項目4）。
    pub outcome: crate::notification::TaskOutcome,
}

/// 読み込み中のレジストリ（[`DisplaySetRegistry`]）を、**バッチ確定のたびに
/// 短時間だけ借りる**ための接続点です。
///
/// # なぜ必要か
///
/// [`register_source_with_control`] は `&mut DisplaySetRegistry` を引数で
/// 受け取るため、呼び出しが返るまでレジストリを占有します。GUI 層
/// （`src-tauri`）はレジストリを `std::sync::Mutex` で包んだ managed state と
/// して持っているので、この形で呼ぶと**読み込みが終わるまでロックを保持し
/// 続ける**ことになり、同じ Mutex を取る範囲取得（`fetch_log_range`）が
/// その間まったく応答できません。GB 級のファイルでは数秒〜数十秒に達します。
/// 読み込み途中でも表示集合を伸長する仕組み（[`DisplaySetRegistry::
/// grow_source_items`]、P06-2）を用意していても、この一点のせいで利用者から
/// 途中経過が見えない状態でした（`ENV-004`・`PERF-009`）。
///
/// そこでレジストリの受け取り方を「借りっぱなし」から「必要なときに借りる」
/// へ変え、ロックの取得・解放そのものを呼び出し側の実装として注入できるように
/// しています。コア層は `std::sync::Mutex` を知らないままでいられます
/// （コアの GUI 非依存の原則）。
///
/// # 実装側の契約
///
/// - `borrow` の実行中だけレジストリへの排他アクセスを与えます。GUI 層の実装は
///   ここでロックを取り、`borrow` から戻った時点で解放します
/// - このモジュールは `with_registry` の内側から `with_registry` を呼びません
///   （再入しません）。実装側で再帰的なロックを考慮する必要はありません
/// - `borrow` が `panic` した場合の扱いは実装側に委ねます（GUI 層は
///   `PoisonError::into_inner` で毒された Mutex を引き継ぐ既存方針のままです）
pub trait RegistryAccess {
    /// レジストリを排他的に借り、`borrow` をちょうど1回実行して結果を返します。
    fn with_registry<R>(&mut self, borrow: impl FnOnce(&mut DisplaySetRegistry) -> R) -> R;
}

/// 既に `&mut DisplaySetRegistry` を持っている呼び出し側のための
/// [`RegistryAccess`] 実装です（ロックは介在せず、借用をそのまま渡します）。
///
/// 単一スレッドから読み込む経路（[`register_source`]・
/// [`load_file_into_registry`]・テスト・`examples/scale_verify.rs`）はこちらを
/// 使います。読み込み中に別スレッドから同じレジストリを触る必要がある GUI 層
/// だけが、ロックを内包する独自の実装を渡します。
pub struct DirectRegistryAccess<'a>(&'a mut DisplaySetRegistry);

impl<'a> DirectRegistryAccess<'a> {
    #[must_use]
    pub fn new(registry: &'a mut DisplaySetRegistry) -> Self {
        DirectRegistryAccess(registry)
    }
}

impl RegistryAccess for DirectRegistryAccess<'_> {
    fn with_registry<R>(&mut self, borrow: impl FnOnce(&mut DisplaySetRegistry) -> R) -> R {
        borrow(self.0)
    }
}

/// P06-2: 進捗・キャンセル・抑制を伴う複数ソース登録です
/// （`tasks/phase-06-large-file-loading.md` 作業項目1・2・4・9・10）。
///
/// [`register_source`] との違いは、[`LoadControl`] 経由で次を外部から渡せる
/// ことです。
///
/// - **進捗通知**（P04-6 の `ProgressSink`／`ProgressThrottle`）。読み込み
///   済みバイト数 / `snapshot_end` を通知します。
/// - **キャンセル**（P04-6 の `CancellationToken`）。チャンク境界で確認し、
///   検出時は読み込み済み範囲を保持したまま
///   [`crate::notification::TaskOutcome::Cancelled`] で終えます
///   （`SourceStatus::CancelledPartial`）。
/// - **抑制**（`hakutaku_data_source::IoThrottle`。`PERF-014` の接続点。
///   同時実行数の上限と I/O 発行間隔）と、**先読み抑制**
///   （`prefetch_paused()`。`eager_bytes` を超える範囲は、先読みが停止して
///   いる間は発行しません）。
/// - **手動プロファイル指定**（[`LoadControl::manual_profile`]、`LOG-022`、
///   P07-2）。`resolve_profile` の第1段階へそのまま渡されます。
/// - **手動書式指定**（[`LoadControl::manual_datetime_format`]、
///   `LOG-022`）。プロファイルの `datetime_format` より優先して使います
///   （本ファイル doc コメント「日時書式の決め方」）。
///
/// 加えて、読み込み途中でも解析済み範囲から表示集合を伸長します（作業項目1。
/// [`DisplaySetRegistry::grow_source_items`]。世代は変わらず、`total_items`
/// が増えていきます）。継続行の結合（`LOG-014`）と日時書式の自動判定は
/// [`crate::streaming_parse::StreamingAssembler`] が1行ずつ届く入力に対して
/// 行い、チャンク境界が行の途中・継続行の途中に落ちても、全件一括読み込みと
/// 同一の項目列になります（チャンク境界の安全な分割点は `\n` バイトのみを
/// 使うため、UTF-8・Windows コードページのいずれでもマルチバイト文字の途中を
/// 割りません）。
///
/// [`register_source`] は、これらをすべて渡さない（無効化した）
/// [`LoadControl::none`] で本関数を呼び出す薄いラッパーです。
///
/// # 読み込み中の変更検知・エラーと、登録済み状態の関係
///
/// 各チャンクの読み込み前に整合性を再確認します（`LOG-023`）。
///
/// - **まだ何も登録していない時点**（最初の解析済みバッチが確定する前）で
///   変更・エラーを検知した場合、[`register_source`] と同様に
///   `Err(RegisterSourceError::Load(_))` を返し、`registry`・`budget` の
///   状態は変更しません。
/// - **既に最初のバッチを登録済み**の場合、`registry` からそのソースを消す
///   ことはできません（利用者が既に一部の内容を見ている可能性があるため）。
///   代わりに、ソースの状態を [`crate::registry::SourceStatus::Changed`]／
///   `Error` へ遷移させたうえで、`Ok(RegisterSourceOutcome)` を
///   `outcome: TaskOutcome::Failed(_)` として返します。
///
/// # レジストリの借り方
///
/// この関数は `registry` を、呼び出しが返るまで占有します。**読み込み中に別
/// スレッドから同じレジストリを読む（範囲取得する）場合は、代わりに
/// [`register_source_with_access`] を使ってください。** 処理内容は同じで、
/// 違いはレジストリを借りる区間だけです（この関数自身、
/// [`DirectRegistryAccess`] を渡して `register_source_with_access` を呼ぶ薄い
/// ラッパーです）。
pub fn register_source_with_control(
    registry: &mut DisplaySetRegistry,
    budget: &crate::budget::SourceBudget,
    path: &Path,
    source_label: String,
    log_profiles: &[hakutaku_config::LogProfileConfig],
    control: &LoadControl<'_>,
) -> Result<RegisterSourceOutcome, RegisterSourceError> {
    register_source_with_access(
        &mut DirectRegistryAccess::new(registry),
        budget,
        path,
        source_label,
        log_profiles,
        control,
    )
}

/// [`register_source_with_control`] と同じ登録処理を、レジストリを
/// **バッチ確定のたびに借り直す**形で実行します。
///
/// 進捗・キャンセル・抑制・手動指定の扱い、戻り値、失敗時の状態遷移は
/// [`register_source_with_control`] とまったく同じです。以下はロックを分割する
/// 設計そのものの記録です。
///
/// # なぜバッチ境界で借り直すのか
///
/// 読み込み1回の内訳は「I/O（チャンク読み込みと抑制の待機）→ 文字コード判定と
/// デコード → 行分割 → 日時解析・継続行の結合 → **確定したバッチをレジストリへ
/// 登録**」です。このうちレジストリを触るのは最後の一手だけで、時間の大半を
/// 占める前段はレジストリと無関係です。バッチ境界（[`deliver_batch`] の
/// 呼び出し）でだけ借りれば、GUI 層はチャンク1つ分の登録が終わるたびに
/// レジストリを取れます。
///
/// 借用区間をこれより細かくしても意味がありません（1バッチの登録は
/// `insert_source`／`grow_source_items` の1回で完結し、途中で分割すると項目列と
/// 索引が食い違う瞬間ができてしまいます）。逆に粗くすると、読み込み全体を占有
/// する形へ戻ってしまいます。
///
/// # 借用区間で行うこと・行わないこと
///
/// **借用区間で行うのは、確定済みバッチの登録（[`deliver_batch`]）、読み込み
/// 終了時の最終確定（ハンドルの取得、`mark_cancelled_partial`／
/// `set_unconfirmed_trailing_line`、統合表示集合の同期）、早期終了時の状態遷移
/// （[`finish_with_early_failure`]）だけです。** ファイルの読み込み、デコード、
/// 行分割、日時解析、進捗通知、`LoadSummary` の組み立ては、いずれも借用の外で
/// 行います。
///
/// # 最終確定の借用が長くなること
///
/// 統合表示（P09-1）が ON のとき、最終確定の借用区間では参加ソース全体の
/// 再マージ（[`DisplaySetRegistry::sync_merged_view_after_load`]）が1回だけ
/// 走ります。全項目を集めて並べ替えるため、この1回はバッチ登録より明らかに
/// 長く、大規模データでは最終確定の借用時間がその分だけ伸びます。
///
/// 実測（`examples/scale_verify.rs` の `SCALE_MERGED=1`。300万項目のソースを
/// 開いて統合表示を ON にした状態で、230万項目のソースを読み込む場合）は、
/// 最終確定の借用が 0.0 ms（OFF）から 466 ms（ON）へ増えます。**この間は GUI
/// 層の範囲取得が待たされる**ため、読み込みの最後に一度だけ待ちが出ます。
///
/// それでもバッチごとの同期は採りません。同じ読み込みの借用回数は36回であり、
/// 伸長のたびに同期すれば同等の費用が35回繰り返されて、1.1 秒の読み込みに
/// 十数秒が上乗せされます（1回あたりを短くしても総和では桁違いに悪化する。
/// 理由の詳細は `sync_merged_view_after_load` の doc コメント）。統合表示が
/// OFF のときは同期が即座に戻るため、借用時間の増分はありません。
///
/// # 途中経過が外から見えることの整合
///
/// 借用の合間に別スレッドが範囲取得できるということは、**読み込み途中の表示
/// 集合が見える**ということです。これは P06-2 が
/// [`DisplaySetRegistry::grow_source_items`] を用意した時点からの設計どおりで、
/// 範囲取得の契約も破りません。
///
/// - 世代（`generation`）は伸長では進みません（`grow_source_items`）。同じ世代
///   のまま `total_items` だけが増えるため、「同じ範囲の再取得で同じ順序・同じ
///   識別子」は保たれます（確定済みの項目の内容・並びは後から変わりません）
/// - 範囲取得は `start` をその時点の `total_items` で丸めるため、伸長の途中で
///   末尾を超えた要求が来ても失敗しません
/// - キャンセル・変更検知・失敗時の扱いも従来どおりです（読み込み済みの範囲は
///   保持し、状態だけを `CancelledPartial`／`Changed`／`Error` へ移します）
///
/// # デッドロック回避規則との整合
///
/// レジストリを借りる回数が「1回」から「バッチ数 + 数回」へ増えるため、
/// `src-tauri` の遅延解放方式（`crate::lib` の `register_release_handler`
/// 配線）との整合を明示しておきます。ソフトしきい値到達時のハンドラは
/// **フラグを立てるだけでレジストリのロックを取りません**。借用区間の内側で
/// 起こり得るメモリ予約（[`crate::item::build_items_from_pending_into`] から
/// `hakutaku_memory_accounting` への予約）がハンドラを呼んでも、そこから
/// レジストリを取り直すことはないため、借用の回数が増えても再入は発生しま
/// せん。実際の解放（[`DisplaySetRegistry::evict_inactive_sources`]）は範囲
/// 取得の入口で行われ、読み込み中に走ってもデコード済みチャンクのキャッシュを
/// 捨てるだけです（索引・項目・世代・状態には触れません）。
pub fn register_source_with_access(
    access: &mut impl RegistryAccess,
    budget: &crate::budget::SourceBudget,
    path: &Path,
    source_label: String,
    log_profiles: &[hakutaku_config::LogProfileConfig],
    control: &LoadControl<'_>,
) -> Result<RegisterSourceOutcome, RegisterSourceError> {
    // 段階別内訳の基準点。ここから戻り値を組み立てるまでが
    // `LoadStageTimings::total` であり、各段階の合計との差が「その他」になる。
    let load_began = Instant::now();

    let (file, snapshot) = hakutaku_data_source::open_and_snapshot(path)
        .map_err(|error| RegisterSourceError::Load(LoadFileError::ReadFile(error)))?;

    let reservation = budget
        .reserve(snapshot.snapshot_end)
        .map_err(RegisterSourceError::BudgetRejected)?;

    // LOG-022 相当のプロファイル起因の生表示退避は、内容を読む前に判定できる。
    // 手動プロファイル指定（P07-2）は control.manual_profile 経由で渡される。
    let resolution = resolve_profile(control.manual_profile, path, log_profiles);
    let profile_resolution_route = resolution.route_label();
    let raw_display_due_to_profile = matches!(
        resolution,
        ResolutionOutcome::Ambiguous { .. } | ResolutionOutcome::ManualNotFound { .. }
    );
    let profile_encoding = profile_encoding_setting(&resolution);
    // 書式の優先順位（手動 > プロファイル > 自動）と、診断ログへ出す決定経路を
    // 1か所で同時に決める（resolve_datetime_format_and_route）。
    let (explicit_datetime_format, datetime_format_route) = resolve_datetime_format_and_route(
        control.manual_datetime_format,
        &resolution,
        raw_display_due_to_profile,
    );

    let mut cursor = DecodeCursor::new(profile_encoding);
    let mut assembler = crate::streaming_parse::StreamingAssembler::new(
        raw_display_due_to_profile,
        explicit_datetime_format,
    );
    let mut progress_throttle = crate::notification::ProgressThrottle::with_defaults();
    let poisoned: RefCell<Option<LoadFileError>> = RefCell::new(None);
    let mut inserted: Option<DisplaySetHandle> = None;
    // P08-5: もはや読み込みバッファの実確保量ではなく、行数に
    // 比例して常駐する構造（索引本体+行番号配列+項目列）へ実際に
    // 振り替えた総量です（`LoadSummary::reserved_bytes` の doc コメント参照）。
    // 事前確保の導入以降は、反映済みの項目数も併せて持ちます（総項目数を外挿して
    // 事前確保するための標本。`deliver_batch`）。
    let mut progress = ResidentProgress::default();
    let path_owned: PathBuf = path.to_path_buf();
    // 段階別の累計。チャンク境界にだけ計時点を置く
    // （[`LoadStageTimings`] の doc コメント「オーバーヘッドが無視できる根拠」）。
    let mut stages = StageAccumulator::default();

    // 4. キャンセルの確認は、P04-6 の CancellationToken に加えて、致命的な
    //    エンコード判定エラー・索引メモリ予約の拒否（下記 on_chunk 内）を
    //    検出した場合も同じ経路で読み込みを打ち切る（`crates/data-source` は
    //    本クレートへ依存しないため、on_chunk 内のエラーを直接 Err として
    //    伝播できない。is_cancelled を介して間接的に打ち切る設計）。
    let is_cancelled_combined = || {
        poisoned.borrow().is_some()
            || control
                .cancellation
                .is_some_and(crate::notification::CancellationToken::is_cancelled)
    };

    let chunk_result = {
        let on_chunk = |chunk: &[u8], bytes_done: u64, total: u64| {
            if poisoned.borrow().is_some() {
                // 既に致命的エラーで打ち切り済み（is_cancelled_combined により
                // 次のチャンクからは呼ばれなくなるはずだが、念のため防御する）。
                return;
            }

            let feed_began = Instant::now();
            let fed = cursor.feed(chunk);
            stages.feed += feed_began.elapsed();
            if let Err(error) = fed {
                *poisoned.borrow_mut() = Some(error);
                return;
            }
            // 行本文は cursor が保持するデコード済み文字列からの借用であり、
            // 複製しない。assembler は渡された &str を保持せず
            // 判定に使うだけなので、次の feed で内容が置き換わっても問題ない。
            //
            // 計時はこのループ全体を1回で囲む。行ごとに計ると
            // 300万行で `Instant::now` が600万回になり、計測が対象と同じ桁の
            // 費用を持ち込む（`LoadStageTimings` の doc コメント）。
            let parse_began = Instant::now();
            for line in cursor.lines() {
                assembler.feed_line(line.text, line.raw_offset, line.raw_content_len, false);
            }
            stages.parse += parse_began.elapsed();

            // 3. 進捗の通知（P04-6）。
            if let Some(sink) = control.progress {
                if progress_throttle.should_notify(Instant::now(), bytes_done) {
                    sink.report(
                        control.task_id,
                        crate::notification::Progress::Determinate {
                            done: bytes_done,
                            total,
                            unit: crate::notification::ProgressUnit::Bytes,
                        },
                    );
                }
            }

            // 作業項目1: 解析済み範囲から表示集合を伸長する。
            //
            // ここがレジストリを借りる唯一の地点（バッチ境界）。
            // 上の I/O・デコード・行分割・日時解析・進捗通知はすべて借用の外で
            // 終わっている。借用区間には deliver_batch だけを置き、それ以外の
            // 処理を混ぜない（この関数の doc コメント「借用区間で行うこと・
            // 行わないこと」）。
            let datetime_format = assembler.detected_datetime_format();
            let selected_encoding = cursor.selected_encoding();
            let batch = assembler.drain_ready();
            // 借用区間そのものを計る。この値は、GUI 層のロックが
            // 実際に保持される時間の累計でもある。
            let deliver_began = Instant::now();
            let delivered = access.with_registry(|registry| {
                deliver_batch(
                    registry,
                    &mut inserted,
                    &mut progress,
                    &path_owned,
                    &source_label,
                    snapshot,
                    reservation,
                    batch,
                    bytes_done,
                    datetime_format,
                    selected_encoding,
                )
            });
            stages.deliver += deliver_began.elapsed();
            if let Err(error) = delivered {
                *poisoned.borrow_mut() = Some(error);
            }
        };

        hakutaku_data_source::stream_snapshotted_bytes_chunked(
            hakutaku_data_source::ChunkedReadRequest {
                file,
                path,
                snapshot: &snapshot,
                budget: hakutaku_memory_accounting::global_budget(),
                chunk_bytes: control.chunk_bytes,
                throttle: &control.throttle,
                eager_bytes: control.eager_bytes,
                is_cancelled: &is_cancelled_combined,
            },
            on_chunk,
        )
    };

    let outcome = match chunk_result {
        Ok(summary) => summary,
        Err(hakutaku_data_source::ChunkReadError::ChangeDetected(verdict)) => {
            let kind = change_kind_from_verdict(verdict);
            return finish_with_early_failure(
                access,
                budget,
                inserted,
                reservation,
                &source_label,
                "読み込み中にファイルの変更を検知しました（LOG-023）。対象を閉じてから、\
                 変更後の内容を再度開き直してください。"
                    .to_string(),
                Some(kind),
                LoadFileError::ChangedDuringLoad(verdict),
                || {
                    build_control_load_summary(
                        snapshot.snapshot_end,
                        progress.committed_bytes,
                        &cursor,
                        &assembler,
                        raw_display_due_to_profile,
                        profile_resolution_route,
                        datetime_format_route,
                        false,
                        stages.snapshot(load_began, cursor.decode_elapsed),
                    )
                },
            );
        }
        Err(hakutaku_data_source::ChunkReadError::Read(read_error)) => {
            let message = read_error.to_string();
            return finish_with_early_failure(
                access,
                budget,
                inserted,
                reservation,
                &source_label,
                message,
                None,
                LoadFileError::ReadFile(read_error),
                || {
                    build_control_load_summary(
                        snapshot.snapshot_end,
                        progress.committed_bytes,
                        &cursor,
                        &assembler,
                        raw_display_due_to_profile,
                        profile_resolution_route,
                        datetime_format_route,
                        false,
                        stages.snapshot(load_began, cursor.decode_elapsed),
                    )
                },
            );
        }
    };

    if let Some(error) = poisoned.into_inner() {
        let message = error.to_string();
        return finish_with_early_failure(
            access,
            budget,
            inserted,
            reservation,
            &source_label,
            message,
            None,
            error,
            || {
                build_control_load_summary(
                    snapshot.snapshot_end,
                    progress.committed_bytes,
                    &cursor,
                    &assembler,
                    raw_display_due_to_profile,
                    profile_resolution_route,
                    datetime_format_route,
                    false,
                    stages.snapshot(load_began, cursor.decode_elapsed),
                )
            },
        );
    }

    // I/O の内訳は、読み切った（またはキャンセル・先読み停止で
    // 正常に打ち切った）場合にだけ得られる。上の早期失敗経路を通ったときは
    // 0 のままになる（[`LoadStageTimings::io_read`] の doc コメント）。
    stages.io_read = outcome.read_elapsed;

    // 5. 正常に完了した場合だけ、末尾断片の最終フラッシュを行う（LOG-026）。
    //    キャンセル・先読み停止で途中終了した場合、carry に残った断片は
    //    「読み込み済み範囲」に含めず破棄する（本モジュール doc コメント
    //    「読み込み中の変更検知・エラーと、登録済み状態の関係」と同じ
    //    「まだ確定していない範囲は保持しない」という考え方）。
    let mut has_unconfirmed_trailing_line = false;
    if !outcome.cancelled && !outcome.prefetch_stopped {
        // 末尾断片も通常のチャンクと同じ段階へ計上する（末尾だけ
        // 内訳から抜けると、合計と段階の差＝「その他」に紛れてしまう）。
        let finish_began = Instant::now();
        let finished = cursor.finish();
        stages.feed += finish_began.elapsed();
        match finished {
            Ok(()) => {
                has_unconfirmed_trailing_line = cursor.last_line_unconfirmed();
                let parse_began = Instant::now();
                for line in cursor.lines() {
                    assembler.feed_line(
                        line.text,
                        line.raw_offset,
                        line.raw_content_len,
                        !line.confirmed,
                    );
                }
                stages.parse += parse_began.elapsed();
            }
            Err(error) => {
                let message = error.to_string();
                return finish_with_early_failure(
                    access,
                    budget,
                    inserted,
                    reservation,
                    &source_label,
                    message,
                    None,
                    error,
                    || {
                        build_control_load_summary(
                            snapshot.snapshot_end,
                            progress.committed_bytes,
                            &cursor,
                            &assembler,
                            raw_display_due_to_profile,
                            profile_resolution_route,
                            datetime_format_route,
                            false,
                            stages.snapshot(load_began, cursor.decode_elapsed),
                        )
                    },
                );
            }
        }
    }

    let final_parse_began = Instant::now();
    assembler.finish();
    let final_batch = assembler.drain_ready();
    stages.parse += final_parse_began.elapsed();
    let final_datetime_format = assembler.detected_datetime_format();
    let final_selected_encoding = cursor.selected_encoding();
    // 最終バッチの登録も、途中のバッチと同じく借用区間はこの1回だけ。
    let final_deliver_began = Instant::now();
    let final_delivery = access.with_registry(|registry| {
        deliver_batch(
            registry,
            &mut inserted,
            &mut progress,
            &path_owned,
            &source_label,
            snapshot,
            reservation,
            final_batch,
            // 全量を読み終えているため、最終バッチの見積もりは外挿ではなく確定値
            // になる（`estimate_total_items`）。途中の見積もりが外れていても、
            // ここでちょうどの容量へ合わせ直せる。
            snapshot.snapshot_end,
            final_datetime_format,
            final_selected_encoding,
        )
    });
    stages.deliver += final_deliver_began.elapsed();
    if let Err(error) = final_delivery {
        let message = error.to_string();
        return finish_with_early_failure(
            access,
            budget,
            inserted,
            reservation,
            &source_label,
            message,
            None,
            error,
            || {
                build_control_load_summary(
                    snapshot.snapshot_end,
                    progress.committed_bytes,
                    &cursor,
                    &assembler,
                    raw_display_due_to_profile,
                    profile_resolution_route,
                    datetime_format_route,
                    has_unconfirmed_trailing_line,
                    stages.snapshot(load_began, cursor.decode_elapsed),
                )
            },
        );
    }

    // 読み込み終了時の最終確定。ハンドルの取得と状態の確定を1回の
    // 借用にまとめ、「登録済みなのに状態が未確定」という中間状態が外から見え
    // ないようにする（借用の外から観測できるのは、確定前か確定後のどちらか）。
    let finalize_began = Instant::now();
    let finalized = access.with_registry(|registry| {
        let handle = match inserted {
            Some(handle) => registry.current_handle(handle.source_id).unwrap_or(handle),
            None => {
                // 1件も読めなかった（空ファイル等）。空のまま登録する。
                registry.insert_source(
                    path_owned,
                    source_label.clone(),
                    &[],
                    snapshot,
                    reservation,
                    has_unconfirmed_trailing_line,
                    assembler.detected_datetime_format(),
                    cursor.selected_encoding().unwrap_or(SelectedEncoding::Utf8),
                    // 1件も読めていないので事前確保する容量もない。
                    CapacityEstimate::Exact(0),
                )?
            }
        };

        // 2. キャンセル・先読み停止のいずれかで途中終了した場合は
        //    「キャンセル済み（部分読み込み）」として区別する（作業項目4）。
        let task_outcome = if outcome.cancelled || outcome.prefetch_stopped {
            registry.mark_cancelled_partial(handle.source_id);
            crate::notification::TaskOutcome::Cancelled
        } else {
            registry.set_unconfirmed_trailing_line(handle.source_id, has_unconfirmed_trailing_line);
            crate::notification::TaskOutcome::Completed
        };

        // 統合表示（P09-1）が ON のとき、この読み込みの全項目を
        // 統合表示集合へ反映する。読み込み途中の伸長（`grow_source_items`）は
        // 意図的に同期しないため、ここで1回同期しないと、統合表示 ON のまま
        // 開いた対象は最初のバッチ分しか統合側に現れないままになる。
        // キャンセル確定（`CancelledPartial`）も「その時点の項目で完了」と
        // して同じ扱いにするため、上の分岐によらず呼ぶ。
        //
        // `inserted` が None の分岐は、直前の `insert_source` が内部で同期
        // 済み。二重に呼ぶと世代だけが余計に進み、フロントエンドに無駄な
        // 再取得（`generation_mismatch` の自己修復）を起こさせるため除く。
        if inserted.is_some() {
            registry.sync_merged_view_after_load();
        }
        Ok((handle, task_outcome))
    });
    stages.deliver += finalize_began.elapsed();
    let (handle, task_outcome) = match finalized {
        Ok(finalized) => finalized,
        Err(rejected) => {
            budget.release(reservation);
            return Err(RegisterSourceError::Load(
                LoadFileError::IndexMemoryBudgetExceeded(rejected),
            ));
        }
    };

    // 10. メモリ会計への接続（PERF-008・PERF-010、作業項目10）。
    //
    // P08-5 以降、`crate::line_index::IndexedText` は本文を
    // 一切保持しません。会計への接続は、この関数の外側では事後的に行わず、
    // `crate::item::build_items_from_pending` が `IndexedText::push_entry` で
    // 実際に追記する**直前**に予約し、追記後に振り替えます（`deliver_batch`
    // → `registry.insert_source`／`grow_source_items` → `build_items_from_
    // pending` の経路。詳細は `crate::item` の doc コメントを参照）。**この
    // 予約の拒否は登録失敗として扱います**（`deliver_batch` が `Err` を
    // 返し、この関数はそれを検出して早期終了します）。したがって、この
    // 関数自身が改めて会計へ接続する必要はありません。

    let summary = build_control_load_summary(
        snapshot.snapshot_end,
        progress.committed_bytes,
        &cursor,
        &assembler,
        raw_display_due_to_profile,
        profile_resolution_route,
        datetime_format_route,
        has_unconfirmed_trailing_line,
        // 要約の組み立て自体は「その他」へ落ちる（この時点で計時を締めるため）。
        // 数マイクロ秒の処理であり、内訳の読み方に影響しない。
        stages.snapshot(load_began, cursor.decode_elapsed),
    );

    Ok(RegisterSourceOutcome {
        handle,
        summary,
        outcome: task_outcome,
    })
}

/// 段階別内訳を読み込み中に積み上げるための内部状態です。
///
/// 公開する [`LoadStageTimings`] と形が違うのは、`feed` が**デコードを含んだ
/// `DecodeCursor::feed`／`finish` 全体**だからです。デコードは
/// `DecodeCursor::decode_and_split` の途中で呼ばれるため外側からは区間として
/// 切り出せず、`DecodeCursor` 側が別に累計しています
/// （`DecodeCursor::decode_elapsed`）。両者の引き算で「デコード」と「行分割」を
/// 分けるのが [`Self::snapshot`] です。
#[derive(Debug, Default, Clone, Copy)]
struct StageAccumulator {
    /// I/O（`hakutaku_data_source::ChunkReadSummary::read_elapsed`）。
    /// 読み込みが正常に終わった時点でだけ入ります。
    io_read: Duration,
    /// `DecodeCursor::feed`／`finish` 全体（デコード＋行分割）。
    feed: Duration,
    /// 日時解析・継続行結合（`StreamingAssembler` へ渡すループ）。
    parse: Duration,
    /// レジストリの借用区間（`deliver_batch` と最終確定）。
    deliver: Duration,
}

impl StageAccumulator {
    /// 現時点の累計を、公開形式 [`LoadStageTimings`] へ変換します。
    ///
    /// `decode_elapsed` には `DecodeCursor::decode_elapsed` を渡します。
    /// 行分割は「`feed` 全体 − デコード」で求めるため、飽和減算にしています
    /// （両者は同じ区間の内と外であり負にはなりませんが、計時の丸めで下回った
    /// 場合にパニックさせる価値はありません）。
    fn snapshot(&self, load_began: Instant, decode_elapsed: Duration) -> LoadStageTimings {
        LoadStageTimings {
            total: load_began.elapsed(),
            io_read: self.io_read,
            decode: decode_elapsed,
            parse: self.parse + self.feed.saturating_sub(decode_elapsed),
            deliver: self.deliver,
        }
    }
}

/// 読み込み中に更新していく、常駐分（索引・行番号配列・項目列）の進捗です。
#[derive(Debug, Default, Clone, Copy)]
struct ResidentProgress {
    /// 索引・項目列へ反映済みの論理項目数。総項目数の外挿
    /// （[`estimate_total_items`]）の標本になります。
    items: u64,
    /// 会計へ実確保として振り替えたバイト数（[`LoadSummary::reserved_bytes`]）。
    committed_bytes: usize,
}

/// 総項目数の外挿（[`estimate_total_items`]）を始めるために最低限必要な、
/// 読み込み済みバイト数です。
///
/// 1 MiB あれば、1行500バイトの長い行でも約2000行の標本になり、平均行長は
/// 数％の精度で決まります。これより小さい標本から外挿すると、行長のばらつきが
/// そのまま見積もりの倍率誤差になります。
const MIN_PROJECTION_SAMPLE_BYTES: u64 = 1024 * 1024;

/// 読み込み済みの範囲から、このソース全体の論理項目数を外挿します。
///
/// 式は「推定項目数 = 総バイト数 / 1項目あたりの平均バイト数」です。平均を
/// **固定の仮定値ではなく、読み終えた範囲の実測値**（`bytes_done /
/// items_so_far`）から求めるのは、固定値では安全な見積もりにならないためです。
/// `Vec` の伸長は倍々であり、事前確保した容量を1件でも超えると次の容量が2倍に
/// なるので、「わずかに過小な見積もり」は事前確保をしない場合より結果が悪く
/// なります（最終容量が必要量の約1.9倍、再確保時の一時確保が約2.9倍）。
/// 一方で過大な見積もりは、使われない容量をそのまま常駐させ、`PERF-008` の
/// 予算を圧迫して**従来読めていたファイルの拒否**を招きます。行の長さはログの
/// 種類によって数十〜数百バイトと大きく変わるため、どの固定値を選んでも
/// どちらかの失敗に寄ります。実測平均なら、同種の行が並ぶ通常のログで数％の
/// 誤差に収まります。
///
/// `headroom`（5%）を上乗せするのは、わずかな過小を避けるためです。過大側に
/// 倒したときの実害は「使われない容量が数％常駐する」ことだけで、過小側に
/// 倒したときの全量コピー1回より軽い、という判断です。
///
/// 全量を読み終えている場合（`bytes_done >= total_bytes`）は外挿せず
/// [`CapacityEstimate::Exact`] を返します。この場合 `items_so_far` が最終的な
/// 項目数そのものであり、上乗せは無駄な常駐にしかならないためです。読み込みの
/// 最終バッチが必ずこの経路を通るので、途中の見積もりが外れていても最後に
/// ちょうどの容量へ合わせ直せます。
fn estimate_total_items(items_so_far: u64, bytes_done: u64, total_bytes: u64) -> CapacityEstimate {
    let items_so_far_usize = usize::try_from(items_so_far).unwrap_or(usize::MAX);
    // 標本がない（1件も項目にできていない）場合は外挿できない。0 を返せば
    // 事前確保は行われず、従来どおりの伸長になる。
    if items_so_far == 0 || bytes_done == 0 || bytes_done >= total_bytes {
        return CapacityEstimate::Exact(items_so_far_usize);
    }
    // 標本が小さいうちは外挿しない。`chunk_bytes` は呼び出し側が指定でき
    // （[`LoadControl`]）、極端に小さい値では最初のバッチが数行しかない標本に
    // なり得ます。数行の平均でファイル全体を外挿すると、行長のばらつきが
    // そのまま倍率の誤差になり、過大な見積もり＝使われない容量の常駐を招き
    // ます。既定のチャンク（8 MiB）では最初のバッチが必ずこの下限を超える
    // ため、通常の読み込みでは1バッチ目から事前確保が効きます。
    if bytes_done < MIN_PROJECTION_SAMPLE_BYTES {
        return CapacityEstimate::Exact(items_so_far_usize);
    }

    // u128 で計算するのは、items_so_far * total_bytes が u64 を溢れ得るため
    // （2000万項目 × 2 GiB は約 4.3e16 で u64 には収まるが、将来の上限緩和で
    // 溢れる余地を残さない）。
    let projected = u128::from(items_so_far) * u128::from(total_bytes) / u128::from(bytes_done);
    let with_headroom = projected + projected / 20;
    CapacityEstimate::Projected(usize::try_from(with_headroom).unwrap_or(usize::MAX))
}

/// バッチ（[`PendingItem`] の列）を、まだ登録済みでなければ新規登録
/// （[`DisplaySetRegistry::insert_source`]）、登録済みならば伸長
/// （[`DisplaySetRegistry::grow_source_items`]）します（作業項目1）。
///
/// `batch` が空の場合は何もしません。まだ未登録の状態で空バッチのまま
/// 登録してしまうと「1件も読めていない」空の表示集合が早期に出来てしまう
/// ため、あえて何もしません（最終的に1件も読めなかった場合の登録は、
/// 呼び出し側が読み込み完了後に明示的に行います）。
///
/// `bytes_done` は、このバッチを取り出した時点でファイル先頭から読み終えた
/// バイト数です（総項目数の外挿に使います）。
///
/// 索引・項目列の伸長分のメモリ予約が拒否された場合（P08-5）、
/// `Err` を返し `progress` は更新しません。
///
/// バッチ境界でのロック取り直しを導入して以降、この関数は**レジストリを借りて
/// いる区間そのもの**です
/// （[`register_source_with_access`] の doc コメント「借用区間で行うこと・
/// 行わないこと」）。GUI 層ではこの呼び出しの間だけ `Mutex` が保持されるため、
/// レジストリを触らない処理（I/O・デコード・解析）をここへ持ち込まないで
/// ください。
#[allow(clippy::too_many_arguments)]
fn deliver_batch(
    registry: &mut DisplaySetRegistry,
    inserted: &mut Option<DisplaySetHandle>,
    progress: &mut ResidentProgress,
    path: &Path,
    source_label: &str,
    snapshot: hakutaku_data_source::FileSnapshot,
    reservation: crate::budget::SourceReservation,
    batch: Vec<PendingItem>,
    bytes_done: u64,
    datetime_format: Option<LogDateTimeFormat>,
    selected_encoding: Option<SelectedEncoding>,
) -> Result<(), LoadFileError> {
    if batch.is_empty() {
        return Ok(());
    }
    // 外挿の標本には、このバッチを反映した後の件数を使う（バッチを反映する
    // 直前の件数だと、最初のバッチで標本が 0 件になり事前確保が効かない）。
    let items_after_batch = progress.items.saturating_add(batch.len() as u64);
    let estimate = estimate_total_items(items_after_batch, bytes_done, snapshot.snapshot_end);

    let source_id = match inserted {
        Some(handle) => {
            match registry.grow_source_items(handle.source_id, &batch, estimate) {
                Ok(_) => {}
                Err(crate::registry::IndexGrowError::UnknownSource) => {
                    // 直前で払い出した source_id を使い続けているため到達しない
                    // はず。防御的に何もしない（データを失うより安全側）。
                }
                Err(crate::registry::IndexGrowError::ReservationRejected(rejected)) => {
                    return Err(LoadFileError::IndexMemoryBudgetExceeded(rejected));
                }
            }
            handle.source_id
        }
        None => {
            let selected_encoding = selected_encoding.unwrap_or(SelectedEncoding::Utf8);
            let handle = registry
                .insert_source(
                    path.to_path_buf(),
                    source_label.to_string(),
                    &batch,
                    snapshot,
                    reservation,
                    false,
                    datetime_format,
                    selected_encoding,
                    estimate,
                )
                .map_err(LoadFileError::IndexMemoryBudgetExceeded)?;
            let source_id = handle.source_id;
            *inserted = Some(handle);
            source_id
        }
    };

    progress.items = items_after_batch;
    // 振り替え済みバイト数はレジストリ側が累計している（事前確保の導入以降、
    // 項目数 × `RESIDENT_BYTES_PER_ITEM` では表せない。事前確保した余剰容量を
    // 含むため。`crate::registry::DisplaySetRegistry::resident_committed_bytes`
    // の doc コメント参照）。
    progress.committed_bytes = registry.resident_committed_bytes(source_id);
    Ok(())
}

/// [`register_source_with_control`] の早期終了（変更検知・読み込みエラー・
/// 致命的なデコードエラー）を、登録状態に応じて振り分ける共通処理です。
///
/// - `inserted` が `None`（まだ何も登録していない）: `budget` の予約を返却し、
///   `Err(RegisterSourceError::Load(load_error))` を返します。
/// - `inserted` が `Some`（既に一部を登録済み）: レジストリ上のそのソースを
///   `change_kind` が `Some` なら [`crate::registry::SourceStatus::Changed`]
///   へ、`None` なら `Error` へ遷移させたうえで、
///   `Ok(RegisterSourceOutcome { outcome: TaskOutcome::Failed(_), .. })` を
///   返します（登録済みの表示は消しません。`ERR-001`）。
///
/// レジストリを借りるのは状態遷移とハンドル取得の間だけで、
/// `summary_if_registered`（`LoadSummary` の組み立て）は借用の外で実行します。
#[allow(clippy::too_many_arguments)]
fn finish_with_early_failure(
    access: &mut impl RegistryAccess,
    budget: &crate::budget::SourceBudget,
    inserted: Option<DisplaySetHandle>,
    reservation: crate::budget::SourceReservation,
    source_label: &str,
    reason: String,
    change_kind: Option<ChangeKind>,
    load_error: LoadFileError,
    summary_if_registered: impl FnOnce() -> LoadSummary,
) -> Result<RegisterSourceOutcome, RegisterSourceError> {
    match inserted {
        None => {
            budget.release(reservation);
            Err(RegisterSourceError::Load(load_error))
        }
        Some(handle) => {
            let handle = access.with_registry(|registry| {
                match change_kind {
                    Some(kind) => {
                        // `LOG-023` は索引ごと無効化する経路であり、
                        // `mark_changed_now` 自身が統合表示集合を同期する
                        // （空になった参加ソースを反映する）。ここで重ねて
                        // 同期する必要はない。
                        registry.mark_changed_now(handle.source_id, kind);
                    }
                    None => {
                        registry.mark_error_now(handle.source_id, reason.clone());
                        // `ERR-001` により、ここまでに登録済みの項目は消さずに
                        // 残す。統合表示集合はまだ伸長分を知らないため、
                        // 「読み込めた範囲で確定した」ものとして同期し、
                        // 個別表示と食い違わないようにする。
                        registry.sync_merged_view_after_load();
                    }
                }
                registry.current_handle(handle.source_id).unwrap_or(handle)
            });
            Ok(RegisterSourceOutcome {
                handle,
                summary: summary_if_registered(),
                outcome: crate::notification::TaskOutcome::Failed(
                    crate::notification::UserFacingError::new(
                        source_label.to_string(),
                        reason,
                        "対象を閉じてから再試行してください。",
                    ),
                ),
            })
        }
    }
}

/// `hakutaku_data_source::SnapshotVerdict` を [`ChangeKind`] へ変換します。
fn change_kind_from_verdict(verdict: hakutaku_data_source::SnapshotVerdict) -> ChangeKind {
    match verdict {
        hakutaku_data_source::SnapshotVerdict::Shrunk { .. } => ChangeKind::Shrunk,
        hakutaku_data_source::SnapshotVerdict::Replaced => ChangeKind::Replaced,
        hakutaku_data_source::SnapshotVerdict::Deleted => ChangeKind::Deleted,
        // crates/data-source の read_snapshotted_bytes_chunked は
        // Unchanged/Appended を ChangeDetected として返さない（読み込みを
        // 継続する）契約のため、ここには来ないはず。防御的に Replaced 扱いに
        // する（安全側：索引を無効化する）。
        hakutaku_data_source::SnapshotVerdict::Unchanged
        | hakutaku_data_source::SnapshotVerdict::Appended { .. } => ChangeKind::Replaced,
    }
}

/// [`register_source_with_control`]・[`stream_decode_and_index`] 用に、
/// チャンク境界をまたいだ安全なデコードと**生バイトオフセットの記録**を行う
/// 累積状態です（P08-5）。
///
/// 生バイト列を `carry` へ蓄積し、最後に確定した改行（`\n`）バイトまでを
/// デコードします。`\n`（`0x0A`）・`\r`（`0x0D`）は UTF-8 の継続バイトにも
/// Windows コードページ（`crates/format-detection` が対応する各コードページ）
/// のマルチバイト文字の構成バイトにもなり得ないため（`hakutaku_data_source::
/// split_raw_lines` のモジュール doc コメント参照）、この分割点は常に安全です。
///
/// **`carry` は1チャンク分（既定 [`hakutaku_data_source::DEFAULT_CHUNK_BYTES`]、
/// 8 MiB）程度の一時バッファであり、ファイル全体を蓄積しません。** デコード
/// 確定のたびに `carry` から取り除かれます（`consume_and_decode` の
/// `self.carry.drain(..end)`）。生バイトの行分割（[`hakutaku_data_source::
/// split_raw_lines_into`]）とデコード後の行分割（[`hakutaku_data_source::
/// split_line_spans_into`]）を同じ `carry[..end]` に対して行い、両者の件数が一致する
/// （`\n` の出現回数は変わらないため）という前提で1対1に対応付けることで、
/// 各行の生バイトオフセット・長さを、デコードした本文を保持することなく
/// 記録します（[`DecodedLine`]）。
struct DecodeCursor {
    profile_encoding: hakutaku_format_detection::ProfileEncodingSetting,
    carry: Vec<u8>,
    /// これまでに確定処理した生バイト数（ファイル先頭からの絶対位置の基準点。
    /// BOM を含む。`decode_invalid_positions` の絶対位置化、および
    /// [`DecodedLine::raw_offset`] の算出に使います）。
    consumed_before: u64,
    decided: Option<hakutaku_format_detection::DecidedEncoding>,
    decode_invalid_positions: Vec<usize>,
    decode_invalid_positions_truncated: bool,
    /// 直近の `feed`／`finish` がデコードした本文。行本文はこの文字列への
    /// 範囲（[`DecodedLine`]）として保持し、行ごとに `String` を確保しません。
    /// 次の `feed` で丸ごと置き換わるため、[`Self::lines`] で
    /// 借用した内容は次の `feed` を呼ぶまでの間だけ有効です（借用検査により
    /// この規則はコンパイル時に強制されます）。
    decoded: String,
    /// 以下3つは、チャンクごとの `Vec` 再確保を避けるための再利用バッファです。
    /// 各呼び出しの先頭で `clear()` してから詰め直します。
    raw_spans: Vec<hakutaku_data_source::RawLineSpan>,
    decoded_spans: Vec<hakutaku_data_source::RawLineSpan>,
    line_spans: Vec<DecodedLine>,
    /// [`Self::consume_and_decode`] に費やした時間の累計
    /// （[`LoadStageTimings::decode`]）。
    ///
    /// 呼び出し側は `feed`／`finish` 全体を計り、そこからこの値を引くことで
    /// 「デコード」と「行分割」を分けます。デコードは `decode_and_split` の
    /// 途中で呼ばれるため、外側からは区間として切り出せないためです。
    decode_elapsed: Duration,
}

/// [`DecodeCursor`] が1行分デコードした結果です（P08-5）。
///
/// 行本文は [`DecodeCursor::decoded`] 内の範囲（`text_start..text_end`）として
/// 持ちます。本文は日時自動判定・継続行結合の判定にだけ使う一時データであり
/// （`crate::streaming_parse::StreamingAssembler` へ渡した後は保持しません）、
/// 行ごとに `String` を確保する必要がないためです。
/// `raw_offset`・`raw_content_len` が、索引（[`crate::line_index::
/// LineIndexEntry`]）へ最終的に記録される生バイト範囲の元になります。
struct DecodedLine {
    /// [`DecodeCursor::decoded`] 内の行本文の開始位置（区切り文字を含まない）。
    text_start: usize,
    /// [`DecodeCursor::decoded`] 内の行本文の終了位置（区切り文字を含まない）。
    text_end: usize,
    /// ソースファイル先頭からの生バイトオフセット（BOM を除く）。
    raw_offset: u64,
    /// 区切り文字を含まない本文の生バイト長。
    raw_content_len: u32,
    confirmed: bool,
}

/// [`DecodeCursor::lines`] が返す1行分の借用ビューです。
///
/// `text` は [`DecodeCursor::decoded`] からの借用であり、複製ではありません。
struct DecodedLineRef<'a> {
    text: &'a str,
    raw_offset: u64,
    raw_content_len: u32,
    confirmed: bool,
}

impl DecodeCursor {
    fn new(profile_encoding: hakutaku_format_detection::ProfileEncodingSetting) -> Self {
        DecodeCursor {
            profile_encoding,
            carry: Vec::new(),
            consumed_before: 0,
            decided: None,
            decode_invalid_positions: Vec::new(),
            decode_invalid_positions_truncated: false,
            decoded: String::new(),
            raw_spans: Vec::new(),
            decoded_spans: Vec::new(),
            line_spans: Vec::new(),
            decode_elapsed: Duration::ZERO,
        }
    }

    /// 直近の `feed`／`finish` が確定した行を、確定順に借用して返します。
    ///
    /// 返す [`DecodedLineRef::text`] は [`Self::decoded`] への借用なので、
    /// 次に `feed`／`finish`（`&mut self`）を呼ぶまでの間だけ使えます。
    fn lines(&self) -> impl Iterator<Item = DecodedLineRef<'_>> + '_ {
        self.line_spans.iter().map(|line| DecodedLineRef {
            text: &self.decoded[line.text_start..line.text_end],
            raw_offset: line.raw_offset,
            raw_content_len: line.raw_content_len,
            confirmed: line.confirmed,
        })
    }

    /// 直近の `feed`／`finish` が確定した最後の行が未確定行（`LOG-026`）か。
    fn last_line_unconfirmed(&self) -> bool {
        self.line_spans.last().is_some_and(|line| !line.confirmed)
    }

    /// 文字コード判定（`ENC-005`）を、最初にバイトが届いた時点で一度だけ
    /// 行います（`crates/format-detection::detect_encoding` は先頭部分だけを
    /// 見るため、`carry` が完全な1行を含んでいなくても判定できます）。
    fn ensure_decided(&mut self) -> Result<(), LoadFileError> {
        if self.decided.is_some() {
            return Ok(());
        }
        let decision =
            hakutaku_format_detection::detect_encoding(&self.carry, &self.profile_encoding)
                .map_err(LoadFileError::InvalidEncodingName)?;
        let decided = match decision {
            hakutaku_format_detection::EncodingDecision::Decided(decided) => decided,
            hakutaku_format_detection::EncodingDecision::Unsupported(unsupported) => {
                return Err(LoadFileError::UnsupportedEncoding(unsupported));
            }
        };
        self.decided = Some(decided);
        Ok(())
    }

    /// 登録時に確定した文字コード（P08-5）。`ensure_decided` が一度も成功して
    /// いなければ `None`（変更検知等で内容を一切読めなかった場合の防御的な
    /// 既定値）。
    fn selected_encoding(&self) -> Option<SelectedEncoding> {
        self.decided.as_ref().map(|decided| decided.encoding)
    }

    /// `carry[..end]` を確定デコードして [`Self::decoded`] へ格納し、`carry`
    /// から取り除きます。BOM は最初の呼び出し（`consumed_before == 0`）でだけ
    /// 読み飛ばします（BOM はファイル先頭にしか現れないため）。
    fn consume_and_decode(&mut self, end: usize) -> Result<(), LoadFileError> {
        self.ensure_decided()?;
        let decided = self
            .decided
            .as_ref()
            .expect("直前の ensure_decided で Some になっているはず");
        let effective = if self.consumed_before == 0 {
            decided.clone()
        } else {
            hakutaku_format_detection::DecidedEncoding {
                bom_len: 0,
                ..decided.clone()
            }
        };

        let outcome = hakutaku_format_detection::decode(&self.carry[..end], &effective)
            .map_err(LoadFileError::Decode)?;

        for position in outcome.invalid_positions {
            let absolute =
                usize::try_from(position as u64 + self.consumed_before).unwrap_or(usize::MAX);
            if self.decode_invalid_positions.len()
                < hakutaku_format_detection::MAX_INVALID_POSITIONS
            {
                self.decode_invalid_positions.push(absolute);
            } else {
                self.decode_invalid_positions_truncated = true;
            }
        }
        if outcome.invalid_positions_truncated {
            self.decode_invalid_positions_truncated = true;
        }

        self.consumed_before += end as u64;
        self.carry.drain(..end);
        self.decoded = outcome.text;
        Ok(())
    }

    /// `carry[..end]` を、生バイトの行分割（[`hakutaku_data_source::
    /// split_raw_lines_into`]）とデコード後の行分割（[`hakutaku_data_source::
    /// split_line_spans_into`]）の両方にかけ、1対1に対応付けて
    /// [`Self::line_spans`] を組み立てます（本メソッド doc コメント「安全性」参照）。
    ///
    /// 結果は戻り値ではなく `self` に持たせ、呼び出し側は [`Self::lines`] で
    /// 借用します（行ごとの `String` 確保を避けるため）。
    fn decode_and_split(&mut self, end: usize) -> Result<(), LoadFileError> {
        self.ensure_decided()?;
        // BOM 長は consume_and_decode より前に読む。consume_and_decode が
        // consumed_before を進めるため、後から読むと「最初の呼び出しか」の
        // 判定が変わってしまう（順序依存）。
        let is_first_call = self.consumed_before == 0;
        let bom_len = if is_first_call {
            self.decided
                .as_ref()
                .expect("直前の ensure_decided で Some になっているはず")
                .bom_len
        } else {
            0
        };
        let base = self.consumed_before;

        // 再利用バッファは、self の別フィールド（carry・decoded）と同時に
        // 借用できないため一時的に取り出す。mem::take が残すのは容量0の空
        // Vec であり、末尾で戻すことで確保済み容量はチャンクをまたいで
        // 保たれる（チャンクごとの Vec 再確保を避ける）。
        let mut raw_spans = std::mem::take(&mut self.raw_spans);
        let mut decoded_spans = std::mem::take(&mut self.decoded_spans);
        let mut line_spans = std::mem::take(&mut self.line_spans);
        raw_spans.clear();
        decoded_spans.clear();
        line_spans.clear();

        // 生バイト側の行分割は、デコードより先に済ませる必要がある
        // （consume_and_decode が carry[..end] を drain するため、後からでは
        // 同じ範囲を参照できない）。
        hakutaku_data_source::split_raw_lines_into(&self.carry[..end], &mut raw_spans);

        // デコードの計時はここで挟む。`consume_and_decode` は
        // 途中に `?` を挟む複数の失敗経路を持ち、関数の内側で計ると計り漏れる
        // 経路ができるため、唯一の呼び出し地点であるここで区間として囲む。
        let decode_began = Instant::now();
        let decoded = self.consume_and_decode(end);
        self.decode_elapsed += decode_began.elapsed();

        // 成否によらず、確保済みバッファは self へ戻してから抜ける。
        let outcome = match decoded {
            Ok(()) => {
                hakutaku_data_source::split_line_spans_into(&self.decoded, &mut decoded_spans);

                debug_assert_eq!(
                    raw_spans.len(),
                    decoded_spans.len(),
                    "生バイトの行分割とデコード後の行分割は同じ件数になるはず\
                     （hakutaku_data_source::split_raw_lines のモジュール doc コメント参照）"
                );
                // 件数が一致するときだけ組み立てる。食い違う場合（設計上
                // 到達しないはず）はパニックせず、行なしとして安全側に倒す。
                if raw_spans.len() == decoded_spans.len() {
                    for (index, (span, decoded)) in
                        raw_spans.iter().zip(decoded_spans.iter()).enumerate()
                    {
                        // 先頭行だけ BOM の分を進める（BOM はファイル先頭に
                        // しか現れないため、2行目以降は補正しない）。
                        let start = if index == 0 {
                            span.content_start + bom_len
                        } else {
                            span.content_start
                        };
                        let content_len = span.content_end.saturating_sub(start);
                        line_spans.push(DecodedLine {
                            text_start: decoded.content_start,
                            text_end: decoded.content_end,
                            raw_offset: base + start as u64,
                            raw_content_len: u32::try_from(content_len).unwrap_or(u32::MAX),
                            confirmed: decoded.confirmed,
                        });
                    }
                }
                Ok(())
            }
            Err(error) => Err(error),
        };

        self.raw_spans = raw_spans;
        self.decoded_spans = decoded_spans;
        self.line_spans = line_spans;
        outcome
    }

    /// 新しく届いたチャンクを取り込み、安全に確定できる行（末尾が改行の行、
    /// 常に `confirmed: true`）を [`Self::lines`] から取り出せる状態にします。
    /// 確定できる行がなければ空になり、未処理分は `carry` に残ります。
    fn feed(&mut self, chunk: &[u8]) -> Result<(), LoadFileError> {
        self.carry.extend_from_slice(chunk);
        let Some(last_newline) = self.carry.iter().rposition(|&byte| byte == b'\n') else {
            // 確定できる行がない場合も、前回の行が残っていると呼び出し側が
            // 同じ行を二重に処理してしまうため、必ず空にする。
            self.line_spans.clear();
            return Ok(());
        };
        self.decode_and_split(last_newline + 1)
    }

    /// 読み込み終了時、`carry` に残った末尾断片をデコードします。断片が
    /// 改行で終わらない場合（`LOG-026`）、最後の行が `confirmed: false` に
    /// なります。
    fn finish(&mut self) -> Result<(), LoadFileError> {
        if self.carry.is_empty() {
            // feed と同じ理由で、前回分を残さない。
            self.line_spans.clear();
            return Ok(());
        }
        let end = self.carry.len();
        self.decode_and_split(end)
    }
}

/// [`register_source_with_control`] 用の [`LoadSummary`] 組み立てです。
#[allow(clippy::too_many_arguments)]
fn build_control_load_summary(
    file_size_bytes: u64,
    reserved_bytes: usize,
    cursor: &DecodeCursor,
    assembler: &crate::streaming_parse::StreamingAssembler,
    raw_display_due_to_profile: bool,
    profile_resolution_route: &'static str,
    datetime_format_route: DatetimeFormatRoute,
    has_unconfirmed_trailing_line: bool,
    stage_timings: LoadStageTimings,
) -> LoadSummary {
    let (encoding_route, selected_encoding, encoding_warnings) = match &cursor.decided {
        Some(decided) => (
            encoding_route_label(decided.route),
            decided.encoding.to_string(),
            decided
                .warnings
                .iter()
                .map(|warning| warning.message.clone())
                .collect(),
        ),
        // 判定に十分なバイトがまだ届いていない（変更検知などでごく初期に
        // 打ち切られた）場合の防御的な既定値。
        None => ("未判定（打ち切り）", String::new(), Vec::new()),
    };

    LoadSummary {
        file_size_bytes,
        line_count: assembler.total_physical_lines(),
        reserved_bytes,
        encoding_route,
        selected_encoding,
        profile_resolution_route,
        detected_datetime_format: assembler
            .detected_datetime_format()
            .map(|format| format.id()),
        datetime_format_route,
        decode_invalid_positions: cursor.decode_invalid_positions.clone(),
        decode_invalid_positions_truncated: cursor.decode_invalid_positions_truncated,
        encoding_warnings,
        fell_back_to_raw_display: assembler.fell_back_to_raw_display(raw_display_due_to_profile),
        has_unconfirmed_trailing_line,
        stage_timings,
    }
}

/// [`reload_source`]・[`restore_evicted_source`] が共有する、ストリーミング
/// 解析結果です（P08-5。`register_source_with_control` は進捗・
/// 伸長を伴う独自のループを持つため、この関数とは別に自前でチャンクごとに
/// 処理します）。
struct StreamedRegistration {
    /// 全項目分の [`PendingItem`]。表示集合（索引・項目列）の構築が終われば
    /// 用済みになる一時バッファであり、呼び出し元
    /// （[`reload_appended`]・[`restore_evicted_source`]）が
    /// `commit_reload`／`commit_restore` を終えた時点で落ちます。容量の事前
    /// 確保と会計への予約は [`stream_decode_and_index`] が行います。
    pending_items: Vec<PendingItem>,
    has_unconfirmed_trailing_line: bool,
    /// このソースで確定した日時書式。`crate::registry` が `timestamp_display`
    /// の再構成用に保持します。生表示・日時なしのソースは `None`。
    datetime_format: Option<LogDateTimeFormat>,
    /// このソースで確定した文字コード。`crate::registry` がオンデマンド
    /// 読み出し時のデコードに使います。
    selected_encoding: SelectedEncoding,
    /// `LOG-022`: この読み直しで日時未解析の生表示へ退避したか。
    /// [`ReloadOutcome::Reloaded`] を通じて `src-tauri` の対象一覧まで運び、
    /// 再解析 UI の出し分けを実際の表示と一致させます。`datetime_format` が
    /// `None` であることとは一致しません（日時付き行が1行も無いファイルは
    /// 生表示になりますが、`LOG-022` の「退避」には当たりません）。
    fell_back_to_raw_display: bool,
}

/// ファイル全体をストリーミングで読み、日時自動判定・継続行結合まで終えた
/// [`PendingItem`] の並びをまとめて返します（`reload_source`・
/// `restore_evicted_source` が使う、一括コミット用の共通経路。P08-5）。
///
/// **本文（生バイト・デコード済み文字列のいずれも）をファイル全体分蓄積する
/// ことはありません。** [`hakutaku_data_source::stream_snapshotted_bytes_chunked`]
/// がチャンクごとに一時的な生バイト列だけを渡し、[`DecodeCursor`] がチャンク
/// サイズ程度の一時デコードで生バイトオフセットを記録し、
/// [`crate::streaming_parse::StreamingAssembler`] が本文を持たない
/// [`PendingItem`]（`Copy`、ヒープ確保なし）だけを蓄積します。
///
/// # 一時バッファの事前確保とメモリ会計
///
/// ただし `PendingItem` 自体は全項目分が1本の `Vec` に溜まります。本文に比べれば
/// 小さい（1件あたり `std::mem::size_of::<PendingItem>()`）ものの、1 GiB 級の
/// ファイルでは数百 MB になり、表示集合の構築が終わるまで常駐分と同時に生き
/// ます。そのため、バッチを追記する前に
/// [`crate::item::ensure_pending_capacity`] で総項目数の外挿
/// （[`estimate_total_items`]。初回登録の [`deliver_batch`] と同じ式）まで容量を
/// 広げ、その伸長分を会計へ予約してから確保します（`PERF-010`）。予約が拒否
/// された場合は事前確保だけを諦め、従来どおりの倍々成長で読み込みを続けます
/// （拒否の回帰を生まないため。`ensure_pending_capacity` の doc コメント参照）。
fn stream_decode_and_index(
    file: std::fs::File,
    path: &Path,
    snapshot: &hakutaku_data_source::FileSnapshot,
    log_profiles: &[hakutaku_config::LogProfileConfig],
) -> Result<StreamedRegistration, LoadFileError> {
    let resolution = resolve_profile(None, path, log_profiles);
    let raw_display_due_to_profile = matches!(
        resolution,
        ResolutionOutcome::Ambiguous { .. } | ResolutionOutcome::ManualNotFound { .. }
    );
    let profile_encoding = profile_encoding_setting(&resolution);
    // 再読み込み・退避復元でも、プロファイルの日時書式指定を同じように効かせる
    // （選択肢1は path_pattern 一致で書式が決まるため、初回登録と同じ結果に
    // なる必要がある）。
    //
    // 決定経路（DatetimeFormatRoute）はここでは決めない。この関数は
    // LoadSummary を返さず、診断ログの読み込みサマリー（DIAG-005）も初回登録の
    // 経路だけが出力するため、記録先が無いからである。手動選択が
    // 届かないこの経路で取り得る値は Profile／Auto／RawDisplayFallback であり、
    // 必要になった時点で resolve_datetime_format_and_route(None, ..) から同じ
    // 規則で得られる。
    let profile_datetime_format = profile_datetime_format(&resolution);

    let mut cursor = DecodeCursor::new(profile_encoding);
    let mut assembler = crate::streaming_parse::StreamingAssembler::new(
        raw_display_due_to_profile,
        profile_datetime_format,
    );
    let poisoned: RefCell<Option<LoadFileError>> = RefCell::new(None);
    let mut pending_items: Vec<PendingItem> = Vec::new();

    let throttle = hakutaku_data_source::IoThrottle::unlimited();
    let is_cancelled = || poisoned.borrow().is_some();

    let chunk_result = {
        let on_chunk = |chunk: &[u8], bytes_done: u64, total_bytes: u64| {
            if poisoned.borrow().is_some() {
                return;
            }
            if let Err(error) = cursor.feed(chunk) {
                *poisoned.borrow_mut() = Some(error);
                return;
            }
            // 上の on_chunk と同じく、行本文は cursor からの借用で複製しない。
            for line in cursor.lines() {
                assembler.feed_line(line.text, line.raw_offset, line.raw_content_len, false);
            }

            let batch = assembler.drain_ready();
            // 追記の**前**に容量を広げる。extend してから広げると、
            // まさに避けたい倍々成長の再確保を一度起こしてからの事前確保になり、
            // 意味がない。外挿の標本には、初回登録の `deliver_batch` と同じく
            // 「このバッチを反映した後の件数」を使う（反映前の件数だと最初の
            // バッチで標本が0件になり、事前確保が効かない）。
            let items_after_batch = pending_items.len().saturating_add(batch.len()) as u64;
            let estimate = estimate_total_items(items_after_batch, bytes_done, total_bytes);
            crate::item::ensure_pending_capacity(&mut pending_items, estimate);
            pending_items.extend(batch);
        };

        hakutaku_data_source::stream_snapshotted_bytes_chunked(
            hakutaku_data_source::ChunkedReadRequest {
                file,
                path,
                snapshot,
                budget: hakutaku_memory_accounting::global_budget(),
                chunk_bytes: hakutaku_data_source::DEFAULT_CHUNK_BYTES,
                throttle: &throttle,
                eager_bytes: snapshot.snapshot_end,
                is_cancelled: &is_cancelled,
            },
            on_chunk,
        )
    };

    match chunk_result {
        Ok(_) => {}
        Err(hakutaku_data_source::ChunkReadError::ChangeDetected(verdict)) => {
            return Err(LoadFileError::ChangedDuringLoad(verdict));
        }
        Err(hakutaku_data_source::ChunkReadError::Read(read_error)) => {
            return Err(LoadFileError::ReadFile(read_error));
        }
    }

    if let Some(error) = poisoned.into_inner() {
        return Err(error);
    }

    cursor.finish()?;
    let has_unconfirmed_trailing_line = cursor.last_line_unconfirmed();
    for line in cursor.lines() {
        assembler.feed_line(
            line.text,
            line.raw_offset,
            line.raw_content_len,
            !line.confirmed,
        );
    }

    assembler.finish();
    let final_batch = assembler.drain_ready();
    // 全量を読み終えているため、ここは外挿ではなく確定値で足りる。それでも
    // 追記の前に容量を合わせ直すのは、事前確保がちょうど埋まっている状態で
    // 保留中の1件を push すると、それだけで倍々成長の再確保（数百 MB 規模の
    // 一時的な二重確保）が起きるためである。
    let total_after_final_batch = pending_items.len().saturating_add(final_batch.len());
    crate::item::ensure_pending_capacity(
        &mut pending_items,
        CapacityEstimate::Exact(total_after_final_batch),
    );
    pending_items.extend(final_batch);

    let datetime_format = assembler.detected_datetime_format();
    let selected_encoding = cursor.selected_encoding().unwrap_or(SelectedEncoding::Utf8);
    // 初回登録の `build_control_load_summary` と同じ式で判定し、再読み込み・
    // 退避復元でも `LoadSummary::fell_back_to_raw_display` と同じ意味の値を
    // 呼び出し側へ返す。
    let fell_back_to_raw_display = assembler.fell_back_to_raw_display(raw_display_due_to_profile);

    Ok(StreamedRegistration {
        pending_items,
        has_unconfirmed_trailing_line,
        datetime_format,
        selected_encoding,
        fell_back_to_raw_display,
    })
}

/// プロファイル解決結果から、文字コード判定へ渡す設定を組み立てます。
///
/// `Manual`／`ExactMatch`／`Glob` はそのプロファイルの `encoding`／
/// `ansi_codepage` をそのまま使います。`NoMatch`（該当プロファイルなし）・
/// `Ambiguous`・`ManualNotFound` はいずれも自動判定
/// （[`hakutaku_format_detection::ProfileEncodingSetting::auto`]）を使います
/// （`Ambiguous`／`ManualNotFound` は日時解析自体を行いませんが、表示のために
/// 文字コードだけは可能な範囲で判定します）。
fn profile_encoding_setting(
    resolution: &ResolutionOutcome,
) -> hakutaku_format_detection::ProfileEncodingSetting {
    match resolution {
        ResolutionOutcome::Manual(profile)
        | ResolutionOutcome::ExactMatch(profile)
        | ResolutionOutcome::Glob(profile) => hakutaku_format_detection::ProfileEncodingSetting {
            named: match &profile.encoding {
                hakutaku_config::EncodingSetting::Auto => None,
                hakutaku_config::EncodingSetting::Named(name) => Some(name.clone()),
            },
            ansi_codepage: profile.ansi_codepage,
        },
        ResolutionOutcome::NoMatch
        | ResolutionOutcome::Ambiguous { .. }
        | ResolutionOutcome::ManualNotFound { .. } => {
            hakutaku_format_detection::ProfileEncodingSetting::auto()
        }
    }
}

/// プロファイル解決結果から、日時解析へ渡す確定書式を組み立てます
/// （`CFG-008`）。
///
/// `Manual`／`ExactMatch`／`Glob` でそのプロファイルが `datetime_format` を
/// 明示している場合だけ `Some` を返します。`Some` を渡された
/// [`crate::streaming_parse::StreamingAssembler`] は、内容による自動判定を
/// 一切行わずその書式で全行を解析します。
///
/// 次の場合はいずれも `None`（＝従来どおりの自動判定）です。
///
/// - プロファイルの `datetime_format` が未指定（`Auto`）
/// - `NoMatch`（該当プロファイルなし）
/// - `Ambiguous`／`ManualNotFound`（プロファイル自体を一意に決められないため、
///   そのプロファイルが持つ書式指定も採用できない。これらは
///   `raw_display_due_to_profile` により生表示へ退避します）
///
/// 戻り値は設定由来の書式だけです。UI での手動選択
/// （[`LoadControl::manual_datetime_format`]）はこの関数を通らず、呼び出し側
/// （[`register_source_with_control`]）で本関数の結果より優先されます。
///
/// # 書式 ID の綴りが二重定義にならないこと
///
/// `hakutaku_config` は `hakutaku-parser` に依存しないため
/// （`crates/config/src/schema.rs` の [`hakutaku_config::DateTimeFormatSetting`]
/// の doc コメント参照）、要件 ID の文字列表はこの2クレートに1つずつあります。
/// 綴りが食い違うと設定と解析結果がずれるため、両者が一致することを
/// このモジュールの単体テスト
/// （`config_datetime_format_ids_match_parser_format_ids`）で確認しています。
fn profile_datetime_format(resolution: &ResolutionOutcome) -> Option<LogDateTimeFormat> {
    let profile = match resolution {
        ResolutionOutcome::Manual(profile)
        | ResolutionOutcome::ExactMatch(profile)
        | ResolutionOutcome::Glob(profile) => profile,
        ResolutionOutcome::NoMatch
        | ResolutionOutcome::Ambiguous { .. }
        | ResolutionOutcome::ManualNotFound { .. } => return None,
    };
    datetime_format_from_setting(profile.datetime_format)
}

/// 日時書式の明示指定を優先順位に従って1つに決め、同時にその決定経路
/// （[`DatetimeFormatRoute`]）を返します。
///
/// 優先順位はモジュール doc コメント「日時書式の決め方」のとおり、UI での手動
/// 選択 ＞ プロファイルの明示指定（`profile_datetime_format`）＞
/// 自動判定（＝明示指定なし。戻り値の書式が `None`）です。手動選択が設定より
/// 弱いと、設定を書かずに開いたファイルをその場の操作だけで解析させるという
/// 目的を果たせません（[`LoadControl::manual_datetime_format`] の doc コメント）。
///
/// **書式と経路を同じ関数で決めるのは、優先順位の実装と診断ログの経路表示が
/// 二重実装になり、片方だけ変更されて食い違うのを防ぐためです。**
///
/// `raw_display_due_to_profile`（`Ambiguous`／`ManualNotFound`）が真のときも、
/// 戻り値の書式は真でないときと同じものを返します。生表示退避との優先関係は
/// `crate::streaming_parse::StreamingAssembler::new` が引き続き判定する
/// （明示指定を渡しても生表示が勝つ）契約であり、ここで書式を落とすとその
/// 契約を二重に実装することになるためです。経路だけは実態に合わせて
/// [`DatetimeFormatRoute::RawDisplayFallback`]（書式を決めに行っていない）に
/// します。
fn resolve_datetime_format_and_route(
    manual_datetime_format: Option<LogDateTimeFormat>,
    resolution: &ResolutionOutcome,
    raw_display_due_to_profile: bool,
) -> (Option<LogDateTimeFormat>, DatetimeFormatRoute) {
    let explicit_datetime_format =
        manual_datetime_format.or_else(|| profile_datetime_format(resolution));

    let route = if raw_display_due_to_profile {
        DatetimeFormatRoute::RawDisplayFallback
    } else if manual_datetime_format.is_some() {
        DatetimeFormatRoute::Manual
    } else if explicit_datetime_format.is_some() {
        // 手動選択が無いときの明示指定は profile_datetime_format の結果だけ。
        // 同じ値から経路を導くことで、書式と経路がずれない。
        DatetimeFormatRoute::Profile
    } else {
        DatetimeFormatRoute::Auto
    };

    (explicit_datetime_format, route)
}

/// 設定側の日時書式指定を、解析側の書式へ写します（`Auto` は `None`）。
fn datetime_format_from_setting(
    setting: hakutaku_config::DateTimeFormatSetting,
) -> Option<LogDateTimeFormat> {
    use hakutaku_config::DateTimeFormatSetting;
    match setting {
        DateTimeFormatSetting::Auto => None,
        DateTimeFormatSetting::LogDt001 => Some(LogDateTimeFormat::LogDt001),
        DateTimeFormatSetting::LogDt002 => Some(LogDateTimeFormat::LogDt002),
        DateTimeFormatSetting::LogDt003 => Some(LogDateTimeFormat::LogDt003),
        DateTimeFormatSetting::LogDt004 => Some(LogDateTimeFormat::LogDt004),
        DateTimeFormatSetting::LogDt005 => Some(LogDateTimeFormat::LogDt005),
        DateTimeFormatSetting::LogDt006 => Some(LogDateTimeFormat::LogDt006),
    }
}

/// 診断ログ表示用に、文字コード判定経路を短い日本語ラベルへ変換します。
///
/// `crates/format-detection` は「実際の診断ログへの出力は呼び出し側（読み込み経路）の
/// 責務」としているため（同クレートの doc コメント参照）、ラベル化はここで
/// 行います。
fn encoding_route_label(route: hakutaku_format_detection::DetectionRoute) -> &'static str {
    use hakutaku_format_detection::{DetectionRoute, ProfileSpecifiedKind};
    match route {
        DetectionRoute::Utf8Bom => "UTF-8 BOM",
        DetectionRoute::Utf8ValidatedNoBom => "UTF-8（BOM無し・妥当性確認）",
        DetectionRoute::ProfileSpecified(ProfileSpecifiedKind::NamedEncoding) => {
            "プロファイル指定（encoding 名前指定）"
        }
        DetectionRoute::ProfileSpecified(ProfileSpecifiedKind::AnsiCodepage) => {
            "プロファイル指定（ansi_codepage）"
        }
        DetectionRoute::EnvironmentAnsi => "実行環境の Windows ANSI コードページ",
    }
}

#[cfg(test)]
mod estimate_tests {
    use super::*;

    // 受け入れ条件: 読み込み途中は、読み終えた範囲の実測平均から
    // 総項目数を外挿し、5%のヘッドルームを上乗せした Projected を返す。
    #[test]
    fn partial_read_projects_total_items_from_measured_average_with_headroom() {
        // 2,000,000 バイトで 20,000 件 = 100 バイト/件。全体 20,000,000 バイト
        // なら 200,000 件と外挿し、5% 上乗せして 210,000 件。
        assert_eq!(
            estimate_total_items(20_000, 2_000_000, 20_000_000),
            CapacityEstimate::Projected(210_000)
        );
    }

    // 受け入れ条件: 標本が MIN_PROJECTION_SAMPLE_BYTES に満たない
    // うちは外挿せず、確定値（＝現時点の件数）を返す。行長のばらつきが大きい
    // 小さな標本で全体を推定しないための下限。
    #[test]
    fn small_sample_is_not_projected_until_the_minimum_is_read() {
        assert_eq!(
            estimate_total_items(10, MIN_PROJECTION_SAMPLE_BYTES - 1, 20_000_000),
            CapacityEstimate::Exact(10)
        );
        // 下限ちょうどからは外挿する（境界値）。
        assert!(matches!(
            estimate_total_items(10, MIN_PROJECTION_SAMPLE_BYTES, 20_000_000),
            CapacityEstimate::Projected(_)
        ));
    }

    // 受け入れ条件: 全量を読み終えていれば外挿もヘッドルームも
    // 行わず、確定値（Exact）を返す。読み込みの最終バッチが必ずこの経路を通り、
    // 途中の見積もりが外れていてもちょうどの容量へ合わせ直せる。
    #[test]
    fn fully_read_file_yields_exact_estimate_without_headroom() {
        assert_eq!(
            estimate_total_items(1_000, 1_000_000, 1_000_000),
            CapacityEstimate::Exact(1_000)
        );
        // 読み終えたバイト数が総バイト数を超える場合（境界の外側）も確定扱い。
        assert_eq!(
            estimate_total_items(1_000, 1_200_000, 1_000_000),
            CapacityEstimate::Exact(1_000)
        );
    }

    // 受け入れ条件: 1件も項目にできていない場合は外挿せず、
    // 事前確保も行わない（Exact(0) は目標容量なしとして扱われる）。
    #[test]
    fn missing_sample_yields_zero_estimate() {
        assert_eq!(
            estimate_total_items(0, 2_000_000, 20_000_000),
            CapacityEstimate::Exact(0)
        );
    }

    // 受け入れ条件: 全量が空（総バイト数0）でもゼロ除算せず、
    // 確定値を返す。
    #[test]
    fn zero_length_file_does_not_divide_by_zero() {
        assert_eq!(estimate_total_items(0, 0, 0), CapacityEstimate::Exact(0));
    }

    // 受け入れ条件: 2000万行・2 GiB 規模でも外挿の乗算が
    // オーバーフローしない（u128 での計算）。
    #[test]
    fn projection_does_not_overflow_at_scale() {
        let total_bytes = 2 * 1024 * 1024 * 1024u64;
        let bytes_done = 8 * 1024 * 1024u64;
        let items_so_far = 68_000u64;
        match estimate_total_items(items_so_far, bytes_done, total_bytes) {
            CapacityEstimate::Projected(estimated) => {
                // 68,000 件 / 8 MiB を 2 GiB へ外挿すると約 1,740 万件。
                assert!(
                    (17_000_000..19_000_000).contains(&estimated),
                    "外挿値が想定の桁に収まっていない: {estimated}"
                );
            }
            CapacityEstimate::Exact(estimated) => {
                panic!("読み込み途中なので Projected のはず: {estimated}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::BudgetRejection;
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
                "hakutaku-core-services-loader-test-{label}-{}-{count}-{nanos}.log",
                std::process::id()
            ));
            std::fs::write(&path, contents).expect("テスト用ファイルを作成できません");
            TempFile { path }
        }

        fn create_text(label: &str, contents: &str) -> Self {
            Self::create(label, contents.as_bytes())
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[test]
    fn loads_file_parses_timestamps_and_registers_a_display_set() {
        let contents = "2026/07/28 15:12:23.456 起動しました\n書式に一致しない行\n";
        let file = TempFile::create_text("basic", contents);
        let mut registry = DisplaySetRegistry::new();

        let (handle, summary) =
            load_file_into_registry(&mut registry, &file.path, "test.log".to_string(), &[])
                .expect("読み込みは成功するはず");

        assert_eq!(handle.generation, 1);
        // 2行目は継続行として1行目へ結合されるため、項目数は1件になる。
        assert_eq!(handle.total_items, 1);
        assert_eq!(summary.line_count, 2);
        assert_eq!(summary.file_size_bytes, contents.len() as u64);
        assert_eq!(summary.detected_datetime_format, Some("LOG-DT-001"));
        assert!(!summary.fell_back_to_raw_display);
        assert_eq!(registry.len(), 1);

        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");

        assert_eq!(response.items.len(), 1);
        assert_eq!(
            response.items[0].timestamp_display.as_deref(),
            Some("2026-07-28T15:12:23.456")
        );
        assert_eq!(response.items[0].source_label, "test.log");
        assert_eq!(
            &*response.items[0].raw_text,
            "2026/07/28 15:12:23.456 起動しました\n書式に一致しない行"
        );
    }

    // 受け入れ条件（`PERF-010`）: 読み込みサマリーの reserved_bytes が、
    // 行数に比例して常駐する構造すべて（索引24 + 行番号8 + 項目24 = 56バイト/件）
    // を会計している。索引分だけの32バイト/件では不足である。
    #[test]
    fn reserved_bytes_accounts_index_auxiliary_and_item_bytes_per_logical_item() {
        let contents = "2026/07/28 15:12:23.456 一件目\n2026/07/28 15:12:24.000 二件目\n\
                        2026/07/28 15:12:25.000 三件目\n";
        let file = TempFile::create_text("reserved-bytes", contents);
        let mut registry = DisplaySetRegistry::new();

        let (handle, summary) =
            load_file_into_registry(&mut registry, &file.path, "test.log".to_string(), &[])
                .expect("読み込みは成功するはず");

        assert_eq!(handle.total_items, 3);
        assert_eq!(
            summary.reserved_bytes,
            3 * crate::item::RESIDENT_BYTES_PER_ITEM
        );
        assert_eq!(summary.reserved_bytes, 168);
    }

    #[test]
    fn missing_file_surfaces_read_file_error() {
        let missing =
            std::env::temp_dir().join("hakutaku-core-services-loader-test-does-not-exist-91af.log");
        let mut registry = DisplaySetRegistry::new();

        let error =
            load_file_into_registry(&mut registry, &missing, "missing.log".to_string(), &[])
                .expect_err("存在しないファイルは失敗するはず");
        assert!(matches!(error, LoadFileError::ReadFile(_)));
        assert!(registry.is_empty(), "失敗時は登録しないはず");
    }

    // 受け入れ条件: ファイル先頭の日時なし行が破棄されず、日時未確定の生データ
    // として扱われる（LOG-014）。
    #[test]
    fn leading_lines_without_datetime_are_kept_as_independent_raw_items() {
        let contents = "起動準備中\n2026/07/28 15:12:23.456 起動しました\n";
        let file = TempFile::create_text("leading-no-datetime", contents);
        let mut registry = DisplaySetRegistry::new();

        let (handle, _summary) =
            load_file_into_registry(&mut registry, &file.path, "test.log".to_string(), &[])
                .expect("読み込みは成功するはず");

        assert_eq!(
            handle.total_items, 2,
            "先頭行は破棄されず独立した項目になる"
        );

        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");

        assert_eq!(&*response.items[0].raw_text, "起動準備中");
        assert_eq!(response.items[0].timestamp_display, None);
        assert_eq!(
            response.items[1].timestamp_display.as_deref(),
            Some("2026-07-28T15:12:23.456")
        );
    }

    // 受け入れ条件: 曖昧な日時（LOG-DT-004 と LOG-DT-005 の同時成立）は生表示へ
    // 退避し、原文がそのまま見える（LOG-022）。
    #[test]
    fn ambiguous_datetime_falls_back_to_raw_display() {
        let contents = "2026/07/28 15:12:23:45 一行目\n2026/07/28 15:12:24:99 二行目\n";
        let file = TempFile::create_text("ambiguous", contents);
        let mut registry = DisplaySetRegistry::new();

        let (handle, summary) =
            load_file_into_registry(&mut registry, &file.path, "test.log".to_string(), &[])
                .expect("読み込みは成功するはず");

        assert!(summary.fell_back_to_raw_display);
        assert_eq!(summary.detected_datetime_format, None);
        assert_eq!(handle.total_items, 2, "生表示では1行=1項目のまま");

        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");
        assert_eq!(response.items[0].timestamp_display, None);
        assert_eq!(
            &*response.items[0].raw_text,
            "2026/07/28 15:12:23:45 一行目"
        );
        assert_eq!(
            &*response.items[1].raw_text,
            "2026/07/28 15:12:24:99 二行目"
        );
    }

    // 受け入れ条件: 一つも日時が見つからなければ全行日時なしとして扱う
    // （LOG-022 の異常系ではなく正常系。1行=1項目のまま）。
    #[test]
    fn no_datetime_anywhere_treats_every_line_as_independent_raw_item() {
        let contents = "起動準備中\n設定を読み込みました\n初期化完了\n";
        let file = TempFile::create_text("no-datetime", contents);
        let mut registry = DisplaySetRegistry::new();

        let (handle, summary) =
            load_file_into_registry(&mut registry, &file.path, "test.log".to_string(), &[])
                .expect("読み込みは成功するはず");

        assert!(
            !summary.fell_back_to_raw_display,
            "曖昧性による退避ではなく、単に日時書式を持たない正常系"
        );
        assert_eq!(summary.detected_datetime_format, None);
        assert_eq!(handle.total_items, 3);
    }

    // 受け入れ条件: 空白数が一定でない区切りでも解析が成立する（LOG-003。
    // 日時以降は原文のまま保持される）。
    #[test]
    fn inconsistent_whitespace_after_datetime_still_parses() {
        let contents = "2026/07/28 15:12:23.456    多めの空白\n2026/07/28 15:12:24.000 通常\n";
        let file = TempFile::create_text("whitespace", contents);
        let mut registry = DisplaySetRegistry::new();

        let (handle, _summary) =
            load_file_into_registry(&mut registry, &file.path, "test.log".to_string(), &[])
                .expect("読み込みは成功するはず");

        assert_eq!(handle.total_items, 2);
        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");
        assert_eq!(
            &*response.items[0].raw_text, "2026/07/28 15:12:23.456    多めの空白",
            "日時以降の空白は原文のまま保持される"
        );
    }

    // 受け入れ条件: ログレベルが存在しない形式でも解析が成立する（LOG-004。
    // 現状 log_level は常に None のまま）。
    #[test]
    fn parsing_succeeds_without_log_level_field() {
        let contents = "2026/07/28 15:12:23.456 レベル表記なしの本文\n";
        let file = TempFile::create_text("no-level", contents);
        let mut registry = DisplaySetRegistry::new();

        let (handle, _summary) =
            load_file_into_registry(&mut registry, &file.path, "test.log".to_string(), &[])
                .expect("読み込みは成功するはず");
        assert_eq!(handle.total_items, 1);
    }

    // 受け入れ条件: 時刻補正が行われず、記録された日時がそのまま使われる
    // （LOG-016・LOG-012）。表示文字列が入力の日時と一致することで確認する。
    #[test]
    fn timestamps_are_not_adjusted_and_reflect_the_recorded_local_time() {
        let contents = "2026/07/28 23:59:59.999 記録された時刻\n";
        let file = TempFile::create_text("no-correction", contents);
        let mut registry = DisplaySetRegistry::new();

        let (handle, _summary) =
            load_file_into_registry(&mut registry, &file.path, "test.log".to_string(), &[])
                .expect("読み込みは成功するはず");
        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");
        assert_eq!(
            response.items[0].timestamp_display.as_deref(),
            Some("2026-07-28T23:59:59.999"),
            "記録された日時がそのまま表示されるはず（補正なし）"
        );
    }

    // 受け入れ条件: ファイルごとに異なるプロファイルを適用できる（LOG-005）。
    // ここでは encoding 名前指定（UTF-8 明示）が使われることを、判定経路
    // （ProfileSpecified）で確認する。
    #[test]
    fn different_files_can_use_different_profiles() {
        let contents = "2026/07/28 15:12:23.456 UTF-8 明示\n";
        let file = TempFile::create_text("per-file-profile", contents);

        let profile = hakutaku_config::LogProfileConfig {
            name: "utf8-profile".to_string(),
            path_pattern: file.path.to_string_lossy().into_owned(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Named("utf-8".to_string()),
            ansi_codepage: None,
            datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
        };
        let mut registry = DisplaySetRegistry::new();

        let (_handle, summary) = load_file_into_registry(
            &mut registry,
            &file.path,
            "test.log".to_string(),
            &[profile],
        )
        .expect("読み込みは成功するはず");

        assert_eq!(summary.profile_resolution_route, "絶対パス完全一致");
        assert_eq!(
            summary.encoding_route,
            "プロファイル指定（encoding 名前指定）"
        );
        assert_eq!(summary.selected_encoding, "utf-8");
    }

    // 受け入れ条件: 手動指定したプロファイル名が見つからない場合
    // （ManualNotFound は現段階では発生しない。resolve_profile への
    // manual_selection は常に None のため）代わりに Ambiguous 経路を確認する。
    // 同一優先度の glob が複数一致する設定は生表示へ退避する。
    #[test]
    fn ambiguous_profile_resolution_falls_back_to_raw_display() {
        let contents = "2026/07/28 15:12:23.456 曖昧なプロファイル\n";
        let file = TempFile::create_text("ambiguous-profile", contents);
        let dir = file.path.parent().unwrap().to_string_lossy().into_owned();

        // 同一優先度で異なるパターン文字列が、同じ対象パスへ同時に一致する
        // ように仕組む（*.log と <basename の先頭1文字>*.log）。
        let file_name = file
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let first_char = file_name.chars().next().unwrap();
        let profiles = vec![
            hakutaku_config::LogProfileConfig {
                name: "glob-a".to_string(),
                path_pattern: format!("{dir}\\*.log"),
                priority: 5,
                encoding: hakutaku_config::EncodingSetting::Auto,
                ansi_codepage: None,
                datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
            },
            hakutaku_config::LogProfileConfig {
                name: "glob-b".to_string(),
                path_pattern: format!("{dir}\\{first_char}*.log"),
                priority: 5,
                encoding: hakutaku_config::EncodingSetting::Auto,
                ansi_codepage: None,
                datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
            },
        ];
        let mut registry = DisplaySetRegistry::new();

        let (handle, summary) =
            load_file_into_registry(&mut registry, &file.path, "test.log".to_string(), &profiles)
                .expect("読み込みは成功するはず（生表示へ退避するだけで失敗ではない）");

        assert!(summary.fell_back_to_raw_display);
        assert_eq!(
            summary.profile_resolution_route,
            "曖昧（同一優先度の glob が複数一致）"
        );
        assert_eq!(handle.total_items, 1);
        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");
        assert_eq!(response.items[0].timestamp_display, None);
        assert_eq!(&*response.items[0].raw_text, contents.trim_end());
    }

    // 受け入れ条件: CP932 の日本語が ansi_codepage: 932 指定で正しくデコード
    // される（統合経路。実データではなくテストコード内で組み立てたバイト列）。
    #[test]
    fn cp932_profile_decodes_japanese_text_correctly() {
        // "2026/07/28 15:12:23.456 " は ASCII、続く "日本語" が CP932 のバイト列。
        let mut bytes = b"2026/07/28 15:12:23.456 ".to_vec();
        bytes.extend_from_slice(&[0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA]); // 「日本語」
        let file = TempFile::create("cp932", &bytes);

        let profile = hakutaku_config::LogProfileConfig {
            name: "cp932-profile".to_string(),
            path_pattern: file.path.to_string_lossy().into_owned(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Auto,
            ansi_codepage: Some(932),
            datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
        };
        let mut registry = DisplaySetRegistry::new();

        let (handle, summary) = load_file_into_registry(
            &mut registry,
            &file.path,
            "test.log".to_string(),
            &[profile],
        )
        .expect("読み込みは成功するはず");

        assert_eq!(summary.selected_encoding, "windows-932");
        assert!(summary.decode_invalid_positions.is_empty());
        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");
        assert_eq!(
            &*response.items[0].raw_text,
            "2026/07/28 15:12:23.456 日本語"
        );
    }

    // 受け入れ条件: デコードできないバイト列で、位置と選択された文字コードが
    // メタデータとして返り、元バイトが破棄されない（decode 自体は
    // format-detection 側で検証済みのため、ここでは統合経路での伝播のみ確認）。
    #[test]
    fn undecodable_bytes_are_reported_in_summary_without_failing_the_load() {
        let mut bytes = b"2026/07/28 15:12:23.456 OK:".to_vec();
        bytes.push(0xFF); // UTF-8 として単独では不正なバイト。
        bytes.extend_from_slice(b":END\n");
        let file = TempFile::create("invalid-bytes", &bytes);

        // 実行環境の既定 ANSI コードページに左右されず UTF-8 判定経路を確実に
        // 通すため、encoding を明示指定する（auto 判定は BOM もバイト列全体の
        // UTF-8 妥当性も満たさないため、環境依存の ANSI 判定へフォールバック
        // してしまい、このテストの意図と食い違う）。
        let profile = hakutaku_config::LogProfileConfig {
            name: "utf8-explicit".to_string(),
            path_pattern: file.path.to_string_lossy().into_owned(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Named("utf-8".to_string()),
            ansi_codepage: None,
            datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
        };
        let mut registry = DisplaySetRegistry::new();

        let (_handle, summary) = load_file_into_registry(
            &mut registry,
            &file.path,
            "test.log".to_string(),
            &[profile],
        )
        .expect("不正バイトがあっても読み込み自体は成功するはず");

        assert_eq!(summary.selected_encoding, "utf-8");
        assert!(
            !summary.decode_invalid_positions.is_empty(),
            "不正バイトの位置が報告されるはず"
        );
        assert!(!summary.decode_invalid_positions_truncated);
    }

    // 受け入れ条件: UTF-16 の BOM を検出すると未対応形式として通知される
    // （ENC-006）。
    #[test]
    fn utf16_bom_is_reported_as_unsupported_encoding() {
        let bytes: Vec<u8> = vec![0xFF, 0xFE, b'a', 0x00, b'\n', 0x00];
        let file = TempFile::create("utf16", &bytes);
        let mut registry = DisplaySetRegistry::new();

        let error = load_file_into_registry(&mut registry, &file.path, "test.log".to_string(), &[])
            .expect_err("UTF-16 は未対応のはず");
        assert!(matches!(error, LoadFileError::UnsupportedEncoding(_)));
        assert!(registry.is_empty(), "失敗時は登録しないはず");
    }

    // 受け入れ条件: BOM あり UTF-8 も正しく判定・デコードされる。
    #[test]
    fn utf8_bom_is_detected_and_stripped() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("2026/07/28 15:12:23.456 BOM付き\n".as_bytes());
        let file = TempFile::create("utf8-bom", &bytes);
        let mut registry = DisplaySetRegistry::new();

        let (handle, summary) =
            load_file_into_registry(&mut registry, &file.path, "test.log".to_string(), &[])
                .expect("読み込みは成功するはず");

        assert_eq!(summary.encoding_route, "UTF-8 BOM");
        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");
        assert_eq!(
            &*response.items[0].raw_text,
            "2026/07/28 15:12:23.456 BOM付き"
        );
    }

    // --- register_source（P06: 複数ソースの登録と上限判定） ---

    // 受け入れ条件: 複数のソースを登録でき、各ソースに source_id と来歴が付く。
    #[test]
    fn register_source_registers_multiple_sources_with_distinct_ids() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file_a = TempFile::create_text("register-a", "2026/07/28 15:12:23.456 a\n");
        let file_b = TempFile::create_text("register-b", "2026/07/28 15:12:24.000 b\n");

        let (handle_a, _) = register_source(
            &mut registry,
            &budget,
            &file_a.path,
            "a.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");
        let (handle_b, _) = register_source(
            &mut registry,
            &budget,
            &file_b.path,
            "b.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        assert_ne!(handle_a.source_id, handle_b.source_id);
        assert_eq!(registry.list_sources().len(), 2);
        assert_eq!(budget.count(), 2);
    }

    // 受け入れ条件（LOG-027）: 共有を許可しない方法（FileShare.None 相当。
    // share_mode(0)）で開かれた対象への register_source は、他の I/O エラー
    // とは区別された共有違反として失敗する。他のソース（既に開いている a.log）
    // の閲覧は継続する。相手（ロック）を閉じた後の再試行（register_source の
    // 再呼び出し）が成功する。
    #[test]
    fn register_source_distinguishes_sharing_violation_and_leaves_other_sources_unaffected() {
        use std::os::windows::fs::OpenOptionsExt;

        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let other = TempFile::create_text("register-sharing-violation-other", "a\n");
        let locked = TempFile::create_text(
            "register-sharing-violation-locked",
            "2026/07/28 15:12:23.456 locked\n",
        );

        let (handle_other, _) = register_source(
            &mut registry,
            &budget,
            &other.path,
            "other.log".to_string(),
            &[],
        )
        .expect("先に開いた対象は成功するはず");

        let locker = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&locked.path)
            .expect("排他的に開けるはず（LOG-027 再現用の前提）");

        let error = register_source(
            &mut registry,
            &budget,
            &locked.path,
            "locked.log".to_string(),
            &[],
        )
        .expect_err("共有違反のため失敗するはず");
        assert!(
            error.is_sharing_violation(),
            "共有違反として区別されるはず: {error:?}"
        );
        // registry・budget の状態は変えていない（拒否時と同じ扱い）。
        assert_eq!(registry.list_sources().len(), 1);

        // 他のソース（other.log）の閲覧は継続する（ERR-001）。
        let response = registry
            .fetch_range(
                handle_other.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle_other.generation,
                },
            )
            .expect("影響を受けず取得できるはず");
        assert_eq!(response.items.len(), 1);

        // ロックを解除すれば再試行（register_source の再呼び出し）が成功する。
        drop(locker);
        let (_handle_locked, _summary) = register_source(
            &mut registry,
            &budget,
            &locked.path,
            "locked.log".to_string(),
            &[],
        )
        .expect("ロック解除後は成功するはず");
        assert_eq!(registry.list_sources().len(), 2);
    }

    // 受け入れ条件: 単一 1 GB 超のファイルは拒否され、registry・budget の状態が
    // 変わらない（既に開いているファイルの表示は維持される）。テストでは
    // SourceBudget::with_limits で小さい上限を注入し、1 GB 級の実ファイルを
    // 用意せずに判定ロジックを検証する。
    #[test]
    fn register_source_rejects_file_over_single_limit_without_side_effects() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::with_limits(1_000_000, 5, 10);
        let file = TempFile::create_text("register-too-large", "123456\n"); // 7バイト > 上限5

        let error = register_source(
            &mut registry,
            &budget,
            &file.path,
            "big.log".to_string(),
            &[],
        )
        .expect_err("単一ファイル上限を超えるので拒否されるはず");
        assert!(matches!(
            error,
            RegisterSourceError::BudgetRejected(BudgetRejection::SingleFileTooLarge { .. })
        ));
        assert!(registry.is_empty(), "拒否時は登録しないはず");
        assert_eq!(budget.total_bytes(), 0, "拒否時は予約しないはず");
    }

    // 受け入れ条件: 合計上限を超える追加は拒否され、既存ソースの表示が維持
    // される。
    #[test]
    fn register_source_rejects_when_total_limit_exceeded_and_keeps_existing_source() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::with_limits(10, 1_000_000, 10);
        let first = TempFile::create_text("register-total-first", "12345\n"); // 6バイト
        let second = TempFile::create_text("register-total-second", "12345678901\n"); // 12バイト

        let (handle_first, _) = register_source(
            &mut registry,
            &budget,
            &first.path,
            "first.log".to_string(),
            &[],
        )
        .expect("合計上限(10)以内なので成功するはず");

        let error = register_source(
            &mut registry,
            &budget,
            &second.path,
            "second.log".to_string(),
            &[],
        )
        .expect_err("合計上限を超えるので拒否されるはず");
        assert!(matches!(
            error,
            RegisterSourceError::BudgetRejected(BudgetRejection::TotalTooLarge { .. })
        ));

        // 既存ソース（first）の表示は維持される。
        assert_eq!(registry.list_sources().len(), 1);
        let response = registry
            .fetch_range(
                handle_first.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle_first.generation,
                },
            )
            .expect("既存ソースは引き続き取得できるはず");
        assert_eq!(response.items.len(), 1);
    }

    // 受け入れ条件: 11ファイル目は拒否される（PERF-005）。
    #[test]
    fn register_source_rejects_the_eleventh_file() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::with_limits(u64::MAX, u64::MAX, 10);

        let mut files = Vec::new();
        for index in 0..10 {
            let file = TempFile::create_text(&format!("register-count-{index}"), "x\n");
            register_source(
                &mut registry,
                &budget,
                &file.path,
                format!("{index}.log"),
                &[],
            )
            .expect("10件目までは成功するはず");
            files.push(file);
        }

        let eleventh = TempFile::create_text("register-count-11", "x\n");
        let error = register_source(
            &mut registry,
            &budget,
            &eleventh.path,
            "11.log".to_string(),
            &[],
        )
        .expect_err("11件目はファイル数上限で拒否されるはず");
        assert!(matches!(
            error,
            RegisterSourceError::BudgetRejected(BudgetRejection::TooManySources { .. })
        ));
        assert_eq!(registry.list_sources().len(), 10);
    }

    // 受け入れ条件: register_source が記録する snapshot_end（summary・budget の
    // 両方）は登録時点の値に固定され、登録後の追記で遡って変化しない
    // （ADR-0007）。読み込み自体が snapshot_end を超えて読まないことの厳密な
    // 境界確認は `hakutaku_data_source::snapshot` の
    // `read_snapshotted_bytes_does_not_read_past_snapshot_end` で行っている
    // （こちらはスナップショット取得後に追記してから読み込む、より直接的な
    // 手順を取れる）。
    #[test]
    fn register_source_snapshot_end_is_not_affected_by_later_appends() {
        use std::io::Write;

        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text(
            "register-snapshot-end",
            "2026/07/28 15:12:23.456 before snapshot\n",
        );

        let (_handle, summary) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        let expected_len = "2026/07/28 15:12:23.456 before snapshot\n".len() as u64;
        assert_eq!(summary.file_size_bytes, expected_len);

        {
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記用に開けるはず");
            writer
                .write_all(b"appended after registration\n")
                .expect("追記できるはず");
        }

        // budget に計上された合計は、登録時点の snapshot_end のままである
        // （追記分を勝手に計上しない）。
        assert_eq!(budget.total_bytes(), expected_len);
    }

    // 受け入れ条件: 末尾が改行で終わらないファイルは未確定行として扱われ、
    // 解析エラーにならない（LOG-026）。
    #[test]
    fn register_source_marks_unconfirmed_trailing_line_without_parse_error() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text(
            "register-unconfirmed",
            "2026/07/28 15:12:23.456 confirmed line\nunconfirmed tail without newline",
        );

        let (handle, summary) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("末尾が未確定でも解析エラーにはならないはず");

        assert!(summary.has_unconfirmed_trailing_line);
        assert!(registry.list_sources()[0].has_unconfirmed_trailing_line);

        // 断片は破棄されず、継続行として保持されている。
        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");
        assert!(response.items[0]
            .raw_text
            .contains("unconfirmed tail without newline"));
    }

    // 末尾が改行で終わるファイルは未確定行にならない。
    #[test]
    fn register_source_does_not_mark_confirmed_file_as_unconfirmed() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text(
            "register-confirmed",
            "2026/07/28 15:12:23.456 confirmed line\n",
        );

        let (_handle, summary) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        assert!(!summary.has_unconfirmed_trailing_line);
    }

    // --- reload_source（P06-5: 明示的な再読み込み。LOG-028、ADR-0007） ---

    use crate::registry::SourceStatus;

    // 受け入れ条件: 追記後に reload_source を呼ぶと、新しい内容が反映され
    // 世代が進む（LOG-028）。呼ぶまでは反映されない（LOG-010: リアルタイム
    // 追従はしない）ことも合わせて確認する。
    #[test]
    fn reload_source_reflects_appended_content_only_after_explicit_call() {
        use std::io::Write;

        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file =
            TempFile::create_text("reload-append", "2026/07/28 15:12:23.456 before reload\n");

        let (handle, _summary) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");
        assert_eq!(handle.total_items, 1);

        {
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記用に開けるはず");
            writer
                .write_all(b"2026/07/28 15:12:24.000 appended line\n")
                .expect("追記できるはず");
        }

        // LOG-010: 明示的に reload_source を呼ぶまでは反映されない
        // （追記後も旧世代のまま取得できることで確認する）。
        let stale_response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("追記だけでは世代が変わらないので成功するはず");
        assert_eq!(stale_response.items.len(), 1, "追記はまだ反映されないはず");

        let outcome = reload_source(&mut registry, &budget, handle.source_id, &[])
            .expect("登録済みのソースなので Some のはず");
        let (generation, total_items) = match outcome {
            ReloadOutcome::Reloaded {
                generation,
                total_items,
                ..
            } => (generation, total_items),
            other => panic!("Reloaded を期待したが {other:?} だった"),
        };
        assert_eq!(total_items, 2, "追記された行が反映されるはず");
        assert!(generation > handle.generation, "世代が進むはず");

        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: generation,
                },
            )
            .expect("最新世代なので成功するはず");
        assert_eq!(response.items.len(), 2);
        assert!(response.items[1].raw_text.contains("appended line"));

        // budget にも新しいサイズが反映されている。
        assert_eq!(
            budget.total_bytes(),
            std::fs::metadata(&file.path).unwrap().len()
        );
    }

    // 受け入れ条件: snapshot_end を固定し、それより先に追記された分はその回
    // では読まない。2回目の追記は、2回目の reload_source を呼ぶまで反映
    // されない。
    #[test]
    fn reload_source_fixes_snapshot_end_and_ignores_appends_after_observation() {
        use std::io::Write;

        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text("reload-snapshot-end", "2026/07/28 15:12:23.456 a\n");

        let (handle, _) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        let append = |text: &[u8]| {
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記用に開けるはず");
            writer.write_all(text).expect("追記できるはず");
        };

        append(b"2026/07/28 15:12:24.000 b\n");
        let first_reload =
            reload_source(&mut registry, &budget, handle.source_id, &[]).expect("登録済みのはず");
        let first_generation = match first_reload {
            ReloadOutcome::Reloaded {
                generation,
                total_items,
                ..
            } => {
                assert_eq!(total_items, 2);
                generation
            }
            other => panic!("Reloaded を期待したが {other:?} だった"),
        };

        // ここでさらに追記する。この分は「次回の再読み込み」まで反映しない。
        append(b"2026/07/28 15:12:25.000 c\n");

        let stale = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: first_generation,
                },
            )
            .expect("2回目の追記はまだ反映されないので世代は変わらないはず");
        assert_eq!(stale.items.len(), 2, "2回目の追記はまだ反映されないはず");

        let second_reload =
            reload_source(&mut registry, &budget, handle.source_id, &[]).expect("登録済みのはず");
        match second_reload {
            ReloadOutcome::Reloaded { total_items, .. } => assert_eq!(total_items, 3),
            other => panic!("Reloaded を期待したが {other:?} だった"),
        }
    }

    // 受け入れ条件: 変化がない場合は Reloaded を返すが、世代は進まない
    // （読み直す必要がないため）。
    #[test]
    fn reload_source_with_no_change_reports_reloaded_without_advancing_generation() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text("reload-unchanged", "2026/07/28 15:12:23.456 a\n");

        let (handle, _) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        let outcome =
            reload_source(&mut registry, &budget, handle.source_id, &[]).expect("登録済みのはず");
        match outcome {
            ReloadOutcome::Reloaded {
                generation,
                total_items,
                fell_back_to_raw_display,
            } => {
                assert_eq!(
                    generation, handle.generation,
                    "変化がなければ世代は進まない"
                );
                assert_eq!(total_items, handle.total_items);
                assert_eq!(
                    fell_back_to_raw_display, None,
                    "表示集合を作り直していないので、生表示退避の判定も据え置きになる"
                );
            }
            other => panic!("Reloaded を期待したが {other:?} だった"),
        }
    }

    // 受け入れ条件: 再読み込みで合計が上限を超える場合、再読み込み全体が
    // 拒否され、現在の合計・見込み・上限・超過量が示される（暫定設計、
    // ADR-0007）。旧スナップショットの表示（世代・項目）は維持され、ソースの
    // 「更新未反映」フラグが立つ。
    #[test]
    fn reload_source_rejects_when_appended_size_exceeds_total_limit_and_keeps_old_display() {
        use std::io::Write;

        let mut registry = DisplaySetRegistry::new();
        // 登録時点のサイズ（"2026/07/28 15:12:23.456 a\n" の長さ）ちょうどを
        // 上限にする。追記すると必ず上限を超える。
        let file = TempFile::create_text("reload-over-limit", "2026/07/28 15:12:23.456 a\n");
        let original_len = std::fs::metadata(&file.path).unwrap().len();
        // 単一ファイル上限は十分大きく取り、合計上限（TotalTooLarge）だけが
        // 効くようにする（PERF-004 の判定と切り分けるため）。
        let budget = crate::budget::SourceBudget::with_limits(original_len, original_len * 10, 10);

        let (handle, _) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("登録時点はちょうど上限以内なので成功するはず");
        assert_eq!(budget.total_bytes(), original_len);

        {
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記用に開けるはず");
            writer.write_all(b"more\n").expect("追記できるはず");
        }
        let new_len = std::fs::metadata(&file.path).unwrap().len();
        assert!(new_len > original_len);

        let outcome =
            reload_source(&mut registry, &budget, handle.source_id, &[]).expect("登録済みのはず");
        match outcome {
            ReloadOutcome::RejectedOverLimit(BudgetRejection::TotalTooLarge {
                current_total_bytes,
                requested_bytes,
                limit_bytes,
                excess_bytes,
            }) => {
                assert_eq!(current_total_bytes, original_len, "現在の合計");
                assert_eq!(requested_bytes, new_len, "見込み（再読み込み後のサイズ）");
                assert_eq!(limit_bytes, original_len, "上限");
                assert_eq!(excess_bytes, new_len - original_len, "超過量");
            }
            other => panic!("RejectedOverLimit(TotalTooLarge) を期待したが {other:?} だった"),
        }

        // 部分読み込みは採らない: budget・登録済みサイズは旧サイズのまま。
        assert_eq!(budget.total_bytes(), original_len);

        // 旧スナップショットの表示（世代・項目数）が維持される。
        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("旧世代のまま維持されるはず");
        assert_eq!(response.items.len(), 1);

        // 「更新未反映」フラグが立つ。
        let summary = registry
            .list_sources()
            .into_iter()
            .find(|summary| summary.source_id == handle.source_id)
            .expect("登録済みのはず");
        assert!(summary.update_pending, "更新未反映フラグが立つはず");
        assert_eq!(
            summary.status,
            SourceStatus::Loaded,
            "上限拒否はソース状態を Loaded のまま維持する（表示は有効なまま）"
        );
    }

    // 受け入れ条件: 削除・切り詰め・置換を検知した場合、LOG-023 どおり索引が
    // 無効化され、従来索引が有効扱いで維持されない（上限拒否とは異なる経路）。
    #[test]
    fn reload_source_invalidates_on_shrink_even_though_size_decreases() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text("reload-shrink", "0123456789\n");

        let (handle, _) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        {
            let writer = std::fs::OpenOptions::new()
                .write(true)
                .open(&file.path)
                .expect("書き込み用に開けるはず");
            writer.set_len(3).expect("切り詰めできるはず");
        }

        let outcome =
            reload_source(&mut registry, &budget, handle.source_id, &[]).expect("登録済みのはず");
        assert!(matches!(
            outcome,
            ReloadOutcome::Changed(crate::registry::ChangeKind::Shrunk)
        ));
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(SourceStatus::Changed(crate::registry::ChangeKind::Shrunk))
        );

        // 索引が無効化される（世代が進み、項目が空になる。従来索引を有効
        // 扱いで維持しない）。
        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation + 1,
                },
            )
            .expect("新しい世代では成功するはず");
        assert_eq!(response.items.len(), 0);
    }

    // 受け入れ条件: 別ファイルへの置換も LOG-023 どおり無効化される。
    #[test]
    fn reload_source_invalidates_on_replace() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text("reload-replace", "original content\n");

        let (handle, _) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        std::fs::remove_file(&file.path).expect("削除できるはず");
        std::fs::write(&file.path, b"different content\n").expect("再作成できるはず");

        let outcome =
            reload_source(&mut registry, &budget, handle.source_id, &[]).expect("登録済みのはず");
        assert!(matches!(
            outcome,
            ReloadOutcome::Changed(crate::registry::ChangeKind::Replaced)
        ));
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(SourceStatus::Changed(crate::registry::ChangeKind::Replaced))
        );
    }

    // 受け入れ条件: 削除も LOG-023 どおり無効化される（reload_source が
    // 再オープンの失敗を Deleted として区別する経路）。
    #[test]
    fn reload_source_invalidates_on_delete() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text("reload-delete", "gone soon\n");

        let (handle, _) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        std::fs::remove_file(&file.path).expect("削除できるはず");

        let outcome =
            reload_source(&mut registry, &budget, handle.source_id, &[]).expect("登録済みのはず");
        assert!(matches!(
            outcome,
            ReloadOutcome::Changed(crate::registry::ChangeKind::Deleted)
        ));
    }

    // 受け入れ条件（LOG-027）: 共有違反により再読み込みできない場合、再試行
    // 可能な SharingViolation として区別され、旧スナップショットの表示は
    // 維持される。ロックを解除した後の再試行（reload_source の再呼び出し）
    // が成功することも確認する。
    #[test]
    fn reload_source_distinguishes_sharing_violation_and_allows_retry_after_unlock() {
        use std::os::windows::fs::OpenOptionsExt;

        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text("reload-sharing-violation", "2026/07/28 15:12:23.456 a\n");

        let (handle, _) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        let locker = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file.path)
            .expect("排他的に開けるはず（LOG-027 再現用の前提）");

        let outcome =
            reload_source(&mut registry, &budget, handle.source_id, &[]).expect("登録済みのはず");
        assert!(matches!(outcome, ReloadOutcome::SharingViolation));
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(SourceStatus::SharingViolation)
        );

        // 旧スナップショットの表示（項目・世代）は維持される。
        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("旧世代のまま維持されるはず");
        assert_eq!(response.items.len(), 1);

        drop(locker);
        let retried =
            reload_source(&mut registry, &budget, handle.source_id, &[]).expect("登録済みのはず");
        assert!(
            matches!(retried, ReloadOutcome::Reloaded { .. }),
            "ロック解除後は再試行できるはず（LOG-027）: {retried:?}"
        );
    }

    // 未登録の source_id に対する reload_source は None を返す。
    #[test]
    fn reload_source_for_unknown_source_id_returns_none() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        assert!(reload_source(&mut registry, &budget, 999, &[]).is_none());
    }

    // 受け入れ条件（ERR-003）: register_source・reload_source のいずれも、
    // 参照元ファイルの内容と最終更新時刻を変化させない（Hakutaku 自身の
    // 操作による変化がないことの確認。他プロセスによる追記そのものは対象外）。
    #[test]
    fn register_and_reload_do_not_modify_the_referenced_file() {
        use std::io::Write;

        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text("err-003", "2026/07/28 15:12:23.456 a\n");

        let snapshot_of = |path: &std::path::Path| -> (Vec<u8>, std::time::SystemTime) {
            let bytes = std::fs::read(path).expect("読めるはず（テスト用の確認）");
            let modified = std::fs::metadata(path)
                .expect("メタデータを取得できるはず")
                .modified()
                .expect("最終更新時刻を取得できるはず");
            (bytes, modified)
        };

        let before_register = snapshot_of(&file.path);
        let (handle, _) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "test.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");
        let after_register = snapshot_of(&file.path);
        assert_eq!(
            before_register, after_register,
            "register_source は参照元ファイルを変更しないはず（ERR-003）"
        );

        // 「他プロセス（ログを書き出す業務ソフトウェア相当）」による追記を模して、Hakutaku とは
        // 無関係な書き込みを行う。
        {
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記用に開けるはず");
            writer
                .write_all(b"2026/07/28 15:12:24.000 b\n")
                .expect("追記できるはず");
        }
        let before_reload = snapshot_of(&file.path);

        let outcome =
            reload_source(&mut registry, &budget, handle.source_id, &[]).expect("登録済みのはず");
        assert!(matches!(outcome, ReloadOutcome::Reloaded { .. }));

        let after_reload = snapshot_of(&file.path);
        assert_eq!(
            before_reload, after_reload,
            "reload_source も参照元ファイルを変更しないはず（ERR-003）"
        );
    }

    // --- restore_evicted_source（P08-3→P08-5 でしきい値到達時の
    //     解放をキャッシュのクリアへ単純化。restore_evicted_source 自体は
    //     互換性のため残しています） ---

    // 受け入れ条件（P08-5）: evict_inactive_sources はもはやデコード済み
    // チャンクキャッシュをクリアするだけであり、ソース状態・世代・項目を
    // 変更しません。解放直後も同一世代のまま、本文がオンデマンドで正しく
    // 再読み出しできることを確認します（`crate::registry` の doc コメント
    // 「P08-3 → P08-5: しきい値到達時の解放の単純化」参照）。
    #[test]
    fn evict_inactive_sources_no_longer_invalidates_items_after_p08_5() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let contents =
            "2026/07/28 15:12:23.456 起動しました\n継続行1\n2026/07/28 15:12:24.000 次の項目\n";
        let file = TempFile::create_text("evict-p08-5", contents);

        let (handle, _summary) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "evict.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        let before = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("成功するはず");
        assert_eq!(before.items.len(), 2);

        // しきい値到達を模して、このソースを非アクティブとして解放する
        // （active_source_id を設定しないため「アクティブなソースはない」扱い
        // になり、Loaded な全ソースが対象になる）。
        let evicted = registry.evict_inactive_sources();
        assert_eq!(evicted, vec![handle.source_id]);
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(SourceStatus::Loaded),
            "P08-5 以降、キャッシュのクリアだけなので状態は変わらないはず"
        );

        // 世代不変のまま、同じ内容がオンデマンドで再取得できる
        // （キャッシュがクリアされたのでファイルへ再アクセスするはず）。
        let after_evict = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("世代は変わっていないので成功するはず");
        assert_eq!(after_evict, before);
    }

    // 受け入れ条件: restore_evicted_source は（ソース状態に関わらず）ファイルが
    // 変化していなければ強制的に再読み込みし、世代を1つ進めます（互換性の
    // ために残した経路。`crate::registry::commit_restore` の doc コメント
    // 「世代は必ず1つ進めます」参照）。復元後の内容は元と同一です。
    #[test]
    fn restore_evicted_source_forces_a_reload_and_advances_generation() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let contents =
            "2026/07/28 15:12:23.456 起動しました\n継続行1\n2026/07/28 15:12:24.000 次の項目\n";
        let file = TempFile::create_text("restore-unchanged", contents);

        let (handle, _summary) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "restore.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");

        let before = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: handle.generation,
                },
            )
            .expect("成功するはず");
        assert_eq!(before.items.len(), 2);

        let outcome = restore_evicted_source(&mut registry, handle.source_id, &[])
            .expect("登録済みのソースなので Some のはず");
        let (restored_generation, restored_total) = match outcome {
            ReloadOutcome::Reloaded {
                generation,
                total_items,
                ..
            } => (generation, total_items),
            other => panic!("Reloaded を期待したが {other:?} だった"),
        };
        assert!(
            restored_generation > handle.generation,
            "世代が進んでいるはず"
        );
        assert_eq!(restored_total, 2);
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(SourceStatus::Loaded)
        );

        let after_restore = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: restored_generation,
                },
            )
            .expect("復元後は最新世代で取得できるはず");

        // 内容（識別子以外）はファイルが変化していないので復元前と同一。
        assert_eq!(after_restore.items.len(), before.items.len());
        for (restored, original) in after_restore.items.iter().zip(before.items.iter()) {
            assert_eq!(restored.raw_text, original.raw_text);
            assert_eq!(restored.timestamp_display, original.timestamp_display);
            assert_eq!(restored.source_line_number, original.source_line_number);
            assert_eq!(restored.confirmed, original.confirmed);
            assert_eq!(restored.continuation_count, original.continuation_count);
            assert_eq!(restored.raw_display, original.raw_display);
        }
    }

    // 受け入れ条件: 復元前に削除されていた場合は LOG-023 と同じ無効化経路
    // （Changed）になる。
    #[test]
    fn restore_evicted_source_detects_deletion_as_changed() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text("restore-deleted", "2026/07/28 15:12:23.456 消える\n");

        let (handle, _summary) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "restore-deleted.log".to_string(),
            &[],
        )
        .expect("登録は成功するはず");
        registry.evict_inactive_sources();

        std::fs::remove_file(&file.path).expect("削除できるはず");

        let outcome = restore_evicted_source(&mut registry, handle.source_id, &[])
            .expect("登録済みのソースなので Some のはず");
        assert!(matches!(
            outcome,
            ReloadOutcome::Changed(ChangeKind::Deleted)
        ));
        assert_eq!(
            registry.source_status(handle.source_id),
            Some(SourceStatus::Changed(ChangeKind::Deleted))
        );
    }

    // 未登録の source_id に対する restore_evicted_source は None を返す。
    #[test]
    fn restore_evicted_source_for_unknown_id_returns_none() {
        let mut registry = DisplaySetRegistry::new();
        assert!(restore_evicted_source(&mut registry, 999, &[]).is_none());
    }

    // 受け入れ条件（`PERF-010`）: 再読み込み・退避復元が共有する
    // `stream_decode_and_index` の一時バッファ（Vec<PendingItem>）が事前確保
    // され、倍々成長の余剰容量を持たない。
    //
    // このファイルは外挿の下限（MIN_PROJECTION_SAMPLE_BYTES）に満たないため、
    // 各バッチの見積もりは常に確定値になり、余剰は0件で決定的に定まる。
    // 最後の1件（`assembler.finish` で確定する保留中の項目）を追記するときの
    // 事前確保が抜けていると、そこだけ倍々成長して容量が2倍になるため、
    // 「容量 == 件数」はチャンク内・最終バッチの両方の事前確保を同時に確認する。
    #[test]
    fn stream_decode_and_index_preallocates_the_pending_buffer_without_growth_waste() {
        let mut contents = String::new();
        for i in 0..200 {
            contents.push_str(&format!(
                "2026/07/28 15:12:{:02}.000 メッセージ{i}\n",
                i % 60
            ));
        }
        let file = TempFile::create_text("pending-capacity", &contents);

        let (opened, snapshot) = hakutaku_data_source::reopen_for_reload(&file.path)
            .expect("テスト用ファイルは開けるはず");
        let streamed = stream_decode_and_index(opened, &file.path, &snapshot, &[])
            .expect("読み込みは成功するはず");

        assert_eq!(streamed.pending_items.len(), 200);
        assert_eq!(
            streamed.pending_items.capacity(),
            streamed.pending_items.len(),
            "事前確保が効いていれば、一時バッファに倍々成長の余剰は出ない"
        );
    }

    // 受け入れ条件: 事前確保を入れても、再読み込みの結果
    // （項目数・追記分の反映）は変わらない。拒否の回帰がないことの最小確認も
    // 兼ねる（既定予算のまま、従来どおり再読み込みが成功する）。
    #[test]
    fn stream_decode_and_index_result_is_unchanged_after_appending() {
        let mut contents = String::new();
        for i in 0..50 {
            contents.push_str(&format!("2026/07/28 15:13:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("pending-append", &contents);

        let (opened, snapshot) = hakutaku_data_source::reopen_for_reload(&file.path)
            .expect("テスト用ファイルは開けるはず");
        let before = stream_decode_and_index(opened, &file.path, &snapshot, &[])
            .expect("読み込みは成功するはず");
        assert_eq!(before.pending_items.len(), 50);

        {
            use std::io::Write;
            let mut appender = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記のために開けるはず");
            appender
                .write_all("2026/07/28 15:14:00.000 追記行\n".as_bytes())
                .expect("追記できるはず");
        }

        let (reopened, snapshot) =
            hakutaku_data_source::reopen_for_reload(&file.path).expect("追記後も開けるはず");
        let after = stream_decode_and_index(reopened, &file.path, &snapshot, &[])
            .expect("追記後の読み込みも成功するはず");

        assert_eq!(after.pending_items.len(), 51, "追記分が1件増える");
        assert_eq!(
            after.pending_items[..50]
                .iter()
                .map(|item| (item.raw_offset, item.raw_byte_len))
                .collect::<Vec<_>>(),
            before
                .pending_items
                .iter()
                .map(|item| (item.raw_offset, item.raw_byte_len))
                .collect::<Vec<_>>(),
            "既存分の生バイト範囲は追記前と同じ"
        );
    }
}

#[cfg(test)]
mod control_tests {
    //! `register_source_with_control`（P06-2）の受け入れ条件テストです。
    //! `mod tests` の `TempFile` と同じ作りの独立したヘルパーを使います
    //! （テスト同士の干渉を避けるため、ファイルごとに一意な一時ファイルを
    //! 作成します）。

    use super::*;
    use crate::notification::{
        CancellationToken, Progress, ProgressSink, ProgressUnit, TaskId, TaskOutcome,
    };
    use crate::registry::SourceStatus;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    struct TempFile {
        path: std::path::PathBuf,
    }

    impl TempFile {
        fn create_text(label: &str, contents: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let count = COUNTER.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "hakutaku-core-services-loader-control-test-{label}-{}-{count}-{nanos}.log",
                std::process::id()
            ));
            std::fs::write(&path, contents.as_bytes()).expect("テスト用ファイルを作成できません");
            TempFile { path }
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn control_with_chunk_bytes(chunk_bytes: u64) -> LoadControl<'static> {
        LoadControl {
            chunk_bytes,
            ..LoadControl::none()
        }
    }

    /// 最初のバッチがレジストリへ登録された直後に、指定した処理をちょうど1回
    /// 実行する [`RegistryAccess`] 実装です。
    ///
    /// 読み込みは1スレッドで「チャンク読み込み → デコード・解析 → バッチ登録
    /// （`with_registry`）→ **次のチャンク境界**でのキャンセル確認・整合性
    /// 再確認」の順に進みます（[`register_source_with_access`] と
    /// `hakutaku_data_source::stream_snapshotted_bytes_chunked`）。したがって、
    /// バッチ登録の直後に差し込んだ処理は「最初のバッチは登録済み・残りの
    /// チャンクは未読み込み」という状態で必ず実行され、その結果は次のチャンク
    /// 境界で必ず観測されます。
    ///
    /// 別スレッドをスリープさせて同じ状態を狙うと、読み込み側が遅い環境では
    /// 登録前に処理が届き（＝まだ何も登録していない経路になり）、テストが
    /// 不定になります。この実装は実時間をまったく参照しないため、負荷や
    /// 実行順に関わらず同じ結果になります。
    struct AfterFirstBatch<'a, F: FnMut()> {
        registry: &'a mut DisplaySetRegistry,
        /// 未実行の間だけ `Some`。実行後は `None`（2回目以降のバッチ登録では
        /// 何もしないため、状態そのものを取り出して消す）。
        action: Option<F>,
    }

    impl<'a, F: FnMut()> AfterFirstBatch<'a, F> {
        fn new(registry: &'a mut DisplaySetRegistry, action: F) -> Self {
            AfterFirstBatch {
                registry,
                action: Some(action),
            }
        }

        /// 差し込んだ処理が実行済みか（＝最初のバッチ登録を観測できたか）を
        /// 返します。テストの前提が崩れていないことを確認するために使います。
        fn fired(&self) -> bool {
            self.action.is_none()
        }
    }

    impl<F: FnMut()> RegistryAccess for AfterFirstBatch<'_, F> {
        fn with_registry<R>(&mut self, borrow: impl FnOnce(&mut DisplaySetRegistry) -> R) -> R {
            let result = borrow(self.registry);
            // 「最初のバッチが登録された」＝ `deliver_batch` が `insert_source`
            // まで進み、レジストリにソースが現れた状態。空のバッチでも
            // `with_registry` は呼ばれる（`deliver_batch` が即座に戻る）ため、
            // 呼び出し回数ではなくレジストリの状態で判定する。
            if !self.registry.list_sources().is_empty() {
                if let Some(mut action) = self.action.take() {
                    action();
                }
            }
            result
        }
    }

    // 受け入れ条件: 事前確保により、常駐分の振替量が「項目数 ×
    // RESIDENT_BYTES_PER_ITEM」ちょうどになる（倍々成長の余剰が出ない）。
    //
    // このテストのファイルは外挿の下限（MIN_PROJECTION_SAMPLE_BYTES）に満たない
    // ため、見積もりは常に確定値になり、余剰は0件で決定的に定まる。チャンク
    // 数を変えても結果が変わらないこと（伸長経路でも事前確保が効くこと）まで
    // 確認する。
    #[test]
    fn resident_commit_has_no_growth_waste_for_exactly_estimated_loads() {
        let mut contents = String::new();
        for i in 0..200 {
            contents.push_str(&format!(
                "2026/07/28 15:12:{:02}.000 メッセージ{i}\n",
                i % 60
            ));
        }

        for chunk_bytes in [64u64, 512, 4096, hakutaku_data_source::DEFAULT_CHUNK_BYTES] {
            let file = TempFile::create_text(&format!("commit-exact-{chunk_bytes}"), &contents);
            let mut registry = DisplaySetRegistry::new();
            let budget = crate::budget::SourceBudget::new();

            let outcome = register_source_with_control(
                &mut registry,
                &budget,
                &file.path,
                "commit.log".to_string(),
                &[],
                &control_with_chunk_bytes(chunk_bytes),
            )
            .expect("読み込みは成功するはず");

            assert_eq!(outcome.handle.total_items, 200);
            assert_eq!(
                outcome.summary.reserved_bytes,
                200 * crate::item::RESIDENT_BYTES_PER_ITEM,
                "chunk_bytes={chunk_bytes} で常駐分の振替量に余剰が出ている"
            );
        }
    }

    // 受け入れ条件: チャンク読み込みで全件読み込みと同一の項目列になる
    // （境界がちょうど・半端どちらでも。チャンク境界が行の途中・継続行の
    // 途中に落ちるケースを、極端に小さい chunk_bytes で強制的に発生させる）。
    #[test]
    fn chunked_control_path_matches_whole_file_load_across_chunk_boundaries() {
        // 日時付き行 + 2行の継続行、を複数回繰り返す。マルチバイト文字
        // （日本語）も含め、UTF-8 の文字境界がチャンク境界と衝突するケースも
        // 混ぜる。
        let mut contents = String::new();
        for i in 0..30 {
            contents.push_str(&format!(
                "2026/07/28 15:12:{:02}.000 起動メッセージ{i}\n継続行A-{i}\n継続行B-{i}\n",
                i % 60
            ));
        }

        for chunk_bytes in [1u64, 2, 3, 5, 7, 11, 64, 4096] {
            let file = TempFile::create_text(&format!("chunk-eq-{chunk_bytes}"), &contents);
            let mut registry = DisplaySetRegistry::new();
            let budget = crate::budget::SourceBudget::new();
            let control = control_with_chunk_bytes(chunk_bytes);

            let outcome = register_source_with_control(
                &mut registry,
                &budget,
                &file.path,
                "chunked.log".to_string(),
                &[],
                &control,
            )
            .expect("読み込みは成功するはず");

            assert_eq!(outcome.outcome, TaskOutcome::Completed);
            assert_eq!(
                outcome.handle.total_items, 30,
                "chunk_bytes={chunk_bytes} で項目数が一致しない"
            );

            let response = registry
                .fetch_range(
                    outcome.handle.display_set_id,
                    crate::display_set::RangeRequest {
                        start: 0,
                        max_items: 100,
                        expected_generation: outcome.handle.generation,
                    },
                )
                .expect("範囲取得は成功するはず");

            assert_eq!(response.items.len(), 30);
            assert_eq!(
                &*response.items[0].raw_text,
                "2026/07/28 15:12:00.000 起動メッセージ0\n継続行A-0\n継続行B-0",
                "chunk_bytes={chunk_bytes} で本文が一致しない"
            );
            assert_eq!(
                &*response.items[29].raw_text,
                "2026/07/28 15:12:29.000 起動メッセージ29\n継続行A-29\n継続行B-29"
            );
        }
    }

    // 受け入れ条件: 読み込み途中の表示集合伸長（total_items が増える。既取得
    // 範囲の識別子・順序は不変。世代は変わらない）。チャンクを細かく分けて
    // 複数回の伸長が発生する状況でも、最終結果（全件一致・世代不変）が保たれる
    // ことを確認する（伸長そのものの単体挙動は
    // `grow_source_items_appends_without_changing_generation` で確認する）。
    #[test]
    fn registry_grows_progressively_while_loading() {
        let mut contents = String::new();
        for i in 0..20 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("grow", &contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        // チャンクを細かく分け、複数回の伸長が発生するようにする。
        let control = control_with_chunk_bytes(20);

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "grow.log".to_string(),
            &[],
            &control,
        )
        .expect("読み込みは成功するはず");

        assert_eq!(outcome.handle.total_items, 20);
        assert_eq!(outcome.handle.generation, 1, "伸長では世代は変わらないはず");
    }

    // 受け入れ条件: 読み込み途中でも解析済み範囲から表示集合を伸長できる
    // ことの傍証として、小さい chunk_bytes では複数回の進捗通知（＝複数回の
    // チャンク処理、それぞれの後で `deliver_batch` による伸長が起こり得る）が
    // 発生し、通知される読み込み済みバイト数が単調増加することを確認する
    // （`ProgressSink` はレジストリへアクセスできない設計のため、伸長自体の
    // 直接観測は `grow_source_items_appends_without_changing_generation` が
    // 担う）。
    #[test]
    fn multiple_progress_notifications_occur_for_small_chunk_bytes() {
        let mut contents = String::new();
        for i in 0..15 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("progress-multi", &contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let sink_calls: Mutex<Vec<u64>> = Mutex::new(Vec::new());
        struct RecordingSink<'a>(&'a Mutex<Vec<u64>>);
        impl ProgressSink for RecordingSink<'_> {
            fn report(&self, _task_id: TaskId, progress: Progress) {
                self.0.lock().unwrap().push(progress.done());
            }
        }
        let sink = RecordingSink(&sink_calls);
        let control = LoadControl {
            chunk_bytes: 20,
            // io_interval_ms によりチャンクごとに実時間を消費させ、
            // ProgressThrottle（既定: 100ms または8MiBごと）の時間側の条件を
            // 現実に跨がせる（既定の間引き設定は LoadControl から変更できない
            // ため、実時間を経過させることで複数回の通知を確実に発生させる）。
            throttle: hakutaku_data_source::IoThrottle::new(None, 15),
            progress: Some(&sink),
            ..LoadControl::none()
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "progress-multi.log".to_string(),
            &[],
            &control,
        )
        .expect("読み込みは成功するはず");

        assert_eq!(outcome.handle.total_items, 15);
        let calls = sink_calls.lock().unwrap();
        assert!(
            calls.len() >= 2,
            "実時間の経過により複数回の進捗通知が発生するはず: {calls:?}"
        );
        assert!(
            calls.windows(2).all(|pair| pair[0] <= pair[1]),
            "進捗は単調増加のはず"
        );
        // NOTE: 最終チャンクの通知が ProgressThrottle の間引き判定（時間 100ms
        // または量8MiBのどちらか）にちょうど収まるかは実行環境のスケジューリング
        // 次第で揺れ得るため、「全量に達すること」までは要求しない（緩めの時間
        // 検証。`progress_is_reported_through_the_sink` の doc コメントと同じ
        // 理由）。ここでは複数回の通知と単調増加という、間引きの仕組み自体が
        // 機能していることだけを確認する。
        assert!(
            *calls.last().unwrap() <= contents.len() as u64,
            "読み込み済みバイト数は総量を超えないはず"
        );
    }

    // 受け入れ条件: 表示集合の伸長そのもの（世代を変えずに項目列へ追記し、
    // total_items が増える）を registry の API 単体で直接確認する
    // （crate::registry の内部 API のため、同一クレート内のこのテストから
    // 検証する）。
    #[test]
    fn grow_source_items_appends_without_changing_generation() {
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file = TempFile::create_text("grow-api", "2026/07/28 15:12:23.456 最初の行\n");

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "grow-api.log".to_string(),
            &[],
            &LoadControl::none(),
        )
        .expect("登録は成功するはず");
        assert_eq!(outcome.handle.total_items, 1);
        let generation_before = outcome.handle.generation;

        let pending = crate::item::PendingItem::simple(99, "手動追記");
        let new_total = registry
            .grow_source_items(
                outcome.handle.source_id,
                &[pending],
                CapacityEstimate::Exact(2),
            )
            .expect("登録済みのソースなので成功するはず");
        assert_eq!(new_total, 2);

        let current = registry
            .current_handle(outcome.handle.source_id)
            .expect("登録済みのはず");
        assert_eq!(
            current.generation, generation_before,
            "伸長では世代が変わらないはず"
        );
        assert_eq!(current.total_items, 2);
    }

    // 受け入れ条件: キャンセルはチャンク境界で停止し、部分読み込み状態が
    // 区別される（読み込み済み範囲は保持される）。
    #[test]
    fn cancellation_stops_at_chunk_boundary_and_marks_cancelled_partial() {
        let mut contents = String::new();
        for i in 0..50 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("cancel", &contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let token = CancellationToken::new();
        // 数バイト読んだ直後にキャンセルされるよう、極端に小さいチャンクと
        // 併用する。
        let control = LoadControl {
            chunk_bytes: 10,
            cancellation: Some(&token),
            ..LoadControl::none()
        };

        // 進捗コールバックの中でキャンセルを要求する術がないため、ここでは
        // 「最初から要求済み」のトークンを渡し、1チャンクも読まずに停止する
        // 経路を確認する（読み込み済み範囲が空でも「保持される」契約は
        // 変わらない）。
        token.request_cancel();

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "cancel.log".to_string(),
            &[],
            &control,
        )
        .expect("キャンセルはエラーではなく正常応答扱い");

        assert_eq!(outcome.outcome, TaskOutcome::Cancelled);
        assert_eq!(
            registry.source_status(outcome.handle.source_id),
            Some(SourceStatus::CancelledPartial)
        );
        assert_eq!(
            outcome.handle.total_items, 0,
            "1チャンクも読めなかったので0件のはず"
        );

        // 他のソースは影響を受けず、引き続き開ける（ERR-001）。
        let other = TempFile::create_text("cancel-other", "2026/07/28 15:12:00.000 別ファイル\n");
        let other_outcome = register_source_with_control(
            &mut registry,
            &budget,
            &other.path,
            "other.log".to_string(),
            &[],
            &LoadControl::none(),
        )
        .expect("他の対象は影響を受けず読み込めるはず");
        assert_eq!(other_outcome.outcome, TaskOutcome::Completed);
        assert_eq!(other_outcome.handle.total_items, 1);
    }

    // 受け入れ条件: キャンセルで途中終了しても、既に読み込み済みの範囲は
    // 保持される（部分読み込み）。
    //
    // 「一部は読み込み済み・残りは未読み込み」という状態は、最初のバッチ登録
    // の直後にキャンセルを要求して作る（[`AfterFirstBatch`]）。キャンセルは
    // 次のチャンク境界で確認されるため、要求は必ず「登録済み・全件読み込み前」
    // の位置で効く。実時間の経過（スリープ）で同じ状態を狙うと、読み込みが
    // 遅い環境では登録前にキャンセルが届き、結果が変わり得た。
    #[test]
    fn cancellation_after_some_batches_preserves_already_loaded_items() {
        let mut contents = String::new();
        for i in 0..200 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("cancel-partial", &contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let token = CancellationToken::new();
        let control = LoadControl {
            // 1行あたり約29バイトなので、chunk_bytes=28で概ね1チャンク=1行。
            // 200行あるため、最初のバッチが確定した後にも未読み込みのチャンクが
            // 必ず残る（＝キャンセルが「途中で」効く）。
            chunk_bytes: 28,
            cancellation: Some(&token),
            ..LoadControl::none()
        };

        let mut access = AfterFirstBatch::new(&mut registry, || token.request_cancel());
        let outcome = register_source_with_access(
            &mut access,
            &budget,
            &file.path,
            "cancel-partial.log".to_string(),
            &[],
            &control,
        )
        .expect("キャンセルはエラーではなく正常応答扱い");
        assert!(
            access.fired(),
            "最初のバッチ登録を観測してキャンセルを要求したはず"
        );

        assert_eq!(outcome.outcome, TaskOutcome::Cancelled);
        assert_eq!(
            registry.source_status(outcome.handle.source_id),
            Some(SourceStatus::CancelledPartial)
        );
        assert!(
            outcome.handle.total_items > 0,
            "いくらかは読み込めているはず"
        );
        assert!(
            outcome.handle.total_items < 200,
            "全件は読み込んでいないはず（キャンセルされたので）"
        );

        // 読み込み済み範囲は破棄されず、先頭から順に取得できる。
        let response = registry
            .fetch_range(
                outcome.handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 100,
                    expected_generation: outcome.handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");
        assert_eq!(response.items.len() as u64, outcome.handle.total_items);
        assert_eq!(&*response.items[0].raw_text, "2026/07/28 15:12:00.000 行0");
    }

    // 受け入れ条件: 進捗が ProgressThrottle 経由で通知される（テスト用 Sink で
    // 受信する）。
    #[test]
    fn progress_is_reported_through_the_sink() {
        let mut contents = String::new();
        for i in 0..10 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("progress", &contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        struct RecordingSink {
            received: Mutex<Vec<(TaskId, u64, u64)>>,
        }
        impl ProgressSink for RecordingSink {
            fn report(&self, task_id: TaskId, progress: Progress) {
                let (done, total) = match progress {
                    Progress::Determinate { done, total, unit } => {
                        assert_eq!(unit, ProgressUnit::Bytes);
                        (done, total)
                    }
                    Progress::Indeterminate { .. } => panic!("Determinate を期待した"),
                };
                self.received.lock().unwrap().push((task_id, done, total));
            }
        }
        let sink = RecordingSink {
            received: Mutex::new(Vec::new()),
        };
        let task_id = TaskId::generate();
        let control = LoadControl {
            chunk_bytes: 10,
            progress: Some(&sink),
            task_id,
            ..LoadControl::none()
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "progress.log".to_string(),
            &[],
            &control,
        )
        .expect("読み込みは成功するはず");
        assert_eq!(outcome.outcome, TaskOutcome::Completed);

        let received = sink.received.lock().unwrap();
        assert!(!received.is_empty(), "進捗が少なくとも1回は通知されるはず");
        for (received_task_id, done, total) in received.iter() {
            assert_eq!(*received_task_id, task_id);
            assert_eq!(*total, contents.len() as u64);
            assert!(*done <= *total, "読み込み済みバイト数は総量を超えないはず");
        }
        // NOTE: ProgressThrottle（既定: 100ms または8MiBごと）は、この程度の
        // 小さいファイルであれば実行がほぼ瞬時に終わるため、最終通知が必ず
        // しも全量（100%）に達するとは限らない（最後の間引き判定を満たす前に
        // 読み込みそのものが完了し得る）。100%到達の確実な通知が必要な場合の
        // 挙動は、複数回の通知が確実に発生する状況（実時間を消費させる）で
        // 検証する `multiple_progress_notifications_occur_for_small_chunk_bytes`
        // を参照。呼び出し側は、完了の判定に `RegisterSourceOutcome::outcome`
        // （本テストで確認済み）を使うべきで、進捗通知の最終値には依存しない。
    }

    // 受け入れ条件: I/O 発行間隔（io_interval_ms）が待機を発生させる
    // （時間検証は緩め）。
    #[test]
    fn io_interval_causes_measurable_delay_through_the_loader() {
        let mut contents = String::new();
        for i in 0..5 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("interval", &contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let control = LoadControl {
            chunk_bytes: 10,
            throttle: hakutaku_data_source::IoThrottle::new(None, 20),
            ..LoadControl::none()
        };

        let started = std::time::Instant::now();
        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "interval.log".to_string(),
            &[],
            &control,
        )
        .expect("読み込みは成功するはず");
        assert_eq!(outcome.outcome, TaskOutcome::Completed);

        assert!(
            started.elapsed() >= std::time::Duration::from_millis(20),
            "io_interval_ms による待機が発生しているはず: {:?}",
            started.elapsed()
        );
    }

    // 受け入れ条件: prefetch_paused() 中は先読みが発行されない（要求済み範囲
    // ＝ eager_bytes までは読み込まれる）。
    #[test]
    fn prefetch_paused_stops_reading_beyond_eager_bytes_through_the_loader() {
        let mut contents = String::new();
        for i in 0..20 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("prefetch", &contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        // グローバル予算を汚さないための代替として、SourceBudget と同様、
        // ここでは prefetch_paused の判定に使われる
        // hakutaku_memory_accounting::global_budget() を直接操作できないため
        // （register_source_with_control は内部でグローバル予算を使う設計。
        // loader.rs 冒頭の doc コメントを参照）、しきい値を極端に低く設定
        // した上でグローバル予算に対して予約を行い、確実に
        // prefetch_paused() を真にする。テスト終了時に元へ戻す。
        let global = hakutaku_memory_accounting::global_budget();
        let original_percent = global.soft_threshold_percent();
        global
            .set_soft_threshold_percent(1)
            .expect("1は有効な割合のはず");
        // 呼び出し前から超過していることを保証するため、大きめの予約を
        // 取ってすぐに保持し続ける（drop すると解放されてしまうため、
        // テスト内で保持する）。
        let guard_reservation = global
            .reserve(global.budget_bytes() / 2)
            .expect("半分の予約は成功するはず");
        assert!(global.prefetch_paused(), "しきい値超過のはず");

        let control = LoadControl {
            chunk_bytes: 30, // 概ね1行分
            eager_bytes: 60, // 先頭2行分程度だけは要求済みとして必ず読む
            ..LoadControl::none()
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "prefetch.log".to_string(),
            &[],
            &control,
        )
        .expect("読み込みは成功するはず（打ち切りはエラーではない）");

        // 後始末（他のテストへ影響しないよう、しきい値・予約を元に戻す）。
        drop(guard_reservation);
        global
            .set_soft_threshold_percent(original_percent)
            .expect("既定値へ戻せるはず");

        assert_eq!(outcome.outcome, TaskOutcome::Cancelled);
        assert_eq!(
            registry.source_status(outcome.handle.source_id),
            Some(SourceStatus::CancelledPartial)
        );
        assert!(
            outcome.handle.total_items > 0,
            "要求済み範囲(eager_bytes)分は読み込まれるはず"
        );
        assert!(
            outcome.handle.total_items < 20,
            "先読み抑制により全件は読み込まれないはず"
        );
    }

    // 受け入れ条件: 変更検知(LOG-023)は、既にバッチを登録済みの場合、
    // ソースを Changed にしたうえで Ok(TaskOutcome::Failed) として報告する
    // （登録前の場合は Err になることは register_source 側の既存テストで
    // 確認済み）。
    //
    // 「既にバッチを登録済み」の状態は、最初のバッチ登録の直後に実ファイルを
    // 切り詰めて作る（[`AfterFirstBatch`]）。切り詰め（縮小）は次のチャンクの
    // 読み込み前に行う整合性再確認で必ず観測される。縮小の確認は毎チャンク
    // 行われる（`hakutaku_data_source::verify_snapshot_by_handle`。周期的に
    // しか行わないパス再オープンとは別の層である）ため、切り詰めを
    // どのチャンク境界へ差し込んでも結果は変わらない。判定はファイル識別子と
    // サイズの比較だけで行い最終更新時刻を見ないため
    // （`hakutaku_data_source::compare_snapshots`）、ファイル時刻の分解能にも
    // 依存しない。
    //
    // スリープで猶予を作る作りでは、切り詰めが読み込みループのどこへ落ちるかを
    // 固定できず、結果が3通りに割れていた。最初のバッチ登録より
    // 前なら `Err`、整合性再確認とそのチャンクの読み込みの間なら読み込みエラー
    // （`SourceStatus::Error`）、その他なら期待どおりの `Changed` である。
    // バッチ境界へ差し込めば、切り詰めは必ず「あるチャンクの読み込み完了後・
    // 次のチャンクの整合性再確認前」に入るため、この3通りが1通りに定まる。
    #[test]
    fn change_detected_after_first_batch_marks_source_changed_and_reports_failed() {
        let mut contents = String::new();
        for i in 0..200 {
            contents.push_str(&format!("2026/07/28 15:12:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("change-mid-load", &contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let control = LoadControl {
            // 1行あたり約29バイトなので、chunk_bytes=28で概ね1チャンク=1行。
            // 200行あるため、最初のバッチが確定した後にも未読み込みのチャンクが
            // 必ず残る（＝切り詰めを観測する機会が必ずある）。
            chunk_bytes: 28,
            ..LoadControl::none()
        };

        let truncate_path = file.path.clone();
        let mut access = AfterFirstBatch::new(&mut registry, || {
            let writer = std::fs::OpenOptions::new()
                .write(true)
                .open(&truncate_path)
                .expect("書き込み用に開けるはず");
            writer.set_len(3).expect("切り詰めできるはず");
        });
        let outcome = register_source_with_access(
            &mut access,
            &budget,
            &file.path,
            "change-mid-load.log".to_string(),
            &[],
            &control,
        )
        .expect("既に登録済みなので Ok(Failed) として返るはず");
        assert!(
            access.fired(),
            "最初のバッチ登録を観測して切り詰めたはず（登録前の切り詰めなら Err 経路になる）"
        );

        let status = registry.source_status(outcome.handle.source_id);
        assert!(
            matches!(outcome.outcome, TaskOutcome::Failed(_)),
            "変更検知は Ok(Failed) として報告されるはず: {:?}",
            outcome.outcome
        );
        assert!(
            matches!(status, Some(SourceStatus::Changed(_))),
            "変更検知は Changed へ遷移させるはず（Error なら読み込みエラー経路）: {status:?}"
        );
        // LOG-023: 索引を無効化する。従来の索引を有効扱いで維持しない。
        assert_eq!(outcome.handle.total_items, 0);
    }

    // 受け入れ条件（P07-2、LOG-022 の手動選択経路）: LoadControl::manual_profile
    // が resolve_profile の第1段階（手動指定）へ伝わり、パスに一致するかどうか
    // に関わらずそのプロファイルが採用される。
    #[test]
    fn manual_profile_is_propagated_to_profile_resolution() {
        let contents = "2026/07/28 15:12:23.456 手動プロファイル\n";
        let file = TempFile::create_text("manual-profile", contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        // path_pattern はこのファイルには一致しない場所を指すが、手動指定は
        // パス照合より優先される（crate::profile_resolution::resolve_profile
        // の doc コメント「手動指定」段階）。
        let profile = hakutaku_config::LogProfileConfig {
            name: "manual-utf8".to_string(),
            path_pattern: r"C:\Other\Unrelated\*.log".to_string(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Named("utf-8".to_string()),
            ansi_codepage: None,
            datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
        };

        let control = LoadControl {
            manual_profile: Some("manual-utf8"),
            ..LoadControl::none()
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "manual.log".to_string(),
            &[profile],
            &control,
        )
        .expect("読み込みは成功するはず");

        assert_eq!(outcome.outcome, TaskOutcome::Completed);
        assert_eq!(outcome.summary.profile_resolution_route, "手動指定");
        assert_eq!(
            outcome.summary.encoding_route,
            "プロファイル指定（encoding 名前指定）"
        );
        assert!(!outcome.summary.fell_back_to_raw_display);
    }

    // 受け入れ条件: プロファイルの datetime_format が
    // StreamingAssembler へ伝わり、自動判定では曖昧になる LOG-DT-004 の
    // ファイルが生表示へ退避せず解析される。
    #[test]
    fn profile_datetime_format_is_propagated_and_parses_log_dt_004() {
        let contents = "2026/07/28 15:12:23:45 一行目\n2026/07/28 15:12:24:99 二行目\n";
        let file = TempFile::create_text("profile-datetime-format", contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let profile = hakutaku_config::LogProfileConfig {
            name: "dt-004".to_string(),
            path_pattern: file.path.to_string_lossy().into_owned(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Auto,
            ansi_codepage: None,
            datetime_format: hakutaku_config::DateTimeFormatSetting::LogDt004,
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "dt-004.log".to_string(),
            &[profile],
            &LoadControl::none(),
        )
        .expect("読み込みは成功するはず");

        assert!(!outcome.summary.fell_back_to_raw_display);
        assert_eq!(
            outcome.summary.detected_datetime_format,
            Some("LOG-DT-004"),
            "明示指定した書式がそのまま確定書式として報告されるはず"
        );
        assert_eq!(outcome.handle.total_items, 2);
        let response = registry
            .fetch_range(
                outcome.handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: outcome.handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");
        assert_eq!(
            response.items[0].timestamp_display.as_deref(),
            Some("2026-07-28T15:12:23.45"),
            "LOG-024・LOG-025: 元の精度（1/100秒2桁）のまま表示される"
        );
    }

    // 受け入れ条件: 同じ書式指定が再読み込み経路
    // （stream_decode_and_index）でも効く。初回登録だけの機能ではない。
    #[test]
    fn profile_datetime_format_also_applies_on_reload() {
        use std::io::Write;

        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        let file =
            TempFile::create_text("reload-datetime-format", "2026/07/28 15:12:23:45 一行目\n");

        let profile = hakutaku_config::LogProfileConfig {
            name: "dt-004".to_string(),
            path_pattern: file.path.to_string_lossy().into_owned(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Auto,
            ansi_codepage: None,
            datetime_format: hakutaku_config::DateTimeFormatSetting::LogDt004,
        };
        let profiles = std::slice::from_ref(&profile);

        let (handle, _summary) = register_source(
            &mut registry,
            &budget,
            &file.path,
            "dt-004.log".to_string(),
            profiles,
        )
        .expect("登録は成功するはず");
        assert_eq!(handle.total_items, 1);

        {
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記用に開けるはず");
            writer
                .write_all("2026/07/28 15:12:24:99 二行目\n".as_bytes())
                .expect("追記できるはず");
        }

        let outcome = reload_source(&mut registry, &budget, handle.source_id, profiles)
            .expect("登録済みのソースなので Some のはず");
        let (generation, total_items, fell_back_to_raw_display) = match outcome {
            ReloadOutcome::Reloaded {
                generation,
                total_items,
                fell_back_to_raw_display,
            } => (generation, total_items, fell_back_to_raw_display),
            other => panic!("Reloaded を期待したが {other:?} だった"),
        };
        assert_eq!(total_items, 2);
        // 設定由来の書式は再読み込みでも再適用されるため、作り直した
        // 結果でも生表示退避は起きない。
        assert_eq!(
            fell_back_to_raw_display,
            Some(false),
            "設定の書式が効いているので生表示へは退避しない"
        );

        let response = registry
            .fetch_range(
                handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: generation,
                },
            )
            .expect("最新世代なので成功するはず");
        assert_eq!(
            response.items[1].timestamp_display.as_deref(),
            Some("2026-07-28T15:12:24.99"),
            "再読み込み後も書式指定が効いているはず"
        );
    }

    // 受け入れ条件: 書式を明示していないプロファイルでは、従来
    // どおり自動判定が働く（既定の挙動を変えていないことの確認）。
    #[test]
    fn profile_without_datetime_format_keeps_auto_detection() {
        let contents = "2026/07/28 15:12:23:45 一行目\n";
        let file = TempFile::create_text("profile-datetime-format-auto", contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let profile = hakutaku_config::LogProfileConfig {
            name: "auto".to_string(),
            path_pattern: file.path.to_string_lossy().into_owned(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Auto,
            ansi_codepage: None,
            datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "auto.log".to_string(),
            &[profile],
            &LoadControl::none(),
        )
        .expect("読み込みは成功するはず");

        assert!(
            outcome.summary.fell_back_to_raw_display,
            "LOG-022: 書式未指定なら従来どおり曖昧判定で生表示へ退避する"
        );
        assert_eq!(outcome.summary.detected_datetime_format, None);
    }

    // 受け入れ条件: 設定側（hakutaku_config）と解析側
    // （hakutaku_parser）が持つ要件 ID の文字列表が一致する。config は parser へ
    // 依存できず表が2つに分かれるため、綴りの食い違いをここで検出する。
    #[test]
    fn config_datetime_format_ids_match_parser_format_ids() {
        for id in hakutaku_config::DateTimeFormatSetting::SPECIFIED_IDS {
            let setting = hakutaku_config::DateTimeFormatSetting::from_setting_str(id)
                .expect("SPECIFIED_IDS の値は設定側で受理されるはず");
            let format = datetime_format_from_setting(setting)
                .expect("明示指定の書式は解析側の書式へ写せるはず");
            assert_eq!(
                format.id(),
                id,
                "設定側と解析側で要件 ID の綴りが食い違っている"
            );
        }
        // auto は「書式の明示なし」であり、解析側の書式へは写らない。
        assert_eq!(
            datetime_format_from_setting(hakutaku_config::DateTimeFormatSetting::Auto),
            None
        );
    }

    // 受け入れ条件（P07-2、LOG-022）: 手動指定したプロファイル名が
    // `log_profiles` に存在しない場合、ManualNotFound により生表示へ退避する
    // （利用者向けエラーにはしない。読み込み自体は成功する）。
    #[test]
    fn unknown_manual_profile_falls_back_to_raw_display() {
        let contents = "2026/07/28 15:12:23.456 不明なプロファイル指定\n";
        let file = TempFile::create_text("manual-profile-unknown", contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let control = LoadControl {
            manual_profile: Some("does-not-exist"),
            ..LoadControl::none()
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "manual-unknown.log".to_string(),
            &[],
            &control,
        )
        .expect("読み込み自体は成功するはず（生表示への退避）");

        assert_eq!(outcome.outcome, TaskOutcome::Completed);
        assert!(outcome.summary.fell_back_to_raw_display);
        assert_eq!(
            outcome.summary.profile_resolution_route,
            "手動指定（該当プロファイルなし）"
        );
        assert_eq!(outcome.handle.total_items, 1, "生表示では1行=1項目のまま");
    }

    /// 手動書式指定のテストで使う、`LOG-DT-004` だけで構成されたファイルです
    /// （自動判定では必ず `LOG-DT-005` とも同時に成立し、曖昧判定になります。
    /// モジュール doc コメント「自動判定だけでは解けない場合」）。
    const LOG_DT_004_ONLY: &str = "2026/07/28 15:12:23:45 一行目\n2026/07/28 15:12:24:99 二行目\n";

    /// 一時ファイルと同じディレクトリの全 `.log` に一致する glob を返します
    /// （`Ambiguous` を作るための道具）。
    fn sibling_glob(path: &std::path::Path) -> String {
        path.parent()
            .expect("一時ファイルには親ディレクトリがあるはず")
            .join("*.log")
            .to_string_lossy()
            .into_owned()
    }

    // 受け入れ条件（LOG-022）: 設定（log_profiles）が空でも、UI で
    // 選んだ書式が StreamingAssembler の確定モードへ伝わり、自動判定では
    // 曖昧になる LOG-DT-004 のみのファイルが生表示へ退避せず解析される。
    // これが選択肢2の中心となるケース（設定を書かずに開いたアドホックな
    // ファイルを、UI 操作だけで日時付き表示へ再解析できる）。
    #[test]
    fn manual_datetime_format_parses_log_dt_004_without_any_profile() {
        let file = TempFile::create_text("manual-datetime-format", LOG_DT_004_ONLY);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let control = LoadControl {
            manual_datetime_format: Some(LogDateTimeFormat::LogDt004),
            ..LoadControl::none()
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "manual-dt.log".to_string(),
            &[],
            &control,
        )
        .expect("読み込みは成功するはず");

        assert!(
            !outcome.summary.fell_back_to_raw_display,
            "手動指定した書式で確定するため、曖昧判定による生表示退避は起きないはず"
        );
        assert_eq!(outcome.summary.detected_datetime_format, Some("LOG-DT-004"));
        assert_eq!(outcome.handle.total_items, 2);

        let response = registry
            .fetch_range(
                outcome.handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: outcome.handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");
        assert_eq!(
            response.items[0].timestamp_display.as_deref(),
            Some("2026-07-28T15:12:23.45"),
            "LOG-024・LOG-025: 元の精度（1/100秒2桁）のまま表示される"
        );
    }

    // 受け入れ条件: UI での手動書式選択は、プロファイルの
    // datetime_format より優先される（優先順位 1 > 2）。
    #[test]
    fn manual_datetime_format_wins_over_profile_datetime_format() {
        let file = TempFile::create_text("manual-datetime-format-wins", LOG_DT_004_ONLY);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        // 設定側は秒までの LOG-DT-005 を指定している（この行は 005 としても
        // 解析でき、その場合は 1/100 秒が落ちて "15:12:23" になる）。
        let profile = hakutaku_config::LogProfileConfig {
            name: "dt-005".to_string(),
            path_pattern: file.path.to_string_lossy().into_owned(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Auto,
            ansi_codepage: None,
            datetime_format: hakutaku_config::DateTimeFormatSetting::LogDt005,
        };

        let control = LoadControl {
            manual_datetime_format: Some(LogDateTimeFormat::LogDt004),
            ..LoadControl::none()
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "manual-dt-wins.log".to_string(),
            &[profile],
            &control,
        )
        .expect("読み込みは成功するはず");

        assert_eq!(
            outcome.summary.detected_datetime_format,
            Some("LOG-DT-004"),
            "手動選択がプロファイル設定（LOG-DT-005）を上書きするはず"
        );

        let response = registry
            .fetch_range(
                outcome.handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: outcome.handle.generation,
                },
            )
            .expect("範囲取得は成功するはず");
        assert_eq!(
            response.items[0].timestamp_display.as_deref(),
            Some("2026-07-28T15:12:23.45"),
            "LOG-DT-005 が採用されていれば 1/100 秒が落ちるため、値で区別できる"
        );
    }

    // 受け入れ条件（LOG-022）: プロファイルを一意に決められない
    // Ambiguous では、手動書式を指定していても生表示退避が優先される
    // （プロファイル自体が決まらない状態で設定の一部だけを採用しないため）。
    #[test]
    fn manual_datetime_format_does_not_override_ambiguous_profile_fallback() {
        let file = TempFile::create_text("manual-datetime-format-ambiguous", LOG_DT_004_ONLY);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        // 同一優先度の glob を2件用意し、resolve_profile の第3段階で
        // Ambiguous になるようにする。
        let pattern = sibling_glob(&file.path);
        let profiles = vec![
            hakutaku_config::LogProfileConfig {
                name: "glob-a".to_string(),
                path_pattern: pattern.clone(),
                priority: 5,
                encoding: hakutaku_config::EncodingSetting::Auto,
                ansi_codepage: None,
                datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
            },
            hakutaku_config::LogProfileConfig {
                name: "glob-b".to_string(),
                path_pattern: pattern,
                priority: 5,
                encoding: hakutaku_config::EncodingSetting::Auto,
                ansi_codepage: None,
                datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
            },
        ];

        let control = LoadControl {
            manual_datetime_format: Some(LogDateTimeFormat::LogDt004),
            ..LoadControl::none()
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "manual-dt-ambiguous.log".to_string(),
            &profiles,
            &control,
        )
        .expect("読み込み自体は成功するはず（生表示への退避）");

        assert_eq!(
            outcome.summary.profile_resolution_route, "曖昧（同一優先度の glob が複数一致）",
            "テストの前提どおり Ambiguous になっていること"
        );
        assert!(
            outcome.summary.fell_back_to_raw_display,
            "手動書式より生表示退避が優先されるはず"
        );
        assert_eq!(outcome.summary.detected_datetime_format, None);
    }

    // 受け入れ条件（LOG-022）: ManualNotFound（指定したプロファイル
    // 名が存在しない）でも、手動書式より生表示退避が優先される。
    #[test]
    fn manual_datetime_format_does_not_override_manual_not_found_fallback() {
        let file = TempFile::create_text("manual-datetime-format-not-found", LOG_DT_004_ONLY);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let control = LoadControl {
            manual_profile: Some("does-not-exist"),
            manual_datetime_format: Some(LogDateTimeFormat::LogDt004),
            ..LoadControl::none()
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "manual-dt-not-found.log".to_string(),
            &[],
            &control,
        )
        .expect("読み込み自体は成功するはず（生表示への退避）");

        assert!(
            outcome.summary.fell_back_to_raw_display,
            "手動書式より生表示退避が優先されるはず"
        );
        assert_eq!(outcome.summary.detected_datetime_format, None);
    }

    // 受け入れ条件: 手動書式指定は1回の読み込み要求限りであり、
    // 明示的な再読み込み（reload_source）へは引き継がれない。既存の
    // manual_profile（reload 経路は resolve_profile(None, ..) を呼ぶ）と挙動を
    // そろえるための確認。この「1回限り」の設計は変えず、
    // 生表示へ戻った実結果を fell_back_to_raw_display として返すことで
    // 表示とフラグのずれだけを解消した（手動指定を再読み込みへ引き継ぐか
    // どうかは引き続き別課題）。
    #[test]
    fn manual_datetime_format_is_not_carried_over_on_reload() {
        use std::io::Write;

        let file = TempFile::create_text(
            "manual-datetime-format-reload",
            "2026/07/28 15:12:23:45 一行目\n",
        );
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let control = LoadControl {
            manual_datetime_format: Some(LogDateTimeFormat::LogDt004),
            ..LoadControl::none()
        };
        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "manual-dt-reload.log".to_string(),
            &[],
            &control,
        )
        .expect("読み込みは成功するはず");
        assert_eq!(
            outcome.summary.detected_datetime_format,
            Some("LOG-DT-004"),
            "初回登録では手動書式が効くこと（この後の対比の前提）"
        );

        {
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&file.path)
                .expect("追記用に開けるはず");
            writer
                .write_all("2026/07/28 15:12:24:99 二行目\n".as_bytes())
                .expect("追記できるはず");
        }

        let reload = reload_source(&mut registry, &budget, outcome.handle.source_id, &[])
            .expect("登録済みのソースなので Some のはず");
        let (generation, total_items, fell_back_to_raw_display) = match reload {
            ReloadOutcome::Reloaded {
                generation,
                total_items,
                fell_back_to_raw_display,
            } => (generation, total_items, fell_back_to_raw_display),
            other => panic!("Reloaded を期待したが {other:?} だった"),
        };
        assert_eq!(total_items, 2);
        // 手動書式が失われて生表示へ戻ったことを、呼び出し側が
        // 対象一覧の fell_back_to_raw_display へ反映できるよう結果で返す
        // （据え置くと、生表示なのに再解析 UI が出ない状態になる）。
        assert_eq!(
            fell_back_to_raw_display,
            Some(true),
            "生表示へ戻った実結果が呼び出し側へ伝わるはず"
        );

        let response = registry
            .fetch_range(
                outcome.handle.display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 10,
                    expected_generation: generation,
                },
            )
            .expect("最新世代なので成功するはず");
        assert_eq!(
            response.items[0].timestamp_display, None,
            "再読み込みでは手動書式が失われ、自動判定の曖昧判定により生表示へ戻る"
        );
    }

    // 受け入れ条件: 明示指定がない既定の読み込みでは、決定経路が
    // 「内容からの自動判定」になる（従来の診断ログと矛盾しないこと）。
    #[test]
    fn datetime_format_route_reports_auto_detection() {
        let file = TempFile::create_text("datetime-route-auto", "2026/07/28 15:12:23.456 一行目\n");
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "route-auto.log".to_string(),
            &[],
            &LoadControl::none(),
        )
        .expect("読み込みは成功するはず");

        assert_eq!(outcome.summary.detected_datetime_format, Some("LOG-DT-001"));
        assert_eq!(
            outcome.summary.datetime_format_route,
            DatetimeFormatRoute::Auto
        );
        assert_eq!(
            outcome.summary.datetime_format_route.route_label(),
            "内容からの自動判定"
        );
    }

    // 受け入れ条件: プロファイルの datetime_format で決まった場合、
    // 決定経路が「プロファイル指定」になる。
    #[test]
    fn datetime_format_route_reports_profile_setting() {
        let file = TempFile::create_text("datetime-route-profile", LOG_DT_004_ONLY);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let profile = hakutaku_config::LogProfileConfig {
            name: "dt-004".to_string(),
            path_pattern: file.path.to_string_lossy().into_owned(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Auto,
            ansi_codepage: None,
            datetime_format: hakutaku_config::DateTimeFormatSetting::LogDt004,
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "route-profile.log".to_string(),
            &[profile],
            &LoadControl::none(),
        )
        .expect("読み込みは成功するはず");

        assert_eq!(outcome.summary.detected_datetime_format, Some("LOG-DT-004"));
        assert_eq!(
            outcome.summary.datetime_format_route,
            DatetimeFormatRoute::Profile
        );
        assert_eq!(
            outcome.summary.datetime_format_route.route_label(),
            "プロファイル指定（datetime_format）"
        );
    }

    // 受け入れ条件: UI での手動選択が採用された場合、決定経路が
    // 「UI での手動選択」になる。プロファイルの datetime_format が同時にあっても
    // （優先順位 1 > 2 で手動が勝つため）経路は手動選択のままになる。
    #[test]
    fn datetime_format_route_reports_manual_selection() {
        let file = TempFile::create_text("datetime-route-manual", LOG_DT_004_ONLY);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let profile = hakutaku_config::LogProfileConfig {
            name: "dt-005".to_string(),
            path_pattern: file.path.to_string_lossy().into_owned(),
            priority: 0,
            encoding: hakutaku_config::EncodingSetting::Auto,
            ansi_codepage: None,
            datetime_format: hakutaku_config::DateTimeFormatSetting::LogDt005,
        };

        let control = LoadControl {
            manual_datetime_format: Some(LogDateTimeFormat::LogDt004),
            ..LoadControl::none()
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "route-manual.log".to_string(),
            &[profile],
            &control,
        )
        .expect("読み込みは成功するはず");

        assert_eq!(
            outcome.summary.detected_datetime_format,
            Some("LOG-DT-004"),
            "手動選択が採用されていること（経路と書式が同じ入力を指すはず）"
        );
        assert_eq!(
            outcome.summary.datetime_format_route,
            DatetimeFormatRoute::Manual
        );
        assert_eq!(
            outcome.summary.datetime_format_route.route_label(),
            "UI での手動選択"
        );
    }

    // 受け入れ条件（LOG-022）: プロファイル起因の生表示退避では、
    // 手動書式を指定していても採用されないため、決定経路は「判定なし（生表示
    // 退避）」になる（手動選択が効いたかのように読めてはならない）。
    #[test]
    fn datetime_format_route_reports_raw_display_fallback() {
        let file = TempFile::create_text("datetime-route-raw-display", LOG_DT_004_ONLY);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let control = LoadControl {
            manual_profile: Some("does-not-exist"),
            manual_datetime_format: Some(LogDateTimeFormat::LogDt004),
            ..LoadControl::none()
        };

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "route-raw-display.log".to_string(),
            &[],
            &control,
        )
        .expect("読み込み自体は成功するはず（生表示への退避）");

        assert!(outcome.summary.fell_back_to_raw_display);
        assert_eq!(outcome.summary.detected_datetime_format, None);
        assert_eq!(
            outcome.summary.datetime_format_route,
            DatetimeFormatRoute::RawDisplayFallback
        );
        assert_eq!(
            outcome.summary.datetime_format_route.route_label(),
            "判定なし（生表示退避）"
        );
    }

    // 受け入れ条件（LOG-022）: 自動判定が曖昧で生表示になった場合は、
    // プロファイル起因の退避とは区別し、決定経路は「内容からの自動判定」のまま
    // にする（書式を決めに行った結果として決まらなかったため）。切り分けは
    // fell_back_to_raw_display と併せて読む。
    #[test]
    fn datetime_format_route_stays_auto_when_auto_detection_is_ambiguous() {
        let file = TempFile::create_text("datetime-route-auto-ambiguous", LOG_DT_004_ONLY);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "route-auto-ambiguous.log".to_string(),
            &[],
            &LoadControl::none(),
        )
        .expect("読み込み自体は成功するはず（生表示への退避）");

        assert!(
            outcome.summary.fell_back_to_raw_display,
            "LOG-DT-004 のみのファイルは自動判定では曖昧になる（この検証の前提）"
        );
        assert_eq!(outcome.summary.detected_datetime_format, None);
        assert_eq!(
            outcome.summary.datetime_format_route,
            DatetimeFormatRoute::Auto,
            "プロファイルは一意に決まっている（NoMatch）ため、退避の原因は自動判定側"
        );
    }

    // --- 読み込み中のロック保持区間 ---

    /// 競合計測（[`measure_lock_contention`]）の1チャンクのバイト数。
    ///
    /// 小さくしてチャンク数を稼ぎ、`CONTENTION_IO_INTERVAL_MS` の待機を
    /// 何度も発生させます（実運用の 8 MiB では、テスト用の小さいファイルが
    /// 1チャンクで読み終わってしまい、読み込み中の競合を観測できません）。
    const CONTENTION_CHUNK_BYTES: u64 = 1024;

    /// 競合計測でチャンクごとに挟む I/O 発行間隔（ミリ秒）。
    ///
    /// 実ファイルの GB 級読み込み（数秒〜数十秒）を、テストで扱える時間へ
    /// 縮めて模したものです。待つのは `crates/data-source` のチャンク読み込み
    /// 側（レジストリの借用の外）であり、実測でも時間の大半は I/O・デコード・
    /// 解析が占めるため、この置き換えは計測の意味を変えません。
    const CONTENTION_IO_INTERVAL_MS: u64 = 10;

    /// 読み込み中に別スレッドがレジストリを借りようとしたときの観測結果です
    /// （ロック分割の効果測定）。
    struct ContentionMeasurement {
        /// 読み込み全体の所要時間。
        load_elapsed: std::time::Duration,
        /// レジストリのロック取得に要した最長時間（＝ UI が待たされる時間）。
        max_lock_wait: std::time::Duration,
        /// ロックを取れた回数。
        observations: usize,
        /// 読み込み完了前の途中経過（`total_items` が最終値未満）を観測した
        /// 回数。
        partial_observations: usize,
        /// 読み込み中に範囲取得（`fetch_range`）が成功した回数。
        fetch_ok: usize,
        total_items: u64,
        reserved_bytes: usize,
        generation: u64,
    }

    /// [`Mutex`] 越しにレジストリを借りる [`RegistryAccess`] 実装です
    /// （`src-tauri` の `PerBatchRegistryLock` と同じ形をテスト内で再現した
    /// もの。コア層のプロダクトコードは `std::sync::Mutex` に依存しません）。
    struct SharedRegistryAccess(std::sync::Arc<Mutex<DisplaySetRegistry>>);

    impl RegistryAccess for SharedRegistryAccess {
        fn with_registry<R>(&mut self, borrow: impl FnOnce(&mut DisplaySetRegistry) -> R) -> R {
            let mut guard = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            borrow(&mut guard)
        }
    }

    /// 読み込みを別スレッドで走らせながら、レジストリのロック取得と範囲取得を
    /// 一定間隔で試み、待たされた最長時間を計測します。
    ///
    /// `split_lock` が真なら [`register_source_with_access`]（バッチ境界ごとに
    /// 借り直す。改善後）、偽なら [`register_source_with_control`] へロック
    /// 済みのガードを渡す形（読み込み中ずっと保持する。改善前）で読み込みます。
    fn measure_lock_contention(path: &std::path::Path, split_lock: bool) -> ContentionMeasurement {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let registry = Arc::new(Mutex::new(DisplaySetRegistry::new()));
        let finished = Arc::new(AtomicBool::new(false));

        let loader_registry = Arc::clone(&registry);
        let loader_finished = Arc::clone(&finished);
        let loader_path = path.to_path_buf();
        let loader = std::thread::spawn(move || {
            let budget = crate::budget::SourceBudget::new();
            let control = LoadControl {
                chunk_bytes: CONTENTION_CHUNK_BYTES,
                throttle: hakutaku_data_source::IoThrottle::new(None, CONTENTION_IO_INTERVAL_MS),
                ..LoadControl::none()
            };
            let started = std::time::Instant::now();
            let outcome = if split_lock {
                register_source_with_access(
                    &mut SharedRegistryAccess(Arc::clone(&loader_registry)),
                    &budget,
                    &loader_path,
                    "contention.log".to_string(),
                    &[],
                    &control,
                )
            } else {
                let mut guard = loader_registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                register_source_with_control(
                    &mut guard,
                    &budget,
                    &loader_path,
                    "contention.log".to_string(),
                    &[],
                    &control,
                )
            }
            .expect("読み込みは成功するはず");
            let load_elapsed = started.elapsed();
            loader_finished.store(true, Ordering::SeqCst);
            (outcome, load_elapsed)
        });

        let mut max_lock_wait = std::time::Duration::ZERO;
        let mut seen_totals: Vec<u64> = Vec::new();
        let mut fetch_ok = 0usize;
        while !finished.load(Ordering::SeqCst) {
            let begin = std::time::Instant::now();
            let mut guard = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let waited = begin.elapsed();
            // ここは `src-tauri::log_view::fetch_log_range` と同じ形
            // （ロックを取ってから `fetch_range` を呼ぶ）。読み込み途中でも
            // 応答できることまで確かめる。
            if let Some(summary) = guard.list_sources().first() {
                let request = crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 512,
                    // 伸長では世代が進まないため、読み込み中は常に初回の世代。
                    expected_generation: 1,
                };
                if let Ok(response) = guard.fetch_range(summary.display_set_id, request) {
                    fetch_ok += 1;
                    seen_totals.push(response.total_items);
                }
            }
            drop(guard);
            max_lock_wait = max_lock_wait.max(waited);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let (outcome, load_elapsed) = loader.join().expect("読み込みスレッドは正常終了するはず");
        let total_items = outcome.handle.total_items;
        ContentionMeasurement {
            load_elapsed,
            max_lock_wait,
            observations: seen_totals.len(),
            partial_observations: seen_totals
                .iter()
                .filter(|seen| **seen < total_items)
                .count(),
            fetch_ok,
            total_items,
            reserved_bytes: outcome.summary.reserved_bytes,
            generation: outcome.handle.generation,
        }
    }

    // 受け入れ条件: 読み込み中に別スレッドが行う範囲取得が、
    // 読み込み完了まで待たされない。
    //
    // 改善前（`register_source_with_control` へロック済みガードを渡す形）と
    // 改善後（`register_source_with_access`）を同じファイル・同じ抑制設定で
    // 続けて計測し、待ち時間を直接比べる。両者の最終結果（total_items・
    // reserved_bytes・世代）が完全に一致することも同時に確認する（ロックの
    // 分割が結果を変えないことの検証）。
    //
    // 時間の閾値は、遅いマシンでも安定するよう十分に緩めてある（本質的な差は
    // 「読み込み全体を待つ」か「1バッチの登録を待つ」かであり、桁が違う）。
    #[test]
    fn per_batch_registry_access_keeps_range_fetch_responsive_during_load() {
        let mut contents = String::new();
        for i in 0..1200 {
            contents.push_str(&format!(
                "2026/07/28 15:12:{:02}.000 競合計測用の行 {i}\n",
                i % 60
            ));
        }
        let file = TempFile::create_text("lock-contention", &contents);

        let split = measure_lock_contention(&file.path, true);
        let whole = measure_lock_contention(&file.path, false);

        println!(
            "[ロック分割] 改善前（読み込み中ずっとロック保持）: 読み込み {:.0} ms / \
             ロック取得の最長待ち {:.1} ms / ロック取得回数 {} / 途中経過の観測 {} 回 / \
             範囲取得成功 {} 回",
            whole.load_elapsed.as_secs_f64() * 1000.0,
            whole.max_lock_wait.as_secs_f64() * 1000.0,
            whole.observations,
            whole.partial_observations,
            whole.fetch_ok,
        );
        println!(
            "[ロック分割] 改善後（バッチ境界でロック取り直し）: 読み込み {:.0} ms / \
             ロック取得の最長待ち {:.1} ms / ロック取得回数 {} / 途中経過の観測 {} 回 / \
             範囲取得成功 {} 回",
            split.load_elapsed.as_secs_f64() * 1000.0,
            split.max_lock_wait.as_secs_f64() * 1000.0,
            split.observations,
            split.partial_observations,
            split.fetch_ok,
        );

        // 結果の同一性（既定挙動を変えていないこと）。
        assert_eq!(
            split.total_items, whole.total_items,
            "ロックの分割は読み込み結果を変えないはず"
        );
        assert_eq!(
            split.reserved_bytes, whole.reserved_bytes,
            "予約振替量（PERF-010）も変わらないはず"
        );
        assert_eq!(split.generation, whole.generation, "世代も変わらないはず");

        // 改善前は、読み込みが終わるまで1回もロックを取れない（＝最初の待ちが
        // 読み込み時間そのものになる）。
        assert!(
            whole.max_lock_wait >= whole.load_elapsed / 2,
            "改善前は読み込み完了までブロックされるはず: 待ち {:?} / 読み込み {:?}",
            whole.max_lock_wait,
            whole.load_elapsed
        );
        assert_eq!(
            whole.partial_observations, 0,
            "改善前は読み込み途中の状態を一度も観測できないはず"
        );

        // 改善後は、1バッチの登録時間だけ待てばロックを取れる。
        assert!(
            split.max_lock_wait <= whole.max_lock_wait / 4,
            "改善後の待ち時間は改善前より桁違いに短いはず: 改善後 {:?} / 改善前 {:?}",
            split.max_lock_wait,
            whole.max_lock_wait
        );
        assert!(
            split.max_lock_wait < std::time::Duration::from_millis(200),
            "改善後の最長待ちは 200 ms 未満のはず: {:?}",
            split.max_lock_wait
        );
        assert!(
            split.partial_observations > 0,
            "改善後は読み込み途中の表示集合（伸長中の total_items）が見えるはず"
        );
        assert!(
            split.fetch_ok > 0,
            "改善後は読み込み中でも範囲取得が応答するはず"
        );
    }

    // --- 統合表示集合の読み込み完了時同期 ---

    /// 統合表示集合（P09-1）の現在の世代を、フロントエンドと同じ自己修復経路
    /// （`generation_mismatch` 応答が返す `current`。`src/log_view.js`）から
    /// 取り出します。
    ///
    /// 世代が `enable_merged_view` から何回進んだかをテストへ書き込むと、
    /// 同期回数を変えるたびにテストが壊れます。ここで確認したいのは統合表示の
    /// **内容**なので、世代は数えずに現在値を問い合わせます。
    fn current_merged_generation(registry: &mut DisplaySetRegistry, display_set_id: u32) -> u64 {
        let error = registry
            .fetch_range(
                display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: 1,
                    expected_generation: u64::MAX,
                },
            )
            .expect_err("あり得ない世代を指定したので不一致になるはず");
        match error {
            crate::registry::FetchRangeError::GenerationMismatch { current, .. } => current,
            other => panic!("統合表示集合が取得できない: {other}"),
        }
    }

    /// 統合表示集合の全項目を、本文の並びとして取り出します。
    ///
    /// 1応答の上限（`MAX_ITEMS_PER_RESPONSE`・`MAX_RESPONSE_RAW_BYTES`）に
    /// かかると全項目の並びにならないため、打ち切りが起きていないことを
    /// ここで確かめます（呼び出し側のテストは上限内の件数に収めます）。
    fn merged_texts(registry: &mut DisplaySetRegistry, display_set_id: u32) -> Vec<String> {
        let generation = current_merged_generation(registry, display_set_id);
        let response = registry
            .fetch_range(
                display_set_id,
                crate::display_set::RangeRequest {
                    start: 0,
                    max_items: crate::display_set::MAX_ITEMS_PER_RESPONSE,
                    expected_generation: generation,
                },
            )
            .expect("現在の世代なら取得できるはず");
        assert!(
            !response.truncated,
            "テストの件数は1応答の上限に収まっているはず"
        );
        response
            .items
            .iter()
            .map(|item| item.raw_text.to_string())
            .collect()
    }

    // 受け入れ条件（`LOG-007`、ADR-0008）: 統合表示 ON のまま新しい
    // 対象を開くと、読み込みが複数バッチに分かれても、完了時に統合表示集合へ
    // その対象の全項目が反映される（最初のバッチ分で止まらない）。並びは比較
    // キー昇順で、同一キーは source_ordinal（= 登録順）→ ソース内の出現順。
    #[test]
    fn merged_view_contains_all_items_after_a_multi_batch_load_completes() {
        // 先に開く既存ソース（source_ordinal = 0）。
        let existing = TempFile::create_text(
            "merged-existing",
            "2026/07/28 15:12:01.000 B-01\n\
             2026/07/28 15:12:03.000 B-03\n\
             2026/07/28 15:12:05.000 B-05\n",
        );
        // 後から開く対象（source_ordinal = 1）。15:12:03.000 の行は既存ソースと
        // 同一比較キーで、ADR-0008 の同順位解決の確認に使う。
        let added = TempFile::create_text(
            "merged-added",
            "2026/07/28 15:12:00.000 A-00\n\
             2026/07/28 15:12:02.000 A-02\n\
             2026/07/28 15:12:03.000 A-03a\n\
             2026/07/28 15:12:03.000 A-03b\n\
             2026/07/28 15:12:04.000 A-04\n\
             2026/07/28 15:12:06.000 A-06\n\
             2026/07/28 15:12:08.000 A-08\n",
        );

        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        register_source_with_control(
            &mut registry,
            &budget,
            &existing.path,
            "b.log".to_string(),
            &[],
            &LoadControl::none(),
        )
        .expect("既存ソースの読み込みは成功するはず");

        let merged = registry
            .enable_merged_view()
            .expect("統合表示を開始できるはず");
        assert_eq!(merged.total_items, 3);

        // 1行あたり約29バイトなので、chunk_bytes=28 なら概ね1チャンク=1行に
        // なり、伸長（grow_source_items）が何度も起きる状況を作れる。修正前は
        // 最初のバッチ分（1件）しか統合表示へ現れなかった。
        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &added.path,
            "a.log".to_string(),
            &[],
            &LoadControl {
                chunk_bytes: 28,
                ..LoadControl::none()
            },
        )
        .expect("追加ソースの読み込みは成功するはず");
        assert_eq!(outcome.outcome, TaskOutcome::Completed);
        assert_eq!(outcome.handle.total_items, 7);

        let texts = merged_texts(&mut registry, merged.display_set_id);
        assert_eq!(
            texts,
            vec![
                "2026/07/28 15:12:00.000 A-00",
                "2026/07/28 15:12:01.000 B-01",
                "2026/07/28 15:12:02.000 A-02",
                // 同一キー（15:12:03.000）は、先に開いた b.log が先。同じソース
                // 内ではファイル内の出現順（A-03a → A-03b）。
                "2026/07/28 15:12:03.000 B-03",
                "2026/07/28 15:12:03.000 A-03a",
                "2026/07/28 15:12:03.000 A-03b",
                "2026/07/28 15:12:04.000 A-04",
                "2026/07/28 15:12:05.000 B-05",
                "2026/07/28 15:12:06.000 A-06",
                "2026/07/28 15:12:08.000 A-08",
            ]
        );
    }

    // 受け入れ条件: キャンセルによる部分読み込みの確定でも統合
    // 表示集合が同期され、その時点までに確定した項目がすべて含まれる（読み込み
    // 済み範囲は保持されるため、統合表示から欠ける理由がない）。
    #[test]
    fn merged_view_is_synced_when_a_load_ends_as_cancelled_partial() {
        let mut contents = String::new();
        for i in 0..200 {
            contents.push_str(&format!("2026/07/28 15:13:{:02}.000 行{i}\n", i % 60));
        }
        let file = TempFile::create_text("merged-cancel", &contents);

        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();
        // 対象を1件も開いていない状態で統合表示を ON にする（`LOG-008`。
        // 既存ソースを置かないのは、[`AfterFirstBatch`] が「レジストリに
        // ソースが現れたか」で最初のバッチ登録を判定するため。既存ソースが
        // あると、この対象の1バッチ目より前に判定が成立してしまう）。
        let merged = registry
            .enable_merged_view()
            .expect("統合表示を開始できるはず");
        assert_eq!(merged.total_items, 0);

        let token = CancellationToken::new();
        let control = LoadControl {
            // `cancellation_after_some_batches_preserves_already_loaded_items`
            // と同じ作り。最初のバッチ登録を観測してからキャンセルを要求する
            // ため、「登録済み・全件読み込み前」で必ず停止する。
            chunk_bytes: 28,
            cancellation: Some(&token),
            ..LoadControl::none()
        };
        let mut access = AfterFirstBatch::new(&mut registry, || token.request_cancel());
        let outcome = register_source_with_access(
            &mut access,
            &budget,
            &file.path,
            "cancelled.log".to_string(),
            &[],
            &control,
        )
        .expect("キャンセルはエラーではなく正常応答扱い");
        assert!(
            access.fired(),
            "最初のバッチ登録を観測してキャンセルを要求したはず"
        );

        assert_eq!(outcome.outcome, TaskOutcome::Cancelled);
        assert!(
            outcome.handle.total_items > 1,
            "最初のバッチの後にも項目が確定し、伸長経路を通っているはず: {}",
            outcome.handle.total_items
        );
        assert!(
            outcome.handle.total_items < 200,
            "全件は読み込んでいないはず（キャンセルされたので）"
        );

        let texts = merged_texts(&mut registry, merged.display_set_id);
        assert_eq!(
            texts.len() as u64,
            outcome.handle.total_items,
            "部分読み込みで確定した全項目が統合表示へ含まれるはず"
        );
        assert_eq!(
            texts[0], "2026/07/28 15:13:00.000 行0",
            "先頭は最初の行のままのはず"
        );
    }

    // 受け入れ条件: 統合表示が OFF のときは、読み込み完了時の
    // 同期が何も行わない（統合表示集合を勝手に作らない）。
    #[test]
    fn completing_a_load_does_not_create_a_merged_view_when_disabled() {
        let file = TempFile::create_text(
            "merged-off",
            "2026/07/28 15:12:00.000 行0\n2026/07/28 15:12:01.000 行1\n",
        );
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "off.log".to_string(),
            &[],
            &LoadControl {
                chunk_bytes: 28,
                ..LoadControl::none()
            },
        )
        .expect("読み込みは成功するはず");

        assert_eq!(outcome.handle.total_items, 2);
        assert!(
            !registry.is_merged_view_enabled(),
            "統合表示は OFF のままのはず"
        );
    }

    // --- 段階別内訳（LoadStageTimings） ---

    // 受け入れ条件: 成功した読み込みでは、4段階の計時区間が互いに
    // 重ならない（合計が total を超えない）。区間が重なると「その他」が負になり、
    // 内訳としての意味を失うため、これが内訳の成立条件そのものになる。
    #[test]
    fn stage_timings_partition_the_total_without_overlapping() {
        // 複数チャンクに割れる小さなチャンクサイズにして、チャンク境界ごとの
        // 累計（feed／parse／deliver）が実際に複数回加算される経路を通す。
        let mut contents = String::new();
        for index in 0..200 {
            contents.push_str(&format!(
                "2026/07/28 15:12:{:02}.000 行{index}\n",
                index % 60
            ));
        }
        let file = TempFile::create_text("stage-timings", &contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "stages.log".to_string(),
            &[],
            &LoadControl {
                chunk_bytes: 64,
                ..LoadControl::none()
            },
        )
        .expect("読み込みは成功するはず");

        let timings = outcome.summary.stage_timings;
        let staged =
            timings.io_read + timings.decode + timings.parse + timings.deliver + timings.other();
        assert_eq!(
            staged, timings.total,
            "4段階と「その他」の合計は total と一致するはず（内訳の定義）"
        );
        assert!(
            timings.io_read + timings.decode + timings.parse + timings.deliver <= timings.total,
            "各段階の合計が total を超えている（区間が重なっている）: {timings:?}"
        );
        assert!(
            timings.total > Duration::ZERO,
            "読み込みには必ず時間がかかるため 0 にはならないはず: {timings:?}"
        );
    }

    // 受け入れ条件: 段階別内訳は診断ログ向けの付随情報であり、
    // 読み込み結果そのもの（項目数・要約の他の項目）を変えない。計測を常時
    // 有効にしても挙動が変わらないことの確認。
    #[test]
    fn stage_timings_do_not_change_the_load_result() {
        let contents = "2026/07/28 15:12:00.000 行0\n継続行\n2026/07/28 15:12:01.000 行1\n";
        let file = TempFile::create_text("stage-timings-result", contents);
        let mut registry = DisplaySetRegistry::new();
        let budget = crate::budget::SourceBudget::new();

        let outcome = register_source_with_control(
            &mut registry,
            &budget,
            &file.path,
            "stages-result.log".to_string(),
            &[],
            &LoadControl::none(),
        )
        .expect("読み込みは成功するはず");

        assert_eq!(outcome.handle.total_items, 2, "継続行は結合されるはず");
        assert_eq!(outcome.summary.line_count, 3);
        assert_eq!(outcome.summary.detected_datetime_format, Some("LOG-DT-001"));
    }
}
