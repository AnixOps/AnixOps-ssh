import { describe, expect, it } from "vitest";
import { defaultFindFlags, resultLabel, searchOptions, validRegex } from "./terminalFind";

describe("terminal find", () => {
  it("labels the active match and the count", () => {
    expect(resultLabel("", { index: 0, count: 5 }, "none")).toBe("");
    expect(resultLabel("a", null, "none")).toBe("none");
    expect(resultLabel("a", { index: -1, count: 0 }, "none")).toBe("none");
    expect(resultLabel("a", { index: 2, count: 17 }, "none")).toBe("3/17");
    expect(resultLabel("a", { index: -1, count: 1000 }, "none")).toBe("1000");
  });

  it("passes the toggles and decorations to the addon", () => {
    const decoration = { matchOverviewRuler: "m", activeMatchColorOverviewRuler: "a" };
    const options = searchOptions({ ...defaultFindFlags, regex: true, wholeWord: true }, decoration, true);
    expect(options).toMatchObject({ regex: true, wholeWord: true, caseSensitive: false, incremental: true, decorations: decoration });
  });

  it("detects a broken regex", () => {
    expect(validRegex("a+b")).toBe(true);
    expect(validRegex("(")).toBe(false);
  });
});
