import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync, existsSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TerminalTransfer, type TransferFiles, type TransferHost, type TransferView } from "./terminalTransfer";
import { textToBytes } from "./terminalBytes";
import { engineDecode } from "./engineDecode.testutil";

// The strings module reads the page's language when it loads; there is no page here.
vi.hoisted(() => {
  Object.assign(globalThis, { window: { location: { search: "?lang=en" } } });
});

const has = (command: string) => spawnSync(command, ["--version"]).status !== null && !spawnSync(command, ["--version"]).error;
const lrzsz = has("sz") && has("rz");

/** The files of a test, on a real temporary folder. */
function diskFiles(dir: string, picks: { send?: string[]; save?: string }) {
  let next = 1;
  const open = new Map<number, { path: string; chunks: Buffer[]; part?: string; read?: Buffer; at?: number }>();
  const files: TransferFiles = {
    pickSend: async () => picks.send,
    pickSave: async () => picks.save,
    async openRead(path) {
      const data = readFileSync(path);
      const id = next++;
      open.set(id, { path, chunks: [], read: data, at: 0 });
      return { id, name: path.split("/").pop() ?? "", size: data.length, modified: Math.floor(statSync(path).mtimeMs / 1000) };
    },
    async read(id, len) {
      const file = open.get(id)!;
      const chunk = file.read!.subarray(file.at!, file.at! + len);
      file.at! += chunk.length;
      return new Uint8Array(chunk);
    },
    async openWrite(name, path) {
      const target = path ?? join(dir, "downloads", name);
      const id = next++;
      open.set(id, { path: target, chunks: [] });
      return { id, path: target };
    },
    async write(id, bytes) {
      open.get(id)!.chunks.push(Buffer.from(bytes));
    },
    async close(id, keep) {
      const file = open.get(id);
      open.delete(id);
      if (file && !file.read && keep) writeFileSync(file.path, Buffer.concat(file.chunks));
    },
  };
  return { files, open };
}

/** A terminal and session as the transfer sees them, wired to a child process like sz or rz. */
function rig(transfer: TerminalTransfer, child: ChildProcessWithoutNullStreams) {
  const terminal: string[] = [];
  const sent: number[] = [];
  const host: TransferHost = {
    write: (text) => terminal.push(text),
    send: (text) => {
      const bytes = textToBytes(text);
      sent.push(...bytes);
      if (!child.stdin.destroyed) child.stdin.write(Buffer.from(bytes));
    },
  };
  transfer.bind(host);
  const exited = new Promise<void>((resolve) => child.on("close", () => resolve()));
  // The child's output reaches the transfer the way the engine delivers it: as
  // text, split wherever the reads happened to end.
  child.stdout.on("data", (data: Buffer) => {
    const sizes: number[] = [];
    for (let left = data.length, step = 1 + (data.length % 97); left > 0; left -= step) sizes.push(Math.min(step, left));
    for (const text of engineDecode(new Uint8Array(data), sizes)) transfer.feed(text);
  });
  return { terminal, sent, exited };
}

