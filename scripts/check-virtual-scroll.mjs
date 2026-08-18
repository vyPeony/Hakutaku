// 仮想スクロールの規模依存ロジックの回帰検査（Issue #16）。
//
// `src/virtual_scroll.js` の純粋関数（DOM にも Tauri IPC にも触れない）を Node から
// 直接呼び、大規模データで壊れやすい3種類の判断を検証する。
//
//   1. スクロール高さのクランプ（`MAX_TOTAL_HEIGHT_PX`）
//   2. クランプ超過時の比例写像（スクロール座標 ↔ 行インデックス）
//   3. 行番号ジャンプと実際の描画位置の一致（画素座標側の不変条件。Issue #33）
//   4. 保持上限（`CFG-022`）に基づく破棄判定（`PERF-012`）
//
// 段階0の実測（`docs/verification/stage0-results.md` 2.3節・10節）で実際に起きた
// 退行——保持行数が上限を一時的に超える、2000万行規模でスクロールの末尾へ到達
// できない——を、2000万行の試験データも WebView2 も使わずに検知することが目的。
// これらの関数は入力から出力が一意に決まるため、行数だけを2000万に設定すれば、
// 実データなしで規模依存の経路をそのまま通せる。
//
// 期待値は「現在の実装が返した値」を写したものではなく、各関数の JSDoc が定める
// 仕様から独立に導いた値を書く。導出根拠は各検査の直前のコメントに残す。実装を
// 書き換えたときに期待値も一緒に書き換えてしまい、検査が何も守らなくなることを
// 防ぐため。
//
// 実行時間・メモリ量は一切扱わない。`VER-005` により段階0で計測した性能値は
// `PERF-009`・`PERF-015` の合否判定に使えないため、自動判定の対象は決定的な内部
// 状態だけに限る（`docs/verification/regression-checks.md`）。
//
// 使い方: node scripts/check-virtual-scroll.mjs

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import {
  DEFAULT_BUFFER_ROWS,
  JUMP_CONTEXT_ROWS,
  MAX_TOTAL_HEIGHT_PX,
  computeChunkRange,
  computeEffectiveTotalHeightPx,
  computeRequiredChunkIndices,
  computeScrollTopForRowIndexScaled,
  computeSpacerHeights,
  computeSpacerHeightsForScroll,
  computeVisibleRange,
  computeVisibleRangeForScroll,
  isHeightScalingActive,
  parseJumpTargetRowIndex,
  selectChunksToEvict,
} from "../src/virtual_scroll.js";

const ROOT = resolve(import.meta.dirname, "..");

// 実運用の動作点。`src/log_view.js` の同名定数と一致させる（末尾の「前提の同期」
// で実際に一致していることを検査する）。virtual_scroll.js 側はこれらを引数で
// 受け取るだけなので、検査自体はこの値に依存しないが、境界値をこの動作点で
// 選べるようにここに置く。
const ROW_HEIGHT_PX = 22;
const CHUNK_SIZE = 512;
const BUFFER_ROWS = 50;

// 1920×1080（`ENV-005` の基準解像度）でログ表示領域が取り得るおおよその高さ。
// 可視行数の計算に使うだけで、値そのものに意味はない。
const VIEWPORT_HEIGHT_PX = 1080;

// `CFG-022` の初期値（保持行数 10,000 行、保持バイト数 64 MiB）。
const DEFAULT_MAX_ROWS = 10_000;
const DEFAULT_MAX_BYTES = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// 検査の土台
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

function expectRange(name, actual, startIndex, endIndex) {
  const ok = actual.startIndex === startIndex && actual.endIndex === endIndex;
  check(name, ok, `期待 ${format({ startIndex, endIndex })} / 実際 ${format(actual)}`);
}

function expectIndices(name, actual, expected) {
  const ok =
    Array.isArray(actual) &&
    actual.length === expected.length &&
    actual.every((value, index) => value === expected[index]);
  check(name, ok, `期待 ${format(expected)} / 実際 ${format(actual)}`);
}

/** 浮動小数点の除算を含む値の比較。許容差は各呼び出し側の根拠コメントを参照。 */
function expectNear(name, actual, expected, tolerance) {
  const ok = Number.isFinite(actual) && Math.abs(actual - expected) <= tolerance;
  check(name, ok, `期待 ${format(expected)} ± ${tolerance} / 実際 ${format(actual)}`);
}

// ---------------------------------------------------------------------------
// 1. スクロール高さのクランプ（`MAX_TOTAL_HEIGHT_PX`）
// ---------------------------------------------------------------------------
//
// 仕様（`computeEffectiveTotalHeightPx`・`isHeightScalingActive` の JSDoc）:
//   実効総高さ = min(totalItems × rowHeightPx, MAX_TOTAL_HEIGHT_PX)
//   比例写像への切り替え = (totalItems × rowHeightPx > MAX_TOTAL_HEIGHT_PX)
// 「以下なら従来の1:1方式へ委譲する」と明記されているため、上限ちょうど（等号）は
// 切り替えないのが仕様。以下の期待値はこの2式から直接導いている。

function checkHeightClamp() {
  expectEqual("クランプ: 上限値そのもの", MAX_TOTAL_HEIGHT_PX, 24_000_000);

  // 0行・1行（表示集合が空、または1件だけ）。理論高さは 0px・22px で上限未満。
  expectEqual("クランプ: 0行では切り替えない", isHeightScalingActive(0, ROW_HEIGHT_PX), false);
  expectEqual("クランプ: 0行の実効総高さ", computeEffectiveTotalHeightPx(0, ROW_HEIGHT_PX), 0);
  expectEqual("クランプ: 1行では切り替えない", isHeightScalingActive(1, ROW_HEIGHT_PX), false);
  expectEqual("クランプ: 1行の実効総高さ", computeEffectiveTotalHeightPx(1, ROW_HEIGHT_PX), 22);

  // 上限ちょうど／上限+1。行高1pxなら理論高さが行数と一致するため、等号の扱いを
  // 端数なしで検査できる（24,000,000 × 1px = 上限ちょうど）。
  expectEqual("クランプ: 上限ちょうどでは切り替えない", isHeightScalingActive(24_000_000, 1), false);
  expectEqual("クランプ: 上限ちょうどの実効総高さ", computeEffectiveTotalHeightPx(24_000_000, 1), 24_000_000);
  expectEqual("クランプ: 上限+1pxで切り替える", isHeightScalingActive(24_000_001, 1), true);
  expectEqual("クランプ: 上限+1pxの実効総高さ", computeEffectiveTotalHeightPx(24_000_001, 1), 24_000_000);

  // 実運用の行高（22px）での境界行数。
  //   floor(24,000,000 / 22) = 1,090,909 → 1,090,909 × 22 = 23,999,998px ≤ 上限
  //   1,090,910 × 22 = 24,000,020px > 上限
  expectEqual("クランプ: 22px行高の境界行数（上限以内）", isHeightScalingActive(1_090_909, ROW_HEIGHT_PX), false);
  expectEqual(
    "クランプ: 22px行高の境界行数（上限以内）の実効総高さ",
    computeEffectiveTotalHeightPx(1_090_909, ROW_HEIGHT_PX),
    23_999_998,
  );
  expectEqual("クランプ: 22px行高の境界行数+1で切り替える", isHeightScalingActive(1_090_910, ROW_HEIGHT_PX), true);
  expectEqual(
    "クランプ: 22px行高の境界行数+1の実効総高さ",
    computeEffectiveTotalHeightPx(1_090_910, ROW_HEIGHT_PX),
    24_000_000,
  );

  // 2000万行規模。理論高さ 20,000,000 × 22px = 440,000,000px で上限の18倍を超える。
  expectEqual("クランプ: 2000万行では切り替える", isHeightScalingActive(20_000_000, ROW_HEIGHT_PX), true);
  expectEqual(
    "クランプ: 2000万行の実効総高さ",
    computeEffectiveTotalHeightPx(20_000_000, ROW_HEIGHT_PX),
    24_000_000,
  );
}

