// Playwright checks of the touch UI on emulated phones, with real touch events
// (Chrome DevTools Protocol Input.dispatchTouchEvent): the key bar, soft
// keyboard composition, selection with handles and the copy/paste toolbar,
// scrolling and pinch, the background policy, the keyboard layout, the key bar
// editor and a hardware keyboard on a tablet.
//
// Usage: node scripts/mobile-e2e.mjs <base-url>
// The page is the preview build (npm run preview); sessions are the preview's
// local echo (src/lib/previewSession.ts), and the page exposes
// window.__mobileDebug (the terminal and what was written) outside Tauri.
import assert from "node:assert/strict";
import { chromium, devices } from "playwright";

const [baseUrl = "http://localhost:4173"] = process.argv.slice(2);
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM_PATH || undefined });
const failures = [];

/** A device descriptor Chromium can use (the iPhone's names a WebKit default). */
function descriptor(name) {
  const { defaultBrowserType, ...rest } = devices[name];
  return rest;
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function open(device, { query = "mobile=1&lang=en", host = "prod-web-01", init } = {}) {
  const context = await browser.newContext({ ...descriptor(device), locale: "en-US", permissions: ["clipboard-read", "clipboard-write"] });
  if (init) await context.addInitScript(init);
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => message.type() === "error" && errors.push(message.text()));
  await page.goto(`${baseUrl}/?${query}`);
  await page.waitForLoadState("networkidle");
  if (host) {
    await page.getByRole("button", { name: new RegExp(host) }).first().tap();
    await page.locator(".xterm-screen").waitFor();
    await page.waitForFunction(() => window.__mobileDebug?.terminal?.buffer.active.getLine(0)?.translateToString().includes("Welcome"));
  }
  const cdp = await context.newCDPSession(page);
  const touch = (type, points) =>
    cdp.send("Input.dispatchTouchEvent", { type, touchPoints: points.map((point, id) => ({ x: point.x, y: point.y, id, radiusX: 4, radiusY: 4, force: 1 })) });
  return { context, page, errors, touch };
}

const writes = (page) => page.evaluate(() => [...window.__mobileDebug.writes]);
const lastWrite = async (page) => (await writes(page)).at(-1);
const clearWrites = (page) => page.evaluate(() => window.__mobileDebug.writes.splice(0));

/** Typed text is echoed by the preview session, so the screen shows it. */
async function screenText(page) {
  return page.evaluate(() => {
    const buffer = window.__mobileDebug.terminal.buffer.active;
    const lines = [];
    for (let row = 0; row < buffer.length; row++) lines.push(buffer.getLine(row)?.translateToString(true) ?? "");
    return lines.join("\n");
  });
}

/** Pixel centre of the first cell of `needle` on screen. */
async function find(page, needle) {
  const found = await page.evaluate((word) => {
    const terminal = window.__mobileDebug.terminal;
    const buffer = terminal.buffer.active;
    const screen = document.querySelector(".xterm-screen").getBoundingClientRect();
    for (let row = buffer.length - 1; row >= 0; row--) {
      const column = buffer.getLine(row)?.translateToString(false).indexOf(word) ?? -1;
      if (column < 0) continue;
      const onScreen = row - buffer.viewportY;
      if (onScreen < 0 || onScreen >= terminal.rows) continue;
      const cellWidth = screen.width / terminal.cols;
      const cellHeight = screen.height / terminal.rows;
      return {
        x: screen.left + (column + 0.5) * cellWidth,
        y: screen.top + (onScreen + 0.5) * cellHeight,
        cellWidth,
        cellHeight,
        endX: screen.left + (column + word.length) * cellWidth,
      };
    }
    return null;
  }, needle);
  assert.ok(found, `"${needle}" is not on screen`);
  return found;
}

async function longPress(touch, point, ms = 650) {
  await touch("touchStart", [point]);
  await sleep(ms);
  await touch("touchEnd", []);
}

