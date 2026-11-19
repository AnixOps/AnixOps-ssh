import { describe, expect, it } from "vitest";
import { NO_UPDATES, updateState, type UpdateStatus } from "./update";

const found = { version: "2.2.0", build: 2_002_000, url: "https://github.com/x", notes: "", publishedAt: null };
const base: UpdateStatus = { ...NO_UPDATES, available: true, current: "2.1.0" };

describe("update row state", () => {
  it("is inert without a verification key", () => {
    expect(updateState(NO_UPDATES)).toBe("unavailable");
    expect(updateState({ ...base, available: false, update: found })).toBe("unavailable");
  });

  it("tells a found update, a current build and a first run apart", () => {
    expect(updateState({ ...base, lastChecked: 5, update: found })).toBe("found");
    expect(updateState({ ...base, lastChecked: 5 })).toBe("current");
    expect(updateState(base)).toBe("unchecked");
  });
});
