// 仮想スクロールの純粋ロジック（P04-2／P08-2／段階0検証の再検証）。
//
// このモジュールは DOM にも IPC にも触れない。可視範囲の計算、必要チャンクの
// 選定、破棄対象チャンクの選定、バイト数計算という4つの純粋関数を核に
// （AGENTS.md の指示、および tasks/phase-04-vertical-slice.md の「純粋ロジック
// を関数として分離し…JSDoc で仕様を明記する」という要求）、P08-2 で
// ジャンプ操作（`parseJumpTargetRowIndex`・`computeScrollTopForRowIndex`）と
// 継続行の折りたたみ表示（`extractFirstLine`）の純粋ロジックを追加した。
// 自動テストの仕組みがまだ無いため、ここでの決定的な入出力仕様が事実上の
// 仕様書になる。P04-3 の計測モードが、この関数群を経由した挙動を
// 実質的に検証する。この関数群は Node.js のスモークテスト（P04-2 と同じ方式。
// 本リポジトリはテストコマンド未確定のため、検証セッションで一時スクリプトを
// 実行するだけで、テストファイル自体はリポジトリへ常設しない）でも検証する。
//
// # scrollHeight クランプへの対応（`MAX_TOTAL_HEIGHT_PX`）
//
// `docs/verification/stage0-results.md`（段階0検証、2.3節・7節の該当項）で、2000万行
// 規模において総理論高さ（`totalItems × ROW_HEIGHT_PX`）が WebView2
// （Chromium）側の要素高さ上限でクランプされ、スクロールバー操作で実際の
// ファイル末尾付近まで到達できない不具合が見つかった。対応として、
// `computeVisibleRange`／`computeSpacerHeights`／`computeScrollTopForRowIndex`
// （スクロール座標と行インデックスが1:1で対応する従来方式）はそのまま残しつつ、
// それぞれに `MAX_TOTAL_HEIGHT_PX` 超過時だけ比例写像へ切り替える別関数
// （`computeVisibleRangeForScroll`／`computeSpacerHeightsForScroll`／
// `computeScrollTopForRowIndexScaled`）を追加した。呼び出し側（`log_view.js`）は
// 常に新しい関数を呼べばよく、規模に応じた分岐は関数内部（`isHeightScalingActive`）
// に閉じている。既存関数を直接書き換えなかったのは、クランプ未満の通常規模での
// 挙動を一切変えない（回帰を避ける）ことを最優先したためである。詳細な設計判断は
// 各関数の JSDoc を参照。
//
// # P08-2: 可変行高ではなく「折りたたみ方式」を採用した理由
//
// tasks/phase-08-log-view.md 作業項目1（LOG-014）は、継続行を含む論理項目の
// 表示方式として、可変行高の仮想スクロール（前置和の推定・実測差分吸収）を
// 第一候補としつつ、「実装が過度に複雑になる場合は折りたたみ方式へ
// フォールバックしてよい」と明示的に許容している。本実装は折りたたみ方式を
// 採用した。
//
//   - 可変行高は、取得済み範囲だけ正確な高さを持ち未取得範囲は推定高さを
//     使う設計になり、取得完了のたびに前置和を再計算してスクロール位置を
//     補正する必要がある。これは本質的に「表示中の行が今どのスクロール位置に
//     対応するか」を非同期に何度も再計算する状態機械であり、誤差の蓄積・
//     スクロール位置の飛び（ジャンプ直後に取得が完了して見た目が動く）
//     といった不具合を作り込みやすい
//   - 折りたたみ方式（一律1行高 + 継続行ありの行には「+N行」バッジ、
//     クリックで下部詳細パネルへ全文を改行保持で表示）は、既存の
//     `computeVisibleRange`・`computeSpacerHeights`・`selectChunksToEvict`
//     を含む本ファイルの前置和ロジックを一切変更せずに実現できる（行高は
//     常に `log_view.js` の `ROW_HEIGHT_PX` で一定のまま）。P04 で
//     決定的に検証済みの仮想スクロール基盤をそのまま維持でき、回帰リスクが
//     小さい
//   - `raw_text`（`hakutaku_core::ItemDto`）は継続行結合済みの本文を改行
//     （`\n`）付きでそのまま保持している（`crates/core-services/src/item.rs`・
//     `display_set.rs` の該当テスト参照）。行一覧では1行目だけ
//     （`extractFirstLine`）を表示し、詳細パネルでは `raw_text` 全体を
//     そのまま表示するため、原文（LOG-024 の「原文を失わない」）は常に
//     完全な形で確認できる
//
// 採用方式の是非は、未確定行の見せ方と同じく人間判断待ちの項目
// （tasks/phase-08-log-view.md「人間判断待ち」）に準ずる設計判断であり、
// 利用者確認の対象として報告する。
//
// ADR-0006 に従い、フレームワーク・バンドラーを使わない素の ES モジュール。

