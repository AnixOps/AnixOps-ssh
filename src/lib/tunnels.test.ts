import { describe, expect, it, vi } from "vitest";

// i18n reads the language from the page URL; the tests run in node.
vi.stubGlobal("window", { location: { search: "?lang=en" } });

const { t } = await import("./i18n");
const { isTunnelActive, tunnelWarning } = await import("./tunnels");

describe("tunnel status", () => {
  it("counts a reconnecting tunnel as on, so its switch stops it", () => {
    expect(isTunnelActive({ state: "reconnecting", message: "lost" })).toBe(true);
    expect(isTunnelActive({ state: "running" })).toBe(true);
    expect(isTunnelActive({ state: "starting" })).toBe(true);
    expect(isTunnelActive({ state: "stopped" })).toBe(false);
    expect(isTunnelActive({ state: "failed", message: "x" })).toBe(false);
  });

  it("warns about connections a running tunnel could not pass on", () => {
    expect(tunnelWarning({ state: "running" })).toBeNull();
    expect(tunnelWarning({ state: "running", errorCount: 0, lastError: "x" })).toBeNull();
    expect(tunnelWarning({ state: "failed", message: "x" })).toBeNull();
    const warning = tunnelWarning({ state: "running", errorCount: 3, lastError: "127.0.0.1:1: connect failed" });
    expect(warning).toBe(
      t.tunnelChannelErrors.replace("{count}", "3").replace("{error}", "127.0.0.1:1: connect failed"),
    );
    expect(warning).toBe("3 connections failed: 127.0.0.1:1: connect failed");
  });
});
