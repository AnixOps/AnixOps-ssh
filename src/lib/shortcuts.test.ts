import { describe, expect, it } from "vitest";
import { resolveShortcut, type KeyInput } from "./shortcuts";

const key = (init: Partial<KeyInput> & { key: string }): KeyInput => ({
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  metaKey: false,
  ...init,
});

describe("shortcuts on Windows and Linux", () => {
  const pc = (init: Partial<KeyInput> & { key: string }) => resolveShortcut(key({ ctrlKey: true, shiftKey: true, ...init }), false);

  it("keeps the existing shortcuts", () => {
    expect(pc({ key: "K" })).toBe("search-hosts");
    expect(pc({ key: "T" })).toBe("new-local");
    expect(pc({ key: "W" })).toBe("close-tab");
    expect(pc({ key: "E" })).toBe("toggle-files");
    expect(pc({ key: "F" })).toBe("find");
    expect(pc({ key: "C" })).toBe("copy");
    expect(pc({ key: "V" })).toBe("paste");
  });

  it("adds splits, broadcast and focus", () => {
    expect(pc({ key: "D" })).toBe("split-right");
    expect(pc({ key: "D", altKey: true })).toBe("split-down");
    expect(pc({ key: "∂", code: "KeyD", altKey: true })).toBe("split-down");
    expect(pc({ key: "B" })).toBe("toggle-broadcast");
    expect(pc({ key: "Enter" })).toBe("toggle-maximise");
    expect(pc({ key: "ArrowLeft" })).toBe("focus-left");
    expect(pc({ key: "ArrowUp" })).toBe("focus-up");
  });

  it("leaves plain Ctrl shortcuts to the shell", () => {
    expect(resolveShortcut(key({ key: "d", ctrlKey: true }), false)).toBeNull();
    expect(resolveShortcut(key({ key: "w", ctrlKey: true }), false)).toBeNull();
    expect(resolveShortcut(key({ key: "ArrowLeft", ctrlKey: true }), false)).toBeNull();
    expect(resolveShortcut(key({ key: "B", ctrlKey: true, shiftKey: true, metaKey: true }), false)).toBeNull();
  });

  it("does not treat Alt as part of the other shortcuts", () => {
    expect(pc({ key: "W", altKey: true })).toBeNull();
    expect(pc({ key: "B", altKey: true })).toBeNull();
  });
});

describe("shortcuts on macOS", () => {
  const mac = (init: Partial<KeyInput> & { key: string }) => resolveShortcut(key({ metaKey: true, ...init }), true);

  it("keeps the existing shortcuts", () => {
    expect(mac({ key: "k" })).toBe("search-hosts");
    expect(mac({ key: "t" })).toBe("new-local");
    expect(mac({ key: "w" })).toBe("close-tab");
    expect(mac({ key: "e" })).toBe("toggle-files");
    expect(mac({ key: "f" })).toBe("find");
  });

  it("leaves native copy and paste alone", () => {
    expect(mac({ key: "c" })).toBeNull();
    expect(mac({ key: "v" })).toBeNull();
  });

  it("adds splits, broadcast and focus", () => {
    expect(mac({ key: "d" })).toBe("split-right");
    expect(mac({ key: "D", shiftKey: true })).toBe("split-down");
    expect(mac({ key: "B", shiftKey: true })).toBe("toggle-broadcast");
    expect(mac({ key: "b" })).toBeNull();
    expect(mac({ key: "ArrowRight", altKey: true })).toBe("focus-right");
    expect(mac({ key: "ArrowRight" })).toBeNull();
  });

  it("ignores Ctrl combinations", () => {
    expect(resolveShortcut(key({ key: "d", metaKey: true, ctrlKey: true }), true)).toBeNull();
  });
});
