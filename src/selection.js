// 選択モデルの純粋関数（P10、COPY-001）。
//
// 行選択は「アンカー〜フォーカス」の連続したインデックス範囲として保持する。
// クリックで単一行、Shift+クリックで範囲、Ctrl+A で全行を選ぶ（表示集合
// 全体のインデックス範囲であり、DOM にも本文にも触れない）。
//
// PERF-012「取得済みの行を累積しない」に合わせ、選択はインデックス範囲
// （数値2つ）だけを保持し、行の本文（raw_text 等）は一切保持しない。仮想
// スクロールで表示されていない行（未取得のチャンク）も、インデックスさえ
// 分かれば選択でき、全選択（Ctrl+A）が仮想スクロールと両立する。
//
// DOM にも IPC にも触れない純粋関数のみを置く（ADR-0006、AGENTS.md の指示）。

/**
 * @typedef {Object} SelectionState
 * @property {number | null} anchorIndex 範囲選択の起点（Shift+クリックの基準）。
 * @property {number | null} focusIndex 範囲選択の終点（直近で操作した行）。
 */

/** @returns {SelectionState} 選択なしの初期状態。 */
export function createSelectionState() {
  return { anchorIndex: null, focusIndex: null };
}

/**
 * 単一行を選択する（クリック、Shift 無し）。
 *
 * @param {number} rowIndex
 * @returns {SelectionState}
 */
export function selectSingleRow(rowIndex) {
  return { anchorIndex: rowIndex, focusIndex: rowIndex };
}

/**
 * 直前の選択（アンカー）から `rowIndex` までへ範囲を広げる（Shift+クリック）。
 * アンカーが無い場合は単一行選択と同じ扱いにする。
 *
 * @param {SelectionState} state
 * @param {number} rowIndex
 * @returns {SelectionState}
 */
export function extendSelectionTo(state, rowIndex) {
  if (state.anchorIndex === null) {
    return selectSingleRow(rowIndex);
  }
  return { anchorIndex: state.anchorIndex, focusIndex: rowIndex };
}

/**
 * 表示集合全体を選択する（Ctrl+A）。`totalItems` が0以下の場合は選択なしの
 * ままにする。
 *
 * @param {number} totalItems
 * @returns {SelectionState}
 */
export function selectAll(totalItems) {
  if (totalItems <= 0) {
    return createSelectionState();
  }
  return { anchorIndex: 0, focusIndex: totalItems - 1 };
}

/** @returns {SelectionState} 選択なしの状態。 */
export function clearSelection() {
  return createSelectionState();
}

/** @param {SelectionState} state
 *  @returns {boolean} 選択が無いか。 */
export function isSelectionEmpty(state) {
  return state.anchorIndex === null || state.focusIndex === null;
}

/**
 * 現在の選択を、コピー等で使う `{ start, count }` 範囲（0起点、`start` を
 * 含む半開区間の長さが `count`）へ変換する。`totalItems` で範囲外をクランプ
 * する（表示集合が縮んだ場合の防御）。選択が無い、またはクランプの結果
 * 範囲が空になった場合は `null`。
 *
 * @param {SelectionState} state
 * @param {number} totalItems
 * @returns {{ start: number, count: number } | null}
 */
export function getSelectionRange(state, totalItems) {
  if (isSelectionEmpty(state) || totalItems <= 0) {
    return null;
  }
  const low = Math.max(0, Math.min(state.anchorIndex, state.focusIndex));
  const high = Math.min(totalItems - 1, Math.max(state.anchorIndex, state.focusIndex));
  if (high < low) {
    return null;
  }
  return { start: low, count: high - low + 1 };
}

/**
 * `rowIndex` が現在の選択範囲に含まれるか（行のハイライト表示に使う）。
 *
 * @param {SelectionState} state
 * @param {number} rowIndex
 * @returns {boolean}
 */
export function isRowSelected(state, rowIndex) {
  if (isSelectionEmpty(state)) {
    return false;
  }
  const low = Math.min(state.anchorIndex, state.focusIndex);
  const high = Math.max(state.anchorIndex, state.focusIndex);
  return rowIndex >= low && rowIndex <= high;
}

/**
 * @typedef {Object} CopyColumns コピーする列の組（`hakutaku_core::CopyColumns`
 * の JS 表現。camelCase）。
 * @property {boolean} lineNumber
 * @property {boolean} timestamp
 * @property {boolean} rawText
 */

/**
 * 既定のコピー列（本文のみ）。ADR-0009「行（論理ログ項目）選択 = 原文
 * そのまま」に対応する既定値で、複数セル選択（quoted TSV）は利用者が
 * ツールバーの列トグルで明示的に列を追加した場合だけ有効になる。
 *
 * @returns {CopyColumns}
 */
export function defaultCopyColumns() {
  return { lineNumber: false, timestamp: false, rawText: true };
}

/**
 * 列の組のうち、少なくとも1列が選択されているか（コピー実行前の防御的
 * チェック用。Rust 側 `hakutaku_core::CopyColumns::any_selected` と同じ規則）。
 *
 * @param {CopyColumns} columns
 * @returns {boolean}
 */
export function hasAnyCopyColumn(columns) {
  return columns.lineNumber || columns.timestamp || columns.rawText;
}
