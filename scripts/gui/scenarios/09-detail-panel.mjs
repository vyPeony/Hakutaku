// シナリオ9: 継続行バッジから詳細パネルを開閉する（`LOG-014`）。
//
// 日時を持たない継続行は直前の項目へ結合され、行一覧には1行目だけが表示される
// （折りたたみ方式。`src/virtual_scroll.js` 冒頭のコメント）。結合された全文を
// 改行を保ったまま確認する唯一の経路が、継続行バッジ → 下部の詳細パネルである。
//
// バッジは行ごとにイベントリスナーを持たず、`#log-rows` への単一の委譲で処理する
// （`PERF-012`: DOM に行データを保持しない）。委譲が壊れるとバッジを押しても何も
// 起きなくなり、静的検査では検出できない。

import {
  SAMPLE_TARGETS,
  openTargetByName,
  readRenderedRows,
  readViewState,
} from "../app.mjs";

export const name = "継続行バッジと詳細パネル";

export async function run({ page, expect }) {
  const target = SAMPLE_TARGETS.CONTINUATION;
  await openTargetByName(page, target, { requireTimestamp: true });

  const initialView = await readViewState(page);
  expect.expectEqual("詳細パネルは既定で閉じている", initialView.detailPanelHidden, true);

  // サンプルは継続行の混入率2割・乱数の種を固定して生成されるため
  // （`scripts/generate-sample-logs.ps1`）、先頭の描画範囲に必ずバッジが現れる。
  const badge = page.locator("#log-rows .log-row__badge--continuation").first();
  await badge.waitFor({ state: "visible", timeout: 15_000 });

  const rowsBefore = await readRenderedRows(page);
  const badgeRow = rowsBefore.find((row) => row.continuationBadge !== null);
  expect.check(
    "LOG-014: 継続行を含む行にバッジが出る",
    badgeRow !== undefined,
    `描画行数 ${rowsBefore.length}`,
  );
  expect.expectMatch(
    "バッジが結合された継続行の数を示す",
    badgeRow?.continuationBadge ?? "",
    /^\+\d+行$/,
  );

  // --- 開く ---
  await badge.click({ timeout: 10_000 });
  await page.waitForFunction(
    () => document.querySelector("#log-detail-panel")?.hidden === false,
    undefined,
    { timeout: 10_000 },
  );

  const openedView = await readViewState(page);
  expect.expectEqual("バッジのクリックで詳細パネルが開く", openedView.detailPanelHidden, false);
  expect.expectContains(
    "詳細パネルの見出しが、含まれる継続行の数を示す",
    openedView.detailPanelTitle,
    "継続行",
  );
  expect.check(
    "LOG-014: 詳細パネルの本文が改行を保った全文である（行一覧の1行表示と異なる）",
    openedView.detailPanelBody.includes("\n"),
    `本文 ${JSON.stringify(openedView.detailPanelBody.slice(0, 120))}`,
  );

  // --- 閉じる ---
  await page.click("#log-detail-panel-close");
  await page.waitForFunction(
    () => document.querySelector("#log-detail-panel")?.hidden === true,
    undefined,
    { timeout: 10_000 },
  );

  const closedView = await readViewState(page);
  expect.expectEqual("閉じるボタンで詳細パネルが閉じる", closedView.detailPanelHidden, true);
  expect.expectEqual("閉じたあとは本文を保持しない", closedView.detailPanelBody, "");
}
