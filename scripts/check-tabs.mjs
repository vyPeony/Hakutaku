// タブの状態モデルの回帰検査（Issue #52）。
//
// `src/tabs.js` の純粋関数（DOM にも Tauri IPC にも触れない）を Node から直接
// 呼び、タブ操作で壊れやすい4種類の判断を検証する。
//
//   1. 追加と再オープン（`upsertTab`。位置を変えずに内容だけ差し替える）
//   2. 閉じたときの後継タブの選び方（`closeTab`。右優先、無ければ左、全て
//      閉じたら `null`）
//   3. フォーカスを動かさない内容更新（`updateTabContent`。`LOG-028` の
//      明示的な再読み込みは、背景のタブに対しても押せる）
//   4. 存在しない `targetId` を渡されたときの防御（同一の状態をそのまま返す）
//
// タブの並び・見出し・どれが選択中かは、`src/shell.js` の DOM 操作と
// `list_targets` のポーリングに挟まれた純粋な状態遷移であり、GUI 検査
// （`scripts/check-gui.mjs`。手動実行）でしか通らない経路が多い。後継タブの
// 選び方や「フォーカスを奪わない更新」は、操作の順序で結果が変わるため、
// 取り違え（右と左の取り違え、再オープンでの並び替え、背景タブの更新で
// 利用者の視点が飛ぶ）は画面を動かさなければ気付けなかった。ここでは操作の
// 組み合わせを Node だけで走らせ、その退行を CI で毎回捉える
// （`docs/verification/regression-checks.md`）。
//
// 期待値は「現在の実装が返した値」を写したものではなく、各関数の JSDoc が定める
// 仕様から独立に導いた値を書く。導出根拠は各検査の直前のコメントに残す。実装を
// 書き換えたときに期待値も一緒に書き換えてしまい、検査が何も守らなくなることを
// 防ぐため（`check-virtual-scroll.mjs`・`check-selection.mjs` と同じ方針）。
//
// 実行時間・メモリ量は一切扱わない（`VER-005`）。
//
// 使い方: node scripts/check-tabs.mjs

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import {
  closeTab,
  createTabsState,
  getActiveTab,
  setActiveTab,
  updateTabContent,
  upsertTab,
} from "../src/tabs.js";

const ROOT = resolve(import.meta.dirname, "..");

// ---------------------------------------------------------------------------
// 検査の土台（`check-selection.mjs` と同じ形）
// ---------------------------------------------------------------------------

const problems = [];
let checkCount = 0;

function format(value) {
  return typeof value === "object" && value !== null ? JSON.stringify(value) : String(value);
}

/**
 * 1件の検査結果を記録する。1件目の失敗で終了せず全件を集めるのは、退行の
 * 影響範囲（1つの境界だけか、経路全体か）を1回の実行で見分けられるようにするため。
 */
function check(name, ok, detail) {
  checkCount += 1;
  if (!ok) {
    problems.push(detail ? `${name}\n    ${detail}` : name);
  }
}

function expectEqual(name, actual, expected) {
  check(name, Object.is(actual, expected), `期待 ${format(expected)} / 実際 ${format(actual)}`);
}

/** タブの並びを `targetId` の配列として突き合わせる。 */
function expectOrder(name, state, expected) {
  const actual = state.tabs.map((tab) => tab.targetId);
  const ok =
    actual.length === expected.length && actual.every((value, index) => value === expected[index]);
  check(name, ok, `期待 ${format(expected)} / 実際 ${format(actual)}`);
}

/**
 * 検査用のタブを作る。`Tab` の必須項目（`src/tabs.js` の typedef）をすべて
 * 埋め、内容が入れ替わったことを見分けられるよう `targetId` から決まる値を使う。
 */
function makeTab(targetId, overrides = {}) {
  return {
    targetId,
    title: `対象${targetId}.log`,
    displaySetId: targetId * 10,
    generation: 1,
    totalItems: targetId * 1000,
    ...overrides,
  };
}

/** 複数のタブを順に開いた状態を作る（末尾に開いたタブがアクティブ）。 */
function openTabs(...targetIds) {
  let state = createTabsState();
  for (const targetId of targetIds) {
    state = upsertTab(state, makeTab(targetId));
  }
  return state;
}

