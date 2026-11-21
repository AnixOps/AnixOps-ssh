// Fails when a GitHub workflow installs dependencies in a way that can drift
// from the lockfiles. CI must install exactly what a release installs, or a
// green CI run says nothing about the release (third-party review, item 12):
//
// - npm: only `npm ci`, never `npm install` / `npm i` / `npm update`
//   (they rewrite package-lock.json to fit package.json instead of failing).
// - Cargo: every workflow that builds Rust runs cargo with `--locked` at least
//   once (`cargo metadata --locked` before building), so a stale Cargo.lock
//   fails instead of being rewritten.
//
// Run by `npm test`.
import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const dir = join(root, ".github", "workflows");

// `npm install <pkg>` of a one-off tool is not allowed either: use `npx pkg@x.y.z`.
const npmDrift = /\bnpm\s+(?:install|i|update|up|upgrade)\b/;
const buildsRust = /\bcargo\s+(?:build|test|clippy|check|run)\b|\btauri\s+(?:build|android|ios)\b/;
const lockedCheck = /\bcargo\b[^\n]*--locked/;

export function checkWorkflow(name, text) {
  const problems = [];
  text.split("\n").forEach((line, index) => {
    const code = line.replace(/(^|\s)#.*$/, "");
    if (npmDrift.test(code)) {
      problems.push(`${name}:${index + 1}: use \`npm ci\` (installs package-lock.json as is): ${line.trim()}`);
    }
  });
  const code = text
    .split("\n")
    .map((line) => line.replace(/(^|\s)#.*$/, ""))
    .join("\n");
  if (buildsRust.test(code) && !lockedCheck.test(code)) {
    problems.push(`${name}: builds Rust but never runs \`cargo metadata --locked\` to check Cargo.lock`);
  }
  return problems;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const problems = [];
  for (const entry of (await readdir(dir)).sort()) {
    if (!/\.ya?ml$/.test(entry)) continue;
    problems.push(...checkWorkflow(`.github/workflows/${entry}`, await readFile(join(dir, entry), "utf8")));
  }
  if (problems.length) {
    console.error(problems.join("\n"));
    process.exit(1);
  }
  console.log("workflows install from lockfiles");
}
