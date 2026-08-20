// 行選択モデルの回帰検査（Issue #85）。
//
// `src/selection.js` の純粋関数（DOM にも Tauri IPC にも触れない）を Node から
// 直接呼び、選択操作で壊れやすい5種類の判断を検証する。
//
//   1. 範囲の集合の正規化（Ctrl+クリックのトグルによる分割・結合、隣接の結合）
//   2. 基本操作（単一選択・Shift 拡張・全選択・クリア）とドラッグ更新
//   3. 表示集合の項目数（`totalItems`）へのクランプ
//   4. `isRowSelected` の二分探索と境界
//   5. `copy_selection` へ渡す範囲列（`toCopyRanges`）が Rust 側の受け入れ条件
//      （`hakutaku_core::assemble_copy`: `start` 昇順・互いに素・`count` が
//      1以上・表示集合の範囲内）を必ず満たすこと
//
// 選択は `src/log_view.js` の DOM 操作と `copy_selection` の呼び出しに挟まれた
// 純粋な状態遷移であり、GUI 検査（`scripts/check-gui.mjs`。手動実行）でしか
// 通らない経路が多い。飛び飛びの選択は「どの操作をどの順で行ったか」で状態が
// 変わるため、境界の取り違え（範囲の分割漏れ、隣接の結合漏れ、クランプ漏れ）は
// 画面を動かさなければ気付けなかった。ここでは操作の組み合わせを Node だけで
// 走らせ、その退行を CI で毎回捉える（`docs/verification/regression-checks.md`）。
//
// 期待値は「現在の実装が返した値」を写したものではなく、各関数の JSDoc が定める
// 仕様から独立に導いた値を書く。導出根拠は各検査の直前のコメントに残す。実装を
// 書き換えたときに期待値も一緒に書き換えてしまい、検査が何も守らなくなることを
// 防ぐため（`check-virtual-scroll.mjs` と同じ方針）。
//
// 実行時間・メモリ量は一切扱わない（`VER-005`）。
//
// 使い方: node scripts/check-selection.mjs

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import {
  clampSelectionToTotalItems,
  clearSelection,
  createSelectionState,
  extendSelectionTo,
  getSelectedRowCount,
  isRowSelected,
  isSelectionEmpty,
  selectAll,
  selectSingleRow,
  toCopyRanges,
  toggleRowSelection,
  updateDragSelection,
} from "../src/selection.js";

const ROOT = resolve(import.meta.dirname, "..");

// ---------------------------------------------------------------------------
// 検査の土台（`check-virtual-scroll.mjs` と同じ形）
// ---------------------------------------------------------------------------

const problems = [];
let checkCount = 0;

function format(value) {
  return typeof value === "object" && value !== null ? JSON.stringify(value) : String(value);
}

/**
 * 1件の検査結果を記録する。1件目の失敗で終了せず全件を集めるのは、退行の
 * 影響範囲（1つの境界だけか、経路全体か）を1回の実行で見分けられるようにするため。
 */
function check(name, ok, detail) {
  checkCount += 1;
  if (!ok) {
    problems.push(detail ? `${name}\n    ${detail}` : name);
  }
}

function expectEqual(name, actual, expected) {
  check(name, Object.is(actual, expected), `期待 ${format(expected)} / 実際 ${format(actual)}`);
}

/** 選択状態の範囲集合を `[[start, count], ...]` として突き合わせる。 */
function expectRanges(name, state, expected) {
  const actual = state.ranges.map((range) => [range.start, range.count]);
  const ok =
    actual.length === expected.length &&
    actual.every(
      (range, index) => range[0] === expected[index][0] && range[1] === expected[index][1],
    );
  check(name, ok, `期待 ${format(expected)} / 実際 ${format(actual)}`);
}