/**
 * @typedef {Object} VisibleRange
 * @property {number} startIndex 可視範囲＋前後バッファの先頭行インデックス（0起点、含む）。
 * @property {number} endIndex 可視範囲＋前後バッファの終端行インデックス（0起点、含まない）。
 */

/**
 * スクロール位置から、DOM に描画すべき行範囲（可視範囲＋前後バッファ）を計算する。
 *
 * 可視範囲だけでなく前後バッファ分も含めることで、わずかなスクロールのたびに
 * チャンク取得が発生することを防ぐ。`totalItems` や `rowHeightPx` が 0 以下の
 * 場合は空範囲（`{0, 0}`）を返す。
 *
 * @param {Object} params
 * @param {number} params.scrollTop 現在のスクロール位置（px、0以上）。
 * @param {number} params.viewportHeightPx ビューポートの表示高さ（px）。
 * @param {number} params.rowHeightPx 固定行高（px）。
 * @param {number} params.totalItems 表示集合の総項目数。
 * @param {number} params.bufferRows 可視範囲の前後に確保するバッファ行数。
 * @returns {VisibleRange}
 */
export function computeVisibleRange({
  scrollTop,
  viewportHeightPx,
  rowHeightPx,
  totalItems,
  bufferRows,
}) {
  if (totalItems <= 0 || rowHeightPx <= 0) {
    return { startIndex: 0, endIndex: 0 };
  }

  const firstVisibleRow = Math.floor(Math.max(0, scrollTop) / rowHeightPx);
  // ビューポートに収まる行数（下端に見切れる半端な1行分も含めるため +1）。
  const visibleRowCount = Math.ceil(Math.max(0, viewportHeightPx) / rowHeightPx) + 1;

  const startIndex = Math.max(0, firstVisibleRow - bufferRows);
  const endIndex = Math.min(
    totalItems,
    firstVisibleRow + visibleRowCount + bufferRows,
  );

  return { startIndex, endIndex: Math.max(startIndex, endIndex) };
}

/**
 * スペーサ合計高さ（`totalItems × rowHeightPx`）の上限（px）。超えた場合は
 * `computeVisibleRangeForScroll`／`computeSpacerHeightsForScroll` が
 * スクロール座標↔行インデックスの比例写像へ切り替える。
 *
 * 根拠: `docs/verification/stage0-results.md` の2000万行検証で、理論値
 * （20,000,000行 × `ROW_HEIGHT_PX`(22px) = 440,000,000px）を要求したところ、
 * WebView2（Chromium）が実際にレイアウトへ反映した `scrollHeight` は
 * 26,843,546px にクランプされていた（同文書 2.3節・7節の該当項）。この実測クランプ
 * 値より安全側（十分小さい側）に丸めた 24,000,000px を採用する。実測値に
 * ちょうど合わせず余裕を持たせるのは、WebView2 のバージョンや実行環境の違いで
 * 実際のクランプ位置が変動し得るため、ここで指定した高さそのものがブラウザに
 * 再度クランプされる事態を避けるためである。
 */
export const MAX_TOTAL_HEIGHT_PX = 24_000_000;

