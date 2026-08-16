// 共通シェル（P07-1／P07-2）。
//
// `tasks/phase-07-shell-ui.md` の P07-1 が定める共通画面
// ——「参照対象一覧」「開いているビューのタブ」——の描画と操作、および対象を
// 開く操作のエラー・通知のモーダルダイアログ表示（`src/error_panel.js`、
// Issue #9）の呼び出しを統括する。DOM 操作と IPC 呼び出し、それらをつなぐ状態管理を
// 担当し、純粋ロジック（一覧の状態モデル・タブ状態管理）は
// src/targets.js・src/tabs.js へ切り出している（ADR-0006、AGENTS.md の
// 指示）。
//
// # 責務の境界（9.2 の責務分離との対応）
//
// このモジュールは形式に依存しない「共通シェル」であり、ログ表示の中身
// （仮想スクロール等）を一切知らない。ビュー領域の操作は src/log_view.js が
// 公開する `logViewer`（`activate` / `showEmpty` の2関数だけの契約）越しに
// だけ行う。将来 DICOM（P14）や SQLite 等の形式別ビューアを追加する場合、
// このモジュールは対象の形式に応じて差し込むビュー実装を選ぶ小さな切り替えを
// 持つだけで済む設計を意図している（現時点ではログビューア1種類のみのため
// 切り替えは無い）。
//
// # 対象一覧の同期方針・進捗表示（P07-2）
//
// 対象の総数は 10 件程度を想定しており（`PERF-005`）、対象一覧を変更する
// 操作（開く・再試行・閉じる）のたびに `list_targets` を呼び直して一覧全体を
// 再取得する（差分更新はしない）。この規模では往復コストより実装の単純さ・
// 状態不整合を避けられる確実さを優先する。
//
// `open_log_file`・`open_config_data_source`・`retry_target` は P07-2 で
// 非同期化され（`src-tauri/src/targets.rs` 参照）、呼び出し直後は
// `loading` 応答だけを返す。実際の完了・失敗・キャンセルは、このモジュールが
// `list_targets` を**読み込み中の対象がある間だけ 500ms 間隔でポーリング**
// することで検出する。
//
// ## イベント（`hakutaku://load-progress`／`hakutaku://load-outcome`）ではなく
// ## ポーリングを採用した理由
//
// Tauri の `window.__TAURI_INTERNALS__` は `invoke` 相当のヘルパーしか
// 提供せず、`listen()`（イベント購読）は `@tauri-apps/api`（npm パッケージ）
// 経由の実装か、`transformCallback` を含む内部プロトコルを自前で再実装する
// 必要がある。本プロジェクトは ADR-0006 によりバンドラー・npm 依存を持たない
// 素の ES モジュール構成であり、`npm` 依存の追加は本フェーズの禁止事項でも
// ある。内部プロトコルの自前実装は、実機での対話的な検証手段を持たない
// このセッションでは動作を確認しきれず（`listen` の登録・
// `transformCallback` の解放漏れ等を作り込むリスクがある）、確実性を優先して
// 見送った。
//
// 一方 `list_targets` のポーリングは、既存の「対象一覧を変更するたびに
// 全件再取得する」という設計をそのまま延長するだけで実装でき、対象数が
// 10件程度（`PERF-005`）であるため 500ms 間隔の全件取得によるコストは
// 無視できる。Rust 側（`src-tauri/src/targets.rs`）は
// `AppHandle::emit` によるイベント発行も行っており（`ProgressSink` の
// 実装。フロントエンドの Capability 許可を必要としない）、将来
// `listen()` 経由の購読へ切り替える場合の土台は既に用意されている。
//
// ADR-0006 に従い、フレームワーク・バンドラーを使わない素の ES モジュール。
// Tauri の IPC 呼び出しは src/main.js と同じ理由で
// window.__TAURI_INTERNALS__.invoke を直接使う。

import { initLogView, logViewer } from "./log_view.js";
import { buildTargetRows } from "./targets.js";
import {
  createTabsState,
  upsertTab,
  updateTabContent,
  closeTab,
  setActiveTab,
  getActiveTab,
} from "./tabs.js";
import { showTargetError, showRawDisplayFallbackNotice } from "./error_panel.js";
import { showErrorBanner, showInfoBanner } from "./banner.js";

/**
 * @typedef {import("./targets.js").TargetSessionDto} TargetSessionDto
 * @typedef {import("./targets.js").TargetRow} TargetRow
 * @typedef {import("./tabs.js").TabsState} TabsState
 * @typedef {import("./tabs.js").Tab} Tab
 */

/**
 * `list_datetime_formats` が返す日時書式1件（`LOG-022`）。
 *
 * @typedef {object} DatetimeFormatOption
 * @property {string} id 要件 ID（`LOG-DT-004` など）。再解析要求でそのまま
 *   送り返す値。
 * @property {string} pattern 表示用の書式パターン（`YYYY/MM/DD HH:mm:ss:SS`）。
 */

/** 読み込み中の対象がある間のポーリング間隔（ミリ秒）。P07-2。 */
const POLL_INTERVAL_MS = 500;

