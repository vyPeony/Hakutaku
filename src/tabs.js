// 開いているビューのタブの純粋な状態モデル（P07-1、`LOG-015`）。
//
// DOM にも IPC にも触れない。統合表示 OFF 時のファイル別タブ切り替え
// （分割表示は作らない。`LOG-015`）を表現する。1タブ = 1対象（`target_id`）
// で、タブの中身（ログ表示そのもの）は src/log_view.js が担当する
// （このモジュールはタブの並び・見出し・どれが選択中かだけを扱う）。
//
// ADR-0006 に従い、フレームワーク・バンドラーを使わない素の ES モジュール。
// 状態は不変更新（呼び出しのたびに新しいオブジェクトを返す）にし、呼び出し側
// （src/shell.js）が戻り値を変数へ再代入する形で使う。`node --check` で構文を
// 検証する。

/**
 * @typedef {Object} Tab
 * @property {number} targetId 対象一覧（src/targets.js）のエントリと対応する識別子。
 * @property {string} title タブの見出し（表示名）。
 * @property {number} displaySetId
 * @property {number} generation
 * @property {number} totalItems
 */

/**
 * @typedef {Object} TabsState
 * @property {Tab[]} tabs 開いている順（先頭が最初に開いたタブ）。
 * @property {number | null} activeTargetId 現在アクティブなタブの `targetId`（タブが無ければ `null`）。
 */

/** @returns {TabsState} 初期状態（タブなし）。 */
export function createTabsState() {
  return { tabs: [], activeTargetId: null };
}

/**
 * タブを追加、または既存のタブ（同じ `targetId`）を更新してアクティブにする。
 * `LOG-028`（対象を開き直す）等で同じ対象を再度開いた場合、タブの位置は
 * 変えずに内容（`displaySetId` 等）だけを更新する。
 *
 * @param {TabsState} state
 * @param {Tab} tab
 * @returns {TabsState}
 */
export function upsertTab(state, tab) {
  const index = state.tabs.findIndex((existing) => existing.targetId === tab.targetId);
  const tabs =
    index === -1
      ? [...state.tabs, tab]
      : state.tabs.map((existing, i) => (i === index ? tab : existing));
  return { tabs, activeTargetId: tab.targetId };
}

/**
 * タブを閉じる。閉じたタブがアクティブだった場合、隣接するタブ
 * （右優先、無ければ左）を新たにアクティブにする。全て閉じた場合は
 * `activeTargetId: null`。
 *
 * @param {TabsState} state
 * @param {number} targetId
 * @returns {TabsState} `targetId` のタブが存在しなければ同一の `state` を返す。
 */
export function closeTab(state, targetId) {
  const index = state.tabs.findIndex((tab) => tab.targetId === targetId);
  if (index === -1) {
    return state;
  }

  const tabs = state.tabs.filter((tab) => tab.targetId !== targetId);

  let activeTargetId = state.activeTargetId;
  if (activeTargetId === targetId) {
    const neighbor = tabs[index] ?? tabs[index - 1] ?? null;
    activeTargetId = neighbor ? neighbor.targetId : null;
  }

  return { tabs, activeTargetId };
}

/**
 * 既存タブの内容（`displaySetId`・`generation`・`totalItems`）だけを更新する。
 * `upsertTab` と異なり、タブの位置・フォーカス（`activeTargetId`）は変えない。
 *
 * `LOG-028`（明示的な再読み込み）で使う。「再読み込み」はツールバーから
 * アクティブなタブの対象へ作用するが（Issue #97 の左ペイン再設計）、更新の
 * たびにタブの
 * 並びやフォーカスが動くのは驚きが大きく、押下と応答の間にタブが切り替わる
 * 競合もあり得るため、フォーカスは変えずタブの中身だけを最新化する
 * （`src/shell.js` の `handleReloadTargetClick` 参照）。
 *
 * `targetId` に一致するタブが無ければ何もせず同じ `state` を返す
 * （`close_target` 等との競合に対する防御。`LOG-028` の対象は常にタブを
 * 持つ設計だが、念のため）。
 *
 * @param {TabsState} state
 * @param {number} targetId
 * @param {{ displaySetId: number, generation: number, totalItems: number }} patch
 * @returns {TabsState}
 */
export function updateTabContent(state, targetId, patch) {
  const index = state.tabs.findIndex((tab) => tab.targetId === targetId);
  if (index === -1) {
    return state;
  }
  const tabs = state.tabs.map((tab, i) => (i === index ? { ...tab, ...patch } : tab));
  return { tabs, activeTargetId: state.activeTargetId };
}

/**
 * 既に開いているタブをアクティブにする。存在しない `targetId` を渡した場合は
 * 何もしない（呼び出し側が誤って未オープンの対象を選択しても状態を壊さない）。
 *
 * @param {TabsState} state
 * @param {number} targetId
 * @returns {TabsState}
 */
export function setActiveTab(state, targetId) {
  if (!state.tabs.some((tab) => tab.targetId === targetId)) {
    return state;
  }
  return { tabs: state.tabs, activeTargetId: targetId };
}

/**
 * 現在アクティブなタブを返す（無ければ `null`）。
 *
 * @param {TabsState} state
 * @returns {Tab | null}
 */
export function getActiveTab(state) {
  if (state.activeTargetId === null) {
    return null;
  }
  return state.tabs.find((tab) => tab.targetId === state.activeTargetId) ?? null;
}
