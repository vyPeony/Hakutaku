// ログ表示ビュー（仮想スクロール、保持上限、破棄・再取得、世代の処理、選択と
// クリップボードコピー）。P04-2。選択とコピーは P10
// で「モジュール冒頭のコメント『P10』」のとおり本実装へ置き換えた。
//
// 純粋ロジック（可視範囲計算・必要チャンク選定・破棄対象選定・バイト数計算）は
// virtual_scroll.js に切り出し、このモジュールは IPC 呼び出し（Rust コア層）と
// DOM 操作、それらをつなぐ状態管理だけを担当する（ADR-0006。GUI 層に解析
// ロジックを持ち込まない）。
//
// PERF-012「取得済みの行を累積しない」を満たすため、行データを保持する場所を
// `state.chunkCache`（1箇所）に限定する。DOM 行要素・イベントリスナー・
// クロージャの中に行データを保持しない設計にする。
//   - 行 DOM は可視範囲が変わるたびに全て作り直す（使い回しによる取り違えの
//     リスクを避け、保持と解放の対応を単純にする）。行ごとのイベント
//     リスナーは一切追加しない（PERF-012 の累積源になるため。禁止事項）。
//   - 取得中チャンクの二重取得は `state.inFlightChunkFetches` で防ぐ。
//   - 保持上限超過時は `virtual_scroll.js` の `selectChunksToEvict` を使い、
//     表示範囲から遠いチャンクから `state.chunkCache` ごと破棄する。
//
// Tauri の IPC 呼び出しは src/main.js と同じ理由で
// window.__TAURI_INTERNALS__.invoke を直接使う。
//
// # 範囲取得応答の適用条件（Issue #34）
//
// 範囲取得（`fetch_log_range`）は非同期であり、往復の最中にタブの切り替えや
// 再読み込み（`reload_target`。表示集合 ID を保ったまま世代だけが進む）が起こり
// 得る。そのため、要求は発行した時点の表示集合 ID と世代を束ねた
// `ChunkFetchRequest` として扱い、**成功・失敗のどちらの応答も、その文脈が現在
// の表示と一致する場合にだけ適用する**。一致しない応答は適用せず捨てる。適用して
// しまうと、総行数・世代・本文を古い表示集合や古い世代の値へ巻き戻し、
// 再読み込み後の表示が次の世代不一致まで誤った内容のままになる（`LOG-028`）。
//
// 捨てた分の取り直しは、チャンク取得が決着するたびに予約する再描画
// （`ensureChunksLoaded` の `finally`）が現在の文脈で改めて要求するため、
// 取りこぼしにはならない。表示集合の切り替えと世代の更新では、取得中の登録
// （`state.inFlightChunkFetches`）も破棄する（IPC にキャンセル手段が無く、古い
// 登録を残すと新しい文脈の取得が「二重取得」と誤判定されて抑止されるため）。
//
// 取得に失敗したチャンクは `state.failedChunkFetches` へ記録し、行の文言を
// 「（読み込み中…）」と区別できる失敗表示へ変えたうえで、短いバックオフを置いて
// 自動的に取り直す。通知は一連の失敗の1件目だけに出す（同じ文の通知を積み増さない。
// `src/banner.js` の集約方針と同じ趣旨）。
//
// # 形式別ビューアの差し込み口（P07-1）
//
// このモジュールは「形式別ビューア」の実装の1つ（テキストログ向け）です。
// 共通シェル（src/shell.js）はタブ切り替えのたびに、このモジュールが公開する
// `logViewer`（`activate` / `showEmpty` の2関数だけの最小契約）を通じてだけ
// ビュー領域を操作します。表示集合の切り替えは、このモジュール内部の
// 単一の `state` をタブ切り替えのたびに再初期化する方式です（複数タブ分の
// 内部状態を同時に保持しません。保持上限はタブ間合計ではなく現在表示中の
// ものだけで良いという P07-1 の設計判断）。ファイルを開くボタンや参照対象
// 一覧の管理は共通シェル側（src/shell.js）の責務であり、このモジュールは
// 一切関与しません（`open_log_file` の呼び出しも shell.js が行います）。
//
// 将来 DICOM（P14）や SQLite 等の非テキスト形式のビューアを追加する場合も、
// 同じ2関数（`activate` / `showEmpty`）を実装した別モジュールを用意すれば
// 共通シェルへ差し込めます（`9.2` の責務分離のうち「ビュー」に対応する境界。
// `docs/architecture/decisions/` へ ADR を起こすことを後続課題とします）。
//
// # P08-2: 表示の完成度（列・元精度日時・継続行・未確定行・
// # ジャンプ）
//
// 継続行の折りたたみ表示（`LOG-014`）・元精度の日時列（`LOG-024`）・
// 未確定行と生表示の区別（`LOG-026`／`LOG-022`）・行番号ジャンプを追加した。
// 採用方式と根拠は `virtual_scroll.js` 冒頭のコメント「可変行高ではなく
// 『折りたたみ方式』を採用した理由」を参照（要点: 行高は常に `ROW_HEIGHT_PX`
// で固定のまま、継続行は「+N行」バッジ→下部詳細パネル（`showDetailPanel`）で
// 改行を保って全文表示する）。
//
// このうち**日時列は Issue #78 で廃止した**（`buildRowElement`）。行の原文は
// 先頭に日時を含むため、その左に解析済みの日時列を置くと同じ日時が画面上で
// 2回並ぶ。`LOG-024` が求める「元の精度を保持した日時を書式変換せずに提示
// する」ことは、ツールバーのコピー列「日時」（ADR-0009）が引き続き満たす。
//
// # P10: 選択・上限判定・コピー生成とクリップボード設定
//
// P04-4 の「ブラウザ既定のテキスト選択 + Ctrl+C」による最小コピーを、正式な
// 選択モデル（`src/selection.js`）と `copy_selection` コマンド経由の経路へ
// 置き換えた（`COPY-001`〜`006`）。
//
// - **行選択**: クリックで単一行、Shift+クリックで範囲、Ctrl+A で全行。
//   選択は表示集合全体のインデックス範囲（`state.selection`）として保持し、
//   表示されていない行（未取得のチャンク）も選択できる（全選択が仮想
//   スクロールと両立する。`PERF-012`: 選択はインデックスだけを保持し、
//   行の本文を一切保持しない）。選択行は `.log-row--selected` でハイライト
//   する。
// - **セル範囲（複数列）**: 行選択とは独立に、ツールバーの「コピー列」
//   チェックボックス（`state.copyColumns`）で「行番号／日時／本文」の
//   組を選ぶ。本文のみ（既定）を選んだ場合は原文そのまま、複数列を選んだ
//   場合は quoted TSV としてコピーされる（`ADR-0009`。生成そのものは
//   `hakutaku_core::assemble_copy` が行う）。計画正本が示す「選択メニュー
//   （右クリックまたはツールバー）で列の組を選んでコピー」のうち、
//   常設ツールバー方式を採用した（右クリックメニューの開閉・外側クリックで
//   の消去といった追加の状態管理を避け、実装を単純に保つ判断）。
// - **Ctrl+C**: 選択があれば `copy_selection` を呼ぶ（`preventDefault` で
//   ブラウザ既定のコピーを抑止し、常にこの経路を通す）。選択が無ければ
//   何もしない（クリップボードを変更しない。`COPY-006`）。上限判定・
//   quoted TSV の生成・Win32 クリップボード書き込みはすべて Rust 側
//   （`hakutaku_core::assemble_copy`・`src-tauri/src/clipboard.rs`）が行う。
//   スクロールや選択変更（クリック・Shift+クリック・Ctrl+A 自体）では
//   `copy_selection` を呼ばない（`COPY-006`／`SEC-004`: 明示的な操作時だけ）。
// - ファイルエクスポート機能は作らない（`COPY-003`／`LOG-019`）。
// - **時系列統合表示（P09-1）でも同じ経路でコピーする**（Issue #37）。
//   `copy_selection` へ渡す `display_set_id`／`generation` は範囲取得と同じ
//   （`state` が保持している現在の表示集合のもの）で、統合表示かどうかで
//   分岐しない。コピーに含まれる列も個別ファイルと同じ「行番号／日時／本文」
//   であり、統合表示の画面にだけ出る読み込み元ラベル列（`LOG-007`）は
//   含まれない（列の並びは ADR-0009 が固定しているため）。
//
// ## 新しい単一購読（PERF-012: 行ごとのイベントリスナーを追加しない設計の継続）
//
// 継続行バッジ（`.log-row__badge--continuation`）はクリック可能だが、行ごとに
// リスナーを追加すると PERF-012 の禁止事項に反する。そのため、他の行 DOM と
// 同様にバッジ自身へはリスナーを一切付けず、`#log-rows`（`elements.rows`）へ
// `initLogView` 時に1つだけ登録したクリックの委譲ハンドラー
// （`handleRowsClick`）が `event.target.closest(...)` で判定する。バッジは
// 実体が `<button>` 要素であるため、Enter / Space キーでの操作も委譲された
// `click` イベントとして自然に届く（キーボード操作用に別途リスナーを足す
// 必要がない）。行選択のクリックも同じ委譲ハンドラーの中で扱う（行ごとに
// 別のリスナーを足さない）。
//
// ジャンプ入力欄・ジャンプボタン・詳細パネルの閉じるボタン・コピー列の
// チェックボックス・ビューポートの keydown（Ctrl+Home／Ctrl+End／Ctrl+A／
// Ctrl+C）も、既存の scroll・resize 購読と同じく `initLogView` で1回だけ
// 登録する単一の購読であり、行数に比例して増えるものではない（PERF-012 の
// 累積源にならない）。
//
// # Issue #48: タブ切り替え時のスクロール・選択の復元、無効なジャンプ入力の明示
//
// 従来、`activateDisplaySet` はタブ切り替え（共通シェルのタブ操作、
// タブを閉じた後の再表示を含む）のたびにスクロール位置を先頭・選択を空へ
// 戻していたため、他のタブを見てから戻ると読んでいた位置を見失っていた。
// 現在は、離れる側のタブの `scrollTop`・選択範囲を `savedTabViewStates`
// （displaySetId をキーとする小さな Map）へ保存し、同じ世代のタブへ戻る
// 場合にだけ復元する（`activateDisplaySet` の doc コメント参照）。
// `reload_target` による世代の更新（＝再読み込み）を挟んだ場合は復元条件
// （世代の一致）を満たさなくなるため、従来どおり先頭へ戻る。
//
// ジャンプ入力欄（`#log-jump-input`）に無効な値（空欄・範囲外など、
// `parseJumpTargetRowIndex` が `null` を返す入力）を送信しようとした場合は、
// バナーを出さず `aria-invalid="true"` と CSS（`src/styles.css` の
// `.log-toolbar__jump-input[aria-invalid="true"]`）による境界色の変化で
// 入力欄自体に明示する（`handleJumpRequest`）。次にその欄へ入力した時点
// （`handleJumpInputInput`）で解除する。

