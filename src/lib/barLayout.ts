import { FUNCTION_KEYS, type BarKey } from "./mobileKeys";

/* The customisable key bar. Two rows: the primary row sits above the keyboard
   always; the extra row swipes sideways and holds what is needed less often
   (navigation keys, symbols, F1-F12, snippet keys). An item is a string id:
   a built-in key ("esc", "ctrl", "f5", ...), a symbol ("char:|") or a snippet
   key ("snippet:<id>") the person made. The layout is plain data, saved with
   the mobile preferences. */

export type BarRow = "primary" | "extra";

export interface SnippetKey {
  id: string;
  /** What the key shows (a few characters). */
  label: string;
  /** What it types. */
  text: string;
  /** Press Enter after the text. */
  run: boolean;
}

export interface BarLayout {
  primary: string[];
  extra: string[];
  snippets: SnippetKey[];
}

export const MAX_ROW_ITEMS = 32;
export const MAX_SNIPPETS = 24;
export const MAX_LABEL = 8;
export const MAX_SNIPPET_TEXT = 240;

/** Symbols a soft keyboard hides on its second page. */
export const SYMBOLS = ["~", "|", "/", "-", "_", "\\", ":", "$", "`", "'", '"', "*", "&", ">", "<", "=", "#", "%", "@", "!", "?", ".", ",", ";", "(", ")", "[", "]", "{", "}"];

const NAMED_KEYS: BarKey[] = ["esc", "tab", "up", "down", "left", "right", "home", "end", "pgup", "pgdn", "ins", "del", ...FUNCTION_KEYS];

export type ItemKind =
  | { kind: "key"; key: BarKey }
  | { kind: "modifier"; modifier: "ctrl" | "alt" }
  | { kind: "paste" }
  | { kind: "char"; text: string }
  | { kind: "snippet"; snippet: SnippetKey };

/** Every built-in item id, in the order the picker lists them. */
export const BUILTIN_ITEMS: string[] = [
  "esc",
  "tab",
  "ctrl",
  "alt",
  "left",
  "down",
  "up",
  "right",
  "home",
  "end",
  "pgup",
  "pgdn",
  "ins",
  "del",
  "paste",
  ...FUNCTION_KEYS,
  ...SYMBOLS.map((symbol) => `char:${symbol}`),
];

export function defaultLayout(): BarLayout {
  return {
    primary: ["esc", "tab", "ctrl", "alt", "left", "down", "up", "right"],
    extra: [
      "home",
      "end",
      "pgup",
      "pgdn",
      "paste",
      ...["~", "|", "/", "-", "_", "\\", ":"].map((symbol) => `char:${symbol}`),
      ...FUNCTION_KEYS,
    ],
    snippets: [],
  };
}

/** What an item id means, or `null` for an id this version does not know. */
export function resolveItem(id: string, layout: Pick<BarLayout, "snippets">): ItemKind | null {
  if (id === "ctrl" || id === "alt") return { kind: "modifier", modifier: id };
  if (id === "paste") return { kind: "paste" };
  if (id.startsWith("char:")) {
    const text = id.slice(5);
    return SYMBOLS.includes(text) ? { kind: "char", text } : null;
  }
  if (id.startsWith("snippet:")) {
    const snippet = layout.snippets.find((candidate) => candidate.id === id.slice(8));
    return snippet ? { kind: "snippet", snippet } : null;
  }
  return NAMED_KEYS.includes(id as BarKey) ? { kind: "key", key: id as BarKey } : null;
}

function isSnippet(value: unknown): value is SnippetKey {
  if (typeof value !== "object" || value === null) return false;
  const snippet = value as Record<string, unknown>;
  return (
    typeof snippet.id === "string" &&
    /^[A-Za-z0-9_-]{1,40}$/.test(snippet.id) &&
    typeof snippet.label === "string" &&
    snippet.label.length > 0 &&
    typeof snippet.text === "string" &&
    snippet.text.length > 0 &&
    typeof snippet.run === "boolean"
  );
}

function cleanRow(raw: unknown, layout: Pick<BarLayout, "snippets">, taken: Set<string>): string[] {
  if (!Array.isArray(raw)) return [];
  const row: string[] = [];
  for (const id of raw) {
    if (typeof id !== "string" || taken.has(id) || resolveItem(id, layout) === null) continue;
    taken.add(id);
    row.push(id);
    if (row.length >= MAX_ROW_ITEMS) break;
  }
  return row;
}