async function drag(touch, from, to, steps = 8) {
  await touch("touchStart", [from]);
  for (let step = 1; step <= steps; step++) {
    await touch("touchMove", [{ x: from.x + ((to.x - from.x) * step) / steps, y: from.y + ((to.y - from.y) * step) / steps }]);
    await sleep(16);
  }
  await touch("touchEnd", []);
}

const buttonBox = async (page, name) => {
  const box = await page.getByRole("button", { name, exact: true }).first().boundingBox();
  assert.ok(box, `no button ${name}`);
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
};

async function runTest(name, device, body, options) {
  let session;
  try {
    session = await open(device, options);
    await body(session);
    assert.deepEqual(session.errors, [], "console errors");
    console.log(`ok    ${device}: ${name}`);
  } catch (error) {
    failures.push(`${device}: ${name}: ${error.stack ?? error}`);
    console.log(`FAIL  ${device}: ${name}\n${error.message}`);
  } finally {
    await session?.context.close();
  }
}

const tests = [];
const test = (name, body, options) => tests.push({ name, body, options });

// ---- key bar ----------------------------------------------------------------

test("the key bar sends Esc, Tab, arrows, Home and F-keys", async ({ page }) => {
  const tap = (name) => page.getByRole("button", { name, exact: true }).tap();
  await clearWrites(page);
  await tap("Esc");
  await tap("Tab");
  await tap("Left");
  await tap("Home");
  await tap("F5");
  await tap("Page up");
  assert.deepEqual(await writes(page), ["\x1b", "\t", "\x1b[D", "\x1b[H", "\x1b[15~", "\x1b[5~"]);
});

test("Ctrl arms for one key, locks on a double tap and Alt prefixes ESC", async ({ page }) => {
  const ctrl = page.getByRole("button", { name: /^Ctrl/ });
  await clearWrites(page);
  await ctrl.tap();
  assert.equal(await ctrl.getAttribute("data-mode"), "armed");
  await page.keyboard.type("c");
  assert.equal(await lastWrite(page), "\x03");
  assert.equal(await ctrl.getAttribute("data-mode"), "off");

  // Two quick taps (the second within 350 ms), which a locator's tap is not guaranteed to be.
  const box = await ctrl.boundingBox();
  await page.touchscreen.tap(box.x + box.width / 2, box.y + box.height / 2);
  await page.touchscreen.tap(box.x + box.width / 2, box.y + box.height / 2);
  assert.equal(await ctrl.getAttribute("data-mode"), "locked");
  await page.keyboard.type("a");
  await page.keyboard.type("b");
  assert.deepEqual((await writes(page)).slice(-2), ["\x01", "\x02"]);
  assert.equal(await ctrl.getAttribute("data-mode"), "locked");
  await sleep(400);
  await ctrl.tap();
  assert.equal(await ctrl.getAttribute("data-mode"), "off");

  const alt = page.getByRole("button", { name: /^Alt/ });
  await alt.tap();
  await page.keyboard.type("x");
  assert.equal(await lastWrite(page), "\x1bx");
  // Ctrl on an arrow uses the xterm modifier form.
  await ctrl.tap();
  await page.getByRole("button", { name: "Up", exact: true }).tap();
  assert.equal(await lastWrite(page), "\x1b[1;5A");
});

test("an arrow repeats while held and stops on release", async ({ page, touch }) => {
  await clearWrites(page);
  const point = await buttonBox(page, "Right");
  await touch("touchStart", [point]);
  await sleep(900);
  await touch("touchEnd", []);
  const held = (await writes(page)).filter((data) => data === "\x1b[C").length;
  assert.ok(held >= 4, `expected a repeat, got ${held}`);
  await sleep(300);
  assert.equal((await writes(page)).filter((data) => data === "\x1b[C").length, held);
  // A quick tap is exactly one key.
  await clearWrites(page);
  await page.getByRole("button", { name: "Right", exact: true }).tap();
  await sleep(200);
  assert.deepEqual(await writes(page), ["\x1b[C"]);
});