import {
  chunkIndexForRow,
  computeChunkRange,
  computeRequiredChunkIndices,
  sumRawTextBytes,
  selectChunksToEvict,
  extractFirstLine,
  parseJumpTargetRowIndex,
  computeVisibleRangeForScroll,
  computeSpacerHeightsForScroll,
  computeScrollTopForRowIndexScaled,
} from "./virtual_scroll.js";
import {
  createSelectionState,
  selectSingleRow,
  extendSelectionTo,
  selectAll,
  getSelectionRange,
  isRowSelected,
  defaultCopyColumns,
  hasAnyCopyColumn,
} from "./selection.js";
import { showErrorBanner, showInfoBanner } from "./banner.js";
import * as retentionStats from "./retention_stats.js";

/**
 * 1チャンクあたりの行数。Rust 側の1回の転送上限
 * （`hakutaku_core::MAX_ITEMS_PER_RESPONSE` = 512）と一致させ、チャンク境界と
 * 転送境界を揃える（`crates/core-services/src/display_set.rs`）。
 */
const CHUNK_SIZE = 512;

/**
 * 可視範囲の前後に確保するバッファ行数（仕様の「例: 上下各50行」）。
 *
 * P08-2（作業項目9）: この値は先読み量そのものであり、大きくする
 * ほどスクロールのたびに取得するチャンク数が増える。本アプリは運用先の専用
 * 端末上で動く前提（`PERF-014`）のため、その端末の CPU・I/O を
 * 圧迫しないよう、実測で必要性が確認されない限りこの控えめな値のまま維持する
 * （`PERF-014` が明示する3設定——同時実行数・I/O 間隔・プロセス優先度——には
 * 先読み量は含まれないが、同じ趣旨の追加の抑制手段として扱う。既定値の
 * 見直しは P13 の実測対象）。
 */
const BUFFER_ROWS = 50;

/**
 * 固定行高（px）。`src/styles.css` の `.log-row { height: ... }` と必ず一致
 * させる（ビルド工程が無いため、値の同期は手動。変更する場合は両方を更新する）。
 */
const ROW_HEIGHT_PX = 22;

/**
 * 取得に失敗したチャンクを自動で取り直すまでの最短待ち時間（ミリ秒。Issue #34）。
 *
 * 失敗した行を放置しないためには自動の再取得が要るが、失敗のたびに即座に取り直す
 * と、失敗が続く間 IPC を叩き続けるホットループになる（`PERF-014` が求める端末の
 * 負荷抑制にも反する）。人が「反応が無い」と感じない程度に短く、かつ連続失敗時に
 * 往復を積み上げない値としてこの初期待ち時間を置く。
 */
const FETCH_RETRY_BASE_DELAY_MS = 1_000;

/**
 * 連続失敗時に伸ばす待ち時間の上限（ミリ秒。Issue #34）。
 *
 * 待ち時間は失敗のたびに倍にしていく（恒久的な失敗——表示集合が消えた等——で
 * 無駄な往復を続けないため）が、上限を設けないと一時的な失敗から復帰したときに
 * 何分も待たされる。復帰の検知が遅れすぎない範囲で頭打ちにする。
 */
const FETCH_RETRY_MAX_DELAY_MS = 30_000;

/**
 * `savedTabViewStates` に保持する最大エントリ数（Issue #48）。
 *
 * `PERF-005`（対象は10件程度を想定）に対して十分な余裕を持たせつつ、対象を
 * 開いては閉じる操作をセッション中に延々と繰り返しても無制限に増えないよう
 * 頭打ちにする。上限を超えたら、最も長く参照されていないエントリ（Map の
 * 挿入順で先頭。`rememberTabViewState` が参照のたびに末尾へ移し替える）から
 * 追い出す。
 */
const MAX_SAVED_TAB_VIEW_STATES = 32;

/**
 * @typedef {Object} SavedTabViewState タブを離れる直前に保存する、そのタブへ
 * 戻ったときに復元する最小限のビュー状態（Issue #48、主セッション裁定2）。
 * @property {number} generation 保存時点の世代。`activateDisplaySet` は、
 *   復元先の世代がこれと一致する場合（＝再読み込みを挟んでいない同一世代の
 *   タブ切り替え）にだけ復元する。再読み込みで世代が進んだ場合は、この
 *   エントリは二度と一致しなくなり（世代は増える一方のため）、結果として
 *   常に先頭へ戻る（主セッション裁定の「再読み込みは先頭へ戻す」）。
 * @property {number} scrollTop 保存時点の `elements.viewport.scrollTop`（px）。
 * @property {import("./selection.js").SelectionState} selection 保存時点の選択範囲
 *   （アンカー・フォーカスの2つの数値のみ。行データは含まない）。
 */

/**
 * @type {Map<number, SavedTabViewState>} displaySetId ->
 * 直近にそのタブを離れた時点のビュー状態（Issue #48）。`activateDisplaySet`
 * が離れる側で書き込み、戻ってきた側で読み出す。`reload_target`
 * （`LOG-028`、ADR-0007）は表示集合 ID を保ったまま世代だけを進める設計
 * （モジュール冒頭のコメント「応答適用条件」参照）のため、displaySetId は
 * タブ（対象）の識別子として安定して使える。
 *
 * PERF-012「取得済みの行を累積しない」への抵触検討: ここに保持するのは
 * displaySetId 1件あたり数値4つ（世代・scrollTop・アンカー・フォーカス）
 * だけであり、行の本文・バイト数は一切保持しない。エントリ数も
 * `MAX_SAVED_TAB_VIEW_STATES` で頭打ちにしているため、PERF-012 が問題視する
 * 「行データの累積」には当たらない。
 */
const savedTabViewStates = new Map();

/**
 * `displaySetId` のビュー状態を記録する（最近参照した順に並べ替え、上限超過分は
 * 最も古いものから追い出す。Issue #48）。
 *
 * @param {number} displaySetId
 * @param {SavedTabViewState} viewState
 */
function rememberTabViewState(displaySetId, viewState) {
  // Map は挿入順を保持する。既存キーを一度消してから入れ直すことで
  // 「最近使った順」の末尾へ移動させる（簡易 LRU）。
  savedTabViewStates.delete(displaySetId);
  savedTabViewStates.set(displaySetId, viewState);
  while (savedTabViewStates.size > MAX_SAVED_TAB_VIEW_STATES) {
    const oldestKey = savedTabViewStates.keys().next().value;
    savedTabViewStates.delete(oldestKey);
  }
}

/**
 * 現在表示中の表示集合（統合表示を除く）のビュー状態を `savedTabViewStates`
 * へ保存する。表示集合を切り替える・空表示にする直前に呼ぶ（Issue #48）。
 *
 * 統合表示（P09-1）を保存しないのは、統合表示が「タブ」（src/tabs.js）を
 * 持たず、ON にするたびに新しい表示集合を作り直す設計だからである
 * （`handleMergedViewToggleClick`）。主セッション裁定も「タブごとに」
 * 保持する対象と定めており、統合表示はその対象外。
 */
function rememberCurrentTabViewStateIfApplicable() {
  if (state.displaySetId === null || state.isMerged) {
    return;
  }
  rememberTabViewState(state.displaySetId, {
    generation: /** @type {number} */ (state.generation),
    scrollTop: elements.viewport.scrollTop,
    selection: state.selection,
  });
}

/**
 * 保存済みの選択範囲を、現在の `totalItems` の範囲へクランプする（Issue #48、
 * 主セッション裁定「復元時は totalItems の範囲へクランプ」）。表示集合が
 * タブを離れている間に縮む通常の経路は無い（読み込みは伸びる方向にしか
 * 進まない設計）が、防御的に丸める。
 *
 * @param {import("./selection.js").SelectionState} selection
 * @param {number} totalItems
 * @returns {import("./selection.js").SelectionState}
 */
function clampSelectionToTotalItems(selection, totalItems) {
  if (selection.anchorIndex === null || selection.focusIndex === null || totalItems <= 0) {
    return createSelectionState();
  }
  const maxIndex = totalItems - 1;
  return {
    anchorIndex: Math.min(selection.anchorIndex, maxIndex),
    focusIndex: Math.min(selection.focusIndex, maxIndex),
  };
}

/**
 * @typedef {Object} LogItemDto `fetch_log_range` 応答の1項目
 * （`src-tauri/src/log_view.rs` の `LogItemDto` の JSON 表現）。
 * @property {number} source_id
 * @property {number} seq
 * @property {string | null} timestamp ISO 8601 風の表示文字列。元の精度を
 *   保持したまま（`LOG-024`）。解析できなかった場合は `null`。
 * @property {string} raw_text 原文。継続行（`LOG-014`）を含む項目は、結合済み
 *   本文を改行（`\n`）付きでそのまま保持する（`crates/core-services/src/item.rs`
 *   参照）。行一覧では `virtual_scroll.js` の `extractFirstLine` で1行目だけを
 *   表示し、全文は詳細パネル（`showDetailPanel`）で確認する。
 * @property {string} source_label
 * @property {number} source_line_number
 * @property {boolean} confirmed 未確定行（書き込み途中の可能性がある末尾の
 *   断片）ではないか（`LOG-026`）。`false` は解析エラーではなく、未確定行
 *   バッジで表示する。
 * @property {number} continuation_count 結合された継続行の数。0 は継続行なし。
 * @property {boolean} raw_display 日時未解析の生データ項目か（`LOG-022`）。
 */

/**
 * @typedef {Object} CachedChunk
 * @property {number} start このチャンクが実際にカバーする表示集合内の先頭インデックス。
 * @property {LogItemDto[]} items
 * @property {number} byteCount `items` の `raw_text` の UTF-8 バイト数合計。
 */

