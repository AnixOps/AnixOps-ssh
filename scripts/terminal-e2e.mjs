// Terminal end-to-end test: the app's real frontend in Chromium, talking to
// a real OpenSSH server through the real session daemon and built-in engine
// (crates/easyssh-e2e/examples/web_bridge.rs). Each check is something a
// person does every day in Windows Terminal, iTerm2 or WezTerm; the run
// writes a screenshot per check and a report.
//
// Usage: node scripts/terminal-e2e.mjs <base-url> <out-dir>
//   EASYSSH_BRIDGE  path of the built web_bridge example
//                   (default target/debug/examples/web_bridge)
//   ONLY            comma-separated check names
// Needs the e2e containers (e2e/README.md) or any sshd that the bridge's
// test ssh config reaches as `e2e-target`.
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { join, resolve } from "node:path";
import { chromium } from "playwright";

const [baseUrl = "http://localhost:4173", outDir = "terminal-e2e"] = process.argv.slice(2);
await mkdir(outDir, { recursive: true });
// Local side of file transfers: downloads land here, uploads come from here.
const localDir = resolve(outDir, "local");
const downloadsDir = join(localDir, "downloads");
await mkdir(downloadsDir, { recursive: true });
// Outside the app, the editor's copies go where backend.ts editPrepare says.
await mkdir("/tmp/anixops-edit", { recursive: true });
const bridgePath = process.env.EASYSSH_BRIDGE || "target/debug/examples/web_bridge";
const only = process.env.ONLY ? new Set(process.env.ONLY.split(",")) : null;
// The app reads the system from navigator.platform; PLATFORM=win (default),
// mac or linux runs the checks as that system's user would press keys.
const platform = process.env.PLATFORM || "win";
const isMacRun = platform === "mac";
const navigatorPlatform = { win: "Win32", mac: "MacIntel", linux: "Linux x86_64" }[platform];
const mod = isMacRun ? "Meta" : "Control";

// ---- The bridge process --------------------------------------------------

const bridge = spawn(bridgePath, [], { stdio: ["pipe", "pipe", "inherit"] });
let bridgePage = null;
const pendingOpens = new Map();
const pendingRequests = new Map();
const openStarted = new Map();
const openTimes = [];
// What the bridge sent for each session, and deliveries to the page that
// failed: when a check times out, these say whether the output never came
// from the server or was lost on the way to the terminal.
const sessionOutput = new Map();
let failedDeliveries = 0;
// Which page each session belongs to, so output of a session from an earlier
// check never queues in front of the current page's.
const sessionPages = new Map();
// Messages go to a page in batches, one evaluate in flight per page: a burst
// (seq 1 200000 is thousands of chunks) costs a few round trips instead of
// one per chunk, which on a slow runner backed up for minutes.
const deliveries = new Map();
function deliver(page, message) {
  if (page.isClosed()) return;
  let state = deliveries.get(page);
  if (!state) deliveries.set(page, (state = { queue: [], busy: false }));
  state.queue.push(message);
  if (!state.busy) void flush(page, state);
}
async function flush(page, state) {
  state.busy = true;
  while (state.queue.length && !page.isClosed()) {
    const batch = state.queue.splice(0);
    await page
      .evaluate((messages) => messages.forEach((m) => window.__anixopsE2eReceive?.(m)), batch)
      .catch(() => (failedDeliveries += batch.length));
  }
  state.queue.length = 0;
  state.busy = false;
}
const ready = new Promise((resolve, reject) => {
  bridge.once("exit", (code) => reject(new Error(`web_bridge exited with ${code}`)));
  createInterface({ input: bridge.stdout }).on("line", (line) => {
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      return;
    }
    if (message.op === "ready") resolve();
    else if (message.op === "opened" || message.op === "error") {
      const started = openStarted.get(message.seq);
      if (started !== undefined) {
        const ms = Date.now() - started;
        openTimes.push(ms);
        // A session that takes this long is worth seeing in the log even when the check passes.
        if (ms > 5000) console.log(`slow session open: ${ms} ms (${message.op}${message.message ? `: ${message.message}` : ""})`);
      }
      openStarted.delete(message.seq);
      const pending = pendingOpens.get(message.seq);
      if (pending && message.op === "opened") sessionPages.set(message.id, pending.page);
      pending?.resolve(message);
      pendingOpens.delete(message.seq);
    } else if (message.op === "reply") {
      pendingRequests.get(message.seq)?.(message);
      pendingRequests.delete(message.seq);
    } else if (sessionPages.has(message.id) || bridgePage) {
      if (process.env.DEBUG_BRIDGE) console.log("bridge>", line.slice(0, 160));
      if (message.op === "data") {
        const seen = sessionOutput.get(message.id) ?? { chars: 0, tail: "" };
        seen.chars += message.data.length;
        seen.tail = (seen.tail + message.data).slice(-120);
        sessionOutput.set(message.id, seen);
      }
      deliver(sessionPages.get(message.id) ?? bridgePage, message);
    }
  });
});
await ready;
let seq = 0;
const send = (message) => bridge.stdin.write(JSON.stringify(message) + "\n");

// Runs in the page: the session bridge the app looks for in backend.ts.
function pageBridge({ downloads }) {
  const sinks = new Map();
  const transfers = new Map();
  const tunnelSinks = new Map();
  // Transfer events that came before the enqueue reply.
  const early = new Map();
  // Output that comes before open() has its sink (the prompt can beat the
  // opened reply into the page) waits here instead of being dropped.
  const earlyOutput = new Map();
  window.__anixopsE2eDelivered = {};
  window.__anixopsE2eTerminals = new Map();
  const deliver = (sink, message) => {
    if (message.op === "data") {
      window.__anixopsE2eDelivered[message.id] = (window.__anixopsE2eDelivered[message.id] ?? 0) + message.data.length;
      sink({ kind: "data", data: message.data });
    } else if (message.op === "exit") sink({ kind: "exit", code: message.code, reason: message.reason, signal: message.signal ?? undefined });
  };
  window.__anixopsE2eReceive = (message) => {
    if (message.op === "tunnel") {
      tunnelSinks.get(message.status.id)?.(message.status);
      return;
    }
    if (message.op === "transfer") {
      const sink = transfers.get(message.id);
      if (sink) sink(message.event);
      else early.set(message.id, [...(early.get(message.id) ?? []), message.event]);
      return;
    }
    const sink = sinks.get(message.id);
    if (sink) deliver(sink, message);
    else if (!window.__anixopsE2eOpened.includes(message.id)) earlyOutput.set(message.id, [...(earlyOutput.get(message.id) ?? []), message]);
  };
  window.__anixopsE2eOpened = [];
  window.__anixopsE2eSession = {
    async open(hostId, cols, rows, onEvent) {
      // A test turns the shell integration on for the next connections.
      const opened = await window.__anixopsE2eOpen({ hostId, cols, rows, integration: window.__anixopsE2eIntegration === true });
      if (opened.op === "error") throw new Error(opened.message);
      const id = opened.id;
      sinks.set(id, onEvent);
      window.__anixopsE2eOpened.push(id);
      for (const message of earlyOutput.get(id) ?? []) deliver(onEvent, message);
      earlyOutput.delete(id);
      const post = (message) => window.__anixopsE2ePost({ ...message, id });
      return {
        id,
        engine: "russh",
        engineReason: "end-to-end test",
        write: (data) => void post({ op: "write", data }),
        writeAsync: (data) => post({ op: "write", data }),
        resize: (cols, rows) => void post({ op: "resize", cols, rows }),
        detach: () => sinks.delete(id),
        close: () => {
          sinks.delete(id);
          void post({ op: "close" });
        },
      };
    },
  };
  // Files: the daemon's SFTP and transfer queue, as src-tauri/src/files.rs
  // drives them for a host on the built-in engine.
  const request = async (kind, payload, host = false) => {
    const reply = await window.__anixopsE2eRequest({ kind, payload, host });
    if (reply.error !== undefined) throw new Error(reply.error);
    return reply.value;
  };
  const modeString = (kind, bits = 0) => {
    let text = { dir: "d", file: "-", symlink: "l" }[kind] ?? "?";
    for (let i = 0; i < 9; i++) text += bits & (1 << (8 - i)) ? "rwx"[i % 3] : "-";
    return text;
  };
  const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  const modified = (seconds) => {
    if (typeof seconds !== "number") return "";
    const d = new Date(seconds * 1000);
    const day = String(d.getDate()).padStart(2, " ");
    const recent = Math.abs(Date.now() - d.getTime()) < 180 * 86400e3;
    const tail = recent ? `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}` : ` ${d.getFullYear()}`;
    return `${months[d.getMonth()]} ${day} ${tail}`;
  };
  const kinds = { dir: "directory", file: "file", symlink: "symlink" };
  window.__anixopsE2eUploads = [];
  // Closed when the check ends: the server allows ten channels per connection.
  window.__anixopsE2eSftp = new Set();
  window.__anixopsE2eFiles = {
    async open() {
      const { sftp_id: id, home } = await request("sftp.open", {}, true);
      window.__anixopsE2eSftp.add(id);
      return {
        home,
        list: async (path) =>
          ((await request("sftp.list", { sftp_id: id, path })).entries ?? []).map((entry) => ({
            name: entry.name,
            kind: kinds[entry.kind] ?? "other",
            size: entry.size ?? 0,
            modified: modified(entry.modified),
            permissions: modeString(entry.kind, entry.permissions),
            owner: entry.uid?.toString() ?? null,
            group: entry.gid?.toString() ?? null,
          })),
        mkdir: (path) => request("sftp.mkdir", { sftp_id: id, path }).then(() => {}),
        remove: (path, directory) => request("sftp.remove", { sftp_id: id, path, directory }).then(() => {}),
        rename: (from, to) => request("sftp.rename", { sftp_id: id, from, to }).then(() => {}),
        chmod: (path, mode, recursive) => request("sftp.chmod", { sftp_id: id, path, mode, recursive }).then(() => {}),
        close: () => {
          window.__anixopsE2eSftp.delete(id);
          void request("sftp.close", { sftp_id: id }).catch(() => {});
        },
      };
    },
    async transfer({ upload, remote, local }, onEvent) {
      const name = (path) => path.split("/").filter(Boolean).pop();
      const localPath = upload ? local : (local ?? `${downloads}/${name(remote)}`);
      const remotePath = upload ? `${remote.replace(/\/$/, "")}/${name(local)}` : remote;
      const started = await request(
        "transfer.enqueue",
        { direction: upload ? "upload" : "download", local_path: localPath, remote_path: remotePath, recursive: true },
        true,
      );
      transfers.set(started.transfer_id, onEvent);
      for (const event of early.get(started.transfer_id) ?? []) onEvent(event);
      early.delete(started.transfer_id);
      return { id: started.transfer_id, local: localPath, remote: remotePath, queued: started.queued ?? false };
    },
    cancel: (id) => request("transfer.cancel", { transfer_id: id }).then(() => {}),
    pickUploads: async () => window.__anixopsE2eUploads.splice(0),
  };
  // Forwards: the daemon's tunnels, as src-tauri/src/tunnels.rs starts them
  // for a host on the built-in engine; statuses as daemon_status maps them.
  // The bridge's daemon outlives the page: each page's tunnel ids are its own.
  const tunnelPrefix = `page-${Math.random().toString(36).slice(2, 8)}-`;
  window.__anixopsE2eTunnels = {
    async start(tunnel, onStatus) {
      tunnelSinks.set(tunnelPrefix + tunnel.id, (status) => {
        const next =
          status.state === "starting"
            ? { state: "starting" }
            : status.state === "running"
              ? { state: "running", port: status.bind_port ?? undefined, lastError: status.channel_errors > 0 ? status.last_channel_error : undefined, errorCount: status.channel_errors }
              : status.state === "reconnecting"
                ? { state: "reconnecting", message: status.error ?? undefined }
                : status.state === "stopped"
                  ? { state: "stopped" }
                  : { state: "failed", message: status.error ?? "the tunnel stopped" };
        onStatus(next);
      });
      const body = {
        id: tunnelPrefix + tunnel.id,
        name: tunnel.name,
        host_id: "",
        kind: tunnel.kind,
        bind_address: tunnel.bindAddress,
        bind_port: tunnel.bindPort,
        target_host: tunnel.targetHost,
        target_port: tunnel.targetPort,
      };
      try {
        await request("tunnel.start", { tunnel: body }, true);
      } catch (error) {
        onStatus({ state: "failed", message: String(error.message ?? error) });
      }
    },
    stop: (id) => request("tunnel.stop", { id: tunnelPrefix + id }).then(() => {}),
  };
  // What the app hands to the system browser (links) is recorded here.
  window.__anixopsE2eOpenedUrls = [];
  window.open = (url) => {
    window.__anixopsE2eOpenedUrls.push(String(url));
    return null;
  };
}