/**
 * 範囲集合が [`SelectionState`] の不変条件（`start` 昇順、`count` が1以上、
 * 重ならない、隣接しない）を満たすか。満たさない場合は理由を返す。
 *
 * 隣接まで禁じるのは、`src/selection.js` の `normalizeRanges` が隣接を必ず
 * 1つへ結合すると定めているため（結合しないと、同じ見た目の選択に対して範囲の
 * 個数が操作履歴で変わってしまう）。
 */
function findRangeInvariantViolation(ranges) {
  let previousEnd = null;
  for (const range of ranges) {
    if (!Number.isInteger(range.start) || !Number.isInteger(range.count)) {
      return `整数でない範囲: ${format(range)}`;
    }
    if (range.count < 1) {
      return `count が1未満: ${format(range)}`;
    }
    if (previousEnd !== null && range.start <= previousEnd) {
      return `昇順でない、重なる、または隣接: 前の終端 ${previousEnd} / 次の開始 ${range.start}`;
    }
    previousEnd = range.start + range.count;
  }
  return null;
}

/** 範囲集合が覆う行インデックスの集合（参照モデルとの突き合わせ用）。 */
function rowsOf(state) {
  const rows = new Set();
  for (const range of state.ranges) {
    for (let index = range.start; index < range.start + range.count; index += 1) {
      rows.add(index);
    }
  }
  return rows;
}

// ---------------------------------------------------------------------------
// 1. 基本操作（単一選択・Shift 拡張・全選択・クリア）
// ---------------------------------------------------------------------------
//
// 仕様（各関数の JSDoc）:
//   selectSingleRow(i)        → アンカー i、範囲は {start: i, count: 1} だけ
//   extendSelectionTo(s, i)   → アンカーは保ったまま、アンカー〜i の1範囲へ
//                               **置き換える**（アンカーが無ければ単一選択）
//   selectAll(n)              → n ≤ 0 なら選択なし、そうでなければ {0, n} の1範囲
//   clearSelection()          → 選択なし
// 以下の期待値はこの4つから直接導いている。

function checkBasicOperations() {
  const empty = createSelectionState();
  expectEqual("初期状態: アンカーが無い", empty.anchorIndex, null);
  expectRanges("初期状態: 範囲が無い", empty, []);
  expectEqual("初期状態: 空と判定される", isSelectionEmpty(empty), true);
  expectEqual("初期状態: 選択行数は0", getSelectedRowCount(empty), 0);

  const single = selectSingleRow(5);
  expectEqual("単一選択: アンカーは押した行", single.anchorIndex, 5);
  expectRanges("単一選択: その行だけ", single, [[5, 1]]);
  expectEqual("単一選択: 空ではない", isSelectionEmpty(single), false);
  expectEqual("単一選択: 選択行数は1", getSelectedRowCount(single), 1);

  // アンカー5から2へ戻る向きの Shift+クリック。両端を含むので 2〜5 の4行。
  const backwards = extendSelectionTo(single, 2);
  expectEqual("Shift 拡張: アンカーは動かない", backwards.anchorIndex, 5);
  expectRanges("Shift 拡張: 逆向きでも両端を含む", backwards, [[2, 4]]);
  expectEqual("Shift 拡張: 選択行数は4", getSelectedRowCount(backwards), 4);

  // アンカー5から9への Shift+クリック。5〜9 の5行。
  expectRanges("Shift 拡張: 順方向", extendSelectionTo(single, 9), [[5, 5]]);
  // 同じ行を Shift+クリックするとその1行だけ。
  expectRanges("Shift 拡張: アンカーと同じ行なら1行", extendSelectionTo(single, 5), [[5, 1]]);

  // アンカーが無い状態の Shift+クリックは単一行選択と同じ扱い。
  const fromEmpty = extendSelectionTo(empty, 7);
  expectEqual("Shift 拡張: アンカーが無ければ単一選択のアンカー", fromEmpty.anchorIndex, 7);
  expectRanges("Shift 拡張: アンカーが無ければ単一選択", fromEmpty, [[7, 1]]);

  // 飛び飛びの選択があっても「置き換える」（足し込まない）。
  const scattered = toggleRowSelection(toggleRowSelection(empty, 0), 20);
  const replaced = extendSelectionTo({ ...scattered, anchorIndex: 3 }, 5);
  expectRanges("Shift 拡張: 飛び飛びの選択を置き換える", replaced, [[3, 3]]);

  expectRanges("全選択: 0行なら選択なし", selectAll(0), []);
  expectRanges("全選択: 負の総行数でも選択なし", selectAll(-1), []);
  expectRanges("全選択: 1行", selectAll(1), [[0, 1]]);
  expectEqual("全選択: アンカーは先頭", selectAll(1).anchorIndex, 0);
  // PERF-012: 2000万行の全選択でも保持するのは範囲1つ（数値2つ）だけ。
  const huge = selectAll(20_000_000);
  expectRanges("全選択: 2000万行でも範囲は1つ", huge, [[0, 20_000_000]]);
  expectEqual("全選択: 2000万行の選択行数", getSelectedRowCount(huge), 20_000_000);

  expectRanges("クリア: 範囲が無くなる", clearSelection(), []);
  expectEqual("クリア: アンカーも無くなる", clearSelection().anchorIndex, null);
}