// ---------------------------------------------------------------------------
// 1. 初期状態と `getActiveTab`
// ---------------------------------------------------------------------------
//
// 仕様（`createTabsState`・`getActiveTab` の JSDoc）:
//   createTabsState() → { tabs: [], activeTargetId: null }
//   getActiveTab(s)   → activeTargetId が null なら null。そうでなければ
//                       その targetId のタブ（見つからなければ null）

function checkInitialState() {
  const empty = createTabsState();
  expectEqual("初期状態: タブが無い", empty.tabs.length, 0);
  expectEqual("初期状態: アクティブなタブが無い", empty.activeTargetId, null);
  expectEqual("初期状態: getActiveTab は null", getActiveTab(empty), null);

  // 一覧の再同期のずれで `activeTargetId` だけが取り残された場合の防御
  // （`getActiveTab` の JSDoc「無ければ null」）。ここで undefined を返すと
  // 呼び出し側（shell.js の restoreActiveTabView）が例外で止まる。
  const dangling = { tabs: [makeTab(1)], activeTargetId: 99 };
  expectEqual("防御: 一覧に無いアクティブ ID なら null", getActiveTab(dangling), null);
}

// ---------------------------------------------------------------------------
// 2. `upsertTab`（追加と再オープン）
// ---------------------------------------------------------------------------
//
// 仕様（`upsertTab` の JSDoc）:
//   同じ targetId のタブが無ければ末尾へ追加する。あれば**位置を変えずに**
//   内容を差し替える（`LOG-028` 等で同じ対象を開き直した場合）。どちらの場合も
//   そのタブをアクティブにする。
// 以下の期待値はこの規則を手で適用した結果である。

function checkUpsert() {
  const one = upsertTab(createTabsState(), makeTab(1));
  expectOrder("追加: 1枚目のタブ", one, [1]);
  expectEqual("追加: 開いたタブがアクティブになる", one.activeTargetId, 1);

  const three = openTabs(1, 2, 3);
  expectOrder("追加: 開いた順に並ぶ", three, [1, 2, 3]);
  expectEqual("追加: 最後に開いたタブがアクティブ", three.activeTargetId, 3);

  // 再オープン（`LOG-028`）。位置は先頭のまま、内容だけが新しくなる。
  const reopened = upsertTab(three, makeTab(1, { displaySetId: 77, generation: 2, totalItems: 5 }));
  expectOrder("再オープン: 並びは変わらない（末尾へ移動しない）", reopened, [1, 2, 3]);
  expectEqual("再オープン: 対象のタブがアクティブになる", reopened.activeTargetId, 1);
  const reopenedTab = getActiveTab(reopened);
  expectEqual("再オープン: 表示集合 ID が更新される", reopenedTab.displaySetId, 77);
  expectEqual("再オープン: 世代が更新される", reopenedTab.generation, 2);
  expectEqual("再オープン: 総行数が更新される", reopenedTab.totalItems, 5);

  // 他のタブは巻き添えで書き換わらない。
  expectEqual("再オープン: 他のタブの内容は変わらない", reopened.tabs[1].displaySetId, 20);

  // 不変更新（`src/tabs.js` 冒頭「状態は不変更新（呼び出しのたびに新しい
  // オブジェクトを返す）」）。呼び出し元は戻り値を変数へ再代入する形で使うため、
  // 引数の状態がその場で書き換わると、再描画の比較対象が壊れる。
  expectEqual("不変更新: 元の状態のタブ数が変わらない", three.tabs.length, 3);
  expectEqual("不変更新: 元の状態のアクティブ ID が変わらない", three.activeTargetId, 3);
  expectEqual("不変更新: 元の状態のタブ内容が変わらない", three.tabs[0].displaySetId, 10);
  check(
    "不変更新: 新しい配列オブジェクトを返す",
    reopened.tabs !== three.tabs,
    "同じ配列を返しています（呼び出し側の差分検出が効かなくなります）",
  );
}

// ---------------------------------------------------------------------------
// 3. `closeTab`（後継タブの選び方）
// ---------------------------------------------------------------------------
//
// 仕様（`closeTab` の JSDoc）:
//   閉じたタブがアクティブだった場合、隣接するタブ（**右優先、無ければ左**）を
//   新たにアクティブにする。全て閉じた場合は activeTargetId: null。
//   targetId のタブが存在しなければ**同一の state を返す**。
// 「右優先」は、閉じた位置に繰り上がってくるタブ（元の並びで1つ右）を指す。

