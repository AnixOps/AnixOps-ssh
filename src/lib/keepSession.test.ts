import { describe, expect, it, vi } from "vitest";
import { attachCommand, claimSlot, hostKeepSession, keepSessionName, reconnectDivider, setHostKeepSession } from "./keepSession";

// The strings module reads the page's language when it loads; there is no page here.
vi.hoisted(() => {
  const store = new Map<string, string>();
  Object.assign(globalThis, {
    window: { location: { search: "?lang=en" } },
    localStorage: {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => void store.set(key, value),
    },
  });
});

describe("keep session", () => {
  it("names the first terminal like the touch app and numbers the rest", () => {
    expect(keepSessionName(1)).toBe("anixops");
    expect(keepSessionName(2)).toBe("anixops-2");
  });

  it("hands out the lowest free slot and reuses released ones", () => {
    const a = claimSlot("h1");
    const b = claimSlot("h1");
    expect([a.slot, b.slot]).toEqual([1, 2]);
    expect(claimSlot("h2").slot).toBe(1);
    a.release();
    expect(claimSlot("h1").slot).toBe(1);
  });

  it("types an exec guarded by a presence check, with a leading space", () => {
    const tmux = attachCommand("tmux", "anixops");
    expect(tmux.startsWith(" command -v tmux")).toBe(true);
    expect(tmux).toContain("exec tmux new -A -s anixops");
    expect(tmux.endsWith("\r")).toBe(true);
    expect(attachCommand("screen", "anixops-2")).toContain("exec screen -dRR anixops-2");
  });

  it("writes a dim divider with the local time, and says what happened to the programs", () => {
    const at = new Date(2026, 0, 2, 14, 32);
    const fresh = reconnectDivider(at, "off");
    expect(fresh.startsWith("\x1b[2m")).toBe(true);
    expect(fresh).toContain("14:32");
    expect(fresh).not.toContain("{");
    expect(fresh.endsWith("\x1b[0m\r\n")).toBe(true);
    expect(reconnectDivider(at, "tmux")).toContain("tmux");
    expect(reconnectDivider(at, "tmux")).not.toBe(fresh);
  });

  it("remembers the choice per host", () => {
    expect(hostKeepSession("hx")).toBe("off");
    setHostKeepSession("hx", "screen");
    expect(hostKeepSession("hx")).toBe("screen");
    setHostKeepSession("hx", "off");
    expect(hostKeepSession("hx")).toBe("off");
  });
});
