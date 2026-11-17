import type { HistoryStore } from "./history";
import { rank } from "./rank";
import { completePaths, wordInsert, type PathLister } from "./remotePaths";
import { analyze, type Quote } from "./tokenizer";
import { type Candidate, type CommandSpec, type LineContext, type Template } from "./types";
import { walk } from "./walker";

export interface EngineDeps {
  loadSpec(command: string): Promise<CommandSpec | null>;
  commandNames(): { name: string; description: string }[];
  history: HistoryStore;
  /** Lists remote directories; absent for local terminals. */
  paths?: PathLister;
  hostNames?: () => Promise<string[]>;
}

export interface Suggestions {
  items: Candidate[];
}

const NONE: Suggestions = { items: [] };
const MAX_ITEMS = 40;
const MAX_WRAP_DEPTH = 5;

interface Resolved {
  candidates: Candidate[];
  templates: Template[];
  valuePrefix: string;
  partial: string;
  /** The command whose arguments are being completed (for history). */
  command: string | null;
}

async function resolve(words: string[], current: string, deps: EngineDeps, depth = 0): Promise<Resolved> {
  if (words.length === 0) {
    return { candidates: [], templates: ["command"], valuePrefix: "", partial: current, command: null };
  }
  const empty = { candidates: [], templates: [], valuePrefix: "", partial: current, command: words[0] } satisfies Resolved;
  const spec = await deps.loadSpec(words[0]);
  if (!spec) return empty;
  const result = walk(spec, words.slice(1), current);
  if (result.wrap && depth < MAX_WRAP_DEPTH) return resolve(result.wrap, current, deps, depth + 1);
  if (result.wrap) return empty;
  return { ...result, command: words[0] };
}

function startsPath(text: string): boolean {
  return text.startsWith("/") || text.startsWith("~") || text.startsWith("./") || text.startsWith("../");
}

/** Suggestions for the end of `context.line`. Paths may resolve to nothing if superseded. */
export async function suggest(context: LineContext, deps: EngineDeps): Promise<Suggestions> {
  const analysis = analyze(context.line);
  const { current } = analysis;
  if (analysis.comment || analysis.assignment) return NONE;
  if (context.line.trim() === "") return NONE;

  let resolved: Resolved;
  if (analysis.afterRedirect) {
    resolved = { candidates: [], templates: ["filepaths"], valuePrefix: "", partial: current.text, command: null };
  } else {
    resolved = await resolve(analysis.words, current.text, deps);
  }
  const quote: Quote = current.quote;
  const withPrefix = (candidate: Candidate): Candidate => ({
    ...candidate,
    insert: resolved.valuePrefix + candidate.insert,
    replaceFrom: current.start,
  });
  const items: Candidate[] = [];

  // Spec candidates: words are quoted for the shell, options and subcommands are plain.
  const fromSpec = resolved.candidates.map((candidate) =>
    candidate.kind === "value"
      ? { ...candidate, insert: wordInsert(candidate.insert, quote, !candidate.keepOpen) }
      : candidate,
  );
  items.push(...rank(fromSpec, resolved.partial).map(withPrefix));

  const templates = new Set(resolved.templates);
  if (templates.has("command") && !startsPath(resolved.partial)) {
    const names: Candidate[] = deps.commandNames().map((entry) => ({
      label: entry.name,
      insert: entry.name,
      kind: "command",
      description: entry.description,
    }));
    const seen = new Set(names.map((name) => name.label));
    for (const line of deps.history.recent(context.hostId ?? "local")) {
      const first = line.split(/\s+/, 1)[0];
      if (first && !seen.has(first) && /^[\w.+-]+$/.test(first)) {
        seen.add(first);
        names.push({ label: first, insert: first, kind: "command" });
      }
    }
    // Commands with a spec first, then ones seen in history.
    const typed = rank(names.filter((name) => !name.description), resolved.partial, 10);
    const known = rank(names.filter((name) => name.description), resolved.partial, 30);
    items.push(...known.concat(typed).map(withPrefix));
  } else if (templates.has("command")) {
    templates.add("filepaths");
  }

  if (templates.has("hosts") && deps.hostNames) {
    const hosts = (await deps.hostNames()).map<Candidate>((name) => ({ label: name, insert: wordInsert(name, quote, true), kind: "host" }));
    items.push(...rank(hosts, resolved.partial, 20).map(withPrefix));
  }

  if (templates.has("history") && resolved.command) {
    const tokens = new Map<string, Candidate>();
    for (const line of deps.history.recent(context.hostId ?? "local")) {
      const parts = line.split(/\s+/);
      if (parts[0] !== resolved.command) continue;
      for (const part of parts.slice(1)) {
        if (part && !part.startsWith("-") && !tokens.has(part)) {
          tokens.set(part, { label: part, insert: wordInsert(part, quote, true), kind: "history" });
        }
      }
    }
    items.push(...rank([...tokens.values()], resolved.partial, 10).map(withPrefix));
  }

  const wantsPaths = templates.has("filepaths") || templates.has("folders") || templates.has("remote-paths");
  if (wantsPaths && context.hostId && deps.paths) {
    const foldersOnly = !templates.has("filepaths") && !templates.has("remote-paths");
    const paths = await completePaths(deps.paths, { text: resolved.partial, quote, foldersOnly, cwd: context.cwd });
    if (paths === null) return NONE;
    items.push(...paths.map(withPrefix));
  }

  // Earlier lines that continue what is typed in this command.
  const segment = context.line.slice(analysis.segmentStart);
  if (!analysis.afterRedirect && segment.trim() !== "" && segment === segment.trimStart()) {
    let count = 0;
    for (const line of deps.history.recent(context.hostId ?? "local")) {
      if (line.length > segment.length && line.startsWith(segment)) {
        items.push({ label: line, insert: line, kind: "history", keepOpen: true, replaceFrom: analysis.segmentStart });
        count += 1;
        if (count === 3) break;
      }
    }
  }

  const seen = new Set<string>();
  const unique = items.filter((item) => {
    const key = `${item.replaceFrom}:${item.insert}`;
    if (seen.has(key) || item.insert === current.raw) return false;
    seen.add(key);
    return true;
  });
  return { items: unique.slice(0, MAX_ITEMS) };
}

/**
 * The keystrokes that turn `line` into one that contains `candidate`: backspaces over
 * what differs, then the new text, with a trailing space unless the item stays open.
 */
export function applyCandidate(line: string, candidate: Candidate): string {
  const from = candidate.replaceFrom ?? line.length;
  const typed = [...line.slice(from)];
  const text = [...(candidate.keepOpen ? candidate.insert : `${candidate.insert} `)];
  let common = 0;
  while (common < typed.length && common < text.length && typed[common] === text[common]) common += 1;
  return "\x7f".repeat(typed.length - common) + text.slice(common).join("");
}

