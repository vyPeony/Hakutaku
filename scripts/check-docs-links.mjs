// Markdown の相対リンクと見出しアンカーの整合を検査する。
//
// この文書群は正本関係を相互リンクで表現しており（AGENTS.md、docs/README.md）、
// 見出しの改名やファイル移動でリンクが静かに壊れる。外部依存を増やさないため、
// Node の標準機能だけで実装する。外部 URL は検査しない（ネットワークへ出ない）。
//
// 使い方: node scripts/check-docs-links.mjs

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, dirname, resolve, relative, sep } from "node:path";

const ROOT = resolve(import.meta.dirname, "..");
const SKIP_DIRS = new Set([".git", "node_modules", "target", "runtime", "dist"]);

/** 検査対象の Markdown を集める。 */
function collectMarkdown(dir, out = []) {
  for (const entry of readdirSync(dir)) {
    if (SKIP_DIRS.has(entry)) continue;
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) collectMarkdown(path, out);
    else if (entry.endsWith(".md")) out.push(path);
  }
  return out;
}

/**
 * 囲みコードブロックを空白へ置き換える。コード例の中の `#` を見出しと
 * 誤認しないようにする。行番号を保つため、改行は残す。
 */
function stripFences(text) {
  return text.replace(/^```[\s\S]*?^```/gm, (m) => m.replace(/[^\n]/g, " "));
}

/**
 * 囲みコードブロックに加えてインラインコードも空白へ置き換える。
 * コード例に含まれる角括弧をリンクとして誤検出しないようにする。
 *
 * 見出しの抽出にはこちらを使わない。GitHub は `## 1.1 \`cargo build\`` の
 * ようなコード入りの見出しからもアンカーを作るため、インラインコードを
 * 消すと実在するアンカーを見落とす。
 */
function stripCode(text) {
  return stripFences(text).replace(/`[^`\n]*`/g, (m) => " ".repeat(m.length));
}

/**
 * 見出しから GitHub のアンカーを作る。
 * 小文字化し、文字・数字・記号の一部以外を除き、空白をハイフンにする。
 * 日本語の見出しをそのままアンカーに使うため、Unicode の文字種で判定する。
 */
function toAnchor(heading) {
  return heading
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1") // リンクは表示文字だけ残す
    // `_` は除かない。この文書群では `scale_verify` のような識別子として使われ、
    // GitHub も語中のアンダースコアはアンカーに残す。強調は `**` を使う。
    .replace(/[`*~]/g, "")
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\p{M}_ -]/gu, "")
    .replace(/ /g, "-");
}

/** ファイル内の見出しから、参照可能なアンカーの集合を作る。 */
function anchorsOf(path) {
  const text = stripFences(readFileSync(path, "utf8"));
  const anchors = new Set();
  const seen = new Map();
  for (const line of text.split("\n")) {
    const m = /^#{1,6}\s+(.*?)\s*$/.exec(line);
    if (!m) continue;
    const base = toAnchor(m[1]);
    if (!base) continue;
    // 同じ見出しが複数ある場合、GitHub は -1, -2 を付ける
    const count = seen.get(base) ?? 0;
    seen.set(base, count + 1);
    anchors.add(count === 0 ? base : `${base}-${count}`);
  }
  return anchors;
}

const anchorCache = new Map();
function cachedAnchors(path) {
  if (!anchorCache.has(path)) anchorCache.set(path, anchorsOf(path));
  return anchorCache.get(path);
}

const files = collectMarkdown(ROOT);
const problems = [];
let linkCount = 0;

for (const file of files) {
  const lines = stripCode(readFileSync(file, "utf8")).split("\n");
  lines.forEach((line, index) => {
    // 画像 `![]()` も含めて相対参照を拾う
    for (const m of line.matchAll(/\[[^\]]*\]\(([^)\s]+)\)/g)) {
      const target = m[1];
      if (/^(https?:|mailto:|#|<)/.test(target)) {
        // 同一ファイル内のアンカーだけはここで検査する
        if (target.startsWith("#")) {
          linkCount++;
          const anchor = decodeURIComponent(target.slice(1));
          if (!cachedAnchors(file).has(anchor)) {
            problems.push({ file, line: index + 1, target, reason: "同一ファイル内に見出しがない" });
          }
        }
        continue;
      }
      linkCount++;

      const [rawPath, rawAnchor] = target.split("#");
      const resolved = resolve(dirname(file), decodeURIComponent(rawPath));

      let stats;
      try {
        stats = statSync(resolved);
      } catch {
        problems.push({ file, line: index + 1, target, reason: "参照先が存在しない" });
        continue;
      }

      if (rawAnchor === undefined) continue;
      if (!stats.isFile() || !resolved.endsWith(".md")) {
        problems.push({ file, line: index + 1, target, reason: "Markdown 以外にアンカーを指定している" });
        continue;
      }
      const anchor = decodeURIComponent(rawAnchor);
      if (!cachedAnchors(resolved).has(anchor)) {
        problems.push({ file, line: index + 1, target, reason: "参照先に見出しがない" });
      }
    }
  });
}

const rel = (p) => relative(ROOT, p).split(sep).join("/");

if (problems.length > 0) {
  console.error(`リンクの問題が ${problems.length} 件あります。\n`);
  for (const p of problems) {
    console.error(`${rel(p.file)}:${p.line}: ${p.reason}`);
    console.error(`  -> ${p.target}`);
  }
  process.exit(1);
}

console.log(`Markdown ${files.length} ファイル、リンク ${linkCount} 件を検査しました。問題はありません。`);