test("the second row swipes sideways to F12, and snippet keys can be added", async ({ page }) => {
  const row = page.locator("[data-extra-row]");
  const before = await row.evaluate((element) => element.scrollLeft);
  await row.evaluate((element) => element.scrollTo({ left: element.scrollWidth }));
  assert.ok((await row.evaluate((element) => element.scrollLeft)) > before);
  await clearWrites(page);
  await page.getByRole("button", { name: "F12", exact: true }).tap();
  assert.equal(await lastWrite(page), "\x1b[24~");

  await page.getByRole("button", { name: "Terminal settings" }).tap();
  await page.getByRole("button", { name: /Customise the key bar/ }).tap();
  await page.getByLabel("Text on the key").fill("up");
  await page.getByLabel("What it types").fill("uptime");
  await page.getByRole("button", { name: "Add a command key" }).tap();
  await page.getByRole("button", { name: "Settings", exact: true }).first().tap();
  await page.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: "Back", exact: true }).tap();
  await row.evaluate((element) => element.scrollTo({ left: element.scrollWidth }));
  await clearWrites(page);
  await page.getByRole("button", { name: /^up: uptime/ }).tap();
  assert.equal(await lastWrite(page), "uptime\r");
});

// ---- soft keyboard ------------------------------------------------------------

/** Dispatches composition events the way a keyboard does. */
const compose = (page, steps) =>
  page.evaluate(async (list) => {
    const input = document.querySelector(".xterm-helper-textarea");
    input.focus();
    for (const [type, data, extra] of list) {
      if (type === "beforeinput") {
        const event = new InputEvent("beforeinput", { inputType: extra, data, cancelable: true, bubbles: true });
        input.dispatchEvent(event);
      } else {
        input.dispatchEvent(new CompositionEvent(type, { data, bubbles: true }));
      }
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
  }, steps);

test("predictive text composes a word and the shell sees it once", async ({ page }) => {
  await clearWrites(page);
  await compose(page, [
    ["compositionstart", ""],
    ["compositionupdate", "l"],
    ["compositionupdate", "ls"],
    ["compositionend", "ls"],
    ["beforeinput", "ls", "insertText"], // a keyboard repeating the commit
    ["beforeinput", "\n", "insertLineBreak"],
  ]);
  const sent = (await writes(page)).join("");
  assert.equal(sent, "ls\r");
  await page.waitForFunction(() => document.querySelector(".xterm-rows").textContent.includes("deploy.sh"));
});

test("pinyin shows in a chip and only the hanzi are sent", async ({ page }) => {
  await clearWrites(page);
  await compose(page, [
    ["compositionstart", ""],
    ["compositionupdate", "ni"],
    ["compositionupdate", "ni hao"],
  ]);
  assert.equal(await page.locator("[data-preedit]").innerText(), "ni hao");
  assert.equal((await writes(page)).join(""), "ni" + "\x7f\x7f", "the letters sent live are taken back at the space");
  await compose(page, [["compositionend", "你好"]]);
  assert.equal(await page.locator("[data-preedit]").count(), 0);
  assert.equal((await writes(page)).join("").endsWith("你好"), true);
  await page.waitForFunction(() => document.querySelector(".xterm-rows").textContent.includes("你好"));
});

test("backspace on an empty field still deletes, and the field keeps its sentinel", async ({ page }) => {
  await clearWrites(page);
  await compose(page, [
    ["beforeinput", null, "deleteContentBackward"],
    ["beforeinput", null, "deleteContentBackward"],
  ]);
  assert.deepEqual(await writes(page), ["\x7f", "\x7f"]);
  assert.equal(await page.locator(".xterm-helper-textarea").inputValue(), " ");
  const attributes = await page.locator(".xterm-helper-textarea").evaluate((element) => ({
    autocapitalize: element.getAttribute("autocapitalize"),
    autocorrect: element.getAttribute("autocorrect"),
    spellcheck: element.getAttribute("spellcheck"),
  }));
  assert.deepEqual(attributes, { autocapitalize: "off", autocorrect: "off", spellcheck: "false" });
});

test("typing real keys is not doubled by the composition layer", async ({ page }) => {
  await clearWrites(page);
  await page.locator(".xterm-helper-textarea").focus();
  await page.keyboard.type("ab");
  assert.deepEqual(await writes(page), ["a", "b"]);
});

// ---- touch --------------------------------------------------------------------

test("tap focuses the terminal and shows the keyboard again after it was dismissed", async ({ page, touch }) => {
  await page.evaluate(() => document.querySelector(".xterm-helper-textarea").blur());
  assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("xterm-helper-textarea")), false);
  const surface = await page.locator("[data-terminal-surface]").boundingBox();
  const point = { x: surface.x + surface.width / 2, y: surface.y + surface.height - 40 };
  await touch("touchStart", [point]);
  await sleep(60);
  await touch("touchEnd", []);
  await page.waitForFunction(() => document.activeElement?.classList.contains("xterm-helper-textarea"));
});

