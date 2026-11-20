import { describe, expect, it } from "vitest";
import {
  ANIXOPS_NET_PROXY,
  cleanProxy,
  isAnixopsNet,
  newProxy,
  proxyLabel,
  proxyReady,
  withKind,
} from "./proxyUi";

describe("proxy settings in the host editor", () => {
  it("the AnixOps Net preset is the local SOCKS5 listener", () => {
    expect(ANIXOPS_NET_PROXY).toEqual({ kind: "socks5", host: "127.0.0.1", port: 7890, username: null });
    expect(isAnixopsNet(ANIXOPS_NET_PROXY)).toBe(true);
    expect(isAnixopsNet({ ...ANIXOPS_NET_PROXY, port: 7891 })).toBe(false);
    expect(isAnixopsNet({ ...ANIXOPS_NET_PROXY, kind: "http" })).toBe(false);
    expect(isAnixopsNet(null)).toBe(false);
  });

  it("picking a kind starts empty with the usual port, and switching keeps a typed port", () => {
    expect(newProxy("socks5")).toEqual({ kind: "socks5", host: "", port: 1080, username: null });
    expect(newProxy("http").port).toBe(8080);
    const typed = { ...newProxy("socks5"), host: "p", port: 7890 };
    expect(withKind(typed, "http").port).toBe(7890);
    expect(withKind(newProxy("socks5"), "http").port).toBe(8080);
  });

  it("is ready with an address and a port in range", () => {
    expect(proxyReady(null)).toBe(true);
    expect(proxyReady({ ...newProxy("http"), host: " " })).toBe(false);
    expect(proxyReady({ ...newProxy("http"), host: "p", port: 0 })).toBe(false);
    expect(proxyReady({ ...newProxy("http"), host: "p", port: 65536 })).toBe(false);
    expect(proxyReady({ ...newProxy("http"), host: "p", port: 3128 })).toBe(true);
  });

  it("saves trimmed values and no login for a blank user", () => {
    expect(cleanProxy(null)).toBeNull();
    expect(cleanProxy({ kind: "http", host: " p ", port: 3128, username: "  " })).toEqual({
      kind: "http",
      host: "p",
      port: 3128,
      username: null,
    });
    expect(cleanProxy({ kind: "socks5", host: "p", port: 1080, username: " me " })?.username).toBe("me");
  });

  it("labels an address without the login, with brackets for IPv6", () => {
    expect(proxyLabel(ANIXOPS_NET_PROXY)).toBe("socks5://127.0.0.1:7890");
    expect(proxyLabel({ kind: "http", host: "::1", port: 3128, username: "me" })).toBe("http://[::1]:3128");
  });
});