// ---------------------------------------------------------------------------
// 2. 可視範囲（1:1方式。クランプ未満の通常規模）
// ---------------------------------------------------------------------------
//
// 仕様（`computeVisibleRange` の JSDoc と実装内コメント）:
//   firstVisibleRow = floor(max(0, scrollTop) / rowHeightPx)
//   visibleRowCount = ceil(max(0, viewportHeightPx) / rowHeightPx) + 1
//                     （下端に見切れる半端な1行を含めるため +1）
//   startIndex = max(0, firstVisibleRow - bufferRows)
//   endIndex   = min(totalItems, firstVisibleRow + visibleRowCount + bufferRows)
//   totalItems ≤ 0 または rowHeightPx ≤ 0 なら空範囲 {0, 0}
// 以下の期待値はこの4式へ数値を代入して手で計算した。
// この動作点では visibleRowCount = ceil(1080 / 22) + 1 = 50 + 1 = 51。

const VISIBLE_ROW_COUNT = Math.ceil(VIEWPORT_HEIGHT_PX / ROW_HEIGHT_PX) + 1;

function checkVisibleRangeOneToOne() {
  expectEqual("可視範囲: 動作点の可視行数（51 = ceil(1080/22)+1）", VISIBLE_ROW_COUNT, 51);

  const base = {
    viewportHeightPx: VIEWPORT_HEIGHT_PX,
    rowHeightPx: ROW_HEIGHT_PX,
    bufferRows: BUFFER_ROWS,
  };

  // 0行: 空範囲。
  expectRange("可視範囲: 0行は空範囲", computeVisibleRange({ ...base, scrollTop: 0, totalItems: 0 }), 0, 0);

  // 1行: startIndex = 0、endIndex = min(1, 0 + 51 + 50) = 1。
  expectRange("可視範囲: 1行は先頭のみ", computeVisibleRange({ ...base, scrollTop: 0, totalItems: 1 }), 0, 1);

  // 行高が0以下（styles.css との同期が崩れた場合の縮退）は空範囲。
  expectRange(
    "可視範囲: 行高0は空範囲",
    computeVisibleRange({ ...base, rowHeightPx: 0, scrollTop: 0, totalItems: 1000 }),
    0,
    0,
  );

  // 先頭: firstVisibleRow = 0 → {max(0, -50), min(1,090,909, 101)} = {0, 101}。
  expectRange(
    "可視範囲: 先頭（1:1方式）",
    computeVisibleRange({ ...base, scrollTop: 0, totalItems: 1_090_909 }),
    0,
    101,
  );

  // 中間: floor(100,000 / 22) = 4,545（22 × 4,545 = 99,990、余り10）。
  //   startIndex = 4,545 - 50 = 4,495、endIndex = 4,545 + 51 + 50 = 4,646。
  expectRange(
    "可視範囲: 中間（1:1方式）",
    computeVisibleRange({ ...base, scrollTop: 100_000, totalItems: 1_090_909 }),
    4_495,
    4_646,
  );

  // 末尾: 総高さ 23,999,998px（クランプ未満）に対する最大スクロール位置は
  //   23,999,998 − 1,080 = 23,998,918px。floor(23,998,918 / 22) = 1,090,859
  //   （22 × 1,090,859 = 23,998,898、余り20）。
  //   endIndex = min(1,090,909, 1,090,859 + 101) = 1,090,909 = 総行数（末尾へ到達）。
  expectRange(
    "可視範囲: 末尾（1:1方式。クランプ未満では末尾へ到達できる）",
    computeVisibleRange({ ...base, scrollTop: 23_998_918, totalItems: 1_090_909 }),
    1_090_809,
    1_090_909,
  );

  // 負のスクロール位置は0として扱う（max(0, scrollTop)）。
  expectRange(
    "可視範囲: 負のスクロール位置は先頭扱い",
    computeVisibleRange({ ...base, scrollTop: -1000, totalItems: 1_090_909 }),
    0,
    101,
  );
}

// ---------------------------------------------------------------------------
// 3. 比例写像（クランプ超過時の可視範囲）
// ---------------------------------------------------------------------------
//
// 仕様（`computeVisibleRangeForScroll`・`computeScaledScrollLayout` の JSDoc）:
//   クランプ未満 → `computeVisibleRange` へそのまま委譲（挙動を変えない）
//   クランプ超過 → `computeSpacerHeightsForScroll` が構成するレイアウトの逆写像。
//     T=総行数 h=行高 B=バッファ行数 E=実効総高さ C=ビューポート高
//     V = ceil(max(0, C) / h) + 1            （可視行数）
//     k = (E − (V + 2B)·h) / (T − (V + 2B))  （内側での未描画1行あたりの高さ）
//     先頭行 f に対する正準スクロール位置 S(f) は3領域の区分一次関数:
//       頭部 f ≤ B          : S = f·h
//       内側 B < f < T−V−B  : S = k·(f − B) + B·h
//       末尾 f ≥ T−V−B      : S = E − (T − f)·h
//     firstVisibleRow は S の逆写像（各領域で floor）を 0〜(T − V) へ切り詰めた値。
//     （maxScrollTopPx ≤ 0 なら 0、scrollTop ≥ maxScrollTopPx なら T − V に固定）
//   かつ「scrollTop === maxScrollTopPx で必ず endIndex === totalItems に到達する」
//   ことを保証すると明記されている。以下の期待値はこの式と保証から導いた。

const HUGE_TOTAL_ITEMS = 20_000_000;
// 2000万行では実効総高さが上限（24,000,000px）へクランプされるため、ブラウザが
// 返す最大スクロール位置は 24,000,000 − 1,080 = 23,998,920px になる。
const HUGE_MAX_SCROLL_TOP_PX = MAX_TOTAL_HEIGHT_PX - VIEWPORT_HEIGHT_PX;
// firstVisibleRow の上限 = 20,000,000 − 51 = 19,999,949。
const HUGE_MAX_FIRST_ROW = HUGE_TOTAL_ITEMS - VISIBLE_ROW_COUNT;

function scaledRange(scrollTop, overrides = {}) {
  return computeVisibleRangeForScroll({
    scrollTop,
    maxScrollTopPx: HUGE_MAX_SCROLL_TOP_PX,
    viewportHeightPx: VIEWPORT_HEIGHT_PX,
    rowHeightPx: ROW_HEIGHT_PX,
    totalItems: HUGE_TOTAL_ITEMS,
    bufferRows: BUFFER_ROWS,
    ...overrides,
  });
}

