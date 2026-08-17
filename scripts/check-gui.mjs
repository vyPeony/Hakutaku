// GUI（画面操作）の回帰検査（Issue #57）。
//
// release ビルドの `Hakutaku.exe` を実際に起動し、WebView2 のデバッグポートへ
// Playwright（`playwright-core`）を CDP 接続して、手動確認
// （`docs/verification/manual-check.md`）のうち決定的な内部状態だけを自動で
// 表明する。実バックエンド（Tauri コマンド、Rust コア、実ファイルの解析）込みで
// フロントエンドの振る舞いを検査できる唯一の手段である。
//
// **この検査は CI に入れていない。** 理由と規約は
// `docs/verification/regression-checks.md` を正本とする（要点は
// `scripts/gui/app.mjs` のモジュールコメントにも書いてある）。
//
// # 使い方
//
//   npm run tauri -- build --no-bundle      # 先に release ビルドを作る
//   node scripts/check-gui.mjs              # 全シナリオを直列に実行する
//   node scripts/check-gui.mjs --exe <path> --port <番号>
//
// # 副作用
//
//   - OS のクリップボードを上書きする（シナリオ6。最小の200行のサンプルだけ）
//   - `%TEMP%\hakutaku-samples` へ試験データを生成する（無い場合のみ。
//     `scripts/generate-sample-logs.ps1` に委ねる）
//   - 実行ファイル直下へ `hakutaku.yaml` を配置する（`target/` 配下。Git 管理外）
//   - 失敗時のスクリーンショットを `%TEMP%\hakutaku-gui-check` へ書き出す
//
// リポジトリ内へは何も生成しない。実データ（個人情報等の機密データを含み得る
// ログ）は一切扱わず、合成データだけを使う。
//
// # 判定に使わないもの
//
// 所要時間・性能値は記録として出力するだけで、合否には使わない（`VER-005`）。

import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import process from "node:process";

import { SCREENSHOT_DIR, killRunningApp, launchApp } from "./gui/app.mjs";
import { createChecker } from "./gui/assert.mjs";
import * as startupSmoke from "./gui/scenarios/01-startup-smoke.mjs";
import * as openConfiguredSource from "./gui/scenarios/02-open-configured-source.mjs";
import * as datetimePrecision from "./gui/scenarios/03-datetime-precision.mjs";
import * as virtualScroll from "./gui/scenarios/04-virtual-scroll.mjs";
import * as jumpToLine from "./gui/scenarios/05-jump-to-line.mjs";
import * as selectAndCopy from "./gui/scenarios/06-select-and-copy.mjs";
import * as mergedView from "./gui/scenarios/07-merged-view.mjs";
import * as closeTab from "./gui/scenarios/08-close-tab.mjs";
import * as detailPanel from "./gui/scenarios/09-detail-panel.mjs";

const ROOT = resolve(import.meta.dirname, "..");
const DEFAULT_EXE_PATH = resolve(
  ROOT,
  "target",
  "x86_64-pc-windows-msvc",
  "release",
  "Hakutaku.exe",
);

// 既定のデバッグポート。`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` を設定した
// このプロセスの子だけが 127.0.0.1 で開くため、他の実行には影響しない。
const DEFAULT_PORT = 9455;

/** サンプル一式の置き場所（`scripts/generate-sample-logs.ps1` の既定と同じ）。 */
const SAMPLE_DIR = join(tmpdir(), "hakutaku-samples");

/**
 * 全シナリオを合わせた実行時間の上限。超えたら実行ファイルを強制終了して
 * 異常終了する（待機が返らないまま検査が居座り続けないため）。合否判定には
 * 使わない（`VER-005`）。
 */
const HARD_TIMEOUT_MS = 15 * 60 * 1000;

