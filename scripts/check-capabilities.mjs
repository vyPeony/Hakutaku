// 登録コマンドと Capability（permission）の整合検査（Issue #52）。
//
// Tauri のコマンドは2箇所に書く必要がある。
//
//   1. `src-tauri/src/lib.rs` の `tauri::generate_handler![...]`（IPC の受け口）
//   2. `src-tauri/permissions/<コマンド名>.toml` の許可定義と、それを
//      `src-tauri/capabilities/default.toml` の `permissions` へ列挙すること
//
// `SEC-012` により、Hakutaku は `core:default` を使わず「必要になった時点で
// そのコマンドだけを許可する」方針を採っている（`capabilities/default.toml` の
// 冒頭コメント）。この方針では、片方だけを書いた状態が2種類とも起こり得る。
//
//   - 登録したが許可していない → 実行時に「コマンドが許可されていません」で
//     失敗する。ビルドは通り、その画面を操作するまで気付けない
//   - 許可したが登録していない、または使わなくなったコマンドの登録・許可が
//     残っている → 攻撃面が増えたまま誰も気付かない（最小権限の破れ）
//
// どちらも `cargo build` / `cargo clippy` / `npm run tauri -- build` では
// 検出できないため、ここで突き合わせる。判定に含めるのは静的な対応関係だけで、
// 実行時の挙動は扱わない（`docs/verification/regression-checks.md`）。
//
// 解析は正規表現ベースだが、**形式が変わったときに「検出できなかった」ので
// はなく「エラーで落ちる」ように書く**。見つからない・複数ある・想定外の
// 記法がある場合はすべて問題として報告し、終了コード 1 で終わる。黙って0件と
// 比較して成功すると、検査が何も守らなくなるため。
//
// 使い方: node scripts/check-capabilities.mjs

