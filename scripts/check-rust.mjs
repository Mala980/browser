#!/usr/bin/env node
// Local Rust syntax checker (see docs/DEVELOPMENT.md).
//
// Why: Kilat is often worked on in sandboxes without `rustc`, where most
// mistakes are plain syntax errors. tree-sitter's Rust grammar flags those in a
// few hundred ms instead of a CI round trip. It is *not* a type checker:
// run `cargo check` for that.
//
// Usage: node scripts/check-rust.mjs [paths...]

import { readdir, readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";

const TSDEPS = process.env.TS_MODULES || "/home/user/tools/tsdeps";

async function loadTreeSitter() {
  try {
    return await import("web-tree-sitter");
  } catch {
    return await import(`${TSDEPS}/web-tree-sitter/tree-sitter.js`);
  }
}

async function collect(dirs) {
  const out = [];
  for (const d of dirs) {
    const st = await readdir(d, { withFileTypes: true }).catch(() => []);
    for (const e of st) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) {
        if (e.name === "target" || e.name === "node_modules" || e.name.startsWith(".")) continue;
        out.push(...(await collect([p])));
      } else if (e.name.endsWith(".rs")) out.push(p);
    }
  }
  return out.sort();
}

function walk(node, cb) {
  cb(node);
  for (const c of node.children) walk(c, cb);
}

const wasm = [
  "node_modules/tree-sitter-wasms/out/tree-sitter-rust.wasm",
  `${TSDEPS}/tree-sitter-rust.wasm`,
].find(existsSync);
if (!wasm) {
  console.error(`tree-sitter-rust.wasm not found (looked in node_modules and ${TSDEPS}); run scripts/setup-dev-tools.sh`);
  process.exit(2);
}

const { Parser, Language } = await loadTreeSitter();
const localCore = existsSync("node_modules/web-tree-sitter/tree-sitter.wasm")
  ? path.resolve("node_modules/web-tree-sitter/tree-sitter.wasm")
  : `${TSDEPS}/web-tree-sitter/tree-sitter.wasm`;
await Parser.init({ locateFile: () => localCore });
const parser = new Parser();
parser.setLanguage(await Language.load(path.resolve(wasm)));

const roots = process.argv.slice(2).filter((a) => existsSync(a));
const files = await collect(roots.length ? roots : ["src", "tests", "build.rs"]);
let bad = 0;
for (const f of files) {
  const src = await readFile(f, "utf8");
  const tree = parser.parse(src);
  const problems = [];
  walk(tree.rootNode, (n) => {
    if (n.type === "ERROR" || (n.isMissing && n.isMissing)) {
      const text = (n.text || "").replace(/\s+/g, " ").slice(0, 70);
      problems.push(`  ${f}:${n.startPosition.row + 1}:${n.startPosition.column + 1} ${n.type === "ERROR" ? "ERROR" : "MISSING"} near \`${text}\``);
    }
  });
  if (problems.length) {
    bad++;
    console.log(problems.slice(0, 6).join("\n"));
    if (problems.length > 6) console.log(`  ... ${problems.length - 6} more in ${f}`);
  }
}
console.log(`${files.length - bad}/${files.length} rust files parse cleanly`);
process.exit(bad ? 1 : 0);
