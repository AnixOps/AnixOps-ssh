// Captures the UI with preview data in light and dark, for review in CI.
// Usage: node scripts/screenshots.mjs <base-url> <out-dir>
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";

const [baseUrl = "http://localhost:4173", outDir = "screenshots"] = process.argv.slice(2);
await mkdir(outDir, { recursive: true });

const scenes = [
  { name: "home", query: "" },
  { name: "terminal", query: "", open: ["prod-db", "staging-api", "prod-web-01"], type: ["ls"] },
  {
    name: "split",
    query: "",
    open: ["prod-web-01"],
    panes: ["Control+Shift+D", "Control+Shift+O", "Control+Shift+B"],
    type: ["ls", "df -h"],
  },
  { name: "exited", query: "", open: ["prod-db"], type: ["exit"] },
  { name: "find", query: "", open: ["prod-web-01"], find: "Ubuntu" },
  { name: "local", query: "", local: true, type: ["ls"] },
  { name: "files", query: "", open: ["prod-web-01"], type: ["ls"], files: true },
  { name: "search", query: "", search: "prod" },
  { name: "askpass", query: "askpass=1" },
  { name: "askpass-touch", query: "askpass=touch" },
  { name: "askpass-pin", query: "askpass=pin" },
  { name: "host-editor", query: "", clicks: [{ name: "更多", exact: false }, { name: "编辑主机|保存到主机列表", role: "menuitem", regex: true }] },
  { name: "forwards", query: "", nav: "端口转发" },
  { name: "tunnel-editor", query: "", nav: "端口转发", click: "新建隧道" },
  { name: "palette", query: "", open: ["prod-web-01"], keys: "Control+Shift+K", typeText: "prod" },
  { name: "settings", query: "", nav: "设置" },
  { name: "no-ssh", query: "ssh=missing" },
  { name: "sync-setup", query: "", nav: "设置", click: "开始设置" },
  { name: "snippets", query: "", open: ["prod-web-01"], nav: "命令片段", run: "磁盘用量" },
  { name: "new-host", query: "hosts=empty", click: "新建主机" },
  { name: "first-run", query: "hosts=empty" },
  {
    name: "import",
    query: "",
    clicks: [{ name: "添加主机" }, { name: "^导入主机", role: "menuitem", regex: true }, { name: "从 ~/.ssh/config 导入" }],
    waitText: "legacy-switch",
  },
  { name: "terminal-en", query: "lang=en", open: ["prod-web-01", "home-nas"] },
];

// The touch UI of the mobile app (components/mobile), forced on with ?mobile=1.
// Sizes: a phone, then a tablet that crosses the md breakpoint (834 px).
const mobileScenes = [
  { name: "mobile-hosts", query: "mobile=1&lang=en", viewport: { width: 390, height: 844 } },
  { name: "mobile-hosts-empty", query: "mobile=1&lang=en&hosts=empty", viewport: { width: 390, height: 844 } },
  { name: "mobile-add-host", query: "mobile=1&lang=en", viewport: { width: 390, height: 844 }, click: "Add host" },
  { name: "mobile-terminal", query: "mobile=1&lang=en", viewport: { width: 390, height: 844 }, open: "prod-web-01", type: ["ls"] },
  { name: "mobile-terminal-ctrl", query: "mobile=1&lang=en", viewport: { width: 390, height: 844 }, open: "prod-web-01", pressCtrl: true },
  { name: "mobile-settings", query: "mobile=1&lang=en", viewport: { width: 390, height: 844 }, taps: ["^Settings$"] },
  { name: "mobile-bar-editor", query: "mobile=1&lang=en", viewport: { width: 390, height: 844 }, taps: ["^Settings$", "Customise the key bar"] },
  { name: "mobile-terminal-landscape", query: "mobile=1&lang=en", viewport: { width: 844, height: 390 }, open: "prod-web-01", type: ["ls"] },
  { name: "mobile-hostkey", query: "mobile=1&lang=en&hostkey=changed", viewport: { width: 390, height: 844 } },
  { name: "mobile-tablet", query: "mobile=1&lang=en", viewport: { width: 1024, height: 768 }, open: "prod-web-01", type: ["ls"] },
];

