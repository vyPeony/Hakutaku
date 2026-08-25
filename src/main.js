// 起動時に get_config_status を呼び出し、設定読み込み結果に応じた通知を
// 表示する（CFG-015: 既定値起動の非致命的通知、CFG-016: 安全モードの警告と
// エラー一覧）。P03-2。
//
// 同じ応答から CFG-022（フロントエンドの保持上限。frontend_retention）を
// 読み取り、ログ表示ビュー（log_view.js）へ渡して初期化する（P04-2）。
// フロントエンドは保持上限をハードコードせず、常にこの応答を
// 単一の情報源として使う。
//
// ビルドツールを使わない素の ES モジュールとして書く（tasks/phase-03-configuration.md
// の制約。src/index.html は `<script type="module" src="./main.js">` で読み込む）。
//
// Tauri の IPC 呼び出しには window.__TAURI_INTERNALS__.invoke を直接使う。
// window.__TAURI__（@tauri-apps/api 相当の便利ラッパー）は Tauri.toml の
// app.withGlobalTauri を有効化しないと注入されないが、
// window.__TAURI_INTERNALS__.invoke はその設定に関わらず常に注入される
// 内部 API であり、Tauri 自身の JS API 実装（@tauri-apps/api の invoke）も
// 最終的にこれを呼び出す（根拠: tauri-2.11.5/scripts/core.js の
// window.__TAURI_INTERNALS__.invoke 定義、および src-tauri/Tauri.toml の
// [app.security] コメント）。Tauri.toml を変更せずに済む分、変更範囲が
// 最小になるためこちらを採用する。
//
// 通知バナーの表示機構（コンテナ・閉じるボタン）は src/banner.js に切り出して
// いる。log_view.js（ログ操作のエラー通知）と共有するため（P04-2）。
//
// P07-1 以降、起動直後のログ表示ビューの初期化は共通シェル
// （src/shell.js）へ委譲する。src/main.js はここまでの起動処理（設定状態の
// 取得とバナー表示、計測モードの起動判定）だけを担当する。
//
// 併せて、どこにも捕まらなかった例外・Promise の拒否の受け皿（window の
// error / unhandledrejection）をここで登録する（Issue #49。
// `reportUnexpectedError`）。フロントエンド全体で1組あれば足りるものであり、
// アプリの入口であるこのモジュールが持つのが自然なため。

import { showErrorBanner, showInfoBanner, showWarningBanner } from "./banner.js";
import { initShell } from "./shell.js";
import { runMeasurement } from "./measurement.js";
import { enableWindowPublication } from "./retention_stats.js";

/**
 * `get_config_status` コマンドを呼び出す。
 *
 * @returns {Promise<{
 *   route: "loaded" | "missing" | "invalid",
 *   errors: Array<{
 *     file_name: string,
 *     line: number | null,
 *     column: number | null,
 *     item_path: string,
 *     reason: string,
 *   }>,
 *   frontend_retention: { max_rows: number, max_bytes: number },
 *   data_source_names: string[],
 * }>}
 */
function fetchConfigStatus() {
  return window.__TAURI_INTERNALS__.invoke("get_config_status");
}

/**
 * エラー1件を「ファイル名:行:列 項目: 理由」の形式へ整形する
 * （`crates/config` の `ConfigError` の Display 表現と揃える）。
 *
 * @param {{file_name: string, line: number | null, column: number | null, item_path: string, reason: string}} error
 */
function formatConfigError(error) {
  const line = error.line ?? "?";
  const column = error.column ?? "?";
  const location = `${error.file_name}:${line}:${column}`;
  return error.item_path
    ? `${location} ${error.item_path}: ${error.reason}`
    : `${location}: ${error.reason}`;
}

/**
 * `get_config_status` の応答そのものが取得できなかった場合だけに使う、最後の
 * 砦のフォールバック値。`crates/config/src/schema.rs` の
 * `FrontendConfig::default()` と同じ値（`CFG-022` の実測前の暫定既定値）。
 * 通常経路では常に応答の `frontend_retention` を使い、この値は使われない
 * （フロントエンドは保持上限をハードコードしない、という方針の例外ではなく、
 * 応答取得自体が失敗した異常系のための最終防御）。
 */
const FALLBACK_RETENTION_LIMITS = {
  maxRows: 10_000,
  maxBytes: 64 * 1024 * 1024,
};

/**
 * `get_measurement_mode` コマンドを呼び出す（P04-3）。
 *
 * 計測モードは開発・検証専用の機能であり、`HAKUTAKU_MEASURE_FILE` 環境変数を
 * 設定して起動した場合だけ `active: true` になる。通常の利用者向け起動では
 * 常に `active: false` であり、`src/measurement.js` は一切実行されず、
 * 保持上限の内部状態観測 API（`window.__hakutakuStats`）も公開しない
 * （Issue #46）。
 *
 * @returns {Promise<{ active: boolean }>}
 */
function fetchMeasurementMode() {
  return window.__TAURI_INTERNALS__.invoke("get_measurement_mode");
}

/**
 * 設定状態に応じたバナーを表示する（CFG-015 / CFG-016）。
 *
 * @param {Awaited<ReturnType<typeof fetchConfigStatus>>} status
 */
