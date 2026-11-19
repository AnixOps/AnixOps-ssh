import { describe, expect, it, vi } from "vitest";
import type { RemoteFile } from "./backend";
import { baseName, locate, normalizePath, parentPath, resolvePath } from "./remotePath";

const entry = (name: string, kind: RemoteFile["kind"]): RemoteFile => ({
  name,
  kind,
  size: 0,
  modified: "",
  permissions: "",
});

describe("resolvePath", () => {
  const cwd = "/home/deploy/app";
  const home = "/root";

  it("keeps a path that starts with / absolute", () => {
    expect(resolvePath("/data/hanshow/env/tools/flash_seq.py", cwd, home)).toBe("/data/hanshow/env/tools/flash_seq.py");
    expect(resolvePath("  /etc/  ", cwd, home)).toBe("/etc");
    expect(resolvePath("/", cwd, home)).toBe("/");
  });

  it("expands ~ to the home folder", () => {
    expect(resolvePath("~", cwd, home)).toBe("/root");
    expect(resolvePath("~/", cwd, home)).toBe("/root");
    expect(resolvePath("~/logs/today", cwd, home)).toBe("/root/logs/today");
  });

  it("resolves other paths against the folder being shown", () => {
    expect(resolvePath("releases", cwd, home)).toBe("/home/deploy/app/releases");
    expect(resolvePath("../shared/./config", cwd, home)).toBe("/home/deploy/shared/config");
    expect(resolvePath("./x", "", home)).toBe("/root/x");
  });

  it("drops quotes around a pasted path", () => {
    expect(resolvePath("'/srv/my app'", cwd, home)).toBe("/srv/my app");
    expect(resolvePath('"~/a b"', cwd, home)).toBe("/root/a b");
  });

  it("does not treat ~user as home", () => {
    expect(resolvePath("~deploy", cwd, home)).toBe("/home/deploy/app/~deploy");
  });
});

describe("path steps", () => {
  it("normalizes", () => {
    expect(normalizePath("//a//b/./c/")).toBe("/a/b/c");
    expect(normalizePath("/../../a")).toBe("/a");
    expect(normalizePath("")).toBe("/");
  });

  it("finds parents and names", () => {
    expect(parentPath("/a/b/c.py")).toBe("/a/b");
    expect(parentPath("/a/b/")).toBe("/a");
    expect(parentPath("/a")).toBe("/");
    expect(parentPath("/")).toBe("/");
    expect(baseName("/a/b/c.py")).toBe("c.py");
    expect(baseName("/")).toBe("/");
  });
});

describe("locate", () => {
  const tree: Record<string, RemoteFile[]> = {
    "/": [entry("data", "directory")],
    "/data/tools": [entry("flash_seq.py", "file"), entry("bin", "directory"), entry("latest", "symlink"), entry("current", "symlink")],
    "/data/tools/bin": [entry("run", "file")],
    "/data/tools/current": [entry("README", "file")],
  };
  const list = vi.fn(async (path: string) => {
    const found = tree[path];
    if (!found) throw new Error(`not found: ${path}`);
    return found;
  });

  it("shows a file's folder with the file selected", async () => {
    const found = await locate(list, "/data/tools/flash_seq.py");
    expect(found.dir).toBe("/data/tools");
    expect(found.select).toBe("flash_seq.py");
    expect(found.entries.map((e) => e.name)).toContain("flash_seq.py");
  });

  it("lists a folder", async () => {
    expect(await locate(list, "/data/tools/bin")).toEqual({ dir: "/data/tools/bin", entries: tree["/data/tools/bin"], select: null });
    expect((await locate(list, "/")).dir).toBe("/");
  });

  it("opens a link to a folder, and selects a link to a file", async () => {
    expect((await locate(list, "/data/tools/current")).dir).toBe("/data/tools/current");
    expect(await locate(list, "/data/tools/latest")).toMatchObject({ dir: "/data/tools", select: "latest" });
  });

  it("lists a folder whose parent cannot be read", async () => {
    const guarded = async (path: string) => {
      if (path === "/secret") throw new Error("permission denied");
      if (path === "/secret/inside") return [entry("a", "file")];
      throw new Error("not found");
    };
    expect((await locate(guarded, "/secret/inside")).dir).toBe("/secret/inside");
  });

  it("reports a missing path", async () => {
    await expect(locate(list, "/data/tools/nope")).rejects.toThrow("not found: /data/tools/nope");
  });
});
