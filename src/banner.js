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
 * @param {HTMLElement} banner
 */
export function createCloseButton(banner) {
  const closeButton = document.createElement("button");
  closeButton.type = "button";
  closeButton.className = "config-banner__close";
  closeButton.setAttribute("aria-label", "通知を閉じる");
  closeButton.textContent = "×";
  closeButton.addEventListener("click", () => banner.remove());
  return closeButton;
}

/**
 * 閉じるボタン付きの情報バナーを表示する（CFG-015 など、非致命的な通知）。
 *
 * @param {string} message
 */
export function showInfoBanner(message) {
  const container = ensureBannerContainer();

  const banner = document.createElement("div");
  banner.className = "config-banner config-banner--info";
  banner.setAttribute("role", "status");

  const text = document.createElement("span");
  text.className = "config-banner__text";
  text.textContent = message;
  banner.appendChild(text);

  banner.appendChild(createCloseButton(banner));
  container.appendChild(banner);
}

/**
 * 見出しと一覧を持つ警告バナーを表示する（CFG-016 の安全モードなど）。
 * UI 自体は既に起動済みであり、この表示は操作をブロックしない。
 *
 * @param {string} heading
 * @param {string[]} [items] 一覧表示する、呼び出し側で整形済みの文字列（省略時は一覧を出さない）。
 */
export function showWarningBanner(heading, items = []) {
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

  banner.appendChild(createCloseButton(banner));
  container.appendChild(banner);
}

/**
 * エラーバナーを表示する（致命的ではないが利用者の注意を要する失敗。
 * open_log_file の failed、範囲取得の失敗など）。
 *
 * @param {string} message
 */
export function showErrorBanner(message) {
  const container = ensureBannerContainer();

  const banner = document.createElement("div");
  banner.className = "config-banner config-banner--error";
  banner.setAttribute("role", "alert");

  const text = document.createElement("span");
  text.className = "config-banner__text";
  text.textContent = message;
  banner.appendChild(text);

  banner.appendChild(createCloseButton(banner));
  container.appendChild(banner);
}