test("long-press selects a word, handles extend it, Copy puts it on the clipboard and Paste types it", async ({ page, touch }) => {
  await page.keyboard.type("ls");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector(".xterm-rows").textContent.includes("README.md"));
  const word = await find(page, "deploy.sh");
  await longPress(touch, { x: word.x + word.cellWidth, y: word.y });
  assert.equal(await page.evaluate(() => window.__mobileDebug.terminal.getSelection()), "deploy.sh");
  await page.locator("[data-handle=start]").waitFor();
  await page.locator("[data-handle=end]").waitFor();
  assert.equal(await page.getByRole("toolbar", { name: "Selection" }).count(), 1);

  // Drag the end handle over start.sh.
  const target = await find(page, "start.sh");
  const handle = await page.locator("[data-handle=end]").boundingBox();
  const grab = { x: handle.x + handle.width / 2, y: handle.y + 14 };
  await drag(touch, grab, { x: target.endX - target.cellWidth * 0.5, y: target.y + target.cellHeight * 1.4 });
  const extended = await page.evaluate(() => window.__mobileDebug.terminal.getSelection());
  assert.match(extended, /^deploy\.sh\s+start\.sh$/, extended);

  // Chrome drops the click of a tap that follows a drag within a few hundred ms.
  await sleep(800);
  await page.getByRole("button", { name: "Copy", exact: true }).tap();
  await page.waitForFunction(() => window.__mobileDebug.terminal.getSelection() === "", null, { timeout: 5000 });
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), extended);
  assert.equal(await page.locator("[data-handle=start]").count(), 0);

  // Paste: long-press an empty cell, tap Paste.
  await clearWrites(page);
  const surface = await page.locator("[data-terminal-surface]").boundingBox();
  await longPress(touch, { x: surface.x + surface.width / 2, y: surface.y + surface.height - 50 });
  await page.getByRole("button", { name: "Paste", exact: true }).tap();
  await sleep(100);
  assert.equal((await writes(page)).join(""), extended.replace(/\n/g, "\r"));
});

test("dragging the start handle past the end swaps the handles", async ({ page, touch }) => {
  await page.keyboard.type("ls");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector(".xterm-rows").textContent.includes("README.md"));
  const word = await find(page, "logs");
  await longPress(touch, { x: word.x + word.cellWidth, y: word.y });
  assert.equal(await page.evaluate(() => window.__mobileDebug.terminal.getSelection()), "logs");
  const readme = await find(page, "README.md");
  const start = await page.locator("[data-handle=start]").boundingBox();
  await drag(touch, { x: start.x + start.width / 2, y: start.y + 14 }, { x: readme.endX - readme.cellWidth, y: readme.y + readme.cellHeight * 1.4 });
  const selection = await page.evaluate(() => window.__mobileDebug.terminal.getSelection());
  assert.ok(selection.length > 0 && !selection.startsWith("logs"), `selection: ${selection}`);
  assert.ok(selection.includes("README"), selection);
});

async function fillScrollback(page) {
  for (let n = 0; n < 40; n++) {
    await page.keyboard.type("ls");
    await page.keyboard.press("Enter");
  }
  await page.waitForFunction(() => window.__mobileDebug.terminal.buffer.active.baseY > 20);
  await sleep(400);
  await page.evaluate(() => window.__mobileDebug.terminal.scrollToBottom());
}