/**
 * 総理論高さ（`totalItems × rowHeightPx`）が `MAX_TOTAL_HEIGHT_PX` を超えて
 * いるかどうか。
 *
 * @param {number} totalItems
 * @param {number} rowHeightPx
 * @returns {boolean}
 */
export function isHeightScalingActive(totalItems, rowHeightPx) {
  return Math.max(0, totalItems) * rowHeightPx > MAX_TOTAL_HEIGHT_PX;
}

/**
 * スペーサ合計・スクロール座標の計算に使う「実効総高さ」（px）。
 *
 * `isHeightScalingActive` が false（クランプ未満の通常規模）なら理論値
 * （`totalItems × rowHeightPx`）をそのまま返す。この場合、
 * `computeVisibleRangeForScroll`／`computeSpacerHeightsForScroll` は内部で
 * 既存の `computeVisibleRange`／`computeSpacerHeights`（スクロール座標と行
 * インデックスが1:1で対応する従来方式）へそのまま委譲するため、クランプ未満の
 * 規模では挙動が一切変わらない（回帰を避ける設計判断）。`isHeightScalingActive`
 * が true の場合は `MAX_TOTAL_HEIGHT_PX` を返す。
 *
 * @param {number} totalItems
 * @param {number} rowHeightPx
 * @returns {number}
 */
export function computeEffectiveTotalHeightPx(totalItems, rowHeightPx) {
  const theoreticalHeightPx = Math.max(0, totalItems) * rowHeightPx;
  return isHeightScalingActive(totalItems, rowHeightPx)
    ? MAX_TOTAL_HEIGHT_PX
    : theoreticalHeightPx;
}

/**
 * スクロール位置から、DOM に描画すべき行範囲（可視範囲＋前後バッファ）を計算
 * する（スクロール高クランプ対応版）。
 *
 * 総理論高さ（`totalItems × rowHeightPx`）が `MAX_TOTAL_HEIGHT_PX` 以下の
 * 通常規模では、`computeVisibleRange`（スクロール座標と行インデックスが1:1で
 * 対応する従来方式）へそのまま委譲する（「クランプ未満の通常規模では従来の
 * 1:1方式を維持する」という設計判断。回帰を避ける）。
 *
 * 上限を超える場合は、スクロール座標をブラウザが実際に許容する範囲
 * （0 〜 `maxScrollTopPx`。呼び出し側が `viewport.scrollHeight -
 * viewport.clientHeight` から渡す）に対する比例（0〜1）とみなし、その比例を
 * 総行数に掛けて先頭行インデックスを決める比例写像を使う
 * （`selectChunksToEvict` 等と同じ「純粋関数として仕様を明記する」方針）。
 *
 * 比例写像は `scrollTop === maxScrollTopPx`（スクロールバーが末尾に到達した
 * 状態）で必ず表示集合の末尾（`endIndex === totalItems`）に到達することを
 * 保証する。クランプによる問題（2000万行規模でスクロールバー操作により実際に
 * ファイル末尾付近まで到達できない）の解消そのものが目的のため、末尾到達性を
 * 最優先する設計判断とした。
 *
 * 一方、比例写像は `totalItems` が巨大な場合に浮動小数点演算による行
 * インデックスの微小な離散化誤差を伴う（同じ `scrollTop` でも、ブラウザ側の
 * scrollTop 丸めや totalItems の大きさにより、隣接ピクセルで指す行が1行程度
 * 前後することがある）。可変行高（前置和の再計算が必要）を避けた
 * `virtual_scroll.js` 冒頭のコメントの設計判断と同じ考え方で、可視範囲の
 * 連続性（スクロールで表示行が飛ばない・逆行しない）と末尾到達性を優先し、
 * 行単位の厳密なピクセル対応は求めない設計判断とした。
 *
 * @param {Object} params
 * @param {number} params.scrollTop 現在のスクロール位置（px、0以上）。
 * @param {number} params.maxScrollTopPx ブラウザの実際の最大スクロール位置
 *   （`viewport.scrollHeight - viewport.clientHeight`）。0以下なら比例写像は
 *   使わず常に先頭（`firstVisibleRow = 0`）とみなす。
 * @param {number} params.viewportHeightPx ビューポートの表示高さ（px）。
 * @param {number} params.rowHeightPx 固定行高（px）。
 * @param {number} params.totalItems 表示集合の総項目数。
 * @param {number} params.bufferRows 可視範囲の前後に確保するバッファ行数。
 * @returns {VisibleRange}
 */
