import { describe, expect, it } from "vitest";
import { describeError, isMove, renameTarget } from "./fileOps";

const cwd = "/srv/app";
const home = "/home/deploy";

describe("renameTarget", () => {
  it("keeps a plain name in the same folder", () => {
    expect(renameTarget("b.txt", "/srv/app/a.txt", cwd, home)).toBe("/srv/app/b.txt");
    expect(renameTarget("a.txt", "/srv/app/a.txt", cwd, home)).toBeNull();
    expect(renameTarget("  ", "/srv/app/a.txt", cwd, home)).toBeNull();
  });

  it("moves with a relative path", () => {
    expect(renameTarget("../old/a.txt", "/srv/app/a.txt", cwd, home)).toBe("/srv/old/a.txt");
    expect(renameTarget("sub/b.txt", "/srv/app/a.txt", cwd, home)).toBe("/srv/app/sub/b.txt");
  });

  it("moves with an absolute path or ~", () => {
    expect(renameTarget("/tmp/x", "/srv/app/a.txt", cwd, home)).toBe("/tmp/x");
    expect(renameTarget("~/keep/a", "/srv/app/a.txt", cwd, home)).toBe("/home/deploy/keep/a");
  });

  it("moves into a folder, keeping the name, when the target ends with a slash", () => {
    expect(renameTarget("/tmp/", "/srv/app/a.txt", cwd, home)).toBe("/tmp/a.txt");
    expect(renameTarget("..", "/srv/app/a.txt", cwd, home)).toBe("/srv/a.txt");
    expect(renameTarget("./", "/srv/app/a.txt", cwd, home)).toBeNull();
  });

  it("detects moves", () => {
    expect(isMove("/srv/app/a", "/srv/app/b")).toBe(false);
    expect(isMove("/srv/app/a", "/srv/b")).toBe(true);
  });
});

describe("describeError", () => {
  it("adds the operation and path", () => {
    expect(describeError("Deleting", "/srv/a", new Error("Permission denied"))).toBe("Deleting /srv/a: Permission denied");
    expect(describeError("Deleting", "/srv/a", "Error: failure")).toBe("Deleting /srv/a: failure");
  });
  it("does not repeat a path the message has", () => {
    expect(describeError("Listing", "/nope", 'Can\'t ls: "/nope" not found')).toBe('Listing: Can\'t ls: "/nope" not found');
  });
});
