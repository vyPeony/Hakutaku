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
//   4. `record_measurement_results` で結果を logs へ送信する。途中でエラーに
//      なった場合も、同じ経路で失敗記録を送信する（`recordFailure`）
//
// 外部からの終了判定は、`logs/measurements/measurement-p04-*.json` の出現で
// 行う（成功・失敗のどちらでも書き出される）。アプリの自動終了はしない。
// 呼び出し側が taskkill する。ウィンドウタイトルを判定に使ってはならない:
// Tauri 2系は
// document.title をネイティブウィンドウのタイトルへ同期しない
// （`on_document_title_changed` ハンドラーを登録した場合だけ変更が Rust 側へ
// 通知される仕組みで、本アプリは登録していない）ため、外部プロセスから
// 見えるタイトルは起動時の「Hakutaku」のまま変化しない。
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
 * 操作を止めた後、表示が自走していないかを観測する時間（ms）。
 *
 * 自走は1フレームあたりの変位が小さくても、放置している間に積み上がって
 * 表示位置が流れていく。3秒は、その積み上がりが px 単位で明確に読み取れ、
 * かつ計測全体（外部から5分以内のポーリングで完了を検出する運用）を
 * 圧迫しない長さとして選ぶ。
 */
const IDLE_OBSERVATION_DURATION_MS = 3000;
/** 末尾ジャンプ直後のアイドル観測のサンプル間隔（ms）。 */
const END_IDLE_SAMPLE_INTERVAL_MS = 500;
/**
 * 相対移動の停止後のアイドル観測のサンプル間隔（ms）。
 *
 * 末尾側より短く取り、停止直後から始まる小さな変位も取りこぼさずに拾う。
 */
const RELATIVE_SCROLL_IDLE_SAMPLE_INTERVAL_MS = 250;
/** 相対移動検証の1回あたりの移動量（px）。ホイールの1操作相当の移動量を模す。 */
const RELATIVE_SCROLL_STEP_PX = 120;
/**
 * 相対移動検証の移動回数。
 *
 * 先頭から `log_view.js` の `BUFFER_ROWS`(50) × `ROW_HEIGHT_PX`(22) = 1,100px
 * を超えて初めて、上スペーサーの高さが0pxから伸び始める（＝行 DOM の
 * 全消し・再生成が上下両方向のスペーサー高さの変化を伴うようになる）。
 * Issue #21 の自走はこの領域から先で起きるため、累計移動量
 * （`RELATIVE_SCROLL_STEP_PX` × この回数 = 1,920px）が 1,100px を確実に
 * 超える回数を選ぶ。
 */
const RELATIVE_SCROLL_STEP_COUNT = 16;
/** 相対移動の間隔（ms）。連続したホイール操作に近い間隔で動かす。 */
const RELATIVE_SCROLL_STEP_INTERVAL_MS = 200;

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
 * サンプル列の `scrollTop` が基準値からどれだけ離れたかの最大値（px）を返す。
 *
 * 自走は一方向へ進むとは限らない（描画のたびに前後へ揺れることもある）ため、
 * 最初と最後の差ではなく、基準値からの絶対変位の最大値で表す。サンプルが
 * 無い場合は0を返す（観測できなかったことと「動かなかった」ことは
 * `sampleCount` で区別する）。
 *
 * @param {{ scrollTop: number }[]} observedSamples
 * @param {number} baselineScrollTop
 */
