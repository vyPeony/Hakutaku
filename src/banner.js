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
//
// # 情報バナーの寿命と固定キーによる上書き（Issue #49 の裁定）
//
// 情報バナー（`showInfoBanner`）には、次の2つの任意指定を追加した。どちらも
// 既定では働かないため、指定しない既存の呼び出し側の挙動は変わらない。
//
//   - `autoDismiss`: 表示から `INFO_AUTO_DISMISS_MS` 後に自動で消す。コピー成功
//     のように「済んだこと」を伝えるだけの通知は、利用者が閉じるまで残ると読み
//     終えた後も画面上部を占有し続け、後から起きた通知や操作領域を押しのける。
//     一方、起動時の設定未検出の案内（`CFG-015`）や次の操作を促す案内
//     （`src/shell.js`）は読み終える前に消えては困るため、既定は「消さない」の
//     ままにし、消えてよい通知だけが明示的に指定する
//   - `key`: 固定の集約キー。**文面が違っても**同じキーの表示中バナー1枚を
//     上書きする（回数表示は付けない）。コピー成功の通知は行数・バイト数が毎回
//     変わるため、Issue #11 の同一性キー（本文を含む）では毎回別内容と判定され、
//     コピーのたびに新しいバナーが積み上がっていた。「最後に成功したコピー」は
//     1枚だけ見えていれば十分なので、キーで1枚に固定する
//
// **警告・エラーのバナーの集約方式（Issue #11 の裁定）は変えない。** 警告と
// エラーは原因が解消したかどうかを利用者が判断するまで残す必要があり、回数表示
// （「（N回目）」）そのものが再発の手掛かりになるためである。

/**
 * 情報バナーを自動で消すまでの時間（ミリ秒。Issue #49）。
 *
 * 短すぎると読み終える前に消え、長すぎると「済んだこと」の通知が画面上部を
 * 占有し続ける。1文の通知を読み切れる長さとして5秒を採る（`autoDismiss` を
 * 指定した情報バナーにだけ適用する）。
 */
const INFO_AUTO_DISMISS_MS = 5_000;

/**
 * 表示中のバナー1枚分の集約状態（Issue #11、Issue #49 で `dismissTimerId` を
 * 追加）。
 *
 * - `banner`: #config-banners 直下のバナー要素
 * - `countTarget`: 「（N回目）」を書き足すテキスト要素（info / error は本文、
 *   warning は見出し）
 * - `baseText`: 回数を付ける前の文。表示のたびにこの文から組み立て直す
 * - `count`: 同じ内容の表示要求を受けた回数（初回の表示が 1）
 * - `dismissTimerId`: 自動消滅のタイマー（Issue #49。`autoDismiss` を指定した
 *   情報バナーだけが持ち、それ以外は常に `null`）
 *
 * @typedef {{
 *   banner: HTMLElement,
 *   countTarget: HTMLElement,
 *   baseText: string,
 *   count: number,
 *   dismissTimerId: ReturnType<typeof setTimeout> | null,
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
 * 呼び出し側が指定した固定の集約キー（Issue #49）から、内部のキーを作る。
 *
 * 先頭に置く `"fixed"` は種別（`"info"` / `"warning"` / `"error"`）のいずれとも
 * 一致しないため、文面から作る同一性キー（[`buildBannerKey`]）と衝突しない。
 *
 * @param {string} key 呼び出し側の固定キー
 */
function buildFixedBannerKey(key) {
  return ["fixed", key].join(KEY_SEPARATOR);
}

/**
 * 自動消滅のタイマーを止める（Issue #49）。既に消えているバナーのタイマーが
 * 後から発火して、同じキーで作り直された別のバナーを消してしまわないよう、
 * バナーを手放す経路すべてから呼ぶ。
 *
 * @param {ActiveBanner} active
 */
function clearBannerDismissTimer(active) {
  if (active.dismissTimerId !== null) {
    clearTimeout(active.dismissTimerId);
    active.dismissTimerId = null;
  }
}