function checkProportionalMapping() {
  expectEqual("比例写像: 2000万行の最大スクロール位置", HUGE_MAX_SCROLL_TOP_PX, 23_998_920);
  expectEqual("比例写像: 2000万行の先頭行インデックス上限", HUGE_MAX_FIRST_ROW, 19_999_949);

  // 先頭: proportion = 0 → firstVisibleRow = 0 → {0, 101}。
  expectRange("比例写像: 先頭（2000万行）", scaledRange(0), 0, 101);

  // 末尾: proportion = 1 → floor(1 × 20,000,000) = 20,000,000 を上限で切り詰めて
  //   19,999,949 → startIndex = 19,999,899、endIndex = min(20,000,000, 20,000,050)
  //   = 20,000,000。JSDoc が保証する「末尾到達性」そのもの。
  expectRange(
    "比例写像: 末尾（2000万行。末尾到達性の保証）",
    scaledRange(HUGE_MAX_SCROLL_TOP_PX),
    19_999_899,
    20_000_000,
  );

  // 中央: scrollTop = 23,998,920 / 2 = 11,999,460。頭部の境界（B·h = 1,100）より
  //   大きく、末尾の境界（E − (V + B)·h = 24,000,000 − 2,222 = 23,997,778）より
  //   小さいので内側領域。逆写像は floor((scrollTop − 1,100) / k) + 50 で、
  //   k = (24,000,000 − 151 × 22) / (20,000,000 − 151) = 23,996,678 / 19,999,849。
  //     (11,999,460 − 1,100) × 19,999,849 / 23,996,678
  //       = 239,965,388,247,640 / 23,996,678
  //       = 10,000,000 − 1,391,752,360 / 23,996,678 = 10,000,000 − 57.997…
  //       = 9,999,942.002…  → floor = 9,999,942
  //   firstVisibleRow = 9,999,942 + 50 = 9,999,992。
  //   startIndex = 9,999,992 − 50、endIndex = 9,999,992 + 51 + 50。
  expectRange(
    "比例写像: 中央（2000万行）",
    scaledRange(HUGE_MAX_SCROLL_TOP_PX / 2),
    9_999_942,
    10_000_093,
  );

  // 範囲外のスクロール位置は proportion を 0〜1 に切り詰める。
  expectRange("比例写像: 負のスクロール位置は先頭", scaledRange(-5_000), 0, 101);
  expectRange(
    "比例写像: 最大値超えのスクロール位置は末尾",
    scaledRange(HUGE_MAX_SCROLL_TOP_PX * 2),
    19_999_899,
    20_000_000,
  );

  // maxScrollTopPx ≤ 0（レイアウト未確定などでスクロール不能）は常に先頭。
  expectRange(
    "比例写像: 最大スクロール位置0は先頭",
    scaledRange(1_000, { maxScrollTopPx: 0 }),
    0,
    101,
  );

  // 空・1行の縮退は1:1方式と同じく空範囲／先頭のみ。
  expectRange("比例写像: 0行は空範囲", scaledRange(0, { totalItems: 0 }), 0, 0);
  expectRange("比例写像: 1行は先頭のみ", scaledRange(0, { totalItems: 1 }), 0, 1);

  // クランプ未満では1:1方式へ委譲する（＝通常規模の挙動を変えない）。
  // 上限ちょうどの規模（1,090,909行 × 22px = 23,999,998px）で、両関数が完全に
  // 一致することを複数のスクロール位置で確認する。
  const boundaryTotal = 1_090_909;
  const boundaryMaxScrollTopPx = boundaryTotal * ROW_HEIGHT_PX - VIEWPORT_HEIGHT_PX;
  for (const scrollTop of [0, 1, 100_000, 12_000_000, boundaryMaxScrollTopPx]) {
    const oneToOne = computeVisibleRange({
      scrollTop,
      viewportHeightPx: VIEWPORT_HEIGHT_PX,
      rowHeightPx: ROW_HEIGHT_PX,
      totalItems: boundaryTotal,
      bufferRows: BUFFER_ROWS,
    });
    const forScroll = computeVisibleRangeForScroll({
      scrollTop,
      maxScrollTopPx: boundaryMaxScrollTopPx,
      viewportHeightPx: VIEWPORT_HEIGHT_PX,
      rowHeightPx: ROW_HEIGHT_PX,
      totalItems: boundaryTotal,
      bufferRows: BUFFER_ROWS,
    });
    expectRange(
      `比例写像: 上限ちょうどでは1:1方式へ委譲（scrollTop=${scrollTop}）`,
      forScroll,
      oneToOne.startIndex,
      oneToOne.endIndex,
    );
  }

  // 比例写像が必要な理由そのものの検査。2000万行では、ブラウザが返す最大
  // スクロール位置（23,998,920px）へ1:1方式（floor(scrollTop / 22)）を当てると
  // 22 × 1,090,860 = 23,998,920 ちょうどで firstVisibleRow = 1,090,860 にしか
  // ならず、endIndex は 1,090,961。総行数 20,000,000 の約5%の地点までしか進めず、
  // 末尾には到達できない（段階0検証 2.3節で観測された不具合の再現）。
  const oneToOneAtHugeEnd = computeVisibleRange({
    scrollTop: HUGE_MAX_SCROLL_TOP_PX,
    viewportHeightPx: VIEWPORT_HEIGHT_PX,
    rowHeightPx: ROW_HEIGHT_PX,
    totalItems: HUGE_TOTAL_ITEMS,
    bufferRows: BUFFER_ROWS,
  });
  expectRange(
    "比例写像: 1:1方式は2000万行の末尾へ到達できない（比例写像が必要な理由）",
    oneToOneAtHugeEnd,
    1_090_810,
    1_090_961,
  );

  // 連続性: スクロール位置が増えるとき、先頭行インデックスは決して逆行しない。
  // proportion が単調非減少で floor と min も単調なので、仕様上も成り立つ性質。
  let previousStart = -1;
  let monotonic = true;
  let boundsOk = true;
  const sampleCount = 512;
  for (let step = 0; step <= sampleCount; step += 1) {
    const range = scaledRange((HUGE_MAX_SCROLL_TOP_PX * step) / sampleCount);
    if (range.startIndex < previousStart) {
      monotonic = false;
    }
    if (
      range.startIndex < 0 ||
      range.endIndex > HUGE_TOTAL_ITEMS ||
      range.endIndex < range.startIndex
    ) {
      boundsOk = false;
    }
    previousStart = range.startIndex;
  }
  check("比例写像: スクロールに対して可視範囲が逆行しない（513点）", monotonic);
  check("比例写像: 可視範囲が常に 0 ≤ start ≤ end ≤ 総行数（513点）", boundsOk);

  // 往復（行インデックス → スクロール位置 → 行インデックス）。
  // `computeScrollTopForRowIndexScaled` は目標行そのものではなく
  // `JUMP_CONTEXT_ROWS` 行手前を先頭行として順写像へ通す（同定数の JSDoc）ため、
  // 戻ってくる先頭行は max(0, 行 − JUMP_CONTEXT_ROWS) である。
  // 誤差の上界は仕様から導ける: S(f) を戻すときの floor は、除算の丸め分だけ
  // 1行手前を指すことがある。したがって許容差は1行。両方の写像へ同じバッファ
  // 行数（ここでは0）を渡す。0にするのは startIndex = firstVisibleRow となり
  // 比較が直接書けるためで、実運用の動作点（50行）での一致は「ジャンプと描画
  // 位置の一致」で検査する。
  for (const rowIndex of [0, 1, 12_345, 10_000_000, 19_000_000, HUGE_MAX_FIRST_ROW]) {
    const scrollTop = computeScrollTopForRowIndexScaled(
      rowIndex,
      ROW_HEIGHT_PX,
      HUGE_TOTAL_ITEMS,
      HUGE_MAX_SCROLL_TOP_PX,
      0,
    );
    const range = scaledRange(scrollTop, { bufferRows: 0 });
    expectNear(
      `比例写像: 往復で文脈行ぶん手前の行へ戻る（行 ${rowIndex}）`,
      range.startIndex,
      Math.max(0, rowIndex - JUMP_CONTEXT_ROWS),
      1,
    );
  }

  // ジャンプ位置は 0 〜 maxScrollTopPx に収まる（範囲外の行番号を渡しても
  // ブラウザが受け付けない位置を返さない）。
  expectEqual(
    "比例写像: 先頭行のジャンプ位置は0",
    computeScrollTopForRowIndexScaled(0, ROW_HEIGHT_PX, HUGE_TOTAL_ITEMS, HUGE_MAX_SCROLL_TOP_PX),
    0,
  );
  expectEqual(
    "比例写像: 総行数を超える行のジャンプ位置は最大スクロール位置",
    computeScrollTopForRowIndexScaled(
      HUGE_TOTAL_ITEMS + 1_000,
      ROW_HEIGHT_PX,
      HUGE_TOTAL_ITEMS,
      HUGE_MAX_SCROLL_TOP_PX,
    ),
    HUGE_MAX_SCROLL_TOP_PX,
  );
  // クランプ未満では従来どおり「行インデックス × 行高」（1:1方式）。
  expectEqual(
    "比例写像: クランプ未満のジャンプ位置は行インデックス×行高",
    computeScrollTopForRowIndexScaled(1_000, ROW_HEIGHT_PX, 1_090_909, boundaryMaxScrollTopPx),
    22_000,
  );

  // 行番号入力のクランプ（`parseJumpTargetRowIndex`）。仕様は「1起点の入力を
  // 0起点へ直し、範囲外は先頭・末尾へ丸める。数値にならない入力は null」。
  expectEqual("クランプ: 行番号1は先頭", parseJumpTargetRowIndex("1", HUGE_TOTAL_ITEMS), 0);
  expectEqual("クランプ: 行番号0以下は先頭へ丸める", parseJumpTargetRowIndex("0", HUGE_TOTAL_ITEMS), 0);
  expectEqual(
    "クランプ: 総行数ちょうどは末尾",
    parseJumpTargetRowIndex("20000000", HUGE_TOTAL_ITEMS),
    19_999_999,
  );
  expectEqual(
    "クランプ: 総行数超えは末尾へ丸める",
    parseJumpTargetRowIndex("30000000", HUGE_TOTAL_ITEMS),
    19_999_999,
  );
  expectEqual("クランプ: 空入力は無効", parseJumpTargetRowIndex("   ", HUGE_TOTAL_ITEMS), null);
  expectEqual("クランプ: 0行のときは常に無効", parseJumpTargetRowIndex("1", 0), null);
}