// ---------------------------------------------------------------------------
// 2. Ctrl+クリックのトグル（範囲の分割・結合・隣接結合）
// ---------------------------------------------------------------------------
//
// 仕様（`toggleRowSelection` の JSDoc）:
//   選択済みの行 → その行を含む範囲から取り除く（必要なら範囲を2つへ分割）
//   未選択の行   → 追加する（隣接・重複する範囲は正規化で1つへ結合）
//   アンカーは操作した行へ移す。ただし選択が空になった場合は null
// 以下の期待値はこの規則を手で適用した結果である。

function checkToggle() {
  const empty = createSelectionState();

  // 追加（空 → 1行）。
  const added = toggleRowSelection(empty, 3);
  expectRanges("トグル: 空の状態への追加", added, [[3, 1]]);
  expectEqual("トグル: 追加でアンカーがその行へ移る", added.anchorIndex, 3);

  // 離れた行の追加（飛び飛び。範囲が2つになる）。
  const scattered = toggleRowSelection(added, 6);
  expectRanges("トグル: 離れた行の追加で範囲が2つ", scattered, [
    [3, 1],
    [6, 1],
  ]);
  expectEqual("トグル: 飛び飛びの選択行数は合計", getSelectedRowCount(scattered), 2);

  // 隣接の結合。3 の直後（4）を足すと {3,2} の1範囲になる（{3,1}+{4,1} の
  // 2範囲のままにしない）。
  expectRanges("トグル: 直後の行を足すと隣接結合", toggleRowSelection(added, 4), [[3, 2]]);
  // 直前（2）を足す場合も同じく1範囲。
  expectRanges("トグル: 直前の行を足すと隣接結合", toggleRowSelection(added, 2), [[2, 2]]);

  // 2つの範囲の間を埋めると1つへ結合される（{0,1} と {2,1} の間の 1）。
  const gap = toggleRowSelection(toggleRowSelection(empty, 0), 2);
  expectRanges("トグル: 前提（間が空いた2範囲）", gap, [
    [0, 1],
    [2, 1],
  ]);
  expectRanges("トグル: 間を埋めると1範囲へ結合", toggleRowSelection(gap, 1), [[0, 3]]);

  // 除外（範囲の分割）。0〜4 の5行から真ん中の2を外すと {0,2} と {3,2}。
  const all5 = selectAll(5);
  const split = toggleRowSelection(all5, 2);
  expectRanges("トグル: 範囲の途中を外すと2つへ分割", split, [
    [0, 2],
    [3, 2],
  ]);
  expectEqual("トグル: 除外でアンカーがその行へ移る", split.anchorIndex, 2);
  expectEqual("トグル: 分割後の選択行数", getSelectedRowCount(split), 4);

  // 端の除外は分割にならない（片側だけが残る）。
  expectRanges("トグル: 範囲の先頭を外す", toggleRowSelection(all5, 0), [[1, 4]]);
  expectRanges("トグル: 範囲の末尾を外す", toggleRowSelection(all5, 4), [[0, 4]]);

  // 最後の1行を外すと選択が空になり、アンカーも手放す（次のクリックが
  // 「アンカー無し」から始まる）。
  const emptied = toggleRowSelection(added, 3);
  expectRanges("トグル: 最後の1行を外すと空", emptied, []);
  expectEqual("トグル: 空になったらアンカーも手放す", emptied.anchorIndex, null);

  // 同じ行を2回トグルすると元へ戻る（GUI 検査シナリオ6の (b) と同じ性質）。
  const twice = toggleRowSelection(toggleRowSelection(scattered, 6), 6);
  expectRanges("トグル: 2回で元へ戻る", twice, [
    [3, 1],
    [6, 1],
  ]);

  // 分割した範囲の片側をさらに分割しても不変条件は崩れない。
  const twiceSplit = toggleRowSelection(split, 4);
  expectRanges("トグル: 分割後の範囲をさらに分割", twiceSplit, [
    [0, 2],
    [3, 1],
  ]);
  check(
    "トグル: 分割を重ねても範囲の不変条件を保つ",
    findRangeInvariantViolation(twiceSplit.ranges) === null,
    findRangeInvariantViolation(twiceSplit.ranges),
  );
}