/** モジュール内部状態。 */
const state = {
  /** @type {string[]} `get_config_status` の `data_source_names`（CFG-003／PROD-006）。 */
  dataSourceNames: [],
  /** @type {TargetSessionDto[]} 直近の `list_targets` 応答。 */
  sessionTargets: [],
  /** @type {TabsState} */
  tabs: createTabsState(),
  /**
   * 開く・再試行の要求を送信中の対象を追跡する（二重送信防止）。キーは
   * 設定由来なら `configured:<name>`、対象一覧に登録済みなら `target:<id>`。
   * @type {Set<string>}
   */
  pendingKeys: new Set(),
  /**
   * 読み込み完了（`ready`／`cancelled_partial`）を検出したら自動でタブを
   * 開く／切り替える対象の ID（P07-2）。`open_log_file` 等の呼び出しが
   * `loading` を返した直後に追加し、ポーリングで完了を検出した時点で
   * 除去する。
   * @type {Set<number>}
   */
  pendingAutoActivate: new Set(),
  /**
   * `pendingAutoActivate` と同様だが、完了検出時にエラー表示を出したことが
   * 無いよう追跡する（`open_config_data_source`／`retry_target` の同期応答が
   * 既にエラーを表示している場合と重複させないため。読み込み開始が非同期の
   * 対象だけをここへ追加する）。
   * @type {Set<number>}
   */
  pendingErrorNotice: new Set(),
  /** @type {ReturnType<typeof setInterval> | null} 読み込み中ポーリングのタイマー。 */
  pollTimer: null,
  /** @type {string[]} `list_log_profiles` の応答（LOG-022 の手動選択 UI）。 */
  logProfileNames: [],
  /**
   * @type {DatetimeFormatOption[]} `list_datetime_formats` の応答（LOG-022 の
   * 手動選択 UI）。設定に依存しない固定の一覧のため、起動時に一度
   * 取得したら以後変わらない。
   */
  datetimeFormats: [],
  /** @type {boolean} `refreshTargets` の多重実行防止（ポーリングと手動更新の重複対策）。 */
  refreshInFlight: false,
  /**
   * @type {boolean} 時系列統合表示（P09-1、`LOG-007`〜`LOG-008`）
   * が現在 ON かどうか。ON の間はタブバーに統合タブ1つだけを表示し
   * （`LOG-015`）、対象一覧・既存タブの操作でビュー領域を個別ファイルへは
   * 切り替えない（`renderTabBar`・`activateSessionTab`・`activateExistingTab`
   * のガードを参照）。
   */
  mergedViewEnabled: false,
};

/** @type {{
 *   targetList: HTMLElement,
 *   openFileButton: HTMLButtonElement,
 *   tabBar: HTMLElement,
 *   mergedViewToggle: HTMLButtonElement,
 * } | null} */
let elements = null;

/**
 * @param {string | null} [manualProfile] `LOG-022`、P07-2。
 * @param {string | null} [manualDatetimeFormat] `LOG-022`。書式の
 *   要件 ID（`LOG-DT-004` など）。
 * @returns {Promise<{ kind: "loading" | "cancelled" | "failed", [key: string]: unknown }>}
 */
function invokeOpenLogFile(manualProfile, manualDatetimeFormat) {
  return window.__TAURI_INTERNALS__.invoke("open_log_file", {
    manualProfile: manualProfile ?? null,
    manualDatetimeFormat: manualDatetimeFormat ?? null,
  });
}

/**
 * @param {string} name
 * @param {string | null} [manualProfile] `LOG-022`、P07-2。
 * @param {string | null} [manualDatetimeFormat] `LOG-022`。
 * @returns {Promise<{ kind: "loading" | "failed", [key: string]: unknown }>}
 */
function invokeOpenConfigDataSource(name, manualProfile, manualDatetimeFormat) {
  return window.__TAURI_INTERNALS__.invoke("open_config_data_source", {
    name,
    manualProfile: manualProfile ?? null,
    manualDatetimeFormat: manualDatetimeFormat ?? null,
  });
}

/**
 * @param {number} targetId
 * @param {string | null} [manualProfile] `LOG-022`、P07-2。生表示へ退避した
 *   対象を手動プロファイル指定で再解析する際に使う（`buildReparseControl` の
 *   「選んで再解析」操作の「プロファイル」選択）。
 * @param {string | null} [manualDatetimeFormat] `LOG-022`。同じ操作
 *   の「日時書式」選択。プロファイルの `datetime_format` 設定より優先される
 *   （`hakutaku_core::LoadControl::manual_datetime_format`）。
 * @returns {Promise<{ kind: "loading" | "failed" | "not_found", [key: string]: unknown }>}
 */
function invokeRetryTarget(targetId, manualProfile, manualDatetimeFormat) {
  return window.__TAURI_INTERNALS__.invoke("retry_target", {
    targetId,
    manualProfile: manualProfile ?? null,
    manualDatetimeFormat: manualDatetimeFormat ?? null,
  });
}

/**
 * 対象を明示的に開き直す（`LOG-028`、ADR-0007）。`Ready` 状態の対象にだけ
 * 作用し、それ以外（読み込み中・キャンセル済み・エラー）では
 * `{ kind: "not_found" }` が返る（`src-tauri/src/targets.rs` の
 * `reload_target` 参照）。
 *
 * `open_log_file` 等と異なり `reload_target` は非同期化の対象外（同じ
 * ファイルの`hakutaku_core::reload_source` は進捗・キャンセルを受け付けない
 * 同期 API で、明示的な再読み込みは短時間で終わる想定のため）。そのため
 * この呼び出しはポーリングを介さず、結果（`kind`）を直接返す。
 *
 * @param {number} targetId
 * @returns {Promise<{
 *   kind: "reloaded" | "rejected_over_limit" | "failed" | "not_found",
 *   [key: string]: unknown,
 * }>}
 */
function invokeReloadTarget(targetId) {
  return window.__TAURI_INTERNALS__.invoke("reload_target", { targetId });
}

/**
 * @param {number} targetId
 * @returns {Promise<boolean>}
 */
function invokeCloseTarget(targetId) {
  return window.__TAURI_INTERNALS__.invoke("close_target", { targetId });
}

/**
 * 読み込み中の対象にキャンセルを要求する（P07-2）。
 *
 * @param {number} targetId
 * @returns {Promise<boolean>}
 */
function invokeCancelLoad(targetId) {
  return window.__TAURI_INTERNALS__.invoke("cancel_load", { targetId });
}

/** @returns {Promise<TargetSessionDto[]>} */
function invokeListTargets() {
  return window.__TAURI_INTERNALS__.invoke("list_targets");
}

/**
 * ログ解析プロファイルの名前一覧を取得する（`LOG-022` の手動選択 UI、P07-2）。
 *
 * @returns {Promise<string[]>}
 */
function invokeListLogProfiles() {
  return window.__TAURI_INTERNALS__.invoke("list_log_profiles");
}

/**
 * 既知の日時書式（`LOG-DT-001`〜`006`）の一覧を取得する（`LOG-022` の手動
 * 選択 UI）。6書式をこのモジュールの定数として持つと、解析側の
 * 増減に追随できないため、必ずこのコマンドの応答を選択肢の出所にする。
 *
 * @returns {Promise<DatetimeFormatOption[]>}
 */
function invokeListDatetimeFormats() {
  return window.__TAURI_INTERNALS__.invoke("list_datetime_formats");
}