import { readFileSync, readdirSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";

const ROOT = resolve(import.meta.dirname, "..");
const LIB_RS = join(ROOT, "src-tauri", "src", "lib.rs");
const PERMISSIONS_DIR = join(ROOT, "src-tauri", "permissions");
const CAPABILITY_TOML = join(ROOT, "src-tauri", "capabilities", "default.toml");
const FRONTEND_DIR = join(ROOT, "src");

const problems = [];
let checkCount = 0;

const rel = (path) => relative(ROOT, path).split(sep).join("/");

function format(value) {
  return Array.isArray(value) ? value.join(", ") : String(value);
}

function check(name, ok, detail) {
  checkCount += 1;
  if (!ok) {
    problems.push(detail ? `${name}\n    ${detail}` : name);
  }
}

/**
 * 解析そのものが成立しなかった場合（想定した記法が見つからない等）。
 * 突き合わせる材料が無い状態で「差分なし」と報告しないため、ここで打ち切る。
 */
function fail(message) {
  console.error(`登録コマンドと permission の整合検査を実行できません。\n\n  ${message}`);
  console.error(
    "\n記法を変えた場合は scripts/check-capabilities.mjs の解析部分も更新してください。",
  );
  process.exit(1);
}

/** TOML のコメント行（行頭が `#`）を取り除く。値の解析を単純に保つため。 */
function stripTomlComments(text) {
  return text
    .split(/\r?\n/)
    .filter((line) => !/^\s*#/.test(line))
    .join("\n");
}

// ---------------------------------------------------------------------------
// 1. 登録コマンド（`src-tauri/src/lib.rs` の `tauri::generate_handler!`）
// ---------------------------------------------------------------------------

/** @returns {string[]} 登録順のコマンド名（モジュールパスは落とす）。 */
function readRegisteredCommands() {
  const source = readFileSync(LIB_RS, "utf8");

  if (!source.includes(".invoke_handler(tauri::generate_handler![")) {
    fail(
      `${rel(LIB_RS)} に \`.invoke_handler(tauri::generate_handler![\` が見つかりません（記法が変わった可能性があります）。`,
    );
  }

  const matches = [...source.matchAll(/tauri::generate_handler!\[([\s\S]*?)\]/g)];
  if (matches.length !== 1) {
    fail(
      `${rel(LIB_RS)} の \`tauri::generate_handler![...]\` が ${matches.length} 件見つかりました（1件だけを想定しています）。`,
    );
  }

  const body = matches[0][1]
    .split(/\r?\n/)
    .map((line) => line.replace(/\/\/.*$/, "").trim())
    .join(" ");

  const entries = body
    .split(",")
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);

  if (entries.length === 0) {
    fail(`${rel(LIB_RS)} の \`tauri::generate_handler![...]\` にコマンドが1件もありません。`);
  }

  const commands = [];
  for (const entry of entries) {
    if (!/^[A-Za-z_][A-Za-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)*$/.test(entry)) {
      fail(
        `${rel(LIB_RS)} の登録コマンドとして解釈できない記述があります: ${entry}（記法が変わった可能性があります）。`,
      );
    }
    commands.push(entry.split("::").pop());
  }
  return commands;
}

// ---------------------------------------------------------------------------
// 2. 許可定義（`src-tauri/permissions/*.toml`）
// ---------------------------------------------------------------------------

/**
 * @returns {{ file: string, identifier: string, commands: string[] }[]}
 *   ファイル名昇順の許可定義。
 */
function readPermissionDefinitions() {
  let files;
  try {
    files = readdirSync(PERMISSIONS_DIR)
      .filter((name) => name.endsWith(".toml"))
      .sort();
  } catch (error) {
    fail(`${rel(PERMISSIONS_DIR)} を読み取れません: ${error.message}`);
  }

  if (files.length === 0) {
    fail(`${rel(PERMISSIONS_DIR)} に許可定義（*.toml）が1件もありません。`);
  }

  return files.map((name) => {
    const path = join(PERMISSIONS_DIR, name);
    const text = stripTomlComments(readFileSync(path, "utf8"));

    const headers = [...text.matchAll(/^\s*\[\[permission\]\]\s*$/gm)];
    if (headers.length !== 1) {
      fail(
        `${rel(path)} の \`[[permission]]\` が ${headers.length} 件です（1件だけを想定しています）。`,
      );
    }

    const identifiers = [...text.matchAll(/^\s*identifier\s*=\s*"([^"]+)"\s*$/gm)];
    if (identifiers.length !== 1) {
      fail(`${rel(path)} の \`identifier\` が ${identifiers.length} 件です（1件だけを想定しています）。`);
    }

    const allows = [...text.matchAll(/^\s*commands\.allow\s*=\s*\[([^\]]*)\]\s*$/gm)];
    if (allows.length !== 1) {
      fail(
        `${rel(path)} の \`commands.allow\` が ${allows.length} 件です（1件だけを想定しています）。`,
      );
    }
    if (/^\s*commands\.deny\s*=/m.test(text)) {
      fail(
        `${rel(path)} に \`commands.deny\` があります。拒否リストは想定していないため、検査の方針を見直してください。`,
      );
    }

    const commands = [...allows[0][1].matchAll(/"([^"]+)"/g)].map((match) => match[1]);
    if (commands.length === 0) {
      fail(`${rel(path)} の \`commands.allow\` が空です。`);
    }

    return { file: name, identifier: identifiers[0][1], commands };
  });
}

// ---------------------------------------------------------------------------
// 3. Capability（`src-tauri/capabilities/default.toml` の `permissions`）
// ---------------------------------------------------------------------------

/** @returns {string[]} 列挙順の permission 識別子。 */
function readCapabilityPermissions() {
  const text = stripTomlComments(readFileSync(CAPABILITY_TOML, "utf8"));

  const matches = [...text.matchAll(/^\s*permissions\s*=\s*\[([\s\S]*?)\]/gm)];
  if (matches.length !== 1) {
    fail(
      `${rel(CAPABILITY_TOML)} の \`permissions = [...]\` が ${matches.length} 件です（1件だけを想定しています）。`,
    );
  }

  return [...matches[0][1].matchAll(/"([^"]+)"/g)].map((match) => match[1]);
}