function showBannerForConfigStatus(status) {
  switch (status.route) {
    case "loaded":
      // 正常起動。通知なし。
      break;
    case "missing":
      showInfoBanner(
        "hakutaku.yaml が見つからないため、組み込み既定値で起動しました。",
      );
      break;
    case "invalid":
      showWarningBanner(
        "hakutaku.yaml の内容が不正なため、安全モードで起動しました。組み込みの" +
          "既定値を使用しており、設定由来のデータソースとログ解析プロファイルは" +
          "無効化されています。アドホックなファイル選択は引き続き利用できます。",
        status.errors.map(formatConfigError),
      );
      break;
    default:
      console.warn("未知の起動経路です:", status.route);
  }
}

/**
 * 起動処理。設定状態を取得してバナーを表示し、そこから読み取った保持上限で
 * ログ表示ビューを初期化する。
 *
 * `get_config_status` 自体の失敗は想定外だが、フロントエンドの初期化全体を
 * 止めないよう、コンソールへ記録したうえでフォールバック値を使って続行する
 * （画面はブロックしない）。
 */
async function bootstrap() {
  let status = null;
  try {
    status = await fetchConfigStatus();
  } catch (error) {
    console.error("設定状態の取得に失敗しました:", error);
  }

  if (status) {
    showBannerForConfigStatus(status);
  }

  const retentionLimits = status
    ? {
        maxRows: status.frontend_retention.max_rows,
        maxBytes: status.frontend_retention.max_bytes,
      }
    : FALLBACK_RETENTION_LIMITS;

  // P04-3: 計測モード（開発・検証専用）かどうかを、共通シェルの初期化より前に
  // 確定させる。保持上限の内部状態観測 API（window.__hakutakuStats）は計測モード
  // のときだけ公開するため（Issue #46）、log_view.js の初期化がその公開を試みる
  // 時点（initShell → initLogView）で可否が決まっている必要がある。
  // 計測の失敗で通常の起動処理を止めないよう、ここでも例外を握りつぶして
  // コンソールへ記録するだけにする。
  let measurementMode = { active: false };
  try {
    measurementMode = await fetchMeasurementMode();
  } catch (error) {
    console.error("計測モードの確認に失敗しました:", error);
  }
  if (measurementMode.active) {
    enableWindowPublication();
  }

  // CFG-016（安全モード）では ConfigState::config が組み込み既定値になり
  // data_source_names も空になるため、事前定義パスの一覧には現れない
  // （設定由来のデータソースを無効化する要件どおり）。get_config_status 自体が
  // 取得できなかった異常系（status が null）でも、アドホックな選択は
  // 引き続き利用できるよう空配列で起動する（CFG-015／CFG-017）。
  initShell({
    retentionLimits,
    dataSourceNames: status ? status.data_source_names : [],
  });

  // 計測モードが有効な場合だけ、計測スクリプトを自動実行する。通常起動では
  // get_measurement_mode が active: false を返し、以降は何も行わない。
  if (measurementMode.active) {
    runMeasurement(retentionLimits).catch((error) => {
      console.error("計測モードの実行に失敗しました:", error);
    });
  }
}

/**
 * 未処理の例外・未処理の Promise 拒否から、利用者へ見せる理由の文を作る
 * （Issue #49）。
 *
 * 実値（パス・エラーコード）は隠さない（`ERR-002` は `DIAG-003`／`DIAG-004`
 * により実値の表示を制限しないと定めている）。理由が読み取れない値だった場合
 * でも「何か起きた」ことは伝える必要があるため、必ず何らかの文字列を返す。
 *
 * @param {unknown} value 例外・拒否理由。
 * @returns {string}
 */
function describeUnexpectedError(value) {
  if (value instanceof Error) {
    return value.message || value.name;
  }
  if (typeof value === "string") {
    return value;
  }
  if (value !== null && typeof value === "object") {
    // Tauri コマンドの失敗は `{ kind, reason }` のようなプレーンオブジェクトで
    // 届く。JSON 化できない値（循環参照など）もあり得るため、失敗したら
    // 既定の文字列化へ落とす。
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  }
  return String(value);
}

/**
 * どこにも捕まらなかったエラーを、既存のエラーバナー経路で通知する
 * （Issue #49）。
 *
 * これが無いと、フロントエンドのバグや想定外の IPC 失敗は開発者ツールの
 * コンソールにしか出ず、利用者からは「操作しても何も起きない」としか見えない。
 *
 * バナーの表示自体が失敗した場合は、コンソールへ記録するだけで再通知しない。
 * ここで例外を投げ直すと、その例外がまた `error` イベントとしてこの関数へ
 * 戻り、無限ループになるため（順序依存の防御）。
 *
 * @param {unknown} value
 */
function reportUnexpectedError(value) {
  const reason = describeUnexpectedError(value);
  console.error("未処理のエラー:", value);
  try {
    showErrorBanner(`予期しないエラーが発生しました（${reason}）。`);
  } catch (bannerError) {
    console.error("予期しないエラーの通知表示に失敗しました:", bannerError);
  }
}

// 受け皿は起動処理（bootstrap）より前に登録する（順序依存。起動処理そのものの
// 中で起きた未処理の拒否も拾えるようにするため）。
window.addEventListener("error", (event) => {
  // `error` イベントは、資産の読み込み失敗（<script>／<img> など）でも発火し、
  // その場合 `event.error` は無い。文面が「undefined」だけにならないよう、
  // メッセージ側へ落とす。
  reportUnexpectedError(event.error ?? event.message);
});
window.addEventListener("unhandledrejection", (event) => {
  reportUnexpectedError(event.reason);
});

bootstrap();
