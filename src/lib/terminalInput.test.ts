import { describe, expect, it } from "vitest";
import { capabilityReply, OSC52_MAX_BYTES, parseOsc52, sanitizeTitle, shouldOpenLink, TITLE_MAX } from "./terminalInput";

const ctrl = { ctrlKey: true, metaKey: false };
const cmd = { ctrlKey: false, metaKey: true };
const plain = { ctrlKey: false, metaKey: false };

function base64(text: string): string {
  return btoa(String.fromCharCode(...new TextEncoder().encode(text)));
}

describe("shouldOpenLink", () => {
  it("opens web links with Ctrl-click, or ⌘-click on macOS", () => {
    expect(shouldOpenLink("https://example.com/a?b=1", ctrl, false)).toBe(true);
    expect(shouldOpenLink("http://10.0.0.1:8080/", ctrl, false)).toBe(true);
    expect(shouldOpenLink("https://example.com", cmd, true)).toBe(true);
  });

  it("needs the modifier of the platform", () => {
    expect(shouldOpenLink("https://example.com", plain, false)).toBe(false);
    expect(shouldOpenLink("https://example.com", cmd, false)).toBe(false);
    expect(shouldOpenLink("https://example.com", ctrl, true)).toBe(false);
  });

  it("opens nothing but http and https", () => {
    for (const uri of ["file:///etc/passwd", "javascript:alert(1)", "ssh://host", "mailto:a@b.c", "not a url", "http://"]) {
      expect(shouldOpenLink(uri, ctrl, false)).toBe(false);
    }
  });
});

describe("parseOsc52", () => {
  it("decodes a clipboard write as UTF-8", () => {
    expect(parseOsc52(`c;${base64("hello")}`)).toBe("hello");
    expect(parseOsc52(`;${base64("你好 ✓")}`)).toBe("你好 ✓");
    expect(parseOsc52(`pc;${base64("x")}`)).toBe("x");
  });

  it("refuses to answer a clipboard query", () => {
    expect(parseOsc52("c;?")).toBeNull();
  });

  it("refuses malformed or oversized writes", () => {
    expect(parseOsc52("c;")).toBeNull();
    expect(parseOsc52("no separator")).toBeNull();
    expect(parseOsc52("c;not base64!")).toBeNull();
    expect(parseOsc52(`x;${base64("a")}`)).toBeNull();
    expect(parseOsc52(`c;${btoa("\xff\xfe")}`)).toBeNull();
    expect(parseOsc52(`c;${"A".repeat((OSC52_MAX_BYTES / 3) * 4 + 8)}`)).toBeNull();
  });

  it("accepts a write right at the limit", () => {
    const text = "a".repeat(OSC52_MAX_BYTES - 1);
    expect(parseOsc52(`c;${btoa(text)}`)).toBe(text);
  });
});

describe("sanitizeTitle", () => {
  it("strips control and bidi characters", () => {
    expect(sanitizeTitle("user@host: ~/src")).toBe("user@host: ~/src");
    expect(sanitizeTitle("a\x07b\x1b[31mc\u009b")).toBe("a b [31mc");
    expect(sanitizeTitle("evil‮txt.exe")).toBe("evil txt.exe");
    expect(sanitizeTitle("  \t\n ")).toBe("");
  });

  it("caps the length", () => {
    const long = sanitizeTitle("x".repeat(500));
    expect([...long]).toHaveLength(TITLE_MAX);
    expect(long.endsWith("…")).toBe(true);
  });
});

describe("XTGETTCAP replies", () => {
  const hex = (text: string) => Array.from(text, (c) => c.charCodeAt(0).toString(16).padStart(2, "0")).join("");
  it("says 24-bit colour works", () => {
    expect(capabilityReply(hex("Tc"))).toBe("\x1bP1+r5463\x1b\\");
    expect(capabilityReply(`${hex("RGB")};${hex("colors")}`)).toBe("\x1bP1+r524742\x1b\\\x1bP1+r636F6C6F7273=323536\x1b\\");
  });
  it("sends string values with a real escape", () => {
    const reply = capabilityReply(hex("setrgbf"));
    expect(reply.startsWith("\x1bP1+r")).toBe(true);
    expect(reply).toContain("=1B5B33383B323B");
  });
  it("refuses unknown or malformed names", () => {
    expect(capabilityReply(hex("kbs"))).toBe(`\x1bP0+r${hex("kbs")}\x1b\\`);
    expect(capabilityReply("zz")).toBe("\x1bP0+rzz\x1b\\");
    expect(capabilityReply("")).toBe("");
  });
});
