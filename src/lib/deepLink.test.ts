import { describe, expect, it } from "vitest";
import type { Host } from "@/lib/backend";
import { parseLink, resolveLink } from "@/lib/deepLink";

describe("parseLink", () => {
  it("reads connect?host= and a bare target", () => {
    expect(parseLink("anixops-ssh://connect?host=prod%20db")).toEqual({ kind: "host", ref: "prod db" });
    expect(parseLink("anixops-ssh://connect/?host=3")).toEqual({ kind: "host", ref: "3" });
    expect(parseLink("anixops-ssh://root@10.0.0.5:2222")).toEqual({
      kind: "target",
      target: { user: "root", host: "10.0.0.5", port: 2222 },
    });
    expect(parseLink("anixops-ssh://[::1]:2200/")).toEqual({
      kind: "target",
      target: { user: null, host: "::1", port: 2200 },
    });
  });

  it("refuses extra options, paths and other schemes", () => {
    for (const link of [
      "anixops-ssh://connect?host=a&command=ls",
      "anixops-ssh://connect?host=a&o=ProxyCommand%3Dx",
      "anixops-ssh://connect?command=ls",
      "anixops-ssh://connect?host=",
      "anixops-ssh://host.example?o=1",
      "anixops-ssh://host.example/path",
      "anixops-ssh://host.example#x",
      "anixops-ssh://-oProxyCommand=x@host.example",
      "anixops-ssh://host.example:99999",
      "anixops-ssh://a.b@host.example",
      "anixops-ssh://",
      "ssh://host.example",
      "https://host.example",
    ]) {
      expect(parseLink(link), link).toBeNull();
    }
  });
});

function host(id: string, name: string, hostname: string, username: string | null, port = 22): Host {
  return {
    id,
    name,
    fields: { alias: null, hostname, username, port, proxyJump: null, notes: "", shellIntegration: false },
  } as unknown as Host;
}

describe("resolveLink", () => {
  const hosts = [host("1", "prod", "10.0.0.5", "root", 2222), host("2", "Dup", "a.example", null), host("3", "dup", "b.example", null)];
  it("opens a saved host by id, name or address", () => {
    expect(resolveLink({ kind: "host", ref: "1" }, hosts)).toMatchObject({ action: "open" });
    expect(resolveLink({ kind: "host", ref: "PROD" }, hosts)).toMatchObject({ action: "open" });
    expect(resolveLink({ kind: "target", target: { user: "root", host: "10.0.0.5", port: 2222 } }, hosts)).toMatchObject({ action: "open" });
  });

  it("always asks for an address that is not saved", () => {
    expect(resolveLink({ kind: "target", target: { user: "root", host: "10.0.0.5", port: 22 } }, hosts).action).toBe("confirm");
    expect(resolveLink({ kind: "host", ref: "x.example" }, hosts).action).toBe("confirm");
  });

  it("does not guess between duplicate names or unknown words", () => {
    expect(resolveLink({ kind: "host", ref: "dup" }, hosts).action).toBe("unknown");
    expect(resolveLink({ kind: "host", ref: "nothing" }, hosts).action).toBe("unknown");
  });
});