export function computeVisibleRangeForScroll({
  scrollTop,
  maxScrollTopPx,
  viewportHeightPx,
  rowHeightPx,
  totalItems,
  bufferRows,
}) {
  if (totalItems <= 0 || rowHeightPx <= 0) {
    return { startIndex: 0, endIndex: 0 };
  }

  if (!isHeightScalingActive(totalItems, rowHeightPx)) {
    return computeVisibleRange({
      scrollTop,
      viewportHeightPx,
      rowHeightPx,
      totalItems,
      bufferRows,
    });
  }

  const visibleRowCount = Math.ceil(Math.max(0, viewportHeightPx) / rowHeightPx) + 1;
  const maxFirstVisibleRow = Math.max(0, totalItems - visibleRowCount);

  let firstVisibleRow;
  if (maxScrollTopPx <= 0) {
    firstVisibleRow = 0;
  } else {
    const proportion = Math.min(1, Math.max(0, scrollTop / maxScrollTopPx));
    firstVisibleRow = Math.min(maxFirstVisibleRow, Math.floor(proportion * totalItems));
  }

  const startIndex = Math.max(0, firstVisibleRow - bufferRows);
  const endIndex = Math.min(totalItems, firstVisibleRow + visibleRowCount + bufferRows);

  return { startIndex, endIndex: Math.max(startIndex, endIndex) };
}

/**
 * 行インデックスが属するチャンク番号を返す（0起点）。
 *
 * @param {number} rowIndex
 * @param {number} chunkSize
 * @returns {number}
 */
export function chunkIndexForRow(rowIndex, chunkSize) {
  return Math.floor(rowIndex / chunkSize);
}

/**
 * チャンク番号から、そのチャンクが表示集合内でカバーする範囲を計算する。
 *
 * 総項目数でクリップするため、末尾のチャンクは `chunkSize` より小さくなり得る。
 * `chunkIndex` の開始位置が総項目数以上の場合は `count: 0` を返す（呼び出し側は
 * 取得を行わないでよい）。
 *
 * @param {number} chunkIndex
 * @param {number} chunkSize
 * @param {number} totalItems
 * @returns {{ start: number, count: number }}
 */
export function computeChunkRange(chunkIndex, chunkSize, totalItems) {
  const start = chunkIndex * chunkSize;
  if (start >= totalItems) {
    return { start, count: 0 };
  }
  const end = Math.min(start + chunkSize, totalItems);
  return { start, count: end - start };
}

/**
 * 可視範囲（＋バッファ）をカバーするために必要なチャンク番号の一覧を返す。
 *
 * 戻り値は昇順で重複を含まない。範囲が空、または総項目数が0の場合は空配列。
 *
 * @param {number} startIndex
 * @param {number} endIndex 終端（含まない）。
 * @param {number} chunkSize
 * @param {number} totalItems
 * @returns {number[]}
 */
export function computeRequiredChunkIndices(
  startIndex,
  endIndex,
  chunkSize,
  totalItems,
) {
  const clampedEnd = Math.min(endIndex, totalItems);
  if (totalItems <= 0 || clampedEnd <= startIndex) {
    return [];
  }

  const firstChunk = chunkIndexForRow(startIndex, chunkSize);
  const lastChunk = chunkIndexForRow(clampedEnd - 1, chunkSize);

  const indices = [];
  for (let index = firstChunk; index <= lastChunk; index += 1) {
    indices.push(index);
  }
  return indices;
}

