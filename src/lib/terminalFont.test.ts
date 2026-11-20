import { describe, expect, it } from "vitest";
import {
  clampFont,
  defaultFontStack,
  defaultTerminalFont,
  fontOptions,
  platformOf,
  sanitizeFamily,
  terminalFontFamily,
  zoomedSize,
} from "./terminalFont";

describe("terminal font", () => {
  it("picks a stack per system, always ending in monospace", () => {
    expect(platformOf("MacIntel")).toBe("mac");
    expect(platformOf("Win32")).toBe("windows");
    expect(platformOf("Linux x86_64")).toBe("linux");
    for (const platform of ["mac", "windows", "linux"] as const) {
      expect(defaultFontStack(platform).endsWith("monospace")).toBe(true);
    }
  });

  it("puts the chosen family first and keeps the fallback", () => {
    const font = { ...defaultTerminalFont, family: "JetBrains Mono" };
    expect(terminalFontFamily(font, "linux", "")).toBe(`"JetBrains Mono", ${defaultFontStack("linux")}`);
    expect(terminalFontFamily(font, "linux", "tokenMono")).toBe('"JetBrains Mono", tokenMono');
    expect(terminalFontFamily({ ...font, family: '"Fira Code", Hack' }, "mac", "")).toMatch(/^"Fira Code", Hack, /);
    expect(terminalFontFamily(defaultTerminalFont, "mac", "")).toBe(defaultFontStack("mac"));
  });

  it("strips characters that could break out of the CSS value", () => {
    expect(sanitizeFamily("Hack; color: red{}")).toBe("Hack color: red");
    expect(sanitizeFamily("  \u0000Hack\n ")).toBe("Hack");
  });

  it("clamps line height and letter spacing", () => {
    expect(clampFont({ family: "", lineHeight: 5, letterSpacing: 99 })).toEqual({ family: "", lineHeight: 2, letterSpacing: 10 });
    expect(clampFont({ family: "", lineHeight: 0, letterSpacing: -9 })).toEqual({ family: "", lineHeight: 1, letterSpacing: -2 });
    expect(clampFont({ family: "", lineHeight: NaN, letterSpacing: 1.4 }).lineHeight).toBe(1);
  });

  it("zooms within the allowed sizes", () => {
    expect(zoomedSize(13, "zoom-in")).toBe(14);
    expect(zoomedSize(9, "zoom-out")).toBe(9);
    expect(zoomedSize(32, "zoom-in")).toBe(32);
    expect(zoomedSize(20, "zoom-reset")).toBe(13);
  });

  it("builds the xterm options", () => {
    const options = fontOptions({ terminalFontSize: 15, terminalFont: { family: "", lineHeight: 1.4, letterSpacing: 1 } }, "mac", "");
    expect(options).toEqual({ fontFamily: defaultFontStack("mac"), fontSize: 15, lineHeight: 1.4, letterSpacing: 1 });
  });
});
