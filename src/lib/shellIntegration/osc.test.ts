import { describe, expect, it } from "vitest";
import { parseIntegrationOsc, percentDecode } from "./osc";

describe("percentDecode", () => {
  it("decodes UTF-8 byte by byte", () => {
    expect(percentDecode("echo%20hi")).toBe("echo hi");
    expect(percentDecode("/home/%E7%94%A8%E6%88%B7/a%20b")).toBe("/home/用户/a b");
    expect(percentDecode("line%0Aline")).toBe("line\nline");
  });

  it("keeps malformed escapes and plain text as they are", () => {
    expect(percentDecode("100%")).toBe("100%");
    expect(percentDecode("%zz%2")).toBe("%zz%2");
    expect(percentDecode("已经没有转义")).toBe("已经没有转义");
    expect(percentDecode("")).toBe("");
  });

  it("does not throw on invalid UTF-8", () => {
    expect(percentDecode("%FF%FEok")).toBe("��ok");
  });
});

describe("parseIntegrationOsc", () => {
  it("parses each kind the scripts print", () => {
    expect(parseIntegrationOsc("R;bash")).toEqual({ kind: "ready", shell: "bash" });
    expect(parseIntegrationOsc("A")).toEqual({ kind: "promptStart" });
    expect(parseIntegrationOsc("B")).toEqual({ kind: "promptEnd" });
    expect(parseIntegrationOsc("C;cd%20/tmp")).toEqual({ kind: "commandStart", command: "cd /tmp" });
    expect(parseIntegrationOsc("D;1")).toEqual({ kind: "commandEnd", exitCode: 1 });
    expect(parseIntegrationOsc("D;0")).toEqual({ kind: "commandEnd", exitCode: 0 });
    expect(parseIntegrationOsc("P;/tmp/a%20b")).toEqual({ kind: "cwd", path: "/tmp/a b" });
  });

  it("keeps semicolons inside the payload", () => {
    expect(parseIntegrationOsc("C;a;b;c")).toEqual({ kind: "commandStart", command: "a;b;c" });
  });

  it("tolerates a missing or odd exit code", () => {
    expect(parseIntegrationOsc("D")).toEqual({ kind: "commandEnd", exitCode: null });
    expect(parseIntegrationOsc("D;")).toEqual({ kind: "commandEnd", exitCode: null });
    expect(parseIntegrationOsc("D;abc")).toEqual({ kind: "commandEnd", exitCode: null });
    expect(parseIntegrationOsc("D;130")).toEqual({ kind: "commandEnd", exitCode: 130 });
  });

  it("ignores unknown kinds and empty directories", () => {
    expect(parseIntegrationOsc("Z;x")).toBeNull();
    expect(parseIntegrationOsc("")).toBeNull();
    expect(parseIntegrationOsc("P;")).toBeNull();
  });
});
