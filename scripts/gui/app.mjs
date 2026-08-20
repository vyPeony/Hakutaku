// GUI 回帰検査（Issue #57）のハーネス。実行ファイルの起動・接続・後始末と、
// シナリオが共通で使う画面操作・読み取りをまとめる。
//
// # 方式
//
// release ビルドの `Hakutaku.exe` を、WebView2 の追加起動引数を渡す環境変数
// `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<ポート>` 付きで
// 起動し、`playwright-core` の `chromium.connectOverCDP` で接続する。この方式を
// 選んだのは、プロダクトコード（`src/`・`src-tauri/`・`crates/`）を一切変更せず、
// 実バックエンド（Tauri コマンド）込みの画面を検査できるためである。WebDriver 方式
// （tauri-driver + msedgedriver）は WebView2 Runtime との版追従保守が重く、
// 埋め込みプロバイダ方式は試験専用の依存をプロダクトへ持ち込む（Issue #57）。
//
// # ハーネスの規約
//
// 1. **`#open-file-button`（ネイティブのファイル選択ダイアログ）は絶対に押さない。**
//    ネイティブダイアログは CDP の操作対象外で、開いてしまうと以降の応答が不安定に
//    なることを実測済み。設定由来のデータソース（`CFG-003`）を左ペインから開けば
//    等価な状態に到達できるため、検査はそちらだけを使う
// 2. **並列実行しない。** WebView2 のユーザーデータフォルダは実行ファイル直下に
//    固定されるため（`SEC-009`）、同じ実行ファイルを同時に2つ動かすと状態が壊れる。
//    シナリオは1つの起動の中で直列に流す
// 3. **コピー試験は OS のクリップボードを上書きする**（`COPY-002`）。利用者が
//    コピー中の内容を壊さないよう、最小のサンプル（200行）だけを対象にする
// 4. **`HAKUTAKU_MEASURE_FILE` は設定しない。** 設定されていると計測モード
//    （`src/measurement.js`、開発・検証専用）が自動で走り、検査の操作と競合する。
//    呼び出し元の環境に残っていた場合は子プロセスの環境から取り除く
// 5. **待機は行の中身に対して書く。** 行 DOM は先に「（読み込み中…）」の
//    プレースホルダーとして描画され、中身は後から差し替わる（`src/log_view.js` の
//    `buildRowElement`）。行の存在だけを待つと空の行を掴む
// 6. **性能値・所要時間を合否に使わない**（`VER-005`）。所要時間は記録として
//    出力するだけで、表明には使わない
//
// # 失敗時の後始末
//
// 起動から後始末までのどの経路で失敗しても、`Hakutaku.exe` を子プロセスごと
// 強制終了する（`taskkill /T /F`）。取り残すと次回の実行がデバッグポートを
// 掴めず、原因の分からない失敗になるため。加えて全体のハードタイムアウトを
// 張り、待機が返らない場合も必ず終了させる。