// ---- Helpers -------------------------------------------------------------

const browser = await chromium.launch({
  executablePath: process.env.CHROMIUM_PATH || undefined,
  // WebGL as in the app (WebView2, WKWebView, WebKitGTK all render with it).
  args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"],
});

async function newPage({ width = 1200, height = 760, query = "" } = {}) {
  const context = await browser.newContext({
    viewport: { width, height },
    deviceScaleFactor: 1,
    colorScheme: "dark",
    locale: "zh-CN",
    permissions: ["clipboard-read", "clipboard-write"],
  });
  const page = await context.newPage();
  page.dialogs = [];
  page.on("dialog", (dialog) => {
    page.dialogs.push(`${dialog.type()}: ${dialog.message()}`);
    void dialog.dismiss();
  });
  page.errors = [];
  page.on("pageerror", (error) => page.errors.push(error.message));
  await page.exposeFunction("__anixopsE2eOpen", ({ cols, rows, integration }) => {
    const id = ++seq;
    bridgePage = page;
    return new Promise((resolve) => {
      pendingOpens.set(id, { resolve, page });
      openStarted.set(id, Date.now());
      send({ op: "open", seq: id, host: "e2e-target", cols, rows, integration });
    });
  });
  await page.exposeFunction("__anixopsE2ePost", (message) => send(message));
  await page.exposeFunction("__anixopsE2eRequest", ({ kind, payload, host }) => {
    const id = ++seq;
    bridgePage = page;
    return new Promise((resolve) => {
      pendingRequests.set(id, resolve);
      send({ op: "request", seq: id, kind, payload, ...(host ? { host: "e2e-target" } : {}) });
    });
  });
  await page.addInitScript((value) => Object.defineProperty(Navigator.prototype, "platform", { get: () => value }), navigatorPlatform);
  await page.addInitScript(pageBridge, { downloads: downloadsDir });
  await page.goto(`${baseUrl}/?webgl=1${query ? `&${query}` : ""}`);
  await page.waitForLoadState("networkidle");
  return page;
}

/** Opens a host from the list; the bridge connects it to the e2e server. */
async function openHost(page, name = "prod-web-01") {
  await page.getByRole("button", { name: new RegExp(name) }).first().click();
  await page.locator(".xterm-screen").last().waitFor();
  // The engine gives a channel that does not open 15 s before it reconnects;
  // the session-open times are reported at the end.
  await waitScreen(page, /keyuser@/, { timeout: 35000 });
}

/** Text of the active terminal's viewport (or the whole buffer). */
function screen(page, { all = false, index = -1 } = {}) {
  return page.evaluate(
    ({ all, index }) => {
      const terminals = [...window.__anixopsE2eTerminals.values()];
      const terminal = terminals.at(index);
      if (!terminal) return "";
      const buffer = terminal.buffer.active;
      const start = all ? 0 : buffer.viewportY;
      const end = all ? buffer.length : buffer.viewportY + terminal.rows;
      const lines = [];
      for (let y = start; y < end; y++) lines.push(buffer.getLine(y)?.translateToString(true) ?? "");
      return lines.join("\n");
    },
    { all, index },
  );
}

async function waitScreen(page, pattern, { timeout = 15000, all = true, index = -1 } = {}) {
  const deadline = Date.now() + timeout;
  let text = "";
  while (Date.now() < deadline) {
    text = await screen(page, { all, index });
    if (pattern.test(text)) return text;
    await page.waitForTimeout(100);
  }
  const shown = text.split("\n").filter((line) => line.trim()).slice(-12).join("\n");
  throw new Error(`timed out waiting for ${pattern} on screen:\n${shown}\n${await outputReport(page)}`);
}

/** Where the output of this page's sessions got to, for a timeout message. */
async function outputReport(page) {
  const state = await page
    .evaluate(() => ({
      opened: window.__anixopsE2eOpened,
      delivered: window.__anixopsE2eDelivered,
      buffers: [...window.__anixopsE2eTerminals.values()].map((t) => t.buffer.active.type),
    }))
    .catch((error) => ({ error: String(error) }));
  if (state.error) return `page: ${state.error}`;
  const sessions = state.opened.map((id) => {
    const seen = sessionOutput.get(id) ?? { chars: 0, tail: "" };
    return `session ${id}: bridge sent ${seen.chars} chars, page got ${state.delivered[id] ?? 0}, last sent ${JSON.stringify(seen.tail.slice(-60))}`;
  });
  return [...sessions, `buffers: ${state.buffers.join(", ")}; failed deliveries: ${failedDeliveries}`].join("\n");
}

/** Types a command and presses Enter, as a person would. */
async function run(page, command) {
  await page.keyboard.type(command);
  await page.keyboard.press("Enter");
}

/** Runs a command and waits for a marker it prints, so output is complete. */
async function runAndWait(page, command, marker = null, { timeout = 15000 } = {}) {
  const token = marker ?? `done-${Math.random().toString(36).slice(2, 8)}`;
  await run(page, marker ? command : `${command}; echo ${token.slice(0, 5)}''${token.slice(5)}`);
  await waitScreen(page, new RegExp(token), { timeout });
}

/** Output of a command run in the terminal (between two markers). */
async function remoteOutput(page, command) {
  const token = Math.random().toString(36).slice(2, 8);
  await page.locator(".xterm-screen").last().click();
  await run(page, `clear; echo B${token}''; ${command}; echo E${token}''`);
  const text = await waitScreen(page, new RegExp(`^E${token}$`, "m"));
  const lines = text.split("\n");
  const start = lines.findIndex((line) => line === `B${token}`);
  const end = lines.findIndex((line) => line === `E${token}`);
  return lines.slice(start + 1, end).join("\n").trim();
}

function term(page, fn, arg) {
  return page.evaluate(
    ({ source, arg }) => {
      const terminal = [...window.__anixopsE2eTerminals.values()].at(-1);
      return new Function("terminal", "arg", `return (${source})(terminal, arg);`)(terminal, arg);
    },
    { source: fn.toString(), arg },
  );
}

/** Pixel position of a cell (0-based col, viewport row) in the active terminal. */
async function cellPoint(page, col, row) {
  const box = await page.locator(".xterm-screen").last().boundingBox();
  const { cols, rows } = await term(page, (t) => ({ cols: t.cols, rows: t.rows }));
  return { x: box.x + ((col + 0.5) * box.width) / cols, y: box.y + ((row + 0.5) * box.height) / rows };
}

/** Viewport row of the last line containing `text`. */
async function rowOf(page, text) {
  return term(
    page,
    (t, needle) => {
      const buffer = t.buffer.active;
      for (let y = buffer.viewportY + t.rows - 1; y >= buffer.viewportY; y--) {
        const line = buffer.getLine(y)?.translateToString(true) ?? "";
        const col = line.indexOf(needle);
        if (col >= 0) return { row: y - buffer.viewportY, col };
      }
      return null;
    },
    text,
  );
}

const clipboard = (page) => page.evaluate(() => navigator.clipboard.readText());
const setClipboard = (page, text) => page.evaluate((value) => navigator.clipboard.writeText(value), text);

// ---- Checks --------------------------------------------------------------

/** Each check: area, name, what mainstream terminals do, and the test.
 *  The test returns a note (what we saw) or throws. */
const checks = [];
const check = (area, name, mainstream, fn) => checks.push({ area, name, mainstream, fn });

