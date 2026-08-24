// 参照対象一覧（左ペイン）の状態モデルの回帰検査（Issue #52）。
//
// `src/targets.js` の純粋関数（DOM にも Tauri IPC にも触れない）を Node から
// 直接呼び、左ペインの行を組み立てる4種類の判断を検証する。
//
//   1. 並び順（設定由来 → アドホック）と、設定由来の名前とセッションの突き合わせ
//   2. 行の鍵（`TargetRow.key`）の安定性（Issue #48。未読み込みから読み込み済みへ
//      変わっても同じ鍵を指し続けること）
//   3. `list_targets` の状態 DTO から表示モデルへの変換（読み込み中の進捗、
//      読み込み済み、キャンセル済み・部分読み込み、エラー、未知の種別）
//   4. 防御的な既定動作（対応する名前が無い設定由来セッション、未知の種別）
//
// 左ペインは `list_targets` のポーリング（読み込み中は 500ms 間隔）で毎回
// 作り直される行の並びであり、GUI 検査（`scripts/check-gui.mjs`。手動実行）で
// しか通らない経路が多い。特に行の鍵は、ずれても画面には「一覧が出ている」
// ようにしか見えないまま、キーボードフォーカスと select の選択値だけが
// 500ms ごとに失われる（Issue #48 で実際に起きた退行）。ここでは変換の
// 決定的な入出力を Node だけで走らせ、その退行を CI で毎回捉える
// （`docs/verification/regression-checks.md`）。
//
// 期待値は「現在の実装が返した値」を写したものではなく、各関数・typedef の
// JSDoc が定める仕様から独立に導いた値を書く。導出根拠は各検査の直前の
// コメントに残す（`check-virtual-scroll.mjs`・`check-selection.mjs` と同じ方針）。
//
// 実行時間・メモリ量は一切扱わない（`VER-005`）。
//
// 使い方: node scripts/check-targets.mjs

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { buildTargetRows, findRowByTargetId } from "../src/targets.js";

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

/** 行の並びを鍵（`TargetRow.key`）の配列として突き合わせる。 */
function expectKeys(name, rows, expected) {
  const actual = rows.map((row) => row.key);
  const ok =
    actual.length === expected.length && actual.every((value, index) => value === expected[index]);
  check(name, ok, `期待 ${format(expected)} / 実際 ${format(actual)}`);
}

/**
 * 検査用の `TargetSessionDto` を作る（`src-tauri/src/targets.rs` の `TargetDto`
 * と同じ形）。`status` は呼び出し側が組み立てる。
 */
function session(targetId, overrides = {}) {
  return {
    target_id: targetId,
    display_name: `対象${targetId}.log`,
    origin: "ad_hoc",
    source_name: null,
    status: { kind: "loading" },
    ...overrides,
  };
}

/** 設定由来（`origin === "configured"`）のセッション。 */
function configuredSession(targetId, sourceName, status) {
  return session(targetId, {
    display_name: sourceName,
    origin: "configured",
    source_name: sourceName,
    status,
  });
}

// ---------------------------------------------------------------------------
// 1. 並び順と突き合わせ
// ---------------------------------------------------------------------------
//
// 仕様（`buildTargetRows` の JSDoc）:
//   並び順は「設定由来（dataSourceNames の順序）→ アドホック（登録順）」。
//   設定由来の名前に対応する開いたセッションがあればその状態を、無ければ
//   `status.kind === "not_opened"` の行を作る。
//   `origin === "configured"` でどの名前にも一致しないセッションは、防御的に
//   アドホック相当として一覧の末尾へ含める。