import { execFileSync, spawn } from "node:child_process";
import { mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import process from "node:process";

import { chromium } from "playwright-core";

/** 失敗時のスクリーンショットの出力先。リポジトリ内には一切書き出さない。 */
export const SCREENSHOT_DIR = join(tmpdir(), "hakutaku-gui-check");

/** CDP エンドポイントが応答するまで待つ上限。release ビルドの初回起動を含む。 */
const CDP_READY_TIMEOUT_MS = 40_000;

/** 画面（共通シェル）の初期描画を待つ上限。 */
const SHELL_READY_TIMEOUT_MS = 30_000;

/** 終了後にデバッグポートが解放されるまで待つ上限。 */
const PORT_RELEASE_TIMEOUT_MS = 15_000;

/** 起動中の実行ファイル。ハードタイムアウトからも参照するためモジュール変数に置く。 */
let runningChild = null;

/** 起動中に張ったハードタイムアウトのタイマー。 */
let hardTimeoutTimer = null;

/**
 * 起動した実行ファイルを子プロセスごと強制終了する。
 *
 * `/T` を付けるのは、WebView2 が `msedgewebview2.exe` を別プロセスとして起動する
 * ため（親だけを終了させると WebView2 側が残り、次回の起動でユーザーデータ
 * フォルダを掴んだままになる）。既に終了している場合の失敗は無視する。
 */
export function killRunningApp() {
  if (!runningChild) {
    return;
  }
  const pid = runningChild.pid;
  runningChild = null;
  try {
    execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore" });
  } catch {
    // 既に終了していれば何もしなくてよい。
  }
}

/**
 * CDP の `/json/version` が応答するまで待つ。
 *
 * `--remote-debugging-port` は 127.0.0.1 だけで待ち受ける（実測済み）。環境変数を
 * 設定していない通常の起動ではポート自体が開かないため、この検査以外の実行に
 * 影響しない。
 *
 * @param {string} cdpUrl
 * @param {number} timeoutMs
 */
async function waitForCdpEndpoint(cdpUrl, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastError = "（応答なし）";
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${cdpUrl}/json/version`, {
        signal: AbortSignal.timeout(2_000),
      });
      if (response.ok) {
        return await response.json();
      }
      lastError = `HTTP ${response.status}`;
    } catch (error) {
      lastError = String(error?.message ?? error);
    }
    await delay(300);
  }
  throw new Error(
    `CDP エンドポイント（${cdpUrl}）が ${timeoutMs}ms 以内に応答しませんでした: ${lastError}`,
  );
}

/**
 * デバッグポートが解放される（＝ `/json/version` が応答しなくなる）まで待つ。
 *
 * 終了直後に次の起動を行うと、前のプロセスがポートを保持したままで新しい
 * WebView2 がポートを開けず、原因の分かりにくい接続失敗になる。連続実行
 * （同じポートでの再起動）を安全にするための待ちである。
 *
 * @param {string} cdpUrl
 * @param {number} timeoutMs
 */
async function waitForPortRelease(cdpUrl, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await fetch(`${cdpUrl}/json/version`, { signal: AbortSignal.timeout(1_000) });
    } catch {
      return true;
    }
    await delay(250);
  }
  return false;
}

/** 指定ミリ秒待つ。 */
export function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * CDP 接続直後にアプリの画面を担うページを取り出す。
 *
 * 接続直後は `about:blank` しか無い、あるいはページが1つも無い瞬間がある
 * （スパイクで実測）。ページの出現とアプリのオリジンへの遷移を分けて待つ。
 *
 * @param {import("playwright-core").BrowserContext} context
 * @param {number} timeoutMs
 */
async function acquireAppPage(context, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const page = context.pages()[0];
    if (page) {
      return page;
    }
    await delay(200);
  }
  throw new Error(`WebView のページが ${timeoutMs}ms 以内に現れませんでした。`);
}

/**
 * release ビルドの実行ファイルを起動し、CDP で接続した状態を返す。
 *
 * @param {object} options
 * @param {string} options.exePath 実行ファイルの絶対パス
 * @param {number} options.port デバッグポート
 * @param {number} options.hardTimeoutMs 全体のハードタイムアウト
 */
export async function launchApp({ exePath, port, hardTimeoutMs }) {
  const cdpUrl = `http://127.0.0.1:${port}`;

  // 規約4: 計測モードとの競合を避けるため、呼び出し元の環境に残っていても
  // 子プロセスへは渡さない。
  const childEnv = { ...process.env };
  const inheritedMeasureFile = childEnv.HAKUTAKU_MEASURE_FILE;
  delete childEnv.HAKUTAKU_MEASURE_FILE;
  childEnv.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = `--remote-debugging-port=${port}`;

  // 設定ファイル・logs・temp・WebView2 のユーザーデータはいずれも実行ファイル
  // 直下を基準に解決される（`CFG-014`、`SEC-009`）。呼び出し元の作業ディレクトリに
  // 左右されないよう、`scripts/start-manual-check.ps1` の起動と同じく実行ファイルの
  // フォルダで動かす。
  runningChild = spawn(exePath, [], {
    cwd: dirname(exePath),
    env: childEnv,
    stdio: "ignore",
  });

  hardTimeoutTimer = setTimeout(() => {
    console.error(
      `\n!! 全体のハードタイムアウト（${hardTimeoutMs}ms）に達しました。実行ファイルを強制終了します。`,
    );
    killRunningApp();
    process.exit(2);
  }, hardTimeoutMs);
  // タイマーだけでプロセスを生かし続けない。
  hardTimeoutTimer.unref?.();

  /** @type {import("playwright-core").Browser | null} */
  let browser = null;
  try {
    const version = await waitForCdpEndpoint(cdpUrl, CDP_READY_TIMEOUT_MS);
    browser = await chromium.connectOverCDP(cdpUrl, { timeout: 15_000 });
    const context = browser.contexts()[0];
    if (!context) {
      throw new Error("CDP 接続にブラウザコンテキストがありません。");
    }
    const page = await acquireAppPage(context, SHELL_READY_TIMEOUT_MS);

    /** @type {string[]} */
    const consoleErrors = [];
    /** @type {string[]} */
    const pageErrors = [];
    // 収集はページの取得直後に始める。CDP は起動後に接続するため、接続より前の
    // 出力は原理的に拾えない（この制約は docs/verification/regression-checks.md に
    // 記載している）。
    page.on("console", (message) => {
      if (message.type() === "error") {
        consoleErrors.push(message.text());
      }
    });
    page.on("pageerror", (error) => {
      pageErrors.push(String(error?.message ?? error));
    });

    // 2段待ち。オリジンがアプリのものになるまで待ってから、共通シェルの初期描画
    // （`#open-file-button` の可視化）を待つ。URL だけを待つと、描画前の DOM を
    // 掴んで後続の待機が空振りする。
    await page.waitForURL("http://tauri.localhost/**", { timeout: SHELL_READY_TIMEOUT_MS });
    await page.waitForSelector("#open-file-button", {
      state: "visible",
      timeout: SHELL_READY_TIMEOUT_MS,
    });

    return {
      page,
      browser,
      cdpUrl,
      browserVersion: version?.Browser ?? "（不明）",
      inheritedMeasureFile,
      consoleErrors,
      pageErrors,

      /**
       * 失敗時の画面を `%TEMP%\hakutaku-gui-check` へ保存する。
       * 保存自体の失敗で検査結果を上書きしないよう、例外は握りつぶして
       * `null` を返す。
       *
       * @param {string} label ファイル名に使う識別子
       */
      async captureScreenshot(label) {
        try {
          mkdirSync(SCREENSHOT_DIR, { recursive: true });
          const safeLabel = label.replace(/[^0-9A-Za-z_-]/g, "_").slice(0, 60);
          const path = join(
            SCREENSHOT_DIR,
            `${new Date().toISOString().replace(/[:.]/g, "-")}-${safeLabel}.png`,
          );
          await page.screenshot({ path, fullPage: false });
          return path;
        } catch {
          return null;
        }
      },

      /** 接続を切り、実行ファイルを終了し、ポートの解放まで待つ。 */
      async close() {
        if (hardTimeoutTimer) {
          clearTimeout(hardTimeoutTimer);
          hardTimeoutTimer = null;
        }
        // CDP 接続を切るだけ。WebView2 のプロセスはこれでは終わらないため、
        // 続けて強制終了する。
        await browser?.close().catch(() => {});
        killRunningApp();
        return waitForPortRelease(cdpUrl, PORT_RELEASE_TIMEOUT_MS);
      },
    };
  } catch (error) {
    // 起動途中の失敗でも実行ファイルを残さない。
    await browser?.close().catch(() => {});
    if (hardTimeoutTimer) {
      clearTimeout(hardTimeoutTimer);
      hardTimeoutTimer = null;
    }
    killRunningApp();
    await waitForPortRelease(cdpUrl, PORT_RELEASE_TIMEOUT_MS);
    throw error;
  }
}

// ---------------------------------------------------------------------------
// シナリオが共通で使う画面操作・読み取り
// ---------------------------------------------------------------------------

/** 行本文が未取得のときに表示されるプレースホルダー（`src/log_view.js`）。 */
export const ROW_PLACEHOLDER_TEXT = "（読み込み中…）";

/**
 * シナリオが開く事前定義データソースの表示名。正本は
 * `scripts/generate-sample-logs.ps1` の `$dataSourceFiles`（表示名を変えたら
 * ここも直す）。シナリオごとに文字列を散らさず1か所へ集める。
 *
 * `LARGE` に「08 大きいログ」ではなく「10 大きめのログ」を選んでいるのは、
 * `08-large.log` が `-LargeLineCount 0` で生成されたサンプル一式には存在せず、
 * その一式が既に置かれていると再生成が省略されるため（`start-manual-check.ps1`
 * の判定は「設定が指すファイルが存在するか」だけを見る）。10万行でも
 * `PERF-012`（DOM 行ノード数が増え続けない）と行番号ジャンプの丸めは検査できる。
 *
 * `MILLISECOND` は生成されるサンプルのうち最小の200行でもあるため、
 * クリップボードを上書きするコピー試験（規約3）にも同じものを使う。
 */
export const SAMPLE_TARGETS = {
  BASIC: "01 基本のログ（2,000行）",
  MILLISECOND: "02 日時書式 LOG-DT-001（ミリ秒3桁）",
  CENTISECOND: "02 日時書式 LOG-DT-003（1/100秒2桁）",
  CONTINUATION: "04 継続行（約2割が日時なし）",
  MERGE_A: "05 統合 a（同形式・+0ms）",
  MERGE_B: "05 統合 b（同形式・+7ms）",
  LARGE: "10 大きめのログ（100,000行）",
  WIDE_LINE: "11 横に長い行（数千文字）",
};

/**
 * 「1,234 行」形式の表示から行数を取り出す（`src/log_view.js` の
 * `toLocaleString("ja-JP")` による桁区切りを外す）。
 *
 * @param {string} text
 */
export function parseItemCount(text) {
  const digits = text.replace(/[^0-9]/g, "");
  return digits.length === 0 ? Number.NaN : Number(digits);
}

/**
 * 現在描画されている内容の「見分け用の指紋」を取る。
 *
 * 表示を切り替える操作（対象を開く、統合表示のトグル）では、読み込み元ラベルが
 * 先に同期的に更新され、行の描画は次のフレーム以降に進む（`src/log_view.js` の
 * `activate` → `scheduleRender`）。そのため「ラベルが期待どおりになったか」だけを
 * 待つと、**切り替え前のファイルの行がまだ残っている一瞬**を掴んでしまう
 * （実測で発生した。前のファイルの行番号・本文のまま表明されるなど、実害が出る）。
 *
 * 切り替え前の指紋と一致しなくなったことを併せて待つことで、この一瞬を除外する。
 *
 * @param {import("playwright-core").Page} page
 */
export function readRowSignature(page) {
  return page.evaluate(() => {
    const rows = Array.from(document.querySelectorAll("#log-rows .log-row"));
    const first = rows[0];
    return [
      document.querySelector("#log-source-label")?.textContent ?? "",
      document.querySelector("#log-total-items")?.textContent ?? "",
      String(rows.length),
      first?.querySelector(".log-row__lineno")?.textContent ?? "",
      (first?.querySelector(".log-row__text")?.textContent ?? "").slice(0, 80),
      rows.at(-1)?.querySelector(".log-row__lineno")?.textContent ?? "",
    ].join(" ");
  });
}

/**
 * 左ペインの参照対象を表示名で開き、読める状態になるまで待つ
 * （`CFG-003` の事前定義データソース）。
 *
 * ネイティブのファイル選択ダイアログ（`#open-file-button`）は規約1により
 * 使わない。設定由来のデータソースをクリックする経路でも、開くのは実ファイル
 * であり、Rust 側の読み込み・解析はまったく同じ経路を通る。
 *
 * 左ペインは読み込み状態の変化のたびに作り直されるため（`src/shell.js` の
 * `renderTargetList`）、クリック対象が操作の途中で差し替わることがある。
 * 一度だけ再試行して、その入れ替わりを検査の失敗として扱わない。
 *
 * @param {import("playwright-core").Page} page
 * @param {string} displayName 表示名（部分一致で探す）
 * @param {object} [options]
 * @param {number} [options.timeoutMs]
 */
export async function openTargetByName(page, displayName, options = {}) {
  const previousSignature = await readRowSignature(page);
  const alreadyActive = (
    await page.evaluate(() => document.querySelector("#log-source-label")?.textContent ?? "")
  ).includes(displayName);

  const main = page
    .locator("#target-list > li", { hasText: displayName })
    .locator(".target-row__main")
    .first();
  try {
    await main.click({ timeout: 10_000 });
  } catch (error) {
    if (!String(error?.message ?? error).includes("not attached")) {
      throw error;
    }
    await main.click({ timeout: 10_000 });
  }

  await waitForLogViewReady(page, {
    sourceLabel: displayName,
    // 既に同じ対象が表示中なら、指紋は変わらないのが正しい（変化を待つと
    // 永久に返らない）。
    previousSignature: alreadyActive ? null : previousSignature,
    ...options,
  });
}

/**
 * ログ表示ビューが「読める状態」になるまで待つ（規約5）。
 *
 * 判定は次の3つがそろうこと。
 *
 * 1. ツールバーの読み込み元ラベルが期待どおりになった
 * 2. 切り替え前の指紋（`readRowSignature`）と一致しなくなった（指定した場合）
 * 3. 描画中の行にプレースホルダー「（読み込み中…）」が1件も残っていない
 *
 * 行 DOM の存在だけを待つと、中身が空の行や切り替え前の行を掴む。
 *
 * @param {import("playwright-core").Page} page
 * @param {object} options
 * @param {string} options.sourceLabel `#log-source-label` に含まれるべき文字列
 * @param {string | null} [options.previousSignature] 切り替え前の指紋
 * @param {number} [options.timeoutMs]
 */
export async function waitForLogViewReady(
  page,
  { sourceLabel, previousSignature = null, timeoutMs = 60_000 },
) {
  await page.waitForFunction(
    ({ label, previous, placeholder }) => {
      const shown = document.querySelector("#log-source-label")?.textContent ?? "";
      if (!shown.includes(label)) {
        return false;
      }
      const rows = Array.from(document.querySelectorAll("#log-rows .log-row"));
      if (rows.length === 0) {
        return false;
      }
      if (
        rows.some((row) => (row.querySelector(".log-row__text")?.textContent ?? "") === placeholder)
      ) {
        return false;
      }
      const first = rows[0];
      const text = first.querySelector(".log-row__text")?.textContent ?? "";
      if (text.length === 0) {
        return false;
      }
      if (previous === null) {
        return true;
      }
      // 指紋の作り方は `readRowSignature` と同一でなければならない（一方だけを
      // 変えると比較が常に不一致になり、切り替えの完了を待てなくなる）。この
      // 関数はページ側で評価されるため Node 側の関数を呼べず、やむを得ず同じ
      // 組み立てを2か所に持っている。
      const signature = [
        shown,
        document.querySelector("#log-total-items")?.textContent ?? "",
        String(rows.length),
        first.querySelector(".log-row__lineno")?.textContent ?? "",
        text.slice(0, 80),
        rows.at(-1)?.querySelector(".log-row__lineno")?.textContent ?? "",
      ].join(" ");
      return signature !== previous;
    },
    {
      label: sourceLabel,
      previous: previousSignature,
      placeholder: ROW_PLACEHOLDER_TEXT,
    },
    { timeout: timeoutMs },
  );
  // 上の条件を満たした直後にもう一度だけ再描画が走ることがある（範囲取得の応答が
  // 分割して届く場合）。行の集合が落ち着くまで待ってから呼び出し側へ返す。
  await waitForRowsSettled(page);
}

/**
 * スクロール位置を「スクロール可能量に対する割合」で指定して移動し、再描画が
 * 落ち着くまで待つ。
 *
 * 実際のホイール操作ではなく `scrollTop` の代入を使うのは、ホイールの1回あたりの
 * 移動量が環境設定に依存し、末尾までの到達に必要な回数が決まらないため
 * （検査を非決定的にしないため）。スクロール自体の挙動は `scroll` イベント経由で
 * 同じ経路（`src/log_view.js` の `handleScroll`）を通る。
 *
 * @param {import("playwright-core").Page} page
 * @param {number} ratio 0（先頭）〜1（末尾）
 */
export async function scrollViewportToRatio(page, ratio) {
  await page.evaluate((value) => {
    const viewport = document.querySelector("#log-viewport");
    viewport.scrollTop = (viewport.scrollHeight - viewport.clientHeight) * value;
  }, ratio);
  await waitForRowsSettled(page);
}

/**
 * 行の再描画が落ち着く（描画された行の集合が変化しなくなる）まで待つ。
 *
 * スクロール後の描画は `requestAnimationFrame` と、範囲取得（Tauri コマンド）の
 * 応答という2段階で進む。固定時間の `waitForTimeout` だと、速い環境では無駄に
 * 待ち、遅い環境ではプレースホルダーのまま表明してしまう。ここでは「先頭行の
 * 行番号と行数が2回連続で同じ」ことを終了条件にして、環境差を吸収する。
 *
 * @param {import("playwright-core").Page} page
 * @param {number} [timeoutMs]
 */
export async function waitForRowsSettled(page, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  let previous = null;
  while (Date.now() < deadline) {
    const current = await page.evaluate((placeholder) => {
      const rows = Array.from(document.querySelectorAll("#log-rows .log-row"));
      return {
        count: rows.length,
        first: rows[0]?.querySelector(".log-row__lineno")?.textContent ?? "",
        last: rows.at(-1)?.querySelector(".log-row__lineno")?.textContent ?? "",
        pending: rows.some(
          (row) => (row.querySelector(".log-row__text")?.textContent ?? "") === placeholder,
        ),
      };
    }, ROW_PLACEHOLDER_TEXT);
    if (
      previous !== null &&
      !current.pending &&
      current.count === previous.count &&
      current.first === previous.first &&
      current.last === previous.last
    ) {
      return current;
    }
    previous = current;
    await delay(150);
  }
  throw new Error(`行の再描画が ${timeoutMs}ms 以内に落ち着きませんでした。`);
}

/**
 * 現在描画されている行を読み出す（DOM に実在する行だけ。仮想スクロールのため
 * 表示集合全体ではない）。
 *
 * @param {import("playwright-core").Page} page
 */
export function readRenderedRows(page) {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll("#log-rows .log-row")).map((row) => ({
      lineNumber: row.querySelector(".log-row__lineno")?.textContent ?? "",
      sourceLabel: row.querySelector(".log-row__source")?.textContent ?? null,
      text: row.querySelector(".log-row__text")?.textContent ?? "",
      selected: row.classList.contains("log-row--selected"),
      continuationBadge: row.querySelector(".log-row__badge--continuation")?.textContent ?? null,
    })),
  );
}

