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

export async function run({ page, expect }) {
  // 規約3: 最小のサンプル（200行）を使う。
  const target = SAMPLE_TARGETS.MILLISECOND;
  await openTargetByName(page, target, { requireTimestamp: true });

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
  // （`src/log_view.js` の `handleRowsClick` がフォーカスを移す）。
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

  // 後続シナリオが「そのシナリオで出た通知」だけを見られるよう片付ける
  // （同一内容のバナーは閉じるまで回数が積み上がる。Issue #11）。
  await dismissBanners(page);
  expect.expectEqual("確認後にバナーを片付けられる", (await readBanners(page)).length, 0);
}
