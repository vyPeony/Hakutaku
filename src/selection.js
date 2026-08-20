// 選択モデルの純粋関数（`COPY-001`、Issue #85）。
//
// 行選択は「アンカー + 互いに素な範囲の集合（`start` 昇順）」として保持する。
// クリックで単一行、Shift+クリックでアンカーからの範囲、Ctrl+クリックで行単位の
// 追加・除外（飛び飛びの選択）、ドラッグで開始行から現在行までの範囲、Ctrl+A で
// 全行を選ぶ（いずれも表示集合全体のインデックスであり、DOM にも本文にも
// 触れない）。
//
// # 範囲の集合にした理由（Issue #85）
//
// P10 の初版は「アンカー〜フォーカス」の連続した1範囲だけを保持していた。
// Issue #85 で飛び飛びの選択（Ctrl+クリック）を追加したため、連続した1範囲では
// 表現できなくなった。範囲の集合にすると、飛び飛びの選択でも保持するのは
// 「選んだ塊の数 × 数値2つ」で済み、行そのものを1件ずつ持つ方式（インデックスの
// 集合）と違って全選択（Ctrl+A）が数値2つに収まる。
//
// `PERF-012`「取得済みの行を累積しない」に合わせ、選択はインデックス範囲
// （数値の組）だけを保持し、行の本文（`raw_text` 等）は一切保持しない。仮想
// スクロールで表示されていない行（未取得のチャンク）も、インデックスさえ
// 分かれば選択でき、全選択（Ctrl+A）が仮想スクロールと両立する。範囲の個数は
// 利用者の Ctrl+クリック操作の回数でしか増えず、行数に比例して増えることは
// ない（隣接・重複する範囲は正規化で必ず1つへ畳む）。
//
// DOM にも IPC にも触れない純粋関数のみを置く（ADR-0006、AGENTS.md の指示）。
//
// この選択モデルの回帰検査は `scripts/check-selection.mjs`（Node から直接
// 呼ぶ純粋関数の検査。`docs/verification/regression-checks.md`）が行う。

/**
 * @typedef {Object} SelectionRange 選択された連続範囲（0起点、`start` を含む
 * 半開区間の長さが `count`）。`count` は必ず1以上。
 * @property {number} start
 * @property {number} count
 */

/**
 * @typedef {Object} SelectionState
 * @property {number | null} anchorIndex 範囲選択の起点（Shift+クリックの基準）。
 *   選択が無ければ `null`。
 * @property {SelectionRange[]} ranges 選択されている範囲の集合。**常に正規化
 *   済み**（`start` 昇順、互いに重ならず、隣接する範囲は1つへ結合済み、
 *   `count` は1以上）。この不変条件は本モジュールの関数だけが維持する
 *   （呼び出し側が直接組み立てない）。
 */

/** @returns {SelectionState} 選択なしの初期状態。 */
export function createSelectionState() {
  return { anchorIndex: null, ranges: [] };
}

/**
 * 範囲の集合を正規化する（`start` 昇順に整列し、重なる範囲と隣接する範囲を
 * 1つへ結合し、`count` が0以下の範囲を捨てる）。
 *
 * 正規化を1か所へ集約しているのは、`isRowSelected` の二分探索と
 * [`toCopyRanges`] が渡す範囲列（Rust 側 `hakutaku_core::assemble_copy` が
 * 「`start` 昇順・互いに素」を要求する）の前提が、この不変条件だけで
 * 成り立っているため。
 *
 * @param {SelectionRange[]} ranges 未整列・重複を含んでよい範囲の集合。
 * @returns {SelectionRange[]} 正規化した新しい配列。
 */
function normalizeRanges(ranges) {
  const sorted = ranges.filter((range) => range.count > 0).sort((a, b) => a.start - b.start);
  /** @type {SelectionRange[]} */
  const merged = [];
  for (const range of sorted) {
    const last = merged[merged.length - 1];
    // 隣接（前の範囲の終端 === 次の範囲の開始）も結合する。結合しないと、
    // 見た目が同じ選択に対して範囲の分かれ方が操作履歴で変わってしまい、
    // 範囲の個数が Ctrl+クリックの回数を超えて増える余地が残る。
    if (last !== undefined && range.start <= last.start + last.count) {
      const end = Math.max(last.start + last.count, range.start + range.count);
      last.count = end - last.start;
      continue;
    }
    merged.push({ start: range.start, count: range.count });
  }
  return merged;
}

