import { describe, expect, it } from "vitest";
import {
  MAX_RATIO,
  MIN_RATIO,
  closeInState,
  closePane,
  clampRatio,
  cycleFocus,
  cyclePane,
  deserialize,
  focusDirection,
  initialState,
  isSavedNode,
  layout,
  leaf,
  leaves,
  moveFocus,
  pruneSaved,
  resizeSplit,
  serialize,
  splitFocused,
  splitPane,
  toggleMaximise,
  visibleLayout,
  type PaneNode,
} from "./panes";

/** a | (b over c) */
function sample(): PaneNode {
  let tree = leaf("a");
  tree = splitPane(tree, "a", "row", "b", "s1");
  tree = splitPane(tree, "b", "col", "c", "s2");
  return tree;
}

describe("split and close", () => {
  it("splits a leaf into two halves", () => {
    const tree = splitPane(leaf("a"), "a", "row", "b", "s1");
    expect(leaves(tree)).toEqual(["a", "b"]);
    expect(layout(tree).panes).toEqual({
      a: { x: 0, y: 0, w: 0.5, h: 1 },
      b: { x: 0.5, y: 0, w: 0.5, h: 1 },
    });
  });

  it("nests splits", () => {
    const tree = sample();
    expect(leaves(tree)).toEqual(["a", "b", "c"]);
    const { panes, dividers } = layout(tree);
    expect(panes.b).toEqual({ x: 0.5, y: 0, w: 0.5, h: 0.5 });
    expect(panes.c).toEqual({ x: 0.5, y: 0.5, w: 0.5, h: 0.5 });
    expect(dividers.map((d) => d.id)).toEqual(["s1", "s2"]);
  });

  it("ignores a split of an unknown pane", () => {
    const tree = sample();
    expect(leaves(splitPane(tree, "zzz", "row", "d", "s3"))).toEqual(["a", "b", "c"]);
  });

  it("gives the space of a closed pane to its sibling", () => {
    const tree = closePane(sample(), "b");
    expect(tree && layout(tree).panes.c).toEqual({ x: 0.5, y: 0, w: 0.5, h: 1 });
  });

  it("returns null when the last pane closes", () => {
    expect(closePane(leaf("a"), "a")).toBeNull();
  });

  it("leaves the tree alone when the pane is unknown", () => {
    const tree = sample();
    expect(closePane(tree, "zzz")).toBe(tree);
  });
});

describe("resize", () => {
  it("moves a divider", () => {
    const tree = resizeSplit(sample(), "s1", 0.3);
    expect(layout(tree).panes.a.w).toBeCloseTo(0.3);
    expect(layout(tree).panes.b.x).toBeCloseTo(0.3);
  });

  it("clamps so no pane vanishes", () => {
    expect(clampRatio(0)).toBe(MIN_RATIO);
    expect(clampRatio(2)).toBe(MAX_RATIO);
    expect(clampRatio(Number.NaN)).toBe(0.5);
    const tree = resizeSplit(sample(), "s2", -5);
    expect(layout(tree).panes.b.h).toBeCloseTo(MIN_RATIO);
  });

  it("ignores an unknown split", () => {
    expect(layout(resizeSplit(sample(), "nope", 0.2))).toEqual(layout(sample()));
  });
});

describe("focus navigation", () => {
  const tree = sample();

  it("cycles in reading order and wraps", () => {
    expect(cyclePane(tree, "a", 1)).toBe("b");
    expect(cyclePane(tree, "c", 1)).toBe("a");
    expect(cyclePane(tree, "a", -1)).toBe("c");
  });

  it("moves by geometry", () => {
    expect(focusDirection(tree, "a", "right")).toBe("b");
    expect(focusDirection(tree, "b", "down")).toBe("c");
    expect(focusDirection(tree, "c", "up")).toBe("b");
    expect(focusDirection(tree, "c", "left")).toBe("a");
    expect(focusDirection(tree, "b", "left")).toBe("a");
  });

  it("stays put at an edge", () => {
    expect(focusDirection(tree, "a", "left")).toBe("a");
    expect(focusDirection(tree, "a", "up")).toBe("a");
    expect(focusDirection(tree, "c", "down")).toBe("c");
  });
});

