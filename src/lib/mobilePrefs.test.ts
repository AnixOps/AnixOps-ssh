import { describe, expect, it } from "vitest";
import { defaultLayout } from "./barLayout";
import { DEFAULT_FONT_SIZE, defaultPrefs, isCjkLanguage, sanitisePrefs } from "./mobilePrefs";

describe("mobile preferences", () => {
  it("defaults when nothing is stored", () => {
    expect(sanitisePrefs(null)).toEqual(defaultPrefs());
    expect(sanitisePrefs("junk")).toEqual(defaultPrefs());
    expect(defaultPrefs().fontSize).toBe(DEFAULT_FONT_SIZE);
    expect(defaultPrefs().bar).toEqual(defaultLayout());
  });

  it("keeps good values and repairs bad ones one by one", () => {
    const prefs = sanitisePrefs({
      fontSize: 99,
      haptics: "yes",
      imeMode: "hold",
      graceSeconds: 7,
      keepaliveSeconds: 15,
      foregroundService: true,
      attach: { a: "tmux", b: "screen", c: "off", d: "zsh" },
    });
    expect(prefs.fontSize).toBe(32);
    expect(prefs.haptics).toBe(true);
    expect(prefs.imeMode).toBe("hold");
    expect(prefs.graceSeconds).toBe(60);
    expect(prefs.keepaliveSeconds).toBe(15);
    expect(prefs.foregroundService).toBe(true);
    expect(prefs.attach).toEqual({ a: "tmux", b: "screen" });
  });

  it("accepts only the offered background periods, including 'as long as the system allows'", () => {
    for (const seconds of [0, 30, 60, 300, 900, -1]) expect(sanitisePrefs({ graceSeconds: seconds }).graceSeconds).toBe(seconds);
    expect(sanitisePrefs({ graceSeconds: 61 }).graceSeconds).toBe(60);
  });

  it("clamps and rounds the font size", () => {
    expect(sanitisePrefs({ fontSize: 3 }).fontSize).toBe(8);
    expect(sanitisePrefs({ fontSize: 13.6 }).fontSize).toBe(14);
    expect(sanitisePrefs({ fontSize: Number.NaN }).fontSize).toBe(DEFAULT_FONT_SIZE);
  });

  it("repairs a damaged key bar", () => {
    expect(sanitisePrefs({ bar: { primary: ["nonsense"] } }).bar).toEqual(defaultLayout());
  });

  it("recognises the languages whose keyboards compose text", () => {
    for (const language of ["zh-CN", "zh", "ja-JP", "ko-KR"]) expect(isCjkLanguage(language), language).toBe(true);
    for (const language of ["en-US", "de", "fr-CA", "", "zhuang"]) expect(isCjkLanguage(language), language).toBe(false);
  });
});
