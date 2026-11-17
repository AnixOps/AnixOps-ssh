import { describe, expect, it } from "vitest";
import {
  allState,
  buildApply,
  decodeChoice,
  defaultSelection,
  describeGroup,
  describeTarget,
  encodeChoice,
  importCount,
  setAll,
  tally,
  toggle,
} from "./importSelection";
import type { ImportHost } from "./proto.gen";

const host = (label: string, extra: Partial<ImportHost> = {}): ImportHost => ({
  label,
  hostname: `${label}.example.com`,
  port: 22,
  ...extra,
});

const hosts: ImportHost[] = [
  host("new-a", { group_path: ["Prod", "Web"] }),
  host("old", { exists: true }),
  host("bad", { problem: "username not acceptable" }),
  host("legacy", { openssh_only: ["ProxyCommand"] }),
];

describe("import selection", () => {
  it("starts with importable hosts that are not stored yet", () => {
    expect([...defaultSelection(hosts)]).toEqual([0, 3]);
  });

  it("never selects a host with a problem", () => {
    expect(toggle(new Set(), hosts, 2).size).toBe(0);
    expect([...setAll(hosts, true)]).toEqual([0, 1, 3]);
    expect(setAll(hosts, false).size).toBe(0);
  });

  it("toggles one host without touching the others, and does not mutate", () => {
    const start = defaultSelection(hosts);
    const next = toggle(start, hosts, 1);
    expect([...next].sort()).toEqual([0, 1, 3]);
    expect([...start]).toEqual([0, 3]);
    expect([...toggle(next, hosts, 0)].sort()).toEqual([1, 3]);
  });

  it("reports the state of the select-all box", () => {
    expect(allState(new Set(), hosts)).toBe("none");
    expect(allState(defaultSelection(hosts), hosts)).toBe("some");
    expect(allState(setAll(hosts, true), hosts)).toBe("all");
  });

  it("counts what each duplicate policy does with a selected duplicate", () => {
    const all = setAll(hosts, true);
    expect(tally(hosts, all, "skip")).toEqual({ create: 2, update: 0, skip: 1 });
    expect(tally(hosts, all, "update")).toEqual({ create: 2, update: 1, skip: 0 });
    expect(tally(hosts, all, "keep-both")).toEqual({ create: 3, update: 0, skip: 0 });
    expect(importCount(tally(hosts, all, "skip"))).toBe(2);
    expect(importCount(tally(hosts, all, "update"))).toBe(3);
    // Unselected duplicates are not counted at all.
    expect(tally(hosts, defaultSelection(hosts), "update")).toEqual({ create: 2, update: 0, skip: 0 });
  });

  it("builds the apply request from the selection, in file order, without preview fields", () => {
    const params = buildApply(hosts, new Set([3, 1, 2, 0]), { mode: "source" }, "update");
    expect(params.hosts.map((entry) => entry.label)).toEqual(["new-a", "old", "legacy"]);
    for (const entry of params.hosts) {
      expect(entry).not.toHaveProperty("exists");
      expect(entry).not.toHaveProperty("problem");
    }
    expect(params.hosts[2].openssh_only).toEqual(["ProxyCommand"]);
    expect(params).toMatchObject({ group_mode: "source", duplicates: "update" });
    expect(params.group_id).toBeUndefined();
  });

  it("passes the target group only for an existing group", () => {
    const chosen = buildApply(hosts, new Set([0]), { mode: "existing", groupId: "g1" }, "skip");
    expect(chosen).toMatchObject({ group_mode: "existing", group_id: "g1" });
    expect(buildApply(hosts, new Set([0]), { mode: "none" }, "skip").group_mode).toBe("none");
  });

  it("round-trips the group select's values", () => {
    for (const choice of [{ mode: "source" }, { mode: "none" }, { mode: "existing", groupId: "a:b" }] as const) {
      expect(decodeChoice(encodeChoice(choice))).toEqual(choice);
    }
    expect(decodeChoice("garbage")).toEqual({ mode: "source" });
  });

  it("describes targets and groups", () => {
    expect(describeTarget(host("a", { username: "root" }))).toBe("root@a.example.com");
    expect(describeTarget(host("a", { port: 2222 }))).toBe("a.example.com:2222");
    expect(describeTarget({ label: "v6", hostname: "2001:db8::2", port: 22 })).toBe("2001:db8::2");
    expect(describeTarget({ label: "v6", hostname: "2001:db8::2", port: 2200, username: "u" })).toBe("u@[2001:db8::2]:2200");
    expect(describeGroup(["Prod", "Web"])).toBe("Prod / Web");
    expect(describeGroup(undefined)).toBe("");
  });
});
