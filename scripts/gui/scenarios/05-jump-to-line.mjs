// シナリオ5: 行番号でジャンプ（正常値と範囲外の丸め）。
//
// ジャンプ先は「表示集合内のインデックス」であり、原本の行番号ではない
// （`src/virtual_scroll.js` の `parseJumpTargetRowIndex` の JSDoc）。ここで使う
// サンプルは継続行を含まないため両者は一致し、行番号列の表示でそのまま照合できる。
//
// 丸めの仕様は同関数が定める「1起点の入力を0起点へ直し、範囲外は先頭・末尾へ
// 丸める」。丸めた結果は入力欄へ書き戻される（`src/log_view.js` の
// `handleJumpRequest`）ため、利用者から見える丸めの結果を入力欄の値で表明できる。
//
// 直前のシナリオ4が開いた10万行のファイルをそのまま使う（同じ表示集合に対する
// 続きの操作であり、開き直す必要がない）。

import {
  SAMPLE_TARGETS,
  parseItemCount,
  readRenderedRows,
  readViewState,
  waitForRowsSettled,
} from "../app.mjs";

export const name = "行番号ジャンプ";

/** `10-medium-100k.log` の生成行数（`scripts/generate-sample-logs.ps1`）。 */
const TOTAL_ITEMS = 100_000;

/** 途中の行へのジャンプ。先頭・末尾のどちらの境界からも十分離れた値。 */
const MIDDLE_LINE_NUMBER = 50_000;

/**
 * 行番号を入力してジャンプし、再描画が落ち着くまで待つ。
 *
 * @param {import("playwright-core").Page} page
 * @param {string} value 入力欄へ入れる文字列（範囲外の値も渡せるよう文字列で受ける）
 */
async function jumpTo(page, value) {
  await page.fill("#log-jump-input", value);
  await page.click("#log-jump-button");
  await waitForRowsSettled(page);
}

export async function run({ page, expect }) {
  const view = await readViewState(page);
  const premisesOk = expect.check(
    "前提: シナリオ4が開いた10万行のファイルが表示されたままである",
    view.sourceLabel.includes(SAMPLE_TARGETS.LARGE) &&
      parseItemCount(view.totalItemsText) === TOTAL_ITEMS,
    `読み込み元 ${JSON.stringify(view.sourceLabel)} / 件数 ${JSON.stringify(view.totalItemsText)}`,
  );
  if (!premisesOk) {
    return;
  }

  // --- 正常値 ---
  await jumpTo(page, String(MIDDLE_LINE_NUMBER));
  const middleRows = await readRenderedRows(page);
  const middleView = await readViewState(page);
  expect.check(
    `行 ${MIDDLE_LINE_NUMBER} へのジャンプで、その行が描画範囲に入る`,
    middleRows.some((row) => row.lineNumber === String(MIDDLE_LINE_NUMBER)),
    `描画範囲 ${JSON.stringify(middleRows[0]?.lineNumber)}〜${JSON.stringify(
      middleRows.at(-1)?.lineNumber,
    )}`,
  );
  expect.expectEqual(
    "範囲内の入力は丸められず、入力欄の値がそのまま残る",
    middleView.jumpInputValue,
    String(MIDDLE_LINE_NUMBER),
  );

  // --- 範囲外（上限超え）---
  await jumpTo(page, "999999999");
  const clampedEndRows = await readRenderedRows(page);
  const clampedEndView = await readViewState(page);
  expect.expectEqual(
    "総行数を超える入力は末尾へ丸められ、入力欄へ丸めた結果が書き戻される",
    clampedEndView.jumpInputValue,
    String(TOTAL_ITEMS),
  );
  expect.expectEqual(
    "末尾へ丸めた結果、最終行が描画範囲の末尾になる",
    clampedEndRows.at(-1)?.lineNumber,
    String(TOTAL_ITEMS),
  );

  // --- 範囲外（下限割れ）---
  await jumpTo(page, "0");
  const clampedStartRows = await readRenderedRows(page);
  const clampedStartView = await readViewState(page);
  expect.expectEqual(
    "0以下の入力は先頭へ丸められ、入力欄へ丸めた結果（1）が書き戻される",
    clampedStartView.jumpInputValue,
    "1",
  );
  expect.expectEqual(
    "先頭へ丸めた結果、1行目が描画範囲の先頭になる",
    clampedStartRows[0]?.lineNumber,
    "1",
  );
}
