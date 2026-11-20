import { Unicode11Addon } from "@xterm/addon-unicode11";
import { Terminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";
import { liveTerminalOptions, terminalOptions, UNICODE_VERSION, type TerminalSettings } from "./terminalOptions";

const defaults: TerminalSettings = { terminalFontSize: 13, terminalScrollback: 10000, terminalOptionAsMeta: false };
const input = { fontFamily: "mono", theme: {} };

describe("terminalOptions", () => {
  it("leaves Option to the keyboard layout by default", () => {
    const options = terminalOptions(defaults, input);
    expect(options.macOptionIsMeta).toBe(false);
    expect(options.macOptionClickForcesSelection).toBe(true);
    expect(options.allowProposedApi).toBe(true);
    expect(options.scrollback).toBe(10000);
    expect(options.fontSize).toBe(13);
    expect(options.screenReaderMode).toBe(false);
  });

  it("follows the settings", () => {
    const current = { terminalFontSize: 15, terminalScrollback: 50000, terminalOptionAsMeta: true };
    const options = terminalOptions(current, input);
    expect(options.macOptionIsMeta).toBe(true);
    expect(options.scrollback).toBe(50000);
    expect(options.fontSize).toBe(15);
    expect(liveTerminalOptions(current)).toEqual({ scrollback: 50000, macOptionIsMeta: true });
  });

  it("lets a view choose its own font size and screen reader mode", () => {
    const options = terminalOptions(defaults, { ...input, fontSize: 18, screenReaderMode: true });
    expect(options.fontSize).toBe(18);
    expect(options.screenReaderMode).toBe(true);
    expect(options.scrollback).toBe(10000);
  });
});

describe("character width", () => {
  const cursorAfter = (text: string, unicode11: boolean) =>
    new Promise<number>((resolve) => {
      const terminal = new Terminal({ cols: 40, rows: 4, allowProposedApi: true });
      if (unicode11) {
        terminal.loadAddon(new Unicode11Addon());
        terminal.unicode.activeVersion = UNICODE_VERSION;
      }
      terminal.write(text, () => {
        resolve(terminal.buffer.active.cursorX);
        terminal.dispose();
      });
    });

  it("counts CJK as two cells", async () => {
    expect(await cursorAfter("你好", true)).toBe(4);
  });

  it("counts emoji as two cells with Unicode 11, as the remote's wcwidth does", async () => {
    expect(await cursorAfter("\u{1F600}a", false)).toBe(2);
    expect(await cursorAfter("\u{1F600}a", true)).toBe(3);
  });
});
