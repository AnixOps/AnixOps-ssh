import { describe, expect, it } from "vitest";
import type { Group, Host, RemoteFile, Tunnel } from "./backend";
import { checkDraft, draftOf, emptyDraft, describe as describeForward } from "./mobileForwards";
import { chunkRanges, crumbs, formatSize, joinPath, parentPath, sortFiles, validName } from "./mobileFiles";
import { groupName, jumpHostFor, jumpOptions, jumpText, listHosts, matchesQuery, parseTags } from "./mobileHosts";

function host(id: string, over: Partial<Host> = {}, fields: Partial<Host["fields"]> = {}): Host {
  return {
    id,
    name: id,
    target: `me@${id}.example`,
    command: "",
    groupId: null,
    tags: [],
    favorite: false,
    color: null,
    source: "saved",
    fields: {
      alias: null,
      hostname: `${id}.example`,
      username: "me",
      port: 22,
      proxyJump: null,
      notes: "",
      shellIntegration: false,
      ...fields,
    },
    ...over,
  };
}

const groups: Group[] = [{ id: "g1", name: "Lab", color: null }];

describe("remote paths", () => {
  it("joins with one slash and finds the parent", () => {
    expect(joinPath("/a", "b")).toBe("/a/b");
    expect(joinPath("/a/", "b")).toBe("/a/b");
    expect(joinPath("/", "b")).toBe("/b");
    expect(parentPath("/a/b")).toBe("/a");
    expect(parentPath("/a/b/")).toBe("/a");
    expect(parentPath("/a")).toBe("/");
    expect(parentPath("/")).toBe("/");
  });

  it("breaks a path into steps from the root", () => {
    expect(crumbs("/")).toEqual([{ name: "/", path: "/" }]);
    expect(crumbs("/home/deploy")).toEqual([
      { name: "/", path: "/" },
      { name: "home", path: "/home" },
      { name: "deploy", path: "/home/deploy" },
    ]);
  });

  it("only accepts names that stay inside the folder", () => {
    for (const bad of ["", "  ", ".", "..", "a/b", "a\0b"]) expect(validName(bad)).toBe(false);
    expect(validName("notes v2.txt")).toBe(true);
    expect(validName(".bashrc")).toBe(true);
  });
});

describe("file listing", () => {
  const file = (name: string, kind: RemoteFile["kind"] = "file"): RemoteFile => ({
    name,
    kind,
    size: 1,
    modified: "",
    permissions: "",
  });

  it("puts folders first and sorts by name ignoring case", () => {
    const sorted = sortFiles([file("b.txt"), file("Zeta", "directory"), file("A.txt"), file("alpha", "directory")]);
    expect(sorted.map((entry) => entry.name)).toEqual(["alpha", "Zeta", "A.txt", "b.txt"]);
  });

  it("writes sizes the way the desktop does", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(812)).toBe("812 B");
    expect(formatSize(1536)).toBe("1.5 KB");
    expect(formatSize(734003200)).toBe("700 MB");
    expect(formatSize(5 * 1024 ** 4)).toBe("5.0 TB");
  });
});

describe("upload chunks", () => {
  it("covers the whole file once, in order", () => {
    expect(chunkRanges(10, 4)).toEqual([
      [0, 4],
      [4, 8],
      [8, 10],
    ]);
    expect(chunkRanges(8, 4)).toEqual([
      [0, 4],
      [4, 8],
    ]);
  });

  it("stages an empty file as one empty chunk", () => {
    expect(chunkRanges(0, 4)).toEqual([[0, 0]]);
  });
});

describe("host search", () => {
  const web = host("web", { tags: ["prod", "nginx"], groupId: "g1" });
  const db = host("db", { tags: ["Prod"], favorite: true });

  it("matches name, address, group and tags", () => {
    expect(matchesQuery(web, groups, "")).toBe(true);
    expect(matchesQuery(web, groups, "WEB")).toBe(true);
    expect(matchesQuery(web, groups, "example")).toBe(true);
    expect(matchesQuery(web, groups, "lab")).toBe(true);
    expect(matchesQuery(web, groups, "ngin")).toBe(true);
    expect(matchesQuery(db, groups, "lab")).toBe(false);
  });

  it("matches a #tag against tags only and needs every word", () => {
    expect(matchesQuery(web, groups, "#prod")).toBe(true);
    expect(matchesQuery(web, groups, "#web")).toBe(false);
    expect(matchesQuery(web, groups, "#")).toBe(true);
    expect(matchesQuery(web, groups, "web #prod")).toBe(true);
    expect(matchesQuery(web, groups, "web #db")).toBe(false);
  });

  it("lists favorites first, then by name, without serial ports", () => {
    const serial = host("com", {}, { serial: { path: "/dev/ttyUSB0" } as never });
    const shown = listHosts([web, serial, db, host("api")], groups, "");
    expect(shown.map((entry) => entry.id)).toEqual(["db", "api", "web"]);
  });

  it("names the group of a host", () => {
    expect(groupName(web, groups)).toBe("Lab");
    expect(groupName(db, groups)).toBeNull();
  });
});