const waitFor = async (check: () => boolean, ms = 20_000) => {
  const end = Date.now() + ms;
  while (!check()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
};

const prefs = (enabled = true, askWhere = false) => () => ({ enabled, askWhere });

describe.runIf(lrzsz)("rz/sz with lrzsz", () => {
  let dir: string;
  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), "rzsz-"));
    spawnSync("mkdir", ["-p", join(dir, "downloads"), join(dir, "up")]);
  });
  afterEach(() => rmSync(dir, { recursive: true, force: true }));

  const seen = (transfer: TerminalTransfer) => {
    const phases: TransferView[] = [];
    transfer.subscribe(() => {
      const view = transfer.snapshot();
      if (view) phases.push(view);
    });
    return phases;
  };

  it("receives files from sz byte for byte and keeps the protocol off the screen", async () => {
    const big = Buffer.alloc(300_000);
    for (let i = 0; i < big.length; i += 1) big[i] = (i * 7 + (i >> 8)) & 0xff;
    writeFileSync(join(dir, "big.bin"), big);
    writeFileSync(join(dir, "主机 notes.txt"), "你好，ZMODEM\n");
    const { files } = diskFiles(dir, {});
    const transfer = new TerminalTransfer(files, prefs());
    const phases = seen(transfer);
    const child = spawn("sz", ["-e", "-b", join(dir, "big.bin"), join(dir, "主机 notes.txt")], { stdio: "pipe" });
    const { terminal, exited } = rig(transfer, child);
    transfer.feed("$ sz big.bin\r\n");
    await exited;
    await waitFor(() => transfer.snapshot()?.phase === "done");
    expect(readFileSync(join(dir, "downloads", "big.bin")).equals(big)).toBe(true);
    expect(readFileSync(join(dir, "downloads", "主机 notes.txt"), "utf8")).toBe("你好，ZMODEM\n");
    // Only the prompt reached the screen; no handshake or file bytes.
    const shown = terminal.join("");
    expect(shown.startsWith("$ sz big.bin\r\n")).toBe(true);
    expect(shown).not.toContain("\x18");
    expect(shown.length).toBeLessThan(40);
    expect(phases.some((view) => view.phase === "running" && view.done > 0)).toBe(true);
    expect(transfer.active()).toBe(false);
    // The terminal is back to normal afterwards.
    transfer.feed("$ ");
    expect(terminal.join("").endsWith("$ ")).toBe(true);
    transfer.dispose();
  });

  it("asks where to save when told to, and a cancelled dialog skips the file", async () => {
    writeFileSync(join(dir, "a.txt"), "alpha");
    const chosen = join(dir, "downloads", "renamed.txt");
    const { files } = diskFiles(dir, { save: chosen });
    const transfer = new TerminalTransfer(files, prefs(true, true));
    const child = spawn("sz", ["-e", "-b", join(dir, "a.txt")], { stdio: "pipe" });
    const { exited } = rig(transfer, child);
    await exited;
    await waitFor(() => transfer.snapshot()?.phase === "done");
    expect(readFileSync(chosen, "utf8")).toBe("alpha");
    transfer.dispose();

    const skipping = diskFiles(dir, { save: undefined });
    const second = new TerminalTransfer(skipping.files, prefs(true, true));
    const again = spawn("sz", ["-e", "-b", join(dir, "a.txt")], { stdio: "pipe" });
    const rigged = rig(second, again);
    await rigged.exited;
    await waitFor(() => !second.active());
    expect(existsSync(join(dir, "downloads", "a.txt"))).toBe(false);
    second.dispose();
  });

  it("sends the chosen files to rz byte for byte", async () => {
    const big = Buffer.alloc(200_000);
    for (let i = 0; i < big.length; i += 1) big[i] = (i * 13 + 5) & 0xff;
    writeFileSync(join(dir, "one.bin"), big);
    writeFileSync(join(dir, "two.txt"), "second\n");
    writeFileSync(join(dir, "empty.dat"), "");
    const { files } = diskFiles(dir, { send: [join(dir, "one.bin"), join(dir, "two.txt"), join(dir, "empty.dat")] });
    const transfer = new TerminalTransfer(files, prefs());
    const child = spawn("rz", ["-e", "-b", "-y"], { stdio: "pipe", cwd: join(dir, "up") });
    const { terminal, exited } = rig(transfer, child);
    await exited;
    await waitFor(() => transfer.snapshot()?.phase === "done");
    expect(readFileSync(join(dir, "up", "one.bin")).equals(big)).toBe(true);
    expect(readFileSync(join(dir, "up", "two.txt"), "utf8")).toBe("second\n");
    expect(statSync(join(dir, "up", "empty.dat")).size).toBe(0);
    expect(terminal.join("")).not.toContain("\x18");
    transfer.dispose();
  });

  it("sends nothing and ends the session when no file is chosen", async () => {
    const { files } = diskFiles(dir, { send: undefined });
    const transfer = new TerminalTransfer(files, prefs());
    const child = spawn("rz", ["-e", "-b", "-y"], { stdio: "pipe", cwd: join(dir, "up") });
    const { exited } = rig(transfer, child);
    await exited;
    await waitFor(() => !transfer.active());
    expect(readdirSync(join(dir, "up"))).toEqual([]);
    expect(transfer.snapshot()).toBeNull();
    transfer.dispose();
  });

  it("cancels: the abort sequence goes out, the partial file goes away, output resumes", async () => {
    const big = Buffer.alloc(8_000_000, 7);
    writeFileSync(join(dir, "huge.bin"), big);
    const { files, open } = diskFiles(dir, {});
    const transfer = new TerminalTransfer(files, prefs());
    const child = spawn("sz", ["-e", "-b", join(dir, "huge.bin")], { stdio: "pipe" });
    const { terminal, sent, exited } = rig(transfer, child);
    await waitFor(() => (transfer.snapshot()?.done ?? 0) > 100_000);
    expect(transfer.input("x")).toBe(true);
    transfer.input("\x1b");
    expect(transfer.snapshot()?.phase).toBe("cancelled");
    expect(transfer.active()).toBe(false);
    await waitFor(() => Buffer.from(sent).includes(Buffer.alloc(5, 0x18)));
    // The real sz stops on the sequence; this harness is slow to drain what is still in flight.
    child.kill();
    await exited;
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(existsSync(join(dir, "downloads", "huge.bin"))).toBe(false);
    expect(open.size).toBe(0);
    const before = terminal.length;
    transfer.feed("$ ");
    expect(terminal.length).toBe(before + 1);
    expect(terminal.join("")).not.toContain("**");
    transfer.dispose();
  });
});

