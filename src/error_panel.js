// 対象を開く操作のエラー・通知のモーダルダイアログ（Issue #9）。
//
// `ERR-002` の5要素（対象・発生位置・理由・継続可否・次操作）を構造化して
// 表示する共通コンポーネント。`src/banner.js`（#config-banners、画面上部）が
// 扱う CFG-015／CFG-016 の起動時通知とは別の機構で、対象を開く・再試行する
// 操作（src/shell.js）から呼ばれる。
//
// 表示方式: P07-1 の旧裁定「操作をブロックしない・モーダルにしない」のもとでは
// 画面下部の累積表示領域へパネルを積み重ねていたが、エラーのたびにパネルが
// 増えていく違和感への利用者フィードバックを受け、Issue #9 の裁定で
// 「モーダルダイアログで1回表示」へ変更した。`ERR-002` が規定するのは5要素の
// 内容であり、表示の方式・領域は規定していないため、この変更で要件との対応は
// 変わらない。HTML ネイティブの <dialog> を遅延生成して `showModal()` で
// 表示し、閉じる手段はダイアログ内の「閉じる」ボタンと <dialog> ネイティブの
// Esc の2つ。連続して呼ばれた場合は、開いているダイアログの内容を最新の
// 1件へ置き換える（積み重ねない）。
//
// `ERR-002` は「`DIAG-003`／`DIAG-004` により実値の表示を制限しないため、
// 原因調査に必要であればフルパスを表示してよい」と定めている。このモジュールは
// いずれのフィールドもマスキング・切り詰めしない。
//
// ADR-0006 に従い、フレームワーク・バンドラーを使わない素の ES モジュール。

/**
 * @typedef {import("./targets.js").UserFacingErrorDto} UserFacingErrorDto
 */

/**
 * モジュール全体で使い回す単一の <dialog>（遅延生成）。表示のたびに内容を
 * 丸ごと作り直すため、画面に出るのは常に最新の1件だけになる。
 *
 * @type {HTMLDialogElement | null}
 */
let sharedDialog = null;

/** 共有の <dialog> を body の末尾に用意する（無ければ作る）。 */
function ensureDialog() {
  if (!sharedDialog) {
    const dialog = document.createElement("dialog");
    // 「閉じる」ボタンでも Esc（<dialog> ネイティブ）でも close イベントを
    // 通るため、閉じ方によらずここで内容を空にし、古い表示を残さない。
    // close イベントは close() から同期では呼ばれず、キューされたタスクとして
    // 遅れて発火する（HTML 仕様）。発火時点で次の表示が既に開き直していた
    // 場合に、その表示中の内容まで消してしまわないよう、閉じたままのとき
    // だけ空にする。
    dialog.addEventListener("close", () => {
      if (!dialog.open) {
        dialog.textContent = "";
      }
    });
    document.body.appendChild(dialog);
    sharedDialog = dialog;
  }
  return sharedDialog;
}

/**
 * 組み立て済みのダイアログをモーダル表示する。既に開いている場合は何も
 * しない（`dialog.open` のまま `showModal()` を再呼び出しすると
 * InvalidStateError になるため必ずガードする。その場合、呼び出し側が
 * 内容を最新の1件へ置き換え済みで、開いたままのダイアログにそのまま映る）。
 *
 * @param {HTMLDialogElement} dialog
 */
function presentDialog(dialog) {
  if (!dialog.open) {
    dialog.showModal();
  }
}

/**
 * ダイアログ末尾の「閉じる」ボタン行を作る。
 *
 * @param {HTMLDialogElement} dialog
 */
function buildCloseFooter(dialog) {
  const footer = document.createElement("div");
  footer.className = "status-dialog__footer";

  const closeButton = document.createElement("button");
  closeButton.type = "button";
  closeButton.className = "status-dialog__close";
  closeButton.textContent = "閉じる";
  closeButton.addEventListener("click", () => dialog.close());
  footer.appendChild(closeButton);

  return footer;
}

/**
 * 1件分の定義リスト行（見出しと値）を作る。
 *
 * @param {string} term
 * @param {string} description
 */
function buildDefinitionRow(term, description) {
  const dt = document.createElement("dt");
  dt.className = "status-dialog__term";
  dt.textContent = term;

  const dd = document.createElement("dd");
  dd.className = "status-dialog__description";
  dd.textContent = description;

  return [dt, dd];
}

/**
 * `ERR-002` の5要素を持つ利用者向けエラーを、モーダルダイアログで表示する。
 * 対象・発生位置・理由・継続可否・次操作のすべてを構造化して表示し、
 * いずれもマスキングしない（フルパスを含んでよい）。
 *
 * @param {UserFacingErrorDto} error
 */
export function showTargetError(error) {
  const dialog = ensureDialog();
  dialog.textContent = "";
  dialog.className = "status-dialog";
  dialog.setAttribute("aria-label", "対象を開けませんでした");

  const heading = document.createElement("p");
  heading.className = "status-dialog__heading";
  heading.textContent = "対象を開けませんでした";
  dialog.appendChild(heading);

  const list = document.createElement("dl");
  list.className = "status-dialog__fields";

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
  dialog.appendChild(list);

  dialog.appendChild(buildCloseFooter(dialog));
  presentDialog(dialog);
}

/**
 * `LOG-022` の非致命的な通知（日時未解析の生表示へ退避したことの案内）を、
 * 同じダイアログ機構の情報バリアント（青系）で表示する。エラーではないため
 * `showTargetError` の赤系とは区別できる控えめな配色にする。
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
  const dialog = ensureDialog();
  dialog.textContent = "";
  dialog.className = "status-dialog status-dialog--info";
  const headingText = `${targetLabel}: 日時未解析の生表示で開きました`;
  dialog.setAttribute("aria-label", headingText);

  const heading = document.createElement("p");
  heading.className = "status-dialog__heading";
  heading.textContent = headingText;
  dialog.appendChild(heading);

  const text = document.createElement("p");
  text.className = "status-dialog__text";
  text.textContent =
    "日時書式またはログ解析プロファイルを一意に決定できなかったため、" +
    "全行を日時未解析のまま表示しています（LOG-022）。参照対象一覧のこの対象の" +
    "行から、ログ解析プロファイルと日時書式のいずれか、または両方を指定して" +
    "開き直すことができます。設定ファイルに書いていないファイルでも、" +
    "日時書式（LOG-DT-001〜006）を選べばその書式で解析できます。";
  dialog.appendChild(text);

  dialog.appendChild(buildCloseFooter(dialog));
  presentDialog(dialog);
}