function maxScrollTopDeviationPx(observedSamples, baselineScrollTop) {
  return observedSamples.reduce(
    (maxSoFar, sample) => Math.max(maxSoFar, Math.abs(sample.scrollTop - baselineScrollTop)),
    0,
  );
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
 * ## 到達性の主判定を走査ではなく live jump に置く理由
 *
 * ラウンドトリップ走査の目標値列（`targets`）は、走査の開始時点で一度だけ
 * 読み取った `maxScrollTop` から組み立てた固定値であり、以後
 * `stepCount`（最大120段階）×2往復のあいだ同じ px 値へ向けて設定し続ける。
 * 一方、実際の利用者操作（Ctrl+End・`handleViewportKeydown` の
 * `elements.viewport.scrollTop = elements.viewport.scrollHeight`、または OS の
 * スクロールバーつまみを一番下までドラッグする操作）は、いずれもその場で
 * 最新の `scrollHeight` を読み直してから使う。固定の目標値列で末尾へ届いたか
 * を見ることは、利用者操作と同じ条件で末尾到達性を確かめることにはならない。
 *
 * そのため、`verifyContinuousScroll` の最後に、Ctrl+End と全く同じ式
 * （`viewport.scrollTop = viewport.scrollHeight`）でその場で読み直した
 * ジャンプを行い、その結果だけを見て判定する
 * `reachedEndOfFileViaLiveJump`（`reachedStartOfFileViaLiveJump` も対称に）を
 * 記録する。これが利用者操作と同じ式・同じ経路をたどる主判定である。
 * ラウンドトリップ走査中の `reachedEndOfFile`（どの段階でも0pxを observe
 * できたか）は診断用の参考値として残す。
 *
 * # 操作をやめた後に表示が自走していないかの確認（Issue #21 の再発検知）
 *
 * ラウンドトリップ走査と live jump は、いずれも各段階で
 * `viewport.scrollTop = <絶対値>` を設定する。この形では、段階の合間に表示位置が
 * ひとりでに動いても次の段階の代入がそれを上書きしてしまい、自走そのものを
 * 観測できない。そこで、次の2つの観測を追加する。どちらも観測の間は
 * `scrollTop` へ一切書き込まず、DOM が示す値（`scrollTop` とスペーサーの
 * 実際の高さ）だけを読む。
 *
 * 1. **末尾到達後のアイドル観測**（`stayedAtEndDuringIdle`・
 *    `scrollTopDriftPx`・`endIdleSamples`）: 末尾ジャンプで下スペーサーが
 *    0pxになった直後、先頭ジャンプを行う前に放置して観測する。末尾に
 *    到達したこと自体だけでなく、そこへ留まり続けるかを見る。
 * 2. **相対移動＋放置による観測**（`relativeScroll`）: live jump の検証が
 *    すべて終わった先頭付近（`scrollTop` はほぼ0）から、ホイール操作を模して
 *    `viewport.scrollTop += RELATIVE_SCROLL_STEP_PX` を繰り返し、その後は
 *    一切操作せずに `scrollTop` を観測する。相対移動を使うのは、絶対値の
 *    代入と違って「今の位置」を起点にするため、移動をやめた時点の位置が
 *    そのまま観測の基準値になり、以後の変位を上書きせずに読み取れるため。
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
  // 上のコメント「到達性の主判定を走査ではなく live jump に置く理由」参照。
  // 判定の主とはしない）。
  const reachedStartOfFile = samples.some((sample) => sample.topSpacerHeightPx === 0);
  const reachedEndOfFile = samples.some((sample) => sample.bottomSpacerHeightPx === 0);

  // 実際の利用者操作（Ctrl+End／Ctrl+Home）と全く同じ式で、その場で
  // 最新の scrollHeight を読み直してジャンプする（上のコメント「到達性の
  // 主判定を走査ではなく live jump に置く理由」参照）。これが末尾・先頭
  // 到達性の主判定。
  //
  // 1回設定しただけで判定せず `LIVE_JUMP_MAX_ATTEMPTS` 回まで「読み直して
  // 設定→待つ」を繰り返すのは、1回の待機（`LIVE_JUMP_WAIT_MS`）の間に
  // 取得と再描画が終わらないほど遅い実機でも、待ち時間に上限を設けたまま
  // 到達を待てるようにするため。200万行の実機計測では1回目の設定で
  // スペーサーが0pxになる（実際に要した回数は `liveJumpEndAttemptsUsed`
  // ／`liveJumpStartAttemptsUsed` に記録する）。毎回 `viewport.scrollHeight`
  // を読み直すため、再描画で scrollHeight が更新された場合は次の試行の
  // 目標値がその値へ追随する。
  const endLiveJump = await jumpAndVerifySpacerReachesZero({
    getTargetScrollTop: () => viewport.scrollHeight, // Ctrl+End と同じ式
    spacer: bottomSpacer,
  });
  const reachedEndOfFileViaLiveJump = endLiveJump.reached;
  const liveJumpEndSample = {
    scrollTop: viewport.scrollTop,
    scrollHeightAtJumpPx: viewport.scrollHeight,
    topSpacerHeightPx: spacerHeightPx(topSpacer),
    bottomSpacerHeightPx: spacerHeightPx(bottomSpacer),
  };

  // 末尾に「到達したか」だけでなく「留まり続けるか」を見る（doc コメントの
  // 「操作をやめた後に表示が自走していないか」1. 参照）。先頭ジャンプを
  // 行う前に観測しないと、末尾での挙動が失われる。
  const endIdleSamples = await observeWithoutScrolling(
    IDLE_OBSERVATION_DURATION_MS,
    END_IDLE_SAMPLE_INTERVAL_MS,
  );
  // 末尾ジャンプが0pxへ届かなかった場合も、この判定は false になる（届いて
  // いない位置に留まっていても「末尾に留まった」とは言えないため）。到達自体の
  // 成否は `reachedEndOfFileViaLiveJump` を見る。
  const stayedAtEndDuringIdle = endIdleSamples.every(
    (sample) => sample.bottomSpacerHeightPx === 0,
  );
  const scrollTopDriftPx = maxScrollTopDeviationPx(
    endIdleSamples,
    endIdleSamples.length > 0 ? endIdleSamples[0].scrollTop : 0,
  );

  const startLiveJump = await jumpAndVerifySpacerReachesZero({
    getTargetScrollTop: () => 0, // Ctrl+Home と同じ式
    spacer: topSpacer,
  });
  const reachedStartOfFileViaLiveJump = startLiveJump.reached;
  const liveJumpStartSample = {
    scrollTop: viewport.scrollTop,
    topSpacerHeightPx: spacerHeightPx(topSpacer),
    bottomSpacerHeightPx: spacerHeightPx(bottomSpacer),
  };

  // 先頭ジャンプ直後（scrollTop はほぼ0）を起点に、相対移動で動かしてから
  // 放置する（doc コメントの同 2. 参照）。
  const relativeScroll = await verifyRelativeScrollSettles();

  /**
   * `getTargetScrollTop()` が返す値（呼び出しのたびに最新の DOM から読み直す）
   * へ `viewport.scrollTop` を設定し、`spacer` の高さが0pxになるまで
   * `LIVE_JUMP_MAX_ATTEMPTS` 回まで再試行する。
   *
   * 到達可否（`reached`）だけでなく、使った試行回数（`attemptsUsed`）と
   * 各試行のサンプル（`attempts`）も返す。到達したかどうかだけでは、再試行
   * 上限が実際の必要回数に対して足りているのか過剰なのかを実測から判断できず、
   * 1回目で届いた場合と上限直前で届いた場合の区別も付かないため。
   *
   * @param {{ getTargetScrollTop: () => number, spacer: HTMLElement }} args
   * @returns {Promise<{
   *   reached: boolean,
   *   attemptsUsed: number,
   *   attempts: { attempt: number, targetScrollTop: number, scrollTopAfterWait: number, spacerHeightAfterWaitPx: number }[],
   * }>}
   */
  async function jumpAndVerifySpacerReachesZero({ getTargetScrollTop, spacer }) {
    const LIVE_JUMP_MAX_ATTEMPTS = 8;
    const LIVE_JUMP_WAIT_MS = 500;
    const attempts = [];
    for (let attempt = 1; attempt <= LIVE_JUMP_MAX_ATTEMPTS; attempt += 1) {
      const targetScrollTop = getTargetScrollTop();
      viewport.scrollTop = targetScrollTop;
      await wait(LIVE_JUMP_WAIT_MS);
      const spacerHeightAfterWaitPx = spacerHeightPx(spacer);
      // 設定した目標値と、待機後に実際に反映されていた scrollTop の両方を
      // 残す（両者のずれが、再試行が必要になる状況そのものを表すため）。
      attempts.push({
        attempt,
        targetScrollTop,
        scrollTopAfterWait: viewport.scrollTop,
        spacerHeightAfterWaitPx,
      });
      if (spacerHeightAfterWaitPx === 0) {
        return { reached: true, attemptsUsed: attempt, attempts };
      }
    }
    return { reached: false, attemptsUsed: LIVE_JUMP_MAX_ATTEMPTS, attempts };
  }

  /**
   * `scrollTop` へ一切書き込まずに、`durationMs` の間だけ `intervalMs` 間隔で
   * `scrollTop` と上下スペーサーの高さを記録する。
   *
   * 書き込みを行わないことがこの関数の要点である（書き込むと、観測したい
   * ひとりでの位置変化を上書きしてしまう）。
   *
   * @param {number} durationMs
   * @param {number} intervalMs
   * @returns {Promise<{ elapsedMs: number, scrollTop: number, topSpacerHeightPx: number, bottomSpacerHeightPx: number }[]>}
   */
  async function observeWithoutScrolling(durationMs, intervalMs) {
    const sampleCount = Math.max(1, Math.round(durationMs / intervalMs));
    const observationStartedAt = performance.now();
    const observed = [];
    for (let i = 0; i < sampleCount; i += 1) {
      await wait(intervalMs);
      observed.push({
        elapsedMs: performance.now() - observationStartedAt,
        scrollTop: viewport.scrollTop,
        topSpacerHeightPx: spacerHeightPx(topSpacer),
        bottomSpacerHeightPx: spacerHeightPx(bottomSpacer),
      });
    }
    return observed;
  }

  /**
   * 相対移動（`viewport.scrollTop += RELATIVE_SCROLL_STEP_PX`）で少しずつ
   * 動かした後、操作をやめて位置が落ち着いたままかを観測する
   * （Issue #21 の再発検知。関数の doc コメント「操作をやめた後に表示が
   * 自走していないか」2. 参照）。
   *
   * 呼び出し前提: 直前の先頭ジャンプにより `scrollTop` はほぼ0にある
   * （`RELATIVE_SCROLL_STEP_COUNT` の doc コメントが述べる移動量の設計は、
   * 先頭付近を起点にすることを前提にしている）。
   */
  async function verifyRelativeScrollSettles() {
    // 直前の先頭ジャンプで生じた取得・再描画が落ち着いてから動かし始める
    // （落ち着く前の変位を自走として数えないため）。
    await wait(SCROLL_SETTLE_WAIT_MS);

    const startScrollTop = viewport.scrollTop;
    const moves = [];
    for (let step = 1; step <= RELATIVE_SCROLL_STEP_COUNT; step += 1) {
      // 絶対値ではなく現在位置からの相対移動にすることで、移動をやめた時点の
      // 位置がそのまま観測の基準値になる。
      viewport.scrollTop += RELATIVE_SCROLL_STEP_PX;
      await wait(RELATIVE_SCROLL_STEP_INTERVAL_MS);
      moves.push({
        step,
        scrollTop: viewport.scrollTop,
        topSpacerHeightPx: spacerHeightPx(topSpacer),
        bottomSpacerHeightPx: spacerHeightPx(bottomSpacer),
      });
    }

    const scrollTopAtRest = viewport.scrollTop;
    const topSpacerHeightAtRestPx = spacerHeightPx(topSpacer);
    const idleSamples = await observeWithoutScrolling(
      IDLE_OBSERVATION_DURATION_MS,
      RELATIVE_SCROLL_IDLE_SAMPLE_INTERVAL_MS,
    );

    return {
      stepPx: RELATIVE_SCROLL_STEP_PX,
      stepCount: RELATIVE_SCROLL_STEP_COUNT,
      stepIntervalMs: RELATIVE_SCROLL_STEP_INTERVAL_MS,
      startScrollTop,
      scrollTopAtRest,
      movedPx: scrollTopAtRest - startScrollTop,
      // 上スペーサーが0pxより大きければ、`RELATIVE_SCROLL_STEP_COUNT` が
      // 意図した領域（上スペーサーが伸び始めた先）まで実際に入れている。
      // 0pxのままなら、この観測は意図した条件を満たしていない。
      topSpacerHeightAtRestPx,
      idleDurationMs: IDLE_OBSERVATION_DURATION_MS,
      idleSampleIntervalMs: RELATIVE_SCROLL_IDLE_SAMPLE_INTERVAL_MS,
      scrollTopStableAfterRelativeMoves: idleSamples.every(
        (sample) => sample.scrollTop === scrollTopAtRest,
      ),
      driftPx: maxScrollTopDeviationPx(idleSamples, scrollTopAtRest),
      moves,
      idleSamples,
    };
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
    liveJumpStartAttemptsUsed: startLiveJump.attemptsUsed,
    liveJumpEndAttemptsUsed: endLiveJump.attemptsUsed,
    liveJumpStartAttempts: startLiveJump.attempts,
    liveJumpEndAttempts: endLiveJump.attempts,
    stayedAtEndDuringIdle,
    scrollTopDriftPx,
    endIdleDurationMs: IDLE_OBSERVATION_DURATION_MS,
    endIdleSampleIntervalMs: END_IDLE_SAMPLE_INTERVAL_MS,
    endIdleSamples,
    relativeScroll,
    samples,
  };
}

/**
 * 計測が途中で失敗したことを、成功時と同じ `record_measurement_results` 経路で
 * `logs/measurements/measurement-p04-*.json` として書き残す（外部からの
 * 終了判定用。モジュール doc コメント参照）。
 *
 * 成功時の結果 JSON と区別できるよう、`failed: true` とエラーメッセージを持つ
 * 最小の記録にする。この書き残し自体に失敗した場合、フロントエンドから外部へ
 * 完了を伝える残りの手段は無い（書き込み失敗は Rust 側が診断ログへ記録する）
 * ため、コンソールへ出力して呼び出し側のタイムアウト検出に委ねる。
 *
 * @param {unknown} error
 */
async function recordFailure(error) {
  const failureRecord = {
    measuredAtIso: new Date().toISOString(),
    failed: true,
    error: error instanceof Error ? error.message : String(error),
  };
  try {
    await invoke("record_measurement_results", {
      resultsJson: JSON.stringify(failureRecord),
    });
  } catch (recordError) {
    console.error("計測失敗の記録の書き出しにも失敗しました:", recordError);
  }
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
  } catch (error) {
    console.error("計測モードの実行中にエラーが発生しました:", error);
    await recordFailure(error);
  }
}
