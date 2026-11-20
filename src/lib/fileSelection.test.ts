import { describe, expect, it } from "vitest";
import {
  clickSelect,
  emptySelection,
  neighbour,
  selectAll,
  selectOnly,
  targetsOf,
} from "./fileSelection";

const order = ["a", "b", "c", "d", "e"];
const plain = { toggle: false, shift: false };

describe("clicking", () => {
  it("selects one entry and makes it the anchor", () => {
    expect(clickSelect(emptySelection, order, "c", plain)).toEqual({ names: ["c"], anchor: "c" });
    expect(clickSelect(selectOnly("a"), order, "c", plain)).toEqual({ names: ["c"], anchor: "c" });
  });

  it("toggles with Ctrl or Command and keeps listing order", () => {
    let selection = clickSelect(emptySelection, order, "d", plain);
    selection = clickSelect(selection, order, "b", { toggle: true, shift: false });
    expect(selection.names).toEqual(["b", "d"]);
    expect(selection.anchor).toBe("b");
    selection = clickSelect(selection, order, "d", { toggle: true, shift: false });
    expect(selection.names).toEqual(["b"]);
  });

  it("selects the range from the anchor in either direction", () => {
    const from = clickSelect(emptySelection, order, "b", plain);
    expect(clickSelect(from, order, "d", { toggle: false, shift: true })).toEqual({ names: ["b", "c", "d"], anchor: "b" });
    expect(clickSelect(from, order, "a", { toggle: false, shift: true }).names).toEqual(["a", "b"]);
  });

  it("keeps the anchor across Shift clicks, replacing the previous range", () => {
    let selection = clickSelect(emptySelection, order, "b", plain);
    selection = clickSelect(selection, order, "e", { toggle: false, shift: true });
    selection = clickSelect(selection, order, "c", { toggle: false, shift: true });
    expect(selection).toEqual({ names: ["b", "c"], anchor: "b" });
  });

  it("adds a range to the selection with Ctrl+Shift", () => {
    let selection = clickSelect(emptySelection, order, "a", plain);
    selection = clickSelect(selection, order, "d", { toggle: true, shift: false });
    selection = clickSelect(selection, order, "e", { toggle: true, shift: true });
    expect(selection.names).toEqual(["a", "d", "e"]);
  });

  it("starts a range at the clicked entry when there is no anchor", () => {
    expect(clickSelect(emptySelection, order, "c", { toggle: false, shift: true }).names).toEqual(["c"]);
  });

  it("ignores names that are not listed", () => {
    const selection = selectOnly("a");
    expect(clickSelect(selection, order, "zzz", plain)).toBe(selection);
  });
});

describe("select all", () => {
  it("selects every entry", () => {
    expect(selectAll(order).names).toEqual(order);
    expect(selectAll([])).toEqual(emptySelection);
  });
});

describe("keyboard and row actions", () => {
  it("finds the neighbour in a direction", () => {
    expect(neighbour(order, "c", 1)).toBe("d");
    expect(neighbour(order, "c", -1)).toBe("b");
    expect(neighbour(order, "a", -1)).toBeNull();
    expect(neighbour(order, "e", 1)).toBeNull();
    expect(neighbour(order, "x", 1)).toBeNull();
  });

  it("acts on the whole selection only when the row belongs to a multi-selection", () => {
    const many = { names: ["a", "c"], anchor: "a" };
    expect(targetsOf(many, "c")).toEqual(["a", "c"]);
    expect(targetsOf(many, "b")).toEqual(["b"]);
    expect(targetsOf(selectOnly("a"), "a")).toEqual(["a"]);
  });
});