/**
 * 2つの行インデックスから、両端を含む1つの範囲を作る（順序はどちらでもよい）。
 *
 * @param {number} indexA
 * @param {number} indexB
 * @returns {SelectionRange}
 */
function rangeBetween(indexA, indexB) {
  const low = Math.min(indexA, indexB);
  const high = Math.max(indexA, indexB);
  return { start: low, count: high - low + 1 };
}

/**
 * 単一行を選択する（修飾キーなしのクリック、ドラッグの開始）。それまでの
 * 選択はすべて置き換える。
 *
 * @param {number} rowIndex
 * @returns {SelectionState}
 */
export function selectSingleRow(rowIndex) {
  return { anchorIndex: rowIndex, ranges: [{ start: rowIndex, count: 1 }] };
}

/**
 * アンカーから `rowIndex` までの範囲へ選択を置き換える（Shift+クリック）。
 * アンカーが無い場合は単一行選択と同じ扱いにする。
 *
 * 飛び飛びの選択（Ctrl+クリック）があっても**置き換える**（追加しない）。
 * Shift+クリックは「アンカーからここまで」を選び直す操作であり、直前の
 * 飛び飛びの選択を足し込むと、利用者から見て何が選ばれているかを予測でき
 * なくなるため。
 *
 * @param {SelectionState} state
 * @param {number} rowIndex
 * @returns {SelectionState}
 */
export function extendSelectionTo(state, rowIndex) {
  if (state.anchorIndex === null) {
    return selectSingleRow(rowIndex);
  }
  return {
    anchorIndex: state.anchorIndex,
    ranges: [rangeBetween(state.anchorIndex, rowIndex)],
  };
}

/**
 * 1行の選択・非選択を反転する（Ctrl+クリック。飛び飛びの選択。Issue #85）。
 *
 * 既に選択されている行なら選択から外し（その行を含む範囲を分割する）、
 * 選択されていなければ追加する（隣接する範囲は正規化で1つへ結合される）。
 * どちらの場合もアンカーは操作した行へ移す（続けて Shift+クリックすると、
 * 最後に Ctrl+クリックした行からの範囲になる）。
 *
 * @param {SelectionState} state
 * @param {number} rowIndex
 * @returns {SelectionState}
 */
export function toggleRowSelection(state, rowIndex) {
  if (!isRowSelected(state, rowIndex)) {
    return {
      anchorIndex: rowIndex,
      ranges: normalizeRanges([...state.ranges, { start: rowIndex, count: 1 }]),
    };
  }

  /** @type {SelectionRange[]} */
  const remaining = [];
  for (const range of state.ranges) {
    const end = range.start + range.count;
    if (rowIndex < range.start || rowIndex >= end) {
      remaining.push({ start: range.start, count: range.count });
      continue;
    }
    // 除外した行の前後を、それぞれ空でなければ残す（範囲の分割）。
    if (rowIndex > range.start) {
      remaining.push({ start: range.start, count: rowIndex - range.start });
    }
    if (rowIndex + 1 < end) {
      remaining.push({ start: rowIndex + 1, count: end - (rowIndex + 1) });
    }
  }
  const ranges = normalizeRanges(remaining);
  return {
    // 選択が空になった場合だけアンカーも手放す（次のクリックが「アンカー
    // 無し」から始まり、Shift+クリックが単一行選択になる）。
    anchorIndex: ranges.length === 0 ? null : rowIndex,
    ranges,
  };
}

/**
 * ドラッグ中の選択を更新する（Issue #85）。開始行から現在行までの範囲で
 * 選択全体を置き換える。
 *
 * ドラッグ前の選択を保たないのは、押した時点で単一行選択へ置き換わる
 * （`selectSingleRow`）操作の延長であり、途中で戻したときに元の飛び飛びの
 * 選択が復活すると挙動が予測しづらくなるため。
 *
 * @param {number} dragStartIndex ドラッグを開始した行。
 * @param {number} currentRowIndex いま指している行。
 * @returns {SelectionState}
 */
