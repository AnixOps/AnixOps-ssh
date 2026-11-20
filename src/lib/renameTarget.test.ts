import { describe, expect, it } from "vitest";
import { resolveRenameTarget } from "./renameTarget";

const cwd = "/srv/app";
const home = "/home/deploy";
const move = (input: string, from = "/srv/app/old.txt") => resolveRenameTarget(input, from, cwd, home);

describe("renaming in place", () => {
  it("keeps the entry in its folder", () => {
    expect(move("new.txt")).toEqual({ ok: true, to: "/srv/app/new.txt", moves: false });
    expect(move("  new.txt  ")).toEqual({ ok: true, to: "/srv/app/new.txt", moves: false });
  });

  it("works at the root", () => {
    expect(resolveRenameTarget("b", "/a", "/", home)).toEqual({ ok: true, to: "/b", moves: false });
  });

  it("refuses empty, dot names and the same name", () => {
    expect(move("   ")).toEqual({ ok: false, reason: "empty" });
    expect(move(".")).toEqual({ ok: false, reason: "invalid" });
    expect(move("..")).toEqual({ ok: false, reason: "invalid" });
    expect(move("old.txt")).toEqual({ ok: false, reason: "same" });
  });
});

describe("moving with a slash", () => {
  it("moves relative to the folder being shown", () => {
    expect(move("archive/old.txt")).toEqual({ ok: true, to: "/srv/app/archive/old.txt", moves: true });
    expect(move("archive/2026/renamed.txt")).toEqual({ ok: true, to: "/srv/app/archive/2026/renamed.txt", moves: true });
    expect(move("../old.txt")).toEqual({ ok: true, to: "/srv/old.txt", moves: true });
  });

  it("moves to an absolute path", () => {
    expect(move("/tmp/x.txt")).toEqual({ ok: true, to: "/tmp/x.txt", moves: true });
  });

  it("expands ~ to the home folder", () => {
    expect(move("~/backup/old.txt")).toEqual({ ok: true, to: "/home/deploy/backup/old.txt", moves: true });
    expect(move("~/")).toEqual({ ok: true, to: "/home/deploy/old.txt", moves: true });
  });

  it("keeps the name when the path ends with a slash", () => {
    expect(move("archive/")).toEqual({ ok: true, to: "/srv/app/archive/old.txt", moves: true });
    expect(move("/tmp/")).toEqual({ ok: true, to: "/tmp/old.txt", moves: true });
  });

  it("treats the typed path to the same place as no change", () => {
    expect(move("./old.txt")).toEqual({ ok: false, reason: "same" });
    expect(move("/srv/app/old.txt")).toEqual({ ok: false, reason: "same" });
    expect(move("/srv/app/")).toEqual({ ok: false, reason: "same" });
  });

  it("refuses to move a folder into itself", () => {
    expect(move("dir/inner/dir", "/srv/app/dir")).toEqual({ ok: false, reason: "inside" });
    expect(move("/srv/app/dir/sub/", "/srv/app/dir")).toEqual({ ok: false, reason: "inside" });
  });

  it("refuses ~ and the root as a target", () => {
    expect(move("~")).toEqual({ ok: false, reason: "invalid" });
    expect(move("/..")).toEqual({ ok: false, reason: "invalid" });
  });

  it("a folder may move to a sibling that merely starts with its name", () => {
    expect(move("/srv/app/dir2/x", "/srv/app/dir")).toEqual({ ok: true, to: "/srv/app/dir2/x", moves: true });
  });
});