// ---------------------------------------------------------------------------
// 4. フロントエンドが呼ぶコマンド（`src/**/*.js` の `invoke("...")`）
// ---------------------------------------------------------------------------

function collectJsFiles(dir, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) collectJsFiles(path, out);
    else if (entry.name.endsWith(".js")) out.push(path);
  }
  return out;
}

/** @returns {Map<string, string[]>} コマンド名 -> 呼び出し元ファイル（リポジトリ相対）。 */
function readInvokedCommands() {
  /** @type {Map<string, string[]>} */
  const invoked = new Map();
  for (const file of collectJsFiles(FRONTEND_DIR)) {
    const text = readFileSync(file, "utf8");
    for (const match of text.matchAll(/invoke\(\s*"([A-Za-z_][A-Za-z0-9_]*)"/g)) {
      const callers = invoked.get(match[1]) ?? [];
      callers.push(rel(file));
      invoked.set(match[1], callers);
    }
  }
  return invoked;
}

// ---------------------------------------------------------------------------
// 突き合わせ
// ---------------------------------------------------------------------------

const registeredCommands = readRegisteredCommands();
const definitions = readPermissionDefinitions();
const capabilityPermissions = readCapabilityPermissions();
const invokedCommands = readInvokedCommands();

const registeredSet = new Set(registeredCommands);
const definitionByIdentifier = new Map(definitions.map((entry) => [entry.identifier, entry]));
const capabilitySet = new Set(capabilityPermissions);

check(
  "登録コマンドに重複が無い",
  registeredSet.size === registeredCommands.length,
  `登録 ${registeredCommands.length} 件 / 一意 ${registeredSet.size} 件`,
);
check(
  "許可定義の識別子に重複が無い",
  definitionByIdentifier.size === definitions.length,
  `定義 ${definitions.length} 件 / 一意 ${definitionByIdentifier.size} 件`,
);
check(
  "Capability の permission 列挙に重複が無い",
  capabilitySet.size === capabilityPermissions.length,
  `列挙 ${capabilityPermissions.length} 件 / 一意 ${capabilitySet.size} 件`,
);

// --- 許可定義の形（SEC-012 の「コマンドごとに専用の最小権限」） ---
//
// 1つの permission が複数のコマンドを束ねると、そのうち1つだけが必要な場面でも
// 全部が許可される。`capabilities/default.toml` の方針コメント（「コマンドごとに
// 専用の最小権限だけを追加する」）に合わせ、1定義=1コマンドを保つ。
for (const definition of definitions) {
  const path = `src-tauri/permissions/${definition.file}`;
  check(
    `許可定義が1コマンドだけを許可する: ${definition.file}`,
    definition.commands.length === 1,
    `許可コマンド: ${format(definition.commands)}`,
  );
  const command = definition.commands[0];
  if (command === undefined) continue;

  // 識別子とファイル名の規約（`allow-<コマンド名のハイフン表記>`）。規約から
  // 外れると、default.toml の列挙とコマンドの対応が目視で追えなくなる。
  const expectedIdentifier = `allow-${command.replace(/_/g, "-")}`;
  check(
    `許可定義の識別子が規約どおり: ${definition.file}`,
    definition.identifier === expectedIdentifier,
    `期待 ${expectedIdentifier} / 実際 ${definition.identifier}（コマンド ${command}）`,
  );
  check(
    `許可定義のファイル名が識別子と対応する: ${definition.file}`,
    definition.file === `${definition.identifier.replace(/^allow-/, "")}.toml`,
    `識別子 ${definition.identifier} に対するファイル名は ${definition.identifier.replace(/^allow-/, "")}.toml を想定しています（${path}）`,
  );
}

// --- 登録コマンド ↔ 許可コマンド ---
const allowedCommands = new Map();
for (const definition of definitions) {
  for (const command of definition.commands) {
    const owners = allowedCommands.get(command) ?? [];
    owners.push(definition.identifier);
    allowedCommands.set(command, owners);
  }
}

const missingPermission = registeredCommands.filter((command) => !allowedCommands.has(command));
check(
  "登録したコマンドに permission がある",
  missingPermission.length === 0,
  `permission が無い登録コマンド: ${format(missingPermission)}（src-tauri/permissions/<コマンド名>.toml を追加するか、登録を外してください）`,
);

const unregisteredAllowed = [...allowedCommands.keys()].filter(
  (command) => !registeredSet.has(command),
);
check(
  "permission があるコマンドは登録されている",
  unregisteredAllowed.length === 0,
  `登録されていない許可コマンド: ${format(unregisteredAllowed)}（許可定義を削除するか、コマンドを登録してください）`,
);

const duplicatedAllow = [...allowedCommands.entries()].filter(([, owners]) => owners.length > 1);
check(
  "1つのコマンドを複数の permission が許可していない",
  duplicatedAllow.length === 0,
  duplicatedAllow.map(([command, owners]) => `${command}: ${format(owners)}`).join(" / "),
);

// --- 許可定義 ↔ Capability の列挙 ---
const notListed = definitions
  .map((definition) => definition.identifier)
  .filter((identifier) => !capabilitySet.has(identifier));
check(
  "許可定義がすべて Capability に列挙されている",
  notListed.length === 0,
  `列挙されていない識別子: ${format(notListed)}（定義だけでは許可されません。src-tauri/capabilities/default.toml へ追加してください）`,
);

const undefinedPermissions = capabilityPermissions.filter(
  (identifier) => !definitionByIdentifier.has(identifier),
);
check(
  "Capability が列挙する permission がすべて定義されている",
  undefinedPermissions.length === 0,
  `定義が無い識別子: ${format(undefinedPermissions)}（src-tauri/permissions/ に対応する .toml がありません）`,
);

// --- フロントエンドの呼び出し ↔ 登録コマンド ---
//
// 未登録・未許可のコマンドを呼ぶ経路は、その画面を操作したときに初めて失敗する。
// 呼び出し側から見た整合もここで押さえる。
const unknownInvocations = [...invokedCommands.entries()].filter(
  ([command]) => !registeredSet.has(command),
);
check(
  "フロントエンドが呼ぶコマンドがすべて登録されている",
  unknownInvocations.length === 0,
  unknownInvocations.map(([command, callers]) => `${command}（${format(callers)}）`).join(" / "),
);

const unallowedInvocations = [...invokedCommands.keys()].filter(
  (command) => !allowedCommands.has(command),
);
check(
  "フロントエンドが呼ぶコマンドがすべて許可されている",
  unallowedInvocations.length === 0,
  `permission が無い呼び出し: ${format(unallowedInvocations)}`,
);

// ---------------------------------------------------------------------------
// 結果
// ---------------------------------------------------------------------------

if (problems.length > 0) {
  console.error(`登録コマンドと permission の不整合が ${problems.length} 件あります。\n`);
  for (const problem of problems) console.error(`  ${problem}`);
  console.error(
    "\n最小権限の方針は src-tauri/capabilities/default.toml の冒頭コメント（SEC-012）を参照してください。",
  );
  process.exit(1);
}

console.log(
  `登録コマンド ${registeredCommands.length} 件、許可定義 ${definitions.length} 件、` +
    `Capability の列挙 ${capabilityPermissions.length} 件、フロントエンドの呼び出し ${invokedCommands.size} 種類を ` +
    `${checkCount} 項目突き合わせました。問題はありません。`,
);