// シナリオは**この順序で直列に**実行する。並列実行は禁止（WebView2 の
// ユーザーデータフォルダが実行ファイル直下に固定されるため。`scripts/gui/app.mjs`
// の規約2）。5 は 4 が開いた10万行の表示集合をそのまま使い、8 は 7 が開いたタブを
// 閉じるため、順序に依存がある（各シナリオは前提を自分で表明してから進む）。
const SCENARIOS = [
  startupSmoke,
  openConfiguredSource,
  datetimePrecision,
  virtualScroll,
  jumpToLine,
  selectAndCopy,
  mergedView,
  closeTab,
  detailPanel,
];

// ---------------------------------------------------------------------------
// 引数
// ---------------------------------------------------------------------------

function parseArguments(argv) {
  const options = { exePath: DEFAULT_EXE_PATH, port: DEFAULT_PORT };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--help" || argument === "-h") {
      return { help: true };
    }
    if (argument === "--exe") {
      options.exePath = resolve(argv[index + 1] ?? "");
      index += 1;
      continue;
    }
    if (argument === "--port") {
      options.port = Number(argv[index + 1]);
      index += 1;
      continue;
    }
    throw new Error(`不明な引数です: ${argument}（使い方は --help）`);
  }
  if (!Number.isInteger(options.port) || options.port < 1024 || options.port > 65_535) {
    throw new Error(`--port には 1024〜65535 の整数を指定してください（指定値: ${options.port}）`);
  }
  return options;
}

function printUsage() {
  console.log(
    [
      "使い方: node scripts/check-gui.mjs [--exe <実行ファイル>] [--port <デバッグポート>]",
      "",
      `  --exe   検査する実行ファイル。既定は ${DEFAULT_EXE_PATH}`,
      `  --port  WebView2 のデバッグポート。既定は ${DEFAULT_PORT}`,
      "",
      "先に release ビルド（npm run tauri -- build --no-bundle）が必要です。",
      "実行方法と規約は docs/verification/regression-checks.md を参照してください。",
    ].join("\n"),
  );
}

// ---------------------------------------------------------------------------
// 試験データの用意
// ---------------------------------------------------------------------------

/**
 * サンプル一式が使える状態かどうかを判定する。
 *
 * 判定方法は `scripts/start-manual-check.ps1` と同じ。`hakutaku.yaml` の有無だけ
 * では、サンプルが増える前に作られた古い一式が残っている場合を検出できないため、
 * 設定が指すファイルが実際に存在するかまで確認する。
 *
 * @param {string} configPath
 * @returns {{ ok: true } | { ok: false, reason: string }}
 */
function inspectSampleSet(configPath) {
  if (!existsSync(configPath)) {
    return { ok: false, reason: `${configPath} がありません` };
  }
  let text;
  try {
    text = readFileSync(configPath, "utf8");
  } catch (error) {
    return { ok: false, reason: `${configPath} を読めません（${error?.message ?? error}）` };
  }

  // 生成される YAML は `    path: '…'` の固定形式（`log_profiles` 側は
  // `path_pattern` なので混ざらない）。単一引用符スカラーの規則どおり `''` を
  // `'` へ戻す（`generate-sample-logs.ps1` の `Format-YamlSingleQuoted`）。
  const missing = [];
  let sawPath = false;
  for (const line of text.split(/\r?\n/)) {
    const matched = /^\s*path:\s*'(.*)'\s*$/.exec(line);
    if (!matched) {
      continue;
    }
    sawPath = true;
    const samplePath = matched[1].replaceAll("''", "'");
    if (!existsSync(samplePath)) {
      missing.push(samplePath);
    }
  }
  if (!sawPath) {
    return { ok: false, reason: `${configPath} に data_sources の path: 行がありません` };
  }
  if (missing.length > 0) {
    return {
      ok: false,
      reason: `設定が指すファイルのうち ${missing.length} 件がありません（例: ${missing[0]}）`,
    };
  }
  return { ok: true };
}