const browser = await chromium.launch({ executablePath: process.env.CHROMIUM_PATH || undefined });
const errors = [];
for (const colorScheme of ["light", "dark"]) {
  const context = await browser.newContext({
    viewport: { width: 1200, height: 760 },
    deviceScaleFactor: Number(process.env.DSF || 2),
    colorScheme,
    locale: "zh-CN",
  });
  for (const scene of scenes) {
    const page = await context.newPage();
    page.on("pageerror", (error) => errors.push(`${scene.name}/${colorScheme}: ${error.message}`));
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(`${scene.name}/${colorScheme}: ${message.text()}`);
    });
    await page.goto(`${baseUrl}/?${scene.query}`);
    await page.waitForLoadState("networkidle");
    for (const host of scene.open ?? []) {
      await page.getByRole("button", { name: new RegExp(host) }).first().click();
      await page.waitForTimeout(300);
    }
    if (scene.local) await page.getByRole("button", { name: "打开本地终端" }).click();
    if (scene.open || scene.local) await page.locator(".xterm-screen").last().waitFor();
    await page.waitForTimeout(300);
    for (const chord of scene.panes ?? []) {
      await page.keyboard.press(chord);
      await page.waitForTimeout(400);
    }
    for (const command of scene.type ?? []) {
      await page.keyboard.type(command);
      await page.keyboard.press("Enter");
    }
    if (scene.find) {
      await page.keyboard.press("Control+Shift+F");
      await page.keyboard.type(scene.find);
      await page.keyboard.press("Enter");
    }
    if (scene.files) {
      await page.getByRole("button", { name: "文件", exact: true }).click();
      await page.getByText("deploy.sh").waitFor();
      await page.getByText("backup-2026-10-13.tar.gz").dblclick();
      await page.getByRole("button", { name: "上传", exact: true }).click();
      await page.waitForTimeout(1200);
    }
    if (scene.search) await page.getByRole("textbox").first().fill(scene.search);
    if (scene.nav) await page.getByRole("button", { name: scene.nav }).click();
    if (scene.run) await page.getByRole("button", { name: new RegExp(scene.run) }).first().click();
    if (scene.keys) {
      await page.keyboard.press(scene.keys);
      await page.keyboard.type(scene.typeText ?? "");
    }
    for (const step of scene.clicks ?? [])
      await page.getByRole(step.role ?? "button", { name: step.regex ? new RegExp(step.name) : step.name, exact: step.regex ? undefined : (step.exact ?? true) }).first().click();
    if (scene.waitText) await page.getByText(scene.waitText).first().waitFor();
    if (scene.click) await page.getByRole("button", { name: scene.click, exact: true }).first().click();
    await page.mouse.move(0, 0);
    await page.waitForTimeout(500);
    await page.screenshot({ path: join(outDir, `${scene.name}-${colorScheme}.png`) });
    await page.close();
  }
  await context.close();
}
for (const colorScheme of ["light", "dark"]) {
  for (const scene of mobileScenes) {
    const context = await browser.newContext({
      viewport: scene.viewport,
      deviceScaleFactor: Number(process.env.DSF || 2),
      colorScheme,
      hasTouch: true,
      isMobile: true,
      locale: "en-US",
    });
    const page = await context.newPage();
    page.on("pageerror", (error) => errors.push(`${scene.name}/${colorScheme}: ${error.message}`));
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(`${scene.name}/${colorScheme}: ${message.text()}`);
    });
    await page.goto(`${baseUrl}/?${scene.query}`);
    await page.waitForLoadState("networkidle");
    if (scene.click) await page.getByRole("button", { name: scene.click, exact: true }).first().click();
    if (scene.open) {
      await page.getByRole("button", { name: new RegExp(scene.open) }).first().click();
      await page.locator(".xterm-screen").waitFor();
      await page.waitForTimeout(300);
      for (const command of scene.type ?? []) {
        await page.keyboard.type(command);
        await page.keyboard.press("Enter");
      }
    }
    if (scene.pressCtrl) await page.getByRole("button", { name: /^Ctrl/ }).click();
    for (const name of scene.taps ?? []) await page.getByRole("button", { name: new RegExp(name) }).first().click();
    // Every touch control is at least 44 x 44 px (layout.md); report any that is not.
    const small = await page.evaluate(() =>
      [...document.querySelectorAll("button, input")]
        .filter((element) => element.getClientRects().length > 0)
        .map((element) => ({ label: element.getAttribute("aria-label") || element.textContent?.trim() || element.tagName, box: element.getBoundingClientRect() }))
        .filter(({ box }) => box.height < 43.5 || box.width < 43.5)
        .map(({ label, box }) => `${label} ${Math.round(box.width)}x${Math.round(box.height)}`),
    );
    if (small.length > 0) errors.push(`${scene.name}/${colorScheme}: touch targets under 44 px: ${small.join(", ")}`);
    await page.mouse.move(0, 0);
    await page.waitForTimeout(500);
    await page.screenshot({ path: join(outDir, `${scene.name}-${colorScheme}.png`) });
    await context.close();
  }
}
await browser.close();

if (errors.length > 0) {
  console.error(errors.join("\n"));
  process.exit(1);
}
console.log(`Saved ${(scenes.length + mobileScenes.length) * 2} screenshots to ${outDir}`);
