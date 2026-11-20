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
import { join } from "node:path";
import { chromium } from "playwright";

const [baseUrl = "http://localhost:4173", outDir = "terminal-e2e"] = process.argv.slice(2);
await mkdir(outDir, { recursive: true });
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
      pendingOpens.get(message.seq)?.(message);
      pendingOpens.delete(message.seq);
    } else if (bridgePage) {
      if (process.env.DEBUG_BRIDGE) console.log("bridge>", line.slice(0, 160));
      bridgePage.evaluate((m) => window.__anixopsE2eReceive?.(m), message).catch(() => {});
    }
  });
});
await ready;
let seq = 0;
const send = (message) => bridge.stdin.write(JSON.stringify(message) + "\n");

// Runs in the page: the session bridge the app looks for in backend.ts.
function pageBridge() {
  const sinks = new Map();
  window.__anixopsE2eTerminals = new Map();
  window.__anixopsE2eReceive = (message) => {
    const sink = sinks.get(message.id);
    if (!sink) return;
    if (message.op === "data") sink({ kind: "data", data: message.data });
    else if (message.op === "exit") sink({ kind: "exit", code: message.code, reason: message.reason, signal: message.signal ?? undefined });
  };
  window.__anixopsE2eOpened = [];
  window.__anixopsE2eSession = {
    async open(hostId, cols, rows, onEvent) {
      const opened = await window.__anixopsE2eOpen({ hostId, cols, rows });
      if (opened.op === "error") throw new Error(opened.message);
      const id = opened.id;
      sinks.set(id, onEvent);
      window.__anixopsE2eOpened.push(id);
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
  await page.exposeFunction("__anixopsE2eOpen", ({ cols, rows }) => {
    const id = ++seq;
    bridgePage = page;
    return new Promise((resolve) => {
      pendingOpens.set(id, resolve);
      send({ op: "open", seq: id, host: "e2e-target", cols, rows });
    });
  });
  await page.exposeFunction("__anixopsE2ePost", (message) => send(message));
  await page.addInitScript((value) => Object.defineProperty(Navigator.prototype, "platform", { get: () => value }), navigatorPlatform);
  await page.addInitScript(pageBridge);
  await page.goto(`${baseUrl}/?webgl=1${query ? `&${query}` : ""}`);
  await page.waitForLoadState("networkidle");
  return page;
}

/** Opens a host from the list; the bridge connects it to the e2e server. */
async function openHost(page, name = "prod-web-01") {
  await page.getByRole("button", { name: new RegExp(name) }).first().click();
  await page.locator(".xterm-screen").last().waitFor();
  await waitScreen(page, /keyuser@/);
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
  throw new Error(`timed out waiting for ${pattern} on screen:\n${text.split("\n").slice(-12).join("\n")}`);
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
  await page.waitForTimeout(800);
  const { cols, rows } = await term(page, (t) => ({ cols: t.cols, rows: t.rows }));
  await runAndWait(page, "stty size");
  const text = await screen(page);
  if (!text.includes(`${rows} ${cols}`)) throw new Error(`stty size does not show ${rows} ${cols}`);
  return `stty size = ${rows} ${cols}`;
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

check("性能", "bulk-output", "大量输出（20 万行）不卡死，几秒内结束", async (page) => {
  await openHost(page);
  const started = Date.now();
  await runAndWait(page, "seq 1 200000", null, { timeout: 40000 });
  const seconds = (Date.now() - started) / 1000;
  if (seconds > 30) throw new Error(`用了 ${seconds.toFixed(1)} 秒`);
  return `${seconds.toFixed(1)} 秒`;
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
const failed = results.filter((r) => !r.ok);
console.log(`${results.length - failed.length}/${results.length} passed`);
if (failed.length && !process.env.REPORT_ONLY) process.exit(1);