// ---------------------------------------------------------------------------
// 3. ドラッグ更新
// ---------------------------------------------------------------------------
//
// 仕様（`updateDragSelection` の JSDoc）:
//   開始行から現在行までの1範囲で選択**全体を置き換える**。アンカーは開始行。
//   向き（上下どちらへドラッグしたか）は結果の範囲に影響しない。

function checkDragSelection() {
  const downwards = updateDragSelection(3, 7);
  expectRanges("ドラッグ: 下方向は開始行から現在行まで", downwards, [[3, 5]]);
  expectEqual("ドラッグ: アンカーは開始行", downwards.anchorIndex, 3);

  const upwards = updateDragSelection(7, 3);
  expectRanges("ドラッグ: 上方向でも同じ範囲", upwards, [[3, 5]]);
  expectEqual("ドラッグ: 上方向でもアンカーは開始行", upwards.anchorIndex, 7);

  expectRanges("ドラッグ: 開始行に戻ると1行", updateDragSelection(3, 3), [[3, 1]]);

  // 途中で戻しても、ドラッグ前の飛び飛びの選択は復活しない（置き換えのため）。
  const scattered = toggleRowSelection(toggleRowSelection(createSelectionState(), 0), 9);
  expectRanges("ドラッグ: ドラッグ前の選択は復活しない", updateDragSelection(4, 5), [[4, 2]]);
  expectEqual("ドラッグ: 前提（ドラッグ前は飛び飛び）", getSelectedRowCount(scattered), 2);
}

// ---------------------------------------------------------------------------
// 4. `totalItems` へのクランプ
// ---------------------------------------------------------------------------
//
// 仕様（`clampSelectionToTotalItems` の JSDoc）:
//   totalItems ≤ 0            → 選択なし
//   範囲が totalItems の外     → 捨てる
//   範囲が totalItems をまたぐ → 末尾を切り詰める
//   アンカーが範囲外、または結果が空 → アンカーは null