/**
 * 対象を明示的に再読み込みする（`LOG-028`、ADR-0007）。`Ready` 状態の行に
 * 常設する「再読み込み」ボタンから呼ばれる（`buildTargetRowElement`）。
 *
 * `update_pending`（上限超過による前回の再読み込み拒否）が真かどうかに
 * かかわらず、いつでも呼び出せる。`update_pending` は「拒否された結果」を
 * 表す付加情報であり、呼び出しの前提条件ではない
 * （`docs/requirements/functional.md` の `LOG-028`: 利用者はいつでも明示的に
 * 再読み込みを指示できる。`ADR-0007` の `update_pending` は再読み込みが
 * 上限超過で拒否された結果を示す状態であり、再読み込みそのものの許可条件
 * ではない）。
 *
 * 押した対象が現在アクティブなタブとは限らない（対象一覧の行はどれでも
 * 押せる）。タブの位置・フォーカスは維持し（`updateTabContent`。ただし
 * 通知のモーダルダイアログ表示中はフォーカスがダイアログへ移り、閉じると
 * 戻る）、
 * 対象が現在アクティブなタブだった場合にだけ、ビュー領域を即時再同期し
 * （`generation_mismatch` の自己修復に任せない）、生表示へ退避していれば
 * 通知（`showRawDisplayFallbackNotice`）を出す。バックグラウンドの対象では
 * どちらも行わない（フォーカスを奪わない。通知は次にそのタブを開いたときの
 * 行内「選んで再解析」UI が常設の手がかりとして機能する。既存の
 * `activateExistingTab` はタブ切替のたびに通知を出す設計ではないため、
 * ここでは変更しない）。
 *
 * @param {number} targetId
 */
async function handleReloadTargetClick(targetId) {
  const pendingKey = `target:${targetId}`;
  if (state.pendingKeys.has(pendingKey)) {
    return;
  }
  state.pendingKeys.add(pendingKey);
  try {
    const response = await invokeReloadTarget(targetId);
    if (response.kind === "reloaded") {
      const displaySetId = /** @type {number} */ (response.display_set_id);
      const generation = /** @type {number} */ (response.generation);
      const totalItems = Number(response.total_items);
      state.tabs = updateTabContent(state.tabs, targetId, {
        displaySetId,
        generation,
        totalItems,
      });
      await refreshTargets();
      if (state.tabs.activeTargetId === targetId && !state.mergedViewEnabled) {
        const tab = state.tabs.tabs.find((candidate) => candidate.targetId === targetId);
        if (tab) {
          logViewer.activate({
            display_set_id: tab.displaySetId,
            generation: tab.generation,
            total_items: tab.totalItems,
            source_label: tab.title,
          });
        }
        const session = state.sessionTargets.find(
          (candidate) => candidate.target_id === targetId,
        );
        if (
          session &&
          session.status.kind === "ready" &&
          session.status.fell_back_to_raw_display
        ) {
          showRawDisplayFallbackNotice(session.display_name);
        }
      }
      return;
    }

    if (response.kind === "rejected_over_limit" || response.kind === "failed") {
      // rejected_over_limit（上限超過拒否、対象は Ready のまま update_pending
      // が立つ）・failed（`LOG-023` の変更検知・`LOG-027` の共有違反等、対象は
      // Error へ遷移）のいずれも `ERR-002` の5要素を持つ。モーダルダイアログ
      // （`src/error_panel.js`）で表示する（同時多発時はキューで順に表示される）。
      showTargetError(/** @type {import("./targets.js").UserFacingErrorDto} */ (response.error));
      await refreshTargets();
      return;
    }

    // "not_found": 一覧から既に除去されていた、または既に Ready でなくなって
    // いた（他操作との競合。ERR-001 の考え方に従い、静かに一覧を再同期する
    // だけに留める）。
    await refreshTargets();
  } catch (error) {
    console.error("reload_target の呼び出しに失敗しました:", error);
    showErrorBanner("対象の再読み込みでエラーが発生しました。");
  } finally {
    state.pendingKeys.delete(pendingKey);
  }
}

/**
 * アクセス拒否エラー表示の「管理者として新しいウィンドウで開く」ボタンから
 * だけ呼び出す（`PRIV-002`〜`004`、P11-2）。
 *
 * @returns {Promise<{ kind: "launched" | "cancelled" | "failed", reason?: string }>}
 */
function invokeLaunchElevated() {
  return window.__TAURI_INTERNALS__.invoke("launch_elevated");
}

/**
 * 現在開いている全ソースを横断する統合表示集合を構築する（P09-1、
 * `LOG-007`〜`LOG-008`）。
 *
 * @returns {Promise<{ display_set_id: number, generation: number, total_items: number }>}
 */
function invokeEnableMergedView() {
  return window.__TAURI_INTERNALS__.invoke("enable_merged_view");
}

/**
 * 統合表示集合を破棄する（`LOG-008`、`LOG-015`）。
 *
 * @returns {Promise<void>}
 */
function invokeDisableMergedView() {
  return window.__TAURI_INTERNALS__.invoke("disable_merged_view");
}

/**
 * 共通シェルを初期化する（呼び出し元は src/main.js）。ビュー領域
 * （src/log_view.js）の初期化もここで行う。フロントエンドは保持上限を
 * ハードコードせず `get_config_status` の応答から受け取る（CFG-022）。
 *
 * @param {{ retentionLimits: { maxRows: number, maxBytes: number }, dataSourceNames: string[] }} params
 */
