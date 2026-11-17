import { describe, expect, it } from "vitest";
import { handlePopupKey, openPopup, type PopupState } from "./popup";
import type { Candidate } from "./types";

const items: Candidate[] = ["one", "two", "three"].map((label) => ({ label, insert: label, kind: "value" }));
const open = (index: number | null = null): PopupState => ({ ...openPopup(items), index });

describe("handlePopupKey", () => {
  it("never takes Enter when nothing is highlighted", () => {
    const outcome = handlePopupKey(open(), { key: "Enter" });
    expect(outcome.consumed).toBe(false);
    expect(outcome.accept).toBeUndefined();
  });

  it("takes Enter to accept the highlighted item", () => {
    const outcome = handlePopupKey(open(1), { key: "Enter" });
    expect(outcome).toMatchObject({ consumed: true, accept: items[1] });
  });

  it("does not take Shift+Enter or modified keys", () => {
    expect(handlePopupKey(open(1), { key: "Enter", shiftKey: true }).consumed).toBe(false);
    expect(handlePopupKey(open(1), { key: "Tab", ctrlKey: true }).consumed).toBe(false);
    expect(handlePopupKey(open(1), { key: "ArrowDown", altKey: true }).consumed).toBe(false);
    expect(handlePopupKey(open(1), { key: "Tab", shiftKey: true }).consumed).toBe(false);
  });

  it("Tab accepts the highlighted item, or the first one", () => {
    expect(handlePopupKey(open(2), { key: "Tab" }).accept).toBe(items[2]);
    expect(handlePopupKey(open(), { key: "Tab" }).accept).toBe(items[0]);
  });

  it("Down enters the list and wraps around", () => {
    const first = handlePopupKey(open(), { key: "ArrowDown" });
    expect(first.consumed).toBe(true);
    expect(first.state.index).toBe(0);
    expect(handlePopupKey(open(2), { key: "ArrowDown" }).state.index).toBe(0);
  });

  it("Up leaves the list at the top, and passes through when nothing is highlighted", () => {
    expect(handlePopupKey(open(2), { key: "ArrowUp" }).state.index).toBe(1);
    const top = handlePopupKey(open(0), { key: "ArrowUp" });
    expect(top.consumed).toBe(true);
    expect(top.state.index).toBeNull();
    expect(handlePopupKey(open(), { key: "ArrowUp" }).consumed).toBe(false);
  });

  it("Escape dismisses the list", () => {
    expect(handlePopupKey(open(), { key: "Escape" })).toMatchObject({ consumed: true, dismiss: true });
  });

  it("passes everything else to the shell", () => {
    for (const key of ["a", "Backspace", " ", "ArrowLeft", "ArrowRight", "Home"]) {
      expect(handlePopupKey(open(1), { key }).consumed, key).toBe(false);
    }
  });

  it("takes nothing when the list is empty", () => {
    const empty = openPopup([]);
    for (const key of ["Tab", "Enter", "ArrowDown", "Escape"]) {
      expect(handlePopupKey(empty, { key }).consumed, key).toBe(false);
    }
  });
});