check("连接", "connect", "打开主机即得到远端 shell 提示符", async (page) => {
  await openHost(page);
  await run(page, "echo TERM=$TERM COLORTERM=$COLORTERM LANG=$LANG");
  const text = await waitScreen(page, /^TERM=/m);
  if (!/TERM=xterm-256color/.test(text)) throw new Error("TERM is not xterm-256color");
  // The server's sshd decides whether COLORTERM gets through (Debian and
  // Ubuntu accept only LANG and LC_*); 24-bit colour is announced through
  // XTGETTCAP instead (terminalInput.ts).
  if (!/LANG=\S*UTF-8/i.test(text)) throw new Error("LANG is not a UTF-8 locale");
  return text.match(/^TERM=.*$/m)?.[0];
});

check("显示", "cjk-emoji-width", "中文、emoji 各占两格，后面的字符和光标对齐（wcwidth / Unicode 11+）", async (page) => {
  await openHost(page);
  await run(page, "clear; printf '中文ab😀x|\\n'; printf '12345678|\\n'");
  await waitScreen(page, /^12345678\|$/m);
  const line = await term(page, (t) => {
    const buffer = t.buffer.active;
    for (let y = 0; y < buffer.length; y++) {
      const l = buffer.getLine(y);
      const s = l?.translateToString(true) ?? "";
      if (s.startsWith("中文ab")) {
        const cells = [];
        for (let x = 0; x < 10; x++) cells.push(l.getCell(x)?.getWidth());
        return { s, cells };
      }
    }
    return null;
  });
  if (!line) throw new Error("line not found");
  const pipe = line.cells.slice(0, 9).join("");
  // 中(2,0) 文(2,0) a b 😀(2,0) x | → bar at column 8, like the second line.
  if (pipe !== "202011201".slice(0, pipe.length)) throw new Error(`cell widths ${pipe}`);
  return "中/文/😀 各占 2 格，竖线与下一行第 9 列对齐";
});

check("显示", "truecolor", "24 位真彩色（38;2;r;g;b）按原色显示", async (page) => {
  await openHost(page);
  await run(page, "clear; printf '\\e[38;2;255;100;0mTRUECOLOR\\e[0m\\n'");
  await waitScreen(page, /^TRUECOLOR$/m);
  const color = await term(page, (t) => {
    const buffer = t.buffer.active;
    for (let y = 0; y < buffer.length; y++) {
      const l = buffer.getLine(y);
      if ((l?.translateToString(true) ?? "").startsWith("TRUECOLOR")) {
        const cell = l.getCell(0);
        return { rgb: cell.isFgRGB(), value: cell.getFgColor() };
      }
    }
    return null;
  });
  if (!color?.rgb || color.value !== 0xff6400) throw new Error(`color ${JSON.stringify(color)}`);
  return "前景色 #ff6400 按 RGB 保存";
});