export function updateDragSelection(dragStartIndex, currentRowIndex) {
  return {
    anchorIndex: dragStartIndex,
    ranges: [rangeBetween(dragStartIndex, currentRowIndex)],
  };
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
  return { anchorIndex: 0, ranges: [{ start: 0, count: totalItems }] };
}

/** @returns {SelectionState} 選択なしの状態。 */
export function clearSelection() {
  return createSelectionState();
}

/** @param {SelectionState} state
 *  @returns {boolean} 選択が無いか。 */
export function isSelectionEmpty(state) {
  return state.ranges.length === 0;
}

/**
 * 選択されている行数の合計（コピー前の件数表示・上限の見積もりに使う）。
 *
 * @param {SelectionState} state
 * @returns {number}
 */
export function getSelectedRowCount(state) {
  let total = 0;
  for (const range of state.ranges) {
    total += range.count;
  }
  return total;
}

/**
 * 選択を `totalItems` の範囲へ丸める（表示集合が縮んだ場合、タブを離れて
 * いる間に内容が変わった場合の防御。Issue #48 の復元経路でも使う）。
 *
 * `totalItems` の外へ出た範囲は捨て、またがる範囲は末尾を切り詰める。
 * アンカーも範囲外なら手放す（範囲外のアンカーから Shift+クリックすると、
 * 表示集合の外を含む範囲になってしまうため）。
 *
 * @param {SelectionState} state
 * @param {number} totalItems
 * @returns {SelectionState}
 */
export function clampSelectionToTotalItems(state, totalItems) {
  if (totalItems <= 0) {
    return createSelectionState();
  }
  /** @type {SelectionRange[]} */
  const clamped = [];
  for (const range of state.ranges) {
    if (range.start >= totalItems) {
      continue;
    }
    const end = Math.min(range.start + range.count, totalItems);
    clamped.push({ start: range.start, count: end - range.start });
  }
  const ranges = normalizeRanges(clamped);
  const anchorIndex =
    state.anchorIndex === null || state.anchorIndex >= totalItems || ranges.length === 0
      ? null
      : state.anchorIndex;
  return { anchorIndex, ranges };
}

/**
 * `rowIndex` が現在の選択に含まれるか（行のハイライト表示に使う）。
 *
 * 範囲の集合は `start` 昇順で互いに素という不変条件（[`SelectionState`]）が
 * あるため二分探索できる。行の描画は可視範囲の行数ぶん毎フレーム呼ばれる
 * ため、範囲の個数に対して線形に走査しない。
 *
 * @param {SelectionState} state
 * @param {number} rowIndex
 * @returns {boolean}
 */
export function isRowSelected(state, rowIndex) {
  const ranges = state.ranges;
  let low = 0;
  let high = ranges.length - 1;
  while (low <= high) {
    const middle = (low + high) >> 1;
    const range = ranges[middle];
    if (rowIndex < range.start) {
      high = middle - 1;
    } else if (rowIndex >= range.start + range.count) {
      low = middle + 1;
    } else {
      return true;
    }
  }
  return false;
}

/**
 * 選択を `copy_selection` コマンドへ渡す範囲列へ変換する（`COPY-001`／
 * `COPY-002`）。`totalItems` でクランプしたうえで、`start` 昇順・互いに素・
 * `count` が1以上という Rust 側の受け入れ条件
 * （`hakutaku_core::assemble_copy`）を満たす配列を返す。
 *
 * 選択が無い、またはクランプの結果すべて空になった場合は空配列を返す。
 * 呼び出し側は空配列ならコマンドを呼ばない（`COPY-006`／`SEC-004`: 選択が
 * 無いときはクリップボードに一切触れない）。
 *
 * @param {SelectionState} state
 * @param {number} totalItems
 * @returns {SelectionRange[]}
 */
export function toCopyRanges(state, totalItems) {
  return clampSelectionToTotalItems(state, totalItems).ranges.map((range) => ({
    start: range.start,
    count: range.count,
  }));
}
