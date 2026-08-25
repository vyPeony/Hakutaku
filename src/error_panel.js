// 対象を開く操作のエラー・通知のモーダルダイアログ（Issue #9）。
//
// `ERR-002` の5要素（対象・発生位置・理由・継続可否・次操作）を構造化して
// 表示する共通コンポーネント。`src/banner.js`（#config-banners、画面上部）が
// 扱う `CFG-015`、`CFG-016` の起動時通知とは別の機構で、対象を開く・再試行する
// 操作（src/shell.js）から呼ばれる。
//
// 表示方式: モーダルダイアログで1件ずつ表示し、画面へ累積させない
// （Issue #9 の裁定）。同時期に複数のエラー・通知が発生した場合は表示待ち
// キューへ積み、利用者が1件閉じるたびに次の1件を表示する（取りこぼさない）。
// `ERR-002` が規定するのは5要素の内容であり、表示の方式・領域は規定していない。
// HTML ネイティブの <dialog> を遅延生成して `showModal()` で表示し、閉じる
// 手段はダイアログ内の「閉じる」ボタンと <dialog> ネイティブの Esc の2つ。
//
// # 表示待ちキューの集約（Issue #49 の裁定）
//
// 同じ理由の失敗が同時期に何件も起きると（複数の対象を続けて開いて同じ理由で
// 失敗した場合など）、利用者は同じ文面のダイアログを件数ぶん閉じることになる。
// そのため、**キューへ積む時点で**同一理由の通知を1件へまとめる。
//
//   - 同一性の判定: `mergeKey`（通知の種別・見出し・理由など、**対象名以外の
//     表示内容**から呼び出し側が組み立てる）。一致した場合は新しい通知を積まず、
//     既にキューにある通知の `targets`（対象一覧）へ対象を追記する。同じ対象は
//     二重に足さない
//   - **表示中のダイアログとは集約しない**（Issue #9 の1件ずつ表示の裁定を
//     維持する）。開いているダイアログの内容をその場で差し替えると、読んで
//     いる最中の内容が黙って変わり、フォーカスと支援技術への通知も失われる
//     （`requestNotice` の doc コメント参照）。表示中のものと同じ理由の通知は
//     キューの1件目として積まれ、閉じた後に表示される
//   - 集約された通知の本文は、対象が2件以上のとき「対象」欄を一覧にして
//     表示する（`buildTargetsRow`）。1件だけのときの見た目は従来と変わらない
//
// `ERR-002` は「`DIAG-003`、`DIAG-004` により実値の表示を制限しないため、
// 原因調査に必要であればフルパスを表示してよい」と定めている。このモジュールは
// いずれのフィールドもマスキング・切り詰めしない。
//
// `showTargetError` の見出しは既定で「対象を開けませんでした」だが、対象が
// Error へ遷移せず閲覧を継続できる失敗（例: 再読み込みの上限超過拒否）では
// 実態と食い違うため、呼び出し側（`src/shell.js`）が `headingText` で
// 差し替えられる（Issue #11）。
//
// ADR-0006 に従い、フレームワーク・バンドラーを使わない素の ES モジュール。

/**
 * @typedef {import("./targets.js").UserFacingErrorDto} UserFacingErrorDto
 */

/**
 * 表示待ちの通知1件分。
 *
 * - `className`: <dialog> に設定するクラス（基本の `status-dialog`、情報
 *   バリアントは `status-dialog--info` を追加）
 * - `headingText`: 見出しの文。aria-label にも同じ文を使う
 * - `mergeKey`: 表示待ちキュー内での同一性キー（Issue #49。種別と理由から作り、
 *   対象名は含めない）
 * - `targets`: この通知が対象とするものの一覧（Issue #49。集約されるたびに
 *   追記される）
 * - `buildBody`: 見出しと「閉じる」フッターの間に入る本文要素を組み立てる。
 *   キューから取り出されて表示される瞬間に、その時点の `targets` を受け取って
 *   呼ばれる（集約された分をすべて反映した本文になる）
 *
 * @typedef {{
 *   className: string,
 *   headingText: string,
 *   mergeKey: string,
 *   targets: string[],
 *   buildBody: (targets: string[]) => Node[],
 * }} PendingNotice
 */

/**
 * モジュール全体で使い回す単一の <dialog>（遅延生成）。
 *
 * @type {HTMLDialogElement | null}
 */
let sharedDialog = null;

/**
 * 表示待ちの通知キュー（FIFO）。ダイアログが開いている間に届いた表示要求を
 * ここへ積み、利用者が1件閉じるたびに先頭から取り出して表示する。
 *
 * @type {PendingNotice[]}
 */
const pendingNotices = [];