// ---------------------------------------------------------------------------
// 4. スペーサ高さ（総高さの一定性）
// ---------------------------------------------------------------------------
//
// 仕様（`computeSpacerHeightsForScroll` の JSDoc）:
//   クランプ未満 → `computeSpacerHeights`（top = start × 行高、
//                   bottom = (総行数 − end) × 行高）へ委譲
//   クランプ超過 → 「topHeightPx + bottomHeightPx + 実描画行の高さ が、可視範囲の
//                   位置によらず常に厳密に実効総高さと一致する」
// この一定性が崩れると `viewport.scrollHeight` がフレームごとに揺れ、比例写像の
// 末尾到達判定を外す（段階0検証 10.6節で観測された非決定的な失敗）。以下は
// 上の等式そのものを検査する。

function checkSpacerHeights() {
  // クランプ未満は整数演算のみで、合計は総理論高さと厳密に一致する。
  const plain = computeSpacerHeights({
    startIndex: 4_495,
    endIndex: 4_646,
    totalItems: 1_090_909,
    rowHeightPx: ROW_HEIGHT_PX,
  });
  expectEqual("スペーサ: クランプ未満の上スペーサ（4,495 × 22）", plain.topHeightPx, 98_890);
  expectEqual(
    "スペーサ: クランプ未満の下スペーサ（(1,090,909 − 4,646) × 22）",
    plain.bottomHeightPx,
    (1_090_909 - 4_646) * ROW_HEIGHT_PX,
  );

  // クランプ未満では `computeSpacerHeightsForScroll` も同じ値を返す（委譲）。
  const delegated = computeSpacerHeightsForScroll({
    startIndex: 4_495,
    endIndex: 4_646,
    totalItems: 1_090_909,
    rowHeightPx: ROW_HEIGHT_PX,
  });
  expectEqual("スペーサ: クランプ未満は1:1方式へ委譲（上）", delegated.topHeightPx, plain.topHeightPx);
  expectEqual("スペーサ: クランプ未満は1:1方式へ委譲（下）", delegated.bottomHeightPx, plain.bottomHeightPx);

  // 2000万行規模での総高さの一定性。可視範囲の位置（先頭・中央・末尾）で実描画
  // 行数が変わっても、合計は常に実効総高さ（24,000,000px）でなければならない。
  // 許容差1e-6px の根拠: 24,000,000 前後の倍精度浮動小数点の刻みは約4e-9px で、
  // 検査対象の式に現れる除算は2回。丸め誤差は1e-8px 程度が上界であり、
  // 1e-6px はそれを2桁上回る余裕。一方 10.6節で問題になった揺れは数百〜数千px
  // 規模なので、この許容差でも退行は確実に捉えられる。
  const positions = [
    { name: "先頭", startIndex: 0, endIndex: 101 },
    { name: "中央", startIndex: 9_999_950, endIndex: 10_000_101 },
    { name: "末尾", startIndex: 19_999_899, endIndex: 20_000_000 },
    { name: "先頭付近（バッファ片側が欠ける位置）", startIndex: 10, endIndex: 111 },
  ];
  for (const position of positions) {
    const spacers = computeSpacerHeightsForScroll({
      startIndex: position.startIndex,
      endIndex: position.endIndex,
      totalItems: HUGE_TOTAL_ITEMS,
      rowHeightPx: ROW_HEIGHT_PX,
    });
    const renderedHeightPx = (position.endIndex - position.startIndex) * ROW_HEIGHT_PX;
    expectNear(
      `スペーサ: 総高さが実効総高さと一致する（${position.name}）`,
      spacers.topHeightPx + spacers.bottomHeightPx + renderedHeightPx,
      MAX_TOTAL_HEIGHT_PX,
      1e-6,
    );
    check(
      `スペーサ: 高さが負にならない（${position.name}）`,
      spacers.topHeightPx >= 0 && spacers.bottomHeightPx >= 0,
      `上 ${spacers.topHeightPx} / 下 ${spacers.bottomHeightPx}`,
    );
  }

  // 先頭では上スペーサが0px、末尾では下スペーサが0px。計測モードの末尾到達判定
  // （`measurement.js` の `reachedEndOfFile`＝下スペーサ0px）と同じ性質。
  const atStart = computeSpacerHeightsForScroll({
    startIndex: 0,
    endIndex: 101,
    totalItems: HUGE_TOTAL_ITEMS,
    rowHeightPx: ROW_HEIGHT_PX,
  });
  expectEqual("スペーサ: 先頭では上スペーサが0px", atStart.topHeightPx, 0);
  const atEnd = computeSpacerHeightsForScroll({
    startIndex: 19_999_899,
    endIndex: 20_000_000,
    totalItems: HUGE_TOTAL_ITEMS,
    rowHeightPx: ROW_HEIGHT_PX,
  });
  expectEqual("スペーサ: 末尾では下スペーサが0px", atEnd.bottomHeightPx, 0);

  // 可視範囲だけで表示集合全体を覆う縮退（行高が極端に大きくクランプが働く場合）。
  // 2行 × 25,000,000px = 50,000,000px > 上限、かつ描画行数 = 総行数。
  const covered = computeSpacerHeightsForScroll({
    startIndex: 0,
    endIndex: 2,
    totalItems: 2,
    rowHeightPx: 25_000_000,
  });
  expectEqual("スペーサ: 全体を描画済みなら上スペーサ不要", covered.topHeightPx, 0);
  expectEqual("スペーサ: 全体を描画済みなら下スペーサ不要", covered.bottomHeightPx, 0);
}