/**
 * UTF-8 バイト数の計算に使い回す `TextEncoder`。
 *
 * `TextEncoder` はエンコード結果を戻り値として返すだけで内部に状態を持たない
 * ため、複数回・複数箇所から呼んでも互いに影響しない（同じ入力に対して常に
 * 同じ結果になる）。`utf8ByteLength` は保持バイト数の判定で項目ごとに呼ばれる
 * ため、呼び出しのたびに新しいインスタンスを作らず、モジュール読み込み時の
 * 1個を共有する。
 */
const UTF8_ENCODER = new TextEncoder();

/**
 * 文字列の UTF-8 バイト数を計算する。
 *
 * 保持バイト数の上限判定（`raw_text` の UTF-8 バイト数合計 ≤ max_bytes）の
 * 基礎となる計算。`TextEncoder` は WebView2（Chromium 系）で利用できる標準 API。
 *
 * @param {string} text
 * @returns {number}
 */
export function utf8ByteLength(text) {
  return UTF8_ENCODER.encode(text).length;
}

/**
 * 項目配列（`raw_text` を持つオブジェクトの配列）の UTF-8 バイト数合計を計算する。
 *
 * @param {Array<{ raw_text: string }>} items
 * @returns {number}
 */
export function sumRawTextBytes(items) {
  let total = 0;
  for (const item of items) {
    total += utf8ByteLength(item.raw_text);
  }
  return total;
}

/**
 * @typedef {Object} ChunkDescriptor
 * @property {number} chunkIndex
 * @property {number} rowCount このチャンクが保持している行数。
 * @property {number} byteCount このチャンクが保持している raw_text の UTF-8 バイト数合計。
 */

/**
 * @typedef {Object} RetentionLimits
 * @property {number} maxRows 保持行数の上限（`CFG-022`）。
 * @property {number} maxBytes 保持バイト数の上限（`CFG-022`）。
 */

/**
 * 保持上限（行数・バイト数）を超えている場合に、破棄すべきチャンク番号を選ぶ。
 *
 * 「表示範囲から遠いチャンクから破棄する」（`tasks/phase-04-vertical-slice.md`
 * 作業項目2・`PERF-012`）を、各チャンクの中心行インデックスと基準行インデックス
 * （通常は現在の可視範囲の中心）との距離が大きい順に破棄することで実現する。
 * `protectedChunkIndices` に含まれるチャンク（現在の可視範囲＋バッファが
 * カバーするチャンク）は破棄の対象から除外する（破棄しても即座に再取得が
 * 必要になるだけで無意味なため）。
 *
 * 保護チャンクだけで上限を超えている場合、それ以上は破棄できないため、上限を
 * 超えたままの結果を返すことがある（呼び出し側の設定値がバッファ必要量に対して
 * 小さすぎる場合の縮退動作。既定値では発生しない想定）。
 *
 * 上限をどちらも満たしている場合は空配列を返す（何も破棄しない）。
 *
 * @param {ChunkDescriptor[]} chunks 現在キャッシュしている全チャンクの記述子。
 * @param {Set<number>} protectedChunkIndices 破棄してはいけないチャンク番号。
 * @param {number} referenceRowIndex 距離計算の基準となる行インデックス。
 * @param {number} chunkSize
 * @param {RetentionLimits} limits
 * @returns {number[]} 破棄すべきチャンク番号（破棄すべき順）。
 */
export function selectChunksToEvict(
  chunks,
  protectedChunkIndices,
  referenceRowIndex,
  chunkSize,
  limits,
) {
  let totalRows = 0;
  let totalBytes = 0;
  for (const chunk of chunks) {
    totalRows += chunk.rowCount;
    totalBytes += chunk.byteCount;
  }

  if (totalRows <= limits.maxRows && totalBytes <= limits.maxBytes) {
    return [];
  }

  const evictable = chunks
    .filter((chunk) => !protectedChunkIndices.has(chunk.chunkIndex))
    .map((chunk) => {
      const chunkCenter = chunk.chunkIndex * chunkSize + chunkSize / 2;
      return {
        chunkIndex: chunk.chunkIndex,
        rowCount: chunk.rowCount,
        byteCount: chunk.byteCount,
        distance: Math.abs(chunkCenter - referenceRowIndex),
      };
    })
    // 距離が大きい（表示範囲から遠い）順。同距離は chunkIndex 昇順にして
    // 呼び出しのたびに結果が決定的になるようにする。
    .sort((a, b) => b.distance - a.distance || a.chunkIndex - b.chunkIndex);

  const toEvict = [];
  for (const chunk of evictable) {
    if (totalRows <= limits.maxRows && totalBytes <= limits.maxBytes) {
      break;
    }
    toEvict.push(chunk.chunkIndex);
    totalRows -= chunk.rowCount;
    totalBytes -= chunk.byteCount;
  }
  return toEvict;
}