check("显示", "ansi-black-contrast", "ANSI 黑色是深色，htop 表头、tmux 状态栏这类“彩色底黑字”清楚可读", async (page) => {
  await openHost(page);
  const { theme, ratio } = await term(page, (t) => ({ theme: t.options.theme, ratio: t.options.minimumContrastRatio }));
  const luminance = (hex) => {
    const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const contrast = (a, b) => {
    const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
    return (x + 0.05) / (y + 0.05);
  };
  const worst = Math.min(...["green", "cyan", "yellow"].map((name) => contrast(theme.black, theme[name])));
  await run(page, "clear; printf '\\e[30;46m F1Help \\e[30;42m PID USER CPU%% \\e[0m\\n'");
  await waitScreen(page, /^ F1Help/m);
  await page.screenshot({ path: join(outDir, "ansi-black-contrast.png") });
  if (worst < 4.5 && !(ratio >= 4.5)) throw new Error(`黑字在彩色底上的对比度只有 ${worst.toFixed(1)}:1，且未设最低对比度`);
  return `黑色 ${theme.black}，彩色底上最低对比度 ${worst.toFixed(1)}:1，最低对比度保护 ${ratio}`;
});

check("窗口", "sidebar-toggle", "侧栏可收起，让终端占满窗口（Termius、MobaXterm、VS Code）", async (page) => {
  await openHost(page);
  const before = await term(page, (t) => t.cols);
  await page.getByRole("button", { name: "主机", exact: true }).click();
  await page.waitForTimeout(600);
  const after = await term(page, (t) => t.cols);
  await page.screenshot({ path: join(outDir, "sidebar-hidden.png") });
  await page.getByRole("button", { name: "主机", exact: true }).click();
  await page.waitForTimeout(300);
  if (!(after > before)) throw new Error(`收起侧栏后终端宽度没有变化（${before} 列）`);
  const visible = await page.getByPlaceholder(/搜索主机/).isVisible();
  if (!visible) throw new Error("再次点击没有展开侧栏");
  return `${before} 列 → ${after} 列，再次点击展开`;
});

check("窗口", "focus-mode", "突出终端：侧栏底部有收起按钮，终端里 Ctrl+Shift+S 收起/展开；专注模式（F11，macOS ⌃⌘Z）只留标签和终端，重开后记住", async (page) => {
  await openHost(page);
  const cols = () => term(page, (t) => t.cols);
  const settle = () => page.waitForTimeout(600);
  const base = await cols();
  await page.getByRole("button", { name: "收起侧栏" }).click();
  await settle();
  const folded = await cols();
  if (!(folded > base)) throw new Error(`点“收起侧栏”后终端没有变宽（${base} 列）`);
  await page.locator(".xterm-screen").last().click();
  await page.keyboard.press(isMacRun ? "Control+Meta+S" : "Control+Shift+S");
  await settle();
  if (!(await page.getByPlaceholder(/搜索主机/).isVisible())) throw new Error("终端里按快捷键没有展开侧栏");
  await page.getByRole("button", { name: "专注模式" }).click();
  await settle();
  const focused = await cols();
  if (await page.getByRole("navigation", { name: "AnixOps SSH" }).count()) throw new Error("专注模式下左侧图标栏还在");
  if (!(focused > folded)) throw new Error(`专注模式没有让终端更宽（${folded} → ${focused} 列）`);
  await page.screenshot({ path: join(outDir, "focus-mode-on.png") });
  // Remembered: a new window of the app opens in focus mode.
  await page.reload();
  await page.waitForLoadState("networkidle");
  await openHost(page);
  if (await page.getByRole("navigation", { name: "AnixOps SSH" }).count()) throw new Error("重开后没有记住专注模式");
  await page.locator(".xterm-screen").last().click();
  await page.keyboard.press(isMacRun ? "Control+Meta+Z" : "F11");
  await page.getByRole("navigation", { name: "AnixOps SSH" }).waitFor({ timeout: 3000 });
  await page.keyboard.press(isMacRun ? "Control+Meta+Z" : "F11");
  await page.getByRole("button", { name: "退出专注模式" }).click();
  await page.getByRole("navigation", { name: "AnixOps SSH" }).waitFor({ timeout: 3000 });
  return `${base} 列 → 收起侧栏 ${folded} 列 → 专注模式 ${focused} 列；快捷键和“退出专注模式”按钮可用，重开后记住`;
});

check("复制粘贴", "copy-shortcut", "选中后 Ctrl+Shift+C（macOS ⌘C）复制", async (page) => {
  await openHost(page);
  await runAndWait(page, "echo copy-me-please");
  const at = await rowOf(page, "copy-me-please");
  const point = await cellPoint(page, at.col + 2, at.row);
  await page.mouse.dblclick(point.x, point.y);
  await page.keyboard.press(`${mod}+Shift+C`);
  if (isMacRun) await page.keyboard.press("Meta+C");
  const copied = await clipboard(page);
  if (copied !== "copy-me-please") throw new Error(`clipboard: ${JSON.stringify(copied)}`);
  return "双击选中整词，快捷键复制";
});

check("复制粘贴", "ctrl-c-copies-selection", "Windows Terminal：有选中时 Ctrl+C 复制（并清除选中），无选中时发送 ^C", async (page) => {
  if (isMacRun) return "macOS 用 ⌘C，不适用";
  await openHost(page);
  await setClipboard(page, "");
  await runAndWait(page, "echo select-for-ctrl-c");
  const at = await rowOf(page, "select-for-ctrl-c");
  const point = await cellPoint(page, at.col + 1, at.row);
  await page.mouse.click(point.x, point.y, { clickCount: 3 });
  await page.keyboard.press("Control+C");
  const copied = (await clipboard(page)).trim();
  if (copied !== "select-for-ctrl-c") throw new Error(`Ctrl+C 没有复制选中内容（剪贴板：${JSON.stringify(copied)}）`);
  // Without a selection, Ctrl+C still interrupts.
  await run(page, "sleep 30");
  await page.waitForTimeout(300);
  await page.keyboard.press("Control+C");
  await waitScreen(page, /\^C/);
  return "有选中复制，无选中中断";
});

check("复制粘贴", "ctrl-v-pastes", "Windows Terminal：Ctrl+V 粘贴；Ctrl+Shift+V 也粘贴", async (page) => {
  if (isMacRun) return "macOS 用 ⌘V，不适用";
  await openHost(page);
  await setClipboard(page, "echo pasted-$((40+2))");
  await page.keyboard.press("Control+V");
  await waitScreen(page, /echo pasted-\$/);
  await page.keyboard.press("Enter");
  await waitScreen(page, /pasted-42/);
  await setClipboard(page, "echo shift-pasted-$((40+3))");
  await page.keyboard.press("Control+Shift+V");
  await waitScreen(page, /echo shift-pasted-\$/);
  await page.keyboard.press("Enter");
  await waitScreen(page, /shift-pasted-43/);
  return "两种快捷键都把剪贴板内容送进远端";
});

check("复制粘贴", "multiline-paste", "多行粘贴：shell 开了括号粘贴（bash 5.1+）时直接粘贴不执行；否则先确认", async (page) => {
  await openHost(page);
  await setClipboard(page, "echo line-one\necho line-two");
  await page.keyboard.press(isMacRun ? "Meta+V" : "Control+Shift+V");
  await page.waitForTimeout(500);
  const dialog = await page.getByRole("dialog").count();
  const text = await screen(page);
  if (dialog > 0) return "弹出确认（shell 未开括号粘贴）";
  if (/^line-one$/m.test(text)) throw new Error("多行内容被直接执行了");
  await page.keyboard.press("Enter");
  await waitScreen(page, /^line-two$/m);
  return "bash 括号粘贴：两行一起放进命令行，按回车才执行";
});

check("复制粘贴", "osc52", "远端程序（tmux、vim、nvim）用 OSC 52 写本机剪贴板", async (page) => {
  await openHost(page);
  await setClipboard(page, "");
  await runAndWait(page, "printf '\\e]52;c;%s\\a' $(printf from-osc52 | base64)");
  await page.waitForTimeout(300);
  const copied = await clipboard(page);
  if (copied !== "from-osc52") throw new Error(`clipboard: ${JSON.stringify(copied)}`);
  return "剪贴板得到 from-osc52";
});

check("复制粘贴", "right-click-menu", "右键：应用自己的菜单（复制/粘贴/清屏…），不是浏览器默认菜单", async (page) => {
  await openHost(page);
  const box = await page.locator(".xterm-screen").last().boundingBox();
  await page.mouse.click(box.x + 200, box.y + 100, { button: "right" });
  const menu = page.getByRole("menu");
  await menu.waitFor({ timeout: 3000 });
  const items = await menu.getByRole("menuitem").allInnerTexts();
  await page.screenshot({ path: join(outDir, "right-click-menu.png") });
  await page.keyboard.press("Escape");
  return `菜单项：${items.map((s) => s.split("\n")[0]).join("、")}`;
});

check("选择", "triple-click-line", "三击选中整行", async (page) => {
  await openHost(page);
  await runAndWait(page, "echo alpha beta gamma");
  const at = await rowOf(page, "alpha beta gamma");
  const point = await cellPoint(page, at.col + 7, at.row);
  await page.mouse.click(point.x, point.y, { clickCount: 3 });
  const selection = await term(page, (t) => t.getSelection());
  if (selection.trim() !== "alpha beta gamma") throw new Error(`selection: ${JSON.stringify(selection)}`);
  return "整行选中";
});

check("选择", "shift-click-extend", "Shift+单击扩展选区", async (page) => {
  await openHost(page);
  await runAndWait(page, "echo one two three four");
  const at = await rowOf(page, "one two three four");
  const a = await cellPoint(page, at.col, at.row);
  const b = await cellPoint(page, at.col + 12, at.row);
  await page.mouse.click(a.x, a.y);
  await page.keyboard.down("Shift");
  await page.mouse.click(b.x, b.y);
  await page.keyboard.up("Shift");
  const selection = await term(page, (t) => t.getSelection());
  if (!selection.startsWith("one two thre")) throw new Error(`selection: ${JSON.stringify(selection)}`);
  return "选区扩展到点击处";
});

check("选择", "select-in-mouse-app", "程序开了鼠标（tmux/htop）时，Shift+拖动（macOS ⌥+拖动）仍可选文字", async (page) => {
  await openHost(page);
  await run(page, "clear; printf 'mouse-mode-text\\n\\e[?1000h\\e[?1006h'");
  await waitScreen(page, /^mouse-mode-text$/m);
  await page.waitForFunction(() => [...window.__anixopsE2eTerminals.values()].at(-1).modes.mouseTrackingMode !== "none");
  const at = await rowOf(page, "mouse-mode-text");
  const a = await cellPoint(page, at.col, at.row);
  const b = await cellPoint(page, at.col + 9, at.row);
  const modifier = isMacRun ? "Alt" : "Shift";
  await page.keyboard.down(modifier);
  await page.mouse.move(a.x, a.y);
  await page.mouse.down();
  await page.mouse.move(b.x, b.y, { steps: 5 });
  await page.mouse.up();
  await page.keyboard.up(modifier);
  const selection = await term(page, (t) => t.getSelection());
  await run(page, "printf '\\e[?1000l\\e[?1006l'");
  if (!selection.startsWith("mouse-mod")) throw new Error(`selection: ${JSON.stringify(selection)}`);
  return `${modifier}+拖动选中 "${selection}"`;
});

check("滚动", "wheel-scrollback", "滚轮翻看历史输出", async (page) => {
  await openHost(page);
  await runAndWait(page, "seq 1 2000");
  const box = await page.locator(".xterm-screen").last().boundingBox();
  await page.mouse.move(box.x + 200, box.y + 200);
  for (let i = 0; i < 5; i++) await page.mouse.wheel(0, -600);
  await page.waitForTimeout(300);
  const { viewportY, baseY } = await term(page, (t) => ({ viewportY: t.buffer.active.viewportY, baseY: t.buffer.active.baseY }));
  if (viewportY >= baseY) throw new Error("did not scroll");
  return `向上滚动 ${baseY - viewportY} 行`;
});

check("滚动", "shift-pageup", "Shift+PageUp / Shift+PageDown 按页翻历史", async (page) => {
  await openHost(page);
  await runAndWait(page, "seq 1 500");
  await page.keyboard.press("Shift+PageUp");
  await page.waitForTimeout(200);
  const { viewportY, baseY } = await term(page, (t) => ({ viewportY: t.buffer.active.viewportY, baseY: t.buffer.active.baseY }));
  if (viewportY >= baseY) throw new Error("Shift+PageUp did not scroll");
  await page.keyboard.press("Shift+PageDown");
  return `翻上一页（${baseY - viewportY} 行）`;
});

check("滚动", "wheel-in-less", "less/man（备用屏、未开鼠标）中滚轮翻页而不是翻命令历史", async (page) => {
  await openHost(page);
  await run(page, "seq 1 300 | less");
  await waitScreen(page, /^1$/m, { all: false });
  const box = await page.locator(".xterm-screen").last().boundingBox();
  await page.mouse.move(box.x + 200, box.y + 200);
  for (let i = 0; i < 3; i++) await page.mouse.wheel(0, 300);
  await page.waitForTimeout(500);
  const top = (await screen(page)).split("\n")[0].trim();
  await page.keyboard.press("q");
  if (top === "1") throw new Error("less did not scroll");
  return `滚动后首行为 ${top}`;
});

check("搜索", "find", "Ctrl+Shift+F（⌘F）在输出中查找，显示匹配数，回车跳下一个", async (page) => {
  await openHost(page);
  await runAndWait(page, "for i in 1 2 3; do echo needle-$i; done");
  await page.keyboard.press(isMacRun ? "Meta+F" : "Control+Shift+F");
  await page.keyboard.type("needle-");
  await page.keyboard.press("Enter");
  await page.waitForTimeout(400);
  await page.screenshot({ path: join(outDir, "find.png") });
  const bar = await page.locator("text=/\\d+\\s*\\/\\s*\\d+/").first().innerText().catch(() => "");
  await page.keyboard.press("Escape");
  if (!/\/\s*[34]/.test(bar)) throw new Error(`match count: ${JSON.stringify(bar)}`);
  return `匹配计数 ${bar.trim()}`;
});

check("链接", "url-click", "普通 URL：Ctrl+单击（⌘+单击）用系统浏览器打开", async (page) => {
  await openHost(page);
  await runAndWait(page, "echo see https://example.com/plain-link for details");
  const at = await rowOf(page, "https://example.com/plain-link");
  const point = await cellPoint(page, at.col + 5, at.row);
  await page.mouse.move(point.x, point.y);
  await page.waitForTimeout(300);
  await page.keyboard.down(isMacRun ? "Meta" : "Control");
  await page.mouse.click(point.x, point.y);
  await page.keyboard.up(isMacRun ? "Meta" : "Control");
  await page.waitForTimeout(300);
  const urls = await page.evaluate(() => window.__anixopsE2eOpenedUrls);
  if (!urls.includes("https://example.com/plain-link")) throw new Error(`opened: ${JSON.stringify(urls)}`);
  return "打开 https://example.com/plain-link";
});

check("链接", "osc8-hyperlink", "OSC 8 超链接（ls --hyperlink、gcc、systemd）：Ctrl+单击打开，不弹浏览器确认框", async (page) => {
  await openHost(page);
  await runAndWait(page, "printf '\\e]8;;https://example.com/osc8\\e\\\\osc8-link-text\\e]8;;\\e\\\\\\n'");
  const at = await rowOf(page, "osc8-link-text");
  const point = await cellPoint(page, at.col + 3, at.row);
  await page.mouse.move(point.x, point.y);
  await page.waitForTimeout(300);
  await page.keyboard.down(isMacRun ? "Meta" : "Control");
  await page.mouse.click(point.x, point.y);
  await page.keyboard.up(isMacRun ? "Meta" : "Control");
  await page.waitForTimeout(300);
  const urls = await page.evaluate(() => window.__anixopsE2eOpenedUrls);
  if (page.dialogs.length) throw new Error(`弹出了浏览器对话框：${page.dialogs.join(" | ")}`);
  if (!urls.includes("https://example.com/osc8")) throw new Error(`opened: ${JSON.stringify(urls)}`);
  return "打开 https://example.com/osc8";
});

check("标题与提醒", "title", "远端用 OSC 0/2 设置标题，标签上能看到", async (page) => {
  await openHost(page);
  // bash sets its own title at every prompt, so the program keeps running.
  await run(page, "printf '\\e]0;remote-title-xyz\\a'; sleep 5");
  await page.waitForTimeout(800);
  const visible = await page.getByText("remote-title-xyz").count();
  const titled = await page.locator('[title*="remote-title-xyz"]').count();
  if (visible === 0 && titled === 0) throw new Error("标题没有出现在标签上");
  return visible > 0 ? "标签文字显示远端标题" : "标签显示主机名，远端标题在悬停提示里（同 Termius；Windows Terminal 直接显示标题）";
});

check("输入", "ime-insert", "输入法上屏的中文直接进入远端", async (page) => {
  await openHost(page);
  await page.keyboard.type("echo ");
  await waitScreen(page, /\$ echo $/m);
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Input.imeSetComposition", { text: "ni", selectionStart: 2, selectionEnd: 2 });
  await page.waitForTimeout(100);
  await cdp.send("Input.imeSetComposition", { text: "你好", selectionStart: 2, selectionEnd: 2 });
  await page.waitForTimeout(100);
  await cdp.send("Input.insertText", { text: "你好世界" });
  await waitScreen(page, /\$ echo 你好世界$/m);
  await page.keyboard.press("Enter");
  await waitScreen(page, /^你好世界$/m);
  return "组字后上屏的“你好世界”被远端回显";
});

check("输入", "keys", "方向键、Home/End、Ctrl+←/→ 按词移动、Alt+. 取上一个参数", async (page) => {
  await openHost(page);
  await run(page, "echo first-arg last-arg");
  await waitScreen(page, /^first-arg last-arg$/m);
  await page.keyboard.type("echo X");
  await page.keyboard.press("Home");
  await page.keyboard.press("Control+ArrowRight");
  await page.keyboard.type(" W");
  await page.keyboard.press("End");
  await page.keyboard.press("Alt+.");
  await page.keyboard.press("Enter");
  await waitScreen(page, /^W Xlast-arg$/m);
  return "编辑结果正确";
});

check("全屏程序", "vim", "vim：备用屏、颜色、退出后恢复原屏幕", async (page) => {
  await openHost(page);
  await runAndWait(page, "echo before-vim");
  await run(page, "vim -u NONE -c 'syntax on' /etc/passwd");
  await waitScreen(page, /root:x:0:0/, { all: false });
  await page.screenshot({ path: join(outDir, "vim.png") });
  await page.keyboard.type(":q!");
  await page.keyboard.press("Enter");
  await waitScreen(page, /before-vim/, { all: false });
  return "进出 vim 正常，原屏幕恢复";
});

check("全屏程序", "tmux", "tmux：状态栏、Ctrl+B % 分屏、窗口大小随终端变化", async (page) => {
  await openHost(page);
  const name = `e2e${Date.now() % 100000}`;
  await run(page, `tmux -f /dev/null new -s ${name}`);
  await waitScreen(page, new RegExp(`\\[${name}\\]`), { all: false });
  await page.keyboard.press("Control+B");
  await page.keyboard.type("%");
  await page.waitForTimeout(500);
  await run(page, "tmux list-panes | wc -l");
  await waitScreen(page, /[│|]2\s*$|^2\s*$/m, { all: false });
  await page.screenshot({ path: join(outDir, "tmux.png") });
  await run(page, `tmux kill-session -t ${name}`);
  await waitScreen(page, /exited|keyuser@/);
  return "状态栏显示，分出两个窗格";
});

check("全屏程序", "htop", "htop/btop：全屏 TUI 正确绘制（鼠标点击见另一线程）", async (page) => {
  await openHost(page);
  await run(page, "htop");
  await waitScreen(page, /F1Help/, { all: false });
  await page.waitForTimeout(800);
  await page.screenshot({ path: join(outDir, "htop.png") });
  await page.keyboard.press("q");
  return "htop 正常绘制";
});

check("窗口", "resize", "窗口变化后远端 stty size 跟着变", async (page) => {
  await openHost(page);
  await page.setViewportSize({ width: 900, height: 560 });
  // The side panes refit after a resize (the monitor folds in a narrow
  // window), so the size settles in steps: wait until it holds still.
  const size = () => term(page, (t) => `${t.rows} ${t.cols}`);
  let settled = await size();
  for (let i = 0; i < 20; i++) {
    await page.waitForTimeout(250);
    const next = await size();
    if (next === settled && i > 2) break;
    settled = next;
  }
  // The shell sees the size the terminal has now.
  let text = "";
  for (let attempt = 0; attempt < 3; attempt++) {
    text = await remoteOutput(page, "stty size");
    if (text === (await size())) return `stty size = ${text}`;
    await page.waitForTimeout(500);
  }
  throw new Error(`stty size ${text}，终端是 ${await size()}`);
});

check("字体", "zoom", "Ctrl+=/Ctrl+-/Ctrl+0（⌘+/⌘-/⌘0）放大、缩小、复位字号", async (page) => {
  await openHost(page);
  const before = await term(page, (t) => t.options.fontSize);
  await page.keyboard.press(`${mod}+Equal`);
  await page.waitForTimeout(300);
  const bigger = await term(page, (t) => t.options.fontSize);
  await page.keyboard.press(`${mod}+Minus`);
  await page.keyboard.press(`${mod}+Minus`);
  await page.waitForTimeout(300);
  const smaller = await term(page, (t) => t.options.fontSize);
  await page.keyboard.press(`${mod}+Digit0`);
  await page.waitForTimeout(300);
  const reset = await term(page, (t) => t.options.fontSize);
  if (!(bigger > before && smaller < before && reset === before)) throw new Error(`字号 ${before} → ${bigger} → ${smaller} → ${reset}`);
  return `字号 ${before} → ${bigger} → ${smaller} → ${reset}`;
});

check("字体", "ctrl-wheel-zoom", "Ctrl+滚轮缩放字号（Windows Terminal、WezTerm）", async (page) => {
  if (isMacRun) return "macOS 无此习惯";
  await openHost(page);
  const before = await term(page, (t) => t.options.fontSize);
  const box = await page.locator(".xterm-screen").last().boundingBox();
  await page.mouse.move(box.x + 100, box.y + 100);
  await page.keyboard.down("Control");
  await page.mouse.wheel(0, -100);
  await page.keyboard.up("Control");
  await page.waitForTimeout(300);
  const after = await term(page, (t) => t.options.fontSize);
  await page.keyboard.press("Control+Digit0");
  if (!(after > before)) throw new Error(`字号 ${before} → ${after}`);
  return `字号 ${before} → ${after}`;
});

check("字体", "ligatures", "连字：Fira Code、Cascadia Code 等字体把 => != 显示成连字（Windows Terminal、WezTerm、iTerm2）", async (page) => {
  await page.getByRole("button", { name: "设置" }).click();
  await page.locator('[data-settings-nav="terminal"]').click();
  await page.getByRole("switch", { name: "连字" }).click();
  await page.getByRole("button", { name: "主机", exact: true }).click();
  await openHost(page);
  await run(page, "clear; echo '=> != -> === <= >='");
  await page.waitForTimeout(300);
  await page.screenshot({ path: join(outDir, "ligatures.png") });
  const renderer = await page.evaluate(() => {
    const screen = document.querySelector(".terminal-ligatures .xterm-rows");
    return screen ? getComputedStyle(screen).fontVariantLigatures : null;
  });
  if (!renderer || !/common-ligatures/.test(renderer)) throw new Error(`连字样式未生效：${renderer}`);
  return `DOM 渲染，font-variant-ligatures: ${renderer}`;
});

check("标签与分屏", "split", "Ctrl+Shift+D 分屏后新窗格也连上同一主机", async (page) => {
  await openHost(page);
  await page.keyboard.press(isMacRun ? "Meta+D" : "Control+Shift+D");
  await page.waitForFunction(() => window.__anixopsE2eTerminals.size >= 2);
  await waitScreen(page, /keyuser@/, { index: -1 });
  await page.screenshot({ path: join(outDir, "split.png") });
  return "两个窗格各有一个会话";
});

check("标签与分屏", "tab-switch", "Ctrl+Tab / Ctrl+Shift+Tab 切换标签", async (page) => {
  await openHost(page, "prod-web-01");
  await openHost(page, "staging-api");
  const activeBefore = await page.getByRole("tab", { selected: true }).innerText();
  await page.keyboard.press("Control+Tab");
  await page.waitForTimeout(300);
  const activeAfter = await page.getByRole("tab", { selected: true }).innerText();
  if (activeBefore === activeAfter) throw new Error(`Ctrl+Tab 没有切换（停在 ${activeBefore}）`);
  await page.keyboard.press("Control+Shift+Tab");
  await page.waitForTimeout(300);
  const back = await page.getByRole("tab", { selected: true }).innerText();
  if (back !== activeBefore) throw new Error("Ctrl+Shift+Tab 没有切回");
  return `${activeBefore.split("\n")[0]} ⇄ ${activeAfter.split("\n")[0]}`;
});


check("标签与分屏", "duplicate-tab", "标签右键“复制标签”再开一个同主机会话（Windows Terminal、Termius、MobaXterm）", async (page) => {
  await openHost(page);
  await page.getByRole("tab").first().click({ button: "right" });
  await page.getByRole("menuitem", { name: "复制标签" }).click();
  await page.waitForFunction(() => window.__anixopsE2eOpened.length >= 2, null, { timeout: 15000 });
  await waitScreen(page, /keyuser@/, { index: -1 });
  const count = await page.getByRole("tab").count();
  if (count !== 2) throw new Error(`标签数 ${count}`);
  return "两个标签，各自一个会话";
});

check("标签与分屏", "open-host-new-tab", "同一主机再开一个标签（主机右键“在新标签中打开”）", async (page) => {
  await openHost(page);
  await page.getByRole("button", { name: /prod-web-01/ }).first().click({ button: "right" });
  await page.getByRole("menuitem", { name: "在新标签中打开" }).click();
  await page.waitForFunction(() => window.__anixopsE2eOpened.length >= 2, null, { timeout: 15000 });
  const count = await page.getByRole("tab").count();
  if (count !== 2) throw new Error(`标签数 ${count}`);
  return "同一主机两个标签";
});

check("标签与分屏", "tab-drag", "拖动标签调整顺序", async (page) => {
  await openHost(page, "prod-web-01");
  await openHost(page, "staging-api");
  const names = async () => (await page.getByRole("tab").allInnerTexts()).map((text) => text.split("\n")[0]);
  const before = await names();
  await page.getByRole("tab").nth(1).dragTo(page.getByRole("tab").nth(0));
  await page.waitForTimeout(300);
  const after = await names();
  if (after[0] !== before[1]) throw new Error(`顺序 ${before.join(",")} → ${after.join(",")}`);
  return `${before.join(" | ")} → ${after.join(" | ")}`;
});

check("标签与分屏", "tab-rename", "给标签改名：双击标签名或右键“重命名标签”（Windows Terminal、iTerm2、MobaXterm）", async (page) => {
  await openHost(page);
  await page.getByRole("tab").first().click({ button: "right" });
  await page.getByRole("menuitem", { name: "重命名标签" }).click();
  const field = page.getByRole("textbox", { name: "重命名标签" });
  await field.fill("数据库主库");
  await field.press("Enter");
  await page.getByRole("tab", { name: /数据库主库/ }).waitFor({ timeout: 3000 });
  // Escape keeps the name.
  await page.getByText("数据库主库").dblclick();
  await page.getByRole("textbox", { name: "重命名标签" }).fill("不要");
  await page.getByRole("textbox", { name: "重命名标签" }).press("Escape");
  await page.waitForTimeout(200);
  if ((await page.getByRole("tab", { name: /数据库主库/ }).count()) !== 1) throw new Error("Esc 没有保留原名");
  return "右键或双击改名，回车确认，Esc 取消";
});

check("会话", "exit-reconnect", "会话结束后显示原因，按回车重新连接", async (page) => {
  await openHost(page);
  await run(page, "exit");
  await page.waitForTimeout(800);
  await page.screenshot({ path: join(outDir, "exit.png") });
  await page.locator(".xterm-screen").last().click();
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.__anixopsE2eOpened.length >= 2, null, { timeout: 15000 });
  await waitScreen(page, /logout[\s\S]*keyuser@/);
  await page.waitForTimeout(500);
  // The new session starts below the old output; nothing of the old screen
  // may show through under the new prompt.
  const lines = (await screen(page, { all: true })).split("\n");
  const prompt = lines.findLastIndex((line) => /keyuser@/.test(line));
  const below = lines.slice(prompt + 1).filter((line) => line.trim());
  if (below.length) throw new Error(`新会话的提示符下面还残留旧内容：${JSON.stringify(below.slice(0, 2))}`);
  return "回车后重新连上，屏幕干净";
});

