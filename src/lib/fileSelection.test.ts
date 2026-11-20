import { describe, expect, it } from "vitest";
import { actionTargets, clickSelect, emptySelection, inOrder, prune, selectAll } from "./fileSelection";

const order = ["a", "b", "c", "d", "e"];
const names = (s: { names: ReadonlySet<string> }) => [...s.names].sort();

describe("clickSelect", () => {
  it("selects only the clicked row", () => {
    const first = clickSelect(emptySelection, order, "b");
    expect(names(first)).toEqual(["b"]);
    expect(names(clickSelect(first, order, "d"))).toEqual(["d"]);
  });

  it("toggles with Ctrl or Cmd", () => {
    let s = clickSelect(emptySelection, order, "a");
    s = clickSelect(s, order, "c", { toggle: true });
    expect(names(s)).toEqual(["a", "c"]);
    s = clickSelect(s, order, "a", { toggle: true });
    expect(names(s)).toEqual(["c"]);
  });

  it("selects a range from the anchor with Shift, in either direction", () => {
    const s = clickSelect(emptySelection, order, "b");
    expect(names(clickSelect(s, order, "d", { shift: true }))).toEqual(["b", "c", "d"]);
    expect(names(clickSelect(s, order, "a", { shift: true }))).toEqual(["a", "b"]);
  });

  it("keeps the anchor while the range changes", () => {
    let s = clickSelect(emptySelection, order, "b");
    s = clickSelect(s, order, "e", { shift: true });
    s = clickSelect(s, order, "c", { shift: true });
    expect(names(s)).toEqual(["b", "c"]);
  });

  it("adds a range to the selection with Ctrl+Shift", () => {
    let s = clickSelect(emptySelection, order, "a");
    s = clickSelect(s, order, "c", { toggle: true });
    s = clickSelect(s, order, "e", { shift: true, toggle: true });
    expect(names(s)).toEqual(["a", "c", "d", "e"]);
  });

  it("Shift without an anchor acts as a plain click", () => {
    expect(names(clickSelect(emptySelection, order, "c", { shift: true }))).toEqual(["c"]);
  });
});

describe("selection helpers", () => {
  it("selects all", () => {
    expect(names(selectAll(order))).toEqual(order);
    expect(selectAll([]).names.size).toBe(0);
  });

  it("drops names that are no longer listed", () => {
    const s = selectAll(order);
    const next = prune(s, ["a", "c"]);
    expect(names(next)).toEqual(["a", "c"]);
    expect(prune(next, ["a", "c"])).toBe(next);
  });

  it("lists the selection in listing order", () => {
    const s = clickSelect(clickSelect(emptySelection, order, "d"), order, "b", { toggle: true });
    expect(inOrder(s, order)).toEqual(["b", "d"]);
  });

  it("applies a row action to the selection only when the row is in it", () => {
    const s = clickSelect(clickSelect(emptySelection, order, "d"), order, "b", { toggle: true });
    expect(actionTargets(s, order, "b")).toEqual(["b", "d"]);
    expect(actionTargets(s, order, "a")).toEqual(["a"]);
  });
});