/**
 * 上下スペーサーの高さ（px）を計算する。
 *
 * 全体の高さを `total_items × 行高` のスペーサで表現するための計算（実際に
 * DOM へ描画するのは可視範囲＋バッファの行だけであり、スペーサが残りの高さを
 * 埋めることでスクロールバーの位置・大きさが総行数に対して正しくなる）。
 *
 * @param {Object} params
 * @param {number} params.startIndex 描画している先頭行インデックス。
 * @param {number} params.endIndex 描画している終端行インデックス（含まない）。
 * @param {number} params.totalItems
 * @param {number} params.rowHeightPx
 * @returns {{ topHeightPx: number, bottomHeightPx: number }}
 */
export function computeSpacerHeights({
  startIndex,
  endIndex,
  totalItems,
  rowHeightPx,
}) {
  const topHeightPx = Math.max(0, startIndex) * rowHeightPx;
  const bottomHeightPx = Math.max(0, totalItems - endIndex) * rowHeightPx;
  return { topHeightPx, bottomHeightPx };
}

/**
 * 上下スペーサーの高さ（px）を計算する（スクロール高クランプ対応版）。
 *
 * 総理論高さ（`totalItems × rowHeightPx`）が `MAX_TOTAL_HEIGHT_PX` 以下の
 * 通常規模では `computeSpacerHeights`（従来どおりの1:1計算）へそのまま委譲
 * する（回帰を避ける設計判断）。
 *
 * 上限を超える場合、スペーサ2つの高さの**合計**が常に
 * `effectiveHeightPx − 実際に描画する行の高さ`（`computeEffectiveTotalHeightPx`
 * が返す実効総高さから、可視範囲＋バッファの実描画行の高さ分を差し引いた値）
 * と厳密に一致するように、その残り高さを `startIndex`（未描画の先頭側行数）と
 * `totalItems - endIndex`（未描画の末尾側行数）の比で配分する。これにより
 * `topHeightPx + bottomHeightPx + 実描画行の高さ` は可視範囲の位置・大きさ
 * （`startIndex`／`endIndex`）によらず常に厳密に `effectiveHeightPx` と一致
 * し、`viewport.scrollHeight` が可視範囲の移動に応じて微小に揺れ動く事態を
 * 避ける。
 *
 * この「常に厳密に一致させる」設計は、先頭・行位置ごとに単純な比例
 * （`startIndex × rowHeightPx × scale` 等）で個別に縮小する素朴な実装からの
 * 修正である。素朴な実装では、可視範囲＋バッファの実描画行数（ウィンドウ幅。
 * 先頭・末尾付近ではバッファの片側がクリップされて狭くなり、中間では両側とも
 * クリップされず広くなる）によって `topHeightPx + bottomHeightPx +
 * 実描画行の高さ` の合計がわずかに変動してしまい、`viewport.scrollHeight`
 * （＝ブラウザが実際に見るスクロール可能な総高さ）がフレームごとに数百〜
 * 数千px程度ふらつく。ジャンプ操作・末尾ジャンプ（Ctrl+End）を含む一連の
 * 検証で、ある瞬間に読んだ `scrollHeight` を基準に計算した目標スクロール
 * 位置（`computeScrollTopForRowIndexScaled` 等）を、実際に描画が反映される
 * 別の瞬間の `scrollHeight`（レンダー中で `computeCurrentRenderTargets` が
 * 都度読み直す、`log_view.js` 参照）に照らすと、比例（0〜1）がわずかに1未満
 * になり得て、`computeVisibleRangeForScroll` の末尾到達判定
 * （`firstVisibleRow` が `maxFirstVisibleRow` まで届くかどうか）を僅かに
 * 外すことがある。2000万行規模の実機検証（`measurement.js` の
 * `reachedEndOfFile`）で、2000行規模では到達できても2000万行規模では
 * 到達できない非決定的な失敗として実際に観測された（総高さの揺れ幅が絶対値
 * では小さくても、比例写像に換算すると `firstVisibleRow` の計算結果を1行分
 * 動かすには十分だったため）。総高さを厳密に一定値へ固定する本方式は、この
 * ふらつきの発生源そのものを消すため、可視範囲の位置によらず決定的に末尾へ
 * 到達できる。
 *
 * @param {Object} params
 * @param {number} params.startIndex 描画している先頭行インデックス。
 * @param {number} params.endIndex 描画している終端行インデックス（含まない）。
 * @param {number} params.totalItems
 * @param {number} params.rowHeightPx
 * @returns {{ topHeightPx: number, bottomHeightPx: number }}
 */