check("会话", "reconnect-divider", "重新连接后出现一行淡色分隔线，说明这是新的 shell；首次连接没有", async (page) => {
  await openHost(page);
  if (/重新连接/.test(await screen(page, { all: true }))) throw new Error("首次连接不该有分隔线");
  await run(page, "exit");
  await page.waitForTimeout(800);
  await page.locator(".xterm-screen").last().click();
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.__anixopsE2eOpened.length >= 2, null, { timeout: 15000 });
  await waitScreen(page, /── 已重新连接 · \d\d:\d\d · 这是新的 shell，之前运行的程序已结束 ──\s*\n[\s\S]*keyuser@/);
  return "重连后有分隔线";
});

check("会话", "keep-session-tmux", "保留会话（tmux）：断开后重连回到同一屏幕，之前的输出还在", async (page) => {
  await page.evaluate(() => localStorage.setItem("anixops.keepSession.v1", JSON.stringify({ 1: "tmux" })));
  await openHost(page);
  const mark = `kept-${Date.now() % 100000}`;
  await waitScreen(page, /\[anixops\]/, { all: false });
  await runAndWait(page, `echo ${mark.slice(0, 5)}''${mark.slice(5)}`, mark);
  // Detach: the shell the attach replaced ends, the tmux session stays.
  await page.keyboard.press("Control+B");
  await page.keyboard.type("d");
  await page.waitForTimeout(800);
  await page.locator(".xterm-screen").last().click();
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.__anixopsE2eOpened.length >= 2, null, { timeout: 15000 });
  await waitScreen(page, /已回到主机上的 tmux 会话/);
  await waitScreen(page, new RegExp(mark), { all: false });
  await run(page, "tmux kill-session -t anixops");
  return "重连回到同一个 tmux 会话，之前的输出还在";
});

