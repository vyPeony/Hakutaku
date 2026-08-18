// 参照対象一覧の純粋な状態モデル（P07-1／P07-2）。
//
// DOM にも IPC にも触れない。Rust 側から受け取った2種類のデータ
// （`get_config_status` の `data_source_names` と `list_targets` の一覧）を
// 突き合わせ、共通シェル（src/shell.js）の左ペインが描画する行の並びへ変換
// する純粋関数だけを持つ（AGENTS.md・tasks/phase-07-shell-ui.md の「純粋
// ロジックを関数分離し JSDoc で仕様を明記する」という要求）。
//
// ADR-0006 に従い、フレームワーク・バンドラーを使わない素の ES モジュール。
// 自動テストの仕組みがまだ無いため（ADR-0006 と同じ前提）、ここでの決定的な
// 入出力仕様が事実上の仕様書になる。`node --check` で構文だけを検証する。

/**
 * @typedef {Object} UserFacingErrorDto `src-tauri/src/targets.rs` の
 * `UserFacingErrorDto`（`ERR-002` の5要素）と同じ形。
 * @property {string} target
 * @property {string | null} location
 * @property {string} reason
 * @property {boolean} continuable
 * @property {string} next_action
 * @property {string | null} error_code
 */

/**
 * @typedef {Object} LoadProgressDto `list_targets` の `status.progress`
 * （`src-tauri/src/targets.rs` の `LoadProgressDto`。P07-2）。
 * @property {number} done_bytes
 * @property {number | null} total_bytes 総量不明（`Progress::Indeterminate`）の場合は `null`。
 */

/**
 * @typedef {Object} TargetStatusDto `list_targets` が返す1件の `status`
 * （`src-tauri/src/targets.rs` の `TargetStatusDto`。P07-2 で
 * `cancelled_partial` を追加し、`loading` に進捗、`ready`／`cancelled_partial`
 * に `fell_back_to_raw_display` を追加した）。
 * @property {"loading" | "ready" | "cancelled_partial" | "error"} kind
 * @property {LoadProgressDto | null} [progress] `kind === "loading"` のときだけ。
 * @property {number} [display_set_id] `kind === "ready" | "cancelled_partial"` のときだけ。
 * @property {number} [generation] `kind === "ready" | "cancelled_partial"` のときだけ。
 * @property {number} [total_items] `kind === "ready" | "cancelled_partial"` のときだけ。
 * @property {boolean} [fell_back_to_raw_display] `kind === "ready" | "cancelled_partial"` のときだけ（LOG-022）。
 * @property {boolean} [update_pending] `kind === "ready"` のときだけ（`LOG-028`、ADR-0007）。
 *   真なら、明示的な再読み込みが上限超過（`PERF-004`〜`006`）で拒否され、旧
 *   スナップショットの表示を維持したまま「更新未反映」になっている。
 * @property {UserFacingErrorDto} [error] `kind === "error"` のときだけ。
 * @property {boolean} [access_denied] `kind === "error"` のときだけ（PRIV-002、P11-1）。
 *   真の場合だけ「管理者として新しいウィンドウで開く」ボタンを表示する。
 */

/**
 * @typedef {Object} TargetSessionDto `list_targets` が返す一覧1件
 * （`src-tauri/src/targets.rs` の `TargetDto`）。
 * @property {number} target_id
 * @property {string} display_name
 * @property {"ad_hoc" | "configured"} origin
 * @property {string | null} source_name
 * @property {TargetStatusDto} status
 */

/**
 * @typedef {Object} TargetRowStatus 左ペイン1行の状態表示モデル。
 * `tasks/phase-07-shell-ui.md` が例示する4分類（読み込み中／読み込み済み／
 * エラー／変更済み）に、P07-2 で `cancelled_partial`
 * （キャンセル済み・部分読み込み。`LOG-027` の再試行対象）を加えた5分類。
 * "changed" はバックエンドがまだ発行しない予約状態のまま
 * （`src-tauri/src/targets.rs` の `TargetStatus` doc コメント参照。P06 の
 * 再構築通知〔`LOG-023`／`LOG-028`〕が届くようになった時点でこの値を使う）。
 * @property {"not_opened" | "loading" | "ready" | "cancelled_partial" | "error" | "changed"} kind
 * @property {{ doneBytes: number, totalBytes: number | null } | null} [loadingProgress] `kind === "loading"` のときだけ（`null` は進捗未確定）。
 * @property {{ displaySetId: number, generation: number, totalItems: number, fellBackToRawDisplay: boolean, updatePending: boolean }} [ready] `kind === "ready"` のときだけ。
 * @property {{ displaySetId: number, generation: number, totalItems: number, fellBackToRawDisplay: boolean }} [cancelledPartial] `kind === "cancelled_partial"` のときだけ。
 * @property {UserFacingErrorDto} [error]
 * @property {boolean} [accessDenied] `kind === "error"` のときだけ（PRIV-002、P11-1）。
 */

