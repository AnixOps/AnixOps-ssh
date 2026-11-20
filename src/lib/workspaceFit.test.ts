import { describe, expect, it } from "vitest";
import { fitWorkspace, TERMINAL_MIN_WIDTH } from "./workspaceFit";

const monitor = { open: true, collapsed: false, width: 288 };

describe("fitWorkspace", () => {
  it("keeps the user's layout when the terminal has room", () => {
    expect(fitWorkspace({ width: 1200, monitor, filesOpen: true })).toEqual({ monitorRail: false, filesOverlay: false });
  });

  it("folds the monitor first", () => {
    // The smallest window: 860 minus the icon rail and the host list.
    expect(fitWorkspace({ width: 508, monitor, filesOpen: false })).toEqual({ monitorRail: true, filesOverlay: false });
  });

  it("floats the files pane when even the rail leaves too little", () => {
    expect(fitWorkspace({ width: 508, monitor, filesOpen: true })).toEqual({ monitorRail: true, filesOverlay: true });
    expect(fitWorkspace({ width: 600, monitor: { ...monitor, open: false }, filesOpen: true })).toEqual({ monitorRail: false, filesOverlay: true });
  });

  it("leaves a folded or closed monitor alone", () => {
    expect(fitWorkspace({ width: 400, monitor: { ...monitor, collapsed: true }, filesOpen: false }).monitorRail).toBe(false);
  });

  it("does nothing before the width is known", () => {
    expect(fitWorkspace({ width: 0, monitor, filesOpen: true })).toEqual({ monitorRail: false, filesOverlay: false });
  });

  it("gives the terminal its minimum exactly at the boundary", () => {
    expect(fitWorkspace({ width: TERMINAL_MIN_WIDTH + 288, monitor, filesOpen: false }).monitorRail).toBe(false);
    expect(fitWorkspace({ width: TERMINAL_MIN_WIDTH + 287, monitor, filesOpen: false }).monitorRail).toBe(true);
  });
});