test("one finger scrolls the scrollback and two fingers scroll faster", async ({ page, touch }) => {
  await fillScrollback(page);
  const viewport = () => page.evaluate(() => window.__mobileDebug.terminal.buffer.active.viewportY);
  const surface = await page.locator("[data-terminal-surface]").boundingBox();
  const x = surface.x + surface.width / 2;
  const y = surface.y + 100;
  const bottom = await viewport();
  await drag(touch, { x, y }, { x, y: y + 120 });
  const one = bottom - (await viewport());
  assert.ok(one >= 4, `one finger moved ${one} lines`);

  // Two fingers, the same distance: about four times as far.
  await page.evaluate(() => window.__mobileDebug.terminal.scrollToBottom());
  const startAt = await viewport();
  await touch("touchStart", [{ x: x - 40, y }, { x: x + 40, y }]);
  for (let step = 1; step <= 8; step++) {
    await touch("touchMove", [{ x: x - 40, y: y + 15 * step }, { x: x + 40, y: y + 15 * step }]);
    await sleep(16);
  }
  await touch("touchEnd", []);
  const two =startAt - (await viewport());
  assert.ok(two >= one * 2, `two fingers moved ${two} lines, one finger ${one} (bottom ${bottom}, start ${startAt})`);
});

test("pinch changes the font size and it is remembered", async ({ page, touch, context }) => {
  const size = () => page.evaluate(() => window.__mobileDebug.terminal.options.fontSize);
  const before = await size();
  const surface = await page.locator("[data-terminal-surface]").boundingBox();
  const cx = surface.x + surface.width / 2;
  const cy = surface.y + 200;
  await touch("touchStart", [{ x: cx - 40, y: cy }, { x: cx + 40, y: cy }]);
  for (let step = 1; step <= 10; step++) {
    await touch("touchMove", [{ x: cx - 40 - step * 8, y: cy }, { x: cx + 40 + step * 8, y: cy }]);
    await sleep(16);
  }
  await touch("touchEnd", []);
  const after = await size();
  assert.ok(after > before + 3, `font size ${before} -> ${after}`);
  const saved = await page.evaluate(() => JSON.parse(localStorage.getItem("anixops.mobile.prefs.v1")).fontSize);
  assert.equal(saved, after);
  // A new session of the app starts with the same size.
  const again = await context.newPage();
  await again.goto(`${baseUrl}/?mobile=1&lang=en`);
  await again.getByRole("button", { name: /prod-web-01/ }).first().tap();
  await again.locator(".xterm-screen").waitFor();
  assert.equal(await again.evaluate(() => window.__mobileDebug.terminal.options.fontSize), after);
  await again.close();
});

// ---- background ---------------------------------------------------------------

/** Pretends the app went to the background (and came back). */
const visibility = (page, state) =>
  page.evaluate((value) => {
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => value });
    document.dispatchEvent(new Event("visibilitychange"));
  }, state);

const grace0 = () => localStorage.setItem("anixops.mobile.prefs.v1", JSON.stringify({ graceSeconds: 0 }));

test("without tmux the session closes in the background and asks before opening a new shell", async ({ page }) => {
  await visibility(page, "hidden");
  await page.getByRole("status").filter({ hasText: "Paused in the background" }).first().waitFor();
  await visibility(page, "visible");
  const card = page.getByRole("alert");
  await card.waitFor();
  assert.match(await card.innerText(), /new shell/);
  await card.getByRole("button", { name: "Reconnect" }).tap();
  await page.getByRole("status").filter({ hasText: "Connected" }).first().waitFor();
  assert.match(await screenText(page), /reconnected/);
}, { init: grace0 });

