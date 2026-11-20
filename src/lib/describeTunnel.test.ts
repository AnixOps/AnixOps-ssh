import { describe, expect, it, vi } from "vitest";

// i18n reads the language from the page URL; the tests run in node.
vi.stubGlobal("window", { location: { search: "?lang=en" } });

const { describeTunnel } = await import("./tunnels");

const local = {
  kind: "local" as const,
  bindAddress: null,
  bindPort: 15432,
  targetHost: "db.internal",
  targetPort: 5432,
};

describe("describeTunnel", () => {
  it("shows the port a tunnel saved with port 0 got while it runs", () => {
    expect(describeTunnel(local)).toBe("15432 → db.internal:5432");
    expect(describeTunnel({ ...local, bindPort: 0 })).toBe("auto → db.internal:5432");
    expect(describeTunnel({ ...local, bindPort: 0, status: { state: "running", port: 40123 } })).toBe(
      "40123 → db.internal:5432",
    );
    expect(describeTunnel({ ...local, kind: "dynamic", bindPort: 0, status: { state: "running", port: 1081 } })).toBe(
      "SOCKS 1081",
    );
  });

  it("brackets IPv6 addresses once", () => {
    expect(describeTunnel({ ...local, bindAddress: "::1", targetHost: "fd00::5" })).toBe("[::1]:15432 → [fd00::5]:5432");
    expect(describeTunnel({ ...local, bindAddress: "[::1]" })).toBe("[::1]:15432 → db.internal:5432");
  });
});
