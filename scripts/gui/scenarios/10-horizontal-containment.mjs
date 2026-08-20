// シナリオ10: 横スクロールの封じ込め（Issue #78）。
//
// 行は折り返さない設計（`src/styles.css` の `.log-row` の `white-space: pre`）の
// ため、数千文字の行があれば横スクロールは必ず必要になる。問題は**どこが**
// スクロールするかである。横スクロールが起きてよいのは `#log-viewport` の内側
// だけで、ウィンドウ全体が横に流れて左ペイン（参照対象一覧）まで画面外へ動くのは
// 不具合である。
//
// 利用者報告時の原因は、`.app-main` に `min-width: 0` が無かったことだった。
// flex アイテムの自動最小サイズ（`min-width: auto`）により最長行の幅が
// `.app-body` へ伝播し、`#log-viewport` は画面幅より広いまま確定して自身の内側では
// スクロールできず、代わりにウィンドウ側へ横スクロールバーが出ていた。
//
// この崩れは「行が読める」「日時が正しい」といった内容の表明では一切検出できず、
// レイアウトの寸法を直接見るしかない。そのためこのシナリオは、内容ではなく
// スクロール量と要素の位置だけを表明する。

import { SAMPLE_TARGETS, openTargetByName } from "../app.mjs";

export const name = "横スクロールの封じ込め（Issue #78）";

/**
 * レイアウトの寸法とスクロール量を読み出す。
 *
 * @param {import("playwright-core").Page} page
 */
function readLayoutMetrics(page) {
  return page.evaluate(() => {
    const root = document.documentElement;
    const viewport = document.querySelector("#log-viewport");
    const targetPane = document.querySelector("#target-pane");
    return {
      documentScrollWidth: root.scrollWidth,
      documentClientWidth: root.clientWidth,
      documentScrollLeft: Math.round(root.scrollLeft),
      viewportScrollWidth: viewport?.scrollWidth ?? 0,
      viewportClientWidth: viewport?.clientWidth ?? 0,
      viewportScrollLeft: Math.round(viewport?.scrollLeft ?? 0),
      targetPaneX: Math.round(targetPane?.getBoundingClientRect().x ?? Number.NaN),
    };
  });
}

/**
 * `#log-viewport` を右端まで横スクロールさせる。
 *
 * ホイールやドラッグではなく `scrollLeft` の代入を使うのは、`scrollViewportToRatio`
 * （縦方向）と同じ理由である（1回あたりの移動量が環境設定に依存し、検査が
 * 非決定的になるため）。
 *
 * @param {import("playwright-core").Page} page
 */
function scrollViewportToRight(page) {
  return page.evaluate(() => {
    const viewport = document.querySelector("#log-viewport");
    viewport.scrollLeft = viewport.scrollWidth - viewport.clientWidth;
    return Math.round(viewport.scrollLeft);
  });
}

/** 後続シナリオへ横スクロールした状態を持ち越さない。 */
function resetViewportScrollLeft(page) {
  return page.evaluate(() => {
    document.querySelector("#log-viewport").scrollLeft = 0;
  });
}

export async function run({ page, expect }) {
  const target = SAMPLE_TARGETS.WIDE_LINE;
  await openTargetByName(page, target);

  try {
    const initial = await readLayoutMetrics(page);

    // (1) ウィンドウ自体が横スクロールしない。
    expect.check(
      "Issue #78: ウィンドウ全体に横スクロールが出ない",
      initial.documentScrollWidth <= initial.documentClientWidth,
      `documentElement.scrollWidth ${initial.documentScrollWidth} / clientWidth ${initial.documentClientWidth}`,
    );

    // (2) 横スクロール自体は存在する（サンプルが実際に長い行を含んでいることの
    //     確認も兼ねる。ここが偽なら (1) は「そもそも長い行が無かった」だけで
    //     成立してしまい、封じ込めを検査したことにならない）。
    expect.check(
      "横スクロールは #log-viewport の内側に存在する",
      initial.viewportScrollWidth > initial.viewportClientWidth,
      `#log-viewport scrollWidth ${initial.viewportScrollWidth} / clientWidth ${initial.viewportClientWidth}`,
    );

    // (3) 右端まで横スクロールしても、左ペインは動かずウィンドウも流れない。
    const scrolledLeft = await scrollViewportToRight(page);
    expect.check(
      "#log-viewport を右端まで横スクロールできる",
      scrolledLeft > 0,
      `scrollLeft ${scrolledLeft}`,
    );

    const scrolled = await readLayoutMetrics(page);
    expect.expectEqual(
      "横スクロールしても左ペイン（参照対象一覧）の位置が変わらない",
      scrolled.targetPaneX,
      initial.targetPaneX,
    );
    expect.expectEqual(
      "横スクロールしてもウィンドウ側は動かない（documentElement.scrollLeft が 0 のまま）",
      scrolled.documentScrollLeft,
      0,
    );
    expect.check(
      "横スクロール後もウィンドウ全体に横スクロールが出ない",
      scrolled.documentScrollWidth <= scrolled.documentClientWidth,
      `documentElement.scrollWidth ${scrolled.documentScrollWidth} / clientWidth ${scrolled.documentClientWidth}`,
    );
  } finally {
    // 表明が失敗しても横スクロールした状態を残さない（後続シナリオの
    // 行の読み取りやクリックが、想定外の位置で行われないようにする）。
    await resetViewportScrollLeft(page);
  }
}
