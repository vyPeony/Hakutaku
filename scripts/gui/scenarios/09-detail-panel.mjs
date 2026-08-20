// シナリオ9: 継続行バッジから詳細パネルを開閉する（`LOG-014`）。
//
// 日時を持たない継続行は直前の項目へ結合され、行一覧には1行目だけが表示される
// （折りたたみ方式。`src/virtual_scroll.js` 冒頭のコメント）。結合された全文を
// 改行を保ったまま確認する唯一の経路が、継続行バッジ → 下部の詳細パネルである。
//
// バッジは行ごとにイベントリスナーを持たず、`#log-rows` への単一の委譲で処理する
// （`PERF-012`: DOM に行データを保持しない）。委譲が壊れるとバッジを押しても何も
// 起きなくなり、静的検査では検出できない。
//
// # 見た目の非表示も表明する理由（Issue #78）
//
// このシナリオは当初 `hidden` プロパティだけを表明していた。しかし
// `src/styles.css` の作成者スタイル `.log-detail-panel { display: flex }` が
// UA スタイルシートの `[hidden] { display: none }` を必ず上書きするため（作成者
// オリジンは詳細度に関係なく UA に勝つ）、**`hidden` が真のままパネルが画面に
// 出続ける**という不具合が起きていた。`hidden` の表明はすべて成功していたため、
// この検査では見逃していた。同じ見落としを繰り返さないよう、開閉のたびに計算後の
// `display` も確認する。

import {
  SAMPLE_TARGETS,
  openTargetByName,
  readRenderedRows,
  readViewState,
} from "../app.mjs";

export const name = "継続行バッジと詳細パネル";

/**
 * 詳細パネルの計算後の `display` を読む（`hidden` プロパティとは別に、実際に
 * 画面へ出ているかを見るため。Issue #78）。
 *
 * @param {import("playwright-core").Page} page
 */
function readDetailPanelDisplay(page) {
  return page.evaluate(() => {
    const panel = document.querySelector("#log-detail-panel");
    return panel === null ? "（要素がありません）" : getComputedStyle(panel).display;
  });
}

export async function run({ page, expect }) {
  const target = SAMPLE_TARGETS.CONTINUATION;
  await openTargetByName(page, target);

  const initialView = await readViewState(page);
  expect.expectEqual("詳細パネルは既定で閉じている", initialView.detailPanelHidden, true);
  expect.expectEqual(
    "Issue #78: 既定では詳細パネルが画面にも出ていない（display: none）",
    await readDetailPanelDisplay(page),
    "none",
  );

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
  const openedDisplay = await readDetailPanelDisplay(page);
  expect.check(
    "開いている間は詳細パネルが画面に出ている（display が none ではない）",
    openedDisplay !== "none",
    `display ${JSON.stringify(openedDisplay)}`,
  );
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
  expect.expectEqual(
    "Issue #78: 閉じたあとは詳細パネルが画面からも消える（display: none）",
    await readDetailPanelDisplay(page),
    "none",
  );
  expect.expectEqual("閉じたあとは本文を保持しない", closedView.detailPanelBody, "");
}
