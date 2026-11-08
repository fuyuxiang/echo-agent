import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { inspectRenderedColors } from "./ui-color-audit.mjs";
import { waitForVisualPolishSurface } from "./ui-review-readiness.mjs";

// Compare real production components against HEAD, including every visible HTML
// box and text range. Fixtures replace native IPC only; no presentation CSS is added.
const output = reviewOutput(join(tmpdir(), "echo-visual-polish"));
const surfaces = ["home", "conversation", "general", "model", "agent-settings",
  "page:技能", "page:连接器", "page:项目", "coding", "execution", "modal-error",
  "controls", "toast", "workspace-picker", "permission-picker", "content-menus", "search"];
const layouts = [[1280, 800, 13], [1024, 680, 13], [1024, 680, 18]];
const records = [], differences = [], colorFailures = [], errors = [];
const browser = await (await reviewChromium()).launch({ headless: true,
  ...(process.env.CHROME_BIN ? { executablePath: process.env.CHROME_BIN } : {}) });
const snapshots = new Map();
writeFileSync(join(output, "run.json"), JSON.stringify({
  startedAt: new Date().toISOString(),
  revision: execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  diffSha256: createHash("sha256").update(execFileSync("git", ["diff", "HEAD"])).digest("hex"),
  browser: browser.version(), layouts, surfaces,
}, null, 2));

function geometry() {
  const rect = r => [r.x, r.y, r.width, r.height].map(value => Math.round(value * 100) / 100);
  const visible = e => {
    // Chromium can retain layout boxes below a closed <details> even though
    // its content is not painted. Never count those implementation details.
    for (let parent = e.parentElement; parent; parent = parent.parentElement) {
      if (parent.tagName === "DETAILS" && !parent.open
        && !parent.querySelector(":scope > summary")?.contains(e)) return false;
    }
    return e.getClientRects().length && getComputedStyle(e).visibility !== "hidden";
  };
  const path = e => {
    if (e === document.body) return "body";
    return path(e.parentElement) + "/" + e.tagName + ":" + [...e.parentElement.children].indexOf(e);
  };
  return [...document.body.querySelectorAll("*")].filter(e => e instanceof HTMLElement
    && !["STYLE", "SCRIPT", "LINK"].includes(e.tagName) && visible(e)).map(e => {
    const text = [...e.childNodes].filter(n => n.nodeType === Node.TEXT_NODE && n.textContent.trim()).flatMap(n => {
      const range = document.createRange(); range.selectNodeContents(n);
      return [...range.getClientRects()].map(rect);
    });
    return { path: path(e), className: e.className, box: rect(e.getBoundingClientRect()), text,
      scroll: [e.clientWidth, e.scrollWidth, e.clientHeight, e.scrollHeight] };
  });
}

async function prepare(page, spec) {
  if (spec === "toast") {
    await page.getByRole("button", { name: "归档示例", exact: true }).click();
    await page.locator(".toast").hover(); // pause the real dismissal timer
  }
  if (spec === "workspace-picker") await page.locator(".workspace-picker__trigger").first().click();
  if (spec === "permission-picker") await page.locator(".permission-picker__trigger").first().click();
  if (spec === "content-menus") {
    await page.getByRole("button", { name: "操作菜单示例", exact: true }).click();
    await page.getByRole("menu").waitFor();
  }
  await page.evaluate(() => document.fonts.ready);
}

