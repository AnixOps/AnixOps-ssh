import { describe, expect, it } from "vitest";
import { formatOctal, formatSymbolic, modeFromListing, parseOctal, toggleBit } from "./permissions";

describe("permissions", () => {
  it("reads the mode of an ls -l string", () => {
    expect(modeFromListing("-rwxr-xr-x")).toBe(0o755);
    expect(modeFromListing("drw-r-----")).toBe(0o640);
    expect(modeFromListing("lrwxrwxrwx")).toBe(0o777);
    expect(modeFromListing("-rwsr-xr-x")).toBe(0o4755);
    expect(modeFromListing("-rwxr-sr-x")).toBe(0o2755);
    expect(modeFromListing("drwxrwxrwt")).toBe(0o1777);
    expect(modeFromListing("-rwxr-xr-T")).toBe(0o1754);
    expect(modeFromListing("----------")).toBe(0);
    expect(modeFromListing("")).toBeNull();
    expect(modeFromListing("not a mode!")).toBeNull();
  });

  it("formats and parses octal text", () => {
    expect(formatOctal(0o755)).toBe("755");
    expect(formatOctal(0o4755)).toBe("4755");
    expect(formatOctal(0o7)).toBe("007");
    expect(parseOctal("0644")).toBe(0o644);
    expect(parseOctal(" 755 ")).toBe(0o755);
    for (const bad of ["", "75", "758", "77777", "rwx"]) expect(parseOctal(bad)).toBeNull();
  });

  it("writes the symbolic form and toggles bits", () => {
    expect(formatSymbolic(0o750)).toBe("rwxr-x---");
    expect(toggleBit(0o644, 0o001)).toBe(0o645);
    expect(toggleBit(0o645, 0o001)).toBe(0o644);
  });
});