check("性能", "bulk-output", "大量输出（20 万行）不卡死，几秒内结束", async (page) => {
  await openHost(page);
  const started = Date.now();
  await runAndWait(page, "seq 1 200000", null, { timeout: 40000 });
  const seconds = (Date.now() - started) / 1000;
  if (seconds > 30) throw new Error(`用了 ${seconds.toFixed(1)} 秒`);
  return `${seconds.toFixed(1)} 秒`;
});


// ---- Files (SFTP) --------------------------------------------------------

/** Makes a fresh folder on the server with a few files, from the terminal. */
async function remoteFixture(page) {
  const dir = `/tmp/e2e-files-${Math.random().toString(36).slice(2, 8)}`;
  await runAndWait(
    page,
    `mkdir -p ${dir}/sub && printf 'hello\\n' > ${dir}/a.txt && head -c 300000 /dev/urandom > ${dir}/big.bin && touch ${dir}/.hidden && cd ${dir}`,
  );
  return dir;
}

/** Opens the files pane of the active tab and goes to `dir`. */
async function openFilesAt(page, dir) {
  await page.keyboard.press(isMacRun ? "Meta+Shift+E" : "Control+Shift+E");
  const pane = page.getByRole("complementary", { name: "文件" });
  const path = pane.getByRole("textbox", { name: "路径" });
  await path.waitFor({ timeout: 15000 });
  await path.fill(dir);
  await path.press("Enter");
  await pane.getByRole("listbox", { name: dir, exact: true }).waitFor({ timeout: 15000 });
  return pane;
}

check("文件", "sftp-browse", "打开文件面板列出远端目录：文件夹在前，显示大小、时间、权限（Termius、FinalShell、MobaXterm）", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  const list = pane.getByRole("listbox", { name: dir });
  await list.getByRole("option", { name: /a\.txt/ }).waitFor();
  const names = await list.getByRole("option").allInnerTexts();
  await page.screenshot({ path: join(outDir, "sftp-browse.png") });
  return names.map((n) => n.replace(/\s+/g, " ")).join(" / ");
});

/** The listbox of the shown folder and one row of it. */
const filesList = (pane, dir) => pane.getByRole("listbox", { name: dir, exact: true });
const fileRow = (pane, name) => pane.locator(`[role=option][data-name="${name}"]`);


check("文件", "sftp-navigate", "双击文件夹进入，“上级目录”和 Backspace 返回（FinalShell、MobaXterm、WinSCP）", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  await fileRow(pane, "sub").dblclick();
  await filesList(pane, `${dir}/sub`).waitFor({ timeout: 10000 });
  await pane.getByRole("button", { name: "上级目录" }).click();
  await filesList(pane, dir).waitFor({ timeout: 10000 });
  await fileRow(pane, "a.txt").click();
  await page.keyboard.press("Backspace");
  const up = await filesList(pane, "/tmp").waitFor({ timeout: 3000 }).then(() => true, () => false);
  if (!up) throw new Error("双击、上级目录可用；在列表里按 Backspace 不会回到上级目录");
  return "双击进入 sub，上级目录返回，Backspace 返回 /tmp";
});

check("文件", "sftp-upload", "上传本地文件到当前目录，完成后列表里出现，内容一致", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const local = join(localDir, `upload-${Date.now()}.txt`);
  await writeFile(local, "uploaded from the e2e test\n".repeat(5000));
  const pane = await openFilesAt(page, dir);
  await page.evaluate((path) => window.__anixopsE2eUploads.push(path), local);
  await pane.getByRole("button", { name: "上传" }).click();
  const name = local.split("/").pop();
  await fileRow(pane, name).waitFor({ timeout: 15000 });
  const sum = await remoteOutput(page, `md5sum < ${dir}/${name} | cut -c1-32`);
  const { createHash } = await import("node:crypto");
  const { readFile } = await import("node:fs/promises");
  const expected = createHash("md5").update(await readFile(local)).digest("hex");
  if (sum !== expected) throw new Error(`远端 md5 ${sum}，本地 ${expected}`);
  await page.screenshot({ path: join(outDir, "sftp-upload.png") });
  return `${name}（130 KB）上传后 md5 一致，列表自动刷新`;
});

check("文件", "sftp-download", "下载文件到本地“下载”文件夹，传输列表显示进度和完成", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  const row = fileRow(pane, "big.bin");
  await row.hover();
  await row.getByRole("button", { name: "下载" }).click();
  await pane.getByText("完成", { exact: true }).first().waitFor({ timeout: 20000 });
  const { stat } = await import("node:fs/promises");
  const size = (await stat(join(downloadsDir, "big.bin"))).size;
  if (size !== 300000) throw new Error(`本地文件 ${size} 字节`);
  return "big.bin 300000 字节，传输列表显示完成";
});

check("文件", "sftp-mkdir-rename-delete", "新建文件夹、F2 重命名、Delete 删除（带确认），远端随之改变", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  await pane.getByRole("button", { name: "新建文件夹" }).click();
  await pane.getByPlaceholder("文件夹名称").fill("made-here");
  await pane.getByPlaceholder("文件夹名称").press("Enter");
  await fileRow(pane, "made-here").waitFor({ timeout: 10000 });
  await fileRow(pane, "a.txt").click();
  await page.keyboard.press("F2");
  const input = pane.getByPlaceholder(/新名称/);
  await input.fill("renamed.txt");
  await input.press("Enter");
  await fileRow(pane, "renamed.txt").waitFor({ timeout: 10000 });
  await fileRow(pane, "renamed.txt").click();
  await page.keyboard.press("Delete");
  await page.getByRole("alertdialog").getByRole("button", { name: "删除" }).click();
  await fileRow(pane, "renamed.txt").waitFor({ state: "detached", timeout: 10000 });
  const listing = await remoteOutput(page, `ls ${dir}`);
  if (!/made-here/.test(listing) || /a\.txt|renamed/.test(listing)) throw new Error(`远端：${listing}`);
  return `远端现在：${listing.replace(/\s+/g, " ")}`;
});

check("文件", "sftp-chmod", "属性里改权限（八进制或勾选），远端 ls -l 随之改变", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  await fileRow(pane, "a.txt").click();
  await page.keyboard.press("Alt+Enter");
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("八进制").fill("750");
  await dialog.getByRole("button", { name: "应用" }).click();
  await dialog.waitFor({ state: "detached", timeout: 10000 });
  const mode = await remoteOutput(page, `stat -c %a ${dir}/a.txt`);
  if (mode !== "750") throw new Error(`远端权限 ${mode}`);
  return "a.txt → 750";
});

check("文件", "sftp-multi-select", "Ctrl+点击多选、Shift+点击连选，批量下载", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  await fileRow(pane, ".hidden").click();
  await fileRow(pane, "big.bin").click({ modifiers: ["Shift"] });
  const text = await pane.getByText(/已选 \d+ 项/).innerText();
  if (text !== "已选 3 项") throw new Error(text);
  await fileRow(pane, "a.txt").click({ modifiers: [isMacRun ? "Meta" : "Control"] });
  const after = await pane.getByText(/已选 \d+ 项/).innerText();
  if (after !== "已选 2 项") throw new Error(`Ctrl+点击取消一项后：${after}`);
  return "Shift 连选 3 项，Ctrl 取消 1 项";
});