function checkOrdering() {
  expectKeys("並び: 設定もセッションも無ければ空", buildTargetRows([], []), []);

  // 設定由来だけ（起動直後。まだ何も開いていない）。
  const notOpened = buildTargetRows(["装置ログ", "監査ログ"], []);
  expectKeys("並び: 設定由来は dataSourceNames の順", notOpened, [
    "configured:装置ログ",
    "configured:監査ログ",
  ]);
  expectEqual("未読み込み: 対象 ID は null", notOpened[0].targetId, null);
  expectEqual("未読み込み: 表示名はデータソース名", notOpened[0].displayName, "装置ログ");
  expectEqual("未読み込み: 由来は configured", notOpened[0].origin, "configured");
  expectEqual("未読み込み: データソース名を保つ", notOpened[0].sourceName, "装置ログ");
  expectEqual("未読み込み: 状態は not_opened", notOpened[0].status.kind, "not_opened");

  // アドホックだけ（ファイル選択ダイアログから開いた対象。`LOG-020`）。
  const adHoc = buildTargetRows([], [session(7), session(8)]);
  expectKeys("並び: アドホックは登録順", adHoc, ["target:7", "target:8"]);
  expectEqual("アドホック: データソース名は null", adHoc[0].sourceName, null);
  expectEqual("アドホック: 由来は ad_hoc", adHoc[0].origin, "ad_hoc");

  // 混在。設定由来が先、アドホックが後。設定由来のうち開いていないものも
  // 一覧上の位置を保つ（開くたびに行が飛び回らない）。
  const mixed = buildTargetRows(
    ["装置ログ", "監査ログ"],
    [session(7), configuredSession(3, "監査ログ", { kind: "loading" })],
  );
  expectKeys("並び: 設定由来が先、アドホックが後", mixed, [
    "configured:装置ログ",
    "configured:監査ログ",
    "target:7",
  ]);
  expectEqual("並び: 開いた設定由来の行に対象 ID が入る", mixed[1].targetId, 3);
  expectEqual("並び: 未読み込みの設定由来は位置を保つ", mixed[0].status.kind, "not_opened");

  // 防御: 設定由来だがどの名前にも一致しないセッション（`get_config_status` と
  // `list_targets` が別の設定を見た場合。通常は起こらない）は末尾へ。
  const orphan = buildTargetRows(
    ["装置ログ"],
    [configuredSession(9, "消えたソース", { kind: "loading" })],
  );
  expectKeys("防御: 名前に一致しない設定由来セッションは末尾へ", orphan, [
    "configured:装置ログ",
    "target:9",
  ]);
  expectEqual("防御: 末尾へ回した行のデータソース名は保つ", orphan[1].sourceName, "消えたソース");

  // 防御: `origin === "configured"` だが `source_name` が無いセッションも、
  // 突き合わせようがないためアドホック相当として末尾へ。
  const noName = buildTargetRows(
    ["装置ログ"],
    [session(11, { origin: "configured", source_name: null })],
  );
  expectKeys("防御: データソース名が無い設定由来セッションは末尾へ", noName, [
    "configured:装置ログ",
    "target:11",
  ]);
}

// ---------------------------------------------------------------------------
// 2. 行の鍵の安定性（Issue #48）
// ---------------------------------------------------------------------------
//
// 仕様（`TargetRow.key` の JSDoc）:
//   設定由来の行は名前ベース（`configured:<name>`）で、未読み込みから開いた後
//   （loading／ready／…）まで**同じ鍵**を指し続ける。対象 ID ベースにすると、
//   開いた直後に鍵が切り替わって DOM が作り直され、その瞬間だけフォーカスが
//   失われる。アドホックは開いた時点から ID が変わらないため
//   `target:<targetId>` でよい。

function checkRowKeys() {
  const names = ["装置ログ"];
  const before = buildTargetRows(names, []);
  const loading = buildTargetRows(names, [configuredSession(3, "装置ログ", { kind: "loading" })]);
  const ready = buildTargetRows(names, [
    configuredSession(3, "装置ログ", {
      kind: "ready",
      display_set_id: 1,
      generation: 1,
      total_items: 100,
    }),
  ]);

  expectEqual("鍵: 未読み込みの設定由来", before[0].key, "configured:装置ログ");
  expectEqual("鍵: 読み込み中も同じ鍵", loading[0].key, "configured:装置ログ");
  expectEqual("鍵: 読み込み済みでも同じ鍵", ready[0].key, "configured:装置ログ");
  check(
    "鍵: 設定由来の鍵は対象 ID を含まない（開いた瞬間に DOM を作り直さない）",
    !ready[0].key.includes("3"),
    `鍵に対象 ID が混ざっています: ${ready[0].key}`,
  );

  // 再オープンで対象 ID が変わっても、設定由来の行の鍵は変わらない。
  const reopened = buildTargetRows(names, [
    configuredSession(42, "装置ログ", { kind: "loading" }),
  ]);
  expectEqual("鍵: 対象 ID が変わっても設定由来の鍵は同じ", reopened[0].key, "configured:装置ログ");

  // アドホックは対象 ID ベース。
  expectEqual("鍵: アドホックは対象 ID ベース", buildTargetRows([], [session(7)])[0].key, "target:7");

  // 鍵は一覧の中で一意（DOM 要素の対応表の鍵として使うため。重複すると
  // 片方の行が描画されない）。
  const many = buildTargetRows(
    ["装置ログ", "監査ログ"],
    [session(7), session(8), configuredSession(3, "監査ログ", { kind: "loading" })],
  );
  const keys = many.map((row) => row.key);
  expectEqual("鍵: 一覧の中で一意", new Set(keys).size, keys.length);
}