test("with tmux the session reconnects by itself and says so", async ({ page }) => {
  await page.getByRole("button", { name: "Terminal settings" }).tap();
  await page.getByLabel("Keep session").selectOption("tmux");
  await page.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: "Back", exact: true }).tap();
  // A new terminal reads the choice at connect time.
  await page.getByRole("button", { name: "Back to hosts" }).tap();
  await page.getByRole("button", { name: /prod-web-01/ }).first().tap();
  await page.waitForFunction(() => window.__mobileDebug?.terminal?.buffer.active.getLine(0)?.translateToString().includes("Welcome"));
  await page.waitForFunction(() => document.querySelector(".xterm-rows").textContent.includes("tmux: anixops"));
  await visibility(page, "hidden");
  await page.getByRole("status").filter({ hasText: "Paused in the background" }).first().waitFor();
  await visibility(page, "visible");
  await page.getByRole("status").filter({ hasText: "Connected" }).first().waitFor();
  assert.equal(await page.getByRole("alert").count(), 0);
  assert.match(await screenText(page), /reconnected/);
}, { init: grace0 });

test("a short trip to the background keeps the session untouched", async ({ page }) => {
  const before = await page.evaluate(() => window.__mobileDebug.status().state);
  await visibility(page, "hidden");
  await sleep(100);
  await visibility(page, "visible");
  assert.equal(before, "connected");
  assert.equal(await page.evaluate(() => window.__mobileDebug.status().state), "connected");
  assert.equal(await page.getByRole("alert").count(), 0);
});

// ---- layout, settings, accessibility ----------------------------------------

test("the key bar sits above the keyboard and the layout follows rotation", async ({ page }) => {
  const size = page.viewportSize();
  const bar = () => page.getByRole("toolbar", { name: "Terminal keys" }).boundingBox();
  const closed = await bar();
  assert.ok(closed.y + closed.height <= size.height + 1);
  // A keyboard of about 300 px takes the bottom of the page.
  await page.setViewportSize({ width: size.width, height: size.height - 300 });
  await page.waitForFunction(() => document.documentElement.dataset.keyboard === "open");
  const open = await bar();
  assert.ok(open.y + open.height <= size.height - 300 + 1, "the bar is above the keyboard");
  assert.ok(open.y + open.height >= size.height - 300 - 60, "and directly above it");
  // Back, then rotate.
  await page.setViewportSize({ width: size.width, height: size.height });
  await page.waitForFunction(() => document.documentElement.dataset.keyboard === "closed");
  await page.setViewportSize({ width: size.height, height: size.width });
  await sleep(200);
  assert.equal(await page.evaluate(() => document.documentElement.dataset.keyboard), "closed");
  const rotated = await bar();
  assert.ok(rotated.y + rotated.height <= size.width + 1);
  assert.ok((await page.locator(".xterm-screen").boundingBox()).height > 60);
});

test("every control has a name and every touch target is at least 44 px", async ({ page }) => {
  const problems = await page.evaluate(() =>
    [...document.querySelectorAll("button, input, select, textarea:not(.xterm-helper-textarea)")]
      .filter((element) => element.getClientRects().length > 0)
      .flatMap((element) => {
        const name = element.getAttribute("aria-label") || element.textContent?.trim() || element.getAttribute("placeholder") || "";
        const box = element.getBoundingClientRect();
        const out = [];
        if (!name && !element.labels?.length) out.push(`${element.tagName} has no name`);
        if (box.height < 43.5 || box.width < 43.5) out.push(`${name || element.tagName} is ${Math.round(box.width)}x${Math.round(box.height)}`);
        return out;
      }),
  );
  assert.deepEqual(problems, []);
  // The status line is announced and the terminal is a labelled group.
  assert.equal(await page.getByRole("group", { name: "Terminal" }).count(), 1);
  assert.equal(await page.getByRole("toolbar", { name: "Terminal keys" }).count(), 1);
});