describe("tags", () => {
  it("trims, drops # and empties, and keeps the first of a repeat", () => {
    expect(parseTags("prod, web ,#db,, PROD")).toEqual(["prod", "web", "db"]);
    expect(parseTags("")).toEqual([]);
    expect(parseTags("a，b\nc")).toEqual(["a", "b", "c"]);
  });
});

describe("jump hosts", () => {
  it("writes the stored text like the engine does", () => {
    expect(jumpText(host("bastion"))).toBe("me@bastion.example");
    expect(jumpText(host("b", {}, { port: 2222 }))).toBe("me@b.example:2222");
    expect(jumpText(host("v6", {}, { hostname: "fe80::1" }))).toBe("me@[fe80::1]");
    expect(jumpText(host("nouser", {}, { username: null }))).toBe("nouser.example");
  });

  it("offers saved hosts but not the host itself, aliases or serial ports", () => {
    const hosts = [
      host("a"),
      host("b"),
      host("alias", { source: "ssh_config" }, { alias: "alias" }),
      host("com", {}, { serial: { path: "/dev/ttyUSB0" } as never }),
    ];
    expect(jumpOptions(hosts, "a").map((entry) => entry.id)).toEqual(["b"]);
    expect(jumpOptions(hosts, null).map((entry) => entry.id)).toEqual(["a", "b"]);
  });

  it("does not offer a host that already goes through the one being edited", () => {
    const a = host("a");
    const b = host("b", {}, { proxyJump: jumpText(a) });
    const c = host("c");
    expect(jumpOptions([a, b, c], "a").map((entry) => entry.id)).toEqual(["c"]);
  });

  it("finds the saved host a stored text names", () => {
    const a = host("a", {}, { port: 2200 });
    expect(jumpHostFor([a, host("b")], "me@a.example:2200")?.id).toBe("a");
    expect(jumpHostFor([a], "other@x")).toBeNull();
    expect(jumpHostFor([a], null)).toBeNull();
  });
});

describe("forward form", () => {
  const filled = { ...emptyDraft("h1"), bindPort: "5432", targetHost: "db.internal", targetPort: "5432" };

  it("accepts a complete draft and names it by what it does", () => {
    const result = checkDraft(filled);
    expect(result.ok && result.input).toMatchObject({
      name: "5432 → db.internal:5432",
      hostId: "h1",
      kind: "local",
      bindPort: 5432,
      targetHost: "db.internal",
      targetPort: 5432,
    });
    const named = checkDraft({ ...filled, name: " Prod DB " });
    expect(named.ok && named.input.name).toBe("Prod DB");
  });

  it("names the first problem", () => {
    expect(checkDraft({ ...filled, hostId: "" })).toEqual({ ok: false, problem: "host" });
    expect(checkDraft({ ...filled, bindPort: "0" })).toEqual({ ok: false, problem: "bindPort" });
    expect(checkDraft({ ...filled, bindPort: "70000" })).toEqual({ ok: false, problem: "bindPort" });
    expect(checkDraft({ ...filled, bindPort: "80a" })).toEqual({ ok: false, problem: "bindPort" });
    expect(checkDraft({ ...filled, targetHost: " " })).toEqual({ ok: false, problem: "targetHost" });
    expect(checkDraft({ ...filled, targetHost: "a b" })).toEqual({ ok: false, problem: "targetHost" });
    expect(checkDraft({ ...filled, targetPort: "" })).toEqual({ ok: false, problem: "targetPort" });
  });

  it("round-trips a saved forward into a draft", () => {
    const tunnel: Tunnel = {
      id: "t",
      name: "db",
      hostId: "h1",
      kind: "local",
      bindAddress: null,
      bindPort: 5432,
      targetHost: "db.internal",
      targetPort: 5433,
      status: { state: "stopped" },
    };
    expect(draftOf(tunnel)).toEqual({
      name: "db",
      hostId: "h1",
      bindPort: "5432",
      targetHost: "db.internal",
      targetPort: "5433",
    });
    expect(describeForward(tunnel)).toBe("5432 → db.internal:5433");
    expect(describeForward({ ...tunnel, kind: "dynamic", targetHost: null, targetPort: null })).toBe("5432 SOCKS");
    expect(describeForward({ ...tunnel, kind: "remote" })).toBe("5432 ← db.internal:5433");
  });
});