export function computeSpacerHeightsForScroll({
  startIndex,
  endIndex,
  totalItems,
  rowHeightPx,
}) {
  if (!isHeightScalingActive(totalItems, rowHeightPx)) {
    return computeSpacerHeights({ startIndex, endIndex, totalItems, rowHeightPx });
  }

  const clampedStart = Math.max(0, Math.min(totalItems, startIndex));
  const clampedEnd = Math.max(clampedStart, Math.min(totalItems, endIndex));
  const renderedRowCount = clampedEnd - clampedStart;
  const remainingRows = totalItems - renderedRowCount;

  if (remainingRows <= 0) {
    // 可視範囲＋バッファだけで表示集合全体を覆っている（総行数がごく少ない
    // 極端なケース）。スペーサは不要。
    return { topHeightPx: 0, bottomHeightPx: 0 };
  }

  const effectiveHeightPx = computeEffectiveTotalHeightPx(totalItems, rowHeightPx);
  const availableSpacerHeightPx = Math.max(
    0,
    effectiveHeightPx - renderedRowCount * rowHeightPx,
  );

  const topHeightPx = (availableSpacerHeightPx * clampedStart) / remainingRows;
  const bottomHeightPx =
    (availableSpacerHeightPx * (totalItems - clampedEnd)) / remainingRows;
  return { topHeightPx, bottomHeightPx };
}

/**
 * 継続行を結合した本文（`raw_text`）から、1行目（改行を含まない）だけを
 * 取り出す（`LOG-014`）。
 *
 * 折りたたみ方式（本ファイル冒頭のコメント「可変行高ではなく『折りたたみ
 * 方式』を採用した理由」参照）の行一覧表示で使う。行 DOM は固定行高
 * （`log_view.js` の `ROW_HEIGHT_PX`）のまま変えず、本文列には1行目だけを
 * 表示する。継続行を含む全文は「+N行」バッジ経由の詳細パネルで確認できる
 * （`log_view.js` の `showDetailPanel`）。
 *
 * `raw_text` 自体（コピー可能な原文、詳細パネルの表示元）は変更しない。この
 * 関数は行一覧表示用の派生文字列を作るだけである。
 *
 * @param {string} rawText
 * @returns {string} 最初の改行より前の部分（改行が無ければ `rawText` 全体）。
 */
export function extractFirstLine(rawText) {
  const newlineIndex = rawText.indexOf("\n");
  return newlineIndex === -1 ? rawText : rawText.slice(0, newlineIndex);
}

