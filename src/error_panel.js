// 下部の状態・エラー表示領域（P07-1）。
//
// `ERR-002` の5要素（対象・発生位置・理由・継続可否・次操作）を構造化して
// 表示する共通コンポーネント。`src/banner.js`（#config-banners、画面上部）が
// 扱う CFG-015／CFG-016 の起動時通知とは別の領域（#status-error-area、画面
// 下部）に表示する。対象を開く・再試行する操作（src/shell.js）から呼ばれる。
//
// `ERR-002` は「`DIAG-003`／`DIAG-004` により実値の表示を制限しないため、
// 原因調査に必要であればフルパスを表示してよい」と定めている。このモジュールは
// いずれのフィールドもマスキング・切り詰めしない。
//
// バナーと同様、操作をブロックしない非致命的な表示として扱う（モーダルに
// しない）。ADR-0006 に従い、フレームワーク・バンドラーを使わない素の ES
// モジュール。

/**
 * @typedef {import("./targets.js").UserFacingErrorDto} UserFacingErrorDto
 */

/** 状態・エラー表示領域の共通コンテナを、body の末尾に用意する（無ければ作る）。 */
export function ensureStatusErrorContainer() {
  let container = document.getElementById("status-error-area");
  if (!container) {
    container = document.createElement("div");
    container.id = "status-error-area";
    document.body.appendChild(container);
  }
  return container;
}

/**
 * バナーへ添える閉じるボタンを作る（`src/banner.js` の `createCloseButton` と
 * 同じ作法。コンテナが別のため、依存を増やさずここでも小さく複製する）。
 *
 * @param {HTMLElement} panel
 */
function createCloseButton(panel) {
  const closeButton = document.createElement("button");
  closeButton.type = "button";
  closeButton.className = "status-error-panel__close";
  closeButton.setAttribute("aria-label", "エラー表示を閉じる");
  closeButton.textContent = "×";
  closeButton.addEventListener("click", () => panel.remove());
  return closeButton;
}

/**
 * 1件分の定義リスト行（見出しと値）を作る。
 *
 * @param {string} term
 * @param {string} description
 */
function buildDefinitionRow(term, description) {
  const dt = document.createElement("dt");
  dt.className = "status-error-panel__term";
  dt.textContent = term;

  const dd = document.createElement("dd");
  dd.className = "status-error-panel__description";
  dd.textContent = description;

  return [dt, dd];
}

/**
 * `ERR-002` の5要素を持つ利用者向けエラーを、下部の状態・エラー表示領域へ
 * 表示する。対象・発生位置・理由・継続可否・次操作のすべてを構造化して表示し、
 * いずれもマスキングしない（フルパスを含んでよい）。
 *
 * @param {UserFacingErrorDto} error
 */
export function showTargetError(error) {
  const container = ensureStatusErrorContainer();

  const panel = document.createElement("div");
  panel.className = "status-error-panel";
  panel.setAttribute("role", "alert");

  const header = document.createElement("div");
  header.className = "status-error-panel__header";
  const heading = document.createElement("p");
  heading.className = "status-error-panel__heading";
  heading.textContent = "対象を開けませんでした";
  header.appendChild(heading);
  header.appendChild(createCloseButton(panel));
  panel.appendChild(header);

  const list = document.createElement("dl");
  list.className = "status-error-panel__fields";

  const rows = [
    ["対象", error.target],
    ["発生位置", error.location ?? "（特定できません）"],
    ["理由", error.reason],
    ["継続可否", error.continuable ? "継続可能（他の対象は引き続き閲覧できます）" : "続行不可"],
    ["次の操作", error.next_action],
  ];
  if (error.error_code) {
    rows.push(["エラーコード", error.error_code]);
  }
  for (const [term, description] of rows) {
    const [dt, dd] = buildDefinitionRow(term, description);
    list.appendChild(dt);
    list.appendChild(dd);
  }
  panel.appendChild(list);

  container.appendChild(panel);
}

/**
 * `LOG-022` の非致命的な通知（日時未解析の生表示へ退避したことの案内）を
 * 状態・エラー表示領域へ表示する。エラーではないため `showTargetError` とは
 * 別の、控えめな見た目で表示する。
 *
 * 手動でのログ解析プロファイル選択（P07-2）と日時書式選択は、
 * 対象一覧（`src/shell.js`）の行内に常設される
 * 「選んで再解析」操作（`src/shell.js` の `buildReparseControl`）から行う。
 * この通知自体は、その操作へ気づく前の最初の手がかりとして「生表示に
 * なったこと」を伝えるためのものである。
 *
 * @param {string} targetLabel 対象の表示名。
 */
export function showRawDisplayFallbackNotice(targetLabel) {
  const container = ensureStatusErrorContainer();

  const panel = document.createElement("div");
  panel.className = "status-error-panel status-error-panel--info";
  panel.setAttribute("role", "status");

  const header = document.createElement("div");
  header.className = "status-error-panel__header";
  const heading = document.createElement("p");
  heading.className = "status-error-panel__heading";
  heading.textContent = `${targetLabel}: 日時未解析の生表示で開きました`;
  header.appendChild(heading);
  header.appendChild(createCloseButton(panel));
  panel.appendChild(header);

  const text = document.createElement("p");
  text.className = "status-error-panel__text";
  text.textContent =
    "日時書式またはログ解析プロファイルを一意に決定できなかったため、" +
    "全行を日時未解析のまま表示しています（LOG-022）。参照対象一覧のこの対象の" +
    "行から、ログ解析プロファイルと日時書式のいずれか、または両方を指定して" +
    "開き直すことができます。設定ファイルに書いていないファイルでも、" +
    "日時書式（LOG-DT-001〜006）を選べばその書式で解析できます。";
  panel.appendChild(text);

  container.appendChild(panel);
}

/** 状態・エラー表示領域を空にする（全パネルを閉じる）。 */
export function clearStatusErrorArea() {
  const container = document.getElementById("status-error-area");
  if (container) {
    container.textContent = "";
  }
}