/**
 * サンプル一式を用意し、正常な `hakutaku.yaml` を実行ファイル直下へ配置する。
 *
 * 設定ファイルは実行ファイルと同じフォルダの `hakutaku.yaml` 固定であり
 * （`CFG-014`）、これが無いと既定値起動（`CFG-015`）になって設定由来の
 * データソースが1件も出ない。シナリオ1が `loaded` 経路を要求するのはこのため。
 *
 * @param {string} exeDir
 */
function prepareSamples(exeDir) {
  const sampleConfigPath = join(SAMPLE_DIR, "hakutaku.yaml");
  const inspection = inspectSampleSet(sampleConfigPath);

  if (!inspection.ok) {
    console.log(`試験データを生成します（${inspection.reason}）: ${SAMPLE_DIR}`);
    const generatorPath = join(ROOT, "scripts", "generate-sample-logs.ps1");
    const generatorArguments = ["-NoProfile", "-File", generatorPath, "-OutputDir", SAMPLE_DIR];
    // 生成先に何か残っている場合、生成側は -Force 無しでは停止する（中断した
    // 一式が残っている場合もここを通る）。
    if (existsSync(SAMPLE_DIR) && readdirSync(SAMPLE_DIR).length > 0) {
      generatorArguments.push("-Force");
    }
    try {
      execFileSync("pwsh", generatorArguments, { stdio: "inherit" });
    } catch (error) {
      throw new Error(
        `試験データの生成に失敗しました（${error?.message ?? error}）。\n` +
          `  手動で実行する場合: pwsh -File scripts/generate-sample-logs.ps1 -OutputDir "${SAMPLE_DIR}"`,
      );
    }
    const reinspection = inspectSampleSet(sampleConfigPath);
    if (!reinspection.ok) {
      throw new Error(`試験データを生成しましたが使える状態になっていません: ${reinspection.reason}`);
    }
  } else {
    console.log(`試験データがあるため生成を省略します: ${SAMPLE_DIR}`);
  }

  mkdirSync(exeDir, { recursive: true });
  const exeConfigPath = join(exeDir, "hakutaku.yaml");
  copyFileSync(sampleConfigPath, exeConfigPath);
  console.log(`設定を配置しました（CFG-014）: ${exeConfigPath}`);
}

// ---------------------------------------------------------------------------
// 実行
// ---------------------------------------------------------------------------

/**
 * 1シナリオを実行し、結果を返す。
 *
 * 例外（操作の失敗、待機のタイムアウト）も、そのシナリオの表明の失敗として
 * 記録する。1件目の例外でスイート全体を止めると、後続シナリオが無関係に壊れて
 * いるのか、それとも巻き添えなのかを1回の実行で見分けられなくなるため。
 */
