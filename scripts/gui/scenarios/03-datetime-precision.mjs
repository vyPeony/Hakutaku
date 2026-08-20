// シナリオ3: 日時の表示精度（`LOG-024`、`LOG-025`）。
//
// `LOG-024` は「元の精度を保持した表示文字列を、書式変換せずそのまま出す」ことを
// 求める。`LOG-025` はその具体例で、1/100 秒（小数2桁）のログを勝手にミリ秒3桁へ
// 揃えない（`.45` を `.450` にしない）ことを求める。
//
// # 検査経路（Issue #78 で変更、Issue #85 で唯一の検査点になった）
//
// かつては行一覧に解析済みの日時列があり、その DOM のテキストを表明していた。
// **日時列は Issue #78 で廃止した**（行の原文が先頭に日時を含むため、画面上で
// 同じ日時が2回並んで見えていた）。その後の提示経路だったツールバーのコピー列
// 「日時」も **Issue #85 で廃止した**（コピーは常に原文そのまま。ADR-0011）。
// 現在、解析済み日時を利用者へ提示する箇所は画面にもコピーにも無く、統合表示の
// 改修（Issue #82）で表示する予定である。
//
// つまり、精度が保たれているかを自動で確かめられる唯一の点が、フロントエンドが
// 受け取る日時の表示文字列——`fetch_log_range` の応答（`src-tauri/src/log_view.rs`
// の `LogItemDto.timestamp`）——である。ここが桁を落とす／引き伸ばすと、#82 で
// 表示を戻したときにそのまま誤った日時が出る。そこでこのシナリオは、
// フロントエンドが使うのと同じコマンド・同じ引数で IPC 境界を直接叩き、返る
// 表示文字列の精度が保たれていることを表明する。日時の解析器（Rust 側）込みで
// 検査するという価値は、DOM を見ていた頃と変わらない。
//
// 期待する書式は「サンプルの生成書式」から導く。`generate-test-log.ps1` は
// `LOG-DT-001` を `yyyy/MM/dd HH:mm:ss.fff`（小数3桁）、`LOG-DT-003` を
// `yyyy/MM/dd HH:mm:ss.ff`（小数2桁）で書き出す。表示は ISO 8601 風の
// `YYYY-MM-DDTHH:MM:SS.<元の小数桁>` になる。

import { SAMPLE_TARGETS, openTargetByName } from "../app.mjs";

export const name = "日時精度（LOG-024／LOG-025）";

/** ミリ秒3桁（`LOG-DT-001`）。末尾の `$` で「3桁ちょうど」を要求する。 */
const MILLISECOND_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}$/;

/** 1/100 秒2桁（`LOG-DT-003`）。3桁へ引き伸ばされていれば一致しない。 */
const CENTISECOND_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{2}$/;

/** IPC 境界から取り出す項目数。先頭行だけの偶然でないことを見るための件数。 */
const SAMPLED_ITEM_COUNT = 50;

/**
 * 表示中の対象の先頭 `count` 件について、`fetch_log_range` が返す日時の表示
 * 文字列を取り出す。
 *
 * 呼び出すコマンドと引数はフロントエンド（`src/log_view.js` の
 * `invokeFetchLogRange`）と同一にする。表示集合 ID と世代は `list_targets` の
 * 応答から取る（`src/targets.js` の `TargetSessionDto`）。世代を要求どおりに
 * 渡さないと `generation_mismatch` になるため、対象の現在値をそのまま使う。
 *
 * @param {import("playwright-core").Page} page
 * @param {string} displayName 開いている対象の表示名
 * @param {number} count
 * @returns {Promise<{ timestamps?: (string | null)[], error?: string }>}
 */
function readTimestampsFromIpc(page, displayName, count) {
  return page.evaluate(
    async ({ name: targetName, maxItems }) => {
      try {
        const targets = await window.__TAURI_INTERNALS__.invoke("list_targets");
        const target = targets.find(
          (entry) =>
            (entry.display_name ?? "").includes(targetName) && entry.status?.kind === "ready",
        );
        if (!target) {
          return {
            error: `読み込み済みの対象「${targetName}」が list_targets にありません（${JSON.stringify(
              targets.map((entry) => [entry.display_name, entry.status?.kind]),
            )}）`,
          };
        }
        const response = await window.__TAURI_INTERNALS__.invoke("fetch_log_range", {
          displaySetId: target.status.display_set_id,
          expectedGeneration: target.status.generation,
          start: 0,
          maxItems,
        });
        return { timestamps: response.items.map((item) => item.timestamp) };
      } catch (error) {
        return { error: String(error?.message ?? JSON.stringify(error) ?? error) };
      }
    },
    { name: displayName, maxItems: count },
  );
}

/**
 * 取り出した日時文字列が、期待する桁数の書式に一致することを表明する。
 *
 * @param {object} args
 * @param {import("../assert.mjs").Checker} args.expect
 * @param {{ timestamps?: (string | null)[], error?: string }} args.result
 * @param {RegExp} args.pattern
 * @param {string} args.requirementId
 * @param {string} args.description
 */
function expectTimestampPrecision({ expect, result, pattern, requirementId, description }) {
  expect.check(
    `${requirementId}: IPC 境界から日時の表示文字列を取得できる`,
    result.error === undefined && Array.isArray(result.timestamps),
    result.error ?? "（timestamps が配列ではありません）",
  );
  const timestamps = result.timestamps ?? [];
  expect.expectMatch(
    `${requirementId}: ${description}（先頭項目）`,
    timestamps[0] ?? "",
    pattern,
  );
  // 先頭項目だけの偶然でないことを確かめるため、取得した項目すべてを見る。
  expect.check(
    `${requirementId}: 取得した全項目が${description}`,
    timestamps.length > 1 && timestamps.every((timestamp) => pattern.test(timestamp ?? "")),
    `項目数 ${timestamps.length} / 一致しない例 ${JSON.stringify(
      timestamps.find((timestamp) => !pattern.test(timestamp ?? "")),
    )}`,
  );
}

export async function run({ page, expect }) {
  // --- LOG-024: ミリ秒3桁のログは3桁のまま ---
  const millisecondTarget = SAMPLE_TARGETS.MILLISECOND;
  await openTargetByName(page, millisecondTarget);

  expectTimestampPrecision({
    expect,
    result: await readTimestampsFromIpc(page, millisecondTarget, SAMPLED_ITEM_COUNT),
    pattern: MILLISECOND_PATTERN,
    requirementId: "LOG-024",
    description: "ミリ秒3桁のログを3桁のまま保持する",
  });

  // --- LOG-025: 1/100 秒2桁のログは2桁のまま ---
  const centisecondTarget = SAMPLE_TARGETS.CENTISECOND;
  await openTargetByName(page, centisecondTarget);

  expectTimestampPrecision({
    expect,
    result: await readTimestampsFromIpc(page, centisecondTarget, SAMPLED_ITEM_COUNT),
    pattern: CENTISECOND_PATTERN,
    requirementId: "LOG-025",
    description: "1/100秒2桁のログを2桁のまま保持し、ミリ秒3桁へ揃えない",
  });
}