function checkClose() {
  // 中間のタブを閉じる → 右隣（3）が繰り上がる。
  const middle = closeTab(setActiveTab(openTabs(1, 2, 3), 2), 2);
  expectOrder("閉じる: 中間のタブが消える", middle, [1, 3]);
  expectEqual("閉じる: 中間を閉じたら右隣がアクティブ", middle.activeTargetId, 3);

  // 末尾のタブを閉じる → 右が無いので左隣（2）。
  const last = closeTab(openTabs(1, 2, 3), 3);
  expectOrder("閉じる: 末尾のタブが消える", last, [1, 2]);
  expectEqual("閉じる: 末尾を閉じたら左隣がアクティブ", last.activeTargetId, 2);

  // 先頭のタブを閉じる → 右隣（2）。
  const first = closeTab(setActiveTab(openTabs(1, 2, 3), 1), 1);
  expectEqual("閉じる: 先頭を閉じたら右隣がアクティブ", first.activeTargetId, 2);

  // 最後の1枚を閉じる → アクティブなし（左ペインは「未読み込み」へ戻る）。
  const emptied = closeTab(openTabs(1), 1);
  expectOrder("閉じる: 最後の1枚を閉じると空になる", emptied, []);
  expectEqual("閉じる: 空になったらアクティブも null", emptied.activeTargetId, null);

  // アクティブでないタブを閉じても、利用者が見ているタブは動かない。
  const background = closeTab(openTabs(1, 2, 3), 1);
  expectOrder("閉じる: 背景のタブだけが消える", background, [2, 3]);
  expectEqual("閉じる: 背景のタブを閉じてもアクティブは動かない", background.activeTargetId, 3);

  // 存在しない targetId（一覧の再同期のずれ、二重クリック）は何も起こさない。
  const before = openTabs(1, 2);
  const unchanged = closeTab(before, 99);
  check(
    "防御: 存在しないタブを閉じても同じ状態を返す",
    Object.is(unchanged, before),
    `別の状態を返しています: ${format(unchanged)}`,
  );
  // 空の状態に対しても同じ（初回描画前のクリックに対する防御）。
  const emptyState = createTabsState();
  check(
    "防御: タブが無い状態で閉じても同じ状態を返す",
    Object.is(closeTab(emptyState, 1), emptyState),
    "別の状態を返しています",
  );
}

// ---------------------------------------------------------------------------
// 4. `updateTabContent`（フォーカスを動かさない内容更新。`LOG-028`）
// ---------------------------------------------------------------------------
//
// 仕様（`updateTabContent` の JSDoc）:
//   既存タブの内容（displaySetId・generation・totalItems）だけを更新する。
//   `upsertTab` と異なり、タブの位置・フォーカス（activeTargetId）は変えない。
//   一致するタブが無ければ何もせず同じ state を返す。
// 背景の対象を再読み込みしただけで利用者の視点が奪われないことが要件
// （`src/shell.js` の handleReloadTargetClick）。

function checkUpdateTabContent() {
  const opened = openTabs(1, 2, 3); // アクティブは 3。
  const updated = updateTabContent(opened, 1, {
    displaySetId: 900,
    generation: 4,
    totalItems: 12_345,
  });

  expectOrder("内容更新: 並びは変わらない", updated, [1, 2, 3]);
  expectEqual("内容更新: アクティブなタブは変わらない（視点を奪わない）", updated.activeTargetId, 3);
  expectEqual("内容更新: 表示集合 ID が更新される", updated.tabs[0].displaySetId, 900);
  expectEqual("内容更新: 世代が更新される", updated.tabs[0].generation, 4);
  expectEqual("内容更新: 総行数が更新される", updated.tabs[0].totalItems, 12_345);
  // 見出し（表示名）と対象 ID は patch に含めないため保たれる。
  expectEqual("内容更新: 見出しは保たれる", updated.tabs[0].title, "対象1.log");
  expectEqual("内容更新: 対象 ID は保たれる", updated.tabs[0].targetId, 1);
  expectEqual("内容更新: 他のタブは変わらない", updated.tabs[1].displaySetId, 20);

  // アクティブなタブ自身を更新しても、アクティブ ID は変わらない。
  const activeUpdated = updateTabContent(opened, 3, {
    displaySetId: 31,
    generation: 2,
    totalItems: 7,
  });
  expectEqual("内容更新: アクティブなタブの更新でもアクティブ ID は同じ", activeUpdated.activeTargetId, 3);
  expectEqual("内容更新: アクティブなタブの内容が更新される", getActiveTab(activeUpdated).displaySetId, 31);

  // 不変更新（元の状態は書き換わらない）。
  expectEqual("内容更新: 元の状態は書き換わらない", opened.tabs[0].displaySetId, 10);

  // 一致するタブが無い場合（`close_target` との競合に対する防御）。
  const missing = updateTabContent(opened, 99, {
    displaySetId: 1,
    generation: 1,
    totalItems: 1,
  });
  check(
    "防御: 存在しないタブの内容更新は同じ状態を返す",
    Object.is(missing, opened),
    `別の状態を返しています: ${format(missing)}`,
  );
}