/** モジュール内部状態。P04 は単一の表示集合だけを扱う（1ファイルのみ）。 */
const state = {
  /** @type {number | null} */
  displaySetId: null,
  /** @type {number | null} */
  generation: null,
  totalItems: 0,
  sourceLabel: "",
  /**
   * @type {boolean} 現在表示中の表示集合が時系列統合表示（P09-1）
   * かどうか。true の間だけ、行ごとの読み込み元ラベル列（LOG-007）を表示する
   * （`buildRowElement`）。
   */
  isMerged: false,
  /** get_config_status の frontend_retention（CFG-022）。initLogView で必ず上書きされる
   * 想定の安全側の初期値（ハードコード方針違反ではなく、初期化前の一時的な既定値）。 */
  retentionLimits: { maxRows: 10_000, maxBytes: 64 * 1024 * 1024 },
  /** @type {Map<number, CachedChunk>} chunkIndex -> チャンク（行データの唯一の保持点）。 */
  chunkCache: new Map(),
  /**
   * @type {Map<number, Promise<void>>} chunkIndex -> 取得中の Promise（二重取得
   * 防止）。表示集合の切り替え・世代の更新では
   * `forgetInFlightChunkFetches` で登録ごと破棄する（Issue #34）。
   */
  inFlightChunkFetches: new Map(),
  /**
   * @type {Map<number, { attempts: number, retryAtMs: number }>} chunkIndex ->
   * 取得に失敗したチャンクの再試行予定（Issue #34）。`attempts` は連続失敗回数
   * （バックオフの算出用）、`retryAtMs` は次に取り直してよい時刻
   * （`Date.now()` 基準）。行データは持たないため、PERF-012 が禁じる
   * 「行データの累積」には当たらない（上限は可視範囲のチャンク数程度）。
   */
  failedChunkFetches: new Map(),
  /**
   * @type {ReturnType<typeof setTimeout> | null} バックオフ明けに再描画
   * （＝自動再取得）を起こすためのタイマー。常に1本だけ持つ。
   */
  retryTimerId: null,
  /**
   * @type {Set<number>} 一度でもキャッシュしたことのあるチャンク番号（再取得
   * 回数の判定用）。行データそのものではなく小さな整数の集合であり、上限は
   * チャンク総数（total_items / CHUNK_SIZE）程度で頭打ちになるため、
   * PERF-012 が禁じる「行データの累積」には当たらない。
   */
  everCachedChunkIndices: new Set(),
  renderScheduled: false,
  /**
   * 行選択（P10、COPY-001）。表示集合内のインデックス範囲だけを
   * 保持し、行の本文は一切保持しない（PERF-012）。
   * @type {import("./selection.js").SelectionState}
   */
  selection: createSelectionState(),
  /**
   * コピーする列の組（P10、COPY-001）。行選択とは独立に、ツールバーの
   * チェックボックスで変更する。
   * @type {import("./selection.js").CopyColumns}
   */
  copyColumns: defaultCopyColumns(),
  /** @type {boolean} copy_selection 呼び出し中の多重実行防止。 */
  copyInFlight: false,
};

/** @type {{
 *   sourceLabel: HTMLElement,
 *   totalItemsLabel: HTMLElement,
 *   viewport: HTMLElement,
 *   rows: HTMLElement,
 *   topSpacer: HTMLElement,
 *   bottomSpacer: HTMLElement,
 *   jumpInput: HTMLInputElement,
 *   jumpButton: HTMLButtonElement,
 *   detailPanel: HTMLElement,
 *   detailPanelTitle: HTMLElement,
 *   detailPanelBody: HTMLElement,
 *   detailPanelCloseButton: HTMLButtonElement,
 *   copyColumnLineNumber: HTMLInputElement,
 *   copyColumnTimestamp: HTMLInputElement,
 *   copyColumnRawText: HTMLInputElement,
 * } | null} */
let elements = null;

/**
 * @param {{ displaySetId: number, expectedGeneration: number, start: number, maxItems: number }} args
 * @returns {Promise<{ generation: number, total_items: number, start: number, items: LogItemDto[], truncated: boolean }>}
 */
function invokeFetchLogRange(args) {
  return window.__TAURI_INTERNALS__.invoke("fetch_log_range", args);
}

/**
 * `copy_selection` コマンドを呼び出す（P10、COPY-002）。
 *
 * @param {{ displaySetId: number, generation: number, start: number, count: number, columns: import("./selection.js").CopyColumns }} args
 * @returns {Promise<
 *   | { copied: { bytes: number, lines: number } }
 *   | { rejected: { limit_bytes: number, limit_lines: number, selected_lines: number, selected_bytes?: number } }
 * >}
 */
function invokeCopySelection(args) {
  return window.__TAURI_INTERNALS__.invoke("copy_selection", args);
}

/**
 * ログ表示ビューを初期化する。フロントエンドは保持上限をハードコードせず、
 * `get_config_status` の応答（`frontend_retention`、CFG-022）から受け取る
 * （呼び出し元は src/shell.js）。
 *
 * 「ファイルを開く」ボタンの配線・参照対象一覧の管理は共通シェル
 * （src/shell.js）の責務であり、このモジュールは行わない（モジュール冒頭の
 * コメント「形式別ビューアの差し込み口」を参照）。
 *
 * @param {{ maxRows: number, maxBytes: number }} retentionLimits
 */
export function initLogView(retentionLimits) {
  state.retentionLimits = retentionLimits;
  retentionStats.publishToWindow();

  elements = {
    sourceLabel: document.getElementById("log-source-label"),
    totalItemsLabel: document.getElementById("log-total-items"),
    viewport: document.getElementById("log-viewport"),
    rows: document.getElementById("log-rows"),
    topSpacer: document.getElementById("log-spacer-top"),
    bottomSpacer: document.getElementById("log-spacer-bottom"),
    jumpInput: /** @type {HTMLInputElement} */ (document.getElementById("log-jump-input")),
    jumpButton: /** @type {HTMLButtonElement} */ (document.getElementById("log-jump-button")),
    detailPanel: document.getElementById("log-detail-panel"),
    detailPanelTitle: document.getElementById("log-detail-panel-title"),
    detailPanelBody: document.getElementById("log-detail-panel-body"),
    detailPanelCloseButton: /** @type {HTMLButtonElement} */ (
      document.getElementById("log-detail-panel-close")
    ),
    copyColumnLineNumber: /** @type {HTMLInputElement} */ (
      document.getElementById("log-copy-column-line-number")
    ),
    copyColumnTimestamp: /** @type {HTMLInputElement} */ (
      document.getElementById("log-copy-column-timestamp")
    ),
    copyColumnRawText: /** @type {HTMLInputElement} */ (
      document.getElementById("log-copy-column-raw-text")
    ),
  };

  // コピー列チェックボックスの初期表示を state.copyColumns（既定値）と揃える。
  syncCopyColumnCheckboxesFromState();

  // scroll・resize・rows のクリック委譲・ジャンプ操作・詳細パネルの開閉・
  // コピー列の変更・ビューポートの keydown は、いずれも行数に関わらず1つ
  // だけ持つ購読（行ごとの購読を増やさない。PERF-012 の禁止事項。モジュール
  // 冒頭のコメント「新しい単一購読」参照）。
  elements.viewport.addEventListener("scroll", scheduleRender);
  window.addEventListener("resize", scheduleRender);
  elements.viewport.addEventListener("keydown", handleViewportKeydown);
  elements.rows.addEventListener("click", handleRowsClick);
  elements.jumpButton.addEventListener("click", handleJumpRequest);
  elements.jumpInput.addEventListener("keydown", handleJumpInputKeydown);
  elements.jumpInput.addEventListener("input", handleJumpInputInput);
  elements.detailPanelCloseButton.addEventListener("click", hideDetailPanel);
  elements.copyColumnLineNumber.addEventListener("change", handleCopyColumnsChange);
  elements.copyColumnTimestamp.addEventListener("change", handleCopyColumnsChange);
  elements.copyColumnRawText.addEventListener("change", handleCopyColumnsChange);

  renderVisibleRows();
}

/**
 * 表示集合を切り替える（共通シェルのタブ切り替え、`open_log_file` /
 * `open_config_data_source` / `retry_target` が返した `opened` 応答、計測
 * モードのいずれからも呼ばれる）。前の表示集合のキャッシュを全破棄してから
 * 新しい内容の取得を開始する。
 *
 * `logViewer`（本モジュール末尾）が公開する「形式別ビューアの差し込み口」
 * 契約のうち `activate` の実体。
 *
 * # タブ切り替え時のスクロール位置・選択の復元（Issue #48、主セッション裁定2）
 *
 * 離れる側のタブの `scrollTop`・選択範囲を `savedTabViewStates` へ保存し
 * （`rememberCurrentTabViewStateIfApplicable`）、戻る側のタブに保存済みかつ
 * 世代が一致するエントリがあれば復元する。世代が異なる場合（＝再読み込みを
 * 挟んでいる）は復元せず先頭へ戻す。
 *
 * 復元する `scrollTop` を代入する前に、可視範囲を空にした状態で一度
 * `renderRows` を呼び、上下スペーサの高さを新しい `totalItems` に合わせて
 * 確定させている。`computeSpacerHeightsForScroll` はどの可視範囲を渡しても
 * スペーサ高さの合計が総高さと厳密に一致するよう作られているため
 * （`virtual_scroll.js` の同関数 doc コメント参照）、この「空範囲での事前
 * 描画」は総高さ（`viewport.scrollHeight`）だけを正しく先に確定させる安全な
 * 手段になる。この順序を踏まないと、直前のタブの内容量に基づく古い
 * `scrollHeight` を基準に `scrollTop` の代入がブラウザ側でクランプされ、
 * 復元先が手前へ丸められてしまう。
 *
 * @param {DisplaySetDescriptor} descriptor
 */
export function activateDisplaySet(descriptor) {
  rememberCurrentTabViewStateIfApplicable();

  clearCache();
  // 前の表示集合へ向けて発行済みの取得は、応答が届いても捨てられる（Issue #34 の
  // 文脈照合）。その登録を残したままだと、新しい表示集合の同じチャンク番号の取得
  // が二重取得と誤判定されて始まらないため、登録と失敗記録もここで手放す。
  forgetInFlightChunkFetches();
  clearFailedChunkFetches();
  state.displaySetId = descriptor.display_set_id;
  state.generation = descriptor.generation;
  state.totalItems = Number(descriptor.total_items);
  state.sourceLabel = descriptor.source_label;
  // P09-1: 統合表示（時系列統合）かどうか。呼び出し側（src/shell.js）が
  // enable_merged_view の応答を activate するときだけ true を渡す。
  state.isMerged = Boolean(descriptor.is_merged);
  state.everCachedChunkIndices.clear();

  const savedViewState = state.isMerged ? undefined : savedTabViewStates.get(state.displaySetId);
  const canRestore = savedViewState !== undefined && savedViewState.generation === state.generation;
  // 表示集合の切り替えでは、古い表示集合のインデックスを指したままにしない
  // よう選択も破棄する（PERF-012: 選択はインデックス範囲だけを保持している
  // ため、切り替え後は無意味な範囲になる）。同一世代のタブへ戻る場合だけ、
  // 保存しておいた選択を復元する。
  state.selection = canRestore
    ? clampSelectionToTotalItems(savedViewState.selection, state.totalItems)
    : createSelectionState();

  elements.sourceLabel.textContent = state.sourceLabel;
  updateTotalItemsLabel();
  elements.jumpInput.value = "";
  elements.jumpInput.removeAttribute("aria-invalid");
  hideDetailPanel();

  // 関数 doc コメント「タブ切り替え時のスクロール位置・選択の復元」参照。
  // scrollTop を読み書きする前に、新しい totalItems でスペーサ高さを確定させる。
  renderRows({ startIndex: 0, endIndex: 0 });
  if (canRestore) {
    const maxScrollTopPx = Math.max(
      0,
      elements.viewport.scrollHeight - elements.viewport.clientHeight,
    );
    elements.viewport.scrollTop = Math.min(Math.max(0, savedViewState.scrollTop), maxScrollTopPx);
  } else {
    elements.viewport.scrollTop = 0;
  }

  scheduleRender();
}