/**
 * ログ表示ビューのツールバーと状態を読み出す。
 *
 * @param {import("playwright-core").Page} page
 */
export function readViewState(page) {
  return page.evaluate(() => {
    const toggle = document.querySelector("#merged-view-toggle");
    const detailPanel = document.querySelector("#log-detail-panel");
    return {
      sourceLabel: document.querySelector("#log-source-label")?.textContent?.trim() ?? "",
      totalItemsText: document.querySelector("#log-total-items")?.textContent?.trim() ?? "",
      jumpInputValue: document.querySelector("#log-jump-input")?.value ?? "",
      mergedToggleLabel: toggle?.textContent?.trim() ?? "",
      mergedTogglePressed: toggle?.getAttribute("aria-pressed") ?? "",
      // Issue #83: 統合表示の UI 入口は #82 の改修実装まで非活性化されている。
      // シナリオ7がこの2値で非活性化そのものを確認する。
      mergedToggleDisabled: toggle instanceof HTMLButtonElement ? toggle.disabled : false,
      mergedToggleTitle: toggle?.getAttribute("title") ?? "",
      renderedRowCount: document.querySelectorAll("#log-rows .log-row").length,
      viewportClientHeight: document.querySelector("#log-viewport")?.clientHeight ?? 0,
      // Issue #78: `clientHeight` は内側の水平スクロールバーの厚みぶん実行中に
      // 変動する。変動しない外形として `offsetHeight` も返す（`.log-viewport` は
      // 境界線を持たないため、clientHeight + 水平スクロールバー厚に等しい）。
      // 使い分けの理由は scenarios/04-virtual-scroll.mjs のコメントを参照。
      viewportOffsetHeight: document.querySelector("#log-viewport")?.offsetHeight ?? 0,
      viewportScrollTop: Math.round(document.querySelector("#log-viewport")?.scrollTop ?? 0),
      detailPanelHidden: detailPanel === null ? true : detailPanel.hidden,
      detailPanelTitle:
        document.querySelector("#log-detail-panel-title")?.textContent?.trim() ?? "",
      detailPanelBody: document.querySelector("#log-detail-panel-body")?.textContent ?? "",
    };
  });
}