async function capture(page, key, version, takeScreenshot = false) {
  const boxes = await page.evaluate(geometry);
  if (version === "before") snapshots.set(key, boxes);
  else {
    const before = snapshots.get(key);
    if (before.length !== boxes.length) differences.push({ key, kind: "element-count", before: before.length, after: boxes.length });
    for (let index = 0; index < Math.min(before.length, boxes.length); index++) {
      const a = before[index], b = boxes[index];
      if (a.path !== b.path || a.className !== b.className) {
        differences.push({ key, kind: "element-identity", before: a, after: b }); continue;
      }
      const changed = (x, y) => x.length !== y.length || x.some((v, i) => Math.abs(v - y[i]) > .25);
      if (changed(a.box, b.box) || changed(a.scroll, b.scroll)
        || a.text.length !== b.text.length || a.text.some((r, i) => b.text[i] && changed(r, b.text[i]))) {
        differences.push({ key, kind: "geometry-or-wrapping", before: a, after: b });
      }
    }
  }
  const colors = await page.evaluate(inspectRenderedColors);
  if (version === "after") colorFailures.push(...[...colors.failures, ...colors.controlFailures].map(failure => ({ key, ...failure })));
  if (takeScreenshot) await page.screenshot({ path: join(output, `${version}-${key}.png`) });
  records.push({ key, version, boxes, colors });
}

try {
  for (const version of ["before", "after"]) {
    const server = await createUiReviewServer(0, { baseline: version === "before", currentFixtures: true });
    try {
      for (const [width, height, font] of layouts) for (const theme of ["light", "dark"]) {
        for (const spec of surfaces) {
          const page = await browser.newPage({ viewport: { width, height }, colorScheme: theme });
          page.setDefaultNavigationTimeout(90000);
          page.setDefaultTimeout(15000);
          page.on("pageerror", e => errors.push({ version, spec, message: e.message }));
          await page.addInitScript(font => localStorage.setItem("echoagent.fontSize", String(font)), font);
          try {
            const [surface, label] = spec.split(":");
            const query = new URLSearchParams({ surface, theme, extensionState: "populated", settingsDesign: "1", ...(label ? { label } : {}) });
            await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?${query}`, { waitUntil: "networkidle" });
            await waitForVisualPolishSurface(page, spec);
            await page.evaluate(font => document.documentElement.style.fontSize = `${16 * font / 13}px`, font);
            await prepare(page, spec);
            const key = `${spec.replace(":", "-")}-${theme}-${width}-${font}`;
            await capture(page, key, version, width === 1280);
            if (spec === "home") {
              const input = page.locator(".echo-composer__input");
              await input.fill("整理 UI 精修验收结果，保留已有布局与交互。");
              await input.focus();
              await capture(page, `${key}-focus`, version, width === 1280);
              await page.locator(".model-selector__trigger").click();
              await page.getByRole("listbox").waitFor();
              await capture(page, `${key}-model-menu`, version, width === 1280);
              await page.keyboard.press("Escape");
            }
            if (spec === "controls") {
              const danger = page.locator(".echo-button--danger");
              await danger.hover();
              await capture(page, `${key}-hover`, version);
              await page.mouse.down();
              await capture(page, `${key}-pressed`, version);
              await page.mouse.move(0, 0); await page.mouse.up();
              await page.getByRole("textbox", { name: "名称", exact: true }).focus();
              await capture(page, `${key}-focus`, version);
            }
          } finally { await page.close(); }
        }
        console.log(`${version}: ${width}×${height}, ${theme}, font ${font}`);
      }
    } finally { await server.close(); }
  }
} finally {
  writeFileSync(join(output, "geometry.json"), JSON.stringify(records, null, 2));
  writeFileSync(join(output, "differences.json"), JSON.stringify(differences, null, 2));
  writeFileSync(join(output, "color-failures.json"), JSON.stringify(colorFailures, null, 2));
  writeFileSync(join(output, "browser-errors.json"), JSON.stringify(errors, null, 2));
  await browser.close();
}
assert.deepEqual(errors, [], "Production components must render without browser errors");
assert.equal(differences.length, 0, `Geometry or wrapping changed; inspect ${join(output, "differences.json")}`);
assert.equal(colorFailures.length, 0, `AA contrast failed; inspect ${join(output, "color-failures.json")}`);
console.log(JSON.stringify({ observations: records.length, geometryDifferences: differences.length, colorFailures: colorFailures.length, output }));
