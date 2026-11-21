import { describe, expect, it, vi } from "vitest";
import { resolveGlobalShortcut, resolveShortcut, type KeyInput } from "./shortcuts";

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
    expect(pc({ key: "O" })).toBe("split-down");
    expect(pc({ key: "o", code: "KeyO" })).toBe("split-down");
    expect(pc({ key: "B" })).toBe("toggle-broadcast");
    expect(pc({ key: "Enter" })).toBe("toggle-maximise");
    expect(pc({ key: "ArrowLeft" })).toBe("focus-left");
    expect(pc({ key: "ArrowUp" })).toBe("focus-up");
  });

  it("jumps between prompts with the page keys, not the pane-focus arrows", () => {
    expect(pc({ key: "PageUp" })).toBe("prev-prompt");
    expect(pc({ key: "PageDown" })).toBe("next-prompt");
    expect(pc({ key: "ArrowUp" })).toBe("focus-up");
    expect(resolveShortcut(key({ key: "PageUp", ctrlKey: true }), false)).toBeNull();
    expect(pc({ key: "PageUp", altKey: true })).toBeNull();
  });

  it("leaves plain Ctrl shortcuts to the shell", () => {
    expect(resolveShortcut(key({ key: "d", ctrlKey: true }), false)).toBeNull();
    expect(resolveShortcut(key({ key: "w", ctrlKey: true }), false)).toBeNull();
    expect(resolveShortcut(key({ key: "ArrowLeft", ctrlKey: true }), false)).toBeNull();
    expect(resolveShortcut(key({ key: "B", ctrlKey: true, shiftKey: true, metaKey: true }), false)).toBeNull();
  });

  it("does not treat Alt as part of the other shortcuts", () => {
    expect(pc({ key: "W", altKey: true })).toBeNull();
    expect(pc({ key: "D", altKey: true })).toBeNull();
    expect(pc({ key: "B", altKey: true })).toBeNull();
  });
});

