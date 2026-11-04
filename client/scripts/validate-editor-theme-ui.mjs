// Exercise the production host frame and bridge against a real staged Theia
// backend. All files, settings and terminal processes belong to a temp workspace.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtempSync, mkdirSync, writeFileSync, createWriteStream, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { createServer } from "node:net";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { inspectEditorPaint } from "./editor-paint-audit.mjs";

const output = reviewOutput(join(tmpdir(), "echo-editor-theme"));
const directory = mkdtempSync(join(tmpdir(), "echo-editor-theme-workspace-"));
mkdirSync(join(directory, "workspace"));
const workspace = realpathSync(join(directory, "workspace"));
const source = "export const status: string = 'completed';\n// 保留语法颜色与可读的差异标记\nexport const retries: number = 3;\nconsole.log(status);\n";
writeFileSync(join(workspace, "status.ts"), source);
const token = randomUUID();
const socket = createServer();
await new Promise(resolve => socket.listen(0, "127.0.0.1", resolve));
const port = socket.address().port;
await new Promise(resolve => socket.close(resolve));
const url = `http://127.0.0.1:${port}/`;
const runtime = resolve("src-tauri/resources/theia");
const log = createWriteStream(join(output, "theia-backend.log"));
const backend = spawn(join(runtime, "node/bin/node"), ["lib/backend/main.js", "--port", String(port), "--hostname", "127.0.0.1", "--plugins=local-dir:plugins"], {
  cwd: join(runtime, "browser"), detached: true,
  env: { ...process.env, THEIA_CONFIG_DIR: join(directory, "config"), ECHO_THEIA_EMBED_TOKEN: token },
  stdio: ["ignore", "pipe", "pipe"],
});
backend.stdout.pipe(log); backend.stderr.pipe(log);
backend.on("error", error => { console.error(error); });
let server, workbenchServer, browser, page;
const evidence = [], browserErrors = [];
const luminance = hex => [1, 3, 5].map(index => parseInt(hex.slice(index, index + 2), 16) / 255).map(value => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4).reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
try {
  let ready = false;
  for (let attempt = 0; attempt < 180; attempt++) {
    if (backend.exitCode !== null) throw new Error(`Theia exited: ${backend.exitCode}; see ${output}/theia-backend.log`);
    try {
      const response = await fetch(`${url}__echo_health?echoEmbedToken=${token}`);
      ready = response.status === 204 && response.headers.get("x-echo-theia-ready") === token;
    } catch { /* backend still starting */ }
    if (ready) break;
    await delay(500);
  }
  assert(ready, "Real Theia backend did not become ready");
  server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/enterprise-theme-review.tsx" });
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  page = await browser.newPage({ viewport: { width: 1280, height: 800 }, colorScheme: "light" });
  page.on("pageerror", error => browserErrors.push(error.message));
  const query = new URLSearchParams({ surface: "real-ide", theme: "light", ideUrl: url, embedToken: token, root: workspace });
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?${query}`, { waitUntil: "networkidle", timeout: 90000 });
  const ide = page.frameLocator("iframe");
  await ide.locator(".monaco-editor .view-lines").first().waitFor({ timeout: 120000 });
  await ide.getByText("export", { exact: true }).first().waitFor();
  const frame = page.frames().find(frame => frame.url().startsWith(url));
  assert(frame, "The real IDE iframe must be present");
  const documentIdentity = await frame.evaluate(() => { window.themeReviewIdentity = crypto.randomUUID(); return window.themeReviewIdentity; });
  const capture = async (state, theme) => {
    const host = await page.evaluate(() => {
      const css = getComputedStyle(document.documentElement);
      return Object.fromEntries(["surface-content", "surface-sidebar", "text-strong", "text-medium", "code-comment", "code-constant"].map(role => [role, css.getPropertyValue(`--echo-${role}`).trim().toLowerCase()]));
    });
    await frame.waitForFunction(expected => {
      const comment = [...document.querySelectorAll(".view-lines span[class*=mtk]")].find(element => element.textContent.trim().startsWith("//"));
      const probe = document.createElement("span"); probe.style.color = expected; document.body.append(probe);
      const color = getComputedStyle(probe).color; probe.remove();
      return comment && getComputedStyle(comment).color === color;
    }, host["code-comment"], { timeout: 30000 });
    const actual = await frame.evaluate(() => {
      const css = getComputedStyle(document.documentElement);
      const properties = ["editor-background", "editor-foreground", "statusBar-background", "statusBar-foreground", "terminal-background", "terminal-foreground"];
      const colors = Object.fromEntries(properties.map(role => [role, css.getPropertyValue(`--theia-${role}`).trim().toLowerCase()]));
      const editor = document.querySelector(".monaco-editor .monaco-scrollable-element");
      const syntax = [...document.querySelectorAll(".view-lines span[class*=mtk]")].map(element => ({ text: element.textContent, color: getComputedStyle(element).color }));
      return { colors, syntax, editorBackground: editor && getComputedStyle(editor).backgroundColor, identity: window.themeReviewIdentity, className: document.body.className };
    });
    assert.equal(actual.identity, documentIdentity, "Live theme changes must retain the mounted IDE and file");
    const constantColor = await frame.evaluate(color => {
      const probe = document.createElement("span"); probe.style.color = color; document.body.append(probe);
      const value = getComputedStyle(probe).color; probe.remove(); return value;
    }, host["code-constant"]);
    assert(actual.syntax.some(token => token.text.trim() === "status" && token.color === constantColor), `${state}/${theme}: constant syntax must use the readable host role`);
    for (const [name, role] of [["editor-background", "surface-content"], ["editor-foreground", "text-strong"], ["statusBar-background", "surface-sidebar"], ["statusBar-foreground", "text-medium"], ["terminal-background", "surface-content"], ["terminal-foreground", "text-strong"]]) {
      assert.equal(actual.colors[name], host[role], `${state}/${theme}: ${name} must follow host role ${role}`);
    }
    const paint = await inspectEditorPaint(page, frame, join(output, `${state}-${theme}-backdrop.png`), { diffHighlights: state === "diff" });
    writeFileSync(join(output, `${state}-${theme}-paint.json`), JSON.stringify(paint, null, 2));
    assert.deepEqual(paint.failures, [], `${state}/${theme}: actual syntax paint on editor decorations`);
    assert.deepEqual(paint.highlightFailures, [], `${state}/${theme}: character ranges must remain distinct from changed lines`);
    await page.screenshot({ path: join(output, `${state}-${theme}.png`) });
    let terminalPalette;
    if (state === "terminal") {
      terminalPalette = await frame.evaluate(() => {
        const css = getComputedStyle(document.documentElement);
        return Object.fromEntries(["background", "ansiRed", "ansiGreen", "ansiYellow", "ansiBlue", "ansiMagenta", "ansiCyan", "ansiBrightBlack", "ansiBrightRed", "ansiBrightGreen", "ansiBrightYellow", "ansiBrightBlue", "ansiBrightMagenta", "ansiBrightCyan"].map(role => [role, css.getPropertyValue(`--theia-terminal-${role}`).trim()]));
      });
      for (const [role, color] of Object.entries(terminalPalette)) {
        if (role === "background") continue;
        assert.match(color, /^#[\da-f]{6}$/i);
        const [light, dark] = [luminance(color), luminance(terminalPalette.background)].sort((a, b) => b - a);
        assert((light + .05) / (dark + .05) >= 4.5, `${theme}: readable ANSI ${role}`);
      }
    }
    evidence.push({ state, theme, host, paint, terminalPalette, ...actual });
    writeFileSync(join(output, "evidence.json"), JSON.stringify({ engine: await browser.version(), workspace, browserErrors, evidence }, null, 2));
  };
  for (const theme of ["light", "dark", "light"]) {
    await page.evaluate(theme => window.enterpriseThemeReview.setTheme(theme), theme);
    await frame.waitForFunction(theme => document.body.classList.contains(`theia-${theme}`), theme);
    await frame.waitForFunction(color => getComputedStyle(document.documentElement).getPropertyValue("--theia-editor-background").trim().toLowerCase() === color, theme === "dark" ? "#1b1f28" : "#ffffff");
    await capture("editor", theme);
  }
  const context = await page.evaluate(() => window.enterpriseThemeReview.ideContext());
  assert(context, "Production bridge must return a real editor context");
  await page.evaluate(() => window.enterpriseThemeReview.ideDiff());
  await ide.locator(".monaco-diff-editor").waitFor();
  for (const theme of ["light", "dark"]) {
    await page.evaluate(theme => window.enterpriseThemeReview.setTheme(theme), theme);
    await frame.waitForFunction(theme => document.body.classList.contains(`theia-${theme}`), theme);
    await capture("diff", theme);
  }
  // The command palette opens a real local terminal; the command prints only
  // fixed status samples and does not read user files or modify project data.
  await ide.locator(".monaco-editor textarea").first().focus();
  await page.keyboard.press(process.platform === "darwin" ? "Meta+Shift+P" : "Control+Shift+P");
  const palette = ide.locator(".quick-input-widget input");
  await palette.waitFor();
  await palette.fill(">终端");
  await delay(500);
  writeFileSync(join(output, "terminal-commands.txt"), await ide.locator(".quick-input-widget").innerText());
  const command = ide.locator(".quick-input-widget .monaco-list-row").filter({ hasText: /新建终端|创建.*终端|Create New Terminal|New Terminal/ }).first();
  await command.waitFor();
  await command.click();
  await ide.locator(".xterm-helper-textarea").waitFor();
  await ide.locator(".xterm-helper-textarea").focus();
  await page.keyboard.type("printf '\\033[32mSUCCESS\\033[0m  \\033[33mWARNING\\033[0m  \\033[31mERROR\\033[0m\\n\\033[92mBRIGHT_SUCCESS\\033[0m  \\033[93mBRIGHT_WARNING\\033[0m  \\033[91mBRIGHT_ERROR\\033[0m\\n'");
  await page.keyboard.press("Enter");
  await delay(1200);
  for (const theme of ["dark", "light"]) {
    await page.evaluate(theme => window.enterpriseThemeReview.setTheme(theme), theme);
    await frame.waitForFunction(theme => document.body.classList.contains(`theia-${theme}`), theme);
    await capture("terminal", theme);
  }
  const terminalContext = await page.evaluate(() => window.enterpriseThemeReview.ideContext());
  assert(terminalContext.terminal?.text.includes("SUCCESS  WARNING  ERROR"), "The real PTY must contain the printed status samples");
  assert(terminalContext.terminal?.text.includes("BRIGHT_SUCCESS  BRIGHT_WARNING  BRIGHT_ERROR"), "The real PTY must contain bright ANSI samples too");
  workbenchServer = await createUiReviewServer(0, { fixture: "/scripts/fixtures/coding-workbench-theme-review.tsx" });
  query.set("theme", "light");
  await page.goto(`http://127.0.0.1:${workbenchServer.httpServer.address().port}/__ui-review?${query}`, { waitUntil: "networkidle", timeout: 90000 });
  const workbenchIde = page.frameLocator("iframe");
  await workbenchIde.locator("#theia-app-shell").waitFor({ timeout: 90000 });
  await workbenchIde.getByText("status.ts", { exact: true }).first().dblclick();
  await workbenchIde.locator(".monaco-editor .view-lines").first().waitFor();
  const workbenchFrame = page.frames().find(frame => frame.url().startsWith(url));
  for (const [width, height, font] of [[1280, 800, 13], [1024, 680, 18]]) for (const theme of ["light", "dark"]) {
    await page.setViewportSize({ width, height });
    await page.evaluate(({ theme, font }) => {
      document.documentElement.style.fontSize = `${16 * font / 13}px`;
      window.codingWorkbenchThemeReview.setTheme(theme);
    }, { theme, font });
    await workbenchIde.locator(`body.theia-${theme}`).waitFor();
    await page.locator(".echo-theia-agent .echo-theia-agent__input").waitFor();
    await workbenchFrame.waitForFunction(expected => {
      const comment = [...document.querySelectorAll(".view-lines span[class*=mtk]")].find(element => element.textContent.trim().startsWith("//"));
      return comment && getComputedStyle(comment).color === expected;
    }, theme === "dark" ? "rgb(156, 184, 147)" : "rgb(78, 106, 88)");
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false);
    const colors = await workbenchFrame.evaluate(() => ({
      background: getComputedStyle(document.documentElement).getPropertyValue("--theia-editor-background").trim().toLowerCase(),
      statusbar: getComputedStyle(document.documentElement).getPropertyValue("--theia-statusBar-background").trim().toLowerCase(),
    }));
    assert.equal(colors.background, theme === "dark" ? "#1b1f28" : "#ffffff");
    const activityIcons = await workbenchFrame.evaluate(() => {
      const canvas = document.createElement("canvas"); canvas.width = canvas.height = 1;
      const context = canvas.getContext("2d", { willReadFrequently: true });
      const rgba = value => {
        context.clearRect(0, 0, 1, 1); context.fillStyle = value; context.fillRect(0, 0, 1, 1);
        return [...context.getImageData(0, 0, 1, 1).data];
      };
      const background = rgba(getComputedStyle(document.documentElement).getPropertyValue("--theia-activityBar-background"));
      const luminance = channels => channels.slice(0, 3).map(value => value / 255).map(value => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4).reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
      return [...document.querySelectorAll(".theia-app-sides .lm-TabBar-tabIcon")].filter(icon => icon.getBoundingClientRect().width > 0).map(icon => {
        const style = getComputedStyle(icon);
        const color = style.maskImage !== "none" ? style.backgroundColor : style.color;
        const foreground = rgba(color);
        let opacity = foreground[3] / 255;
        for (let ancestor = icon; ancestor; ancestor = ancestor.parentElement) opacity *= Number(getComputedStyle(ancestor).opacity);
        const composed = foreground.slice(0, 3).map((value, index) => value * opacity + background[index] * (1 - opacity));
        const levels = [luminance(composed), luminance(background)].sort((a, b) => b - a);
        return { label: icon.closest(".lm-TabBar-tab")?.title, color, background, ratio: (levels[0] + .05) / (levels[1] + .05) };
      });
    });
    assert(activityIcons.length >= 5, "Actual activity bar icons must participate in the review");
    for (const icon of activityIcons) assert(icon.ratio >= 3, `${theme}: activity icon ${icon.label} contrast ${icon.ratio}`);
    const file = `workbench-${theme}${width === 1280 ? "" : `-${width}-${font}`}.png`;
    await page.screenshot({ path: join(output, file) });
    const paint = await inspectEditorPaint(page, workbenchFrame, join(output, file.replace(".png", "-backdrop.png")));
    assert.deepEqual(paint.failures, [], `${file}: actual workbench syntax paint`);
    evidence.push({ state: "workbench", theme, width, height, font, colors, file, paint, activityIcons });
  }
  assert.deepEqual(browserErrors, [], "Real host/IDE browser exceptions");
  const workbenchCalls = await page.evaluate(() => window.codingWorkbenchThemeReview.calls);
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ engine: await browser.version(), workspace, browserErrors, terminalOutput: terminalContext.terminal?.text, workbenchCalls, evidence }, null, 2));
  console.log(JSON.stringify({ passed: true, snapshots: evidence.length, output }));
} catch (error) {
  if (page) {
    await page.screenshot({ path: join(output, "failure.png") }).catch(() => {});
    writeFileSync(join(output, "failure.json"), JSON.stringify({ message: String(error), browserErrors, frames: page.frames().map(frame => frame.url()) }, null, 2));
  }
  throw error;
} finally {
  await browser?.close();
  await workbenchServer?.close();
  await server?.close();
  await fetch(`${url}__echo_shutdown`, { method: "POST", headers: { "x-echo-shutdown-token": token } }).catch(() => {});
  if (backend.exitCode === null) {
    await Promise.race([new Promise(resolve => backend.once("exit", resolve)), delay(5000)]);
    if (backend.exitCode === null) { try { process.kill(-backend.pid, "SIGTERM"); } catch {} }
  }
  log.end();
}