// ---------------------------------------------------------------------------
// 5. `setActiveTab`（未オープンの対象を選んでも状態を壊さない）
// ---------------------------------------------------------------------------
//
// 仕様（`setActiveTab` の JSDoc）:
//   既に開いているタブをアクティブにする。存在しない targetId を渡した場合は
//   何もしない（呼び出し側が誤って未オープンの対象を選択しても状態を壊さない）。

function checkSetActiveTab() {
  const opened = openTabs(1, 2, 3);
  const switched = setActiveTab(opened, 1);
  expectEqual("切り替え: 指定したタブがアクティブになる", switched.activeTargetId, 1);
  expectOrder("切り替え: 並びは変わらない", switched, [1, 2, 3]);
  expectEqual("切り替え: 元の状態は書き換わらない", opened.activeTargetId, 3);

  const missing = setActiveTab(opened, 99);
  check(
    "防御: 未オープンの対象を選んでも同じ状態を返す",
    Object.is(missing, opened),
    `別の状態を返しています: ${format(missing)}`,
  );
  const emptyState = createTabsState();
  check(
    "防御: タブが無い状態で選んでも同じ状態を返す",
    Object.is(setActiveTab(emptyState, 1), emptyState),
    "別の状態を返しています",
  );
}

// ---------------------------------------------------------------------------
// 6. 操作の組み合わせに対する不変条件（参照モデルとの突き合わせ）
// ---------------------------------------------------------------------------
//
// 単発の期待値では、操作の**順序**によってしか現れない退行（閉じた直後に
// 同じ対象を開き直す、後継タブを選んだ直後にそれも閉じる、など）を捉え
// られない。ここでは決定的な擬似乱数で操作列を作り、次を毎回突き合わせる。
//
//   - `targetId` が重複しない（`upsertTab` が同じ対象を2枚作らない）
//   - `activeTargetId` は null か、必ず現在のタブのいずれかを指す
//     （どちらでもない値になると `getActiveTab` が null を返し、閉じた
//     はずのタブの内容が復元されない・空表示のまま操作が効かなくなる）
//   - `getActiveTab` の結果が `activeTargetId` と一致する
//
// 擬似乱数は線形合同法で、種を固定する（同じ入力に対して常に同じ操作列に
// なり、失敗が再現できる）。`check-selection.mjs` と同じ方式。

const TARGET_ID_RANGE = 6;
const OPERATION_COUNT = 300;

/** 線形合同法（Numerical Recipes の係数）。決定性のためだけに使う。 */
function createRandom(seed) {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1_664_525) + 1_013_904_223) >>> 0;
    return state / 4_294_967_296;
  };
}