// ---------------------------------------------------------------------------
// 3. 状態 DTO から表示モデルへの変換
// ---------------------------------------------------------------------------
//
// 仕様（`TargetRowStatus`・`TargetStatusDto` の JSDoc）:
//   loading            → loadingProgress（進捗が無ければ null、総量不明なら
//                        totalBytes は null）
//   ready              → ready（displaySetId・generation・totalItems・
//                        fellBackToRawDisplay・updatePending）
//   cancelled_partial  → cancelledPartial（`LOG-027` の再試行対象。
//                        updatePending は持たない）
//   error              → error（`ERR-002` の5要素）と accessDenied（`PRIV-002`）
//   上記以外           → 「読み込み中」として安全側に倒す（画面を壊さない）
//
// 数値は `Number(...)`、真偽値は `Boolean(...)` を通す。Rust 側の `u64` は
// JSON で文字列として届き得る（`serde_json` の設定や桁数によって変わる）ため、
// 文字列で来ても表示モデルでは数値になっていることを確認する。

function rowStatus(status) {
  return buildTargetRows([], [session(1, { status })])[0].status;
}

function checkStatusConversion() {
  // --- 読み込み中 ---
  const withProgress = rowStatus({
    kind: "loading",
    progress: { done_bytes: 1024, total_bytes: 4096 },
  });
  expectEqual("読み込み中: 種別", withProgress.kind, "loading");
  expectEqual("読み込み中: 済みバイト数", withProgress.loadingProgress.doneBytes, 1024);
  expectEqual("読み込み中: 総バイト数", withProgress.loadingProgress.totalBytes, 4096);

  // 総量不明（`Progress::Indeterminate`）は null のまま（0 に潰さない。0 に
  // すると進捗率が 0% 固定になり、止まっているように見える）。
  const indeterminate = rowStatus({
    kind: "loading",
    progress: { done_bytes: 512, total_bytes: null },
  });
  expectEqual("読み込み中: 総量不明は null のまま", indeterminate.loadingProgress.totalBytes, null);
  expectEqual("読み込み中: 総量不明でも済みバイト数は数値", indeterminate.loadingProgress.doneBytes, 512);

  // 進捗そのものが無い（開始直後）。
  expectEqual("読み込み中: 進捗が無ければ null", rowStatus({ kind: "loading" }).loadingProgress, null);
  expectEqual(
    "読み込み中: 進捗が null でも null",
    rowStatus({ kind: "loading", progress: null }).loadingProgress,
    null,
  );

  // 文字列で届いた場合も数値へ揃える。
  const stringProgress = rowStatus({
    kind: "loading",
    progress: { done_bytes: "1048576", total_bytes: "2097152" },
  });
  expectEqual("読み込み中: 文字列の済みバイト数を数値化", stringProgress.loadingProgress.doneBytes, 1_048_576);
  expectEqual("読み込み中: 文字列の総バイト数を数値化", stringProgress.loadingProgress.totalBytes, 2_097_152);

  // --- 読み込み済み ---
  const ready = rowStatus({
    kind: "ready",
    display_set_id: 5,
    generation: 2,
    total_items: 20_000_000,
    fell_back_to_raw_display: true,
    update_pending: true,
  });
  expectEqual("読み込み済み: 種別", ready.kind, "ready");
  expectEqual("読み込み済み: 表示集合 ID", ready.ready.displaySetId, 5);
  expectEqual("読み込み済み: 世代", ready.ready.generation, 2);
  expectEqual("読み込み済み: 総行数", ready.ready.totalItems, 20_000_000);
  expectEqual("読み込み済み: 生表示への退避（LOG-022）", ready.ready.fellBackToRawDisplay, true);
  expectEqual("読み込み済み: 更新未反映（LOG-028）", ready.ready.updatePending, true);

  // 省略された真偽値は false（`undefined` を画面へ漏らさない。`undefined` は
  // 条件式では偽になるが、`Boolean` を通さないと表示側の比較で取り違えが起きる）。
  const minimalReady = rowStatus({
    kind: "ready",
    display_set_id: 1,
    generation: 1,
    total_items: "12345",
  });
  expectEqual("読み込み済み: 文字列の総行数を数値化", minimalReady.ready.totalItems, 12_345);
  expectEqual("読み込み済み: 省略された退避フラグは false", minimalReady.ready.fellBackToRawDisplay, false);
  expectEqual("読み込み済み: 省略された更新未反映は false", minimalReady.ready.updatePending, false);

  // --- キャンセル済み・部分読み込み（`LOG-027`） ---
  const cancelled = rowStatus({
    kind: "cancelled_partial",
    display_set_id: 6,
    generation: 1,
    total_items: 30,
    fell_back_to_raw_display: false,
  });
  expectEqual("部分読み込み: 種別", cancelled.kind, "cancelled_partial");
  expectEqual("部分読み込み: 表示集合 ID", cancelled.cancelledPartial.displaySetId, 6);
  expectEqual("部分読み込み: 総行数", cancelled.cancelledPartial.totalItems, 30);
  expectEqual("部分読み込み: 退避フラグ", cancelled.cancelledPartial.fellBackToRawDisplay, false);
  expectEqual("部分読み込み: 読み込み済みの枠は作らない", cancelled.ready, undefined);

  // --- エラー（`ERR-002` の5要素） ---
  const errorDto = {
    target: "C:\\Logs\\device.log",
    location: null,
    reason: "共有違反です",
    continuable: true,
    next_action: "書き込み側を停止して再試行してください",
    error_code: "HKT-LOG-0002",
  };
  const errorStatus = rowStatus({ kind: "error", error: errorDto, access_denied: true });
  expectEqual("エラー: 種別", errorStatus.kind, "error");
  expectEqual("エラー: 理由をそのまま渡す", errorStatus.error.reason, "共有違反です");
  expectEqual("エラー: 次の操作をそのまま渡す", errorStatus.error.next_action, errorDto.next_action);
  expectEqual("エラー: エラーコードをそのまま渡す", errorStatus.error.error_code, "HKT-LOG-0002");
  expectEqual("エラー: アクセス拒否（PRIV-002）", errorStatus.accessDenied, true);
  // 昇格ボタンは「アクセス拒否のときだけ」出す（`PRIV-001` の趣旨。省略時に
  // 真へ倒すと、無関係なエラーでも管理者権限を促してしまう）。
  expectEqual(
    "エラー: 省略されたアクセス拒否は false",
    rowStatus({ kind: "error", error: errorDto }).accessDenied,
    false,
  );

  // --- 未知の種別（安全側） ---
  const unknown = rowStatus({ kind: "しらない状態" });
  expectEqual("防御: 未知の種別は読み込み中として扱う", unknown.kind, "loading");
  expectEqual("防御: 未知の種別では進捗を作らない", unknown.loadingProgress, null);
}