/**
 * ビュー領域を「何も選択されていない」状態にする（タブを全て閉じた直後など）。
 *
 * `logViewer`（本モジュール末尾）が公開する「形式別ビューアの差し込み口」
 * 契約のうち `showEmpty` の実体。
 */
export function showEmptyState() {
  clearCache();
  forgetInFlightChunkFetches();
  clearFailedChunkFetches();
  state.displaySetId = null;
  state.generation = null;
  state.totalItems = 0;
  state.sourceLabel = "";
  state.isMerged = false;
  state.everCachedChunkIndices.clear();
  state.selection = createSelectionState();

  if (elements) {
    elements.sourceLabel.textContent = "";
    updateTotalItemsLabel();
    elements.viewport.scrollTop = 0;
    elements.jumpInput.value = "";
    elements.jumpInput.removeAttribute("aria-invalid");
    hideDetailPanel();
  }

  scheduleRender();
}

/** ツールバーの総行数表示を現在の `state.totalItems` へ更新する。 */
function updateTotalItemsLabel() {
  elements.totalItemsLabel.textContent = `${state.totalItems.toLocaleString("ja-JP")} 行`;
}

/**
 * キャッシュ済みの全チャンクを破棄する（新しいファイルを開いた時、世代不一致を
 * 検出した時に呼び出す）。取得中の Promise には触れない（IPC にキャンセル手段が
 * 無いため）。到着した応答は `fetchChunk` の文脈照合（`isRequestContextCurrent`）
 * で捨てられる。取得中の**登録**は別途 `forgetInFlightChunkFetches` で破棄する
 * （この関数と呼び出し箇所が同じでも役割が違うため分けている。Issue #34）。
 */
function clearCache() {
  for (const chunk of state.chunkCache.values()) {
    retentionStats.recordChunkEvicted(chunk.items.length, chunk.byteCount);
  }
  state.chunkCache.clear();
}

/**
 * 取得中チャンクの登録（二重取得の抑止）を全て取り消す（Issue #34）。
 *
 * 進行中の取得そのものは止められない（IPC にキャンセル手段が無い）。止められない
 * まま登録を残すと、切り替え後の表示集合・世代で同じチャンク番号を取得しようと
 * したときに「取得中」と誤判定して抑止してしまい、古い応答が文脈照合で捨てられた
 * 後は誰も取り直さない（行が「（読み込み中…）」のまま残る）。登録だけを手放し、
 * 古い取得の結果は文脈照合で捨てる。
 */
function forgetInFlightChunkFetches() {
  state.inFlightChunkFetches.clear();
}

/**
 * 取得失敗の記録と、バックオフ明けの再描画予約を破棄する（Issue #34）。
 * 表示集合の切り替えと世代の更新では、失敗はその古い内容に対するものであり、
 * 新しい内容の表示・再取得判断へ持ち越さない。
 */
function clearFailedChunkFetches() {
  state.failedChunkFetches.clear();
  if (state.retryTimerId !== null) {
    clearTimeout(state.retryTimerId);
    state.retryTimerId = null;
  }
}

/**
 * @typedef {Object} DisplayContext IPC 要求を発行した時点の表示文脈
 * （Issue #34）。応答の適用可否は、この2つが現在の `state` と一致するかで
 * 判定する。表示集合 ID だけでは足りない（`reload_target` は表示集合 ID を
 * 保ったまま世代を進めるため、世代を照合しないと再読み込み直前の応答を
 * 「同じ表示集合のもの」として適用してしまう）。
 * @property {number} displaySetId
 * @property {number} generation
 */

/**
 * @typedef {DisplayContext & { chunkIndex: number }} ChunkFetchRequest
 * 1チャンク分の範囲取得要求（`fetchChunk` の引数）。
 */

/**
 * 要求発行時の文脈が、現在表示している内容と一致するか（Issue #34）。
 *
 * 一致しない = その要求の応答は、既に画面に無い表示集合または世代の内容を
 * 指している。成功応答なら適用せず捨て、失敗応答なら現在の表示の状態
 * （キャッシュ・世代・通知）へ一切反映しない。
 *
 * @param {DisplayContext} context
 * @returns {boolean}
 */
function isRequestContextCurrent(context) {
  return (
    state.displaySetId === context.displaySetId && state.generation === context.generation
  );
}

/** requestAnimationFrame で描画を1フレームにまとめる（連続スクロール時の過剰な再描画を防ぐ）。 */
function scheduleRender() {
  if (state.renderScheduled) {
    return;
  }
  state.renderScheduled = true;
  requestAnimationFrame(() => {
    state.renderScheduled = false;
    renderVisibleRows();
  });
}

/**
 * 現在のスクロール位置・ビューポートサイズから、可視範囲（＋バッファ）と
 * それをカバーするために必要なチャンク番号を計算する（副作用なし）。
 * `renderVisibleRows` と、チャンク取得完了直後の追加破棄判定
 * （`runPostFetchEvictionPass`）の両方から呼ぶ共通ヘルパー。
 *
 * スクロール高クランプ対応（`computeVisibleRangeForScroll`）のため、常にブラウザの
 * 実際の `scrollHeight` / `clientHeight` から `maxScrollTopPx` を読み直す
 * （`renderRows` が直前に設定したスペーサ高さ由来の値であり、呼び出し時点の
 * 最新の DOM 状態を反映する）。
 *
 * @returns {{
 *   visibleRange: import("./virtual_scroll.js").VisibleRange,
 *   requiredChunkIndices: number[],
 * }}
 */
function computeCurrentRenderTargets() {
  const maxScrollTopPx = Math.max(
    0,
    elements.viewport.scrollHeight - elements.viewport.clientHeight,
  );
  const visibleRange = computeVisibleRangeForScroll({
    scrollTop: elements.viewport.scrollTop,
    maxScrollTopPx,
    viewportHeightPx: elements.viewport.clientHeight,
    rowHeightPx: ROW_HEIGHT_PX,
    totalItems: state.totalItems,
    bufferRows: BUFFER_ROWS,
  });
  const requiredChunkIndices = computeRequiredChunkIndices(
    visibleRange.startIndex,
    visibleRange.endIndex,
    CHUNK_SIZE,
    state.totalItems,
  );
  return { visibleRange, requiredChunkIndices };
}

/**
 * 現在のスクロール位置に基づき、可視範囲の再計算・不足チャンクの取得開始・
 * 上限超過チャンクの破棄・行 DOM の再構築を行う（仮想スクロールの中心処理）。
 */
function renderVisibleRows() {
  if (state.displaySetId === null || state.totalItems <= 0) {
    renderRows({ startIndex: 0, endIndex: 0 });
    return;
  }

  const { visibleRange, requiredChunkIndices } = computeCurrentRenderTargets();

  ensureChunksLoaded(requiredChunkIndices);
  evictFarChunks(requiredChunkIndices, visibleRange);
  renderRows(visibleRange);
}

/**
 * チャンク取得完了時（キャッシュへ格納した直後）にも破棄判定を
 * 実行する。
 *
 * 背景: `renderVisibleRows` は「不足チャンクの取得を開始する
 * （`ensureChunksLoaded`。非同期、完了を待たない）→ その時点でキャッシュ
 * 済みのチャンクだけを対象に破棄判定を行う（`evictFarChunks`）」という順で
 * 1回のスクロール処理内に実行される。2000万行規模のような大きなファイルでは
 * 1回のスクロールステップで多数の新規チャンクが必要になり得るため、複数
 * チャンクの非同期取得が次々と完了してキャッシュへ格納されるタイミングと、
 * 次のスクロールステップで `evictFarChunks` が呼ばれるタイミングの間に、
 * 一時的に保持行数・保持バイト数が上限を超える窓が生じ得る
 * （`docs/verification/stage0-results.md` 2.3.1節。241サンプル中18件で最大
 * +12.6% の一時超過を観測）。
 *
 * 対処方針の選択: 検討した2案のうち (b) を採用した。
 *   (a) 取得中（in-flight）チャンクの予定行数・概算バイト数を破棄予算へ
 *       前もって算入する方式。取得完了を待たずに超過を予測できる利点が
 *       ある一方、`raw_text` の実バイト数は応答が届くまで判明せず
 *       （`fetchChunk` 参照）、見積もり方法自体に新しい仕様判断
 *       （見積もり値をどう算出するか、実測値との差分をいつ・どう補正
 *       するか）を要し、実装も複雑になる。
 *   (b) チャンク取得が完了しキャッシュへ格納した直後に、その時点の実際の
 *       保持行数・保持バイト数で破棄判定（`evictFarChunks`）を再実行する
 *       方式。見積もりを一切使わず実測値だけで判定するため予測誤差が
 *       生じず、既存の `selectChunksToEvict`／`evictFarChunks` をそのまま
 *       追加で呼ぶだけで実装できる。
 * (b) は `computeVisibleRangeForScroll`・`computeRequiredChunkIndices`・
 * `selectChunksToEvict` という既存の軽量な純粋関数の呼び出しを1回増やす
 * だけであり、DOM 操作や IPC 呼び出しを伴わないためスクロール応答性への
 * 悪影響が無い。決定的（実測値のみに基づく）で単純なため (b) を採用した。
 */
function runPostFetchEvictionPass() {
  if (state.displaySetId === null || state.totalItems <= 0) {
    return;
  }
  const { visibleRange, requiredChunkIndices } = computeCurrentRenderTargets();
  evictFarChunks(requiredChunkIndices, visibleRange);
}

