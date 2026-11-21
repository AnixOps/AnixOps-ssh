import { describe, expect, it, vi } from "vitest";

// i18n reads the language from the page URL; the tests run in node.
vi.stubGlobal("window", { location: { search: "?lang=en" } });

const { environmentMatches, environmentName, environmentTone, environmentsById, isProduction } = await import("./environment");

describe("host environments", () => {
  it("only production is guarded", () => {
    expect(isProduction("production")).toBe(true);
    expect(isProduction("staging")).toBe(false);
    expect(isProduction(null)).toBe(false);
    expect(isProduction(undefined)).toBe(false);
  });

  it("tones: danger, warning, none", () => {
    expect(environmentTone("production")).toBe("danger");
    expect(environmentTone("staging")).toBe("warning");
    expect(environmentTone("testing")).toBeNull();
    expect(environmentTone(null)).toBeNull();
  });

  it("search finds a host by the shown or the stored name", () => {
    expect(environmentMatches("production", "prod")).toBe(true);
    expect(environmentMatches("production", environmentName("production").toLowerCase())).toBe(true);
    expect(environmentMatches("staging", "prod")).toBe(false);
    expect(environmentMatches(null, "prod")).toBe(false);
    expect(environmentMatches("production", "")).toBe(false);
    expect(environmentName("production")).toBe("Production");
  });

  it("maps host ids to marked environments only", () => {
    const map = environmentsById([
      { id: "a", environment: "production" },
      { id: "b", environment: null },
      { id: "c" },
    ]);
    expect([...map]).toEqual([["a", "production"]]);
  });
});