test("settings: font size, IME mode and the explanation of the background policy", async ({ page }) => {
  await page.getByRole("button", { name: "Terminal settings" }).tap();
  const dialog = page.getByRole("dialog", { name: "Settings" });
  await dialog.waitFor();
  assert.match(await dialog.innerText(), /does not keep an SSH connection/);
  const before = await page.evaluate(() => window.__mobileDebug.terminal.options.fontSize);
  await dialog.getByRole("button", { name: "Larger text" }).tap();
  await page.waitForFunction((size) => window.__mobileDebug.terminal.options.fontSize === size, before + 1);
  await dialog.getByLabel("Keyboard composition").selectOption("hold");
  await dialog.getByLabel("Keep the connection in the background").selectOption("300");
  await dialog.getByRole("button", { name: "Back", exact: true }).tap();
  const prefs = await page.evaluate(() => JSON.parse(localStorage.getItem("anixops.mobile.prefs.v1")));
  assert.equal(prefs.imeMode, "hold");
  assert.equal(prefs.graceSeconds, 300);
  // No Android bridge in a browser: no notification switch.
  await page.getByRole("button", { name: "Terminal settings" }).tap();
  assert.equal(await page.getByRole("switch", { name: /Foreground notification/ }).count(), 0);
});

test("the bar editor reorders, moves and restores keys", async ({ page }) => {
  await page.getByRole("button", { name: "Terminal settings" }).tap();
  await page.getByRole("button", { name: /Customise the key bar/ }).tap();
  await page.getByRole("button", { name: /^Move later: Esc/ }).tap();
  const order = await page.evaluate(() => JSON.parse(localStorage.getItem("anixops.mobile.prefs.v1")).bar.primary.slice(0, 2));
  assert.deepEqual(order, ["tab", "esc"]);
  await page.getByRole("button", { name: /^Remove key: Home/ }).tap();
  assert.equal(await page.locator("[data-bar-item=home]").count(), 0);
  await page.getByRole("button", { name: "Restore the default bar" }).tap();
  assert.equal(await page.locator("[data-bar-item=home]").count(), 1);
});

// ---- tablet with a hardware keyboard --------------------------------------------

test("a tablet shows list and terminal together and a hardware keyboard drives the shortcuts", async ({ page }) => {
  assert.equal(await page.getByRole("button", { name: /prod-web-01/ }).first().isVisible(), true);
  assert.equal(await page.locator(".xterm-screen").isVisible(), true);
  // In a terminal plain Ctrl belongs to the shell, so the shortcuts use Ctrl+Shift there (keyboard.md K22).
  await page.locator(".xterm-helper-textarea").focus();
  await page.keyboard.press("Control+Shift+/");
  const list = page.getByRole("dialog", { name: "Shortcuts" });
  await list.waitFor();
  assert.match(await list.innerText(), /Ctrl\+K/);
  await page.keyboard.press("Escape");
  await list.waitFor({ state: "detached" });
  // Ctrl+Shift+, opens the settings.
  await page.locator(".xterm-helper-textarea").focus();
  await page.keyboard.press("Control+Shift+,");
  await page.getByRole("dialog", { name: "Settings" }).waitFor();
  await page.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: "Back", exact: true }).tap();
  // Plain Ctrl+C inside the terminal belongs to the shell.
  await clearWrites(page);
  await page.locator(".xterm-helper-textarea").focus();
  await page.keyboard.press("Control+c");
  assert.equal(await lastWrite(page), "\x03");
  // Ctrl+Shift+W (close tab) leaves the terminal; Ctrl+K then focuses the host search.
  await page.keyboard.press("Control+Shift+W");
  await page.locator(".xterm-screen").waitFor({ state: "detached" });
  await page.keyboard.press("Control+k");
  await page.waitForFunction(() => document.activeElement?.hasAttribute("data-host-search"));
});

// ---- run ------------------------------------------------------------------------

const phones = ["Pixel 7", "iPhone 14"];
const only = process.env.ONLY;
for (const { name, body, options } of tests) {
  if (only && !name.includes(only)) continue;
  if (name.startsWith("a tablet")) {
    await runTest(name, "iPad Pro 11", body, { ...options });
    continue;
  }
  for (const device of phones) await runTest(name, device, body, options);
}
await browser.close();

if (failures.length > 0) {
  console.error(`\n${failures.length} failed:\n${failures.join("\n\n")}`);
  process.exit(1);
}
console.log("\nAll mobile checks passed.");
