import { describe, expect, it, vi } from "vitest";

// i18n reads the language from the page URL; the tests run in node.
vi.stubGlobal("window", { location: { search: "?lang=en" } });

const { describeTunnel, isSocketPath, validSocketPath } = await import("./tunnels");

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

describe("Unix socket tunnels", () => {
  it("shows a socket path where a host and port would be", () => {
    const toSocket = { ...local, bindPort: 8080, targetHost: "/run/docker.sock", targetPort: null };
    expect(describeTunnel(toSocket)).toBe("8080 → /run/docker.sock");
    expect(describeTunnel({ ...toSocket, bindAddress: "/tmp/docker.sock", bindPort: 0 })).toBe(
      "/tmp/docker.sock → /run/docker.sock",
    );
    expect(
      describeTunnel({ ...local, kind: "remote", bindAddress: "/run/app.sock", bindPort: 0, targetHost: "localhost", targetPort: 3000 }),
    ).toBe("Remote /run/app.sock → localhost:3000");
  });

  it("tells socket paths from hosts and checks them", () => {
    expect(isSocketPath("/run/x.sock")).toBe(true);
    expect(isSocketPath(" /run/x.sock ")).toBe(true);
    expect(isSocketPath("db.internal")).toBe(false);
    expect(isSocketPath(null)).toBe(false);
    expect(validSocketPath("/run/x.sock")).toBe(true);
    expect(validSocketPath("/")).toBe(false);
    expect(validSocketPath("run/x.sock")).toBe(false);
    expect(validSocketPath("/run/a:b")).toBe(false);
    expect(validSocketPath("/run/a\nb")).toBe(false);
  });
});