/**
 * @typedef {Object} TargetRow 左ペインが描画する1行。
 * @property {string} key 描画時の安定した識別子（DOM の再利用判定に使う。
 *   src/shell.js の renderTargetList が差分更新の対応表の鍵にする。
 *   Issue #48）。設定由来（`origin === "configured"`）の行は名前ベース
 *   （`configured:<name>`）で、未読み込み（`not_opened`）から開いた後
 *   （`loading`／`ready`／…）まで同じ一覧上の「枠」を指し続ける。target_id
 *   ベースにしなかったのは、開く操作の直後に key が切り替わって DOM が
 *   作り直され、その瞬間だけフォーカスが失われる問題を避けるため。
 *   アドホックに開いた対象（`origin === "ad_hoc"`）は開いた時点で初めて
 *   一覧に現れ、以後 ID が変わらないため対象 ID ベース（`target:<targetId>`）
 *   のままでよい。
 * @property {number | null} targetId `null` は「設定由来だがまだ開いていない」行（`status.kind === "not_opened"`）を表す。
 * @property {string} displayName
 * @property {"ad_hoc" | "configured"} origin
 * @property {string | null} sourceName `origin === "configured"` のときだけ非 null。
 * @property {TargetRowStatus} status
 */

/**
 * 設定由来のデータソース名一覧（`get_config_status` の `data_source_names`）
 * と、開いている対象一覧（`list_targets` の応答）を突き合わせ、左ペインに
 * 描画する行の並びを作る。
 *
 * 並び順は「設定由来（`dataSourceNames` の順序）→ アドホック（登録順）」。
 * 設定由来の名前に対応する開いたセッションが見つかった場合はその状態を、
 * 見つからない場合は `status.kind === "not_opened"` の行を作る。
 * `sessionTargets` のうち `origin === "configured"` で、かつどの
 * `dataSourceNames` にも一致しないものは通常発生しない（`get_config_status`
 * と `list_targets` は同じ起動時 `ConfigState` を参照するため）が、防御的に
 * アドホック相当として一覧の末尾へ含める。
 *
 * @param {string[]} dataSourceNames
 * @param {TargetSessionDto[]} sessionTargets
 * @returns {TargetRow[]}
 */
export function buildTargetRows(dataSourceNames, sessionTargets) {
  const sessionsByName = new Map();
  const remaining = [];
  for (const session of sessionTargets) {
    if (session.origin === "configured" && session.source_name != null) {
      sessionsByName.set(session.source_name, session);
    } else {
      remaining.push(session);
    }
  }

  /** @type {TargetRow[]} */
  const rows = [];

  for (const name of dataSourceNames) {
    const session = sessionsByName.get(name);
    if (session) {
      // key は名前ベースへ上書きする（`sessionToRow` は既定で target_id
      // ベースの key を作るが、設定由来の行は名前ベースで安定させる。
      // TargetRow.key の doc コメント参照。Issue #48）。
      rows.push({ ...sessionToRow(session), key: `configured:${name}` });
      sessionsByName.delete(name);
    } else {
      rows.push({
        key: `configured:${name}`,
        targetId: null,
        displayName: name,
        origin: "configured",
        sourceName: name,
        status: { kind: "not_opened" },
      });
    }
  }

  // 通常は空（防御的な扱い。関数 doc コメント参照）。
  for (const session of sessionsByName.values()) {
    remaining.push(session);
  }

  for (const session of remaining) {
    rows.push(sessionToRow(session));
  }

  return rows;
}

/**
 * @param {TargetSessionDto} session
 * @returns {TargetRow}
 */
function sessionToRow(session) {
  return {
    key: `target:${session.target_id}`,
    targetId: session.target_id,
    displayName: session.display_name,
    origin: session.origin,
    sourceName: session.source_name ?? null,
    status: toRowStatus(session.status),
  };
}

/**
 * @param {TargetStatusDto} status
 * @returns {TargetRowStatus}
 */
function toRowStatus(status) {
  switch (status.kind) {
    case "ready":
      return {
        kind: "ready",
        ready: {
          displaySetId: /** @type {number} */ (status.display_set_id),
          generation: /** @type {number} */ (status.generation),
          totalItems: Number(status.total_items),
          fellBackToRawDisplay: Boolean(status.fell_back_to_raw_display),
          updatePending: Boolean(status.update_pending),
        },
      };
    case "cancelled_partial":
      return {
        kind: "cancelled_partial",
        cancelledPartial: {
          displaySetId: /** @type {number} */ (status.display_set_id),
          generation: /** @type {number} */ (status.generation),
          totalItems: Number(status.total_items),
          fellBackToRawDisplay: Boolean(status.fell_back_to_raw_display),
        },
      };
    case "error":
      return {
        kind: "error",
        error: /** @type {UserFacingErrorDto} */ (status.error),
        accessDenied: Boolean(status.access_denied),
      };
    case "loading":
      return {
        kind: "loading",
        loadingProgress: status.progress
          ? {
              doneBytes: Number(status.progress.done_bytes),
              totalBytes:
                status.progress.total_bytes == null ? null : Number(status.progress.total_bytes),
            }
          : null,
      };
    default:
      // 未知の kind は「読み込み中」として安全側に倒す（画面を壊さない）。
      return { kind: "loading", loadingProgress: null };
  }
}

/**
 * `targetId` に一致する行を探す。
 *
 * @param {TargetRow[]} rows
 * @param {number} targetId
 * @returns {TargetRow | null}
 */
export function findRowByTargetId(rows, targetId) {
  return rows.find((row) => row.targetId === targetId) ?? null;
}
