// シナリオ7: 複数タブと時系列統合トグルの非活性化（Issue #83、`LOG-006`、`LOG-015`）。
//
// 時系列統合表示は表示品質の課題（表示改善案は Issue #82 に集約済み）のため、
// #82 の改修実装まで UI 入口（`#merged-view-toggle`）を一時的に非活性化して
// いる（`src/shell.js` の `MERGED_VIEW_TEMPORARILY_DISABLED`）。統合表示の
// 実装本体（`enable_merged_view`／`disable_merged_view`、`src/log_view.js` の
// 統合経路）は削除していないが、UI から到達できない間は検査しようがないため、
// このシナリオは非活性化そのもの（disabled・title・aria-pressed・統合タブが
// 現れないこと）だけを確認する。統合表示の ON/OFF・混在表示・並び順の確認は、
// #82 の改修実装後にこのシナリオへ戻す（旧手順は Git 履歴を参照）。
//
// 後続シナリオ（タブを閉じる）はこのシナリオが開いたタブに依存するため、
// このシナリオは複数タブを開いた状態のまま終わる。

import { SAMPLE_TARGETS, openTargetByName, readTabs, readViewState } from "../app.mjs";

export const name = "複数タブと時系列統合トグルの非活性化（#83）";

export async function run({ page, expect }) {
  // --- 複数タブ ---
  for (const target of [SAMPLE_TARGETS.MERGE_A, SAMPLE_TARGETS.MERGE_B]) {
    await openTargetByName(page, target);
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

  // --- 統合トグルの非活性化（Issue #83） ---
  const viewState = await readViewState(page);
  expect.check(
    "統合トグルが disabled である（#82 の改修実装まで一時無効化。Issue #83）",
    viewState.mergedToggleDisabled === true,
    `disabled ${JSON.stringify(viewState.mergedToggleDisabled)}`,
  );
  expect.expectEqual(
    "統合トグルの表示ラベルは「時系列統合: OFF」のまま",
    viewState.mergedToggleLabel,
    "時系列統合: OFF",
  );
  expect.expectEqual(
    "統合トグルの aria-pressed が false のまま変化しない",
    viewState.mergedTogglePressed,
    "false",
  );
  expect.check(
    "統合トグルの title に案内先の Issue #82 への参照がある",
    viewState.mergedToggleTitle.includes("82"),
    `title ${JSON.stringify(viewState.mergedToggleTitle)}`,
  );

  const tabsAfter = await readTabs(page);
  expect.check(
    "統合タブ（tab--merged）は存在しない",
    tabsAfter.every((tab) => !tab.merged),
    `タブ ${JSON.stringify(tabsAfter.map((tab) => tab.title))}`,
  );
}
