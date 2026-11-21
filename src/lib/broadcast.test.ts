import { describe, expect, it } from "vitest";
import {
  broadcastTargets,
  participants,
  pasteNeedsConfirm,
  scopeOf,
  unconfirmedProduction,
  withoutUnconfirmed,
  type BroadcastState,
} from "./broadcast";

const panes = new Map<string, string[]>([
  ["t1", ["p1", "p2", "p3"]],
  ["t2", ["p4", "p5"]],
]);
const off: BroadcastState = { global: false, tabs: new Set() };

describe("broadcast targeting", () => {
  it("sends nowhere when broadcast is off", () => {
    expect(broadcastTargets(off, panes, "p1")).toEqual([]);
    expect(scopeOf(off, "t1")).toBeNull();
  });

  it("limits a tab broadcast to that tab, without the source", () => {
    const state: BroadcastState = { global: false, tabs: new Set(["t1"]) };
    expect(broadcastTargets(state, panes, "p2")).toEqual(["p1", "p3"]);
    expect(broadcastTargets(state, panes, "p4")).toEqual([]);
    expect(scopeOf(state, "t1")).toBe("tab");
    expect(participants(state, panes, "t1")).toEqual(["p1", "p2", "p3"]);
    expect(participants(state, panes, "t2")).toEqual([]);
  });

  it("reaches every pane of every tab when global", () => {
    const state: BroadcastState = { global: true, tabs: new Set() };
    expect(broadcastTargets(state, panes, "p5")).toEqual(["p1", "p2", "p3", "p4"]);
    expect(scopeOf(state, "t2")).toBe("global");
  });

  it("skips panes that are not connected", () => {
    const state: BroadcastState = { global: true, tabs: new Set() };
    expect(broadcastTargets(state, panes, "p1", (id) => id !== "p3" && id !== "p5")).toEqual(["p2", "p4"]);
  });

  it("ignores an unknown source", () => {
    const state: BroadcastState = { global: true, tabs: new Set() };
    expect(broadcastTargets(state, panes, "nope")).toEqual([]);
  });
});

describe("paste confirmation", () => {
  it("asks only for a newline going to other panes", () => {
    expect(pasteNeedsConfirm("ls\n", 2)).toBe(true);
    expect(pasteNeedsConfirm("a\r\nb", 1)).toBe(true);
    expect(pasteNeedsConfirm("ls", 2)).toBe(false);
    expect(pasteNeedsConfirm("ls\n", 0)).toBe(false);
  });

  it("asks before a multi-line paste into one pane without bracketed paste", () => {
    const plain = { bracketedPaste: false, warnMultiline: true };
    expect(pasteNeedsConfirm("rm -rf build\nmake\n", 0, plain)).toBe(true);
    expect(pasteNeedsConfirm("a\rb", 0, plain)).toBe(true);
    expect(pasteNeedsConfirm("one line", 0, plain)).toBe(false);
    // The app takes the paste as one block: nothing runs by itself.
    expect(pasteNeedsConfirm("a\nb", 0, { bracketedPaste: true, warnMultiline: true })).toBe(false);
    // Turned off in the settings.
    expect(pasteNeedsConfirm("a\nb", 0, { bracketedPaste: false, warnMultiline: false })).toBe(false);
    // Several panes: always asked, whatever the pane's mode.
    expect(pasteNeedsConfirm("a\nb", 2, { bracketedPaste: true, warnMultiline: false })).toBe(true);
  });
});

describe("production hosts in a broadcast", () => {
  const production = new Map([["t2", "prod-db"]]);
  it("asks for the production tabs a scope reaches, once", () => {
    expect(unconfirmedProduction(null, "t1", panes, production, new Set())).toEqual([]);
    expect(unconfirmedProduction("tab", "t1", panes, production, new Set())).toEqual([]);
    expect(unconfirmedProduction("tab", "t2", panes, production, new Set())).toEqual([["t2", "prod-db"]]);
    expect(unconfirmedProduction("global", "t1", panes, production, new Set())).toEqual([["t2", "prod-db"]]);
    expect(unconfirmedProduction("global", "t1", panes, production, new Set(["t2"]))).toEqual([]);
  });

  it("leaves out production panes that were not confirmed", () => {
    const ids = ["p1", "p4", "p5"];
    expect(withoutUnconfirmed(ids, panes, production, new Set())).toEqual(["p1"]);
    expect(withoutUnconfirmed(ids, panes, production, new Set(["t2"]))).toEqual(ids);
  });

  it("always confirms a multi-line paste into production, unless the app takes bracketed paste", () => {
    const off = { bracketedPaste: false, warnMultiline: false };
    expect(pasteNeedsConfirm("a\nb", 0, off)).toBe(false);
    expect(pasteNeedsConfirm("a\nb", 0, { ...off, production: true })).toBe(true);
    expect(pasteNeedsConfirm("a", 0, { ...off, production: true })).toBe(false);
    expect(pasteNeedsConfirm("a\nb", 0, { bracketedPaste: true, warnMultiline: true, production: true })).toBe(false);
  });
});
