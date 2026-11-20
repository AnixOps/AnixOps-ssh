import { describe, expect, it } from "vitest";
import { checkEditable, editConflict, editKey, isEditableName, MAX_EDIT_BYTES, stampOf } from "./remoteEdit";
import { followTarget, clearTerminalCwd, onTerminalCwd, publishTerminalCwd, terminalCwd } from "./terminalCwd";

const base = { size: 120, modified: "Oct 14 09:30" };

describe("editConflict", () => {
  it("is the same when size and time match", () => {
    expect(editConflict(base, { ...base })).toBe("same");
  });
  it("sees a change in either", () => {
    expect(editConflict(base, { size: 121, modified: base.modified })).toBe("changed");
    expect(editConflict(base, { size: 120, modified: "Oct 14 09:31" })).toBe("changed");
  });
  it("sees a file that is gone", () => {
    expect(editConflict(base, null)).toBe("gone");
  });
  it("takes the stamp from a listing entry", () => {
    expect(stampOf({ size: 1, modified: "x" })).toEqual({ size: 1, modified: "x" });
  });
});

describe("checkEditable", () => {
  it("refuses folders and files over 20 MB", () => {
    expect(checkEditable({ kind: "directory", size: 0 })).toBe("not-file");
    expect(checkEditable({ kind: "file", size: MAX_EDIT_BYTES + 1 })).toBe("too-large");
    expect(checkEditable({ kind: "file", size: MAX_EDIT_BYTES })).toBe("ok");
    expect(checkEditable({ kind: "symlink", size: 5 })).toBe("ok");
  });
  it("only takes names that survive as local file names", () => {
    expect(isEditableName("nginx.conf")).toBe(true);
    expect(isEditableName("my file (1).txt")).toBe(true);
    expect(isEditableName("a:b")).toBe(false);
    expect(isEditableName("..")).toBe(false);
    expect(isEditableName("a\\b")).toBe(false);
  });
  it("keys one watch per host and path", () => {
    expect(editKey("h1", "/a")).not.toBe(editKey("h2", "/a"));
    expect(editKey("h1", "/a")).not.toBe(editKey("h1", "/b"));
  });
});

describe("followTarget", () => {
  const on = { integration: true, cwd: "/srv/app" };
  it("moves to the terminal's folder when it differs", () => {
    expect(followTarget({ enabled: true, state: on, shown: "/home/me" })).toBe("/srv/app");
  });
  it("stays when already there, off, or without integration", () => {
    expect(followTarget({ enabled: true, state: on, shown: "/srv/app" })).toBeNull();
    expect(followTarget({ enabled: false, state: on, shown: "/" })).toBeNull();
    expect(followTarget({ enabled: true, state: { integration: false, cwd: "/x" }, shown: "/" })).toBeNull();
    expect(followTarget({ enabled: true, state: { integration: true, cwd: undefined }, shown: "/" })).toBeNull();
  });
  it("ignores a trailing slash and a relative report", () => {
    expect(followTarget({ enabled: true, state: { integration: true, cwd: "/srv/app/" }, shown: "/srv/app" })).toBeNull();
    expect(followTarget({ enabled: true, state: { integration: true, cwd: "/" }, shown: "/srv" })).toBe("/");
    expect(followTarget({ enabled: true, state: { integration: true, cwd: "src" }, shown: "/" })).toBeNull();
  });
});

describe("terminal folder reports", () => {
  it("tells listeners and forgets a closed terminal", () => {
    const seen: (string | undefined)[] = [];
    const stop = onTerminalCwd("host-a", (state) => seen.push(state.cwd));
    const first = Symbol("a");
    const second = Symbol("b");
    publishTerminalCwd("host-a", first, { integration: true, cwd: "/one" });
    publishTerminalCwd("host-a", second, { integration: true, cwd: "/two" });
    clearTerminalCwd("host-a", first); // no longer the reporting terminal
    expect(terminalCwd("host-a").cwd).toBe("/two");
    clearTerminalCwd("host-a", second);
    expect(terminalCwd("host-a")).toEqual({ integration: false, cwd: undefined });
    stop();
    expect(seen).toEqual(["/one", "/two", undefined]);
  });
});