/** 共有の <dialog> を body の末尾に用意する（無ければ作る）。 */
function ensureDialog() {
  if (!sharedDialog) {
    const dialog = document.createElement("dialog");
    // 「閉じる」ボタンでも Esc（<dialog> ネイティブ）でも close イベントを
    // 通るため、閉じ方によらずここでキューの次の1件へ進める。close イベントは
    // close() から同期では呼ばれず、キューされたタスクとして遅れて発火する
    // （HTML 仕様）。close() 直後の閉じた状態を見た新しい表示要求が、発火
    // までの間に直接開き直している場合があるため、閉じたままのときだけ
    // 次を取り出し、表示中の内容を上書きしない。
    dialog.addEventListener("close", () => {
      if (dialog.open) {
        return;
      }
      const next = pendingNotices.shift();
      if (next) {
        renderNotice(dialog, next);
      } else {
        // 表示待ちが無ければ内容を空にし、閉じたダイアログへ古い表示を残さない。
        dialog.textContent = "";
      }
    });
    document.body.appendChild(dialog);
    sharedDialog = dialog;
  }
  return sharedDialog;
}

/**
 * 表示要求の共通入口。ダイアログが閉じていればすぐ表示し、開いていれば
 * キューへ積んで、利用者が現在の1件を閉じたときに順に表示する。
 *
 * 開いているダイアログの内容をその場で差し替える経路は持たない。差し替えると
 * フォーカスの当たっていた要素が消えてフォーカスが body へ落ち、支援技術へ
 * 新しい内容が通知されず、読み終える前のエラーが黙って消えるため、表示は
 * 常に「閉じた状態からの showModal()」だけにする。
 *
 * キューへ積む前に、同一理由の通知が既に待っていないかを調べる（Issue #49。
 * モジュール冒頭のコメント「表示待ちキューの集約」参照）。
 *
 * @param {PendingNotice} notice
 */
function requestNotice(notice) {
  const dialog = ensureDialog();
  if (dialog.open) {
    if (!mergeIntoPendingNotice(notice)) {
      pendingNotices.push(notice);
    }
    return;
  }
  renderNotice(dialog, notice);
}

/**
 * 同一理由（`mergeKey` が一致）の通知が表示待ちキューにあれば、その通知の
 * 対象一覧へ `notice` の対象を追記して `true` を返す（Issue #49）。
 *
 * 一致するものが無ければ `false` を返し、呼び出し側が新しい1件として積む。
 * 同じ対象を二重に足さないのは、同一の失敗が短時間に繰り返し通知される経路
 * （再試行、複数チャンクの同時失敗）で対象名が並ぶだけになるのを避けるため。
 *
 * @param {PendingNotice} notice
 * @returns {boolean} 集約したか。
 */
function mergeIntoPendingNotice(notice) {
  const existing = pendingNotices.find((pending) => pending.mergeKey === notice.mergeKey);
  if (existing === undefined) {
    return false;
  }
  for (const target of notice.targets) {
    if (!existing.targets.includes(target)) {
      existing.targets.push(target);
    }
  }
  return true;
}

/**
 * 通知1件をダイアログへ描画してモーダル表示する。閉じた状態のダイアログに
 * 対してだけ呼ぶ（開いたままの差し替えはしない。`requestNotice` 参照）。
 *
 * @param {HTMLDialogElement} dialog
 * @param {PendingNotice} notice
 */
function renderNotice(dialog, notice) {
  dialog.textContent = "";
  dialog.className = notice.className;
  dialog.setAttribute("aria-label", notice.headingText);

  const heading = document.createElement("p");
  heading.className = "status-dialog__heading";
  heading.textContent = notice.headingText;
  // showModal() の自動フォーカスは、既定では最初のフォーカス可能要素＝末尾の
  // 「閉じる」ボタンに当たる。それだと内容が長いときに末尾までスクロール
  // した状態で開き、非同期表示の瞬間に打鍵中だった Enter がそのまま
  // 「閉じる」を押して即閉じし得る。tabindex="-1" と autofocus で自動
  // フォーカスを見出しへ向け、先頭から読める状態で開くようにする。
  heading.tabIndex = -1;
  heading.setAttribute("autofocus", "");
  dialog.appendChild(heading);

  // 集約された対象（Issue #49）をすべて反映するため、表示する瞬間の `targets`
  // を渡す（キューで待つ間に対象が追記されている場合がある）。
  for (const node of notice.buildBody(notice.targets)) {
    dialog.appendChild(node);
  }

  dialog.appendChild(buildCloseFooter(dialog));
  dialog.showModal();
}

/**
 * ダイアログ末尾の「閉じる」ボタン行を作る。ボタンは他の操作ボタンと同じ
 * 既定の見た目のため、ボタン専用のクラスは付けない。
 *
 * @param {HTMLDialogElement} dialog
 */
