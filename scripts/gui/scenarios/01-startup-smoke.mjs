// シナリオ1: 起動スモーク。
//
// ウィンドウが生成され、共通シェルが描画され、設定の読み込み経路（`CFG-014`）が
// 正常起動であることを確認する。以降のシナリオはすべて「設定由来のデータソースが
// 左ペインに並んでいる」ことを前提にするため、その前提自体をここで表明する。
//
// `#open-file-button` は可視であることだけを確認し、クリックしない
// （`scripts/gui/app.mjs` の規約1）。

import { readBanners, readTargetRows } from "../app.mjs";

export const name = "起動スモーク";

export async function run({ page, app, expect }) {
  expect.expectEqual("ウィンドウのタイトルが Hakutaku である", await page.title(), "Hakutaku");

  expect.check(
    "「ファイルを開く」ボタンが可視である（PROD-016。規約1によりクリックはしない）",
    await page.locator("#open-file-button").isVisible(),
  );

  // 設定の起動経路（`CFG-014`／`CFG-015`／`CFG-016`）。実バックエンドの Tauri
  // コマンドをそのまま呼ぶ。`scripts/check-gui.mjs` が実行ファイル直下へ正常な
  // `hakutaku.yaml` を配置しているため、期待する経路は `loaded` だけである。
  const status = await page.evaluate(() =>
    window.__TAURI_INTERNALS__.invoke("get_config_status"),
  );
  expect.expectEqual("get_config_status が loaded 経路を返す（CFG-014）", status?.route, "loaded");
  expect.expectAtLeast(
    "設定由来のデータソースが1件以上ある（CFG-003）",
    status?.data_source_names?.length ?? 0,
    1,
  );

  const targetRows = await readTargetRows(page);
  expect.expectEqual(
    "左ペインの件数が data_source_names と一致する（CFG-003、PROD-006）",
    targetRows.length,
    status?.data_source_names?.length ?? -1,
  );
  // 左ペイン再設計（Issue #97）により、行は「ファイル名＋必要時だけ右端の
  // 短い状態表示」の1行表示（由来の印は表示しない）。状態は data-status-kind
  // から読む。
  expect.check(
    "起動直後はどの対象も未読み込み（not_opened）である",
    targetRows.length > 0 && targetRows.every((row) => row.kind === "not_opened"),
    `状態の内訳 ${JSON.stringify([...new Set(targetRows.map((row) => row.kind))])}`,
  );
  expect.check(
    "起動直後は短い状態表示（バッジ）もタブの強調も出ていない",
    targetRows.every((row) => row.badge === "" && !row.open && !row.current),
    `行の内訳 ${JSON.stringify(targetRows.slice(0, 5))}`,
  );

  // 正常起動（`loaded`）では通知を出さない（`src/main.js` の
  // `showBannerForConfigStatus`）。既定値起動（`CFG-015`）や安全モード
  // （`CFG-016`）と取り違えていないことの確認でもある。
  const banners = await readBanners(page);
  expect.check(
    "正常起動では通知バナーが1件も出ない（CFG-015／CFG-016 のどちらでもない）",
    banners.length === 0,
    `表示されていたバナー ${JSON.stringify(banners)}`,
  );

  // CDP は起動後に接続するため、接続より前に出力された分は原理的に拾えない
  // （この制約は docs/verification/regression-checks.md に記載している）。
  expect.check(
    "起動時の JS コンソールエラーが0件",
    app.consoleErrors.length === 0,
    `コンソールエラー ${JSON.stringify(app.consoleErrors.slice(0, 5))}`,
  );
  expect.check(
    "起動時の未捕捉例外が0件",
    app.pageErrors.length === 0,
    `未捕捉例外 ${JSON.stringify(app.pageErrors.slice(0, 5))}`,
  );
}
