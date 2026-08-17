// シナリオ3: 日時の表示精度（`LOG-024`、`LOG-025`）。
//
// `LOG-024` は「元の精度を保持した表示文字列を、書式変換せずそのまま出す」ことを
// 求める。`LOG-025` はその具体例で、1/100 秒（小数2桁）のログを勝手にミリ秒3桁へ
// 揃えない（`.45` を `.450` にしない）ことを求める。
//
// この2件は、日時の解析器（Rust 側）と表示（`src/log_view.js` の
// `formatTimestampCell`）の両方が揃って初めて成立するため、純粋関数の検査では
// 到達できない。GUI 検査で押さえる価値がもっとも高い項目のひとつ。
//
// 期待する書式は「サンプルの生成書式」から導く。`generate-test-log.ps1` は
// `LOG-DT-001` を `yyyy/MM/dd HH:mm:ss.fff`（小数3桁）、`LOG-DT-003` を
// `yyyy/MM/dd HH:mm:ss.ff`（小数2桁）で書き出す。表示は ISO 8601 風の
// `YYYY-MM-DDTHH:MM:SS.<元の小数桁>` になる。

import { SAMPLE_TARGETS, openTargetByName, readRenderedRows } from "../app.mjs";

export const name = "日時精度（LOG-024／LOG-025）";

/** ミリ秒3桁（`LOG-DT-001`）。末尾の `$` で「3桁ちょうど」を要求する。 */
const MILLISECOND_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}$/;

/** 1/100 秒2桁（`LOG-DT-003`）。3桁へ引き伸ばされていれば一致しない。 */
const CENTISECOND_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{2}$/;

export async function run({ page, expect }) {
  // --- LOG-024: ミリ秒3桁のログは3桁のまま ---
  const millisecondTarget = SAMPLE_TARGETS.MILLISECOND;
  await openTargetByName(page, millisecondTarget, { requireTimestamp: true });

  const millisecondRows = await readRenderedRows(page);
  expect.expectMatch(
    "LOG-024: ミリ秒3桁のログを3桁のまま表示する（先頭行）",
    millisecondRows[0]?.timestamp ?? "",
    MILLISECOND_PATTERN,
  );
  // 先頭行だけの偶然でないことを確かめるため、描画済みの行すべてを見る。
  expect.check(
    "LOG-024: 描画済みの全行がミリ秒3桁で表示される",
    millisecondRows.length > 1 &&
      millisecondRows.every((row) => MILLISECOND_PATTERN.test(row.timestamp)),
    `行数 ${millisecondRows.length} / 一致しない例 ${JSON.stringify(
      millisecondRows.find((row) => !MILLISECOND_PATTERN.test(row.timestamp))?.timestamp,
    )}`,
  );

  // --- LOG-025: 1/100 秒2桁のログは2桁のまま ---
  const centisecondTarget = SAMPLE_TARGETS.CENTISECOND;
  await openTargetByName(page, centisecondTarget, { requireTimestamp: true });

  const centisecondRows = await readRenderedRows(page);
  expect.expectMatch(
    "LOG-025: 1/100秒2桁のログを2桁のまま表示する（先頭行）",
    centisecondRows[0]?.timestamp ?? "",
    CENTISECOND_PATTERN,
  );
  expect.check(
    "LOG-025: 描画済みの全行が2桁のままで、ミリ秒3桁へ揃えられていない",
    centisecondRows.length > 1 &&
      centisecondRows.every((row) => CENTISECOND_PATTERN.test(row.timestamp)),
    `行数 ${centisecondRows.length} / 一致しない例 ${JSON.stringify(
      centisecondRows.find((row) => !CENTISECOND_PATTERN.test(row.timestamp))?.timestamp,
    )}`,
  );
}
