// シナリオ4: 仮想スクロール（`PERF-012`）。
//
// 10万行のファイルを全域までスクロールしても、DOM に実在する行ノードの数が
// 上限を超えないことを表明する。`PERF-012` が禁じるのは「表示集合の規模に比例して
// DOM ノードが増える」ことであり、増え方が線形なら数万行の時点で操作不能になる。
//
// 上限値は固定の魔法の数ではなく、`src/log_view.js` の定数（行高・バッファ行数）と
// 実際のビューポート高さから導く。`computeVisibleRange` の仕様（同関数の JSDoc）は
//
//   可視行数 = ceil(ビューポート高さ / 行高) + 1
//   描画行数 = 可視行数 + 前後バッファ（bufferRows × 2）
//
// であり、描画行数はこの値を超えない。定数を `src/log_view.js` から読み出すのは、
// 実装側の値が変わったときに検査だけが古い前提のまま通り続けることを防ぐため
// （`scripts/check-virtual-scroll.mjs` の「前提の同期」と同じ考え方）。
//
// 実行時間・スクロール応答は一切表明しない（`VER-005`）。

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import {
  SAMPLE_TARGETS,
  openTargetByName,
  parseItemCount,
  readRenderedRows,
  readViewState,
  scrollViewportToRatio,
} from "../app.mjs";

export const name = "仮想スクロール（PERF-012）";

/** `10-medium-100k.log` の生成行数（`scripts/generate-sample-logs.ps1`）。 */
const EXPECTED_ITEM_COUNT = 100_000;

/** 先頭から末尾まで、途中を含めて確認するスクロール位置（割合）。 */
const SCROLL_RATIOS = [0, 0.25, 0.5, 0.75, 1, 0];

const ROOT = resolve(import.meta.dirname, "..", "..", "..");

/**
 * `src/log_view.js` の数値定数を読み出す。`export` されていないため import では
 * 取れず、ソースから読む（`scripts/check-virtual-scroll.mjs` の `checkPremises`
 * と同じ方法）。
 *
 * @param {string} source
 * @param {string} constantName
 */
function readConstant(source, constantName) {
  const matched = new RegExp(`^const ${constantName} = ([0-9_]+);$`, "m").exec(source);
  return matched === null ? null : Number(matched[1].replace(/_/g, ""));
}

export async function run({ page, expect }) {
  const logViewSource = readFileSync(resolve(ROOT, "src", "log_view.js"), "utf8");
  const rowHeightPx = readConstant(logViewSource, "ROW_HEIGHT_PX");
  const bufferRows = readConstant(logViewSource, "BUFFER_ROWS");
  const premisesOk = expect.check(
    "前提: src/log_view.js から行高とバッファ行数を読み出せる",
    Number.isFinite(rowHeightPx) && rowHeightPx > 0 && Number.isFinite(bufferRows),
    `ROW_HEIGHT_PX=${rowHeightPx} / BUFFER_ROWS=${bufferRows}`,
  );
  if (!premisesOk) {
    // 上限値を導けない以上、この先の表明は意味を持たない。
    return;
  }

  const target = SAMPLE_TARGETS.LARGE;
  await openTargetByName(page, target, { requireTimestamp: true, timeoutMs: 90_000 });

  const opened = await readViewState(page);
  expect.expectEqual(
    "10万行のファイルを開ける（PERF-007）",
    parseItemCount(opened.totalItemsText),
    EXPECTED_ITEM_COUNT,
  );

  // 描画行数の上限。ビューポート高さは実行環境のウィンドウ寸法で決まるため、
  // 実測値から毎回導く。
  const maxRenderedRows =
    Math.ceil(opened.viewportClientHeight / rowHeightPx) + 1 + bufferRows * 2;
  expect.expectAtLeast(
    "前提: ビューポートに高さがある（ウィンドウが生成されている）",
    opened.viewportClientHeight,
    1,
  );

  /** @type {{ratio: number, rowCount: number, firstLineNumber: string, lastLineNumber: string, scrollTop: number}[]} */
  const samples = [];
  for (const ratio of SCROLL_RATIOS) {
    await scrollViewportToRatio(page, ratio);
    const view = await readViewState(page);
    const rows = await readRenderedRows(page);
    samples.push({
      ratio,
      rowCount: view.renderedRowCount,
      firstLineNumber: rows[0]?.lineNumber ?? "",
      lastLineNumber: rows.at(-1)?.lineNumber ?? "",
      scrollTop: view.viewportScrollTop,
    });
  }

  expect.check(
    `PERF-012: どのスクロール位置でも DOM 行ノード数が上限（${maxRenderedRows}）を超えない`,
    samples.every((sample) => sample.rowCount > 0 && sample.rowCount <= maxRenderedRows),
    `行ノード数 ${JSON.stringify(samples.map((sample) => sample.rowCount))}`,
  );

  // 「上限を超えない」だけでは、そもそもスクロールできていない場合も通ってしまう。
  // 実際に表示位置が動いていることを併せて確かめる。
  const head = samples[0];
  const tail = samples[samples.length - 2];
  expect.check(
    "末尾までスクロールすると表示位置が先頭から動く",
    tail.scrollTop > head.scrollTop && tail.firstLineNumber !== head.firstLineNumber,
    `先頭 ${JSON.stringify(head)} / 末尾 ${JSON.stringify(tail)}`,
  );
  expect.expectEqual(
    "末尾までスクロールすると表示集合の最終行へ到達する",
    tail.lastLineNumber,
    String(EXPECTED_ITEM_COUNT),
  );

  // 末尾まで往復したあとで先頭へ戻しても、破棄されずに残った行が積み上がって
  // いないこと（`CFG-022` の保持上限と `PERF-012` の破棄が働いていること）。
  const returned = samples[samples.length - 1];
  expect.expectEqual("先頭へ戻すと先頭行が1へ戻る", returned.firstLineNumber, "1");
  expect.expectAtMost(
    "先頭へ戻したあとも DOM 行ノード数が上限を超えない",
    returned.rowCount,
    maxRenderedRows,
  );
}
