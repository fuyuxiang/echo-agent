import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { inspectButtonBackgrounds, assertButtonBackgrounds } from "./ui-button-audit.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-brand-palette-")));
let server, browser;
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
    const selected = token("--echo-control-selected-bg");
    const selectedFg = token("--echo-control-selected-fg");
    const primary = token("--echo-button-primary-bg");
    const primaryFg = token("--echo-button-primary-fg");
    const states = [selected, token("--echo-control-selected-bg-hover"), token("--echo-control-selected-bg-active")];
    const primaryStates = [primary, token("--echo-button-primary-bg-hover"), token("--echo-button-primary-bg-active")];
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
        const selectedKind = el.matches(".pd-pill--on") ? "project"
          : el.matches('.atm-task-filter[aria-pressed="true"]') ? "schedule"
          : el.matches(".task-filter-menu__option--selected") ? "sidebar" : null;
        return { name: (el.getAttribute("aria-label") || el.textContent || "").trim().slice(0, 80), className: el.className, selectedKind, background: rgba(style.backgroundColor), color: rgba(style.color) };
      });
    // State controls carry a distinct selected role, irrespective of brand hue.
    const selectedControls = controls.filter(control => control.selectedKind);
    const selectedCounts = Object.fromEntries(["project", "schedule", "sidebar"].map(kind => [kind, selectedControls.filter(control => control.selectedKind === kind).length]));
    const drift = selectedControls.filter(control => !states.some(state => close(control.background, state)) || !close(control.color, selectedFg));
    const branded = controls.filter(control => [...states, ...primaryStates].some(state => close(control.background, state)));
    const unreadable = branded.filter(control => contrast(control.background, control.color) < 4.5);
    const switches = [...document.querySelectorAll(".echo-switch--checked,.sk-toggle input:checked + .sk-toggle-track,.skill-item-toggle input:checked + .skill-item-toggle-track")]
      .filter(el => el.checkVisibility()).map(el => {
        const thumb = el.querySelector(".echo-switch-thumb,.sk-toggle-thumb,.skill-item-toggle-thumb");
        const trackColor = rgba(getComputedStyle(el).backgroundColor);
        const thumbColor = rgba(getComputedStyle(thumb).backgroundColor);
        return { className: el.className, trackColor, thumbColor, unified: close(trackColor, selected), contrast: contrast(trackColor, thumbColor) };
      });
    return { selected, selectedFg, primary, primaryFg, distinct: !close(primary, selected), branded: branded.length, selectedCounts, drift, unreadable, switches };
  });
}

function assertPalette(result, label, requiredSelections = {}) {
  assert(result.distinct, `${label}: primary and selected roles must be distinct`);
  for (const [kind, minimum] of Object.entries(requiredSelections)) {
    assert(result.selectedCounts[kind] >= minimum, `${label}: expected at least ${minimum} measured ${kind} selections`);
  }
  assert.deepEqual(result.drift, [], `${label}: selected palette drift`);
  assert.deepEqual(result.unreadable, [], `${label}: paired labels below 4.5:1 contrast`);
  assert(result.switches.every(control => control.unified && control.contrast >= 3), `${label}: enabled switch is unclear`);
}

async function checkSelectedStates(page, kind, selector, minimum, theme) {
  const control = page.locator(selector).first();
  // Prove that a wrongly styled selected control is detected even when its
  // background no longer matches any known brand colour.
  const previous = await control.getAttribute("style");
  try {
    await control.evaluate(element => { element.style.backgroundColor = "#808080"; element.style.color = "#808080"; });
    const negative = await inspectPalette(page);
    assert(negative.drift.some(item => item.selectedKind === kind), `${kind}: the negative control must be measured`);
    assert.throws(() => assertPalette(negative, kind, { [kind]: minimum }), /selected palette drift/);
  } finally {
    await control.evaluate((element, style) => { if (style === null) element.removeAttribute("style"); else element.setAttribute("style", style); }, previous);
  }
  for (const state of ["default", "hover", "pressed"]) {
    if (state === "default") await page.mouse.move(0, 0);
    if (state === "hover") await control.hover();
    if (state === "pressed") await page.mouse.down();
    try {
      const result = await inspectPalette(page);
      assertPalette(result, `${kind}/${theme}/${state}`, { [kind]: minimum });
      evidence.push({ surface: `${kind}-selected`, theme, state, ...result });
      writeFileSync(join(output, "evidence.json"), JSON.stringify(evidence, null, 2));
      await page.screenshot({ path: join(output, `${kind}-selected-${state}-${theme}.png`) });
    } finally { if (state === "pressed") await page.mouse.up(); }
  }
}

try {
  server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/theme-review-router.ts" });
  browser = await (await reviewChromium()).launch({ headless: true,
    executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  const routes = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
  const base = `${routes}/ui-surfaces`;
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
      // Empty management pages have no filter yet. Required selection counts
      // are checked below against populated, interactive production fixtures.
      assertPalette(result, `${surface}/${theme}`);
      assert.deepEqual(errors, [], `${surface}/${theme}: browser errors`);
      evidence.push({ surface, theme, ...result });
      writeFileSync(join(output, "evidence.json"), JSON.stringify(evidence, null, 2));
      await page.screenshot({ path: join(output, `${surface}-${theme}.png`) });
    }
    // Real switch interactions retain their state and use the same surface as
    // the selected task filter, including when the text size is enlarged.
    await page.goto(`${routes}/scheduled-tasks-review?theme=${theme}`, { waitUntil: "networkidle" });
    const toggle = page.getByRole("switch", { name: "启用定时任务「每日 AI 新闻简报」", exact: true });
    await toggle.waitFor();
    const checked = await toggle.getAttribute("aria-checked");
    await toggle.click();
    await page.waitForFunction(before => document.querySelector('[aria-label="启用定时任务「每日 AI 新闻简报」"]')?.getAttribute("aria-checked") !== before, checked);
    await toggle.click();
    await page.waitForFunction(before => document.querySelector('[aria-label="启用定时任务「每日 AI 新闻简报」"]')?.getAttribute("aria-checked") === before, checked);
    const identical = await page.evaluate(() => getComputedStyle(document.querySelector('.atm-task-filter[aria-pressed="true"]')).backgroundColor === getComputedStyle(document.querySelector(".echo-switch--checked")).backgroundColor);
    assert(identical, "task filter and enabled switch must share exactly the same fill");
    await checkSelectedStates(page, "schedule", '.atm-task-filter[aria-pressed="true"]', 1, theme);
    await page.evaluate(() => { document.documentElement.style.fontSize = `${16*18/13}px`; });
    await page.screenshot({ path: join(output, `schedules-large-text-${theme}.png`) });

    await page.goto(`${routes}/enterprise-theme-review?surface=project&theme=${theme}`, { waitUntil: "networkidle" });
    await page.getByRole("button", { name: "动态", exact: true }).click();
    await checkSelectedStates(page, "project", ".pd-pill--on", 1, theme);

    await page.goto(`${base}?surface=workbench&state=plan&theme=${theme}`, { waitUntil: "networkidle" });
    await page.getByRole("button", { name: "筛选任务", exact: true }).click();
    await page.getByRole("menu", { name: "任务筛选", exact: true }).waitFor();
    await checkSelectedStates(page, "sidebar", ".task-filter-menu__option--selected", 2, theme);

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
  try { await browser?.close(); } finally { await server?.close(); }
}