// ---------------------------------------------------------------------------
// 5. 行番号ジャンプと描画位置の一致（Issue #33）
// ---------------------------------------------------------------------------
//
// 3節は可視範囲の写像を、4節はスペーサ配分を、それぞれ単独で検査している。
// Issue #33 の不具合は、両者が個別には当時の仕様どおりでも、互いに別の一次式
// だったために起きた。行番号ジャンプで求めたスクロール位置に対してスペーサが
// 配る描画ブロックの上端がずれ、目標行が画面の外（2000万行では上方23行、
// 最大で46行）へ出ていた。行インデックスの往復（3節）は両者を組み合わせないため
// このずれを検出できない。そこでここでは、2つの写像を組み合わせた結果——実際に
// 画面へ出る画素の位置——だけを検査する。
//
// 仕様（`computeSpacerHeightsForScroll` の JSDoc）:
//   上スペーサの高さ = 描画ブロックの document 座標（先頭の描画行の上端の位置）
// したがって、あるスクロール位置で画面最上部に来る行は
//   topRow = startIndex + (scrollTop − topHeightPx) / rowHeightPx
// であり、これが `computeVisibleRangeForScroll` の firstVisibleRow と一致すること
// （2つの写像が同一のレイアウトから導かれること）が統一後の仕様である。以下は
// この等式から導かれる2つの帰結を検査する。
//
//   (a) ジャンプした目標行が描画ウィンドウ内に入り、かつ画面上端から
//       `JUMP_CONTEXT_ROWS` 行ぶん下（逆写像の floor による1行未満の誤差を許す）
//       に来ること。目標行が `JUMP_CONTEXT_ROWS` 行未満の先頭側では、スクロール
//       位置を0より手前へ動かせないため上端から「行インデックスぶん下」になる。
//       末尾1画面ぶんの行はスクロール位置が maxScrollTopPx で頭打ちになるため、
//       位置は指定できず「行全体が画面内」だけを期待する。いずれの場合も
//       「行全体が画面内」は共通の期待とする（利用者から見える保証そのもの）
//   (b) 画素側の不変条件。startIndex > 0（先頭でクリップされていない）なら
//       scrollTop − topHeightPx は必ず bufferRows × rowHeightPx 以上であり、
//       上界は (V + B) × h − C = 101 × 22 − 1,080 = 1,142px（scrollTop が
//       maxScrollTopPx のとき、先頭行が上限 T − V へ固定されて最大になる）
//
// あわせて、スクロールアンカリングの自走（Issue #21）が再発しないための性質——
// 同じ scrollTop に対してスペーサ高さが安定し、描画後の総高さから読み直した
// maxScrollTopPx で再計算しても同じ描画ウィンドウになること——も検査する。

// Issue #33 が実測を挙げた5つの規模。いずれも totalItems × 22px が
// 24,000,000px を超えるため、比例写像の経路を通る。
const SCALED_TOTALS = [2_000_000, 2_500_000, 5_000_000, 10_000_000, 20_000_000];

// 画素比較の許容差。位置の計算は除算を2回しか含まず、値の大きさは 24,000,000px
// 前後（倍精度の刻みは約4e-9px）なので、丸め誤差の上界は1e-8px 程度。1e-6px は
// それを2桁上回る余裕であり、検出したい「ずれ」（最小でも1行 = 22px）よりは
// 7桁小さいため、退行は確実に捉えられる。
const PIXEL_TOLERANCE = 1e-6;

/** 指定した規模での動作点（実効総高さと、ブラウザが返す最大スクロール位置）。 */
function scaledOperatingPoint(totalItems) {
  const effectiveHeightPx = computeEffectiveTotalHeightPx(totalItems, ROW_HEIGHT_PX);
  return {
    totalItems,
    effectiveHeightPx,
    maxScrollTopPx: effectiveHeightPx - VIEWPORT_HEIGHT_PX,
  };
}

/**
 * あるスクロール位置で実装が実際に構成する描画結果を、公開関数だけから組み立てる
 * （`log_view.js` の `renderVisibleRows` が DOM へ設定する内容と同じ組み合わせ）。
 * `topRow` は画面最上部に来る行（端数を含む実数）。
 */
function renderedAt(scale, scrollTop, maxScrollTopPx = scale.maxScrollTopPx) {
  const range = computeVisibleRangeForScroll({
    scrollTop,
    maxScrollTopPx,
    viewportHeightPx: VIEWPORT_HEIGHT_PX,
    rowHeightPx: ROW_HEIGHT_PX,
    totalItems: scale.totalItems,
    bufferRows: BUFFER_ROWS,
  });
  const spacers = computeSpacerHeightsForScroll({
    startIndex: range.startIndex,
    endIndex: range.endIndex,
    totalItems: scale.totalItems,
    rowHeightPx: ROW_HEIGHT_PX,
  });
  const renderedHeightPx = (range.endIndex - range.startIndex) * ROW_HEIGHT_PX;
  const offsetPx = scrollTop - spacers.topHeightPx;
  return {
    range,
    spacers,
    offsetPx,
    topRow: range.startIndex + offsetPx / ROW_HEIGHT_PX,
    totalHeightPx: spacers.topHeightPx + spacers.bottomHeightPx + renderedHeightPx,
  };
}

