import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RemoteFile } from "../backend";
import { applyCandidate, suggest, type EngineDeps } from "./engine";
import { HistoryStore, shouldRemember, type KeyValueStore } from "./history";
import { completePaths, createPathLister, type PathLister, type PathSource } from "./remotePaths";
import { commandNames, loadSpec } from "./specs";

function memoryStorage(): KeyValueStore & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    getItem: (key) => data.get(key) ?? null,
    setItem: (key, value) => void data.set(key, value),
    removeItem: (key) => void data.delete(key),
  };
}

const file = (name: string, kind: RemoteFile["kind"] = "file"): RemoteFile => ({
  name,
  kind,
  size: 1,
  modified: "",
  permissions: kind === "directory" ? "drwxr-xr-x" : "-rw-r--r--",
});

function staticLister(tree: Record<string, RemoteFile[]>, home = "/home/deploy"): PathLister & { asked: string[] } {
  const asked: string[] = [];
  return {
    asked,
    home: async () => home,
    list: async (dir) => {
      asked.push(dir);
      return tree[dir] ?? [];
    },
  };
}

function deps(overrides: Partial<EngineDeps> = {}, history = new HistoryStore(null)): EngineDeps {
  return { loadSpec, commandNames, history, ...overrides };
}

const run = async (line: string, d: EngineDeps = deps(), hostId: string | null = "h1") =>
  (await suggest({ line, hostId }, d)).items;
const inserts = (items: { insert: string }[]) => items.map((item) => item.insert);

describe("suggest", () => {
  it("completes command names at the start of a line and after operators", async () => {
    expect(inserts(await run("gi"))).toContain("git");
    expect(inserts(await run("cd /tmp && doc"))).toContain("docker");
    expect(inserts(await run("cat x | gre"))).toContain("grep");
  });

  it("says nothing for an empty prompt, a comment or an assignment", async () => {
    expect(await run("")).toEqual([]);
    expect(await run("   ")).toEqual([]);
    expect(await run("ls # gi")).toEqual([]);
    expect(await run("FOO=ba")).toEqual([]);
  });

  it("completes subcommands and options with descriptions", async () => {
    const items = await run("git com");
    expect(items[0]).toMatchObject({ label: "commit", kind: "subcommand" });
    expect(items[0].description).toBeTruthy();
    expect(items[0].replaceFrom).toBe(4);
    const options = await run("git commit --am");
    expect(options[0]).toMatchObject({ label: "--amend", kind: "option" });
  });

  it("finds a command through its alias", async () => {
    expect(inserts(await run("yum ins"))).toContain("install");
    expect(inserts(await run("pip3 ins"))).toContain("install");
  });

  it("follows sudo into the wrapped command", async () => {
    expect(inserts(await run("sudo systemctl rest"))).toContain("restart");
    expect(inserts(await run("sudo -u deploy systemctl --us"))).toContain("--user");
    expect(inserts(await run("sudo gi"))).toContain("git");
  });

  it("follows sudo -> sudo chains but gives up on unknown commands", async () => {
    expect(inserts(await run("sudo sudo systemctl rest"))).toContain("restart");
    expect(await run("sudo frobnicate ")).toEqual([]);
  });

  it("quotes values for the shell", async () => {
    const d = deps({
      loadSpec: async () => ({ name: "t", description: "t", args: [{ suggestions: ["two words", "plain"] }] }),
    });
    const items = await run("t t", d);
    expect(inserts(items)).toEqual(["two\\ words"]);
    const all = await run("t ", d);
    expect(inserts(all)).toEqual(["two\\ words", "plain"]);
    const quoted = await run('t "tw', d);
    expect(quoted[0].insert).toBe('"two words"');
  });

  it("keeps --opt= in front of completed values", async () => {
    const items = await run("ls --color=n");
    expect(items[0]).toMatchObject({ label: "never", insert: "--color=never" });
  });

  it("offers earlier lines that continue what was typed", async () => {
    const history = new HistoryStore(null);
    history.add("h1", "docker logs -f web");
    history.add("h1", "git status");
    const items = await run("docker logs", deps({}, history));
    const entry = items.find((item) => item.kind === "history");
    expect(entry).toMatchObject({ insert: "docker logs -f web", replaceFrom: 0 });
    // Only for this host.
    expect((await run("docker logs", deps({}, history), "h2")).some((item) => item.kind === "history")).toBe(false);
  });

  it("completes history inside a second command of the line", async () => {
    const history = new HistoryStore(null);
    history.add("h1", "git stash pop");
    const items = await run("cd x && git sta", deps({}, history));
    expect(items.find((item) => item.kind === "history")).toMatchObject({ insert: "git stash pop", replaceFrom: 8 });
  });

  it("uses history tokens for history templates", async () => {
    const history = new HistoryStore(null);
    history.add("h1", "systemctl restart nginx");
    history.add("h1", "systemctl status sshd");
    const items = await run("systemctl status n", deps({}, history));
    expect(inserts(items)).toContain("nginx");
  });

  it("offers host names for host arguments", async () => {
    const items = await run("ssh pro", deps({ hostNames: async () => ["prod-db", "staging", "prod-web"] }));
    expect(inserts(items)).toEqual(["prod-db", "prod-web"]);
  });

  it("completes remote paths for the session's host only", async () => {
    const lister = staticLister({
      "/var/log/": [file("syslog"), file("nginx", "directory"), file(".hidden"), file("my file.log")],
    });
    const items = await run("tail -f /var/log/", deps({ paths: lister }));
    expect(items.map((item) => [item.label, item.kind])).toEqual([
      ["nginx", "folder"],
      ["my file.log", "file"],
      ["syslog", "file"],
    ]);
    expect(inserts(items)).toEqual(["/var/log/nginx/", "/var/log/my\\ file.log", "/var/log/syslog"]);
    expect(items[0].keepOpen).toBe(true);
    // A local terminal has no remote file system.
    expect(await run("tail -f /var/log/", deps({ paths: lister }), null)).toEqual([]);
  });

  it("only lists folders for folder arguments, and expands ~/", async () => {
    const lister = staticLister({ "/home/deploy/": [] });
    lister.list = async (dir) => (lister.asked.push(dir), dir === "/home/deploy/app/" ? [file("src", "directory"), file("a.txt")] : []);
    const items = await run("cd ~/app/", deps({ paths: lister }));
    expect(inserts(items)).toEqual(["~/app/src/"]);
    expect(lister.asked).toEqual(["/home/deploy/app/"]);
  });

  it("does not list relative paths without a known cwd, and does with one", async () => {
    const lister = staticLister({ "/srv/app/": [file("main.go")] });
    expect(await run("cat ma", deps({ paths: lister }))).toEqual([]);
    expect(lister.asked).toEqual([]);
    const items = (await suggest({ line: "cat ma", hostId: "h1", cwd: "/srv/app" }, deps({ paths: lister }))).items;
    expect(inserts(items)).toEqual(["main.go"]);
  });

  it("completes the target of a redirection as a path", async () => {
    const lister = staticLister({ "/tmp/": [file("out.log")] });
    expect(inserts(await run("ls > /tmp/o", deps({ paths: lister })))).toEqual(["/tmp/out.log"]);
  });

  it("returns nothing when a newer path request replaced this one", async () => {
    const lister: PathLister = { home: async () => "/", list: async () => null };
    expect(await run("cat /tm", deps({ paths: lister }))).toEqual([]);
  });

  it("does not suggest what is already typed", async () => {
    expect(inserts(await run("git commit"))).not.toContain("commit");
  });
});

