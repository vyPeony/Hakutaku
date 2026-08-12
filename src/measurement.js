// 計測モード専用のスクリプト（P04-3）。
//
// **開発・検証専用であり、利用者向け機能ではない。** `HAKUTAKU_MEASURE_FILE`
// 環境変数を設定して起動した場合だけ、src/main.js の起動フローから
// `runMeasurement` が自動的に呼び出される。通常の利用者向け起動では
// `get_measurement_mode` が `active: false` を返すため、このモジュールの処理は
// 一切実行されない。
//
// 実行内容（tasks/phase-04-vertical-slice.md 作業項目3・4・5）:
//   1. `open_measurement_file` で計測用ファイルを開く
//   2. 転送コストの実測（max_items を変えて `fetch_log_range` を反復）
//   3. 連続スクロール検証（PERF-012 の保持上限のゲート判定）
//   4. `record_measurement_results` で結果を logs へ送信
//   5. 完了をウィンドウタイトルへ反映する（外部からの終了判定用。アプリの
//      自動終了はしない。呼び出し側が taskkill する）
//
// 仮想スクロールの内部状態（chunkCache 等）は log_view.js だけが所有する
// （PERF-012 の「行データを保持する場所を1箇所に限定する」設計）。このモジュール
// は log_view.js が公開する `activateDisplaySet`（P07-1 で共通シェルの
// タブ切り替えとも共有する表示集合切り替え関数。通常の「ファイルを開く」
// ボタン経由と同じ内部状態遷移）と、実際の DOM 要素（#log-viewport）の
// scrollTop 操作だけを通じて、実際の仮想スクロール経路をそのまま計測する
// （内部関数を直接呼び出したり、内部状態を書き換えたりしない）。

import { activateDisplaySet } from "./log_view.js";
import { sumRawTextBytes } from "./virtual_scroll.js";

/** 転送コスト計測で試す max_items の値（作業項目3）。 */
const TRANSFER_MAX_ITEMS_VARIANTS = [64, 128, 256, 512];
/** max_items の値ごとに反復する回数（作業項目3「各20回」）。 */
const TRANSFER_SAMPLES_PER_VARIANT = 20;

/** 連続スクロール検証の1段階の間隔（作業項目4「例: 200msごと」）。 */
const SCROLL_STEP_INTERVAL_MS = 200;
/** 先頭→末尾→先頭の往復回数（作業項目4「全体を2往復」）。 */
const SCROLL_ROUND_TRIPS = 2;
/**
 * 1往復あたりの段階数の上限。
 *
 * 作業項目4は「200msごとに1画面分」を例示しているが、これを大規模ファイル
 * （P04-3 の実測対象は約30万行）へそのまま適用すると、1画面分の移動量が
 * 総スクロール範囲に対して小さすぎ、段階数が数万に達して計測全体が数十分〜
 * 数時間かかってしまう（計測完了は5分以内のポーリングを前提とする運用と
 * 両立しない）。そのため、1往復の段階数をこの上限に頭打ちにし、超える場合は
 * 1段階あたりの移動量を広げる（`computeStepPx` 参照）。小規模ファイルでは
 * 文字どおり「1画面分」の移動になる。
 */
const MAX_STEPS_PER_TRAVERSAL = 60;
/** 最後の取得が落ち着くのを待つ時間（スクロール終了後の最終スナップショット用）。 */
const SCROLL_SETTLE_WAIT_MS = 800;

/**
 * @param {string} command
 * @param {Record<string, unknown>} [args]
 */
function invoke(command, args) {
  return window.__TAURI_INTERNALS__.invoke(command, args);
}

function waitForAnimationFrame() {
  return new Promise((resolve) => requestAnimationFrame(resolve));
}