describe("pane state", () => {
  it("focuses the new pane after a split", () => {
    const state = splitFocused(initialState("a"), "row", "b", "s1");
    expect(state.focused).toBe("b");
    expect(leaves(state.tree)).toEqual(["a", "b"]);
  });

  it("moves focus to a neighbour when the focused pane closes", () => {
    let state = splitFocused(initialState("a"), "row", "b", "s1");
    state = splitFocused(state, "row", "c", "s2");
    const closed = closeInState(state, "c");
    expect(closed?.focused).toBe("b");
    expect(closeInState(closeInState(state, "b")!, "c")?.focused).toBe("a");
  });

  it("keeps focus when another pane closes, and refuses to close the last", () => {
    const state = splitFocused(initialState("a"), "row", "b", "s1");
    expect(closeInState(state, "a")?.focused).toBe("b");
    expect(closeInState(initialState("a"), "a")).toBeNull();
  });

  it("maximises and restores the focused pane", () => {
    const state = splitFocused(initialState("a"), "row", "b", "s1");
    const max = toggleMaximise(state);
    expect(max.maximised).toBe("b");
    expect(visibleLayout(max).panes).toEqual({ b: { x: 0, y: 0, w: 1, h: 1 } });
    expect(visibleLayout(max).dividers).toEqual([]);
    expect(toggleMaximise(max).maximised).toBeNull();
  });

  it("does not maximise a lone pane", () => {
    expect(toggleMaximise(initialState("a")).maximised).toBeNull();
  });

  it("takes the maximised view along when focus cycles", () => {
    const max = toggleMaximise(splitFocused(initialState("a"), "row", "b", "s1"));
    const next = cycleFocus(max, 1);
    expect(next.focused).toBe("a");
    expect(next.maximised).toBe("a");
    expect(moveFocus(max, "left")).toBe(max);
  });

  it("clears maximise when the maximised pane closes", () => {
    const max = toggleMaximise(splitFocused(initialState("a"), "row", "b", "s1"));
    expect(closeInState(max, "b")?.maximised).toBeNull();
  });
});

describe("serialization", () => {
  it("round-trips the shape, ratios and sessions", () => {
    const tree = resizeSplit(sample(), "s1", 0.3);
    const sessions: Record<string, string> = { a: "sess-a", b: "sess-b", c: "sess-c" };
    const saved = serialize(tree, (id) => sessions[id] ?? null);
    const json = JSON.parse(JSON.stringify(saved));
    expect(isSavedNode(json)).toBe(true);

    let counter = 0;
    const restored = deserialize(json, () => `n${counter++}`);
    expect(leaves(restored.tree).map((id) => restored.sessions[id])).toEqual(["sess-a", "sess-b", "sess-c"]);
    const originalShape = Object.values(layout(tree).panes);
    const restoredShape = Object.values(layout(restored.tree).panes);
    expect(restoredShape).toEqual(originalShape);
  });

  it("clamps ratios read from storage", () => {
    const { tree } = deserialize(
      { t: "split", axis: "row", ratio: 7, a: { t: "leaf", session: null }, b: { t: "leaf", session: null } },
      (() => {
        let n = 0;
        return () => `p${n++}`;
      })(),
    );
    expect(tree.type === "split" && tree.ratio).toBe(MAX_RATIO);
  });

  it("rejects malformed data", () => {
    expect(isSavedNode(null)).toBe(false);
    expect(isSavedNode({ t: "leaf" })).toBe(false);
    expect(isSavedNode({ t: "split", axis: "diag", ratio: 0.5, a: { t: "leaf", session: null }, b: { t: "leaf", session: null } })).toBe(false);
    let deep: unknown = { t: "leaf", session: null };
    for (let i = 0; i < 40; i++) deep = { t: "split", axis: "row", ratio: 0.5, a: deep, b: { t: "leaf", session: null } };
    expect(isSavedNode(deep)).toBe(false);
  });

  it("prunes panes whose session is gone", () => {
    const saved = serialize(sample(), (id) => `s-${id}`);
    const pruned = pruneSaved(saved, (session) => session !== "s-b");
    expect(pruned && leaves(deserialize(pruned, () => Math.random().toString()).tree).length).toBe(2);
    expect(pruneSaved(saved, () => false)).toBeNull();
  });
});
