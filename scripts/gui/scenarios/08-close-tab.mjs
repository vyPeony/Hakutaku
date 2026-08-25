// シナリオ8: タブの × で閉じると、左ペインの表示状態も追随する。
//
// タブを閉じる操作は `close_target`（Tauri コマンド）で対象そのものを解放し、
// そのあと左ペインを取得し直す（`src/shell.js` の `handleTabClose`）。タブ側だけを
// 消して左ペインが「読み込み済み」のまま残ると、利用者からは「開いているのに
// タブが無い」状態に見える。タブと左ペインが同じ実態を指し続けることが、この
// シナリオの確認事項。
//
// 参照対象のファイルそのものは変更しない（`ERR-003`）。

import { SAMPLE_TARGETS, readTabs, readTargetRows, readViewState } from "../app.mjs";

export const name = "タブを閉じる";

export async function run({ page, expect }) {
  const target = SAMPLE_TARGETS.MERGE_B;

  const premisesOk = expect.check(
    "前提: 統合表示が OFF で、閉じる対象のタブがある",
    (await readViewState(page)).mergedTogglePressed === "false" &&
      (await readTabs(page)).some((tab) => tab.title === target),
    `タブ ${JSON.stringify((await readTabs(page)).map((tab) => tab.title))}`,
  );
  if (!premisesOk) {
    return;
  }

  // 左ペイン再設計（Issue #97）: 行の状態は data-status-kind で読む。閉じる前は
  // 読み込み済み（ready）で、タブを開いた対象として強調されている。
  const beforeRow = (await readTargetRows(page)).find((row) => row.name === target);
  expect.expectEqual("閉じる前の左ペインの状態が読み込み済み（ready）である", beforeRow?.kind, "ready");
  expect.expectEqual(
    "閉じる前はタブを開いている対象として強調されている（target-row--open）",
    beforeRow?.open,
    true,
  );
  const tabCountBefore = (await readTabs(page)).length;

  await page
    .locator("#tab-bar > *", { hasText: target })
    .locator(".tab__close")
    .first()
    .click({ timeout: 10_000 });

  // タブの消滅と左ペインの追随は別の非同期処理（`close_target` の応答 →
  // タブ再描画 → `list_targets` の取得し直し）のため、両方が済むまで待つ。
  await page.waitForFunction(
    (title) => {
      const tabTitles = Array.from(document.querySelectorAll("#tab-bar .tab__title")).map(
        (node) => node.textContent?.trim() ?? "",
      );
      if (tabTitles.includes(title)) {
        return false;
      }
      const row = Array.from(document.querySelectorAll("#target-list > li")).find(
        (item) => (item.querySelector(".target-row__name")?.textContent?.trim() ?? "") === title,
      );
      return (row?.dataset.statusKind ?? "") !== "ready";
    },
    target,
    { timeout: 20_000 },
  );

  const tabsAfter = await readTabs(page);
  expect.check(
    "× を押したタブが無くなる",
    !tabsAfter.some((tab) => tab.title === target),
    `残ったタブ ${JSON.stringify(tabsAfter.map((tab) => tab.title))}`,
  );
  expect.expectEqual("閉じたのは1件だけで、他のタブは残る", tabsAfter.length, tabCountBefore - 1);

  const afterRow = (await readTargetRows(page)).find((row) => row.name === target);
  expect.check(
    "左ペインの行自体は残る（設定由来のデータソースは閉じても一覧から消えない。CFG-003）",
    afterRow !== undefined,
    `左ペイン ${JSON.stringify((await readTargetRows(page)).map((row) => row.name))}`,
  );
  expect.expectEqual("左ペインの状態が未読み込み（not_opened）へ戻る", afterRow?.kind, "not_opened");
  expect.expectEqual(
    "タブを開いている対象の強調（target-row--open）が外れる",
    afterRow?.open,
    false,
  );
}
