import { describe, expect, it } from "vitest";
import { formatOctal, formatSymbolic, hasBit, parseModeString, parseOctal, setBit } from "./filePermissions";

describe("reading a mode string", () => {
  it("reads the nine permission columns", () => {
    expect(parseModeString("-rwxr-xr-x")).toBe(0o755);
    expect(parseModeString("drw-r-----")).toBe(0o640);
    expect(parseModeString("lrwxrwxrwx")).toBe(0o777);
    expect(parseModeString("----------")).toBe(0);
  });

  it("reads setuid, setgid and sticky", () => {
    expect(parseModeString("-rwsr-xr-x")).toBe(0o4755);
    expect(parseModeString("-rwSr--r--")).toBe(0o4644);
    expect(parseModeString("drwxr-sr-x")).toBe(0o2755);
    expect(parseModeString("drwxrwxrwt")).toBe(0o1777);
    expect(parseModeString("drwxrwxrwT")).toBe(0o1776);
  });

  it("refuses text that is not a mode", () => {
    expect(parseModeString("")).toBeNull();
    expect(parseModeString("rwxr-xr-x")).toBeNull();
    expect(parseModeString("-rwxr-xr-xx")).toBeNull();
    expect(parseModeString("-rwqr-xr-x")).toBeNull();
  });
});

describe("octal text", () => {
  it("parses one to four octal digits", () => {
    expect(parseOctal("755")).toBe(0o755);
    expect(parseOctal(" 0644 ")).toBe(0o644);
    expect(parseOctal("4755")).toBe(0o4755);
    expect(parseOctal("7")).toBe(7);
  });

  it("refuses anything else", () => {
    for (const bad of ["", "8", "789", "75a", "-1", "12345", "0x1f", "7 5"]) expect(parseOctal(bad), bad).toBeNull();
  });

  it("formats with three digits, or four for special bits", () => {
    expect(formatOctal(0o755)).toBe("755");
    expect(formatOctal(0o44)).toBe("044");
    expect(formatOctal(0)).toBe("000");
    expect(formatOctal(0o4755)).toBe("4755");
    expect(formatOctal(0o1777)).toBe("1777");
    expect(formatOctal(0o170755)).toBe("755");
  });

  it("round-trips", () => {
    for (const mode of [0, 0o600, 0o755, 0o2750, 0o7777]) expect(parseOctal(formatOctal(mode))).toBe(mode);
  });
});

describe("the checkboxes", () => {
  it("reads and sets single bits", () => {
    expect(hasBit(0o754, "user", "execute")).toBe(true);
    expect(hasBit(0o754, "group", "write")).toBe(false);
    expect(hasBit(0o754, "other", "read")).toBe(true);
    expect(setBit(0o644, "user", "execute", true)).toBe(0o744);
    expect(setBit(0o644, "other", "read", false)).toBe(0o640);
    expect(setBit(0o644, "group", "write", true)).toBe(0o664);
  });

  it("leaves the special bits alone", () => {
    expect(setBit(0o4755, "group", "execute", false)).toBe(0o4745);
  });

  it("writes the mode as rwx text", () => {
    expect(formatSymbolic(0o755)).toBe("rwxr-xr-x");
    expect(formatSymbolic(0o4755)).toBe("rwsr-xr-x");
    expect(formatSymbolic(0o4644)).toBe("rwSr--r--");
    expect(formatSymbolic(0o1777)).toBe("rwxrwxrwt");
    expect(formatSymbolic(0o1776)).toBe("rwxrwxrwT");
    expect(formatSymbolic(0)).toBe("---------");
  });

  it("agrees with the string parser", () => {
    for (const mode of [0o644, 0o755, 0o4755, 0o2750, 0o1777, 0o4644]) {
      expect(parseModeString(`-${formatSymbolic(mode)}`)).toBe(mode);
    }
  });
});