/**
 * 必要なチャンクのうち、未取得かつ取得中でないものの取得を開始する
 * （取得中チャンクの二重取得防止）。
 *
 * 直前に取得へ失敗したチャンクは、バックオフが明けるまで取り直さない
 * （Issue #34）。明ける時刻には `scheduleFailedChunkRetry` が再描画を予約する
 * ため、利用者が操作しなくても自動で取り直される。
 *
 * @param {number[]} chunkIndices
 */
function ensureChunksLoaded(chunkIndices) {
  if (state.displaySetId === null || state.generation === null) {
    return;
  }

  const nowMs = Date.now();
  /** @type {number | null} 再試行待ちのうち最も早い時刻。 */
  let earliestRetryAtMs = null;

  for (const chunkIndex of chunkIndices) {
    if (state.chunkCache.has(chunkIndex)) {
      continue;
    }
    if (state.inFlightChunkFetches.has(chunkIndex)) {
      continue;
    }
    const failure = state.failedChunkFetches.get(chunkIndex);
    if (failure !== undefined && nowMs < failure.retryAtMs) {
      earliestRetryAtMs =
        earliestRetryAtMs === null
          ? failure.retryAtMs
          : Math.min(earliestRetryAtMs, failure.retryAtMs);
      continue;
    }

    /** @type {ChunkFetchRequest} */
    const request = {
      chunkIndex,
      displaySetId: state.displaySetId,
      generation: state.generation,
    };
    const fetchPromise = fetchChunk(request)
      .catch((error) => handleFetchChunkError(error, request))
      .finally(() => {
        // 文脈が変わり、同じチャンク番号の取得が既に登録し直されている場合に
        // そちらを巻き添えで消さないよう、自分の登録だけを取り消す。
        if (state.inFlightChunkFetches.get(chunkIndex) === fetchPromise) {
          state.inFlightChunkFetches.delete(chunkIndex);
        }
        // 登録を外した**後**に再描画を予約する（順序依存。先に予約すると、その
        // 描画はこのチャンクをまだ「取得中」と見なして取り直さない）。文脈違いで
        // 応答を捨てた場合に、現在の文脈で取得をやり直す契機はこれ一つ
        // （Issue #34）。多重スケジュールは `scheduleRender` が防ぐ。
        scheduleRender();
      });
    state.inFlightChunkFetches.set(chunkIndex, fetchPromise);
  }

  scheduleFailedChunkRetry(earliestRetryAtMs, nowMs);
}

/**
 * 失敗したチャンクのバックオフが明けた時点で再描画（＝自動再取得）が起きるよう、
 * タイマーを1本だけ張り直す（Issue #34）。
 *
 * 描画はスクロールなどの契機がなければ起こらないため、これが無いと失敗した行は
 * 利用者が操作するまで失敗表示のまま残る。予約先は常に絶対時刻（`retryAtMs`）
 * であり、描画のたびに張り直しても再試行が先送りされることはない。
 *
 * @param {number | null} retryAtMs 再試行待ちのうち最も早い時刻。待機中が無ければ null。
 * @param {number} nowMs
 */
function scheduleFailedChunkRetry(retryAtMs, nowMs) {
  if (state.retryTimerId !== null) {
    clearTimeout(state.retryTimerId);
    state.retryTimerId = null;
  }
  if (retryAtMs === null) {
    return;
  }
  state.retryTimerId = setTimeout(
    () => {
      state.retryTimerId = null;
      scheduleRender();
    },
    Math.max(0, retryAtMs - nowMs),
  );
}

/**
 * チャンク取得の失敗を記録し、失敗表示と自動再取得の契機を作る（Issue #34）。
 *
 * @param {number} chunkIndex
 * @returns {boolean} 一連の失敗（ストリーク）の1件目か。通知はこの場合だけ出す。
 */
function recordChunkFetchFailure(chunkIndex) {
  // 通知をストリークの1件目に限る理由: 可視範囲の複数チャンクは同時に失敗し、
  // さらに自動再取得も失敗を繰り返すため、失敗のたびに通知すると同じ文の回数表示
  // （`src/banner.js` の集約）が際限なく伸び続ける。全て復旧すればストリークは
  // 終わり、次の失敗はまた1件目として通知される。
  const isFirstOfStreak = state.failedChunkFetches.size === 0;
  const attempts = (state.failedChunkFetches.get(chunkIndex)?.attempts ?? 0) + 1;
  const delayMs = Math.min(
    FETCH_RETRY_BASE_DELAY_MS * 2 ** (attempts - 1),
    FETCH_RETRY_MAX_DELAY_MS,
  );
  state.failedChunkFetches.set(chunkIndex, { attempts, retryAtMs: Date.now() + delayMs });
  // 失敗した行を「（読み込み中…）」のまま放置せず、すぐ失敗表示へ切り替える
  // （この再描画が `scheduleFailedChunkRetry` の予約も張り直す）。
  scheduleRender();
  return isFirstOfStreak;
}

/**
 * 指定した行のチャンクが取得に失敗し、再試行待ちかどうか（Issue #34）。
 * 行の本文が未取得である理由が「まだ届いていない」のか「失敗して待っている」の
 * かを、`buildRowElement` が文言で区別するために使う。
 *
 * @param {number} rowIndex
 * @returns {boolean}
 */
function hasFailedChunkFetch(rowIndex) {
  return state.failedChunkFetches.has(chunkIndexForRow(rowIndex, CHUNK_SIZE));
}

/**
 * 1チャンク分の範囲を取得し、キャッシュへ格納する。
 *
 * `fetch_log_range` は1回の応答を項目数（512件）とバイト数（2 MiB）の上限で
 * 打ち切ることがある（`truncated: true`）。チャンクの論理範囲を1回で埋め
 * きれない場合に備え、`truncated` が立っている間は続きを取得し続ける。
 * Rust 側は「要求開始位置が総項目数未満なら少なくとも1件は返す」ことを
 * 保証しているため、0件の応答は「これ以上データが無い」ことを意味し、ここで
 * 打ち切ってよい。
 *
 * 応答を適用する条件は2つ（Issue #34。モジュール冒頭のコメント「範囲取得応答の
 * 適用条件」参照）。往復のたびに両方を確認する。
 *   1. 要求時の文脈（表示集合 ID・世代）が現在の表示と一致すること。一致しない
 *      応答は、集めた分ごと捨てる
 *   2. 応答が契約どおりであること（`findRangeResponseViolation`）。違反した応答は
 *      成功として扱わず、取得失敗と同じ経路へ倒す
 *
 * @param {ChunkFetchRequest} request
 */
async function fetchChunk(request) {
  const { chunkIndex } = request;
  const { start: chunkStart, count: desiredCount } = computeChunkRange(
    chunkIndex,
    CHUNK_SIZE,
    state.totalItems,
  );
  if (desiredCount <= 0) {
    return;
  }

  const wasCachedBefore = state.everCachedChunkIndices.has(chunkIndex);

  /** @type {LogItemDto[]} */
  const collected = [];
  let cursor = chunkStart;
  const targetEnd = chunkStart + desiredCount;

  while (cursor < targetEnd) {
    retentionStats.recordFetchStart();
    let response;
    try {
      response = await invokeFetchLogRange({
        displaySetId: request.displaySetId,
        expectedGeneration: request.generation,
        start: cursor,
        maxItems: targetEnd - cursor,
      });
    } finally {
      retentionStats.recordFetchEnd();
    }

    // 往復の最中に表示集合が切り替わった（別のファイルを開いた・タブを切り替え
    // た）、または世代が進んだ（再読み込み・世代不一致からの復旧）場合、この応答
    // はもう画面に無い内容を指している。集めた分ごと捨てる（Issue #34）。
    // 現在の文脈での取得は、この取得が決着した後の再描画（`ensureChunksLoaded`
    // の `finally`）が改めて発行するため、取りこぼしにはならない。
    if (!isRequestContextCurrent(request)) {
      return;
    }

    const violation = findRangeResponseViolation(response, request, cursor);
    if (violation !== null) {
      throw new Error(`範囲取得の応答が契約に反しています（${violation}）。`);
    }

    syncTotalItemsFromResponse(response);

    if (response.items.length === 0) {
      if (cursor < state.totalItems) {
        // 「要求開始位置が総項目数未満なら少なくとも1件は返す」という契約に反する
        // （直前の同期で総行数が縮んで cursor がその外へ出た場合は、0件が正しい）。
        // このまま黙って戻ると、チャンクは未取得のまま失敗記録も残らないため、
        // 取得完了時の再描画が同じ要求を延々と出し続けるホットループになる。
        // 失敗として扱い、バックオフと失敗表示に乗せる（Issue #34）。
        throw new Error(
          `範囲取得の応答が契約に反しています（開始位置 ${cursor} は総行数 ${state.totalItems} 未満なのに0件です）。`,
        );
      }
      break;
    }
    collected.push(...response.items);
    // `response.start === cursor` は検証済み（`findRangeResponseViolation`）。
    cursor += response.items.length;

    if (!response.truncated) {
      break;
    }
  }

  if (collected.length === 0) {
    return;
  }

  const byteCount = sumRawTextBytes(collected);
  state.chunkCache.set(chunkIndex, { start: chunkStart, items: collected, byteCount });
  state.everCachedChunkIndices.add(chunkIndex);
  // 取得できたので、失敗表示とバックオフの根拠も畳む（次に失敗したときは
  // 1回目からやり直す。Issue #34）。
  state.failedChunkFetches.delete(chunkIndex);
  retentionStats.recordChunkCached(collected.length, byteCount);
  if (wasCachedBefore) {
    // このチャンク番号は以前にもキャッシュされていた（＝破棄後の再取得）。
    retentionStats.recordRefetch();
  }

  // 取得完了直後にも破棄判定を行う（`runPostFetchEvictionPass`
  // の JSDoc 参照）。
  runPostFetchEvictionPass();

  scheduleRender();
}

/**
 * 範囲取得応答が契約どおりかを判定する（Issue #34）。契約の正本は
 * `src-tauri/src/log_view.rs` の `fetch_log_range`（世代が一致する場合だけ成功を
 * 返し、応答の `start` は要求した開始位置と一致する）。
 *
 * 検査するのは、違反したまま表示へ流すと**無言で誤った本文を見せてしまう**項目
 * だけ。`start` がずれた応答をそのまま `chunkStart` として格納すると、行番号と
 * 本文が1行ずれた表示になり、利用者にはそれと分からない。世代のずれも同様に、
 * 別世代の本文を現在の世代の内容として並べてしまう。
 *
 * @param {{ generation: number, start: number, items: LogItemDto[] }} response
 * @param {ChunkFetchRequest} request
 * @param {number} expectedStart この往復で要求した開始位置。
 * @returns {string | null} 違反の説明。契約どおりなら `null`。
 */
