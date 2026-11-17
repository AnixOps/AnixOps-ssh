import { describe, expect, it } from "vitest";
import { broadcastTargets, participants, pasteNeedsConfirm, scopeOf, type BroadcastState } from "./broadcast";

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
});
