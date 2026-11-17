import { describe, expect, it } from "vitest";
import { parseLayout, planRestore, type SavedLayout } from "./layoutStore";
import type { SavedNode } from "./panes";

const leaf = (session: string | null): SavedNode => ({ t: "leaf", session });
const split = (a: SavedNode, b: SavedNode): SavedNode => ({ t: "split", axis: "row", ratio: 0.5, a, b });

describe("layout restore", () => {
  const saved: SavedLayout = {
    version: 1,
    active: 1,
    tabs: [
      { hostId: "h1", filesOpen: false, tree: split(leaf("s1"), leaf("s2")), focused: 1, maximised: null },
      { hostId: null, filesOpen: false, tree: leaf("s3"), focused: 0, maximised: null },
    ],
  };

  it("restores the arrangement when every session is alive", () => {
    const plan = planRestore(saved, [
      { id: "s1", hostId: "h1" },
      { id: "s2", hostId: "h1" },
      { id: "s3", hostId: null },
    ]);
    expect(plan.tabs).toHaveLength(2);
    expect(plan.tabs[0].tree).toEqual(saved.tabs[0].tree);
    expect(plan.tabs[0].focused).toBe(1);
    expect(plan.active).toBe(1);
  });

  it("drops dead panes and empty tabs, and keeps focus on the right pane", () => {
    const plan = planRestore(saved, [{ id: "s2", hostId: "h1" }]);
    expect(plan.tabs).toHaveLength(1);
    expect(plan.tabs[0].tree).toEqual(leaf("s2"));
    expect(plan.tabs[0].focused).toBe(0);
    expect(plan.active).toBe(0);
  });

  it("brings back sessions the layout never mentioned as their own tabs", () => {
    const plan = planRestore(saved, [
      { id: "s1", hostId: "h1" },
      { id: "s9", hostId: "h2" },
    ]);
    expect(plan.tabs.map((tab) => tab.hostId)).toEqual(["h1", "h2"]);
    expect(plan.tabs[1].tree).toEqual(leaf("s9"));
  });

  it("falls back to one tab per session without a saved layout", () => {
    const plan = planRestore(null, [
      { id: "a", hostId: null },
      { id: "b", hostId: "h" },
    ]);
    expect(plan.tabs).toHaveLength(2);
    expect(plan.active).toBe(0);
    expect(planRestore(null, []).active).toBeNull();
  });

  it("uses a session only once", () => {
    const duplicate: SavedLayout = {
      ...saved,
      tabs: [saved.tabs[1], { ...saved.tabs[1] }],
    };
    expect(planRestore(duplicate, [{ id: "s3", hostId: null }]).tabs).toHaveLength(1);
  });
});

describe("parseLayout", () => {
  it("round-trips JSON", () => {
    const saved: SavedLayout = {
      version: 1,
      active: 0,
      tabs: [{ hostId: "h", filesOpen: true, tree: leaf("s"), focused: 0, maximised: null }],
    };
    expect(parseLayout(JSON.stringify(saved))).toEqual(saved);
  });

  it("rejects garbage and drops bad tabs", () => {
    expect(parseLayout(null)).toBeNull();
    expect(parseLayout("{")).toBeNull();
    expect(parseLayout(JSON.stringify({ version: 2, tabs: [] }))).toBeNull();
    const mixed = { version: 1, active: 0, tabs: [{ nope: true }, { hostId: null, tree: leaf("x"), focused: 0, maximised: null }] };
    expect(parseLayout(JSON.stringify(mixed))?.tabs).toHaveLength(1);
  });
});