function findRangeResponseViolation(response, request, expectedStart) {
  if (!response || !Array.isArray(response.items)) {
    return "items が配列ではありません";
  }
  if (Number(response.generation) !== request.generation) {
    return `世代が要求と異なります（要求 ${request.generation}、応答 ${response.generation}）`;
  }
  if (Number(response.start) !== expectedStart) {
    return `開始位置が要求と異なります（要求 ${expectedStart}、応答 ${response.start}）`;
  }
  return null;
}

/**
 * `fetch_log_range` の応答から、表示集合の総行数を最新へ同期する（読み込み中の
 * 伸長に追随するため。契約に織り込む4点の4）。
 *
 * **世代はここで代入しない**（Issue #34）。成功応答は要求した世代と同じ世代の
 * 内容しか返さない（世代が一致しなければ `generation_mismatch` になる。
 * `src-tauri/src/log_view.rs`）ため、応答から世代を代入しても現在値と同じか、
 * 遅れて届いた古い応答による巻き戻しにしかならない。世代を進めるのは表示集合の
 * 切り替え（`activateDisplaySet`）と世代不一致からの復旧
 * （`handleGenerationMismatch`）だけに限る。
 *
 * 呼び出し側は、要求時の文脈が現在の表示と一致することを確認済みであること
 * （`isRequestContextCurrent`）。同じ世代の中で総行数が変わるのは読み込みの進行
 * による伸長であり、この同期は巻き戻しにならない。
 *
 * @param {{ total_items: number }} response
 */
function syncTotalItemsFromResponse(response) {
  const totalItems = Number(response.total_items);
  if (totalItems !== state.totalItems) {
    state.totalItems = totalItems;
    updateTotalItemsLabel();
  }
}

/**
 * チャンク取得の失敗を処理する。`generation_mismatch` は世代の再取得フロー
 * （契約に織り込む4点の4）、それ以外は失敗として記録し（自動再取得と失敗表示。
 * Issue #34）、ストリークの1件目だけエラーバナーを出す。
 *
 * 失敗も、要求を発行した時点の文脈に束ねて扱う（Issue #34）。往復の最中にタブを
 * 切り替えた・再読み込みした場合、失敗したのは今表示している内容ではないため、
 * 現在の表示のキャッシュ・世代・通知には一切触れない。触れると、今のタブが毎回
 * 世代不一致になって余計な往復が続いたり、今のタブとは無関係なバナーが出たりする。
 *
 * @param {unknown} error
 * @param {ChunkFetchRequest} request
 */
function handleFetchChunkError(error, request) {
  if (!isRequestContextCurrent(request)) {
    // 発生元の表示集合・世代は既に画面上に無い。原因調査のためコンソールへは
    // 残しつつ、現在の表示へは何も反映しない（バナーも出さない）。
    console.warn(
      "既に切り替わった表示集合・世代の範囲取得が失敗しました（現在の表示へは反映しません）:",
      error,
    );
    return;
  }

  if (error && typeof error === "object" && "kind" in error) {
    if (error.kind === "generation_mismatch") {
      // 自己修復の経路。失敗として記録せず（バックオフを挟むと復旧が遅れる）、
      // 世代を進めた直後の再描画が現在の世代で取得し直す。
      handleGenerationMismatch(error.current, request);
      return;
    }
    if (error.kind === "unknown_display_set") {
      if (recordChunkFetchFailure(request.chunkIndex)) {
        showErrorBanner(
          "表示中のログの内部状態が見つからないため、表示を更新できません。もう一度ファイルを開き直してください。",
        );
      }
      return;
    }
  }
  console.error("範囲取得に失敗しました:", error);
  if (recordChunkFetchFailure(request.chunkIndex)) {
    showErrorBanner("ログの範囲取得に失敗しました。もう一度お試しください。");
  }
}

/**
 * 世代不一致を検出した際の復旧処理（契約に織り込む4点の4。`LOG-023`・
 * `LOG-028` の下地）。表示集合の再構築コマンドは無いため、`fetch_log_range`
 * の `generation_mismatch` 応答が返す `current` 値をまず反映し、キャッシュを
 * 全破棄したうえで、直後の再描画がトリガーする再取得の応答で総数
 * （`total_items`）を確定させる（`syncTotalItemsFromResponse`）。
 *
 * `context` は不一致を検出した要求を発行した時点の表示文脈。これが現在の表示と
 * 一致する場合にだけ復旧を行う（Issue #34）。一致を確認せずに `current` を代入
 * すると、別のタブ（表示集合）で起きた不一致の世代を今のタブへ書き込んでしまい、
 * 以後そのタブは毎回世代不一致になって余分な往復を繰り返す。要求時の文脈が現在と
 * ずれている場合には「既に他の取得が復旧済み」も含まれる（その場合も何もしない）。
 *
 * @param {number} currentGeneration
 * @param {DisplayContext} context
 */
function handleGenerationMismatch(currentGeneration, context) {
  if (!isRequestContextCurrent(context)) {
    return;
  }
  if (state.generation === currentGeneration) {
    // 要求時と現在の世代が同じなのに不一致が返る場合（契約違反）。破棄も再取得も
    // 意味が無いため何もしない。
    return;
  }
  clearCache();
  // 旧世代へ向けた取得の登録と、旧世代に対する失敗の記録は持ち越さない。
  forgetInFlightChunkFetches();
  clearFailedChunkFetches();
  state.generation = currentGeneration;
  scheduleRender();
}

/**
 * 保持上限を超えている場合、表示範囲から遠いチャンクから破棄する
 * （`PERF-012`）。
 *
 * @param {number[]} requiredChunkIndices 現在の可視範囲＋バッファがカバーするチャンク（破棄対象から除外）。
 * @param {import("./virtual_scroll.js").VisibleRange} visibleRange
 */
function evictFarChunks(requiredChunkIndices, visibleRange) {
  const descriptors = [];
  for (const [chunkIndex, chunk] of state.chunkCache) {
    descriptors.push({
      chunkIndex,
      rowCount: chunk.items.length,
      byteCount: chunk.byteCount,
    });
  }

  const referenceRowIndex = Math.floor(
    (visibleRange.startIndex + visibleRange.endIndex) / 2,
  );
  const toEvict = selectChunksToEvict(
    descriptors,
    new Set(requiredChunkIndices),
    referenceRowIndex,
    CHUNK_SIZE,
    state.retentionLimits,
  );

  for (const chunkIndex of toEvict) {
    const chunk = state.chunkCache.get(chunkIndex);
    if (!chunk) {
      continue;
    }
    state.chunkCache.delete(chunkIndex);
    retentionStats.recordChunkEvicted(chunk.items.length, chunk.byteCount);
  }
}

/**
 * 指定した行インデックスの項目を、現在のキャッシュから探す。チャンクが
 * 未取得の場合は `null`（呼び出し側はプレースホルダーを描画する）。
 *
 * @param {number} rowIndex
 * @returns {LogItemDto | null}
 */
function lookupItem(rowIndex) {
  const chunkIndex = chunkIndexForRow(rowIndex, CHUNK_SIZE);
  const chunk = state.chunkCache.get(chunkIndex);
  if (!chunk) {
    return null;
  }
  const offset = rowIndex - chunk.start;
  if (offset < 0 || offset >= chunk.items.length) {
    return null;
  }
  return chunk.items[offset];
}

/**
 * 可視範囲の行 DOM を作り直す。既存の行要素は毎回すべて破棄してから作り直す
 * （行ごとのイベントリスナーを一切追加しない設計のため、破棄は
 * `textContent = ""` だけで安全に行える。使い回しによる状態の取り違えも
 * 起きない）。
 *
 * @param {import("./virtual_scroll.js").VisibleRange} visibleRange
 */
function renderRows(visibleRange) {
  const { rows, topSpacer, bottomSpacer } = elements;

  const previousRowCount = rows.children.length;
  rows.textContent = "";
  retentionStats.recordRowNodesRemoved(previousRowCount);

  const fragment = document.createDocumentFragment();
  let createdCount = 0;
  for (
    let rowIndex = visibleRange.startIndex;
    rowIndex < visibleRange.endIndex;
    rowIndex += 1
  ) {
    fragment.appendChild(buildRowElement(rowIndex));
    createdCount += 1;
  }
  rows.appendChild(fragment);
  retentionStats.recordRowNodesCreated(createdCount);

  const { topHeightPx, bottomHeightPx } = computeSpacerHeightsForScroll({
    startIndex: visibleRange.startIndex,
    endIndex: visibleRange.endIndex,
    totalItems: state.totalItems,
    rowHeightPx: ROW_HEIGHT_PX,
  });
  topSpacer.style.height = `${topHeightPx}px`;
  bottomSpacer.style.height = `${bottomHeightPx}px`;
}

/**
 * 1行分の DOM 要素を作る。行番号・（統合表示のときだけ読み込み元ラベル）・
 * （未確定／継続行バッジ）・原文の列。
 *
 * Issue #78: かつては行番号と原文の間に解析済みの日時列を置いていたが、
 * 原文の先頭には元々日時が含まれており、画面上で同じ日時が2回並んで見えて
 * いたため廃止した。解析済み日時（`LOG-024` の元精度を保持した表示文字列）を
 * 利用者へ提示する経路は、ツールバーのコピー列「日時」（ADR-0009）に一本化
 * してある。
 *
 * 行ごとのイベントリスナーは追加しない（PERF-012 の累積源になるため。
 * 禁止事項）。継続行バッジは `<button>` 要素だが、リスナーは付けず
 * `elements.rows` の委譲クリックハンドラー（`handleRowsClick`）に拾わせる
 * （モジュール冒頭のコメント「新しい単一購読」参照）。
 *
 * 列の間に半角スペースのテキストノードを挟み、画面上で列同士が詰まって
 * 見えないようにする（CSS の余白だけでなく実テキストとして挟む）。P10
 * 以降、コピー内容はこの DOM から生成するのではなく、
 * `hakutaku_core::assemble_copy` が選択インデックス範囲から本文を
 * 読み直して組み立てる（`copy_selection` コマンド）ため、この空白文字は
 * コピー結果には一切影響しない。
 *
 * @param {number} rowIndex
 * @returns {HTMLDivElement}
 */