function checkClamp() {
  const all10 = selectAll(10);
  expectRanges("クランプ: 総行数0なら選択なし", clampSelectionToTotalItems(all10, 0), []);
  expectRanges("クランプ: 負の総行数でも選択なし", clampSelectionToTotalItems(all10, -5), []);

  // またぐ範囲は切り詰める（0〜9 の選択を4行の表示集合へ当てると 0〜3）。
  expectRanges("クランプ: またぐ範囲を切り詰める", clampSelectionToTotalItems(all10, 4), [[0, 4]]);
  // 総行数ちょうどは切り詰めない（境界は内側）。
  expectRanges("クランプ: 総行数ちょうどはそのまま", clampSelectionToTotalItems(all10, 10), [
    [0, 10],
  ]);

  // 外に出た範囲は捨てる。{0,1} と {9,2}（9〜10）を総行数5へ当てると {0,1} だけ。
  const scattered = {
    anchorIndex: 9,
    ranges: [
      { start: 0, count: 1 },
      { start: 9, count: 2 },
    ],
  };
  const clamped = clampSelectionToTotalItems(scattered, 5);
  expectRanges("クランプ: 範囲外の範囲は捨てる", clamped, [[0, 1]]);
  expectEqual("クランプ: 範囲外のアンカーは手放す", clamped.anchorIndex, null);

  // 範囲内のアンカーは保つ。
  const keptAnchor = clampSelectionToTotalItems({ ...scattered, anchorIndex: 0 }, 5);
  expectEqual("クランプ: 範囲内のアンカーは保つ", keptAnchor.anchorIndex, 0);

  // 開始位置が総行数ちょうどの範囲は丸ごと外（0起点のため最後の行は総行数-1）。
  expectRanges(
    "クランプ: 開始位置が総行数ちょうどの範囲は捨てる",
    clampSelectionToTotalItems({ anchorIndex: null, ranges: [{ start: 5, count: 1 }] }, 5),
    [],
  );

  // 切り詰めの結果も不変条件を満たす（隣接が生じたら結合される）。
  const adjacentAfterClamp = clampSelectionToTotalItems(
    {
      anchorIndex: 0,
      ranges: [
        { start: 0, count: 2 },
        { start: 2, count: 10 },
      ],
    },
    6,
  );
  expectRanges("クランプ: 切り詰め後に隣接すれば結合する", adjacentAfterClamp, [[0, 6]]);
}

// ---------------------------------------------------------------------------
// 5. `isRowSelected`（二分探索）の境界
// ---------------------------------------------------------------------------
//
// 仕様（`isRowSelected` の JSDoc）:
//   rowIndex が範囲 {start, count} の半開区間 [start, start + count) に入るか。
//   範囲集合は昇順・互いに素なので二分探索できる。
// 半開区間なので、start は含み start + count は含まない。以下はその境界を、
// 範囲が1つの場合と多数の場合の両方で確認する。

function checkIsRowSelected() {
  const empty = createSelectionState();
  expectEqual("含有判定: 選択が無ければ常に false", isRowSelected(empty, 0), false);
  expectEqual("含有判定: 負の行でも false", isRowSelected(empty, -1), false);

  // 単一範囲 {10, 5} = 行 10〜14。
  const one = { anchorIndex: 10, ranges: [{ start: 10, count: 5 }] };
  expectEqual("含有判定: 範囲の1つ手前は false", isRowSelected(one, 9), false);
  expectEqual("含有判定: 範囲の先頭は true", isRowSelected(one, 10), true);
  expectEqual("含有判定: 範囲の末尾は true", isRowSelected(one, 14), true);
  expectEqual("含有判定: 範囲の直後は false", isRowSelected(one, 15), false);

  // 多数の範囲（偶数行だけを選んだ状態）。二分探索が全ての位置で正しいこと、
  // 特に範囲の間（谷）と両端で誤らないことを確認する。
  const evenRanges = [];
  for (let index = 0; index < 200; index += 2) {
    evenRanges.push({ start: index, count: 1 });
  }
  const evens = { anchorIndex: 0, ranges: evenRanges };
  check(
    "含有判定: 多数の範囲の前提（不変条件を満たす）",
    findRangeInvariantViolation(evens.ranges) === null,
    findRangeInvariantViolation(evens.ranges),
  );
  let mismatched = null;
  for (let index = -1; index <= 200; index += 1) {
    const expected = index >= 0 && index < 200 && index % 2 === 0;
    if (isRowSelected(evens, index) !== expected) {
      mismatched ??= `行 ${index}: 期待 ${expected}`;
    }
  }
  check("含有判定: 100範囲の全ての位置で正しい（202点）", mismatched === null, mismatched);

  // 2000万行の全選択でも、範囲は1つなので両端と中央が正しく判定できる
  // （PERF-012: 行ごとの集合を持たない設計が成り立っていること）。
  const huge = selectAll(20_000_000);
  expectEqual("含有判定: 2000万行の先頭", isRowSelected(huge, 0), true);
  expectEqual("含有判定: 2000万行の中央", isRowSelected(huge, 10_000_000), true);
  expectEqual("含有判定: 2000万行の末尾", isRowSelected(huge, 19_999_999), true);
  expectEqual("含有判定: 2000万行の1つ外", isRowSelected(huge, 20_000_000), false);
}