export function initShell({ retentionLimits, dataSourceNames }) {
  state.dataSourceNames = dataSourceNames;

  elements = {
    targetList: document.getElementById("target-list"),
    openFileButton: /** @type {HTMLButtonElement} */ (
      document.getElementById("open-file-button")
    ),
    tabBar: document.getElementById("tab-bar"),
    mergedViewToggle: /** @type {HTMLButtonElement} */ (
      document.getElementById("merged-view-toggle")
    ),
  };

  elements.openFileButton.addEventListener("click", handleOpenFileButtonClick);
  elements.mergedViewToggle.addEventListener("click", handleMergedViewToggleClick);

  // 現時点ではビュー領域の実装はテキストログビューア（src/log_view.js）
  // 1種類のみのため、その初期化（DOM 要素の取得・スクロール購読）は
  // ここで直接呼び出す。`logViewer`（activate / showEmpty の2関数）は
  // タブ切り替えのたびに呼ぶ差し込み口の契約であり、1回限りの初期化は
  // 契約に含めていない（形式ごとに必要な初期化パラメータが異なり得るため。
  // src/log_view.js のモジュール doc コメント参照）。
  initLogView(retentionLimits);

  updateMergedViewToggleLabel();
  renderTargetList();
  renderTabBar();

  refreshTargets();

  // LOG-022 の手動選択 UI（プロファイル・日時書式を選んで再解析）用に、
  // 選択肢を取得しておく。どちらも失敗しても起動処理は止めない（既定は
  // 「生表示のまま」で使い続けられる、という裁定）。片方が失敗しても
  // もう片方の選択肢は使えるよう、2つの要求は独立に扱う。
  invokeListLogProfiles()
    .then((names) => {
      state.logProfileNames = names;
      renderTargetList();
    })
    .catch((error) => {
      console.error("list_log_profiles の呼び出しに失敗しました:", error);
    });

  invokeListDatetimeFormats()
    .then((formats) => {
      state.datetimeFormats = formats;
      renderTargetList();
    })
    .catch((error) => {
      console.error("list_datetime_formats の呼び出しに失敗しました:", error);
    });
}

/**
 * 対象一覧を再取得し、左ペインを再描画する。読み込み中の対象が無くなれば
 * ポーリングを止め、あれば継続する（P07-2、`syncPolling`）。
 */
async function refreshTargets() {
  if (state.refreshInFlight) {
    // ポーリングと手動更新（開く・再試行・キャンセル操作直後）が重なった
    // 場合の二重実行を防ぐ。
    return;
  }
  state.refreshInFlight = true;
  try {
    try {
      state.sessionTargets = await invokeListTargets();
    } catch (error) {
      console.error("list_targets の呼び出しに失敗しました:", error);
      showErrorBanner("参照対象一覧の取得に失敗しました。");
      return;
    }
    processCompletedAutoActivations();
    renderTargetList();
    syncPolling();
  } finally {
    state.refreshInFlight = false;
  }
}

/**
 * `state.pendingAutoActivate` に登録済みの対象のうち、読み込みが完了・
 * キャンセル・失敗したものを処理する（P07-2）。`state.sessionTargets` を
 * 読み取った直後に呼ぶ。
 *
 * `open_log_file` 等は読み込みを開始した時点で `loading` 応答だけを返す
 * ため（`src-tauri/src/targets.rs` のモジュール doc コメント「非同期化の
 * 設計」）、完了時にタブを開く・エラーを表示する処理をここへ集約する。
 */
function processCompletedAutoActivations() {
  for (const targetId of Array.from(state.pendingAutoActivate)) {
    const session = state.sessionTargets.find((candidate) => candidate.target_id === targetId);
    if (!session) {
      // 対象が一覧から消えた（close_target 等との競合）。追跡をやめる。
      state.pendingAutoActivate.delete(targetId);
      state.pendingErrorNotice.delete(targetId);
      continue;
    }
    if (session.status.kind === "ready" || session.status.kind === "cancelled_partial") {
      activateSessionTab(session);
      state.pendingAutoActivate.delete(targetId);
      state.pendingErrorNotice.delete(targetId);
    } else if (session.status.kind === "error") {
      state.pendingAutoActivate.delete(targetId);
      if (state.pendingErrorNotice.has(targetId)) {
        showTargetError(
          /** @type {import("./targets.js").UserFacingErrorDto} */ (session.status.error),
        );
        state.pendingErrorNotice.delete(targetId);
      }
    }
    // "loading" のままなら、次回のポーリングまでそのまま待つ。
  }
}

/**
 * `list_targets` の1件（状態 `ready` または `cancelled_partial`）をタブとして
 * 開く・切り替える。
 *
 * @param {TargetSessionDto} session
 */
function activateSessionTab(session) {
  const status = session.status;
  /** @type {Tab} */
  const tab = {
    targetId: session.target_id,
    title: session.display_name,
    displaySetId: /** @type {number} */ (status.display_set_id),
    generation: /** @type {number} */ (status.generation),
    totalItems: Number(status.total_items),
  };
  state.tabs = upsertTab(state.tabs, tab);
  // P09-1: 統合表示 ON の間は、ビュー領域を統合表示のまま維持する
  // （LOG-015: 統合タブ1つだけを表示する）。タブの記録（state.tabs）は
  // 更新しておき、統合表示を OFF にした時点で正しく復元できるようにする。
  if (!state.mergedViewEnabled) {
    logViewer.activate({
      display_set_id: tab.displaySetId,
      generation: tab.generation,
      total_items: tab.totalItems,
      source_label: tab.title,
    });
  }
  renderTabBar();

  if (status.fell_back_to_raw_display) {
    showRawDisplayFallbackNotice(tab.title);
  }
}

/**
 * 読み込み中の対象があるかどうかに応じて、`list_targets` ポーリングの
 * 開始・停止を切り替える（P07-2。「読み込み中のみ 500ms 間隔」）。
 */
function syncPolling() {
  const hasLoadingTarget = state.sessionTargets.some(
    (session) => session.status.kind === "loading",
  );
  if (hasLoadingTarget) {
    ensurePolling();
  } else {
    stopPolling();
  }
}

function ensurePolling() {
  if (state.pollTimer !== null) {
    return;
  }
  state.pollTimer = setInterval(() => {
    refreshTargets();
  }, POLL_INTERVAL_MS);
}

function stopPolling() {
  if (state.pollTimer === null) {
    return;
  }
  clearInterval(state.pollTimer);
  state.pollTimer = null;
}

/** 「ファイルを開く」ボタンのクリックハンドラー（`PROD-016`／`LOG-020`）。 */
async function handleOpenFileButtonClick() {
  elements.openFileButton.disabled = true;
  try {
    const response = await invokeOpenLogFile();
    if (response.kind === "cancelled") {
      // 利用者がダイアログをキャンセルした。正常応答なので何もしない。
      return;
    }
    await applyLoadAttemptResponse(response);
  } catch (error) {
    console.error("open_log_file の呼び出しに失敗しました:", error);
    showErrorBanner("ログファイルを開く処理でエラーが発生しました。");
  } finally {
    elements.openFileButton.disabled = false;
  }
}

