// 通知バナーの共通機構（P03-2 で導入）。
//
// P04-2 で、ログ表示ビュー（log_view.js）が open_log_file の
// failed を「既存バナー機構の作法で警告表示」するために、この機構を
// src/main.js から切り出して独立モジュール化した（tasks/phase-04-vertical-slice.md
// 「エラー表示」）。main.js（設定状態の通知）と log_view.js（ログ操作の通知）の
// 両方がこのモジュールを呼び出す。
//
// バナーは操作をブロックしない（モーダル等にしない）非致命的な通知として表示する。
// ビルドツールを使わない素の ES モジュール（ADR-0006）。
//
// 表示方式: 同一内容のバナーは1枚に集約する（Issue #11 の裁定）。呼び出し側には
// 同じ内容を重ねて通知し得るものがある（可視範囲の複数チャンクの取得が同時に
// 失敗した場合や、別々の呼び出し元が同じ文面を出す場合。各呼び出し元の通知抑制
// —— 例: src/log_view.js は失敗ストリークの1件目だけ通知する —— を貫通して同文
// が届くことは依然あり得る）ため、要求ごとに1枚を積むと同じ文が画面上部を
// 埋め尽くし、他の通知も操作領域も押しのける。
// 裁定の内訳は次のとおり。
//
//   - 同一性キー: 種別（info / warning / error）+ 見出し（warning のみ）+ 本文 +
//     一覧項目（items）。通知文には現れない U+0000 で連結する
//   - 2回目以降: 新しいバナーを足さず、表示中のバナーのテキストへ「（N回目）」を
//     付けて更新する（N は 2 から）。warning は見出しの末尾へ付ける。DOM 上の
//     位置は動かさない（読んでいる最中に並び順が変わらないようにする）
//   - 閉じるボタンで閉じたら、その内容の回数は破棄する。次の同じ内容は回数なしの
//     新しいバナーとして表示する（利用者が一度片付けた後の再発は、続きではなく
//     新しい出来事として見せる）
//   - 異なる内容のバナー同士には枚数の上限を設けない（原因が異なる通知はどれも
//     個別に残す価値があり、枚数は実際にはメッセージの種類数で頭打ちになる）

/**
 * 表示中のバナー1枚分の集約状態（Issue #11）。
 *
 * - `banner`: #config-banners 直下のバナー要素
 * - `countTarget`: 「（N回目）」を書き足すテキスト要素（info / error は本文、
 *   warning は見出し）
 * - `baseText`: 回数を付ける前の文。表示のたびにこの文から組み立て直す
 * - `count`: 同じ内容の表示要求を受けた回数（初回の表示が 1）
 *
 * @typedef {{
 *   banner: HTMLElement,
 *   countTarget: HTMLElement,
 *   baseText: string,
 *   count: number,
 * }} ActiveBanner
 */

/**
 * 同一性キー → 表示中のバナーの集約状態。バナーが閉じられた時点でエントリを
 * 削除し、DOM から切り離した要素への参照を残さない。
 *
 * @type {Map<string, ActiveBanner>}
 */
const activeBanners = new Map();

/**
 * 同一性キーの各要素を連結する区切り文字（U+0000）。通知文（利用者向けの日本語と、
 * ファイルパスなどの実値）には現れない制御文字を選び、「本文の末尾」と「次の項目の
 * 先頭」がつながって別内容のキーと一致する事故を避ける。ソースへ制御文字をそのまま
 * 埋め込まないよう、コードポイントから組み立てる。
 */
const KEY_SEPARATOR = String.fromCharCode(0);

/**
 * バナーの同一性キーを作る（Issue #11）。種別・見出し・本文・一覧項目が
 * すべて一致するものを「同じ内容」とみなす。
 *
 * @param {string} kind 種別（"info" / "warning" / "error"）
 * @param {string} heading 見出し（warning 以外は空文字）
 * @param {string} body 本文（warning は空文字）
 * @param {string[]} items 一覧項目（warning 以外は空配列）
 */
function buildBannerKey(kind, heading, body, items) {
  return [kind, heading, body, ...items].join(KEY_SEPARATOR);
}

/**
 * 同じ内容のバナーが表示中なら回数表示を更新し、`true` を返す（新しいバナーは
 * 作らない）。表示中でなければ `false` を返し、呼び出し側が新規に組み立てる。
 *
 * @param {string} key
 */
function countUpExistingBanner(key) {
  const active = activeBanners.get(key);
  if (!active) {
    return false;
  }
  // 閉じるボタン以外の経路で DOM から外れている場合（コンテナごと差し替えられた
  // 場合など）は、見えないバナーの回数を数えても利用者へ伝わらない。状態を捨てて
  // 新規表示へ委ね、切り離した要素への参照もここで手放す。
  if (!active.banner.isConnected) {
    activeBanners.delete(key);
    return false;
  }
  active.count += 1;
  // 回数だけを別の要素として足すのではなく、テキスト要素の textContent を
  // 丸ごと書き換える。バナーの role=status / role=alert に対する内容の変更として
  // 支援技術へ再通知されることを期待するため（Issue #11 の裁定）。
  active.countTarget.textContent = `${active.baseText}（${active.count}回目）`;
  return true;
}