// ---------------------------------------------------------------------------
// 6. `toCopyRanges`（Rust 側の受け入れ条件）
// ---------------------------------------------------------------------------
//
// 仕様（`toCopyRanges` の JSDoc と `hakutaku_core::assemble_copy` のモジュール
// doc コメント「選択範囲の受け入れ条件」）:
//   totalItems でクランプしたうえで、start 昇順・互いに素・count ≥ 1 で、
//   すべて表示集合の範囲内の配列を返す。空なら空配列（呼び出し側はコマンドを
//   呼ばない = クリップボードに触れない。COPY-006）。

function checkCopyRanges() {
  expectEqual("コピー範囲: 選択が無ければ空配列", toCopyRanges(createSelectionState(), 100).length, 0);
  expectEqual("コピー範囲: 総行数0なら空配列", toCopyRanges(selectAll(10), 0).length, 0);

  // 飛び飛びの選択がそのまま複数範囲として渡る（Rust 側が昇順に連結する）。
  let scattered = createSelectionState();
  for (const row of [7, 1, 4]) {
    scattered = toggleRowSelection(scattered, row);
  }
  const ranges = toCopyRanges(scattered, 10);
  check(
    "コピー範囲: 飛び飛びの選択が昇順の範囲列になる",
    ranges.length === 3 &&
      ranges[0].start === 1 &&
      ranges[1].start === 4 &&
      ranges[2].start === 7 &&
      ranges.every((range) => range.count === 1),
    format(ranges),
  );

  // クランプが効く（表示集合の外を含む範囲は渡さない）。
  const clampedRanges = toCopyRanges(selectAll(10), 3);
  check(
    "コピー範囲: 表示集合の外へ出ない",
    clampedRanges.length === 1 &&
      clampedRanges[0].start === 0 &&
      clampedRanges[0].count === 3,
    format(clampedRanges),
  );

  // 呼び出し側（log_view.js）が渡すのはプレーンな数値2つの組であること
  // （余分な項目が混ざると Tauri の引数解釈で落ちる）。
  const keys = Object.keys(clampedRanges[0]).sort();
  check(
    "コピー範囲: 範囲の項目は start と count だけ",
    keys.length === 2 && keys[0] === "count" && keys[1] === "start",
    format(keys),
  );
}

// ---------------------------------------------------------------------------
// 7. 操作の組み合わせに対する不変条件（参照モデルとの突き合わせ）
// ---------------------------------------------------------------------------
//
// 単発の期待値では、操作の**順序**によってしか現れない退行（分割した範囲へ
// さらに追加する、クランプ後にトグルする、など）を捉えられない。ここでは
// 決定的な擬似乱数で操作列を作り、行インデックスの集合（Set）を参照モデルと
// して次を毎回突き合わせる。
//
//   - 範囲集合が常に昇順・互いに素・隣接なし・count ≥ 1（[`SelectionState`] の
//     不変条件。`isRowSelected` の二分探索と Rust 側の受け入れ条件の前提）
//   - `isRowSelected` が参照モデルと一致する
//   - `getSelectedRowCount` が参照モデルの要素数と一致する
//   - `toCopyRanges` の結果が Rust 側の受け入れ条件を満たし、覆う行が参照
//     モデルと一致する
//
// 擬似乱数は線形合同法で、種を固定する（同じ入力に対して常に同じ操作列になり、
// 失敗が再現できる）。

