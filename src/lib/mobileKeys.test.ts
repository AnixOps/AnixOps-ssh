import { describe, expect, it } from "vitest";
import {
  applyCtrl,
  consumeModifier,
  FUNCTION_KEYS,
  keySequence,
  MODIFIER_OFF,
  modifyTyped,
  tapModifier,
  type BarKey,
} from "./mobileKeys";
import { detectMobileShell } from "./platform";

describe("key bar sequences", () => {
  it("sends Esc and Tab as a keyboard would", () => {
    expect(keySequence("esc", false, false)).toBe("\x1b");
    expect(keySequence("tab", false, false)).toBe("\t");
  });

  it("follows the cursor-key mode for arrows", () => {
    expect(keySequence("up", false, false)).toBe("\x1b[A");
    expect(keySequence("up", true, false)).toBe("\x1bOA");
    expect(keySequence("left", false, false)).toBe("\x1b[D");
    expect(keySequence("right", true, false)).toBe("\x1bOC");
    expect(keySequence("down", false, false)).toBe("\x1b[B");
  });

  it("adds the Ctrl modifier to arrows in either mode", () => {
    expect(keySequence("left", false, true)).toBe("\x1b[1;5D");
    expect(keySequence("left", true, true)).toBe("\x1b[1;5D");
  });
});

describe("navigation and function keys", () => {
  it("sends Home, End and the page keys like xterm", () => {
    expect(keySequence("home", false, false)).toBe("\x1b[H");
    expect(keySequence("end", false, false)).toBe("\x1b[F");
    expect(keySequence("home", true, false)).toBe("\x1bOH");
    expect(keySequence("end", true, false)).toBe("\x1bOF");
    expect(keySequence("pgup", false, false)).toBe("\x1b[5~");
    expect(keySequence("pgdn", true, false)).toBe("\x1b[6~");
    expect(keySequence("ins", false, false)).toBe("\x1b[2~");
    expect(keySequence("del", false, false)).toBe("\x1b[3~");
  });

  it("sends F1 to F12 with the sequences of xterm", () => {
    const expected = [
      "\x1bOP", "\x1bOQ", "\x1bOR", "\x1bOS", "\x1b[15~", "\x1b[17~",
      "\x1b[18~", "\x1b[19~", "\x1b[20~", "\x1b[21~", "\x1b[23~", "\x1b[24~",
    ];
    expect(FUNCTION_KEYS.map((key) => keySequence(key, false, false))).toEqual(expected);
    // Function keys do not depend on the cursor-key mode.
    expect(FUNCTION_KEYS.map((key) => keySequence(key, true, false))).toEqual(expected);
  });

  it("adds the modifier parameter: Alt 3, Ctrl 5, both 7", () => {
    expect(keySequence("up", false, { ctrl: false, alt: true })).toBe("\x1b[1;3A");
    expect(keySequence("up", false, { ctrl: true, alt: true })).toBe("\x1b[1;7A");
    expect(keySequence("right", true, false, true)).toBe("\x1b[1;3C");
    expect(keySequence("home", false, { ctrl: true, alt: false })).toBe("\x1b[1;5H");
    expect(keySequence("pgup", false, { ctrl: true, alt: false })).toBe("\x1b[5;5~");
    expect(keySequence("del", false, { ctrl: false, alt: true })).toBe("\x1b[3;3~");
    expect(keySequence("f1", false, { ctrl: true, alt: false })).toBe("\x1b[1;5P");
    expect(keySequence("f5", false, { ctrl: false, alt: true })).toBe("\x1b[15;3~");
    expect(keySequence("f12", false, { ctrl: true, alt: true })).toBe("\x1b[24;7~");
  });

  it("treats Alt on Esc and Tab as the ESC prefix and Ctrl on them as plain", () => {
    expect(keySequence("tab", false, { ctrl: false, alt: true })).toBe("\x1b\t");
    expect(keySequence("esc", false, { ctrl: false, alt: true })).toBe("\x1b\x1b");
    expect(keySequence("tab", false, { ctrl: true, alt: false })).toBe("\t");
  });

  it("covers every key the bar can show", () => {
    const keys: BarKey[] = ["esc", "tab", "up", "down", "left", "right", "home", "end", "pgup", "pgdn", "ins", "del", ...FUNCTION_KEYS];
    for (const key of keys) expect(keySequence(key, false, false)).not.toBe("");
  });
});

