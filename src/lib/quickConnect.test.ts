import { describe, expect, it } from "vitest";
import type { Host } from "@/lib/backend";
import { adhocId, findSavedHost, formatTarget, parseAdhocId, parseTarget } from "@/lib/quickConnect";

const t = (user: string | null, host: string, port = 22) => expect.objectContaining({ user, host, port });

describe("parseTarget", () => {
  it("reads host, user@host and user@host:port", () => {
    expect(parseTarget("example.com")).toEqual(t(null, "example.com"));
    expect(parseTarget("root@10.0.0.5")).toEqual(t("root", "10.0.0.5"));
    expect(parseTarget(" deploy@web-1.example.com:2222 ")).toEqual(t("deploy", "web-1.example.com", 2222));
  });

  it("reads IPv6, bracketed or bare", () => {
    expect(parseTarget("[fe80::1]:2200")).toEqual(t(null, "fe80::1", 2200));
    expect(parseTarget("me@[::1]")).toEqual(t("me", "::1"));
    expect(parseTarget("2001:db8::7")).toEqual(t(null, "2001:db8::7"));
    expect(parseTarget("[fe80::1")).toBeNull();
    expect(parseTarget("[zz]")).toBeNull();
    expect(parseTarget("[::1]x")).toBeNull();
  });

  it("reads ssh:// URLs and pasted ssh commands", () => {
    expect(parseTarget("ssh://admin@host.example:2022/")).toEqual(t("admin", "host.example", 2022));
    expect(parseTarget("ssh -p 2222 user@host.example")).toEqual(t("user", "host.example", 2222));
    expect(parseTarget("ssh -p2222 host.example")).toEqual(t(null, "host.example", 2222));
    expect(parseTarget("ssh -l bob host.example")).toEqual(t("bob", "host.example"));
    expect(parseTarget("ssh -p 22 u@h.example:2222")).toBeNull();
  });

  it("refuses options and commands in a pasted line", () => {
    expect(parseTarget("ssh -o ProxyCommand=evil host.example")).toBeNull();
    expect(parseTarget("ssh -oProxyCommand=x host.example")).toBeNull();
    expect(parseTarget("ssh -J jump host.example")).toBeNull();
    expect(parseTarget("ssh host.example ls")).toBeNull();
    expect(parseTarget("ssh://host.example/path")).toBeNull();
  });

  it("refuses bad ports", () => {
    for (const port of ["0", "65536", "99999", "-1", "ab", "22x", ""]) {
      expect(parseTarget(`host.example:${port}`)).toBeNull();
    }
    expect(parseTarget("host.example:65535")).toEqual(t(null, "host.example", 65535));
  });

  it("applies the username rules of the backend", () => {
    expect(parseTarget("a.b@host.example")).toBeNull();
    expect(parseTarget("a@b@host.example")).toBeNull();
    expect(parseTarget("-oX@host.example")).toBeNull();
    expect(parseTarget("@host.example")).toBeNull();
    expect(parseTarget("under_score-1@host.example")).toEqual(t("under_score-1", "host.example"));
  });

  it("refuses unsafe hosts and plain text", () => {
    for (const text of ["-host", "ho st", "a;b.example", "a$b", "host$(x)", "", "  ", "a|b", "`x`"]) {
      expect(parseTarget(text)).toBeNull();
    }
  });

  it("marks a bare word as not explicit", () => {
    expect(parseTarget("web")?.explicit).toBe(false);
    expect(parseTarget("web:22")?.explicit).toBe(true);
    expect(parseTarget("localhost")?.explicit).toBe(true);
    expect(parseTarget("a@web")?.explicit).toBe(true);
  });
});

describe("ids and display", () => {
  it("formats without the default port and brackets IPv6", () => {
    expect(formatTarget({ user: "u", host: "h.example", port: 22 })).toBe("u@h.example");
    expect(formatTarget({ user: null, host: "::1", port: 2200 })).toBe("[::1]:2200");
  });

  it("round-trips through the adhoc id", () => {
    for (const target of [
      { user: "u", host: "h.example", port: 22 },
      { user: null, host: "fe80::1", port: 2200 },
    ]) {
      expect(parseAdhocId(adhocId(target))).toEqual(target);
    }
    expect(adhocId({ user: "u", host: "h.example", port: 22 })).toBe("adhoc:u@h.example:22");
    expect(parseAdhocId("ssh-config:pi")).toBeNull();
  });
});

function host(name: string, hostname: string, username: string | null, port = 22): Host {
  return {
    id: name,
    name,
    fields: { alias: null, hostname, username, port, proxyJump: null, notes: "", shellIntegration: false },
  } as unknown as Host;
}

describe("findSavedHost", () => {
  const hosts = [host("prod", "10.0.0.5", "root", 2222), host("db", "db.example", null)];
  it("matches address, port and given user", () => {
    expect(findSavedHost(hosts, { user: "root", host: "10.0.0.5", port: 2222 })?.id).toBe("prod");
    expect(findSavedHost(hosts, { user: null, host: "10.0.0.5", port: 2222 })?.id).toBe("prod");
    expect(findSavedHost(hosts, { user: "other", host: "10.0.0.5", port: 2222 })).toBeUndefined();
    expect(findSavedHost(hosts, { user: null, host: "10.0.0.5", port: 22 })).toBeUndefined();
    expect(findSavedHost(hosts, { user: null, host: "DB.example", port: 22 })?.id).toBe("db");
  });
});