describe("shortcuts on macOS", () => {
  const mac = (init: Partial<KeyInput> & { key: string }) => resolveShortcut(key({ metaKey: true, ...init }), true);

  it("jumps between prompts with Cmd+Up and Cmd+Down", () => {
    expect(mac({ key: "ArrowUp" })).toBe("prev-prompt");
    expect(mac({ key: "ArrowDown" })).toBe("next-prompt");
    expect(mac({ key: "ArrowUp", altKey: true })).toBe("focus-up");
    expect(mac({ key: "ArrowLeft" })).toBeNull();
    expect(mac({ key: "ArrowUp", shiftKey: true })).toBeNull();
  });

  it("keeps the existing shortcuts", () => {
    expect(mac({ key: "K", shiftKey: true })).toBe("search-hosts");
    expect(mac({ key: "k" })).toBeNull();
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

describe("reserved app shortcuts", () => {
  const pc = (init: Partial<KeyInput> & { key: string }, inTerminal: boolean) =>
    resolveGlobalShortcut(key({ ctrlKey: true, shiftKey: inTerminal, ...init }), false, inTerminal);
  const mac = (init: Partial<KeyInput> & { key: string }) => resolveGlobalShortcut(key({ metaKey: true, ...init }), true, false);

  it("uses plain Ctrl outside the terminal", () => {
    expect(pc({ key: "k" }, false)).toBe("search-hosts");
    expect(pc({ key: "/" }, false)).toBe("shortcut-list");
    expect(pc({ key: ",", code: "Comma" }, false)).toBe("settings");
    expect(pc({ key: "1", code: "Digit1" }, false)).toBe("go-hosts");
    expect(pc({ key: "4", code: "Digit4" }, false)).toBe("go-team");
    expect(pc({ key: "i" }, false)).toBe("import");
    expect(pc({ key: "n" }, false)).toBe("new-host");
    expect(pc({ key: "f" }, false)).toBe("find");
  });

  it("leaves plain Ctrl to the shell in the terminal and uses Ctrl+Shift instead", () => {
    expect(resolveGlobalShortcut(key({ key: "k", ctrlKey: true }), false, true)).toBeNull();
    expect(resolveGlobalShortcut(key({ key: "n", ctrlKey: true }), false, true)).toBeNull();
    expect(pc({ key: "K" }, true)).toBe("search-hosts");
    expect(pc({ key: "?", code: "Slash" }, true)).toBe("shortcut-list");
    expect(pc({ key: "<", code: "Comma" }, true)).toBe("settings");
    expect(pc({ key: "!", code: "Digit1" }, true)).toBe("go-hosts");
  });

  it("takes the Ctrl+Shift form outside the terminal too, so the shown shortcut works everywhere", () => {
    const shift = (init: Partial<KeyInput> & { key: string }) =>
      resolveGlobalShortcut(key({ ctrlKey: true, shiftKey: true, ...init }), false, false);
    expect(shift({ key: "?", code: "Slash" })).toBe("shortcut-list");
    expect(shift({ key: "<", code: "Comma" })).toBe("settings");
    expect(shift({ key: "!", code: "Digit1" })).toBe("go-hosts");
    expect(shift({ key: "N" })).toBe("new-host");
    expect(shift({ key: "S", code: "KeyS" })).toBe("toggle-sidebar");
    // The palette's Ctrl+Shift+K is a terminal command, so it works everywhere.
    expect(resolveShortcut(key({ key: "K", ctrlKey: true, shiftKey: true }), false)).toBe("search-hosts");
  });

  it("shows one shortcut per command, the form that works everywhere", async () => {
    vi.stubGlobal("window", { location: { search: "?lang=en" } });
    const { shortcutTable } = await import("./shortcutTable");
    for (const row of shortcutTable) {
      if (row.pcOutside) expect(row.pc).toMatch(/^Ctrl\+Shift\+/);
    }
    expect(shortcutTable.find((row) => row.id === "search-hosts")).toMatchObject({ pc: "Ctrl+Shift+K", mac: "⌘⇧K" });
  });

  it("clears the terminal with Cmd+K on macOS and moves the palette to Cmd+Shift+K there", () => {
    const k = key({ key: "k", metaKey: true });
    expect(resolveGlobalShortcut(k, true, true)).toBe("clear-terminal");
    expect(resolveGlobalShortcut(k, true, false)).toBe("search-hosts");
    expect(mac({ key: "K", shiftKey: true })).toBeNull();
    expect(resolveShortcut(key({ key: "K", metaKey: true, shiftKey: true }), true)).toBe("search-hosts");
  });

  it("never uses Ctrl+Alt", () => {
    expect(pc({ key: "k", altKey: true }, false)).toBeNull();
    expect(pc({ key: "k", altKey: true }, true)).toBeNull();
  });

  it("uses Cmd on macOS", () => {
    expect(mac({ key: "/" })).toBe("shortcut-list");
    expect(mac({ key: ",", code: "Comma" })).toBe("settings");
    expect(mac({ key: "2", code: "Digit2" })).toBe("go-forwards");
    expect(mac({ key: "i" })).toBe("import");
    expect(mac({ key: "n" })).toBe("new-host");
  });
});

describe("tabs, zoom and the sidebar", () => {
  it("switches tabs with Ctrl+Tab and Ctrl+Shift+Tab everywhere", () => {
    for (const mac of [false, true]) {
      expect(resolveShortcut(key({ key: "Tab", ctrlKey: true }), mac)).toBe("next-tab");
      expect(resolveShortcut(key({ key: "Tab", ctrlKey: true, shiftKey: true }), mac)).toBe("prev-tab");
    }
    expect(resolveShortcut(key({ key: "}", code: "BracketRight", metaKey: true, shiftKey: true }), true)).toBe("next-tab");
    expect(resolveShortcut(key({ key: "{", code: "BracketLeft", metaKey: true, shiftKey: true }), true)).toBe("prev-tab");
    expect(resolveShortcut(key({ key: "Tab" }), false)).toBeNull();
  });

  it("zooms with Ctrl+= / Ctrl+- / Ctrl+0, and Cmd on macOS", () => {
    expect(resolveShortcut(key({ key: "=", code: "Equal", ctrlKey: true }), false)).toBe("zoom-in");
    expect(resolveShortcut(key({ key: "+", code: "Equal", ctrlKey: true, shiftKey: true }), false)).toBe("zoom-in");
    expect(resolveShortcut(key({ key: "+", code: "NumpadAdd", ctrlKey: true }), false)).toBe("zoom-in");
    expect(resolveShortcut(key({ key: "-", code: "Minus", ctrlKey: true }), false)).toBe("zoom-out");
    expect(resolveShortcut(key({ key: "0", code: "Digit0", ctrlKey: true }), false)).toBe("zoom-reset");
    expect(resolveShortcut(key({ key: "=", code: "Equal", metaKey: true }), true)).toBe("zoom-in");
    expect(resolveShortcut(key({ key: "-", code: "Minus", metaKey: true }), true)).toBe("zoom-out");
    expect(resolveShortcut(key({ key: "0", code: "Digit0", metaKey: true }), true)).toBe("zoom-reset");
  });

  it("leaves readline's Ctrl+_ (undo) and Ctrl+Alt to the shell", () => {
    expect(resolveShortcut(key({ key: "_", code: "Minus", ctrlKey: true, shiftKey: true }), false)).toBeNull();
    expect(resolveShortcut(key({ key: "=", code: "Equal", ctrlKey: true, altKey: true }), false)).toBeNull();
    expect(resolveShortcut(key({ key: "=", code: "Equal", ctrlKey: true }), true)).toBeNull();
  });

  it("toggles the sidebar with Ctrl+B outside the terminal and ⌃⌘S on macOS", () => {
    expect(resolveGlobalShortcut(key({ key: "b", ctrlKey: true }), false, false)).toBe("toggle-sidebar");
    // In a terminal Ctrl+B is the shell's and tmux's.
    expect(resolveGlobalShortcut(key({ key: "b", ctrlKey: true }), false, true)).toBeNull();
    expect(resolveGlobalShortcut(key({ key: "s", code: "KeyS", ctrlKey: true, metaKey: true }), true, true)).toBe("toggle-sidebar");
    // …and Ctrl+Shift+S from inside one.
    expect(resolveGlobalShortcut(key({ key: "S", code: "KeyS", ctrlKey: true, shiftKey: true }), false, true)).toBe("toggle-sidebar");
    expect(resolveGlobalShortcut(key({ key: "s", code: "KeyS", ctrlKey: true }), false, true)).toBeNull();
  });

  it("toggles focus mode with F11, ⌃⌘Z on macOS, in the terminal too", () => {
    expect(resolveGlobalShortcut(key({ key: "F11", code: "F11" }), false, true)).toBe("toggle-focus");
    expect(resolveGlobalShortcut(key({ key: "F11", code: "F11" }), false, false)).toBe("toggle-focus");
    expect(resolveGlobalShortcut(key({ key: "F11", code: "F11", shiftKey: true }), false, true)).toBeNull();
    expect(resolveGlobalShortcut(key({ key: "z", code: "KeyZ", ctrlKey: true, metaKey: true }), true, true)).toBe("toggle-focus");
    expect(resolveGlobalShortcut(key({ key: "z", code: "KeyZ", metaKey: true }), true, true)).toBeNull();
  });
});
