// シナリオ6: 行の選択とコピー（`COPY-001`、`COPY-002`）。
//
// **このシナリオは OS のクリップボードを上書きする**（`scripts/gui/app.mjs` の
// 規約3）。利用者がコピー中の内容を壊す量を最小にするため、生成されるサンプルの
// うち最小のファイル（200行）だけを対象にする。
//
// 選択（`COPY-001`）は DOM の `.log-row--selected` で、コピーの成否
// （`COPY-002`）は完了通知バナー（`src/log_view.js` の `handleCopyRequest`）で
// 確認する。クリップボードの中身そのものは読み取らない。読み取りには追加の権限と
// 別経路が要るうえ、書き込みの正しさは Rust 側の単体テストが受け持つ範囲であり、
// GUI 検査で押さえたいのは「操作から通知までの経路がつながっていること」だから。
//
// # Issue #85 の経緯
//
// ツールバーの「コピー列」（行番号／日時／本文のチェックボックス）と quoted TSV
// は廃止し、Ctrl+C は**常に選択行の原文そのまま**をコピーする（ADR-0011）。
// 代わりに選択操作を広げた（Ctrl+クリックによる飛び飛びの選択、ドラッグによる
// 範囲選択）。このシナリオは、従来の Ctrl+A → Ctrl+C に加えて、新しい2つの
// 選択操作が実際のイベント（`mousedown`／`mousemove`／`mouseup`）で機能し、
// 飛び飛びの選択でもコピー完了通知の行数が選択合計と一致することを確認する。
//
// 選択の純粋ロジック（範囲の分割・結合、クランプ）そのものは
// `scripts/check-selection.mjs` が CI で検査する。ここで見たいのは、その
// モデルと DOM イベント・IPC のつなぎ目である。

import {
  SAMPLE_TARGETS,
  dismissBanners,
  openTargetByName,
  parseItemCount,
  readBanners,
  readRenderedRows,
  readViewState,
} from "../app.mjs";

export const name = "選択とコピー（COPY-001／COPY-002）";

/** コピー完了通知の本文（`src/log_view.js` の `handleCopyRequest`）。 */
const COPY_DONE_TEXT = "をクリップボードへコピーしました。";

/**
 * 描画済みの行のうち、選択されているものの `data-row-index` を昇順で読み出す。
 *
 * `readRenderedRows` は選択の有無しか返さないため、飛び飛びの選択で
 * 「どの行が」選ばれているかを確認するにはこちらを使う。
 *
 * @param {import("playwright-core").Page} page
 */
function readSelectedRowIndices(page) {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll("#log-rows .log-row--selected"))
      .map((row) => Number(row.dataset.rowIndex))
      .sort((a, b) => a - b),
  );
}

/**
 * 描画済みの行の中心座標（ビューポート座標）を返す。実際のマウス操作
 * （`page.mouse`）でドラッグするために必要。
 *
 * @param {import("playwright-core").Page} page
 * @param {number} renderedIndex 描画済みの行の並び（0起点。表示集合の
 *   インデックスではない）
 */
function readRowCenter(page, renderedIndex) {
  return page.evaluate((index) => {
    const row = document.querySelectorAll("#log-rows .log-row")[index];
    const rect = row.getBoundingClientRect();
    return { x: rect.left + 8, y: rect.top + rect.height / 2 };
  }, renderedIndex);
}