async function runScenario(scenario, app) {
  const expect = createChecker(scenario.name);
  const startedAt = Date.now();
  try {
    await scenario.run({ page: app.page, app, expect });
  } catch (error) {
    expect.check("シナリオが例外なく完了する", false, String(error?.stack ?? error?.message ?? error));
  }
  const elapsedMs = Date.now() - startedAt;

  // 画面は失敗したときだけ残す（成功のたびに書き出すと %TEMP% を圧迫する）。
  const screenshotPath =
    expect.problems.length > 0 ? await app.captureScreenshot(scenario.name) : null;

  return {
    name: scenario.name,
    problems: expect.problems,
    checkCount: expect.count,
    elapsedMs,
    screenshotPath,
  };
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.help) {
    printUsage();
    return 0;
  }

  if (!existsSync(options.exePath)) {
    console.error(`実行ファイルが見つかりません: ${options.exePath}`);
    console.error("  先に release ビルドを作ってください: npm run tauri -- build --no-bundle");
    return 1;
  }

  prepareSamples(dirname(options.exePath));

  console.log(`起動します: ${options.exePath}（デバッグポート ${options.port}）`);
  const app = await launchApp({
    exePath: options.exePath,
    port: options.port,
    hardTimeoutMs: HARD_TIMEOUT_MS,
  });
  console.log(`接続しました: ${app.browserVersion}`);
  if (app.inheritedMeasureFile !== undefined) {
    // 規約4。設定されたままだと計測モードが自動で走り、検査の操作と競合する。
    console.log(
      `HAKUTAKU_MEASURE_FILE が設定されていたため、子プロセスの環境から取り除きました（${app.inheritedMeasureFile}）。`,
    );
  }

  const results = [];
  try {
    for (const scenario of SCENARIOS) {
      const result = await runScenario(scenario, app);
      results.push(result);
      const mark = result.problems.length === 0 ? "OK  " : "FAIL";
      console.log(
        `[${mark}] ${result.name}（表明 ${result.checkCount} 件 / ${result.elapsedMs}ms）` +
          (result.screenshotPath ? `\n       画面: ${result.screenshotPath}` : ""),
      );
      // ページごと落ちている場合、以降のシナリオはすべてタイムアウトを待つだけに
      // なる。原因の分かる1件目の失敗を残して切り上げる。
      if (app.page.isClosed()) {
        console.error("WebView のページが閉じられたため、以降のシナリオを中止します。");
        break;
      }
    }
  } finally {
    await app.close();
  }

  // 全シナリオを通したコンソール出力の確認。シナリオ1は起動時点までを見るのに
  // 対し、こちらは操作中に出た分を拾う。
  const runtimeProblems = [];
  if (app.consoleErrors.length > 0) {
    runtimeProblems.push(
      `実行中に JS コンソールエラーが ${app.consoleErrors.length} 件出ました\n      ${JSON.stringify(
        app.consoleErrors.slice(0, 5),
      )}`,
    );
  }
  if (app.pageErrors.length > 0) {
    runtimeProblems.push(
      `実行中に未捕捉例外が ${app.pageErrors.length} 件出ました\n      ${JSON.stringify(
        app.pageErrors.slice(0, 5),
      )}`,
    );
  }

  const failed = results.filter((result) => result.problems.length > 0);
  const totalChecks = results.reduce((sum, result) => sum + result.checkCount, 0);
  const skipped = SCENARIOS.length - results.length;

  console.log("");
  if (failed.length > 0 || runtimeProblems.length > 0 || skipped > 0) {
    console.error(
      `GUI 回帰検査に問題があります（シナリオ ${failed.length}/${results.length} 件が失敗` +
        (skipped > 0 ? `、${skipped} 件が未実行` : "") +
        "）。\n",
    );
    for (const result of failed) {
      console.error(`  ${result.name}`);
      for (const problem of result.problems) {
        console.error(`    - ${problem}`);
      }
      if (result.screenshotPath) {
        console.error(`    画面: ${result.screenshotPath}`);
      }
      console.error("");
    }
    for (const problem of runtimeProblems) {
      console.error(`  - ${problem}`);
    }
    console.error(
      `\n失敗時の画面は ${SCREENSHOT_DIR} にあります。` +
        "\n判定方法と規約は docs/verification/regression-checks.md を参照してください。",
    );
    return 1;
  }

  console.log(
    `GUI 回帰検査の ${results.length} シナリオ（表明 ${totalChecks} 項目）を実行しました。問題はありません。`,
  );
  return 0;
}

// Ctrl+C で中断した場合も実行ファイルを取り残さない（残ると次回の実行が
// デバッグポートを掴めず、原因の分からない失敗になる）。
process.on("SIGINT", () => {
  console.error("\n中断されました。実行ファイルを終了します。");
  killRunningApp();
  process.exit(130);
});

main()
  .then((code) => {
    process.exit(code);
  })
  .catch((error) => {
    console.error(`GUI 回帰検査の実行自体が失敗しました: ${error?.stack ?? error?.message ?? error}`);
    process.exit(1);
  });