describe("applyCandidate", () => {
  const item = (insert: string, extra = {}) => ({ label: insert, insert, kind: "value" as const, ...extra });

  it("types the missing part and a space", () => {
    expect(applyCandidate("git com", item("commit", { replaceFrom: 4 }))).toBe("mit ");
  });

  it("backspaces over a differing prefix", () => {
    expect(applyCandidate("git CO", item("commit", { replaceFrom: 4 }))).toBe("\x7f\x7fcommit ");
  });

  it("does not add a space to open items such as folders", () => {
    expect(applyCandidate("cd /va", item("/var/", { replaceFrom: 3, keepOpen: true }))).toBe("r/");
  });

  it("replaces a whole command for history entries", () => {
    expect(applyCandidate("docker lo", item("docker logs -f web", { replaceFrom: 0, keepOpen: true }))).toBe("gs -f web");
  });

  it("counts characters, not UTF-16 units, when backspacing", () => {
    expect(applyCandidate("echo 😀x", item("y", { replaceFrom: 5, keepOpen: true }))).toBe("\x7f\x7fy");
  });
});

describe("HistoryStore", () => {
  it("keeps the newest first without duplicates, per host", () => {
    const history = new HistoryStore(null);
    history.add("a", "ls");
    history.add("a", "pwd");
    history.add("a", "ls");
    history.add("b", "whoami");
    expect(history.recent("a")).toEqual(["ls", "pwd"]);
    expect(history.recent("b")).toEqual(["whoami"]);
    expect(history.recent("c")).toEqual([]);
  });

  it("does not remember blank, indented or secret-looking lines", () => {
    expect(shouldRemember("ls")).toBe(true);
    expect(shouldRemember("")).toBe(false);
    expect(shouldRemember(" ls")).toBe(false);
    expect(shouldRemember("mysql --password=hunter2")).toBe(false);
    expect(shouldRemember("export API_KEY=abc")).toBe(false);
    expect(shouldRemember("x".repeat(500))).toBe(false);
  });

  it("persists to storage and reads it back", () => {
    const storage = memoryStorage();
    new HistoryStore(storage).add("a", "ls -la");
    expect(new HistoryStore(storage).recent("a")).toEqual(["ls -la"]);
  });

  it("remembers nothing and shows nothing while turned off", () => {
    const storage = memoryStorage();
    let enabled = true;
    const history = new HistoryStore(storage, () => enabled);
    history.add("a", "ls");
    enabled = false;
    history.add("a", "pwd");
    expect(history.recent("a")).toEqual([]);
    enabled = true;
    expect(history.recent("a")).toEqual(["ls"]);
  });

  it("clears memory and storage", () => {
    const storage = memoryStorage();
    const history = new HistoryStore(storage);
    history.add("a", "ls");
    history.clear();
    expect(history.recent("a")).toEqual([]);
    expect(storage.data.size).toBe(0);
  });

  it("survives broken storage", () => {
    const broken: KeyValueStore = {
      getItem: () => "{not json",
      setItem: () => {
        throw new Error("full");
      },
      removeItem: () => {
        throw new Error("gone");
      },
    };
    const history = new HistoryStore(broken);
    expect(history.recent("a")).toEqual([]);
    history.add("a", "ls");
    expect(history.recent("a")).toEqual(["ls"]);
    expect(() => history.clear()).not.toThrow();
  });
});

