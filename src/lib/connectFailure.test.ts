import { describe, expect, it } from "vitest";
import { classifyStage, triedMethods } from "./connectFailure";

describe("classifyStage", () => {
  const cases: [string, string][] = [
    // built-in engine
    ["network error: failed to lookup address information: Name or service not known", "dns"],
    ["network error: Connection refused (os error 111)", "connect"],
    ["network error: connection timed out", "connect"],
    ["network error: No route to host (os error 113)", "connect"],
    ["authentication failed (tried: agent, identity file ~/.ssh/id_ed25519)", "auth"],
    ["authentication failed (tried: password); agent is not running", "auth"],
    ["host key of h:22 was rejected (SHA256:abc)", "hostkey"],
    ["host key of h:22 CHANGED (now SHA256:abc, recorded SHA256:def); this could be a man-in-the-middle attack", "hostkey"],
    ["host key of h:22 is revoked (SHA256:abc)", "hostkey"],
    ["the server refused to open a channel: administratively prohibited", "session"],
    ["channel error: closed", "session"],
    ["locked: the vault is locked", "vault"],
    ["cancelled: user cancelled", "cancelled"],
    ["something odd happened", "other"],
    // OpenSSH engine (stderr of ssh)
    ["ssh: Could not resolve hostname nope.invalid: Name or service not known", "dns"],
    ["ssh: connect to host 10.0.0.9 port 22: Connection refused", "connect"],
    ["ssh: connect to host 10.0.0.9 port 22: Connection timed out", "connect"],
    ["ssh: connect to host 10.0.0.9 port 22: Network is unreachable", "connect"],
    ["user@h: Permission denied (publickey,password).", "auth"],
    ["Host key verification failed.", "hostkey"],
  ];
  it.each(cases)("%s -> %s", (message, stage) => {
    expect(classifyStage(message)).toBe(stage);
  });
});

describe("triedMethods", () => {
  it("lists the methods", () => {
    expect(triedMethods("authentication failed (tried: agent, identity file ~/.ssh/id_ed25519)")).toEqual([
      "agent",
      "identity file ~/.ssh/id_ed25519",
    ]);
    expect(triedMethods("authentication failed (tried: password); agent not running")).toEqual(["password"]);
  });
  it("is empty without a list", () => {
    expect(triedMethods("authentication failed (tried: nothing)")).toEqual([]);
    expect(triedMethods("Permission denied (publickey)")).toEqual([]);
  });
});