/**
 * 「時系列統合」トグルのクリックハンドラー（P09-1、
 * `LOG-007`〜`LOG-008`）。
 *
 * - OFF -> ON: 現在開いている全ソースを横断する統合表示集合を構築し、統合
 *   タブ（1つ）を表示する（`LOG-015`。分割表示は作らない）。
 * - ON -> OFF: 統合表示集合を破棄し、直前にアクティブだったファイル別タブ
 *   （無ければ空表示）へ戻す。
 *
 * いずれの操作も参照対象ファイルそのものは変更しない（`ERR-003`）。
 */
async function handleMergedViewToggleClick() {
  elements.mergedViewToggle.disabled = true;
  try {
    if (state.mergedViewEnabled) {
      await invokeDisableMergedView();
      state.mergedViewEnabled = false;
      restoreActiveTabView();
    } else {
      const handle = await invokeEnableMergedView();
      state.mergedViewEnabled = true;
      logViewer.activate({
        display_set_id: handle.display_set_id,
        generation: handle.generation,
        total_items: Number(handle.total_items),
        source_label: "時系列統合",
        is_merged: true,
      });
    }
  } catch (error) {
    console.error("時系列統合表示の切り替えに失敗しました:", error);
    showErrorBanner("時系列統合表示の切り替えに失敗しました。");
  } finally {
    elements.mergedViewToggle.disabled = false;
    updateMergedViewToggleLabel();
    renderTabBar();
  }
}

/** 「時系列統合」トグルの表示ラベル・押下状態を `state.mergedViewEnabled` へ同期する。 */
function updateMergedViewToggleLabel() {
  elements.mergedViewToggle.textContent = `時系列統合: ${state.mergedViewEnabled ? "ON" : "OFF"}`;
  elements.mergedViewToggle.setAttribute("aria-pressed", String(state.mergedViewEnabled));
}

/**
 * 統合表示を OFF にした際、現在アクティブなタブ（無ければ空表示）を
 * ビュー領域へ再表示する。
 */
function restoreActiveTabView() {
  const active = getActiveTab(state.tabs);
  if (active) {
    logViewer.activate({
      display_set_id: active.displaySetId,
      generation: active.generation,
      total_items: active.totalItems,
      source_label: active.title,
    });
  } else {
    logViewer.showEmpty();
  }
}

/**
 * 左ペインの行クリックハンドラー。行の状態に応じて分岐する。
 *
 * @param {TargetRow} row
 */
async function handleTargetRowClick(row) {
  if (row.status.kind === "ready" || row.status.kind === "cancelled_partial") {
    activateExistingTab(row.targetId);
    return;
  }
  if (row.status.kind === "not_opened") {
    await handleOpenConfiguredRow(row.sourceName);
    return;
  }
  // "loading"・"error"・"changed" は行クリックでは何もしない
  // （"error"／"cancelled_partial" は専用の再試行ボタン、"loading" は専用の
  // キャンセルボタンを使う。ERR-001: 他の対象の操作に影響しない）。
}

/**
 * 設定由来の未オープン行を開く（`CFG-003`／`PROD-006`）。
 *
 * @param {string} name
 */
async function handleOpenConfiguredRow(name) {
  const pendingKey = `configured:${name}`;
  if (state.pendingKeys.has(pendingKey)) {
    return;
  }
  state.pendingKeys.add(pendingKey);
  try {
    const response = await invokeOpenConfigDataSource(name);
    await applyLoadAttemptResponse(response);
  } catch (error) {
    console.error("open_config_data_source の呼び出しに失敗しました:", error);
    showErrorBanner(`"${name}" を開く処理でエラーが発生しました。`);
  } finally {
    state.pendingKeys.delete(pendingKey);
  }
}

/**
 * 失敗・キャンセル済みの対象の再試行（`LOG-027`）。`manualProfile` を渡すと
 * `LOG-022` の手動プロファイル指定で、`manualDatetimeFormat` を渡すと同じく
 * 手動の日時書式指定で開き直す（`buildReparseControl` の「選んで再解析」
 * 操作。P07-2）。両方を同時に渡すこともできる。
 *
 * どちらの手動指定もこの1回の読み込み要求限りで、Rust 側は保持しない
 * （`src-tauri/src/targets.rs` の `retry_target`）。一覧の「再試行」ボタンは
 * 引数なしで呼ぶため、自動解決・自動判定へ戻る。
 *
 * @param {number} targetId
 * @param {string | null} [manualProfile]
 * @param {string | null} [manualDatetimeFormat]
 */
async function handleRetryClick(targetId, manualProfile, manualDatetimeFormat) {
  const pendingKey = `target:${targetId}`;
  if (state.pendingKeys.has(pendingKey)) {
    return;
  }
  state.pendingKeys.add(pendingKey);
  try {
    const response = await invokeRetryTarget(targetId, manualProfile, manualDatetimeFormat);
    if (response.kind === "not_found") {
      // 一覧から既に除去されていた（他操作との競合。ERR-001 の考え方に従い、
      // 静かに一覧を再同期するだけに留める）。
      await refreshTargets();
      return;
    }
    await applyLoadAttemptResponse(response);
  } catch (error) {
    console.error("retry_target の呼び出しに失敗しました:", error);
    showErrorBanner("対象の再試行でエラーが発生しました。");
  } finally {
    state.pendingKeys.delete(pendingKey);
  }
}

/**
 * アクセス拒否エラー表示の「管理者として新しいウィンドウで開く」ボタンの
 * クリックハンドラー（`PRIV-002`〜`004`、P11-2）。誤用防止のため、アクセス
 * 拒否時（`row.status.accessDenied === true`）だけこのボタンが表示される
 * （`buildTargetRowElement` 参照）。
 *
 * `launch_elevated` は新しい昇格済みプロセスを起動するだけで、現在の
 * プロセス（このウィンドウ）は開いたまま維持される（`PRIV-003`）。既存の
 * タブ・表示位置・解析済みデータは自動転送しないため、新しいウィンドウでは
 * 対象を選び直す必要がある旨を案内する。
 */