describe("without a transfer", () => {
  const plain = () => {
    const shown: string[] = [];
    const sent: string[] = [];
    return { shown, sent, host: { write: (text: string) => void shown.push(text), send: (text: string) => void sent.push(text) } };
  };

  it("passes ordinary output through, with undecodable bytes shown as U+FFFD", () => {
    const { shown, host } = plain();
    const transfer = new TerminalTransfer({} as TransferFiles, prefs());
    transfer.bind(host);
    transfer.feed("hello  world\r\n");
    transfer.feed("**\x18B2 not zmodem");
    expect(shown.join("")).toBe("hello � world\r\n**\x18B2 not zmodem");
    expect(transfer.active()).toBe(false);
    expect(transfer.input("a")).toBe(false);
  });

  it("leaves a handshake alone when rz/sz is turned off", () => {
    const { shown, sent, host } = plain();
    const transfer = new TerminalTransfer({} as TransferFiles, prefs(false));
    transfer.bind(host);
    transfer.feed("rz\r**\x18B00000000000000\r\x11");
    expect(shown.join("")).toContain("B00000000000000");
    expect(sent).toEqual([]);
    expect(transfer.active()).toBe(false);
  });

  it("shows a lone handshake start as text when nothing follows it", () => {
    vi.useFakeTimers();
    const { shown, host } = plain();
    const transfer = new TerminalTransfer({} as TransferFiles, prefs());
    transfer.bind(host);
    transfer.feed("a **\x18B01 b");
    expect(shown.join("")).toBe("a ");
    vi.advanceTimersByTime(500);
    expect(shown.join("")).toBe("a **\x18B01 b");
    vi.useRealTimers();
    transfer.dispose();
  });

  it("starts a session on a handshake split across reads and gives up when none follows", () => {
    vi.useFakeTimers();
    const { shown, host } = plain();
    const transfer = new TerminalTransfer({ pickSend: () => new Promise(() => {}) } as unknown as TransferFiles, prefs());
    transfer.bind(host);
    transfer.feed("ready\r\n*");
    transfer.feed("*\x18B01000");
    transfer.feed("00063f694\r\uf78a\x11");
    expect(transfer.active()).toBe(true);
    expect(shown.join("")).not.toContain("B0100");
    // Cancelling (Esc) sends the abort sequence and the terminal is free again.
    expect(transfer.input("\x1b")).toBe(true);
    expect(transfer.active()).toBe(false);
    vi.useRealTimers();
    transfer.dispose();
  });
});
