// シナリオ2: 設定由来のデータソースを開く（`CFG-003`、`LOG-001`、`LOG-020`）。
//
// 左ペインのクリックから、実ファイルの読み込み・解析・表示までを一気通貫で
// 確認する。ネイティブのファイル選択ダイアログ（規約1）を使わずに実ファイルを
// 開ける唯一の経路であり、以降のシナリオもすべてこの経路に依存する。

import {
  SAMPLE_TARGETS,
  openTargetByName,
  parseItemCount,
  readRenderedRows,
  readTabs,
  readTargetRows,
  readViewState,
} from "../app.mjs";

export const name = "設定由来データソースを開く";

/** `01-basic-utf8.log` の生成行数（`scripts/generate-sample-logs.ps1`）。 */
const EXPECTED_ITEM_COUNT = 2_000;

export async function run({ page, expect }) {
  const target = SAMPLE_TARGETS.BASIC;

  await openTargetByName(page, target, { requireTimestamp: true });

  const view = await readViewState(page);
  expect.expectEqual("ツールバーの読み込み元ラベルが対象の表示名になる", view.sourceLabel, target);
  expect.expectEqual(
    "表示集合の件数が生成した行数と一致する（LOG-001）",
    parseItemCount(view.totalItemsText),
    EXPECTED_ITEM_COUNT,
  );

  const tabs = await readTabs(page);
  const tab = tabs.find((entry) => entry.title === target);
  expect.check("開いた対象のタブができる（LOG-015）", tab !== undefined, `タブ ${JSON.stringify(tabs)}`);
  expect.expectEqual("開いたタブが選択状態になる", tab?.selected, true);
  expect.expectEqual("ファイル別タブには閉じるボタンがある", tab?.closable, true);

  // 左ペインの状態表示は、読み込み完了で「読み込み済み（<行数> 行）」へ変わる
  // （`src/shell.js` の `STATUS_LABELS` と `statusLabelFor`）。行数の表記まで
  // 固定すると表示の細部に縛られるため、状態の種別だけを前方一致で見る。
  // タブと左ペインが同じ実態を指していることの確認でもある。
  const targetRow = (await readTargetRows(page)).find((row) => row.name === target);
  expect.check(
    "左ペインの状態が「読み込み済み」になる",
    targetRow?.status?.startsWith("読み込み済み") === true,
    `左ペインの状態 ${JSON.stringify(targetRow?.status)}`,
  );

  // 行の中身。先頭行は表示集合の1件目であり、行番号は原本の行番号
  // （`source_line_number`）をそのまま表示する。
  const rows = await readRenderedRows(page);
  expect.expectAtLeast("行が1件以上描画される", rows.length, 1);
  expect.expectEqual("先頭行の行番号が1である", rows[0]?.lineNumber, "1");
  expect.check(
    "先頭行の本文が空でもプレースホルダーでもない（LOG-020）",
    (rows[0]?.text?.length ?? 0) > 0 && rows[0]?.text !== "（読み込み中…）",
    `先頭行の本文 ${JSON.stringify(rows[0]?.text)}`,
  );
  expect.expectEqual(
    "ファイル別タブでは読み込み元ラベル列を出さない（統合表示専用の列。LOG-007）",
    rows[0]?.sourceLabel,
    null,
  );
}