async function handleElevateClick() {
  try {
    const response = await invokeLaunchElevated();
    if (response.kind === "launched") {
      showInfoBanner(
        "管理者として新しいウィンドウを起動しました。新しいウィンドウで対象を選び直してください" +
          "（このウィンドウのタブや表示位置は自動的には引き継がれません）。",
      );
      return;
    }
    if (response.kind === "cancelled") {
      // PRIV-004: キャンセルは異常ではない。元プロセスは何も変わらない。
      showInfoBanner("昇格をキャンセルしました。このウィンドウはそのまま操作できます。");
      return;
    }
    showErrorBanner(`管理者として新しいウィンドウを開けませんでした: ${response.reason ?? ""}`);
  } catch (error) {
    console.error("launch_elevated の呼び出しに失敗しました:", error);
    showErrorBanner("管理者として新しいウィンドウを開く処理でエラーが発生しました。");
  }
}

/**
 * 読み込み中の対象にキャンセルを要求する（P07-2）。
 *
 * @param {number} targetId
 */
async function handleCancelClick(targetId) {
  try {
    await invokeCancelLoad(targetId);
  } catch (error) {
    console.error("cancel_load の呼び出しに失敗しました:", error);
    showErrorBanner("キャンセル要求の送信でエラーが発生しました。");
    return;
  }
  // キャンセルはチャンク境界で確認されるため、応答直後はまだ loading の
  // ままのことがある。ensurePolling 済み（loading があるため）のはずだが、
  // 念のため即座に1回再取得しておく。
  await refreshTargets();
}

/**
 * `open_log_file` / `open_config_data_source` / `retry_target` はいずれも
 * `loading` / `failed` を共通の形（`target_id`・`error` 等）で返す
 * （P07-2 により非同期化。読み込みそのものが完了する前に応答するため、
 * 従来の `opened` は廃止した）。ここで一括して処理する。
 *
 * @param {{ kind: "loading" | "failed", [key: string]: unknown }} response
 */
async function applyLoadAttemptResponse(response) {
  if (response.kind === "loading") {
    const targetId = /** @type {number} */ (response.target_id);
    // 完了・失敗をポーリングで検出したら、タブを開く／エラーを表示する
    // （processCompletedAutoActivations）。
    state.pendingAutoActivate.add(targetId);
    state.pendingErrorNotice.add(targetId);
    await refreshTargets();
    return;
  }

  // failed（同期的に判明した失敗）。ファイル選択ダイアログ自体の失敗や
  // 設定に存在しない名前のように一覧へ行が残らないものと、フォルダ未対応
  // （登録済みの行が Error へ遷移し、行に理由と「再試行」が残る）の両方が
  // ここへ来る。`ERR-002` の5要素をそのままモーダルダイアログへ表示する
  // （フルパスをマスキングしない）。
  showTargetError(/** @type {import("./targets.js").UserFacingErrorDto} */ (response.error));
  await refreshTargets();
}

/**
 * 既に開いているタブへ切り替える。
 *
 * @param {number | null} targetId
 */
function activateExistingTab(targetId) {
  if (targetId === null) {
    return;
  }
  const tab = state.tabs.tabs.find((candidate) => candidate.targetId === targetId);
  if (!tab) {
    // 通常は起こらない（"ready" 状態の行には必ず対応するタブがある設計。
    // 一覧の再同期タイミングのずれに備えた防御）。
    return;
  }
  state.tabs = setActiveTab(state.tabs, targetId);
  // P09-1: 統合表示 ON の間はファイル別タブへ切り替えない（LOG-015）。
  if (!state.mergedViewEnabled) {
    logViewer.activate({
      display_set_id: tab.displaySetId,
      generation: tab.generation,
      total_items: tab.totalItems,
      source_label: tab.title,
    });
  }
  renderTabBar();
}

/**
 * タブを閉じる（`close_target`）。コア側の表示集合はそのまま残る（完全な
 * クローズは P06 結合後の後続課題。`src-tauri/src/targets.rs` 参照）。
 *
 * @param {number} targetId
 */
async function handleTabClose(targetId) {
  try {
    await invokeCloseTarget(targetId);
  } catch (error) {
    console.error("close_target の呼び出しに失敗しました:", error);
    showErrorBanner("タブを閉じる処理でエラーが発生しました。");
    return;
  }

  state.tabs = closeTab(state.tabs, targetId);
  // P09-1: 統合表示 ON の間はビュー領域を統合表示のまま維持する（LOG-015。
  // 統合タブには閉じるボタンが無いため、通常この分岐は OFF のときだけ通る）。
  if (!state.mergedViewEnabled) {
    restoreActiveTabView();
  }
  renderTabBar();

  await refreshTargets();
}

/** 左ペイン（参照対象一覧）を再描画する。 */
function renderTargetList() {
  const rows = buildTargetRows(state.dataSourceNames, state.sessionTargets);

  elements.targetList.textContent = "";
  const fragment = document.createDocumentFragment();
  for (const row of rows) {
    fragment.appendChild(buildTargetRowElement(row));
  }
  elements.targetList.appendChild(fragment);
}

/** 状態表示の日本語ラベル（`TargetRowStatus.kind` -> 表示文字列）。 */
const STATUS_LABELS = {
  not_opened: "未読み込み",
  loading: "読み込み中…",
  ready: "読み込み済み",
  cancelled_partial: "キャンセル済み（部分読み込み）",
  error: "エラー",
  // P06 の再構築通知（LOG-023／LOG-028）が届くようになった時点で使う予約状態
  // （src/targets.js の doc コメント参照）。
  changed: "変更済み",
};

/**
 * アクセス拒否時（`PRIV-002`、P11-1）の一覧表示ラベル。汎用の `STATUS_LABELS.error`
 * とは別に、昇格で再試行できることを明示する。
 */
const ACCESS_DENIED_STATUS_LABEL = "アクセス拒否（昇格で再試行可）";

/**
 * バイト数を読みやすい単位（KB／MB／GB）の日本語表示へ整形する。
 *
 * @param {number} bytes
 * @returns {string}
 */