/**
 * バナーを DOM から取り除き、集約状態も破棄する（Issue #49）。閉じるボタンと
 * 自動消滅の共通処理。
 *
 * `banner` を照合するのは、同じキーで既に別のバナーが作り直されている場合に
 * そちらを巻き添えで消さないため（閉じるボタンが元々行っていた照合と同じ理由）。
 *
 * @param {string} key
 * @param {HTMLElement} banner
 */
function removeBanner(key, banner) {
  const active = activeBanners.get(key);
  if (active === undefined || active.banner !== banner) {
    banner.remove();
    return;
  }
  clearBannerDismissTimer(active);
  activeBanners.delete(key);
  banner.remove();
}

/**
 * 自動消滅のタイマーを張り直す（Issue #49）。`autoDismiss` が `false` の場合は
 * 何も予約しない（＝利用者が閉じるまで残る、Issue #11 までの挙動）。
 *
 * 表示要求のたびに張り直すのは、同じ内容が再び通知された時点を起点に数え直す
 * ため（消える直前に届いた2件目が、読む間もなく消えるのを避ける）。
 *
 * @param {string} key
 * @param {ActiveBanner} active
 * @param {boolean} autoDismiss
 */
function scheduleBannerAutoDismiss(key, active, autoDismiss) {
  clearBannerDismissTimer(active);
  if (!autoDismiss) {
    return;
  }
  const banner = active.banner;
  active.dismissTimerId = setTimeout(() => {
    removeBanner(key, banner);
  }, INFO_AUTO_DISMISS_MS);
}

/**
 * 同じ内容のバナーが表示中なら回数表示を更新し、`true` を返す（新しいバナーは
 * 作らない）。表示中でなければ `false` を返し、呼び出し側が新規に組み立てる。
 *
 * @param {string} key
 * @param {boolean} [autoDismiss] 自動消滅のタイマーを張り直すか（Issue #49）。
 */
function countUpExistingBanner(key, autoDismiss = false) {
  const active = activeBanners.get(key);
  if (!active) {
    return false;
  }
  // 閉じるボタン以外の経路で DOM から外れている場合（コンテナごと差し替えられた
  // 場合など）は、見えないバナーの回数を数えても利用者へ伝わらない。状態を捨てて
  // 新規表示へ委ね、切り離した要素への参照もここで手放す。
  if (!active.banner.isConnected) {
    clearBannerDismissTimer(active);
    activeBanners.delete(key);
    return false;
  }
  active.count += 1;
  // 回数だけを別の要素として足すのではなく、テキスト要素の textContent を
  // 丸ごと書き換える。バナーの role=status / role=alert に対する内容の変更として
  // 支援技術へ再通知されることを期待するため（Issue #11 の裁定）。
  active.countTarget.textContent = `${active.baseText}（${active.count}回目）`;
  scheduleBannerAutoDismiss(key, active, autoDismiss);
  return true;
}

/**
 * 固定キー（Issue #49）のバナーが表示中なら、文面を新しいものへ**上書き**して
 * `true` を返す。表示中でなければ `false` を返し、呼び出し側が新規に組み立てる。
 *
 * 回数（「（N回目）」）を付けないのは、固定キーの通知が「同じ内容の再発」では
 * なく「同じ場所に出す最新の結果」だからである（コピー成功の行数・バイト数は
 * 毎回変わる）。DOM 上の位置は動かさない（Issue #11 と同じ理由。読んでいる
 * 最中に並び順を変えない）。
 *
 * @param {string} key
 * @param {string} message
 * @param {boolean} autoDismiss
 */
function overwriteExistingBanner(key, message, autoDismiss) {
  const active = activeBanners.get(key);
  if (!active) {
    return false;
  }
  if (!active.banner.isConnected) {
    clearBannerDismissTimer(active);
    activeBanners.delete(key);
    return false;
  }
  active.baseText = message;
  active.count = 1;
  active.countTarget.textContent = message;
  scheduleBannerAutoDismiss(key, active, autoDismiss);
  return true;
}

