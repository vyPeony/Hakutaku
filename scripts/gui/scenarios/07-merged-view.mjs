// シナリオ7: 複数タブと時系列統合表示の ON/OFF（`LOG-007`、`LOG-008`、`LOG-015`）。
//
// 統合表示は「開いている全ソースを横断する1つの表示集合」を作り、タブは統合タブ
// 1つだけになる（`LOG-015`: 分割表示は作らない）。OFF に戻すと、直前にアクティブ
// だったファイル別タブへ戻る（`src/shell.js` の `handleMergedViewToggleClick`）。
//
// 後続シナリオ（タブを閉じる）は統合表示が OFF であることを前提にするため、
// このシナリオは必ず OFF へ戻して終わる。

import {
  SAMPLE_TARGETS,
  openTargetByName,
  parseItemCount,
  readRenderedRows,
  readRowSignature,
  readTabs,
  readViewState,
  waitForLogViewReady,
} from "../app.mjs";

export const name = "複数タブと時系列統合表示";

/**
 * 統合表示のトグルを押し、押下状態が期待どおりになるまで待つ。
 *
 * トグルは処理中 `disabled` になり、完了時にラベルと `aria-pressed` が更新される
 * （`updateMergedViewToggleLabel`）。押下状態を待つことで、統合表示集合の構築
 * （Tauri コマンド）の完了まで待てる。
 *
 * @param {import("playwright-core").Page} page
 * @param {boolean} expectedPressed
 */
async function toggleMergedView(page, expectedPressed) {
  await page.click("#merged-view-toggle");
  await page.waitForFunction(
    (pressed) =>
      document.querySelector("#merged-view-toggle")?.getAttribute("aria-pressed") === pressed,
    String(expectedPressed),
    { timeout: 30_000 },
  );
}

export async function run({ page, expect }) {
  // --- 複数タブ ---
  for (const target of [SAMPLE_TARGETS.MERGE_A, SAMPLE_TARGETS.MERGE_B]) {
    await openTargetByName(page, target, { requireTimestamp: true });
  }

  const tabsBefore = await readTabs(page);
  expect.expectAtLeast("複数のファイルを開くとタブが並ぶ（LOG-015）", tabsBefore.length, 2);
  for (const target of [SAMPLE_TARGETS.MERGE_A, SAMPLE_TARGETS.MERGE_B]) {
    expect.check(
      `タブに「${target}」がある`,
      tabsBefore.some((tab) => tab.title === target),
      `タブ ${JSON.stringify(tabsBefore.map((tab) => tab.title))}`,
    );
  }
  const singleFileItemCount = parseItemCount((await readViewState(page)).totalItemsText);

  // --- 統合表示 ON ---
  // 統合表示への切り替えも、読み込み元ラベルが先に更新されてから行が描画される
  // （`src/log_view.js` の `activate`）。切り替え前の指紋を渡し、前のファイルの
  // 行が残っている一瞬を掴まないようにする。
  const beforeMerged = await readRowSignature(page);
  await toggleMergedView(page, true);
  await waitForLogViewReady(page, {
    sourceLabel: "時系列統合",
    previousSignature: beforeMerged,
    requireTimestamp: true,
  });

  const mergedView = await readViewState(page);
  expect.expectEqual("統合表示 ON のラベルになる", mergedView.mergedToggleLabel, "時系列統合: ON");
  expect.expectEqual("統合表示 ON で aria-pressed が true になる", mergedView.mergedTogglePressed, "true");
  expect.expectEqual("統合表示のビューは読み込み元ラベルが「時系列統合」になる", mergedView.sourceLabel, "時系列統合");

  const mergedTabs = await readTabs(page);
  expect.expectEqual("LOG-015: 統合表示ではタブが1つだけになる（分割表示を作らない）", mergedTabs.length, 1);
  expect.expectEqual("統合タブの見出しが「時系列統合」である", mergedTabs[0]?.title, "時系列統合");
  expect.expectEqual(
    "統合タブには閉じるボタンが無い（OFF はトグルで行う）",
    mergedTabs[0]?.closable,
    false,
  );

  const mergedItemCount = parseItemCount(mergedView.totalItemsText);
  expect.check(
    "統合表示の件数が、単一ファイルの件数より多い（全ソースを横断している。LOG-006）",
    Number.isFinite(mergedItemCount) && mergedItemCount > singleFileItemCount,
    `統合 ${mergedItemCount} / 単一 ${singleFileItemCount}`,
  );

  // 統合表示では行ごとに読み込み元ラベル列が出る（`LOG-007`）。複数のソースの行が
  // 実際に混ざって並んでいることを、描画済みの行に現れるラベルの種類数で見る。
  const mergedRows = await readRenderedRows(page);
  expect.check(
    "LOG-007: 統合表示の行に読み込み元ラベル列が出る",
    mergedRows.length > 0 && mergedRows.every((row) => (row.sourceLabel ?? "").length > 0),
    `先頭行 ${JSON.stringify(mergedRows[0])}`,
  );
  const distinctSources = new Set(mergedRows.map((row) => row.sourceLabel));
  expect.expectAtLeast(
    "LOG-006／LOG-008: 描画範囲に複数のソース由来の行が混在する",
    distinctSources.size,
    2,
  );

  // --- 統合表示 OFF ---
  const beforeRestore = await readRowSignature(page);
  await toggleMergedView(page, false);
  await waitForLogViewReady(page, {
    sourceLabel: SAMPLE_TARGETS.MERGE_B,
    previousSignature: beforeRestore,
    requireTimestamp: true,
  });

  const restoredView = await readViewState(page);
  expect.expectEqual("統合表示 OFF のラベルへ戻る", restoredView.mergedToggleLabel, "時系列統合: OFF");
  expect.check(
    "OFF に戻すとファイル別タブの表示へ戻る",
    restoredView.sourceLabel !== "時系列統合" && restoredView.sourceLabel.length > 0,
    `読み込み元 ${JSON.stringify(restoredView.sourceLabel)}`,
  );
  const restoredTabs = await readTabs(page);
  expect.expectAtLeast("OFF に戻すとファイル別タブが並び直す", restoredTabs.length, 2);
  expect.check(
    "OFF に戻すと統合タブが無くなる",
    restoredTabs.every((tab) => !tab.merged),
    `タブ ${JSON.stringify(restoredTabs.map((tab) => tab.title))}`,
  );
}