/**
 * 開いているビューのタブを読み出す（`#tab-bar`）。
 *
 * @param {import("playwright-core").Page} page
 */
export function readTabs(page) {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll("#tab-bar > *")).map((tab) => ({
      title: tab.querySelector(".tab__title")?.textContent?.trim() ?? "",
      selected: tab.getAttribute("aria-selected") === "true",
      merged: tab.classList.contains("tab--merged"),
      closable: tab.querySelector(".tab__close") !== null,
    })),
  );
}

/**
 * 左ペインの参照対象一覧を読み出す（`#target-list`）。
 *
 * パスは読み出さない。フロントエンドへはそもそも渡らないため（`SEC-012`）、
 * 表示名・由来・状態だけで検査する。
 *
 * @param {import("playwright-core").Page} page
 */
export function readTargetRows(page) {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll("#target-list > li")).map((row) => ({
      name: row.querySelector(".target-row__name")?.textContent?.trim() ?? "",
      origin: row.querySelector(".target-row__origin")?.textContent?.trim() ?? "",
      status: row.querySelector(".target-row__status")?.textContent?.trim() ?? "",
    })),
  );
}

/**
 * 画面上部の通知バナーを読み出す（`#config-banners`。Issue #11 の集約方式）。
 *
 * コンテナは通知が1件も出ていなければ生成されないため（`src/banner.js` の
 * `ensureBannerContainer`）、存在しない場合は空配列を返す。
 *
 * @param {import("playwright-core").Page} page
 */
export function readBanners(page) {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll("#config-banners > .config-banner")).map((banner) => ({
      kind: banner.classList.contains("config-banner--error")
        ? "error"
        : banner.classList.contains("config-banner--warning")
          ? "warning"
          : "info",
      text: banner.textContent?.replace(/×\s*$/, "").trim() ?? "",
    })),
  );
}

/**
 * 表示中の通知バナーをすべて閉じる。
 *
 * 同一内容のバナーは1枚へ集約され、閉じるまで回数が積み上がる（Issue #11）。
 * 後続シナリオが「このシナリオで出たバナー」だけを見られるよう、確認が済んだ
 * ものはその場で片付ける。
 *
 * @param {import("playwright-core").Page} page
 */
export async function dismissBanners(page) {
  await page.evaluate(() => {
    for (const button of document.querySelectorAll("#config-banners .config-banner__close")) {
      button.click();
    }
  });
}