function buildCloseFooter(dialog) {
  const footer = document.createElement("div");
  footer.className = "status-dialog__footer";

  const closeButton = document.createElement("button");
  closeButton.type = "button";
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
 * 「対象」欄の定義リスト行を作る（Issue #49）。集約された対象が2件以上の場合
 * だけ一覧（<ul>）にし、1件のときは他の欄と同じ1行の表示にする（集約が起きて
 * いないときの見た目を変えないため）。
 *
 * @param {string[]} targets
 * @returns {Node[]}
 */
function buildTargetsRow(targets) {
  if (targets.length <= 1) {
    return buildDefinitionRow("対象", targets[0] ?? "（不明）");
  }

  const dt = document.createElement("dt");
  dt.className = "status-dialog__term";
  dt.textContent = `対象（${targets.length}件）`;

  const dd = document.createElement("dd");
  dd.className = "status-dialog__description";
  const list = document.createElement("ul");
  list.className = "status-dialog__targets";
  for (const target of targets) {
    const li = document.createElement("li");
    li.textContent = target;
    list.appendChild(li);
  }
  dd.appendChild(list);

  return [dt, dd];
}

/**
 * 表示待ちキューの同一性キー（[`PendingNotice`] の `mergeKey`）の各要素を
 * 連結する区切り文字（U+0000）。理由や見出しに現れない制御文字を選ぶ理由は
 * `src/banner.js` の `KEY_SEPARATOR` と同じ。ソースへ制御文字をそのまま
 * 埋め込まないよう、コードポイントから組み立てる。
 */
const MERGE_KEY_SEPARATOR = String.fromCharCode(0);

/**
 * `ERR-002` の5要素を持つ利用者向けエラーを、モーダルダイアログで表示する。
 * 対象・発生位置・理由・継続可否・次操作のすべてを構造化して表示し、
 * いずれもマスキングしない（フルパスを含んでよい）。別の通知を表示中の
 * 場合は、それが閉じられた後に順に表示される。
 *
 * 見出しは既定で「対象を開けませんでした」。対象が Error へ遷移せず、
 * 旧スナップショットの閲覧を継続できる失敗（例: 再読み込みの上限超過拒否）
 * では既定の見出しが実態と食い違うため、`options.headingText` で呼び出し側
 * が差し替えられる（Issue #11）。省略時は従来どおりの挙動を保つ。
 *
 * @param {UserFacingErrorDto} error
 * @param {{ headingText?: string }} [options] `headingText` を指定すると
 *   既定の見出しを置き換える（aria-label にも同じ文が使われる）。
 */
export function showTargetError(error, options = {}) {
  const headingText = options.headingText ?? "対象を開けませんでした";
  requestNotice({
    className: "status-dialog",
    headingText,
    // 同一理由の判定（Issue #49）: **対象名（`error.target`）以外のすべて**が
    // 一致するものを1件へまとめる。対象が違っても理由が同じなら1件にまとめ、
    // 対象一覧として並べるのがこの集約の目的である。一方、対象名以外の欄
    // （発生位置・継続可否・次の操作・エラーコード）は集約後に1組しか表示
    // できないため、1つでも違えば別の通知として積む（まとめた側の値を、
    // 実際には違う対象の説明として見せないため）。
    mergeKey: [
      "target-error",
      headingText,
      error.reason,
      error.location ?? "",
      String(error.continuable),
      error.next_action,
      error.error_code ?? "",
    ].join(MERGE_KEY_SEPARATOR),
    targets: [error.target],
    buildBody: (targets) => {
      const list = document.createElement("dl");
      list.className = "status-dialog__fields";

      for (const node of buildTargetsRow(targets)) {
        list.appendChild(node);
      }

      const rows = [
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

      return [list];
    },
  });
}

/**
 * `LOG-022` の非致命的な通知（日時未解析の生表示へ退避したことの案内）を、
 * 同じダイアログ機構の情報バリアント（青系）で表示する。エラーではないため
 * `showTargetError` の赤系とは区別できる控えめな配色にする。別の通知を
 * 表示中の場合は、それが閉じられた後に順に表示される。
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
  const headingText = `${targetLabel}: 日時未解析の生表示で開きました`;
  requestNotice({
    className: "status-dialog status-dialog--info",
    headingText,
    // 見出しに対象名が入るため、同じ対象の重複だけがキューで集約される
    // （別の対象は見出しが違うので別の1件として残る。Issue #49）。本文は
    // 対象一覧を使わない固定の案内文なので `targets` は集約の判定にのみ効く。
    mergeKey: ["raw-display-fallback", headingText].join(MERGE_KEY_SEPARATOR),
    targets: [targetLabel],
    buildBody: () => {
      const text = document.createElement("p");
      text.className = "status-dialog__text";
      text.textContent =
        "日時書式またはログ解析プロファイルを一意に決定できなかったため、" +
        "全行を日時未解析のまま表示しています（LOG-022）。参照対象一覧のこの対象の" +
        "行から、ログ解析プロファイルと日時書式のいずれか、または両方を指定して" +
        "開き直すことができます。設定ファイルに書いていないファイルでも、" +
        "日時書式（LOG-DT-001〜006）を選べばその書式で解析できます。";
      return [text];
    },
  });
}