function checkJumpAlignment() {
  // 上界 1,142px の導出（4節の動作点と同じ V = 51、B = 50、h = 22、C = 1,080）。
  const maxOffsetPx = (VISIBLE_ROW_COUNT + BUFFER_ROWS) * ROW_HEIGHT_PX - VIEWPORT_HEIGHT_PX;
  expectEqual("ジャンプ: 画素オフセットの上界（(51+50)×22−1,080）", maxOffsetPx, 1_142);
  // 文脈行の余裕は、実機の量子化ずれ（±1.3行程度）を吸収しつつジャンプ先の
  // 視認性を損なわない値として2行に決めた（`JUMP_CONTEXT_ROWS` の JSDoc）。
  // 期待値をこの定数から組み立てるため、値そのものも表明しておく。
  expectEqual("ジャンプ: 文脈行の余裕は2行", JUMP_CONTEXT_ROWS, 2);

  for (const totalItems of SCALED_TOTALS) {
    const label = totalItems.toLocaleString("en-US");
    const scale = scaledOperatingPoint(totalItems);
    check(
      `ジャンプ: ${label}行では比例写像が有効（前提）`,
      isHeightScalingActive(totalItems, ROW_HEIGHT_PX),
      `理論高さ ${totalItems * ROW_HEIGHT_PX} / 上限 ${MAX_TOTAL_HEIGHT_PX}`,
    );

    // (a) 代表的な行番号ジャンプ。先頭付近（文脈行とバッファ境界の前後）、中央、
    //     Issue #33 がずれの実測を挙げた 3/4 付近、末尾領域の境界、末尾。
    const quarter = Math.floor(totalItems / 4);
    const threeQuarters = Math.floor((totalItems * 3) / 4);
    const tailStartRow = totalItems - VISIBLE_ROW_COUNT - BUFFER_ROWS;
    // 目標行の位置を指定できるのは、順写像へ通す先頭行
    // （max(0, 目標行 − JUMP_CONTEXT_ROWS)）のスクロール位置がスクロール範囲に
    // 収まる場合だけである。末尾領域の式 S(a) = E − (T − a)·h が
    // maxScrollTopPx = E − C 以下になる条件は (T − a)·h ≥ C、すなわち
    // a ≤ T − ceil(C / h) = T − 50。a = 目標行 − JUMP_CONTEXT_ROWS なので、
    // 目標行 ≤ T − 50 + 2 = T − 48。これより後ろの行（末尾1画面ぶん）は
    // スクロールが頭打ちになるため「行全体が画面内」だけを期待する。
    // 実装が返す scrollTop ではなく仕様から決めるのは、頭打ちの判定そのものを
    // 実装に委ねると、頭打ちにしない実装を見逃してしまうため。
    const lastPlaceableTarget =
      totalItems - Math.ceil(VIEWPORT_HEIGHT_PX / ROW_HEIGHT_PX) + JUMP_CONTEXT_ROWS;
    const targets = [
      // 0〜3 は文脈行の余裕を取れない／取れるようになる境界（JUMP_CONTEXT_ROWS = 2）。
      0,
      1,
      JUMP_CONTEXT_ROWS,
      JUMP_CONTEXT_ROWS + 1,
      BUFFER_ROWS - 1,
      BUFFER_ROWS,
      BUFFER_ROWS + 1,
      1_000,
      quarter,
      Math.floor(totalItems / 2),
      threeQuarters - 1,
      threeQuarters,
      tailStartRow - 1,
      tailStartRow,
      totalItems - VISIBLE_ROW_COUNT,
      totalItems - 1,
    ];

    let outsideWindow = null;
    // ずれは規模と行位置によって大きさが変わるため、最初の1件ではなく最大の
    // 1件を残す（Issue #33 の特徴である「末尾へ向かうほど広がるずれ」が、
    // 失敗時のメッセージからそのまま読み取れるようにする）。
    let notAtContext = null;
    let notAtContextPx = 0;
    let notFullyVisible = null;
    for (const target of targets) {
      const scrollTop = computeScrollTopForRowIndexScaled(
        target,
        ROW_HEIGHT_PX,
        totalItems,
        scale.maxScrollTopPx,
        BUFFER_ROWS,
      );
      const rendered = renderedAt(scale, scrollTop);
      // 目標行の上端・下端の、ビューポート上端からの画素距離。
      const rowTopPx = (target - rendered.topRow) * ROW_HEIGHT_PX;
      const rowBottomPx = rowTopPx + ROW_HEIGHT_PX;
      if (target < rendered.range.startIndex || target >= rendered.range.endIndex) {
        outsideWindow ??= `行 ${target} は描画ウィンドウ ${format(rendered.range)} の外`;
      }
      if (target <= lastPlaceableTarget) {
        // 目標行の位置を指定できる範囲。期待は「上端から JUMP_CONTEXT_ROWS 行
        // ぶん下」だが、先頭側（目標行 < JUMP_CONTEXT_ROWS）はスクロール位置を
        // 0より手前へ動かせないため「上端から行インデックスぶん下」になる。
        // 逆写像の floor による端数（1行未満）だけさらに下へずれることは許す。
        const expectedTopPx = Math.min(target, JUMP_CONTEXT_ROWS) * ROW_HEIGHT_PX;
        const deviationPx = rowTopPx - expectedTopPx;
        if (
          !(deviationPx >= -PIXEL_TOLERANCE && deviationPx < ROW_HEIGHT_PX) &&
          Math.abs(deviationPx) > notAtContextPx
        ) {
          notAtContextPx = Math.abs(deviationPx);
          notAtContext =
            `行 ${target} は上端から ${rowTopPx.toFixed(1)}px（期待 ${expectedTopPx}px）で、` +
            `${(deviationPx / ROW_HEIGHT_PX).toFixed(1)} 行ずれている`;
        }
      }
      // 行全体が画面内（0px 〜 1,080px）に収まることは、位置を指定できるかどうかに
      // よらず共通の期待。末尾1画面ぶんではこれだけが保証される。
      if (
        !(
          rowTopPx >= -PIXEL_TOLERANCE &&
          rowBottomPx <= VIEWPORT_HEIGHT_PX + PIXEL_TOLERANCE
        )
      ) {
        notFullyVisible ??= `行 ${target} は上端 ${rowTopPx.toFixed(3)}px・下端 ${rowBottomPx.toFixed(3)}px で画面外`;
      }
    }
    check(`ジャンプ: 目標行が描画ウィンドウ内（${label}行）`, outsideWindow === null, outsideWindow);
    check(
      `ジャンプ: 目標行が上端から${JUMP_CONTEXT_ROWS}行下（誤差1行未満。${label}行）`,
      notAtContext === null,
      notAtContext,
    );
    check(
      `ジャンプ: 目標行の全体が画面内（${label}行）`,
      notFullyVisible === null,
      notFullyVisible,
    );

    // (b) 画素側の不変条件と、再描画に対する安定性。スクロール範囲全体を
    //     等間隔（先頭・末尾を含む257点）で走査する。
    let offsetOutOfRange = null;
    let offsetBelowBuffer = null;
    let heightDrift = null;
    let unstable = null;
    let regressed = null;
    let previousStart = -1;
    const sampleCount = 256;
    for (let step = 0; step <= sampleCount; step += 1) {
      const scrollTop = (scale.maxScrollTopPx * step) / sampleCount;
      const rendered = renderedAt(scale, scrollTop);
      if (
        !(
          rendered.offsetPx >= -PIXEL_TOLERANCE &&
          rendered.offsetPx <= maxOffsetPx + PIXEL_TOLERANCE
        )
      ) {
        offsetOutOfRange ??= `scrollTop=${scrollTop} で ${rendered.offsetPx}px（許容 0〜${maxOffsetPx}px）`;
      }
      if (
        rendered.range.startIndex > 0 &&
        rendered.offsetPx < BUFFER_ROWS * ROW_HEIGHT_PX - PIXEL_TOLERANCE
      ) {
        offsetBelowBuffer ??= `scrollTop=${scrollTop} で ${rendered.offsetPx}px（下限 ${BUFFER_ROWS * ROW_HEIGHT_PX}px）`;
      }
      if (Math.abs(rendered.totalHeightPx - scale.effectiveHeightPx) > PIXEL_TOLERANCE) {
        heightDrift ??= `scrollTop=${scrollTop} で総高さ ${rendered.totalHeightPx}`;
      }
      // 描画後の総高さから maxScrollTopPx を読み直しても同じ描画ウィンドウに
      // なること（`log_view.js` の `computeCurrentRenderTargets` は毎回
      // DOM の scrollHeight/clientHeight から読み直す）。ここが崩れると、
      // 再描画のたびに描画位置が動き続ける（Issue #21 の自走）。
      const reread = renderedAt(
        scale,
        scrollTop,
        rendered.totalHeightPx - VIEWPORT_HEIGHT_PX,
      );
      if (
        reread.range.startIndex !== rendered.range.startIndex ||
        reread.range.endIndex !== rendered.range.endIndex ||
        reread.spacers.topHeightPx !== rendered.spacers.topHeightPx
      ) {
        unstable ??= `scrollTop=${scrollTop} で ${format(rendered.range)} → ${format(reread.range)}`;
      }
      if (rendered.range.startIndex < previousStart) {
        regressed ??= `scrollTop=${scrollTop} で ${previousStart} → ${rendered.range.startIndex}`;
      }
      previousStart = rendered.range.startIndex;
    }
    check(
      `ジャンプ: scrollTop − 上スペーサが 0〜${maxOffsetPx}px（257点。${label}行）`,
      offsetOutOfRange === null,
      offsetOutOfRange,
    );
    check(
      `ジャンプ: 先頭でクリップされていなければ scrollTop − 上スペーサ ≥ バッファ高（257点。${label}行）`,
      offsetBelowBuffer === null,
      offsetBelowBuffer,
    );
    check(
      `ジャンプ: 総高さが実効総高さのまま動かない（257点。${label}行）`,
      heightDrift === null,
      heightDrift,
    );
    check(
      `ジャンプ: 再描画で描画ウィンドウとスペーサが変わらない（257点。${label}行）`,
      unstable === null,
      unstable,
    );
    check(
      `ジャンプ: スクロールに対して可視範囲が逆行しない（257点。${label}行）`,
      regressed === null,
      regressed,
    );
  }
}

// ---------------------------------------------------------------------------
// 6. 破棄判定（保持上限 `CFG-022`、`PERF-012`）
// ---------------------------------------------------------------------------
//
// 仕様（`selectChunksToEvict` の JSDoc）:
//   - 保持行数・保持バイト数がどちらも上限以下なら何も破棄しない（空配列）
//   - 超過している場合、保護チャンク（可視範囲＋バッファがカバーする範囲）を
//     除き、基準行インデックスからチャンク中心が遠い順に破棄する
//   - 同距離のときは chunkIndex 昇順（呼び出しのたびに結果が決定的になるため）
//   - 上限を満たした時点で止める（必要以上に破棄しない）
//   - 保護チャンクだけで上限を超える場合は、上限を超えたままの結果を返し得る
//   - チャンク中心 = chunkIndex × chunkSize + chunkSize / 2
// 以下の期待値は、この規則へ数値を代入して手で並べ替えた結果である。