function buildRowElement(rowIndex) {
  const row = document.createElement("div");
  row.className = "log-row";
  // 行選択のクリック委譲（handleRowsClick）が行インデックスを特定するための
  // 添字。行データそのもの（item への参照）は持たせない（PERF-012）。
  // 表示外の行でも rowIndex 自体は確定しているため、item が未取得
  // （プレースホルダー表示）でも選択できる。
  row.dataset.rowIndex = String(rowIndex);

  const item = lookupItem(rowIndex);

  if (item && item.raw_display) {
    // LOG-022: 日時未解析の生データ行。行全体の見た目をわずかに変える
    // （CSS の .log-row--raw）。日時列を廃止した Issue #78 以降、一覧上で
    // この行と通常行を見分ける手掛かりはこの背景色だけである。
    row.classList.add("log-row--raw");
  }
  if (item && !item.confirmed) {
    // LOG-026: 未確定行（書き込み途中の可能性）。解析エラーとは異なる中立的な
    // 見た目にする（CSS の .log-row--unconfirmed。エラー表示の赤系・警告表示の
    // 橙系とは別系統の色）。
    row.classList.add("log-row--unconfirmed");
  }
  if (isRowSelected(state.selection, rowIndex)) {
    // P10（COPY-001）: 選択中の行をハイライトする。
    row.classList.add("log-row--selected");
  }

  const lineNumber = document.createElement("span");
  lineNumber.className = "log-row__lineno";
  lineNumber.textContent = item ? String(item.source_line_number) : "";
  row.appendChild(lineNumber);
  row.appendChild(document.createTextNode(" "));

  if (state.isMerged) {
    // P09-1（LOG-007）: 統合表示では、どのファイル由来かを行ごとに識別
    // できるよう読み込み元ラベル列を出す。個別ファイルのタブでは全行が
    // 同じファイルのため表示しない。
    const source = document.createElement("span");
    source.className = "log-row__source";
    source.textContent = item ? item.source_label : "";
    row.appendChild(source);
    row.appendChild(document.createTextNode(" "));
  }

  if (item && !item.confirmed) {
    // 未確定行の見せ方（記号・色・別欄など）は設計判断であり、
    // tasks/phase-08-log-view.md「人間判断待ち」が示すとおり利用者の確認が
    // 望ましい項目である。ここでは暫定案として、中立色のバッジ＋ツールチップ
    // を採用した。見せ方の最終確認は利用者確認で行う想定。
    const unconfirmedBadge = document.createElement("span");
    unconfirmedBadge.className = "log-row__badge log-row__badge--unconfirmed";
    unconfirmedBadge.textContent = "未確定";
    unconfirmedBadge.title =
      "ログファイルへの書き込みが完了していない可能性がある末尾の行です（LOG-026）。解析エラーではありません。";
    row.appendChild(unconfirmedBadge);
    row.appendChild(document.createTextNode(" "));
  }

  if (item && item.continuation_count > 0) {
    const continuationBadge = document.createElement("button");
    continuationBadge.type = "button";
    continuationBadge.className = "log-row__badge log-row__badge--continuation";
    continuationBadge.textContent = `+${item.continuation_count}行`;
    continuationBadge.title =
      "継続行を含む項目です。クリックすると改行を保ったまま全文を下部の詳細パネルに表示します（LOG-014）。";
    // クリック委譲（handleRowsClick）が行を特定するための添字。行データそのもの
    // （item への参照）は持たせない（PERF-012: DOM に行データを保持しない）。
    continuationBadge.dataset.rowIndex = String(rowIndex);
    row.appendChild(continuationBadge);
    row.appendChild(document.createTextNode(" "));
  }

  const text = document.createElement("span");
  text.className = "log-row__text";
  if (!item) {
    // Issue #34: 未取得の理由が「まだ届いていない」のか「取得に失敗して再試行を
    // 待っている」のかを、利用者が文言で見分けられるようにする（失敗した行を
    // 「（読み込み中…）」のまま放置しない）。
    text.textContent = hasFailedChunkFetch(rowIndex)
      ? "（取得に失敗しました。自動で再試行します）"
      : "（読み込み中…）";
  } else if (item.continuation_count > 0) {
    // 折りたたみ方式（virtual_scroll.js 冒頭のコメント参照）: 行一覧には1行目
    // だけを表示する。全文（改行付き）は詳細パネルで確認できる。
    text.textContent = extractFirstLine(item.raw_text);
  } else {
    text.textContent = item.raw_text;
  }
  row.appendChild(text);

  return row;
}

/**
 * `elements.rows` へ1つだけ登録するクリックの委譲ハンドラー（PERF-012:
 * 行ごとのイベントリスナーを追加しない設計。モジュール冒頭のコメント
 * 「新しい単一購読」参照）。継続行バッジ（`.log-row__badge--continuation`）の
 * クリックは詳細パネルを開き、それ以外の行クリックは行選択（P10、
 * COPY-001）を更新する。
 *
 * @param {MouseEvent} event
 */
function handleRowsClick(event) {
  const target = /** @type {HTMLElement} */ (event.target);

  const badge = target.closest(".log-row__badge--continuation");
  if (badge) {
    const rowIndex = Number(/** @type {HTMLElement} */ (badge).dataset.rowIndex);
    if (!Number.isInteger(rowIndex)) {
      return;
    }
    const item = lookupItem(rowIndex);
    if (!item) {
      // 描画とクリックの間にチャンクが破棄された場合の防御（通常は起こらない。
      // バッジは可視範囲＋バッファの行にしか存在せず、その範囲のチャンクは
      // evictFarChunks の保護対象のため、表示中のバッジのクリック時点では
      // 必ずキャッシュに残っているはず）。
      return;
    }
    showDetailPanel(item);
    return;
  }

  const row = target.closest(".log-row");
  if (!row) {
    return;
  }
  const rowIndex = Number(/** @type {HTMLElement} */ (row).dataset.rowIndex);
  if (!Number.isInteger(rowIndex)) {
    return;
  }
  // P10（COPY-001）: クリックで単一行選択、Shift+クリックで範囲選択。行の
  // 内容（item）が未取得でも、インデックスさえ分かれば選択できる
  // （PERF-012: 選択はインデックスだけを保持し、表示外の選択と両立する）。
  state.selection = event.shiftKey
    ? extendSelectionTo(state.selection, rowIndex)
    : selectSingleRow(rowIndex);
  // Ctrl+A／Ctrl+C がクリック直後も効くよう、キーボードフォーカスを
  // ビューポートへ移す（#log-viewport は tabindex="0" でフォーカス可能。
  // クリックした行 <div> 自体はフォーカス不可のため、ブラウザに任せると
  // フォーカスが移動しないことがある）。
  elements.viewport.focus();
  scheduleRender();
}

/**
 * 継続行を含む項目の全文（改行を保持）を下部の詳細パネルへ表示する
 * （`LOG-014`）。
 *
 * @param {LogItemDto} item
 */
function showDetailPanel(item) {
  const { detailPanel, detailPanelTitle, detailPanelBody } = elements;
  const totalLines = item.continuation_count + 1;
  detailPanelTitle.textContent =
    `行 ${item.source_line_number}` +
    (item.timestamp ? ` ・ ${item.timestamp}` : "") +
    `（継続行 ${item.continuation_count} 行を含む、全 ${totalLines} 行）`;
  // raw_text は継続行結合済みの本文を改行付きでそのまま保持している
  // （virtual_scroll.js 冒頭のコメント参照）。<pre> + CSS の white-space:
  // pre-wrap（styles.css）で改行を保ったまま表示する。
  detailPanelBody.textContent = item.raw_text;
  detailPanel.hidden = false;
}

/** 詳細パネルを閉じ、内容を空にする。 */
function hideDetailPanel() {
  elements.detailPanel.hidden = true;
  elements.detailPanelTitle.textContent = "";
  elements.detailPanelBody.textContent = "";
}

/**
 * ジャンプボタンのクリック・ジャンプ入力欄での Enter キー、共通のハンドラー
 * （`tasks/phase-08-log-view.md` 作業項目4）。
 */
function handleJumpRequest() {
  const rowIndex = parseJumpTargetRowIndex(elements.jumpInput.value, state.totalItems);
  if (rowIndex === null) {
    // 低優先の作業項目（Issue #48）: 空欄・数値でない・0以下など無効な入力を
    // 無反応のままにせず、入力欄自体に明示する。同時多発を避けたい他の通知
    // （Issue #11 の集約バナー）と違い、この誤りは入力欄1箇所に閉じているため
    // バナーは出さない（主セッション裁定）。次の入力（`handleJumpInputInput`）
    // で解除する。
    elements.jumpInput.setAttribute("aria-invalid", "true");
    return;
  }
  elements.jumpInput.removeAttribute("aria-invalid");
  // 丸めた結果を入力欄へ反映し、利用者が実際に移動した先を確認できるようにする。
  elements.jumpInput.value = String(rowIndex + 1);
  // クランプ済みの大規模ファイルでは行インデックス↔スクロール
  // 位置が1:1で対応しないため、比例写像の逆変換（`computeCurrentRenderTargets`
  // と同じく実際の DOM の scrollHeight/clientHeight から maxScrollTopPx を
  // 読む）を使う。クランプ未満の通常規模では内部で従来の1:1計算へ委譲される
  // （`computeScrollTopForRowIndexScaled` の JSDoc 参照）。
  const maxScrollTopPx = Math.max(
    0,
    elements.viewport.scrollHeight - elements.viewport.clientHeight,
  );
  elements.viewport.scrollTop = computeScrollTopForRowIndexScaled(
    rowIndex,
    ROW_HEIGHT_PX,
    state.totalItems,
    maxScrollTopPx,
  );
  // scrollTop の変更は 'scroll' イベント経由で scheduleRender を呼ぶ
  // （measurement.js の連続スクロール検証と同じ経路）。目標位置が現在位置と
  // 一致し 'scroll' イベントが発火しない場合に備え、念のため明示的にも
  // 再描画をスケジュールしておく（scheduleRender は多重スケジュールを
  // 防止済みのため、二重に呼んでも実害はない）。
  scheduleRender();
}

/**
 * ジャンプ入力欄で Enter キーが押された時のハンドラー。
 *
 * @param {KeyboardEvent} event
 */
function handleJumpInputKeydown(event) {
  if (event.key === "Enter") {
    event.preventDefault();
    handleJumpRequest();
  }
}

/**
 * ジャンプ入力欄の内容が変わるたびに呼ばれる（Issue #48）。無効な入力の明示
 * （`aria-invalid`）は、利用者が値を直そうとした時点（次の入力）で解除する。
 * 新しい値がまだ有効かどうかはここでは判定しない（判定は
 * `handleJumpRequest` が改めて行う。入力の途中経過を逐一 aria-invalid の
 * 有無へ反映すると、桁を打ち切る途中の一瞬だけ無効になる入力（例:
 * 「10」を打つ途中の「1」は総行数によっては範囲外）で明示がちらつくため）。
 */
function handleJumpInputInput() {
  elements.jumpInput.removeAttribute("aria-invalid");
}

