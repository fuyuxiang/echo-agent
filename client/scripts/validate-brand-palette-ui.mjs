import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium } from "./ui-review-runtime.mjs";
import { inspectButtonBackgrounds, assertButtonBackgrounds } from "./ui-button-audit.mjs";

const output = mkdtempSync(join(tmpdir(), "echo-brand-palette-"));
const server = await createUiReviewServer(0);
const scheduledServer = await createUiReviewServer(0, { fixture: "/scripts/fixtures/scheduled-tasks-review.tsx" });
const browser = await (await reviewChromium()).launch({
  headless: true,
  executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
});
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
const evidence = [];
console.log(`Brand palette screenshots: ${output}`);

async function inspectPalette(page) {
  return page.evaluate(() => {
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = 1;
    const context = canvas.getContext("2d", { willReadFrequently: true });
    const rgba = value => {
      context.clearRect(0, 0, 1, 1);
      context.fillStyle = value;
      context.fillRect(0, 0, 1, 1);
      return [...context.getImageData(0, 0, 1, 1).data];
    };
    const probe = document.createElement("span");
    document.body.append(probe);
    const token = name => {
      probe.style.backgroundColor = `var(${name})`;
      return rgba(getComputedStyle(probe).backgroundColor);
    };
    const mint = token("--echo-brand-soft");
    const states = [mint, token("--echo-brand-soft-hover"), token("--echo-brand-soft-active")];
    probe.remove();
    const close = (a, b) => a.every((channel, index) => Math.abs(channel - b[index]) <= 1);
    const luminance = color => color.slice(0, 3).map(channel => {
      const value = channel / 255;
      return value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4;
    }).reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
    const contrast = (a, b) => {
      const foreground = b.slice(0, 3).map((channel, index) => channel * b[3] / 255 + a[index] * (1 - b[3] / 255));
      const [light, dark] = [luminance(a), luminance(foreground)].sort((x, y) => y - x);
      return (light + .05) / (dark + .05);
    };
    const controls = [...document.querySelectorAll("button,[role=button],.echo-checkbox--checked .echo-checkbox-box")]
      .filter(el => el.checkVisibility() && !el.matches(":disabled,[aria-disabled=true]") && !el.closest(".monaco-editor,.xterm,.markdown-body,.md-content,[class*=avatar]"))
      .map(el => {
        const style = getComputedStyle(el);
        return { name: (el.getAttribute("aria-label") || el.textContent || "").trim().slice(0, 80), className: el.className, background: rgba(style.backgroundColor), color: rgba(style.color) };
      });
    const green = controls.filter(({ background: [r, g, b, a] }) => a > 20 && g > r + 6 && g > b + 2);
    const drift = green.filter(control => !states.some(state => close(control.background, state)));
    const unreadable = green.filter(control => contrast(control.background, control.color) < 4.5);
    const switches = [...document.querySelectorAll(".echo-switch--checked,.sk-toggle input:checked + .sk-toggle-track,.skill-item-toggle input:checked + .skill-item-toggle-track")]
      .filter(el => el.checkVisibility()).map(el => {
        const thumb = el.querySelector(".echo-switch-thumb,.sk-toggle-thumb,.skill-item-toggle-thumb");
        const trackColor = rgba(getComputedStyle(el).backgroundColor);
        const thumbColor = rgba(getComputedStyle(thumb).backgroundColor);
        return { className: el.className, trackColor, thumbColor, unified: close(trackColor, mint), contrast: contrast(trackColor, thumbColor) };
      });
    return { mint, green: green.length, drift, unreadable, switches };
  });
}

try {
  const surfaces = ["controls", "home", "projects", "management-schedules", "management-experts", "management-knowledge", "organization-shell", "organization-login", "conversation", "coding", "meeting", "general", "personal-memory", "notify-channels", "model", "archived"];
  for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, colorScheme: theme });
    page.setDefaultNavigationTimeout(60000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    for (const surface of surfaces) {
      await page.goto(`${base}?surface=${surface}&theme=${theme}&orgState=many`, { waitUntil: "networkidle" });
      await page.locator("button").first().waitFor();
      await page.mouse.move(0, 0);
      const audit = await inspectButtonBackgrounds(page, theme);
      assertButtonBackgrounds(audit, `${surface}/${theme}`);
      const result = await inspectPalette(page);
      assert.deepEqual(result.drift, [], `${surface}/${theme}: mixed brand backgrounds`);
      assert.deepEqual(result.unreadable, [], `${surface}/${theme}: mint labels below 4.5:1 contrast`);
      assert(result.switches.every(control => control.unified && control.contrast >= 3), `${surface}/${theme}: enabled switch is unclear`);
      assert.deepEqual(errors, [], `${surface}/${theme}: browser errors`);
      evidence.push({ surface, theme, ...result });
      writeFileSync(join(output, "evidence.json"), JSON.stringify(evidence, null, 2));
      await page.screenshot({ path: join(output, `${surface}-${theme}.png`) });
    }
    // Real switch interactions retain their state and use the same surface as
    // the selected task filter, including when the text size is enlarged.
    await page.goto(`http://127.0.0.1:${scheduledServer.httpServer.address().port}/__ui-review?theme=${theme}`, { waitUntil: "networkidle" });
    const toggle = page.getByRole("switch", { name: "启用定时任务「每日 AI 新闻简报」", exact: true });
    await toggle.waitFor();
    const checked = await toggle.getAttribute("aria-checked");
    await toggle.click();
    await page.waitForFunction(before => document.querySelector('[aria-label="启用定时任务「每日 AI 新闻简报」"]')?.getAttribute("aria-checked") !== before, checked);
    await toggle.click();
    await page.waitForFunction(before => document.querySelector('[aria-label="启用定时任务「每日 AI 新闻简报」"]')?.getAttribute("aria-checked") === before, checked);
    const identical = await page.evaluate(() => getComputedStyle(document.querySelector('.atm-task-filter[aria-pressed="true"]')).backgroundColor === getComputedStyle(document.querySelector(".echo-switch--checked")).backgroundColor);
    assert(identical, "task filter and enabled switch must share exactly the same fill");
    await page.evaluate(() => { document.documentElement.style.fontSize = "22px"; });
    await page.screenshot({ path: join(output, `schedules-large-text-${theme}.png`) });

    await page.goto(`${base}?surface=controls&theme=${theme}`, { waitUntil: "networkidle" });
    const button = page.locator(".btn--primary").first();
    for (const state of ["default", "hover", "pressed"]) {
      if (state === "hover") await button.hover();
      if (state === "pressed") await page.mouse.down();
      const result = await inspectPalette(page);
      assert.deepEqual(result.unreadable, [], `${theme}/${state}: button labels must remain readable`);
      assert.deepEqual(result.drift, [], `${theme}/${state}: button palette drift`);
    }
    await page.mouse.up();
    await page.close();
    console.log(`${theme}: ${surfaces.length} pages and switch/button interactions passed`);
  }
  console.log(JSON.stringify({ output, layouts: evidence.length, checkedSwitches: evidence.reduce((count, item) => count + item.switches.length, 0) }));
} finally {
  await browser.close();
  await server.close();
  await scheduledServer.close();
}