/**
 * ツールバーの「行番号でジャンプ」欄の入力値を、表示集合内の有効な行
 * インデックス（0起点）へ変換する（`tasks/phase-08-log-view.md` 作業項目4）。
 *
 * 入力は1起点の「行番号」（表示集合内の位置。継続行を結合した論理項目単位の
 * 位置であり、ファイル上の `source_line_number` とは一致しないことがある。
 * `source_line_number` を指定した検索・ジャンプは検索機能（`LOG-017`〜`019`、
 * P14）の対象であり、このフェーズの対象外）として解釈する。
 *
 * 数値として解釈できない入力（空文字・非数値など）は `null` を返し、
 * 呼び出し側は何もしない（現在のスクロール位置を変えない）。範囲外の数値は
 * 先頭または末尾の行へ丸める（作業項目4「範囲外は丸める」）。
 *
 * @param {string | number} rawValue
 * @param {number} totalItems 表示集合の総項目数。0以下なら常に `null`。
 * @returns {number | null} 0起点の行インデックス。
 */
export function parseJumpTargetRowIndex(rawValue, totalItems) {
  if (totalItems <= 0) {
    return null;
  }
  if (typeof rawValue === "string" && rawValue.trim() === "") {
    // `Number("")` は 0 を返す（JS の仕様上の落とし穴）ため、空文字（未入力・
    // 空白のみ）はここで明示的に無効値として扱う。
    return null;
  }
  const parsed = typeof rawValue === "number" ? rawValue : Number(rawValue);
  if (!Number.isFinite(parsed)) {
    return null;
  }
  const oneBasedClamped = Math.min(Math.max(Math.trunc(parsed), 1), totalItems);
  return oneBasedClamped - 1;
}

/**
 * 行インデックス（0起点）を、その行の先頭が可視範囲の先頭に来るスクロール
 * 位置（px）へ変換する（ジャンプ操作）。固定行高（折りたたみ方式。本ファイル
 * 冒頭のコメント参照）を前提とする単純な乗算だが、ジャンプ位置の決定が
 * 行インデックスの丸め（`parseJumpTargetRowIndex`）と分離されていることを
 * 明確にするため、独立した関数として切り出している。
 *
 * @param {number} rowIndex
 * @param {number} rowHeightPx
 * @returns {number}
 */
export function computeScrollTopForRowIndex(rowIndex, rowHeightPx) {
  return Math.max(0, rowIndex) * rowHeightPx;
}

/**
 * 行インデックス（0起点）を、その行の先頭が可視範囲の先頭に来るスクロール
 * 位置（px）へ変換する（ジャンプ操作、スクロール高クランプ対応版）。
 *
 * 総理論高さ（`totalItems × rowHeightPx`）が `MAX_TOTAL_HEIGHT_PX` 以下の
 * 通常規模では `computeScrollTopForRowIndex`（従来の1:1方式）へそのまま委譲
 * する（回帰を避ける設計判断）。
 *
 * 上限を超える場合は `computeVisibleRangeForScroll` が使う比例写像
 * （スクロール位置の比例 → 行インデックス）の逆変換（行インデックス →
 * 比例 → スクロール位置）を使う。往復（行インデックス→スクロール位置→
 * `computeVisibleRangeForScroll` で先頭行インデックスを再計算）は、比例
 * 写像に伴う微小な離散化誤差（`computeVisibleRangeForScroll` のコメント
 * 参照）の範囲内で一致する。
 *
 * @param {number} rowIndex
 * @param {number} rowHeightPx
 * @param {number} totalItems
 * @param {number} maxScrollTopPx ブラウザの実際の最大スクロール位置
 *   （`viewport.scrollHeight - viewport.clientHeight`）。0以下なら常に0を返す。
 * @returns {number}
 */
export function computeScrollTopForRowIndexScaled(
  rowIndex,
  rowHeightPx,
  totalItems,
  maxScrollTopPx,
) {
  if (!isHeightScalingActive(totalItems, rowHeightPx)) {
    return computeScrollTopForRowIndex(rowIndex, rowHeightPx);
  }
  if (totalItems <= 0 || maxScrollTopPx <= 0) {
    return 0;
  }
  const clampedRowIndex = Math.min(Math.max(0, rowIndex), totalItems);
  const proportion = clampedRowIndex / totalItems;
  return Math.min(maxScrollTopPx, Math.max(0, proportion * maxScrollTopPx));
}