describe("path lister", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  function source(): PathSource & { calls: string[] } {
    const calls: string[] = [];
    return {
      calls,
      home: "/home/me",
      list: async (path) => {
        calls.push(path);
        return [file(`in-${path.length}`)];
      },
    };
  }

  it("waits for typing to pause and answers only the newest request", async () => {
    const backend = source();
    const lister = createPathLister(async () => backend);
    const first = lister.list("/a/");
    await vi.advanceTimersByTimeAsync(50);
    const second = lister.list("/ab/");
    await vi.advanceTimersByTimeAsync(200);
    expect(await first).toBeNull();
    expect(await second).not.toBeNull();
    expect(backend.calls).toEqual(["/ab/"]);
  });

  it("caches a listing for five seconds", async () => {
    const backend = source();
    let now = 1000;
    const lister = createPathLister(async () => backend, { now: () => now });
    const read = async () => {
      const pending = lister.list("/x/");
      await vi.advanceTimersByTimeAsync(200);
      return pending;
    };
    await read();
    now += 4000;
    await lister.list("/x/");
    expect(backend.calls).toHaveLength(1);
    now += 1500;
    await read();
    expect(backend.calls).toHaveLength(2);
  });

  it("treats a failing listing as empty and does not throw", async () => {
    const lister = createPathLister(async () => ({
      home: "/",
      list: async () => {
        throw new Error("denied");
      },
    }));
    const pending = lister.list("/root/");
    await vi.advanceTimersByTimeAsync(200);
    expect(await pending).toEqual([]);
  });

  it("opens the connection once", async () => {
    let opened = 0;
    const lister = createPathLister(async () => {
      opened += 1;
      return source();
    });
    await lister.home();
    await lister.home();
    expect(opened).toBe(1);
  });
});

describe("completePaths", () => {
  const lister = staticLister({ "/data/": [file("b.txt"), file("a", "directory"), file(".env")] });

  it("lists folders first and hides dot files until a dot is typed", async () => {
    const plain = await completePaths(lister, { text: "/data/", quote: "none", foldersOnly: false });
    expect(plain?.map((item) => item.label)).toEqual(["a", "b.txt"]);
    const dotted = await completePaths(lister, { text: "/data/.", quote: "none", foldersOnly: false });
    expect(dotted?.map((item) => item.label)).toEqual([".env"]);
  });

  it("closes the quote around a finished file name but not a folder", async () => {
    const files = await completePaths(lister, { text: "/data/b", quote: "double", foldersOnly: false });
    expect(files?.[0].insert).toBe('"/data/b.txt"');
    const folders = await completePaths(lister, { text: "/data/a", quote: "double", foldersOnly: false });
    expect(folders?.[0].insert).toBe('"/data/a/');
  });

  it("suggests ~/ for a bare tilde", async () => {
    const result = await completePaths(lister, { text: "~", quote: "none", foldersOnly: true });
    expect(result?.map((item) => item.insert)).toEqual(["~/"]);
  });
});