function checkOperationSequences() {
  const random = createRandom(20_260_052);
  const pickTargetId = () => Math.floor(random() * TARGET_ID_RANGE);

  let state = createTabsState();
  let duplicateTargetId = null;
  let danglingActive = null;
  let activeMismatch = null;

  for (let step = 0; step < OPERATION_COUNT; step += 1) {
    const operation = Math.floor(random() * 4);
    const targetId = pickTargetId();
    switch (operation) {
      case 0:
        state = upsertTab(state, makeTab(targetId, { generation: step }));
        break;
      case 1:
        state = closeTab(state, targetId);
        break;
      case 2:
        state = setActiveTab(state, targetId);
        break;
      default:
        state = updateTabContent(state, targetId, {
          displaySetId: step,
          generation: step,
          totalItems: step,
        });
        break;
    }

    const ids = state.tabs.map((tab) => tab.targetId);
    if (new Set(ids).size !== ids.length) {
      duplicateTargetId ??= `${step} 手目（操作 ${operation}、対象 ${targetId}）: ${format(ids)}`;
    }
    if (state.activeTargetId !== null && !ids.includes(state.activeTargetId)) {
      danglingActive ??=
        `${step} 手目（操作 ${operation}、対象 ${targetId}）: ` +
        `アクティブ ${state.activeTargetId} / タブ ${format(ids)}`;
    }
    const active = getActiveTab(state);
    const activeIdFromTab = active === null ? null : active.targetId;
    if (activeIdFromTab !== state.activeTargetId) {
      activeMismatch ??=
        `${step} 手目（操作 ${operation}、対象 ${targetId}）: ` +
        `getActiveTab ${format(activeIdFromTab)} / activeTargetId ${format(state.activeTargetId)}`;
    }
  }

  check(
    `操作列: 同じ対象のタブが2枚できない（${OPERATION_COUNT} 手）`,
    duplicateTargetId === null,
    duplicateTargetId,
  );
  check(
    `操作列: アクティブ ID が常に現在のタブを指す（${OPERATION_COUNT} 手）`,
    danglingActive === null,
    danglingActive,
  );
  check(
    `操作列: getActiveTab がアクティブ ID と一致する（${OPERATION_COUNT} 手）`,
    activeMismatch === null,
    activeMismatch,
  );
}

// ---------------------------------------------------------------------------
// 7. 前提の同期（`src/shell.js` がタブモデルを通していること）
// ---------------------------------------------------------------------------
//
// 上の検査は `src/tabs.js` の純粋関数だけを見ており、`shell.js` がその関数を
// 実際に使っているかまでは分からない。タブの並びやアクティブの決定が
// shell.js の中へ書き戻されると、この検査は通ったまま実物の挙動だけが変わる。
// ソースから最小限の手掛かりを読み出して突き合わせる
// （`check-selection.mjs` の「前提の同期」と同じ方式）。

function checkPremises() {
  const source = readFileSync(resolve(ROOT, "src", "shell.js"), "utf8");

  check(
    "前提: shell.js がタブの追加・再オープンを upsertTab へ委ねる",
    source.includes("state.tabs = upsertTab(state.tabs, tab)"),
    "タブの並びが shell.js 側で組み立てられている可能性があります",
  );
  check(
    "前提: shell.js がタブを閉じる処理を closeTab へ委ねる",
    source.includes("state.tabs = closeTab(state.tabs, targetId)"),
    "後継タブの選び方が shell.js 側で決められている可能性があります",
  );
  check(
    "前提: shell.js がタブの切り替えを setActiveTab へ委ねる",
    source.includes("state.tabs = setActiveTab(state.tabs, targetId)"),
    "アクティブなタブの決定が shell.js 側で行われている可能性があります",
  );
  check(
    "前提: shell.js が再読み込みを updateTabContent へ委ねる（LOG-028）",
    source.includes("state.tabs = updateTabContent(state.tabs, targetId, {"),
    "再読み込みが upsertTab 経由になると、背景の対象の更新で視点が奪われます",
  );
  check(
    "前提: shell.js がアクティブなタブの復元に getActiveTab を使う",
    source.includes("getActiveTab(state.tabs)"),
    "アクティブなタブの取得が shell.js 側で行われている可能性があります",
  );
}

// ---------------------------------------------------------------------------
// 実行
// ---------------------------------------------------------------------------

checkInitialState();
checkUpsert();
checkClose();
checkUpdateTabContent();
checkSetActiveTab();
checkOperationSequences();
checkPremises();

if (problems.length > 0) {
  console.error(`タブの状態モデルに問題が ${problems.length} 件あります。\n`);
  for (const problem of problems) console.error(`  ${problem}`);
  console.error(
    "\n判定方法と対象の一覧は docs/verification/regression-checks.md を参照してください。",
  );
  process.exit(1);
}

console.log(
  `タブの状態モデル（追加と再オープン、後継タブの選択、フォーカスを動かさない内容更新、防御的な既定動作）を ${checkCount} 項目検査しました。問題はありません。`,
);