/** チャンク記述子の配列から保持行数・保持バイト数の合計を出す（後条件の確認用）。 */
function totalsOf(chunks, evicted = new Set()) {
  let rows = 0;
  let bytes = 0;
  for (const chunk of chunks) {
    if (evicted.has(chunk.chunkIndex)) continue;
    rows += chunk.rowCount;
    bytes += chunk.byteCount;
  }
  return { rows, bytes };
}

function checkEviction() {
  // --- 小さな手計算ケース ---
  // チャンクサイズ10、チャンク0〜4が各10行・各100バイト。基準行は25。
  // 中心は 5 / 15 / 25 / 35 / 45、基準25からの距離は 20 / 10 / 0 / 10 / 20。
  // 保護は{2}。破棄順は距離降順＋同距離は番号昇順で [0, 4, 1, 3]。
  const small = [0, 1, 2, 3, 4].map((chunkIndex) => ({
    chunkIndex,
    rowCount: 10,
    byteCount: 100,
  }));
  const protectedSmall = new Set([2]);
  const referenceRowIndex = 25;
  const smallChunkSize = 10;

  // 上限ちょうど（50行・500バイト）は「上限以下」なので破棄しない。
  expectIndices(
    "破棄判定: 上限ちょうどでは破棄しない",
    selectChunksToEvict(small, protectedSmall, referenceRowIndex, smallChunkSize, {
      maxRows: 50,
      maxBytes: 500,
    }),
    [],
  );

  // 行数超過（上限25行）: 50 → 40（0を破棄）→ 30（4）→ 20（1）で上限以下。
  // 3 は破棄しない（20 ≤ 25 で停止するため）。
  const byRows = selectChunksToEvict(small, protectedSmall, referenceRowIndex, smallChunkSize, {
    maxRows: 25,
    maxBytes: 10_000,
  });
  expectIndices("破棄判定: 行数超過の破棄対象と順序", byRows, [0, 4, 1]);
  expectEqual("破棄判定: 行数超過の破棄後の保持行数", totalsOf(small, new Set(byRows)).rows, 20);
  // 最小限しか破棄していないこと（最後の1件を戻すと上限を超える）。
  expectEqual(
    "破棄判定: 行数超過で1件少ないと上限を超える（過剰破棄していない）",
    totalsOf(small, new Set(byRows.slice(0, -1))).rows,
    30,
  );

  // バイト数超過のみ（行数は上限内）: 500 → 400（0）→ 300（4）で 350 以下。
  // 保持バイト数も判定に効いていることの確認（`CFG-022` は2軸）。
  const byBytes = selectChunksToEvict(small, protectedSmall, referenceRowIndex, smallChunkSize, {
    maxRows: 10_000,
    maxBytes: 350,
  });
  expectIndices("破棄判定: バイト数超過の破棄対象と順序", byBytes, [0, 4]);
  expectEqual("破棄判定: バイト数超過の破棄後の保持バイト数", totalsOf(small, new Set(byBytes)).bytes, 300);

  // 保護チャンクは破棄されない。
  check(
    "破棄判定: 保護チャンクを破棄しない",
    !byRows.includes(2) && !byBytes.includes(2),
    `行数超過 ${format(byRows)} / バイト数超過 ${format(byBytes)}`,
  );

  // 全チャンクが保護されている場合は破棄できず、上限超過のまま空配列を返す
  // （JSDoc が明記する縮退動作。無限に破棄を試み続けない）。
  expectIndices(
    "破棄判定: 全チャンクが保護されていれば破棄しない",
    selectChunksToEvict(small, new Set([0, 1, 2, 3, 4]), referenceRowIndex, smallChunkSize, {
      maxRows: 1,
      maxBytes: 1,
    }),
    [],
  );

  // キャッシュが空なら破棄も発生しない。
  expectIndices(
    "破棄判定: キャッシュが空なら破棄しない",
    selectChunksToEvict([], new Set(), 0, smallChunkSize, { maxRows: 0, maxBytes: 0 }),
    [],
  );

  // --- 2000万行規模のケース ---
  // 末尾へジャンプした直後の可視範囲を保護し、そこへ至るまでに取得した200個の
  // チャンクがキャッシュに残っている状態を再現する。
  const hugeRange = scaledRange(HUGE_MAX_SCROLL_TOP_PX);
  const required = computeRequiredChunkIndices(
    hugeRange.startIndex,
    hugeRange.endIndex,
    CHUNK_SIZE,
    HUGE_TOTAL_ITEMS,
  );
  // 可視範囲 19,999,899〜20,000,000 は、512 × 39,062 = 19,999,744 で始まる末尾
  // チャンク1個に収まる（末尾行 19,999,999 も同じチャンク）。
  expectIndices("破棄判定: 末尾の可視範囲が必要とするチャンク", required, [39_062]);
  // 末尾チャンクは総行数で切り詰められ、20,000,000 − 19,999,744 = 256 行。
  const lastChunkRange = computeChunkRange(39_062, CHUNK_SIZE, HUGE_TOTAL_ITEMS);
  expectEqual("破棄判定: 末尾チャンクの開始行", lastChunkRange.start, 19_999_744);
  expectEqual("破棄判定: 末尾チャンクの行数", lastChunkRange.count, 256);

  // チャンク 38,863〜39,062 の200個。末尾チャンクだけ256行、他は512行。
  // 1行あたり80バイトとすると合計は約7.8 MiB で、64 MiB の上限には届かない
  // （＝この事例は行数上限だけで破棄が決まる）。
  const hugeChunks = [];
  for (let chunkIndex = 38_863; chunkIndex <= 39_062; chunkIndex += 1) {
    const rowCount = chunkIndex === 39_062 ? 256 : CHUNK_SIZE;
    hugeChunks.push({ chunkIndex, rowCount, byteCount: rowCount * 80 });
  }
  const before = totalsOf(hugeChunks);
  expectEqual("破棄判定: 2000万行ケースの破棄前の保持行数", before.rows, 102_144);
  check(
    "破棄判定: 2000万行ケースはバイト数上限に達していない（行数上限だけで決まる）",
    before.bytes <= DEFAULT_MAX_BYTES,
    `保持バイト数 ${before.bytes} / 上限 ${DEFAULT_MAX_BYTES}`,
  );

  // 基準行は可視範囲の中心 floor((19,999,899 + 20,000,000) / 2) = 19,999,949
  // （`log_view.js` の `evictFarChunks` と同じ求め方）。距離が最大なのは番号が
  // 最小のチャンクなので、破棄順は 38,863 から昇順に進む。
  // 102,144 行を 10,000 行以下にするには 92,144 行を落とす必要があり、
  // 512行チャンクで ceil(92,144 / 512) = 180 個。破棄後は
  // 102,144 − 180 × 512 = 9,984 行（179個では 10,496 行で上限超過）。
  const hugeReference = Math.floor((hugeRange.startIndex + hugeRange.endIndex) / 2);
  expectEqual("破棄判定: 2000万行ケースの基準行", hugeReference, 19_999_949);
  const hugeEvicted = selectChunksToEvict(
    hugeChunks,
    new Set(required),
    hugeReference,
    CHUNK_SIZE,
    { maxRows: DEFAULT_MAX_ROWS, maxBytes: DEFAULT_MAX_BYTES },
  );
  expectEqual("破棄判定: 2000万行ケースの破棄数", hugeEvicted.length, 180);
  expectEqual("破棄判定: 2000万行ケースで最初に破棄するチャンク", hugeEvicted[0], 38_863);
  expectEqual("破棄判定: 2000万行ケースで最後に破棄するチャンク", hugeEvicted[179], 39_042);
  const after = totalsOf(hugeChunks, new Set(hugeEvicted));
  expectEqual("破棄判定: 2000万行ケースの破棄後の保持行数", after.rows, 9_984);
  check(
    "破棄判定: 2000万行ケースで保持上限（CFG-022）を満たす",
    after.rows <= DEFAULT_MAX_ROWS && after.bytes <= DEFAULT_MAX_BYTES,
    `保持行数 ${after.rows} / 保持バイト数 ${after.bytes}`,
  );
  check(
    "破棄判定: 2000万行ケースで表示中のチャンクを破棄しない",
    !hugeEvicted.includes(39_062),
    `破棄一覧に ${39_062} が含まれている`,
  );

  // バイト数上限だけで破棄が決まる事例。行数は 10 × 512 = 5,120 行で上限内だが、
  // 1チャンク10 MiB × 10 個 = 100 MiB で 64 MiB を超える。
  // 100 − 64 = 36 MiB を落とすには 10 MiB チャンクが4個必要（3個では 70 MiB）。
  const fatChunks = [];
  for (let chunkIndex = 0; chunkIndex < 10; chunkIndex += 1) {
    fatChunks.push({ chunkIndex, rowCount: CHUNK_SIZE, byteCount: 10 * 1024 * 1024 });
  }
  const fatEvicted = selectChunksToEvict(fatChunks, new Set([9]), 9 * CHUNK_SIZE, CHUNK_SIZE, {
    maxRows: DEFAULT_MAX_ROWS,
    maxBytes: DEFAULT_MAX_BYTES,
  });
  expectEqual("破棄判定: バイト数上限だけで決まる破棄数", fatEvicted.length, 4);
  expectIndices("破棄判定: バイト数上限だけで決まる破棄順（遠い順）", fatEvicted, [0, 1, 2, 3]);
  const fatAfter = totalsOf(fatChunks, new Set(fatEvicted));
  check(
    "破棄判定: バイト数上限を満たす",
    fatAfter.bytes <= DEFAULT_MAX_BYTES,
    `保持バイト数 ${fatAfter.bytes} / 上限 ${DEFAULT_MAX_BYTES}`,
  );

  // 決定性: 同じ入力なら結果も同じ（同距離のタイブレークが番号昇順で固定される）。
  const repeated = selectChunksToEvict(
    hugeChunks,
    new Set(required),
    hugeReference,
    CHUNK_SIZE,
    { maxRows: DEFAULT_MAX_ROWS, maxBytes: DEFAULT_MAX_BYTES },
  );
  expectIndices("破棄判定: 同じ入力に対して結果が決定的", repeated, hugeEvicted);
}