const TOTAL_ITEMS = 64;
const OPERATION_COUNT = 400;

/** 線形合同法（Numerical Recipes の係数）。決定性のためだけに使う。 */
function createRandom(seed) {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1_664_525) + 1_013_904_223) >>> 0;
    return state / 4_294_967_296;
  };
}

function checkOperationSequences() {
  const random = createRandom(20_260_085);
  const pickRow = () => Math.floor(random() * TOTAL_ITEMS);

  let state = createSelectionState();
  /** @type {Set<number>} 参照モデル（選択されている行インデックスの集合）。 */
  let expectedRows = new Set();

  let invariantViolation = null;
  let membershipMismatch = null;
  let countMismatch = null;
  let copyRangeViolation = null;
  let copyRowsMismatch = null;

  const rowsBetween = (a, b) => {
    const rows = new Set();
    for (let index = Math.min(a, b); index <= Math.max(a, b); index += 1) {
      rows.add(index);
    }
    return rows;
  };

  for (let step = 0; step < OPERATION_COUNT; step += 1) {
    const operation = Math.floor(random() * 5);
    const row = pickRow();
    switch (operation) {
      case 0:
        state = selectSingleRow(row);
        expectedRows = new Set([row]);
        break;
      case 1: {
        const anchor = state.anchorIndex;
        state = extendSelectionTo(state, row);
        expectedRows = anchor === null ? new Set([row]) : rowsBetween(anchor, row);
        break;
      }
      case 2:
        state = toggleRowSelection(state, row);
        if (expectedRows.has(row)) {
          expectedRows.delete(row);
        } else {
          expectedRows.add(row);
        }
        break;
      case 3: {
        const dragStart = pickRow();
        state = updateDragSelection(dragStart, row);
        expectedRows = rowsBetween(dragStart, row);
        break;
      }
      default:
        state = selectAll(TOTAL_ITEMS);
        expectedRows = new Set();
        for (let index = 0; index < TOTAL_ITEMS; index += 1) {
          expectedRows.add(index);
        }
        break;
    }

    const violation = findRangeInvariantViolation(state.ranges);
    if (violation !== null) {
      invariantViolation ??= `${step} 手目（操作 ${operation}、行 ${row}）: ${violation}`;
    }
    for (let index = 0; index < TOTAL_ITEMS; index += 1) {
      if (isRowSelected(state, index) !== expectedRows.has(index)) {
        membershipMismatch ??= `${step} 手目（操作 ${operation}、行 ${row}）: 行 ${index}`;
        break;
      }
    }
    if (getSelectedRowCount(state) !== expectedRows.size) {
      countMismatch ??=
        `${step} 手目（操作 ${operation}、行 ${row}）: ` +
        `期待 ${expectedRows.size} / 実際 ${getSelectedRowCount(state)}`;
    }

    const copyRanges = toCopyRanges(state, TOTAL_ITEMS);
    const copyViolation = findRangeInvariantViolation(copyRanges);
    const outOfBounds = copyRanges.find(
      (range) => range.start < 0 || range.start + range.count > TOTAL_ITEMS,
    );
    if (copyViolation !== null || outOfBounds !== undefined) {
      copyRangeViolation ??=
        `${step} 手目: ${copyViolation ?? `表示集合の外 ${format(outOfBounds)}`}`;
    }
    const copyRows = rowsOf({ ranges: copyRanges });
    if (
      copyRows.size !== expectedRows.size ||
      [...expectedRows].some((index) => !copyRows.has(index))
    ) {
      copyRowsMismatch ??= `${step} 手目: 期待 ${expectedRows.size} 行 / 実際 ${copyRows.size} 行`;
    }
  }

  check(
    `操作列: 範囲集合が常に昇順・互いに素・隣接なし（${OPERATION_COUNT} 手）`,
    invariantViolation === null,
    invariantViolation,
  );
  check(
    `操作列: isRowSelected が参照モデルと一致する（${OPERATION_COUNT} 手）`,
    membershipMismatch === null,
    membershipMismatch,
  );
  check(
    `操作列: 選択行数が参照モデルと一致する（${OPERATION_COUNT} 手）`,
    countMismatch === null,
    countMismatch,
  );
  check(
    `操作列: コピー範囲が Rust 側の受け入れ条件を満たす（${OPERATION_COUNT} 手）`,
    copyRangeViolation === null,
    copyRangeViolation,
  );
  check(
    `操作列: コピー範囲が覆う行が選択と一致する（${OPERATION_COUNT} 手）`,
    copyRowsMismatch === null,
    copyRowsMismatch,
  );
}

