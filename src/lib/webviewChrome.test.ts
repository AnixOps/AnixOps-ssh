import { describe, expect, it } from "vitest";
import { isBrowserKey, type BrowserKeyInput } from "./webviewChrome";

const key = (k: string, mods: Partial<BrowserKeyInput> = {}): BrowserKeyInput => ({
  key: k,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  metaKey: false,
  ...mods,
});
const pc = { mac: false, dev: false, editable: false };
const mac = { mac: true, dev: false, editable: false };

describe("isBrowserKey", () => {
  it("catches reload, print, find, save, source and zoom", () => {
    for (const k of ["r", "p", "f", "s", "u", "g", "=", "-", "0"]) expect(isBrowserKey(key(k, { ctrlKey: true }), pc)).toBe(true);
    expect(isBrowserKey(key("R", { ctrlKey: true, shiftKey: true }), pc)).toBe(true);
    expect(isBrowserKey(key("F5"), pc)).toBe(true);
    expect(isBrowserKey(key("F7"), pc)).toBe(true);
    expect(isBrowserKey(key("p", { metaKey: true }), mac)).toBe(true);
  });

  it("catches history navigation and the inspector", () => {
    expect(isBrowserKey(key("ArrowLeft", { altKey: true }), pc)).toBe(true);
    expect(isBrowserKey(key("BrowserBack"), pc)).toBe(true);
    expect(isBrowserKey(key("F12"), pc)).toBe(true);
    expect(isBrowserKey(key("I", { ctrlKey: true, shiftKey: true }), pc)).toBe(true);
    expect(isBrowserKey(key("i", { metaKey: true, altKey: true }), mac)).toBe(true);
  });

  it("leaves editing keys and app shortcuts alone", () => {
    for (const k of ["c", "v", "x", "z", "y", "k", "n", "t", "w", ",", "/"]) expect(isBrowserKey(key(k, { ctrlKey: true }), pc)).toBe(false);
    expect(isBrowserKey(key("V", { ctrlKey: true, shiftKey: true }), pc)).toBe(false);
    expect(isBrowserKey(key("C", { ctrlKey: true, shiftKey: true }), { ...pc, dev: true })).toBe(false);
    expect(isBrowserKey(key("a"), pc)).toBe(false);
    expect(isBrowserKey(key("Enter"), pc)).toBe(false);
  });

  it("keeps select all and word moves in text fields", () => {
    expect(isBrowserKey(key("a", { ctrlKey: true }), { ...pc, editable: true })).toBe(false);
    expect(isBrowserKey(key("a", { ctrlKey: true }), pc)).toBe(true);
    expect(isBrowserKey(key("ArrowLeft", { altKey: true }), { ...mac, editable: true })).toBe(false);
  });

  it("leaves Ctrl keys alone on a Mac, where they belong to the shell", () => {
    expect(isBrowserKey(key("r", { ctrlKey: true }), mac)).toBe(false);
    expect(isBrowserKey(key("p", { ctrlKey: true }), mac)).toBe(false);
  });

  it("keeps reload and the inspector in development", () => {
    const dev = { ...pc, dev: true };
    expect(isBrowserKey(key("F5"), dev)).toBe(false);
    expect(isBrowserKey(key("r", { ctrlKey: true }), dev)).toBe(false);
    expect(isBrowserKey(key("F12"), dev)).toBe(false);
    expect(isBrowserKey(key("I", { ctrlKey: true, shiftKey: true }), dev)).toBe(false);
    expect(isBrowserKey(key("p", { ctrlKey: true }), dev)).toBe(true);
  });
});
