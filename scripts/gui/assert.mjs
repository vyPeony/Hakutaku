// GUI 回帰検査（Issue #57）の表明ヘルパー。
//
// 出力様式と「1件目の失敗で止めずに全件を集める」方針は
// `scripts/check-virtual-scroll.mjs` に合わせている。GUI 検査では1つの操作の
// 失敗が後続の表明を連鎖的に失敗させるため、どこまでが本当の退行でどこからが
// 巻き添えかを、1回の実行で見分けられるようにする必要がある。
//
// 純粋関数の検査（check-virtual-scroll.mjs）と違い、GUI 検査はシナリオ単位で
// 実行するため、記録先をモジュール全体で1つ持つのではなくシナリオごとに
// 作り直す（`createChecker`）。失敗したシナリオを名指しで報告するため。
//
// 性能値・所要時間は表明の対象にしない（`VER-005`）。所要時間は記録として
// 出力するだけで、合否には一切使わない。

/** 表明の詳細メッセージに値を埋め込むための整形。 */
export function formatValue(value) {
  if (typeof value === "string") {
    return JSON.stringify(value);
  }
  if (typeof value === "object" && value !== null) {
    return JSON.stringify(value);
  }
  return String(value);
}

/**
 * 1シナリオ分の表明記録を作る。
 *
 * 返り値の `problems` は失敗した表明の説明文の配列で、シナリオ実行後に
 * 呼び出し側（`scripts/check-gui.mjs`）が集計する。`count` は実行した表明の
 * 件数（成功・失敗の合計）で、「表明が1件も動いていないのに成功扱いになる」
 * 状態を検出するために使う。
 *
 * @param {string} scenarioName シナリオの表示名（失敗報告の見出しに使う）
 */
export function createChecker(scenarioName) {
  /** @type {string[]} */
  const problems = [];
  let count = 0;

  /**
   * 表明1件を記録する。`ok` が偽のときだけ `problems` へ積む。
   *
   * @param {string} name 表明の内容（何が成り立つべきか）
   * @param {boolean} ok
   * @param {string} [detail] 失敗時に添える実測値など
   */
  function check(name, ok, detail) {
    count += 1;
    if (!ok) {
      problems.push(detail ? `${name}\n      ${detail}` : name);
    }
    return ok;
  }

  return {
    scenarioName,
    problems,
    get count() {
      return count;
    },
    check,

    /** 厳密等価（`Object.is`）。 */
    expectEqual(name, actual, expected) {
      return check(
        name,
        Object.is(actual, expected),
        `期待 ${formatValue(expected)} / 実際 ${formatValue(actual)}`,
      );
    },

    /** 正規表現との一致。日時書式（`LOG-024`／`LOG-025`）の桁数判定に使う。 */
    expectMatch(name, actual, pattern) {
      const text = typeof actual === "string" ? actual : String(actual);
      return check(
        name,
        pattern.test(text),
        `期待 ${pattern} に一致 / 実際 ${formatValue(text)}`,
      );
    },

    /** 部分文字列を含むこと。表示ラベルの照合に使う。 */
    expectContains(name, actual, expected) {
      const text = typeof actual === "string" ? actual : String(actual);
      return check(
        name,
        text.includes(expected),
        `期待 ${formatValue(expected)} を含む / 実際 ${formatValue(text)}`,
      );
    },

    /** 上限以下であること（`PERF-012` の DOM 行ノード数の上限判定など）。 */
    expectAtMost(name, actual, limit) {
      return check(
        name,
        Number.isFinite(actual) && actual <= limit,
        `期待 ${limit} 以下 / 実際 ${formatValue(actual)}`,
      );
    },

    /** 下限以上であること。 */
    expectAtLeast(name, actual, limit) {
      return check(
        name,
        Number.isFinite(actual) && actual >= limit,
        `期待 ${limit} 以上 / 実際 ${formatValue(actual)}`,
      );
    },
  };
}

/** @typedef {ReturnType<typeof createChecker>} Checker */
