// フロントエンド資産（src/）の静的検査。
//
// Hakutaku は bundler を使わず、src/ をそのまま release バイナリへ埋め込む
// （Tauri.toml の frontendDist）。そのため `tauri build` は資産の中身を検証せず、
// 構文エラーや参照切れは WebView 上で初めて表面化する。ここではその隙間を、
// 外部依存を増やさずに埋める。
//
// 検査する内容:
//   1. 各 .js の構文（`node --check`）
//   2. 相対 import の解決（改名・移動によるモジュール参照切れ）
//   3. index.html が参照する相対資産の存在（script / link / img）
//
// 使い方: node scripts/check-frontend.mjs

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";

const ROOT = resolve(import.meta.dirname, "..");
const SRC = join(ROOT, "src");

const problems = [];
const rel = (p) => relative(ROOT, p).split(sep).join("/");

/** src/ 配下の .js を再帰的に集める。 */
function collectJs(dir, out = []) {
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) collectJs(path, out);
    else if (entry.endsWith(".js")) out.push(path);
  }
  return out;
}

const jsFiles = collectJs(SRC);

// 1. 構文検査。ルートの package.json が "type": "module" のため、
//    `node --check` は ES モジュールとして解析する。
for (const file of jsFiles) {
  try {
    execFileSync(process.execPath, ["--check", file], { stdio: "pipe" });
  } catch (error) {
    const detail = String(error.stderr ?? error.message).trim();
    problems.push(`${rel(file)}: 構文エラー\n${detail.replace(/^/gm, "  ")}`);
  }
}

// 2. 相対 import の解決。外部パッケージは使っていないため、相対指定でないものは
//    それ自体を問題として報告する（依存を増やす変更に気付けるようにする）。
const IMPORT_PATTERN = /(?:^|\n)\s*(?:import|export)[\s\S]*?\sfrom\s+["']([^"']+)["']/g;
const BARE_IMPORT_PATTERN = /(?:^|\n)\s*import\s+["']([^"']+)["']/g;

for (const file of jsFiles) {
  const text = readFileSync(file, "utf8");
  for (const pattern of [IMPORT_PATTERN, BARE_IMPORT_PATTERN]) {
    for (const match of text.matchAll(pattern)) {
      const spec = match[1];
      if (!spec.startsWith(".") && !spec.startsWith("/")) {
        problems.push(`${rel(file)}: 相対指定でない import があります: ${spec}`);
        continue;
      }
      if (!existsSync(resolve(dirname(file), spec))) {
        problems.push(`${rel(file)}: import の参照先がありません: ${spec}`);
      }
    }
  }
}

// 3. index.html が参照する相対資産。`href`/`src` のうち、URL でもデータ URI でも
//    ないものを対象にする。
const html = join(SRC, "index.html");
if (existsSync(html)) {
  const text = readFileSync(html, "utf8");
  for (const match of text.matchAll(/\b(?:src|href)\s*=\s*"([^"]+)"/g)) {
    const target = match[1];
    if (/^(https?:|data:|mailto:|#|\/\/)/.test(target)) continue;
    const path = resolve(dirname(html), target.split(/[?#]/)[0]);
    if (!existsSync(path)) {
      problems.push(`${rel(html)}: 参照先がありません: ${target}`);
    }
  }
}

if (problems.length > 0) {
  console.error(`フロントエンドの問題が ${problems.length} 件あります。\n`);
  for (const problem of problems) console.error(problem);
  process.exit(1);
}

console.log(`JavaScript ${jsFiles.length} ファイルの構文、import の解決、index.html の参照を検査しました。問題はありません。`);