// ---------------------------------------------------------------------------
// 8. 前提の同期（`src/log_view.js` が選択モデルを通していること）
// ---------------------------------------------------------------------------
//
// 上の検査は `src/selection.js` の純粋関数だけを見ており、`log_view.js` が
// その関数を実際に使っているかまでは分からない。選択やコピー範囲の組み立てが
// log_view.js の中へ書き戻されると、この検査は通ったまま実物の挙動だけが
// 変わる。ソースから最小限の手掛かりを読み出して突き合わせる。

function checkPremises() {
  const source = readFileSync(resolve(ROOT, "src", "log_view.js"), "utf8");

  check(
    "前提: log_view.js が copy_selection の範囲を toCopyRanges から作る",
    source.includes("toCopyRanges(state.selection, state.totalItems)"),
    "選択モデルを通さずに範囲を組み立てている可能性があります",
  );
  check(
    "前提: log_view.js が行の選択表示を isRowSelected で復元する",
    source.includes("isRowSelected(state.selection, rowIndex)"),
    "仮想スクロールの再描画で選択表示が復元されなくなる可能性があります",
  );
  check(
    "前提: log_view.js が Ctrl+クリックのトグルを選択モデルへ委ねる",
    source.includes("toggleRowSelection(state.selection, rowIndex)"),
    "飛び飛びの選択が log_view.js 側で組み立てられている可能性があります",
  );
  check(
    "前提: log_view.js がドラッグ選択を選択モデルへ委ねる",
    source.includes("updateDragSelection(drag.startIndex, rowIndex)"),
    "ドラッグ選択が log_view.js 側で組み立てられている可能性があります",
  );
  // Issue #85 で廃止したコピー列 UI（`#log-copy-columns` 一式）が復活すると、
  // コピー内容が「常に原文そのまま」（ADR-0011）でなくなる。
  check(
    "前提: 廃止したコピー列 UI を参照していない",
    !source.includes("log-copy-column"),
    "コピー列のチェックボックスを読み直しています（ADR-0011 と矛盾します）",
  );

  const html = readFileSync(resolve(ROOT, "src", "index.html"), "utf8");
  check(
    "前提: index.html にコピー列 UI が無い",
    !html.includes("log-copy-column"),
    "コピー列の UI が残っています（ADR-0011 と矛盾します）",
  );
}

// ---------------------------------------------------------------------------
// 実行
// ---------------------------------------------------------------------------

checkBasicOperations();
checkToggle();
checkDragSelection();
checkClamp();
checkIsRowSelected();
checkCopyRanges();
checkOperationSequences();
checkPremises();

if (problems.length > 0) {
  console.error(`行選択モデルに問題が ${problems.length} 件あります。\n`);
  for (const problem of problems) console.error(`  ${problem}`);
  console.error(
    "\n判定方法と対象の一覧は docs/verification/regression-checks.md を参照してください。",
  );
  process.exit(1);
}

console.log(
  `行選択モデル（正規化、トグルによる分割と結合、ドラッグ更新、クランプ、二分探索、コピー範囲の変換）を ${checkCount} 項目検査しました。問題はありません。`,
);