describe("modifiers on typed text", () => {
  it("applies Ctrl, Alt or both to a single character", () => {
    expect(modifyTyped("c", { ctrl: true, alt: false })).toBe("\x03");
    expect(modifyTyped("b", { ctrl: false, alt: true })).toBe("\x1bb");
    expect(modifyTyped("f", { ctrl: true, alt: true })).toBe("\x1b\x06");
    expect(modifyTyped("1", { ctrl: true, alt: true })).toBe("\x1b1");
  });

  it("leaves everything alone with no modifier, a digit under Ctrl alone, or longer text", () => {
    expect(modifyTyped("c", { ctrl: false, alt: false })).toBeNull();
    expect(modifyTyped("1", { ctrl: true, alt: false })).toBeNull();
    expect(modifyTyped("你好", { ctrl: true, alt: true })).toBeNull();
    expect(modifyTyped("", { ctrl: true, alt: true })).toBeNull();
  });

  it("keeps one character from an IME as one character", () => {
    expect(modifyTyped("你", { ctrl: false, alt: true })).toBe("\x1b你");
  });
});

describe("sticky modifiers", () => {
  it("arms on one tap, is used up by a key, and a second tap releases it", () => {
    let state = tapModifier(MODIFIER_OFF, 1000);
    expect(state.mode).toBe("armed");
    state = consumeModifier(state);
    expect(state.mode).toBe("off");
    state = tapModifier(tapModifier(MODIFIER_OFF, 1000), 3000);
    expect(state.mode).toBe("off");
  });

  it("locks on a quick double tap, survives keys, and a tap unlocks", () => {
    let state = tapModifier(tapModifier(MODIFIER_OFF, 1000), 1200);
    expect(state.mode).toBe("locked");
    state = consumeModifier(consumeModifier(state));
    expect(state.mode).toBe("locked");
    state = tapModifier(state, 5000);
    expect(state.mode).toBe("off");
  });

  it("does not lock when the second tap is slow", () => {
    const state = tapModifier(tapModifier(MODIFIER_OFF, 1000), 1400);
    expect(state.mode).toBe("off");
  });
});

describe("Ctrl on the next key", () => {
  it("turns letters into control characters", () => {
    expect(applyCtrl("c")).toBe("\x03");
    expect(applyCtrl("C")).toBe("\x03");
    expect(applyCtrl("d")).toBe("\x04");
    expect(applyCtrl("z")).toBe("\x1a");
    expect(applyCtrl("[")).toBe("\x1b");
    expect(applyCtrl(" ")).toBe("\0");
    expect(applyCtrl("?")).toBe("\x7f");
  });

  it("leaves digits, pasted text and escape sequences alone", () => {
    expect(applyCtrl("1")).toBeNull();
    expect(applyCtrl("ls")).toBeNull();
    expect(applyCtrl("\x1b[A")).toBeNull();
    expect(applyCtrl("")).toBeNull();
  });
});

describe("mobile shell detection", () => {
  const desktop = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/126 Safari/537.36";

  it("recognises Android and iOS web views", () => {
    expect(detectMobileShell("Mozilla/5.0 (Linux; Android 14; Pixel 8) Chrome/126 Mobile", 5, "")).toBe(true);
    expect(detectMobileShell("Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X)", 5, "")).toBe(true);
  });

  it("recognises an iPad that reports a Mac", () => {
    expect(detectMobileShell("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 5, "")).toBe(true);
    expect(detectMobileShell("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 0, "")).toBe(false);
  });

  it("leaves the desktop alone and lets the query force either way", () => {
    expect(detectMobileShell(desktop, 0, "")).toBe(false);
    expect(detectMobileShell(desktop, 0, "?mobile=1")).toBe(true);
    expect(detectMobileShell("Mozilla/5.0 (iPhone)", 5, "?mobile=0")).toBe(false);
  });
});