// ---------------------------------------------------------------------------
// 4. `findRowByTargetId`
// ---------------------------------------------------------------------------
//
// 仕様（`findRowByTargetId` の JSDoc）:
//   `targetId` に一致する行を返す。無ければ null。

function checkFindRow() {
  const rows = buildTargetRows(
    ["装置ログ"],
    [session(7), configuredSession(3, "装置ログ", { kind: "loading" })],
  );

  expectEqual("行の検索: 設定由来の行を対象 ID で引ける", findRowByTargetId(rows, 3).key, "configured:装置ログ");
  expectEqual("行の検索: アドホックの行を対象 ID で引ける", findRowByTargetId(rows, 7).key, "target:7");
  expectEqual("行の検索: 一致しなければ null", findRowByTargetId(rows, 99), null);
  expectEqual("行の検索: 空の一覧でも null", findRowByTargetId([], 1), null);

  // 未読み込みの行（`targetId === null`）は、開いている対象の ID では引けない。
  const notOpenedOnly = buildTargetRows(["装置ログ"], []);
  expectEqual("行の検索: 未読み込みの行は対象 ID で引けない", findRowByTargetId(notOpenedOnly, 0), null);
}

// ---------------------------------------------------------------------------
// 5. 前提の同期（`src/shell.js` が対象一覧モデルを通していること）
// ---------------------------------------------------------------------------
//
// 上の検査は `src/targets.js` の純粋関数だけを見ており、`shell.js` がその
// 関数と鍵を実際に使っているかまでは分からない。行の組み立てや DOM の
// 対応付けが shell.js の中へ書き戻されると、この検査は通ったまま実物の
// 挙動だけが変わる（Issue #48 の退行がまさにこの形だった）。

function checkPremises() {
  const source = readFileSync(resolve(ROOT, "src", "shell.js"), "utf8");

  check(
    "前提: shell.js が左ペインの行を buildTargetRows から作る",
    source.includes("buildTargetRows(state.dataSourceNames, state.sessionTargets)"),
    "行の並びが shell.js 側で組み立てられている可能性があります",
  );
  check(
    "前提: shell.js が TargetRow.key を DOM 要素の対応表の鍵に使う（Issue #48）",
    source.includes("state.targetRowElements.get(row.key)"),
    "鍵が別の値に変わると、読み込み中のポーリングのたびにフォーカスが失われます",
  );
}

// ---------------------------------------------------------------------------
// 実行
// ---------------------------------------------------------------------------

checkOrdering();
checkRowKeys();
checkStatusConversion();
checkFindRow();
checkPremises();

if (problems.length > 0) {
  console.error(`参照対象一覧の状態モデルに問題が ${problems.length} 件あります。\n`);
  for (const problem of problems) console.error(`  ${problem}`);
  console.error(
    "\n判定方法と対象の一覧は docs/verification/regression-checks.md を参照してください。",
  );
  process.exit(1);
}

console.log(
  `参照対象一覧の状態モデル（並び順、行の鍵の安定性、状態 DTO の変換、防御的な既定動作）を ${checkCount} 項目検査しました。問題はありません。`,
);