// ---------------------------------------------------------------------------
// 7. 必要チャンクの選定（表示範囲と破棄判定をつなぐ部分）
// ---------------------------------------------------------------------------
//
// 仕様（`computeRequiredChunkIndices`・`computeChunkRange` の JSDoc）:
//   必要チャンク = 可視範囲の先頭行が属するチャンクから、終端の1つ前の行が
//   属するチャンクまでの連番（昇順・重複なし）。範囲が空なら空配列。
//   チャンク範囲は総項目数でクリップする。

function checkRequiredChunks() {
  expectIndices(
    "必要チャンク: 範囲が空なら空配列",
    computeRequiredChunkIndices(100, 100, CHUNK_SIZE, HUGE_TOTAL_ITEMS),
    [],
  );
  expectIndices(
    "必要チャンク: 総項目数0なら空配列",
    computeRequiredChunkIndices(0, 100, CHUNK_SIZE, 0),
    [],
  );
  // 行0〜511 は 512 で割ると全てチャンク0、行512 からチャンク1。
  expectIndices(
    "必要チャンク: チャンク境界をまたぐ範囲",
    computeRequiredChunkIndices(511, 513, CHUNK_SIZE, HUGE_TOTAL_ITEMS),
    [0, 1],
  );
  // 先頭の可視範囲（0〜101）はチャンク0だけに収まる。
  expectIndices(
    "必要チャンク: 先頭の可視範囲",
    computeRequiredChunkIndices(0, 101, CHUNK_SIZE, HUGE_TOTAL_ITEMS),
    [0],
  );
  // 総項目数を超える終端はクリップされる。
  expectIndices(
    "必要チャンク: 総項目数を超える終端はクリップする",
    computeRequiredChunkIndices(19_999_900, 20_000_500, CHUNK_SIZE, HUGE_TOTAL_ITEMS),
    [39_062],
  );
  // 開始位置が総項目数以上のチャンクは取得不要（count 0）。
  const beyond = computeChunkRange(39_063, CHUNK_SIZE, HUGE_TOTAL_ITEMS);
  expectEqual("必要チャンク: 総項目数の外側は取得不要", beyond.count, 0);
}

// ---------------------------------------------------------------------------
// 8. 前提の同期（`src/log_view.js` の動作点）
// ---------------------------------------------------------------------------
//
// 上の境界値は `log_view.js` の定数（行高22px、チャンク512行、バッファ50行）を
// 動作点として選んでいる。これらは `log_view.js` の内部定数で import できない
// ため、値がずれたまま検査だけが通り続けることを防ぐ目的で、ソースから読み出して
// 突き合わせる。ずれた場合は、この検査の境界値も選び直す必要がある。

function checkPremises() {
  const source = readFileSync(resolve(ROOT, "src", "log_view.js"), "utf8");
  const readConstant = (name) => {
    const matched = new RegExp(`^const ${name} = ([0-9_]+);$`, "m").exec(source);
    return matched === null ? null : Number(matched[1].replace(/_/g, ""));
  };
  expectEqual("前提: log_view.js の ROW_HEIGHT_PX", readConstant("ROW_HEIGHT_PX"), ROW_HEIGHT_PX);
  expectEqual("前提: log_view.js の CHUNK_SIZE", readConstant("CHUNK_SIZE"), CHUNK_SIZE);
  expectEqual("前提: log_view.js の BUFFER_ROWS", readConstant("BUFFER_ROWS"), BUFFER_ROWS);

  // `handleJumpRequest` は `computeScrollTopForRowIndexScaled` へバッファ行数を
  // 渡していないため、既定値（`DEFAULT_BUFFER_ROWS`）が使われる。この既定値が
  // `log_view.js` の `BUFFER_ROWS` とずれると、比例写像が有効な規模でジャンプ
  // 位置が静かにずれる（バッファ行数1行あたり約2行、50行ずれれば約95行）。
  // 5節の検査は動作点の値を明示的に渡しているため、このずれは検出できない。
  expectEqual(
    "前提: log_view.js の BUFFER_ROWS と virtual_scroll.js の DEFAULT_BUFFER_ROWS が一致",
    DEFAULT_BUFFER_ROWS,
    readConstant("BUFFER_ROWS"),
  );
}

// ---------------------------------------------------------------------------
// 実行
// ---------------------------------------------------------------------------

checkHeightClamp();
checkVisibleRangeOneToOne();
checkProportionalMapping();
checkSpacerHeights();
checkJumpAlignment();
checkEviction();
checkRequiredChunks();
checkPremises();

if (problems.length > 0) {
  console.error(`仮想スクロールの規模依存ロジックに問題が ${problems.length} 件あります。\n`);
  for (const problem of problems) console.error(`  ${problem}`);
  console.error(
    "\n判定方法と対象の一覧は docs/verification/regression-checks.md を参照してください。",
  );
  process.exit(1);
}

console.log(
  `仮想スクロールの規模依存ロジック（クランプ、比例写像、ジャンプと描画位置の一致、破棄判定）を ${checkCount} 項目検査しました。問題はありません。`,
);
