// Fails when a literal colour appears in the UI outside src/design.
// AnixOps Design rule: components reference tokens, never literal colours
// (guidelines/adoption.md, "Every product", 1). Run by `npm test`.
import { readdir, readFile } from "node:fs/promises";
import { join, relative } from "node:path";

const root = new URL("..", import.meta.url).pathname;
const skip = new Set(["design", "node_modules"]);
const skipFiles = new Set(["proto.gen.ts"]);
// The brand marks in src/assets are vendored SVGs, like src/design.
const extensions = [".ts", ".tsx", ".css", ".html"];

const literal =
  /#[0-9a-fA-F]{8}\b|#[0-9a-fA-F]{6}\b|#[0-9a-fA-F]{3,4}\b|\b(?:rgba?|hsla?|hwb|lab|lch|oklab|oklch|color)\(\s*[\d.%-]/g;

async function* walk(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    if (skip.has(entry.name)) continue;
    const path = join(dir, entry.name);
    if (entry.isDirectory()) yield* walk(path);
    else if (extensions.some((ext) => entry.name.endsWith(ext)) && !skipFiles.has(entry.name)) yield path;
  }
}

const found = [];
for (const base of ["src", "index.html"]) {
  const target = join(root, base);
  const files = base.endsWith(".html") ? [target] : walk(target);
  for await (const file of files) {
    const lines = (await readFile(file, "utf8")).split("\n");
    lines.forEach((line, index) => {
      for (const match of line.matchAll(literal)) found.push(`${relative(root, file)}:${index + 1}: ${match[0]}`);
    });
  }
}

if (found.length > 0) {
  console.error("Literal colours outside src/design (use AnixOps Design tokens instead):");
  console.error(found.join("\n"));
  process.exit(1);
}
console.log("No literal colours outside src/design.");
