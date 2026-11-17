// Validates every completion spec against the schema, checks for duplicate
// names and keeps completion/index.json (command name -> file) up to date.
//
//   npm run check:specs              check only (CI)
//   npm run check:specs -- --write   also rewrite completion/index.json
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import Ajv from "ajv";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "completion");
const specsDir = join(root, "specs");
const indexPath = join(root, "index.json");
const write = process.argv.includes("--write");

const schema = JSON.parse(readFileSync(join(root, "schema", "spec.schema.json"), "utf8"));
const validate = new Ajv({ allErrors: true, strict: true }).compile(schema);
const names = (name) => (Array.isArray(name) ? name : [name]);

const errors = [];
const fail = (where, message) => errors.push(`${where}: ${message}`);

/** Reports names that appear twice in a list. */
function duplicates(where, what, list) {
  const seen = new Set();
  for (const name of list) {
    if (seen.has(name)) fail(where, `duplicate ${what} "${name}"`);
    seen.add(name);
  }
}

function checkNode(where, node, inheritedOptions) {
  const own = (node.options ?? []).flatMap((option) => names(option.name));
  duplicates(where, "option", own);
  const visible = new Set([...own, ...inheritedOptions]);
  for (const option of node.options ?? []) {
    for (const other of option.exclusiveWith ?? []) {
      if (!visible.has(other)) fail(where, `option ${names(option.name)[0]} is exclusive with unknown option "${other}"`);
    }
    for (const name of option.exclusiveWith ?? []) {
      if (names(option.name).includes(name)) fail(where, `option ${name} is exclusive with itself`);
    }
  }
  duplicates(where, "subcommand", (node.subcommands ?? []).flatMap((sub) => names(sub.name)));
  const persistent = (node.options ?? []).filter((option) => option.persistent).flatMap((option) => names(option.name));
  for (const sub of node.subcommands ?? []) {
    checkNode(`${where} ${names(sub.name)[0]}`, sub, [...inheritedOptions, ...persistent]);
  }
  const args = node.args ?? [];
  args.forEach((arg, index) => {
    if (arg.variadic && index !== args.length - 1) fail(where, `variadic argument "${arg.name ?? index}" must be last`);
  });
}

const files = readdirSync(specsDir).filter((file) => file.endsWith(".json")).sort();
const index = {};
for (const file of files) {
  const where = `completion/specs/${file}`;
  let spec;
  try {
    spec = JSON.parse(readFileSync(join(specsDir, file), "utf8"));
  } catch (error) {
    fail(where, `invalid JSON (${error.message})`);
    continue;
  }
  if (!validate(spec)) {
    for (const error of validate.errors) fail(where, `${error.instancePath || "/"} ${error.message}`);
    continue;
  }
  const primary = names(spec.name)[0];
  if (file !== `${primary}.json`) fail(where, `the file must be named after the first name: ${primary}.json`);
  duplicates(where, "name", names(spec.name));
  for (const name of names(spec.name)) {
    if (Object.hasOwn(index, name)) fail(where, `command name "${name}" is already used by ${index[name].file}.json`);
    else index[name] = { file: primary, description: spec.description };
  }
  checkNode(primary, spec, []);
}

const sorted = Object.fromEntries(Object.entries(index).sort(([a], [b]) => a.localeCompare(b)));
const text = `${JSON.stringify(sorted, null, 2)}\n`;
if (write) {
  writeFileSync(indexPath, text);
} else {
  let current = "";
  try {
    current = readFileSync(indexPath, "utf8");
  } catch {
    // Reported below.
  }
  if (current !== text) fail("completion/index.json", "is out of date; run `npm run check:specs -- --write`");
}

if (errors.length > 0) {
  console.error(errors.map((error) => `  ${error}`).join("\n"));
  console.error(`\n${errors.length} problem(s) in the completion specs.`);
  process.exit(1);
}
console.log(`${files.length} completion specs are valid (${Object.keys(index).length} command names).`);