/** @param {number} ms */
function wait(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * 昇順に並んだ数値配列の中央値を返す。
 *
 * @param {number[]} sortedAscendingValues
 */
function median(sortedAscendingValues) {
  const n = sortedAscendingValues.length;
  if (n === 0) {
    return 0;
  }
  const mid = Math.floor(n / 2);
  return n % 2 === 0
    ? (sortedAscendingValues[mid - 1] + sortedAscendingValues[mid]) / 2
    : sortedAscendingValues[mid];
}

/**
 * 昇順に並んだ数値配列の p パーセンタイルを返す（最近接順位法）。
 *
 * @param {number[]} sortedAscendingValues
 * @param {number} p 0〜100。
 */
function percentile(sortedAscendingValues, p) {
  const n = sortedAscendingValues.length;
  if (n === 0) {
    return 0;
  }
  const index = Math.min(n - 1, Math.max(0, Math.ceil((p / 100) * n) - 1));
  return sortedAscendingValues[index];
}

/**
 * 転送コストを計測する（作業項目3）。max_items を変えて `fetch_log_range` を
 * 反復し、所要時間（`performance.now()`）と応答バイト概算（`raw_text` 合計）を
 * 記録する。
 *
 * JSON の直列化・逆直列化の内訳は Tauri IPC からは個別に取得できないため、
 * 応答サイズと所要時間の関係で代替する（結果にその旨を含める）。
 *
 * @param {{ displaySetId: number, generation: number, totalItems: number }} target
 */
async function measureTransferCost(target) {
  const { displaySetId, generation, totalItems } = target;
  const byVariant = [];

  for (const maxItems of TRANSFER_MAX_ITEMS_VARIANTS) {
    const samples = [];
    const maxStart = Math.max(0, totalItems - maxItems);

    for (let i = 0; i < TRANSFER_SAMPLES_PER_VARIANT; i += 1) {
      // 取得開始位置をファイル全体へ均等に分散させる（先頭付近だけに
      // 偏った計測にならないようにする）。
      const start =
        TRANSFER_SAMPLES_PER_VARIANT <= 1
          ? 0
          : Math.floor((maxStart * i) / (TRANSFER_SAMPLES_PER_VARIANT - 1));

      const startedAt = performance.now();
      const response = await invoke("fetch_log_range", {
        displaySetId,
        expectedGeneration: generation,
        start,
        maxItems,
      });
      const durationMs = performance.now() - startedAt;

      samples.push({
        start,
        itemsReturned: response.items.length,
        truncated: response.truncated,
        durationMs,
        approxResponseBytes: sumRawTextBytes(response.items),
      });
    }

    const durations = samples.map((sample) => sample.durationMs).sort((a, b) => a - b);
    byVariant.push({
      maxItems,
      sampleCount: samples.length,
      medianDurationMs: median(durations),
      p95DurationMs: percentile(durations, 95),
      minDurationMs: durations[0],
      maxDurationMs: durations[durations.length - 1],
      samples,
    });
  }

  return {
    note:
      "JSON の直列化・逆直列化の内訳は Tauri IPC 経由では個別に取得できないため、" +
      "応答バイト概算（raw_text の UTF-8 バイト数合計。JSON全体のオーバーヘッドは" +
      "含まない）と所要時間の関係で代替する。",
    byVariant,
  };
}

/**
 * 1段階あたりの移動量（px）を計算する。`MAX_STEPS_PER_TRAVERSAL` の doc
 * コメントを参照。
 *
 * @param {number} maxScrollTop
 * @param {number} clientHeight
 */
function computeStepPx(maxScrollTop, clientHeight) {
  const oneScreen = Math.max(1, clientHeight);
  if (maxScrollTop <= 0) {
    return oneScreen;
  }
  const screensTotal = maxScrollTop / oneScreen;
  if (screensTotal <= MAX_STEPS_PER_TRAVERSAL) {
    return oneScreen;
  }
  return Math.ceil(maxScrollTop / MAX_STEPS_PER_TRAVERSAL);
}

/**
 * 先頭→末尾→先頭を `roundTrips` 回繰り返す scrollTop の目標値列を作る。
 *
 * @param {number} maxScrollTop
 * @param {number} stepPx
 * @param {number} roundTrips
 * @returns {number[]}
 */
function buildRoundTripTargets(maxScrollTop, stepPx, roundTrips) {
  const targets = [];
  for (let trip = 0; trip < roundTrips; trip += 1) {
    for (let pos = stepPx; pos < maxScrollTop; pos += stepPx) {
      targets.push(pos);
    }
    targets.push(maxScrollTop);
    for (let pos = maxScrollTop - stepPx; pos > 0; pos -= stepPx) {
      targets.push(pos);
    }
    targets.push(0);
  }
  return targets;
}

/**
 * 連続スクロール検証（作業項目4、`PERF-012` のゲート判定。スクロール高
 * クランプの再検証で末尾到達性の確認を追加）。ビューポートの scrollTop をプログラムで
 * 先頭→末尾→先頭へ段階的に動かし、各段階で
 * `__hakutakuStats.getStats()`（retention_stats.js の内部状態観測 API）と、
 * 上下スペーサーの実際の高さ（DOM から直接読む）を記録する。全サンプルで
 * `retainedRows ≤ maxRows` かつ `retainedBytes ≤ maxBytes` であることと、
 * 破棄（`evictedChunksTotal`）・再取得（`refetchTotal`）が実際に発生した
 * ことを確認する。
 *
 * # 末尾到達性の確認（`reachedEndOfFile`）
 *
 * `maxScrollTop` は `viewport.scrollHeight`（ブラウザが実際にレイアウトへ
 * 反映した値。`log_view.js` の `computeSpacerHeightsForScroll` が構成する
 * スペーサ高さの合計を反映する）から計算しており、このスクロール検証の
 * 段階列（`targets`）はもともと scrollHeight ベースだった（変更不要）。
 * 2000万行規模で `scrollHeightPx` の実測値が理論値を大幅に下回っていた
 * こと自体がスクロール高クランプ対応の発端であり、「scrollTop を理論上の末尾まで動かせて
 * いるか」だけでは、`computeVisibleRangeForScroll` の比例写像が実際に
 * 表示集合の末尾（`endIndex === totalItems`）まで描画しているかを確認
 * できない（scrollHeight 自体が誤ってクランプされていた旧不具合の再発を
 * 見逃す）。そのため、各段階で下スペーサー（`#log-spacer-bottom`）の実際の
 * 高さも読み取り、`0px`（＝描画中の行が表示集合の末尾に達している。
 * `computeSpacerHeightsForScroll` の `bottomHeightPx` は `endIndex ===
 * totalItems` のとき常に厳密に0になる）になった段階が1回でもあれば
 * `reachedEndOfFile: true` とする。上スペーサー（`#log-spacer-top`）につい
 * ても対称に `reachedStartOfFile` を記録する。
 *
 * ## ラウンドトリップ走査だけでは末尾到達を確実に検出できなかった実測結果
 *
 * `maxScrollTop` は走査の開始時点で一度だけ読み取った固定値であり、以後
 * `stepCount`（最大120段階）×2往復・数十秒にわたって同じ固定の px 値へ向けて
 * `viewport.scrollTop` を設定し続ける。2000万行規模の実機検証で、この
 * 固定値ベースの走査だけでは `bottomSpacerHeightPx === 0` の段階が1回も
 * 観測できないことがあった（実測: 総行数の0.01〜0.04%程度、最大約1万行分、
 * 末尾に届かない状態が残った）。一方、実際の利用者操作
 * （Ctrl+End・`handleViewportKeydown` の `elements.viewport.scrollTop =
 * elements.viewport.scrollHeight`、または OS のスクロールバーつまみを
 * 一番下までドラッグする操作）は、いずれもその場で最新の `scrollHeight` を
 * 読み直してから使う（走査開始時の古い値を使い回さない）。そのため、
 * `verifyContinuousScroll` の最後に、Ctrl+End と全く同じ式
 * （`viewport.scrollTop = viewport.scrollHeight`）でその場で読み直した
 * ジャンプを1回行い、その結果だけを見て判定する
 * `reachedEndOfFileViaLiveJump`（`reachedStartOfFileViaLiveJump` も対称に）を
 * 別途記録する。こちらが実際の利用者操作と同じ経路を使った、より確実な
 * 末尾到達性の確認である。ラウンドトリップ走査中の `reachedEndOfFile`
 * （どの段階でも0pxを observe できたか）は診断用の参考値として残す。
 *
 * @param {{ maxRows: number, maxBytes: number }} retentionLimits
 */
async function verifyContinuousScroll(retentionLimits) {
  const viewport = document.getElementById("log-viewport");
  const topSpacer = document.getElementById("log-spacer-top");
  const bottomSpacer = document.getElementById("log-spacer-bottom");

  // 最初の描画（requestAnimationFrame）を待ってから、スペーサーの高さ
  // （totalItems 由来の scrollHeight）を読む。
  await waitForAnimationFrame();
  await wait(SCROLL_STEP_INTERVAL_MS);

  const clientHeight = viewport.clientHeight;
  const maxScrollTop = Math.max(0, viewport.scrollHeight - clientHeight);
  const stepPx = computeStepPx(maxScrollTop, clientHeight);
  const targets = buildRoundTripTargets(maxScrollTop, stepPx, SCROLL_ROUND_TRIPS);

  /** @param {HTMLElement} spacer @returns {number} */
  const spacerHeightPx = (spacer) => parseFloat(spacer.style.height) || 0;

  const samples = [];
  const startedAt = performance.now();
  for (const scrollTop of targets) {
    // 実際の利用者操作と同じ経路（scroll イベント→log_view.js の
    // scheduleRender）を通す。内部関数は直接呼び出さない。
    viewport.scrollTop = scrollTop;
    await wait(SCROLL_STEP_INTERVAL_MS);
    const stats = window.__hakutakuStats.getStats();
    samples.push({
      elapsedMs: performance.now() - startedAt,
      scrollTop,
      topSpacerHeightPx: spacerHeightPx(topSpacer),
      bottomSpacerHeightPx: spacerHeightPx(bottomSpacer),
      ...stats,
    });
  }

  // 最後の取得（fetch_log_range の往復）が落ち着くのを待ってから、最終
  // スナップショットを取る。
  await wait(SCROLL_SETTLE_WAIT_MS);
  const finalStats = window.__hakutakuStats.getStats();
  samples.push({
    elapsedMs: performance.now() - startedAt,
    scrollTop: viewport.scrollTop,
    topSpacerHeightPx: spacerHeightPx(topSpacer),
    bottomSpacerHeightPx: spacerHeightPx(bottomSpacer),
    ...finalStats,
  });

  const withinRetentionLimitsForAllSamples = samples.every(
    (sample) =>
      sample.retainedRows <= retentionLimits.maxRows &&
      sample.retainedBytes <= retentionLimits.maxBytes,
  );

  // 上下スペーサーが0pxになった（＝表示集合の先頭/末尾まで
  // 実際に描画が届いた）段階が1回でもあったかどうか（診断用の参考値。
  // 上のコメント「ラウンドトリップ走査だけでは…」参照。判定の主とはしない）。
  const reachedStartOfFile = samples.some((sample) => sample.topSpacerHeightPx === 0);
  const reachedEndOfFile = samples.some((sample) => sample.bottomSpacerHeightPx === 0);

  // 実際の利用者操作（Ctrl+End／Ctrl+Home）と全く同じ式で、その場で
  // 最新の scrollHeight を読み直してジャンプする（上のコメント参照）。これが
  // 末尾・先頭到達性の主判定。
  //
  // 単発の1回だけでは、2000万行規模の実機検証で `viewport.scrollTop =
  // viewport.scrollHeight` を実行した直後に読み直した `scrollTop` が、
  // その時点で読んだ `scrollHeight`（layout 側の値）よりも明らかに小さい値
  // （実測で理論値より約36,500px、0.15%ほど小さい）にとどまることがあった。
  // WebView2（Chromium）の layout 側の値（`scrollHeight`）と、実際に
  // 適用されるスクロール可能範囲（コンポジタ側が使う値と推測される。詳細な
  // 内部機構は未確定）が、極端に大きなスクロール高さ・頻繁な再描画の下では
  // 一時的に同期しきれないことがあると考えられる。そのため `LIVE_JUMP_MAX_ATTEMPTS`
  // 回まで「読み直して設定→待つ」を繰り返し、コンポジタ側が追いつくのを待つ
  // （毎回 `viewport.scrollHeight` を読み直すため、追いつくたびに目標が
  // 正しい値へ近づく形で自己修正する）。
  const reachedEndOfFileViaLiveJump = await jumpAndVerifySpacerReachesZero({
    getTargetScrollTop: () => viewport.scrollHeight, // Ctrl+End と同じ式
    spacer: bottomSpacer,
  });
  const liveJumpEndSample = {
    scrollTop: viewport.scrollTop,
    scrollHeightAtJumpPx: viewport.scrollHeight,
    topSpacerHeightPx: spacerHeightPx(topSpacer),
    bottomSpacerHeightPx: spacerHeightPx(bottomSpacer),
  };

  const reachedStartOfFileViaLiveJump = await jumpAndVerifySpacerReachesZero({
    getTargetScrollTop: () => 0, // Ctrl+Home と同じ式
    spacer: topSpacer,
  });
  const liveJumpStartSample = {
    scrollTop: viewport.scrollTop,
    topSpacerHeightPx: spacerHeightPx(topSpacer),
    bottomSpacerHeightPx: spacerHeightPx(bottomSpacer),
  };

  /**
   * `getTargetScrollTop()` が返す値（呼び出しのたびに最新の DOM から読み直す）
   * へ `viewport.scrollTop` を設定し、`spacer` の高さが0pxになるまで
   * `LIVE_JUMP_MAX_ATTEMPTS` 回まで再試行する。
   *
   * @param {{ getTargetScrollTop: () => number, spacer: HTMLElement }} args
   * @returns {Promise<boolean>}
   */
  async function jumpAndVerifySpacerReachesZero({ getTargetScrollTop, spacer }) {
    const LIVE_JUMP_MAX_ATTEMPTS = 8;
    const LIVE_JUMP_WAIT_MS = 500;
    for (let attempt = 0; attempt < LIVE_JUMP_MAX_ATTEMPTS; attempt += 1) {
      viewport.scrollTop = getTargetScrollTop();
      await wait(LIVE_JUMP_WAIT_MS);
      if (spacerHeightPx(spacer) === 0) {
        return true;
      }
    }
    return false;
  }

  return {
    retentionLimits,
    clientHeightPx: clientHeight,
    scrollHeightPx: viewport.scrollHeight,
    stepPx,
    stepCount: targets.length,
    sampleCount: samples.length,
    withinRetentionLimitsForAllSamples,
    evictedChunksTotal: finalStats.evictedChunksTotal,
    refetchTotal: finalStats.refetchTotal,
    evictionAndRefetchObserved:
      finalStats.evictedChunksTotal > 0 && finalStats.refetchTotal > 0,
    reachedStartOfFile,
    reachedEndOfFile,
    reachedStartOfFileViaLiveJump,
    reachedEndOfFileViaLiveJump,
    liveJumpStartSample,
    liveJumpEndSample,
    samples,
  };
}

/** 完了をウィンドウタイトルへ反映する（外部からの終了判定用）。 */
function markCompletion(success, detail) {
  document.title = success
    ? "Hakutaku [MEASUREMENT_DONE]"
    : `Hakutaku [MEASUREMENT_FAILED] ${detail ?? ""}`;
}

/**
 * 計測モードの本体。`src/main.js` から、`get_measurement_mode` が
 * `active: true` を返した場合にだけ呼び出される。
 *
 * @param {{ maxRows: number, maxBytes: number }} retentionLimits `get_config_status` から読み取った CFG-022 の現在値。
 */
export async function runMeasurement(retentionLimits) {
  console.info("計測モードを開始します。");
  try {
    const openResponse = await invoke("open_measurement_file");
    if (openResponse.kind !== "opened") {
      throw new Error(
        `計測用ファイルを開けませんでした: ${JSON.stringify(openResponse)}`,
      );
    }

    // log_view.js の内部状態（chunkCache・displaySetId 等）を、通常の
    // 「ファイルを開く」ボタン経由と同じ遷移で更新する（実際の仮想スクロール
    // 経路をそのまま計測するため。モジュール冒頭のコメント参照）。
    activateDisplaySet(openResponse);

    const target = {
      displaySetId: openResponse.display_set_id,
      generation: openResponse.generation,
      totalItems: Number(openResponse.total_items),
    };

    const transferCost = await measureTransferCost(target);
    const scrollVerification = await verifyContinuousScroll(retentionLimits);

    const results = {
      measuredAtIso: new Date().toISOString(),
      sourceLabel: openResponse.source_label,
      totalItems: target.totalItems,
      retentionLimits,
      transferCost,
      scrollVerification,
    };

    await invoke("record_measurement_results", {
      resultsJson: JSON.stringify(results),
    });

    console.info("計測モードが完了しました。");
    markCompletion(true);
  } catch (error) {
    console.error("計測モードの実行中にエラーが発生しました:", error);
    markCompletion(false, error instanceof Error ? error.message : String(error));
  }
}