function formatBytes(bytes) {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(1)} ${units[unitIndex]}`;
}

/**
 * 読み込み中の進捗表示文字列を組み立てる（P07-2）。総量が判明していれば
 * 割合を、不明なら読み込み済みバイト数だけを示す。
 *
 * @param {{ doneBytes: number, totalBytes: number | null } | null} progress
 * @returns {string}
 */
function formatLoadingProgress(progress) {
  if (!progress) {
    return STATUS_LABELS.loading;
  }
  if (progress.totalBytes === null || progress.totalBytes <= 0) {
    return `${STATUS_LABELS.loading}（${formatBytes(progress.doneBytes)}）`;
  }
  const percent = Math.min(
    100,
    Math.round((progress.doneBytes / progress.totalBytes) * 100),
  );
  return `${STATUS_LABELS.loading}（${percent}%、${formatBytes(progress.doneBytes)} / ${formatBytes(
    progress.totalBytes,
  )}）`;
}

/**
 * 左ペイン1行分の DOM 要素を作る。
 *
 * @param {TargetRow} row
 */
function buildTargetRowElement(row) {
  const item = document.createElement("li");
  item.className = `target-row target-row--${row.status.kind}`;

  const main = document.createElement("div");
  main.className = "target-row__main";
  main.setAttribute("role", "button");
  main.tabIndex = 0;
  main.addEventListener("click", () => handleTargetRowClick(row));
  main.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      handleTargetRowClick(row);
    }
  });

  const name = document.createElement("span");
  name.className = "target-row__name";
  name.textContent = row.displayName;
  main.appendChild(name);

  const origin = document.createElement("span");
  origin.className = "target-row__origin";
  origin.textContent = row.origin === "configured" ? "設定" : "アドホック";
  main.appendChild(origin);

  const status = document.createElement("span");
  status.className = `target-row__status target-row__status--${row.status.kind}`;
  status.textContent = statusLabelFor(row.status);
  main.appendChild(status);

  item.appendChild(main);

  if (row.status.kind === "loading" && row.targetId !== null) {
    const targetId = row.targetId;
    const cancelButton = document.createElement("button");
    cancelButton.type = "button";
    cancelButton.className = "target-row__cancel";
    cancelButton.textContent = "キャンセル";
    cancelButton.addEventListener("click", (event) => {
      event.stopPropagation();
      handleCancelClick(targetId);
    });
    item.appendChild(cancelButton);
  }

  // LOG-028、ADR-0007: 「再読み込み」は Ready 状態の行に常設する
  // （update_pending の真偽に関わらず、いつでも呼び出せる操作。
  // handleReloadTargetClick の doc コメント参照）。update_pending が真の
  // ときだけ、前回の再読み込みが上限超過で反映されなかったことを付加情報
  // として伝える。
  if (row.status.kind === "ready" && row.targetId !== null) {
    const targetId = row.targetId;
    const updatePending = Boolean(row.status.ready?.updatePending);
    const reloadButton = document.createElement("button");
    reloadButton.type = "button";
    reloadButton.className = "target-row__reload";
    reloadButton.textContent = "再読み込み";
    reloadButton.title = updatePending
      ? "前回の再読み込みは上限超過のため反映されませんでした。他の対象を閉じてから再試行してください。"
      : "最新の内容を反映して開き直します。追記された内容はリアルタイムには反映されません。";
    reloadButton.addEventListener("click", (event) => {
      event.stopPropagation();
      handleReloadTargetClick(targetId);
    });
    item.appendChild(reloadButton);
  }

  if ((row.status.kind === "error" || row.status.kind === "cancelled_partial") && row.targetId !== null) {
    const targetId = row.targetId;
    const retryButton = document.createElement("button");
    retryButton.type = "button";
    retryButton.className = "target-row__retry";
    retryButton.textContent = "再試行";
    retryButton.addEventListener("click", (event) => {
      event.stopPropagation();
      handleRetryClick(targetId);
    });
    item.appendChild(retryButton);
  }

  // PRIV-002〜004、P11-2: アクセス拒否時だけ「管理者として新しいウィンドウで
  // 開く」ボタンを表示する（誤用防止。PRIV-001 の趣旨「常時管理者権限を
  // 要求しない」を損なわないため）。
  if (row.status.kind === "error" && row.status.accessDenied) {
    const elevateButton = document.createElement("button");
    elevateButton.type = "button";
    elevateButton.className = "target-row__elevate";
    elevateButton.textContent = "管理者として新しいウィンドウで開く";
    elevateButton.addEventListener("click", (event) => {
      event.stopPropagation();
      handleElevateClick();
    });
    item.appendChild(elevateButton);
  }

  if (row.status.kind === "error") {
    const reason = document.createElement("p");
    reason.className = "target-row__error-reason";
    reason.textContent = row.status.error?.reason ?? "";
    item.appendChild(reason);
  }

  const fellBackToRawDisplay =
    (row.status.kind === "ready" && row.status.ready?.fellBackToRawDisplay) ||
    (row.status.kind === "cancelled_partial" && row.status.cancelledPartial?.fellBackToRawDisplay);
  if (fellBackToRawDisplay && row.targetId !== null) {
    item.appendChild(buildReparseControl(row.targetId));
  }

  return item;
}

/**
 * @param {import("./targets.js").TargetRowStatus} status
 * @returns {string}
 */
function statusLabelFor(status) {
  if (status.kind === "ready" && status.ready) {
    const base = `${STATUS_LABELS.ready}（${status.ready.totalItems.toLocaleString("ja-JP")} 行）`;
    // ADR-0007: 前回の明示的な再読み込みが上限超過で拒否され、旧
    // スナップショットの表示を維持したまま「更新未反映」になっている
    // （src/targets.js の updatePending）。
    return status.ready.updatePending ? `${base}・更新未反映` : base;
  }
  if (status.kind === "cancelled_partial" && status.cancelledPartial) {
    return `${STATUS_LABELS.cancelled_partial}（${status.cancelledPartial.totalItems.toLocaleString(
      "ja-JP",
    )} 行）`;
  }
  if (status.kind === "loading") {
    return formatLoadingProgress(status.loadingProgress ?? null);
  }
  if (status.kind === "error" && status.accessDenied) {
    // PRIV-002、P11-1: 一覧に「アクセス拒否（昇格で再試行可）」を表示する。
    return ACCESS_DENIED_STATUS_LABEL;
  }
  return STATUS_LABELS[status.kind];
}

/**
 * `LOG-022` の「選んで再解析」操作（P07-2）。生表示へ退避した
 * 対象（`fell_back_to_raw_display`）だけに表示する。既定はどちらの選択も
 * 「自動」側（選択肢の先頭、という裁定）で、モーダルにはしない（常設の行内
 * UI）。
 *
 * プロファイルと日時書式を別々のセレクトにしているのは、この2つが独立した
 * 指定だからである。設定ファイルへプロファイルを書いていないファイル
 * （アドホックに開いた `LOG-DT-004` のみのログなど）は、プロファイルの一覧に
 * 選ぶべき項目が無く、書式だけを選べる必要がある。
 * 逆に `Ambiguous`／`ManualNotFound` による生表示退避は書式だけでは解けず、
 * プロファイルの選択が要る（`hakutaku_core::LoadControl::
 * manual_datetime_format` の doc コメント）。
 *
 * @param {number} targetId
 */
function buildReparseControl(targetId) {
  const container = document.createElement("div");
  container.className = "target-row__reparse";

  const label = document.createElement("span");
  label.className = "target-row__reparse-label";
  label.textContent = "日時未解析の生表示です。";
  container.appendChild(label);

  const profileSelect = buildReparseSelect(
    `reparse-profile-${targetId}`,
    "プロファイル",
    "指定しない",
    state.logProfileNames.map((name) => ({ value: name, text: name })),
    container,
  );

  const formatSelect = buildReparseSelect(
    `reparse-datetime-format-${targetId}`,
    "日時書式",
    "自動判定",
    // 要件 ID だけでは何の書式か分からないため、パターンを併記する
    // （例: LOG-DT-004（YYYY/MM/DD HH:mm:ss:SS））。
    state.datetimeFormats.map((format) => ({
      value: format.id,
      text: `${format.id}（${format.pattern}）`,
    })),
    container,
  );

  const applyButton = document.createElement("button");
  applyButton.type = "button";
  applyButton.className = "target-row__reparse-apply";
  applyButton.textContent = "再解析";
  applyButton.addEventListener("click", (event) => {
    event.stopPropagation();
    const selectedProfile = profileSelect.value;
    const selectedFormat = formatSelect.value;
    if (!selectedProfile && !selectedFormat) {
      // どちらも既定のままなら、再解析しても現在と同じ結果にしかならない
      // （生表示のまま）。無駄な読み込みを避けるため何もしない。
      return;
    }
    handleRetryClick(targetId, selectedProfile || null, selectedFormat || null);
  });

  container.appendChild(applyButton);
  return container;
}

/**
 * `buildReparseControl` のセレクト1つ分（ラベルと「指定しない」相当の既定
 * 選択肢を含む）を組み立て、`container` へ追加する。
 *
 * ラベルを `aria-label` ではなく可視の `<label>` にしているのは、セレクトが
 * 2つ並ぶため、見た目だけでどちらが何の選択か区別できる必要があるからである。
 *
 * @param {string} id セレクトの DOM ID（`<label for>` と対応させる）。
 * @param {string} labelText 可視ラベルの文言。
 * @param {string} defaultOptionText 先頭（既定）の選択肢の文言。値は空文字。
 * @param {{ value: string, text: string }[]} options 追加する選択肢。
 * @param {HTMLElement} container 追加先。
 * @returns {HTMLSelectElement}
 */
function buildReparseSelect(id, labelText, defaultOptionText, options, container) {
  const label = document.createElement("label");
  label.className = "target-row__reparse-field-label";
  label.htmlFor = id;
  label.textContent = `${labelText}:`;
  container.appendChild(label);

  const select = document.createElement("select");
  select.id = id;
  select.className = "target-row__reparse-select";

  const defaultOption = document.createElement("option");
  defaultOption.value = "";
  defaultOption.textContent = defaultOptionText;
  select.appendChild(defaultOption);

  for (const option of options) {
    const element = document.createElement("option");
    element.value = option.value;
    element.textContent = option.text;
    select.appendChild(element);
  }
  // 行そのもののクリック（対象の切り替え）へ伝播させない。
  select.addEventListener("click", (event) => event.stopPropagation());

  container.appendChild(select);
  return select;
}

/**
 * 上部のタブバーを再描画する（`LOG-015`。分割表示は作らない）。統合表示
 * （P09-1）ON の間は、ファイル別タブの代わりに単一の疑似タブを表示する。
 */
function renderTabBar() {
  elements.tabBar.textContent = "";
  const fragment = document.createDocumentFragment();
  if (state.mergedViewEnabled) {
    fragment.appendChild(buildMergedTabElement());
  } else {
    for (const tab of state.tabs.tabs) {
      fragment.appendChild(buildTabElement(tab));
    }
  }
  elements.tabBar.appendChild(fragment);
}

/**
 * 統合表示 ON 時に表示する単一の疑似タブ（P09-1、`LOG-015`: 分割表示を
 * 作らないため、常に1つだけ表示する）。閉じるボタンは持たない（OFF にする
 * 操作はツールバーのトグルで行う）。
 */
function buildMergedTabElement() {
  const element = document.createElement("div");
  element.className = "tab tab--active tab--merged";
  element.setAttribute("role", "tab");
  element.setAttribute("aria-selected", "true");

  const title = document.createElement("span");
  title.className = "tab__title";
  title.textContent = "時系列統合";
  element.appendChild(title);

  return element;
}

/**
 * タブ1件分の DOM 要素を作る。
 *
 * @param {Tab} tab
 */
function buildTabElement(tab) {
  const isActive = state.tabs.activeTargetId === tab.targetId;

  const element = document.createElement("div");
  element.className = `tab${isActive ? " tab--active" : ""}`;
  element.setAttribute("role", "tab");
  element.setAttribute("aria-selected", String(isActive));
  element.tabIndex = 0;
  element.addEventListener("click", () => activateExistingTab(tab.targetId));
  element.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      activateExistingTab(tab.targetId);
    }
  });

  const title = document.createElement("span");
  title.className = "tab__title";
  title.textContent = tab.title;
  element.appendChild(title);

  const closeButton = document.createElement("button");
  closeButton.type = "button";
  closeButton.className = "tab__close";
  closeButton.setAttribute("aria-label", `${tab.title} を閉じる`);
  closeButton.textContent = "×";
  closeButton.addEventListener("click", (event) => {
    event.stopPropagation();
    handleTabClose(tab.targetId);
  });
  element.appendChild(closeButton);

  return element;
}