/**
 * 新しく表示したバナーを、以後の同一内容の集約対象として登録する。
 *
 * @param {string} key 同一性キー
 * @param {HTMLElement} banner
 * @param {HTMLElement} countTarget 「（N回目）」を書き足すテキスト要素
 * @param {string} baseText 回数を付ける前の文
 */
function registerBanner(key, banner, countTarget, baseText) {
  activeBanners.set(key, { banner, countTarget, baseText, count: 1 });
}

/** 通知バナーの共通コンテナを、body の先頭に用意する（無ければ作る）。 */
export function ensureBannerContainer() {
  let container = document.getElementById("config-banners");
  if (!container) {
    container = document.createElement("div");
    container.id = "config-banners";
    document.body.insertBefore(container, document.body.firstChild);
  }
  return container;
}

/**
 * バナーへ添える閉じるボタンを作る。
 *
 * `key` を渡すと、閉じたときに集約状態（回数）も破棄する。以後、同じ内容の通知が
 * 起きたら回数なしの新しいバナーとして表示される（Issue #11 の裁定）。省略した
 * 場合は DOM から取り除くだけで、集約には関与しない。
 *
 * @param {HTMLElement} banner
 * @param {string} [key] 集約の同一性キー
 */
export function createCloseButton(banner, key) {
  const closeButton = document.createElement("button");
  closeButton.type = "button";
  closeButton.className = "config-banner__close";
  closeButton.setAttribute("aria-label", "通知を閉じる");
  closeButton.textContent = "×";
  closeButton.addEventListener("click", () => {
    banner.remove();
    // 同じキーで既に別のバナーが作り直されている場合に、そちらの状態まで
    // 巻き添えで消さないよう、自分の登録だけを取り消す。
    if (key !== undefined && activeBanners.get(key)?.banner === banner) {
      activeBanners.delete(key);
    }
  });
  return closeButton;
}

/**
 * 閉じるボタン付きの情報バナーを表示する（CFG-015 など、非致命的な通知）。
 *
 * 同じ文の情報バナーが表示中の場合は、新しいバナーを足さずに既存の文末へ
 * 「（N回目）」を付けて更新する（Issue #11）。
 *
 * @param {string} message
 */
export function showInfoBanner(message) {
  const key = buildBannerKey("info", "", message, []);
  if (countUpExistingBanner(key)) {
    return;
  }

  const container = ensureBannerContainer();

  const banner = document.createElement("div");
  banner.className = "config-banner config-banner--info";
  banner.setAttribute("role", "status");

  const text = document.createElement("span");
  text.className = "config-banner__text";
  text.textContent = message;
  banner.appendChild(text);

  banner.appendChild(createCloseButton(banner, key));
  container.appendChild(banner);
  registerBanner(key, banner, text, message);
}

/**
 * 見出しと一覧を持つ警告バナーを表示する（CFG-016 の安全モードなど）。
 * UI 自体は既に起動済みであり、この表示は操作をブロックしない。
 *
 * 見出しと一覧が同じ警告バナーが表示中の場合は、新しいバナーを足さずに見出しの
 * 末尾へ「（N回目）」を付けて更新する（Issue #11。一覧は複数行になり得るため、
 * 回数は先頭の見出しへ添える）。
 *
 * @param {string} heading
 * @param {string[]} [items] 一覧表示する、呼び出し側で整形済みの文字列（省略時は一覧を出さない）。
 */
export function showWarningBanner(heading, items = []) {
  const key = buildBannerKey("warning", heading, "", items);
  if (countUpExistingBanner(key)) {
    return;
  }

  const container = ensureBannerContainer();

  const banner = document.createElement("div");
  banner.className = "config-banner config-banner--warning";
  banner.setAttribute("role", "alert");

  const headingEl = document.createElement("p");
  headingEl.className = "config-banner__heading";
  headingEl.textContent = heading;
  banner.appendChild(headingEl);

  if (items.length > 0) {
    const list = document.createElement("ul");
    list.className = "config-banner__errors";
    for (const item of items) {
      const li = document.createElement("li");
      li.textContent = item;
      list.appendChild(li);
    }
    banner.appendChild(list);
  }

  banner.appendChild(createCloseButton(banner, key));
  container.appendChild(banner);
  registerBanner(key, banner, headingEl, heading);
}

/**
 * エラーバナーを表示する（致命的ではないが利用者の注意を要する失敗。
 * open_log_file の failed、範囲取得の失敗など）。
 *
 * 同じ文のエラーバナーが表示中の場合は、新しいバナーを足さずに既存の文末へ
 * 「（N回目）」を付けて更新する（Issue #11。スクロール中の範囲取得のように、
 * 同じ失敗が短時間に何度も起きる呼び出し経路がある）。
 *
 * @param {string} message
 */
export function showErrorBanner(message) {
  const key = buildBannerKey("error", "", message, []);
  if (countUpExistingBanner(key)) {
    return;
  }

  const container = ensureBannerContainer();

  const banner = document.createElement("div");
  banner.className = "config-banner config-banner--error";
  banner.setAttribute("role", "alert");

  const text = document.createElement("span");
  text.className = "config-banner__text";
  text.textContent = message;
  banner.appendChild(text);

  banner.appendChild(createCloseButton(banner, key));
  container.appendChild(banner);
  registerBanner(key, banner, text, message);
}
