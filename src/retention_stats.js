// 保持上限の内部状態の観測モジュール（P04-2）。
//
// PERF-012 は「フロントエンドが保持する全行データ」を対象とし、自前キャッシュ
// だけでなく、表示ライブラリ内部の行モデル、DOM が保持する行参照、購読・
// イベントハンドラーが保持する行参照までを含める（tasks/phase-04-vertical-slice.md
// 作業項目4の対象表）。この実装（ADR-0006）では、行データの保持点（chunkCache）、
// DOM 行ノードの生成/削除、範囲取得の発生のすべてを自前コード（log_view.js）が
// 直接所有・呼び出しているため、ここに集約した計数だけで観測対象を過不足なく
// カバーできる（サードパーティの表示ライブラリを使わないため、内部行モデルの
// 生成/解放計数のような間接計測は不要）。
//
// このモジュール自身は状態を書き換えず、log_view.js からの記録呼び出しを
// 受けて計数を更新するだけの薄い層に徹する。P04-3 の計測モードが
// `window.__hakutakuStats` を直接読み、上限順守の検証に使う。
//
// `window.__hakutakuStats` の公開は計測モードのときだけ行う（Issue #46）。
// 計測モードは開発・検証専用であり、通常の利用者向け起動でこの観測 API を
// WebView のグローバルへ置く必要はない（SEC-012 の「フロントエンドへ与える
// 能力を必要最小限に保つ」趣旨。公開の可否は `enableWindowPublication` を
// 参照）。

let retainedRows = 0;
let retainedBytes = 0;
let retainedChunks = 0;
let evictedChunksTotal = 0;
let refetchTotal = 0;
let domRowNodeCount = 0;
let rowNodesCreatedTotal = 0;
let rowNodesRemovedTotal = 0;
let inFlightFetches = 0;

/**
 * キャッシュへチャンクを追加した際に呼び出す（保持行数・保持バイト数・保持
 * チャンク数を増やす）。
 *
 * @param {number} rowCount
 * @param {number} byteCount
 */
export function recordChunkCached(rowCount, byteCount) {
  retainedRows += rowCount;
  retainedBytes += byteCount;
  retainedChunks += 1;
}

/**
 * キャッシュからチャンクを破棄した際に呼び出す。保持上限超過による破棄、
 * 世代不一致によるキャッシュ全破棄のいずれの経路でも呼び出す。
 *
 * @param {number} rowCount
 * @param {number} byteCount
 */
export function recordChunkEvicted(rowCount, byteCount) {
  retainedRows -= rowCount;
  retainedBytes -= byteCount;
  retainedChunks -= 1;
  evictedChunksTotal += 1;
}

/** 以前に破棄したチャンクを再取得した際に呼び出す（累計カウンタ）。 */
export function recordRefetch() {
  refetchTotal += 1;
}

/** 範囲取得（`fetch_log_range`）を1回開始した際に呼び出す。 */
export function recordFetchStart() {
  inFlightFetches += 1;
}

/** 範囲取得が1回完了（成功・失敗いずれも）した際に呼び出す。 */
export function recordFetchEnd() {
  inFlightFetches = Math.max(0, inFlightFetches - 1);
}

/**
 * 行 DOM ノードを生成した際に呼び出す。
 *
 * @param {number} count 生成した個数。
 */
export function recordRowNodesCreated(count) {
  rowNodesCreatedTotal += count;
  domRowNodeCount += count;
}

/**
 * 行 DOM ノードを削除した際に呼び出す。
 *
 * @param {number} count 削除した個数。
 */
export function recordRowNodesRemoved(count) {
  rowNodesRemovedTotal += count;
  domRowNodeCount = Math.max(0, domRowNodeCount - count);
}

/**
 * 現在の内部状態のスナップショットを返す（PERF-012 の観測点。P04-3 が
 * 保持上限の順守を判定するために使う）。
 *
 * @returns {{
 *   retainedRows: number,
 *   retainedBytes: number,
 *   retainedChunks: number,
 *   evictedChunksTotal: number,
 *   refetchTotal: number,
 *   domRowNodeCount: number,
 *   rowNodesCreatedTotal: number,
 *   rowNodesRemovedTotal: number,
 *   inFlightFetches: number,
 * }}
 */
export function getStats() {
  return {
    retainedRows,
    retainedBytes,
    retainedChunks,
    evictedChunksTotal,
    refetchTotal,
    domRowNodeCount,
    rowNodesCreatedTotal,
    rowNodesRemovedTotal,
    inFlightFetches,
  };
}

/**
 * `window.__hakutakuStats` を公開してよいかどうか。既定は「公開しない」。
 *
 * 計測モードの判定（`get_measurement_mode`）は Rust 側にしかないため、
 * その結果を受け取る `src/main.js` が `enableWindowPublication()` で立てる。
 */
let windowPublicationEnabled = false;

/**
 * `window.__hakutakuStats` の公開を許可する（Issue #46）。
 *
 * 計測モード（`get_measurement_mode` が `active: true`）のときだけ、
 * `src/main.js` の起動フローから呼び出す。呼び出し順に依存しないよう、
 * ここでも公開を試みる（`initLogView` の `publishToWindow()` が先に走っていて
 * も、後から走っても、計測モードなら公開される）。
 */
export function enableWindowPublication() {
  windowPublicationEnabled = true;
  publishToWindow();
}

/**
 * 計測モードのときだけ、`window.__hakutakuStats` を読み取り専用で公開する
 * （P04-3 の計測モードが直接 `window.__hakutakuStats.getStats()` を呼び出せる
 * ようにするため）。通常の利用者向け起動では何もしない（Issue #46。モジュール
 * 冒頭のコメント参照）。
 *
 * `Object.freeze` は公開するオブジェクト自身（`getStats` プロパティの再代入）
 * だけを禁止する。`getStats` は呼び出しのたびに最新のスナップショットを生成
 * する関数のため、公開オブジェクトを凍結しても返す値が古くなることはない。
 */
export function publishToWindow() {
  if (!windowPublicationEnabled) {
    return;
  }
  if (typeof window === "undefined") {
    return;
  }
  window.__hakutakuStats = Object.freeze({
    getStats,
  });
}