/**
 * 新しく表示したバナーを、以後の同一内容の集約対象として登録する。
 *
 * @param {string} key 同一性キー
 * @param {HTMLElement} banner
 * @param {HTMLElement} countTarget 「（N回目）」を書き足すテキスト要素
 * @param {string} baseText 回数を付ける前の文
 * @param {boolean} [autoDismiss] 自動消滅させるか（Issue #49）。
 */
function registerBanner(key, banner, countTarget, baseText, autoDismiss = false) {
  /** @type {ActiveBanner} */
  const active = { banner, countTarget, baseText, count: 1, dismissTimerId: null };
  activeBanners.set(key, active);
  scheduleBannerAutoDismiss(key, active, autoDismiss);
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
    // 同じキーで既に別のバナーが作り直されている場合に、そちらの状態まで
    // 巻き添えで消さないよう、自分の登録だけを取り消す（照合は `removeBanner`
    // が行う。自動消滅のタイマーもそこで止める。Issue #49）。
    if (key === undefined) {
      banner.remove();
      return;
    }
    removeBanner(key, banner);
  });
  return closeButton;
}

/**
 * @typedef {Object} InfoBannerOptions 情報バナーの任意指定（Issue #49）。
 * どちらも省略時は Issue #11 までの挙動（文面による集約、利用者が閉じるまで
 * 残る）と同じになる。
 * @property {string} [key] 固定の集約キー。指定すると、**文面が違っても**同じ
 *   キーの表示中バナー1枚を上書きする（回数表示は付けない）。同じ場所に出す
 *   「最新の結果」が1枚だけ見えていればよい通知（コピー成功など）に使う。
 * @property {boolean} [autoDismiss] `true` なら表示から
 *   `INFO_AUTO_DISMISS_MS` 後に自動で消す。読み終えた後も残す必要がない通知
 *   （処理の完了通知など）にだけ指定する。
 */

/**
 * 閉じるボタン付きの情報バナーを表示する（CFG-015 など、非致命的な通知）。
 *
 * 同じ文の情報バナーが表示中の場合は、新しいバナーを足さずに既存の文末へ
 * 「（N回目）」を付けて更新する（Issue #11）。`options.key` を指定した場合は
 * 文面によらず同じキーのバナー1枚を上書きする（Issue #49。モジュール冒頭の
 * コメント「情報バナーの寿命と固定キーによる上書き」参照）。
 *
 * @param {string} message
 * @param {InfoBannerOptions} [options]
 */
export function showInfoBanner(message, options = {}) {
  const autoDismiss = options.autoDismiss === true;
  const fixedKey = options.key;
  const key =
    fixedKey === undefined
      ? buildBannerKey("info", "", message, [])
      : buildFixedBannerKey(fixedKey);

  // 固定キーは「上書き」、文面によるキーは従来どおり「回数の積み増し」。
  const handled =
    fixedKey === undefined
      ? countUpExistingBanner(key, autoDismiss)
      : overwriteExistingBanner(key, message, autoDismiss);
  if (handled) {
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
  registerBanner(key, banner, text, message, autoDismiss);
}

/**
 * 固定の集約キー（[`InfoBannerOptions`] の `key`）で表示したバナーを、時間切れ
 * や閉じるボタンを待たずに消す（Issue #49）。
 *
 * 進行中を示すバナー（「コピー中…」）のように、**表示した側が終わりを知って
 * いる**通知のために用意する。該当するバナーが無ければ何もしない（完了が
 * 進行表示の開始より早い場合は、そもそも表示していない）。
 *
 * @param {string} key 表示時に指定した固定の集約キー
 */
export function dismissBanner(key) {
  const bannerKey = buildFixedBannerKey(key);
  const active = activeBanners.get(bannerKey);
  if (active === undefined) {
    return;
  }
  removeBanner(bannerKey, active.banner);
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