/** Makes saved data safe to use: unknown ids, duplicates, broken snippets and
 *  oversize rows are dropped; a layout with no usable primary row falls back
 *  to the default. */
export function sanitiseLayout(raw: unknown): BarLayout {
  if (typeof raw !== "object" || raw === null) return defaultLayout();
  const data = raw as Record<string, unknown>;
  const snippets = (Array.isArray(data.snippets) ? data.snippets : [])
    .filter(isSnippet)
    .slice(0, MAX_SNIPPETS)
    .map((snippet) => ({
      id: snippet.id,
      label: [...snippet.label].slice(0, MAX_LABEL).join(""),
      text: snippet.text.slice(0, MAX_SNIPPET_TEXT),
      run: snippet.run,
    }));
  const taken = new Set<string>();
  const primary = cleanRow(data.primary, { snippets }, taken);
  const extra = cleanRow(data.extra, { snippets }, taken);
  if (primary.length === 0) return defaultLayout();
  return { primary, extra, snippets };
}

/** Items that exist but are in neither row (what "Add key" offers). */
export function unusedItems(layout: BarLayout): string[] {
  const used = new Set([...layout.primary, ...layout.extra]);
  return [...BUILTIN_ITEMS, ...layout.snippets.map((snippet) => `snippet:${snippet.id}`)].filter((id) => !used.has(id));
}

export function moveItem(layout: BarLayout, row: BarRow, index: number, delta: -1 | 1): BarLayout {
  const items = [...layout[row]];
  const target = index + delta;
  if (index < 0 || index >= items.length || target < 0 || target >= items.length) return layout;
  [items[index], items[target]] = [items[target], items[index]];
  return { ...layout, [row]: items };
}

/** Moves an item to the other row (end of it). The primary row keeps at least one item. */
export function moveToRow(layout: BarLayout, from: BarRow, index: number): BarLayout {
  const to: BarRow = from === "primary" ? "extra" : "primary";
  const id = layout[from][index];
  if (id === undefined || layout[to].length >= MAX_ROW_ITEMS) return layout;
  if (from === "primary" && layout.primary.length <= 1) return layout;
  return {
    ...layout,
    [from]: layout[from].filter((_, position) => position !== index),
    [to]: [...layout[to], id],
  };
}

export function removeItem(layout: BarLayout, row: BarRow, index: number): BarLayout {
  if (row === "primary" && layout.primary.length <= 1) return layout;
  const id = layout[row][index];
  if (id === undefined) return layout;
  const next = { ...layout, [row]: layout[row].filter((_, position) => position !== index) };
  // A snippet key that is on no row is gone for good.
  if (id.startsWith("snippet:") && !next.primary.includes(id) && !next.extra.includes(id)) {
    return { ...next, snippets: next.snippets.filter((snippet) => `snippet:${snippet.id}` !== id) };
  }
  return next;
}

export function addItem(layout: BarLayout, row: BarRow, id: string): BarLayout {
  if (resolveItem(id, layout) === null || layout.primary.includes(id) || layout.extra.includes(id)) return layout;
  if (layout[row].length >= MAX_ROW_ITEMS) return layout;
  return { ...layout, [row]: [...layout[row], id] };
}

/** Adds a snippet key to the end of a row. `id` must be unique; returns the
 *  layout unchanged for an empty label or text. */
export function addSnippet(layout: BarLayout, row: BarRow, snippet: SnippetKey): BarLayout {
  const label = [...snippet.label.trim()].slice(0, MAX_LABEL).join("");
  const text = snippet.text.slice(0, MAX_SNIPPET_TEXT);
  if (!label || !text.trim() || layout.snippets.length >= MAX_SNIPPETS) return layout;
  if (layout.snippets.some((existing) => existing.id === snippet.id)) return layout;
  if (layout[row].length >= MAX_ROW_ITEMS) return layout;
  return {
    ...layout,
    snippets: [...layout.snippets, { id: snippet.id, label, text, run: snippet.run }],
    [row]: [...layout[row], `snippet:${snippet.id}`],
  };
}

/** What a snippet key sends: its text, then Enter when it runs. */
export function snippetBytes(snippet: SnippetKey): string {
  return snippet.run ? `${snippet.text}\r` : snippet.text;
}