check("文件", "sftp-path", "路径栏输入 ~、绝对路径、文件路径都能打开（FinalShell、WinSCP）", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  const path = pane.getByRole("textbox", { name: "路径" });
  await path.fill("~");
  await path.press("Enter");
  await filesList(pane, "/home/keyuser").waitFor({ timeout: 10000 });
  await path.fill(`${dir}/a.txt`);
  await path.press("Enter");
  await filesList(pane, dir).waitFor({ timeout: 10000 });
  const selected = await fileRow(pane, "a.txt").getAttribute("aria-selected");
  if (selected !== "true") throw new Error("输入文件路径后没有选中该文件");
  return "~ → /home/keyuser；文件路径打开其目录并选中";
});

check("文件", "sftp-follow-osc7", "终端报告当前目录（OSC 7，fish、Fedora/Ubuntu 的 VTE 配置会发）时，文件面板可以跟随（Windows Terminal、WezTerm、iTerm2 都读 OSC 7）", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  // What /etc/profile.d/vte.sh makes bash send before each prompt.
  await runAndWait(page, `PROMPT_COMMAND='printf "\\033]7;file://%s%s\\033\\\\" "$HOSTNAME" "$PWD"'`);
  const pane = await openFilesAt(page, dir);
  const follow = pane.getByRole("button", { name: "跟随终端目录" });
  if ((await follow.getAttribute("aria-pressed")) !== "true") await follow.click();
  await page.locator(".xterm-screen").last().click();
  await runAndWait(page, `cd ${dir}/sub`);
  const followed = await filesList(pane, `${dir}/sub`).waitFor({ timeout: 4000 }).then(() => true, () => false);
  if (!followed) throw new Error("终端 cd 后文件面板没有跟过去（OSC 7 没有被读取）");
  return "开启跟随后，终端 cd sub，文件面板随之打开 sub";
});

check("文件", "sftp-follow-integration", "没开 Shell 集成时点“跟随终端目录”，可以直接为这台主机开启；重连后跟随（FinalShell 默认就跟随）", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  await pane.getByRole("button", { name: "跟随终端目录" }).click();
  const dialog = page.getByRole("alertdialog");
  await dialog.waitFor({ timeout: 3000 }).catch(() => {
    throw new Error("没开 Shell 集成时，跟随按钮点了没反应，只能去主机设置里找开关");
  });
  await dialog.getByRole("button", { name: "开启" }).click();
  await pane.getByText(/已为这台主机开启 Shell 集成/).waitFor({ timeout: 5000 });
  // The next connection has the integration (the bridge's host stands in for the saved one).
  await page.evaluate(() => (window.__anixopsE2eIntegration = true));
  await page.getByRole("button", { name: "关闭标签页" }).first().click();
  await openHost(page);
  await openFilesAt(page, dir);
  const pane2 = page.getByRole("complementary", { name: "文件" });
  await page.locator(".xterm-screen").last().click();
  await runAndWait(page, `cd ${dir}/sub`);
  const followed = await filesList(pane2, `${dir}/sub`).waitFor({ timeout: 6000 }).then(() => true, () => false);
  if (!followed) throw new Error("开启 Shell 集成并重连后，文件面板仍不跟随");
  return "确认开启 → 重连 → 终端 cd sub，文件面板随之打开 sub";
});

check("文件", "sftp-many-tabs", "同一台服务器开 12 个标签后，文件面板和下载照常可用（OpenSSH 每个连接最多 10 个会话；Termius、FinalShell 不受影响）", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  for (let i = 1; i < 12; i++) {
    await page.getByRole("tab").first().click({ button: "right" });
    await page.getByRole("menuitem", { name: "复制标签" }).click();
    await page.waitForFunction((n) => window.__anixopsE2eOpened.length >= n, i + 1, { timeout: 15000 });
  }
  await waitScreen(page, /keyuser@/, { index: -1 });
  const pane = await openFilesAt(page, dir);
  await fileRow(pane, "a.txt").waitFor({ timeout: 10000 });
  const row = fileRow(pane, "big.bin");
  await row.hover();
  await row.getByRole("button", { name: "下载" }).click();
  await pane.getByText("完成", { exact: true }).first().waitFor({ timeout: 20000 });
  return "12 个终端时文件面板照常打开、下载";
});

check("文件", "sftp-double-click", "双击文件用编辑器打开，保存后传回（FinalShell、MobaXterm、WinSCP；设置里可改成下载）", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  await fileRow(pane, "a.txt").dblclick();
  const editing = pane.getByRole("region", { name: "正在编辑" });
  const opened = await editing.getByText("a.txt").waitFor({ timeout: 10000 }).then(() => true, () => false);
  if (!opened) throw new Error("双击文件没有用编辑器打开");
  return "a.txt 出现在“正在编辑”里";
});

check("文件", "sftp-double-click-binary", "双击二进制文件（压缩包、程序、图片）直接下载，不用编辑器打开，免得卡住", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  await fileRow(pane, "big.bin").dblclick();
  await pane.getByText("完成", { exact: true }).first().waitFor({ timeout: 20000 });
  const editing = await pane.getByRole("region", { name: "正在编辑" }).getByText("big.bin").count();
  if (editing) throw new Error("big.bin 被当成文本用编辑器打开了");
  await fileRow(pane, "big.bin").click({ button: "right" });
  const edit = await page.getByRole("menuitem", { name: /编辑/ }).count();
  await page.keyboard.press("Escape");
  if (edit) throw new Error("二进制文件的右键菜单里还有“编辑”");
  return "big.bin 双击后下载完成，右键菜单没有“编辑”";
});

check("文件", "sftp-cancel", "大文件传输可以取消，状态显示已取消", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  await runAndWait(page, `head -c 400000000 /dev/zero > ${dir}/huge.bin`);
  const pane = await openFilesAt(page, dir);
  const row = fileRow(pane, "huge.bin");
  await row.hover();
  await row.getByRole("button", { name: "下载" }).click();
  await pane.getByRole("button", { name: "取消传输" }).click({ timeout: 5000 });
  await pane.getByText("已取消").waitFor({ timeout: 10000 });
  return "取消后显示“已取消”";
});

check("文件", "sftp-context-menu", "在文件上右键弹出菜单：下载、重命名、属性、删除、复制路径（FinalShell、MobaXterm）", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  await fileRow(pane, "a.txt").click({ button: "right" });
  const menu = page.getByRole("menu");
  await menu.waitFor({ timeout: 3000 });
  const items = (await menu.getByRole("menuitem").allInnerTexts()).map((s) => s.split("\n")[0].trim());
  await page.screenshot({ path: join(outDir, "sftp-context-menu.png") });
  const missing = ["下载", "重命名", "属性", "删除", "复制路径"].filter((name) => !items.includes(name));
  if (missing.length) throw new Error(`右键菜单缺少：${missing.join("、")}（现有：${items.join("、")}）`);
  return items.join("、");
});

check("文件", "sftp-sort", "点列头按名称、大小、修改时间排序（FinalShell、MobaXterm、WinSCP）", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  const header = pane.getByRole("button", { name: /^大小/ });
  if (!(await header.count())) throw new Error("没有可点的列头，只能按名称排");
  await header.click();
  const names = await pane.locator("[role=option]").evaluateAll((rows) => rows.map((r) => r.dataset.name));
  return names.join(" / ");
});

check("文件", "sftp-hidden", "可以隐藏或显示以 . 开头的文件（Termius、MobaXterm、WinSCP）", async (page) => {
  await openHost(page);
  const dir = await remoteFixture(page);
  const pane = await openFilesAt(page, dir);
  const toggle = pane.getByRole("button", { name: /隐藏文件/ });
  if (!(await toggle.count())) throw new Error("没有显示/隐藏隐藏文件的开关");
  await toggle.click();
  const shown = await fileRow(pane, ".hidden").count();
  await toggle.click();
  const back = await fileRow(pane, ".hidden").count();
  if (shown === back) throw new Error("开关不起作用");
  return "开关切换 .hidden 的显示";
});

// ---- Connections and forwards --------------------------------------------

check("连接管理", "quick-connect", "在主机搜索框输入 user@host:port 回车直接连接（Termius、MobaXterm、Tabby 的快速连接）", async (page) => {
  const search = page.getByRole("textbox", { name: "搜索主机" });
  await search.fill("keyuser@10.20.30.40:2200");
  const row = page.getByRole("button", { name: /连接到 keyuser@10\.20\.30\.40:2200/ });
  if (!(await row.count())) throw new Error("搜索框里输入地址只显示“没有匹配的主机”，不能直接连接");
  await search.press("Enter");
  await page.locator(".xterm-screen").last().waitFor();
  await waitScreen(page, /keyuser@/);
  const tab = await page.getByRole("tab").first().innerText();
  return `回车后打开标签“${tab.split("\n")[0]}”并连上`;
});

check("连接管理", "search-enter", "搜索主机后回车连接第一个结果（Termius、Royal TSX）", async (page) => {
  const search = page.getByRole("textbox", { name: "搜索主机" });
  await search.fill("staging");
  await search.press("Enter");
  const opened = await page.locator(".xterm-screen").first().waitFor({ timeout: 4000 }).then(() => true, () => false);
  if (!opened) throw new Error("回车没有反应");
  const tab = await page.getByRole("tab").first().innerText();
  if (!/staging-api/.test(tab)) throw new Error(`打开了 ${tab}`);
  return "回车打开 staging-api";
});

check("连接管理", "duplicate-host", "主机右键“复制主机”，得到设置相同的新主机（Termius、MobaXterm、FinalShell）", async (page) => {
  await page.getByRole("button", { name: /prod-web-02/ }).first().click({ button: "right" });
  const item = page.getByRole("menuitem", { name: "复制主机" });
  if (!(await item.count())) throw new Error(`主机菜单没有“复制主机”（现有：${(await page.getByRole("menuitem").allInnerTexts()).map((s) => s.split("\n")[0]).join("、")}）`);
  await item.click();
  const dialog = page.getByRole("dialog");
  const name = await dialog.getByLabel("名称").first().inputValue();
  await dialog.getByRole("button", { name: "保存", exact: true }).click();
  await dialog.waitFor({ state: "detached" });
  await page.getByRole("button", { name: new RegExp(name) }).first().waitFor({ timeout: 3000 });
  return `新主机“${name}”，地址相同`;
});