export async function run({ page, expect }) {
  // 規約3: 最小のサンプル（200行）を使う。
  const target = SAMPLE_TARGETS.MILLISECOND;
  await openTargetByName(page, target);

  // このシナリオで出た通知だけを見るため、先に画面上部を片付ける。
  await dismissBanners(page);

  const view = await readViewState(page);
  const totalItems = parseItemCount(view.totalItemsText);
  expect.expectAtMost(
    "前提: クリップボードを上書きする対象が最小のサンプル（1,000行未満）である",
    totalItems,
    1_000,
  );

  // --- 選択（COPY-001）---
  // 行クリックでビューポートへフォーカスが移り、そのあと Ctrl+A が効く
  // （`src/log_view.js` の `handleRowsMouseDown` がフォーカスを移す）。
  await page.locator("#log-rows .log-row").first().click();
  await page.keyboard.press("Control+a");
  await page.waitForFunction(
    () =>
      document.querySelectorAll("#log-rows .log-row--selected").length ===
      document.querySelectorAll("#log-rows .log-row").length,
    undefined,
    { timeout: 10_000 },
  );

  const selectedRows = await readRenderedRows(page);
  expect.check(
    "COPY-001: Ctrl+A で描画済みの全行が選択状態になる",
    selectedRows.length > 0 && selectedRows.every((row) => row.selected),
    `描画行数 ${selectedRows.length} / 選択行数 ${selectedRows.filter((row) => row.selected).length}`,
  );

  // --- コピー（COPY-002）---
  await page.keyboard.press("Control+c");
  await page.waitForFunction(
    (doneText) =>
      Array.from(document.querySelectorAll("#config-banners .config-banner")).some((banner) =>
        (banner.textContent ?? "").includes(doneText),
      ),
    COPY_DONE_TEXT,
    { timeout: 15_000 },
  );

  const banners = await readBanners(page);
  const copyBanner = banners.find((banner) => banner.text.includes(COPY_DONE_TEXT));
  expect.check(
    "COPY-002: Ctrl+C でコピー完了の通知が出る",
    copyBanner !== undefined,
    `バナー ${JSON.stringify(banners)}`,
  );
  expect.expectEqual("コピー完了の通知が情報バナーである（エラーではない）", copyBanner?.kind, "info");
  expect.expectContains(
    "コピー完了の通知が、選択した全行の件数を示す",
    copyBanner?.text ?? "",
    `${totalItems.toLocaleString("ja-JP")} 行`,
  );

  await dismissBanners(page);

  // --- 飛び飛びの選択（Ctrl+クリック。Issue #85）---
  // 離れた2行（描画済みの0行目と5行目）を Ctrl+クリックで選ぶ。1回目の
  // Ctrl+クリックは、直前の Ctrl+A で選択済みの行を**外す**方向に働くため、
  // 先に修飾キーなしのクリックで選択を1行へ畳んでから始める。
  await page.locator("#log-rows .log-row").nth(0).click();
  await page.locator("#log-rows .log-row").nth(5).click({ modifiers: ["Control"] });
  await page.waitForFunction(
    () => document.querySelectorAll("#log-rows .log-row--selected").length === 2,
    undefined,
    { timeout: 10_000 },
  );
  const scattered = await readSelectedRowIndices(page);
  expect.check(
    "COPY-001: Ctrl+クリックで離れた2行だけが選択される（飛び飛びの選択）",
    scattered.length === 2 && scattered[0] === 0 && scattered[1] === 5,
    `選択された行 ${JSON.stringify(scattered)}`,
  );

  // --- 飛び飛びの選択のコピー（通知の行数が選択合計と一致する）---
  await page.keyboard.press("Control+c");
  await page.waitForFunction(
    (doneText) =>
      Array.from(document.querySelectorAll("#config-banners .config-banner")).some((banner) =>
        (banner.textContent ?? "").includes(doneText),
      ),
    COPY_DONE_TEXT,
    { timeout: 15_000 },
  );
  const scatteredBanner = (await readBanners(page)).find((banner) =>
    banner.text.includes(COPY_DONE_TEXT),
  );
  expect.expectContains(
    "COPY-002: 飛び飛びの選択でも、完了通知の行数が選択合計（2行）と一致する",
    scatteredBanner?.text ?? "",
    "2 行",
  );
  await dismissBanners(page);

  // --- Ctrl+クリックによる除外（トグル）---
  await page.locator("#log-rows .log-row").nth(5).click({ modifiers: ["Control"] });
  await page.waitForFunction(
    () => document.querySelectorAll("#log-rows .log-row--selected").length === 1,
    undefined,
    { timeout: 10_000 },
  );
  const afterUntoggle = await readSelectedRowIndices(page);
  expect.check(
    "COPY-001: 選択済みの行を Ctrl+クリックすると選択から外れる",
    afterUntoggle.length === 1 && afterUntoggle[0] === 0,
    `選択された行 ${JSON.stringify(afterUntoggle)}`,
  );

  // --- ドラッグによる範囲選択（Issue #85）---
  // 実際のマウスイベント（mousedown → mousemove → mouseup）で数行ドラッグする。
  // クリックの合成（locator.click）では mousemove を挟まないため、ドラッグ経路
  // （`handleDragSelectionMove`）を通せない。
  const dragFrom = await readRowCenter(page, 2);
  const dragTo = await readRowCenter(page, 6);
  await page.mouse.move(dragFrom.x, dragFrom.y);
  await page.mouse.down();
  // 途中の座標も通す（1回の move で飛ばすと、実機のドラッグと違う経路になる）。
  await page.mouse.move(dragFrom.x, (dragFrom.y + dragTo.y) / 2);
  await page.mouse.move(dragTo.x, dragTo.y);
  await page.mouse.up();
  await page.waitForFunction(
    () => document.querySelectorAll("#log-rows .log-row--selected").length === 5,
    undefined,
    { timeout: 10_000 },
  );
  const dragged = await readSelectedRowIndices(page);
  const isContiguous = dragged.every((rowIndex, index) => rowIndex === dragged[0] + index);
  expect.check(
    "COPY-001: ドラッグで開始行から終了行までの連続範囲が選択される",
    dragged.length === 5 && isContiguous && dragged[0] === 2 && dragged[4] === 6,
    `選択された行 ${JSON.stringify(dragged)}`,
  );

  // 後続シナリオが「そのシナリオで出た通知」だけを見られるよう片付ける
  // （同一内容のバナーは閉じるまで回数が積み上がる。Issue #11）。
  await dismissBanners(page);
  expect.expectEqual("確認後にバナーを片付けられる", (await readBanners(page)).length, 0);
}