/**
 * ビューポートの Ctrl+Home / Ctrl+End / Ctrl+A / Ctrl+C を明示的に処理する
 * （`tasks/phase-08-log-view.md` 作業項目4、P10 で Ctrl+A・
 * Ctrl+C を追加）。
 *
 * PageUp / PageDown、および修飾キー無しの Home / End はブラウザ既定の
 * スクロール動作に任せる。`#log-viewport` は `tabindex="0"`（`src/index.html`）
 * によりフォーカス可能なスクロール領域になっており、フォーカスを持つ状態で
 * これらのキーを押すと、Chromium 系ブラウザ（WebView2）は既定でその要素を
 * スクロールする。Ctrl+Home / Ctrl+End にはスクロール可能な `div` に対する
 * ブラウザ既定の割り当てが無いため、ここで明示的に実装する。
 *
 * Ctrl+A（全選択）・Ctrl+C（コピー）はブラウザ既定のページ全体選択／コピーを
 * 必ず `preventDefault` で抑止し、常に P10 の正式経路（`state.selection`・
 * `copy_selection` コマンド）を通す。
 *
 * scrollHeight クランプ対応との関係: Ctrl+Home（`scrollTop = 0`）
 * と Ctrl+End（`scrollTop = viewport.scrollHeight`。ブラウザが実際の最大値へ
 * 自動的にクランプする）は、`computeVisibleRangeForScroll` の比例写像でも
 * ちょうど比例0・比例1（＝先頭行・末尾到達）に一致する境界値そのものであり、
 * `computeScrollTopForRowIndexScaled` による逆変換を経由しなくても常に正確
 * （`handleJumpRequest` のような中間の行番号ジャンプだけが逆変換を要する）。
 *
 * @param {KeyboardEvent} event
 */
function handleViewportKeydown(event) {
  if (!event.ctrlKey) {
    return;
  }
  if (event.key === "Home") {
    event.preventDefault();
    elements.viewport.scrollTop = 0;
  } else if (event.key === "End") {
    event.preventDefault();
    elements.viewport.scrollTop = elements.viewport.scrollHeight;
  } else if (event.key === "a" || event.key === "A") {
    event.preventDefault();
    handleSelectAllRequest();
  } else if (event.key === "c" || event.key === "C") {
    event.preventDefault();
    handleCopyRequest();
  }
}

/** Ctrl+A: 表示集合全体を選択する（P10、COPY-001）。 */
function handleSelectAllRequest() {
  if (state.displaySetId === null || state.totalItems <= 0) {
    return;
  }
  state.selection = selectAll(state.totalItems);
  scheduleRender();
}

/**
 * コピー列チェックボックスの変更を `state.copyColumns` へ反映する
 * （P10、COPY-001）。
 */
function handleCopyColumnsChange() {
  state.copyColumns = {
    lineNumber: elements.copyColumnLineNumber.checked,
    timestamp: elements.copyColumnTimestamp.checked,
    rawText: elements.copyColumnRawText.checked,
  };
}

/** `state.copyColumns`（既定値）をチェックボックスの表示へ反映する。 */
function syncCopyColumnCheckboxesFromState() {
  elements.copyColumnLineNumber.checked = state.copyColumns.lineNumber;
  elements.copyColumnTimestamp.checked = state.copyColumns.timestamp;
  elements.copyColumnRawText.checked = state.copyColumns.rawText;
}

/**
 * Ctrl+C: 選択があれば `copy_selection` を呼び、成功・拒否を通知する
 * （P10、COPY-002／COPY-005／COPY-006）。選択が無ければ何もしない
 * （クリップボードを変更しない）。呼び出し中の多重実行（Ctrl+C 連打）は
 * `state.copyInFlight` で防ぐ。
 */
async function handleCopyRequest() {
  if (state.copyInFlight) {
    return;
  }
  if (state.displaySetId === null || state.generation === null) {
    return;
  }
  const range = getSelectionRange(state.selection, state.totalItems);
  if (!range) {
    // COPY-006／SEC-004: 選択が無ければクリップボードに一切触れない。
    return;
  }
  if (!hasAnyCopyColumn(state.copyColumns)) {
    showErrorBanner("コピーする列が選択されていません。ツールバーの「コピー列」で列を選んでください。");
    return;
  }

  // 範囲取得と同じく、要求を発行した時点の文脈を束ねる（Issue #34）。コピーの
  // 往復中にタブを切り替えられても、失敗の処理を別のタブの状態へ適用しない。
  /** @type {DisplayContext} */
  const context = { displaySetId: state.displaySetId, generation: state.generation };

  state.copyInFlight = true;
  try {
    const response = await invokeCopySelection({
      displaySetId: context.displaySetId,
      generation: context.generation,
      start: range.start,
      count: range.count,
      columns: state.copyColumns,
    });

    if ("copied" in response) {
      showInfoBanner(
        `${response.copied.lines.toLocaleString("ja-JP")} 行（${formatByteCount(
          response.copied.bytes,
        )}）をクリップボードへコピーしました。`,
      );
    } else if ("rejected" in response) {
      showErrorBanner(formatCopyRejectionMessage(response.rejected));
    }
  } catch (error) {
    handleCopySelectionError(error, context);
  } finally {
    state.copyInFlight = false;
  }
}

/**
 * `copy_selection` の失敗（Rust 側の `Result::Err`）を処理する。
 *
 * `context` は要求を発行した時点の表示文脈。`handleGenerationMismatch` へ渡し、
 * 別の表示集合・別の世代の `current` を今のタブへ代入しないようにする
 * （Issue #34）。通知そのものは文脈がずれていても出す。コピーは利用者の明示的な
 * 操作（Ctrl+C）であり、結果を黙って捨てると「コピーできたのか」が分からなく
 * なるため（範囲取得の失敗が背景処理であるのとは事情が異なる）。
 *
 * @param {unknown} error
 * @param {DisplayContext} context
 */
function handleCopySelectionError(error, context) {
  if (error && typeof error === "object" && "kind" in error) {
    switch (error.kind) {
      case "generation_mismatch":
        handleGenerationMismatch(error.current, context);
        showErrorBanner(
          "表示内容が更新されたため、コピーできませんでした。選択し直してもう一度お試しください。",
        );
        return;
      case "unknown_display_set":
        // 時系列統合表示（P09-1）の表示集合もコピーできるようになったため
        // （Issue #37）、この種別は「表示していた集合が既に無い」場合だけを
        // 指す（対象を閉じた、統合表示を OFF にした等）。`ERR-002` の「次の
        // 操作」として、両方の復帰手段を示す。
        showErrorBanner(
          "表示していたログの内部状態が既に無いため、コピーできませんでした。" +
            "対象を開き直すか、時系列統合表示を入れ直してからもう一度お試しください。",
        );
        return;
      case "source_unavailable":
        // COPY-005（Issue #37）: 本文を読み出せない項目が選択範囲に含まれて
        // いたため、Rust 側がコピー全体を中止した（空の本文を混ぜたまま
        // クリップボードへ渡さない）。クリップボードは変更されていない。
        showErrorBanner(
          "コピー対象のファイルを読み取れなくなったため、コピーを中止しました" +
            "（削除・置換・他ソフトウェアによる占有の可能性があります）。" +
            "クリップボードは変更していません。左の一覧で対象の状態を確認し、" +
            "必要なら再読み込みしてからもう一度お試しください。",
        );
        return;
      case "no_columns_selected":
        showErrorBanner("コピーする列が選択されていません。ツールバーの「コピー列」で列を選んでください。");
        return;
      case "memory_reservation_rejected":
        showErrorBanner(`メモリ不足のためコピーできませんでした（${error.reason}）。`);
        return;
      case "clipboard_write_failed":
        showErrorBanner(`クリップボードへの書き込みに失敗しました（${error.reason}）。`);
        return;
      default:
        break;
    }
  }
  console.error("copy_selection の呼び出しに失敗しました:", error);
  showErrorBanner("コピー処理でエラーが発生しました。");
}

/**
 * バイト数を読みやすい単位（KB／MB）の日本語表示へ整形する
 * （`src/shell.js` の `formatBytes` と同じ考え方の簡略版。コピー上限は
 * 既定16 MiBであり GB 単位は通常発生しないため、KB／MB までで十分）。
 *
 * @param {number} bytes
 * @returns {string}
 */
function formatByteCount(bytes) {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(1)} ${units[unitIndex]}`;
}

/**
 * 上限超過による拒否（`COPY-005`）の通知文を組み立てる。上限値と選択量
 * （行数・判明しているバイト数）を必ず含める（作業指示「拒否時の表示」）。
 *
 * @param {{ limit_bytes: number, limit_lines: number, selected_lines: number, selected_bytes?: number }} rejected
 * @returns {string}
 */
function formatCopyRejectionMessage(rejected) {
  const limitLines = rejected.limit_lines.toLocaleString("ja-JP");
  const limitBytes = formatByteCount(rejected.limit_bytes);
  const selectedLines = rejected.selected_lines.toLocaleString("ja-JP");
  const selectedBytesPart =
    rejected.selected_bytes != null
      ? `、判明しているバイト数 ${formatByteCount(rejected.selected_bytes)}`
      : "";
  return (
    `選択範囲が上限を超えているため、コピーを中止しました` +
    `（上限 ${limitLines} 行 / ${limitBytes}、選択 ${selectedLines} 行${selectedBytesPart}）。` +
    `選択範囲を絞り込んでください。`
  );
}

/**
 * @typedef {Object} DisplaySetDescriptor `activate` へ渡す表示集合の記述子
 * （`open_log_file` 等の `opened` 応答と同じ形。バックエンドの DTO に合わせ
 * フィールド名は snake_case のまま扱う）。
 * @property {number} display_set_id
 * @property {number} generation
 * @property {number} total_items
 * @property {string} source_label
 * @property {boolean} [is_merged] 時系列統合表示（P09-1）かどうか。
 *   省略時は false 扱い。true の間だけ行ごとの読み込み元ラベル列（LOG-007）を
 *   表示する。
 */

/**
 * @typedef {Object} FormatViewer 共通シェル（src/shell.js）が形式別ビューアを
 * 差し込むための最小契約（`9.2` の責務分離の「ビュー」に対応）。現時点では
 * このモジュール（テキストログビューア）が唯一の実装。
 * @property {(descriptor: DisplaySetDescriptor) => void} activate 指定した表示集合をビュー領域へ表示する。
 * @property {() => void} showEmpty ビュー領域を「何も選択されていない」状態にする。
 */

/** @type {FormatViewer} */
export const logViewer = {
  activate: activateDisplaySet,
  showEmpty: showEmptyState,
};