/** A port nothing listens on. */
async function freePort() {
  const { createServer } = await import("node:net");
  return new Promise((resolve) => {
    const server = createServer();
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

/** The first bytes a TCP server sends (an SSH server's banner). */
async function banner(port, { socks = null } = {}) {
  const { connect } = await import("node:net");
  return new Promise((resolve, reject) => {
    const socket = connect(port, "127.0.0.1");
    let data = Buffer.alloc(0);
    let stage = socks ? "greet" : "data";
    const timer = setTimeout(() => (socket.destroy(), reject(new Error("no answer"))), 5000);
    socket.on("connect", () => socks && socket.write(Buffer.from([5, 1, 0])));
    socket.on("data", (chunk) => {
      data = Buffer.concat([data, chunk]);
      if (stage === "greet" && data.length >= 2) {
        data = data.subarray(2);
        stage = "connect";
        const host = Buffer.from(socks.host);
        socket.write(Buffer.concat([Buffer.from([5, 1, 0, 3, host.length]), host, Buffer.from([socks.port >> 8, socks.port & 255])]));
      }
      if (stage === "connect" && data.length >= 10) {
        if (data[1] !== 0) return (clearTimeout(timer), socket.destroy(), reject(new Error(`SOCKS reply ${data[1]}`)));
        data = data.subarray(10);
        stage = "data";
      }
      if (stage === "data" && data.includes(10)) {
        clearTimeout(timer);
        socket.destroy();
        resolve(data.toString().split("\n")[0].trim());
      }
    });
    socket.on("error", (error) => (clearTimeout(timer), reject(error)));
  });
}

/** The port the e2e server's sshd listens on, as the shell sees it. */
async function serverSshPort(page) {
  await openHost(page);
  const text = await remoteOutput(page, "echo ${SSH_CONNECTION##* }");
  return Number(text.trim());
}

/** Creates a forward through the form and turns it on. */
async function createTunnel(page, { name, kind, bindPort, targetHost, targetPort }) {
  await page.getByRole("button", { name: "端口转发", exact: true }).click();
  await page.getByRole("button", { name: "新建隧道" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("名称").fill(name);
  await dialog.getByLabel("经由主机").selectOption({ label: /prod-web-01/ }).catch(async () => {
    const value = await dialog.locator("select option", { hasText: "prod-web-01" }).first().getAttribute("value");
    await dialog.getByLabel("经由主机").selectOption(value);
  });
  await dialog.getByText({ local: "本地", remote: "远程", dynamic: "SOCKS" }[kind], { exact: true }).click();
  await dialog.getByLabel(kind === "remote" ? "服务器端口" : "本机端口").fill(String(bindPort));
  if (kind !== "dynamic") {
    await dialog.getByLabel("目标主机").fill(targetHost);
    await dialog.getByLabel("目标端口").fill(String(targetPort));
  }
  await dialog.getByRole("button", { name: "保存", exact: true }).click();
  await dialog.waitFor({ state: "detached" });
  await page.getByRole("switch", { name }).click();
  const row = page.locator("[data-context-row]", { has: page.getByRole("switch", { name }) });
  await row.getByText("运行中").waitFor({ timeout: 10000 }).catch(async () => {
    throw new Error(`隧道没有运行：${(await row.innerText()).replace(/\s+/g, " ")}`);
  });
}

check("端口转发", "forward-local", "本地转发（-L）：打开后本机端口直通服务器那边的目标，关掉后端口释放", async (page) => {
  const sshPort = await serverSshPort(page);
  const port = await freePort();
  await createTunnel(page, { name: "e2e-local", kind: "local", bindPort: port, targetHost: "127.0.0.1", targetPort: sshPort });
  const text = await banner(port);
  if (!/^SSH-2\.0-/.test(text)) throw new Error(`本机 ${port} 读到：${text}`);
  await page.getByRole("switch", { name: "e2e-local" }).click();
  await page.waitForTimeout(800);
  const closed = await banner(port).then(() => false, () => true);
  if (!closed) throw new Error("关掉后端口还在转发");
  await page.screenshot({ path: join(outDir, "forward-local.png") });
  return `127.0.0.1:${port} → 服务器 127.0.0.1:${sshPort}，读到 ${text.slice(0, 20)}…；关掉后端口释放`;
});

check("端口转发", "forward-socks", "动态转发（-D，SOCKS5）：浏览器或工具经它访问服务器那边", async (page) => {
  const sshPort = await serverSshPort(page);
  const port = await freePort();
  await createTunnel(page, { name: "e2e-socks", kind: "dynamic", bindPort: port });
  const text = await banner(port, { socks: { host: "127.0.0.1", port: sshPort } });
  if (!/^SSH-2\.0-/.test(text)) throw new Error(`SOCKS 读到：${text}`);
  return `SOCKS5 127.0.0.1:${port} → 服务器 127.0.0.1:${sshPort}`;
});

check("端口转发", "forward-remote", "远程转发（-R）：服务器上的端口直通本机的服务", async (page) => {
  const { createServer } = await import("node:net");
  const local = createServer((socket) => socket.end("hello-from-local\n"));
  const localPort = await new Promise((resolve) => local.listen(0, "127.0.0.1", () => resolve(local.address().port)));
  try {
    await openHost(page);
    const remotePort = 20000 + Math.floor(Math.random() * 20000);
    await createTunnel(page, { name: "e2e-remote", kind: "remote", bindPort: remotePort, targetHost: "127.0.0.1", targetPort: localPort });
    await page.getByRole("button", { name: "主机", exact: true }).click();
    const text = await remoteOutput(page, `exec 3<>/dev/tcp/127.0.0.1/${remotePort} && head -1 <&3; exec 3<&-`);
    if (!/hello-from-local/.test(text)) throw new Error(`服务器上读到：${text}`);
    return `服务器 127.0.0.1:${remotePort} → 本机 ${localPort}`;
  } finally {
    local.close();
  }
});

// ---- Settings ------------------------------------------------------------

check("设置", "settings-search", "设置可以搜索，只留下匹配的项（Windows Terminal、VS Code、Termius）", async (page) => {
  await page.getByRole("button", { name: "设置", exact: true }).click();
  const search = page.getByRole("textbox", { name: "搜索设置" });
  if (!(await search.count())) throw new Error("设置没有搜索框，只能在侧栏里一项项往下翻");
  await search.fill("连字");
  await page.getByText("连字", { exact: true }).waitFor({ timeout: 2000 });
  const themeShown = await page.getByText("主题", { exact: true }).isVisible().catch(() => false);
  if (themeShown) throw new Error("搜“连字”时仍显示“主题”");
  await page.screenshot({ path: join(outDir, "settings-search.png") });
  await search.fill("没有这一项");
  await page.getByText("没有匹配的设置").waitFor({ timeout: 2000 });
  await search.fill("终端");
  const rows = await page.locator("[data-settings-row]").count();
  return `搜“连字”只剩连字；搜“终端”显示终端组 ${rows} 项；搜不到时提示`;
});

check("设置", "settings-live", "改字号立刻作用到已打开的终端，不用重连（Windows Terminal、iTerm2）", async (page) => {
  await openHost(page);
  const before = await term(page, (t) => t.options.fontSize);
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.locator('[data-settings-nav="terminal"]').click();
  await page.getByRole("button", { name: "字号 +" }).click();
  await page.getByRole("button", { name: "字号 +" }).click();
  await page.waitForTimeout(300);
  const after = await term(page, (t) => t.options.fontSize);
  if (after !== before + 2) throw new Error(`字号 ${before} → ${after}`);
  return `字号 ${before} → ${after}，会话不断`;
});

// ---- Run -----------------------------------------------------------------

const results = [];
for (const item of checks) {
  if (only && !only.has(item.name)) continue;
  const page = await newPage();
  let result;
  const started = Date.now();
  try {
    const note = await item.fn(page);
    result = { ...item, ok: true, note: note ?? "" };
  } catch (error) {
    result = { ...item, ok: false, note: String(error?.message ?? error).split("\n")[0], detail: String(error?.stack ?? error) };
  }
  if (page.errors.length) result.pageErrors = page.errors;
  await page.screenshot({ path: join(outDir, `${item.name}.png`) }).catch(() => {});
  result.seconds = (Date.now() - started) / 1000;
  delete result.fn;
  results.push(result);
  console.log(`${result.ok ? "PASS" : "FAIL"} ${item.name} (${result.seconds.toFixed(1)}s) ${result.note}`);
  if (!result.ok && process.env.VERBOSE) console.log(result.detail);
  for (const id of await page.evaluate(() => window.__anixopsE2eOpened).catch(() => [])) send({ op: "close", id });
  for (const id of await page.evaluate(() => [...window.__anixopsE2eSftp]).catch(() => []))
    send({ op: "request", seq: ++seq, kind: "sftp.close", payload: { sftp_id: id } });
  await page.context().close();
}
await browser.close();
bridge.stdin.end();
bridge.kill();

const rows = results.map((r) => `| ${r.area} | \`${r.name}\` | ${r.mainstream} | ${r.ok ? "✅" : "❌"} ${r.note.replace(/\|/g, "\\|")} |`);
await writeFile(
  join(outDir, "report.md"),
  `| 方面 | 检查 | 主流终端 | 结果 |\n|---|---|---|---|\n${rows.join("\n")}\n`,
);
await writeFile(join(outDir, "report.json"), JSON.stringify(results, null, 2));
const sorted = [...openTimes].sort((a, b) => a - b);
if (sorted.length)
  console.log(
    `session opens: ${sorted.length}, median ${sorted[Math.floor(sorted.length / 2)]} ms, slowest ${sorted.at(-1)} ms, over 5 s: ${sorted.filter((ms) => ms > 5000).length}`,
  );
const failed = results.filter((r) => !r.ok);
console.log(`${results.length - failed.length}/${results.length} passed`);
if (failed.length && !process.env.REPORT_ONLY) process.exit(1);
