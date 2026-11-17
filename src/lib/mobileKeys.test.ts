import { describe, expect, it } from "vitest";
import { applyCtrl, keySequence } from "./mobileKeys";
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
