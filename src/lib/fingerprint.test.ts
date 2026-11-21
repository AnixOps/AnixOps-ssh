import { describe, expect, it } from "vitest";
import { compareFingerprint, keygenCommand, normalizeFingerprint } from "./fingerprint";

const body = "3fA9k2Lq0yQkZf1mJb7Vx2c8Tn5Rw4Hs6Pd0Ue1Gi9o";
const presented = `SHA256:${body}`;

describe("normalizeFingerprint", () => {
  it("accepts with and without prefix, padding and whitespace", () => {
    expect(normalizeFingerprint(presented)).toBe(body);
    expect(normalizeFingerprint(`  ${body}=\n`)).toBe(body);
    expect(normalizeFingerprint(`sha256:${body}`)).toBe(body);
  });
  it("extracts the token from an ssh-keygen line", () => {
    expect(normalizeFingerprint(`256 SHA256:${body} root@host (ED25519)`)).toBe(body);
  });
  it("rejects junk and truncated input", () => {
    expect(normalizeFingerprint("hello")).toBeNull();
    expect(normalizeFingerprint(presented.slice(0, 30))).toBeNull();
  });
});

describe("compareFingerprint", () => {
  it("matches", () => {
    expect(compareFingerprint(presented, `256 SHA256:${body} root@host (ED25519)`)).toBe("match");
    expect(compareFingerprint(presented, body)).toBe("match");
  });
  it("mismatches, including a single changed character", () => {
    expect(compareFingerprint(presented, presented.replace("3fA", "3fB"))).toBe("mismatch");
    expect(compareFingerprint(presented, "garbage")).toBe("mismatch");
  });
  it("is empty for blank input", () => {
    expect(compareFingerprint(presented, "  ")).toBe("empty");
  });
  it("flags MD5 as unsupported", () => {
    expect(compareFingerprint(presented, "MD5:" + Array(16).fill("ab").join(":"))).toBe("md5");
  });
});

describe("keygenCommand", () => {
  it("names the key file for the algorithm", () => {
    expect(keygenCommand("ssh-ed25519")).toContain("ssh_host_ed25519_key.pub");
    expect(keygenCommand("ecdsa-sha2-nistp256")).toContain("ssh_host_ecdsa_key.pub");
    expect(keygenCommand("rsa-sha2-512")).toContain("ssh_host_rsa_key.pub");
    expect(keygenCommand("weird")).toContain("ssh_host_*_key.pub");
  });
});
