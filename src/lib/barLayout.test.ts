import { describe, expect, it } from "vitest";
import {
  addItem,
  addSnippet,
  BUILTIN_ITEMS,
  defaultLayout,
  MAX_LABEL,
  moveItem,
  moveToRow,
  removeItem,
  resolveItem,
  sanitiseLayout,
  snippetBytes,
  unusedItems,
} from "./barLayout";

describe("the default bar", () => {
  it("has the keys a soft keyboard lacks in the first row and F1 to F12 in the second", () => {
    const layout = defaultLayout();
    expect(layout.primary).toEqual(["esc", "tab", "ctrl", "alt", "left", "down", "up", "right"]);
    for (let n = 1; n <= 12; n++) expect(layout.extra).toContain(`f${n}`);
    for (const key of ["home", "end", "pgup", "pgdn", "paste"]) expect(layout.extra).toContain(key);
  });

  it("uses every id once and every id resolves", () => {
    const layout = defaultLayout();
    const all = [...layout.primary, ...layout.extra];
    expect(new Set(all).size).toBe(all.length);
    for (const id of all) expect(resolveItem(id, layout), id).not.toBeNull();
    for (const id of BUILTIN_ITEMS) expect(resolveItem(id, layout), id).not.toBeNull();
  });
});

describe("resolving items", () => {
  it("tells keys, modifiers, symbols, paste and snippets apart", () => {
    const layout = { snippets: [{ id: "a1", label: "up", text: "uptime", run: true }] };
    expect(resolveItem("esc", layout)).toEqual({ kind: "key", key: "esc" });
    expect(resolveItem("ctrl", layout)).toEqual({ kind: "modifier", modifier: "ctrl" });
    expect(resolveItem("char:|", layout)).toEqual({ kind: "char", text: "|" });
    expect(resolveItem("paste", layout)).toEqual({ kind: "paste" });
    expect(resolveItem("snippet:a1", layout)).toMatchObject({ kind: "snippet" });
    expect(resolveItem("snippet:missing", layout)).toBeNull();
    expect(resolveItem("char:é", layout)).toBeNull();
    expect(resolveItem("f13", layout)).toBeNull();
  });
});

describe("editing the bar", () => {
  it("moves an item and stops at the ends", () => {
    const layout = defaultLayout();
    expect(moveItem(layout, "primary", 0, 1).primary.slice(0, 2)).toEqual(["tab", "esc"]);
    expect(moveItem(layout, "primary", 0, -1)).toBe(layout);
    expect(moveItem(layout, "primary", layout.primary.length - 1, 1)).toBe(layout);
  });

  it("removes an item and offers it again", () => {
    let layout = removeItem(defaultLayout(), "extra", 0);
    expect(layout.extra).not.toContain("home");
    expect(unusedItems(layout)).toContain("home");
    layout = addItem(layout, "primary", "home");
    expect(layout.primary.at(-1)).toBe("home");
    expect(unusedItems(layout)).not.toContain("home");
  });

  it("never lets the primary row run empty", () => {
    let layout = { ...defaultLayout(), primary: ["esc"] };
    expect(removeItem(layout, "primary", 0)).toBe(layout);
    expect(moveToRow(layout, "primary", 0)).toBe(layout);
    layout = { ...layout, primary: ["esc", "tab"] };
    const moved = moveToRow(layout, "primary", 1);
    expect(moved.primary).toEqual(["esc"]);
    expect(moved.extra.at(-1)).toBe("tab");
  });

  it("refuses an id that is unknown or already placed", () => {
    const layout = defaultLayout();
    expect(addItem(layout, "extra", "nonsense")).toBe(layout);
    expect(addItem(layout, "extra", "esc")).toBe(layout);
  });

  it("adds a snippet key, trims its label and types its text with Enter when asked", () => {
    const layout = addSnippet(defaultLayout(), "extra", { id: "s1", label: "  tail logs-long-label ", text: "tail -f /var/log/syslog", run: true });
    expect(layout.extra.at(-1)).toBe("snippet:s1");
    expect([...layout.snippets[0].label].length).toBeLessThanOrEqual(MAX_LABEL);
    expect(snippetBytes(layout.snippets[0])).toBe("tail -f /var/log/syslog\r");
    expect(snippetBytes({ ...layout.snippets[0], run: false })).toBe("tail -f /var/log/syslog");
  });

  it("refuses an empty snippet or a repeated id", () => {
    const layout = defaultLayout();
    expect(addSnippet(layout, "extra", { id: "s1", label: "", text: "ls", run: false })).toBe(layout);
    expect(addSnippet(layout, "extra", { id: "s1", label: "ls", text: "  ", run: false })).toBe(layout);
    const once = addSnippet(layout, "extra", { id: "s1", label: "ls", text: "ls", run: false });
    expect(addSnippet(once, "extra", { id: "s1", label: "x", text: "x", run: false })).toBe(once);
  });

  it("deleting a snippet key from its row deletes the snippet", () => {
    let layout = addSnippet(defaultLayout(), "primary", { id: "s1", label: "ls", text: "ls", run: true });
    layout = removeItem(layout, "primary", layout.primary.length - 1);
    expect(layout.snippets).toEqual([]);
  });
});

describe("loading a saved bar", () => {
  it("falls back to the default for nothing or junk", () => {
    expect(sanitiseLayout(null)).toEqual(defaultLayout());
    expect(sanitiseLayout("x")).toEqual(defaultLayout());
    expect(sanitiseLayout({ primary: [] })).toEqual(defaultLayout());
    expect(sanitiseLayout({ primary: "esc" })).toEqual(defaultLayout());
  });

  it("drops unknown ids, duplicates and snippets whose definition is gone", () => {
    const layout = sanitiseLayout({
      primary: ["esc", "esc", "bogus", 7, "snippet:gone", "snippet:s1"],
      extra: ["esc", "f1", "f1"],
      snippets: [
        { id: "s1", label: "ls", text: "ls", run: true },
        { id: "bad id!", label: "x", text: "x", run: true },
        { id: "s2", label: "", text: "x", run: true },
        { label: "x" },
      ],
    });
    expect(layout.primary).toEqual(["esc", "snippet:s1"]);
    expect(layout.extra).toEqual(["f1"]);
    expect(layout.snippets).toHaveLength(1);
  });

  it("keeps rows and snippets within their limits", () => {
    const many = Array.from({ length: 100 }, (_, n) => `f${(n % 12) + 1}`);
    const layout = sanitiseLayout({ primary: ["esc", ...many], extra: [], snippets: [] });
    expect(layout.primary.length).toBeLessThanOrEqual(32);
    const long = sanitiseLayout({ primary: ["snippet:s"], snippets: [{ id: "s", label: "0123456789abc", text: "x".repeat(1000), run: false }] });
    expect(long.snippets[0].label).toHaveLength(MAX_LABEL);
    expect(long.snippets[0].text.length).toBeLessThanOrEqual(240);
  });

  it("round-trips through JSON", () => {
    const layout = addSnippet(defaultLayout(), "extra", { id: "s1", label: "ls", text: "ls -la", run: true });
    expect(sanitiseLayout(JSON.parse(JSON.stringify(layout)))).toEqual(layout);
  });
});
