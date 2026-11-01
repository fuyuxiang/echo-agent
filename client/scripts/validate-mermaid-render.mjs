// Real production component + CSS + SVG image decoder. Do not freeze motion:
// doing so hides the reduced-motion label/geometry regression this guards.
import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { execFile } from "node:child_process";
import { tmpdir } from "node:os";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-mermaid-render-")));
const server = await createUiReviewServer(0, { freezeMotion: false, fixture: "/scripts/fixtures/mermaid-diagram.tsx" });
const base = `http://127.0.0.1:${server.httpServer.address().port}`;
let browser;
const evidence = [];
const baselineSizes = new Map();
const normalized = text => text.replace(/[\s\u00a0]+/gu, " ").trim();
const fits = values => Object.values(values).every(value => Number.isFinite(value) && value >= -0.75);
function paintContrast(foreground, background, opacity = 1) {
  const rgb = color => {
    const channels = color.match(/[\d.]+/g)?.map(Number);
    assert.ok(channels && channels.length >= 3, `Unmeasurable SVG paint: ${color}`);
    return [...channels.slice(0,3).map(c => color.startsWith("color(srgb ") ? c * 255 : c), channels[3] ?? 1];
  };
  const back = rgb(background), front = rgb(foreground), alpha = front[3] * opacity;
  assert.equal(back[3],1,"SVG backdrop must be composited before measuring contrast");
  const bg = back.slice(0,3), fg = front.slice(0,3).map((c,i) => c * alpha + bg[i] * (1-alpha));
  const luminance = color => color.map(c => c / 255).map(c => c <= .04045 ? c / 12.92 : ((c+.055)/1.055)**2.4).reduce((s,c,i) => s+c*[.2126,.7152,.0722][i],0);
  const levels = [luminance(fg),luminance(bg)].sort((a,b)=>b-a);
  return (levels[0]+.05)/(levels[1]+.05);
}
assert.ok(Math.abs(paintContrast("rgba(0,0,0,.5)","rgb(255,255,255)") - 3.97665) < .001);
function validateBounds(result, key) {
  const [, , width, height] = result.viewBox;
  assert.ok(result.viewBox.length === 4 && result.viewBox.every(Number.isFinite) && width > 0 && height > 0, `${key}: invalid viewBox`);
  assert.ok(width < 6000 && height < 6000, `${key}: diagram collapsed due to corrupted bounds`);
  assert.ok(Math.abs(result.svgWidth - width) <= 0.001 && Math.abs(result.svgHeight - height) <= 0.001, `${key}: root SVG dimensions differ from viewBox`);
  assert.ok(Math.abs(result.imageWidth - width) <= 1 && Math.abs(result.imageHeight - height) <= 1, `${key}: image decoder dimensions differ from SVG`);
  assert.equal(result.overlaps.length, 0, `${key}: overlapping card/text unions ${JSON.stringify(result.overlaps)}`);
  assert.equal(result.textOverlaps.length, 0, `${key}: overlapping text ${JSON.stringify(result.textOverlaps)}`);
  for (const node of result.nodes) {
    assert.ok(node.text === "" || node.textRects.length > 0, `${key}: label has no measurable text ${node.text}`);
    assert.ok(fits(node.viewportSlack), `${key}: node or text extends outside viewBox ${JSON.stringify(node)}`);
    assert.ok(node.cardSlack === null || fits(node.cardSlack), `${key}: text extends outside card ${JSON.stringify(node)}`);
    assert.ok(node.curvedOverflow === null || node.curvedOverflow <= 0.75, `${key}: text extends outside curved shape ${JSON.stringify(node)}`);
  }
  for (const edge of result.edgeDetails) {
    assert.ok(paintContrast(edge.stroke,result.surface,edge.strokeOpacity) >= 3, `${key}: tree connection contrast below 3:1 (${edge.stroke})`);
    assert.ok(Number.isFinite(edge.length) && edge.length > 0, `${key}: invalid edge ${JSON.stringify(edge)}`);
    assert.ok(fits(edge.viewportSlack), `${key}: edge extends outside viewBox ${JSON.stringify(edge)}`);
  }
}
function validateMindmap(result, expected, key) {
  validateBounds(result, key);
  assert.equal(result.nodes.length, expected.labels.length, `${key}: missing nodes`);
  assert.deepEqual(result.nodes.map(node => node.text).sort(), expected.labels.map(normalized).sort(), `${key}: changed labels`);
  assert.equal(result.edges, expected.edges.length, `${key}: missing or duplicate edges`);
  assert.deepEqual(result.edgeDetails.map(edge => [edge.from, edge.to]).sort(), expected.edges.map(edge => edge.map(normalized)).sort(), `${key}: incorrect tree connections`);
  for (const edge of result.edgeDetails) {
    assert.ok(edge.startGap !== null && edge.startGap <= 3 && edge.endGap !== null && edge.endGap <= 3,
      `${key}: edge no longer touches its node outlines ${JSON.stringify(edge)}`);
  }
  for (const node of result.nodes) {
    for (const color of node.textColors) assert.ok(paintContrast(color,node.fill) >= 4.5, `${key}: diagram label contrast below 4.5:1 (${color} on ${node.fill})`);
    assert.equal(node.html, true, `${key}: mindmap labels lost wrapping`);
    assert.equal(node.whiteSpace, "pre-wrap", `${key}: standalone SVG lost wrapping CSS`);
    assert.equal(node.overflowWrap, "anywhere", `${key}: identifier cannot wrap`);
    assert.ok(node.labelWidth <= 241, `${key}: oversized label ${node.text}`);
    if (!node.text) continue;
    assert.ok(node.boxWidth - node.labelWidth >= 7, `${key}: missing clipping slack ${node.text}`);
    assert.ok(node.slack !== null && fits(node.slack), `${key}: foreignObject clips text ${JSON.stringify(node)}`);
    // Cloud and bang outlines have asymmetric bounding boxes.
    if (["rect", "circle", "ellipse"].includes(node.shape)) {
      assert.ok(Math.abs(node.centerOffset) < 1, `${key}: off-center label ${node.text}`);
    }
    if (["write", "grep"].includes(node.text)) {
      assert.ok(node.cardWidth < 100 && node.lines === 1, `${key}: short label was stretched`);
    }
    const minLines = expected.minLines?.[node.text] ?? (node.text === "get_command_or_subagent_output" ? 2 : 1);
    assert.ok(node.lines >= minLines, `${key}: lost wrapping or explicit lines for ${node.text}`);
  }
}
function validateFlowchart(result, expected, key) {
  validateBounds(result, key);
  assert.ok(!result.svg.includes("foreignObject"), `${key}: flowchart regressed to HTML labels`);
  assert.equal(result.edges, expected.edges, `${key}: incorrect flowchart edge count`);
  assert.deepEqual([...result.nodes.map(node => node.text), ...result.edgeLabels].sort(), expected.labels.map(normalized).sort(), `${key}: changed flowchart node or edge labels`);
  if (expected.source.includes("fill: red")) {
    assert.ok(result.nodes.every(node => node.fill === "rgb(255, 0, 0)"), `${key}: custom flowchart themeCSS was ignored`);
  }
}
function geometrySignature(result) {
  return { viewBox: result.viewBox, nodes: result.nodes.map(node => ({ text: node.text, shape: node.shape,
    width: node.cardWidth, height: node.cardHeight, fill: node.fill })), edgeLabels: result.edgeLabels, edges: result.edges };
}
async function expectedFixture(page) {
  return page.evaluate(() => {
    const { kind, source, labels, edges, minLines } = window.__mermaidReview;
    return { kind, source, labels, edges, minLines };
  });
}
async function inspect(page) {
  await page.getByRole("button", { name: "放大预览图表", exact: true }).waitFor();
  return page.evaluate(() => window.__mermaidReview.inspect());
}
async function selectFixture(page, variant) {
  const previous = await page.locator(".md-mermaid-diagram img").getAttribute("src");
  await page.locator("select").selectOption(variant);
  await page.waitForFunction(src => {
    const img = document.querySelector(".md-mermaid-diagram img");
    return img?.src !== src && img?.complete && img.naturalWidth > 0;
  }, previous);
  return inspect(page);
}
async function verifyActions(page, result, key, { download = false, screenshot = false } = {}) {
  const imageUrl = await page.locator(".md-mermaid-diagram img").getAttribute("src");
  await page.getByRole("button", { name: "放大预览图表", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "图表预览" });
  await dialog.waitFor();
  assert.equal(await dialog.locator("img").getAttribute("src"), imageUrl, `${key}: preview uses a different SVG asset`);
  await page.getByRole("button", { name: "原始大小", exact: true }).click();
  assert.ok(Math.abs(await dialog.locator("img").evaluate(img => parseFloat(img.style.width)) - result.viewBox[2]) < 0.1,
    `${key}: original-size preview changed SVG dimensions`);
  if (screenshot) await dialog.screenshot({ path: join(output, `preview-${key}.png`) });
  await page.keyboard.press("Escape");
  assert.equal(await dialog.count(), 0);
  if (download) {
    const downloadEvent = page.waitForEvent("download");
    await page.getByRole("button", { name: "下载", exact: true }).click();
    const asset = await downloadEvent;
    const downloaded = join(output, `download-${key}.svg`); await asset.saveAs(downloaded);
    assert.equal(readFileSync(downloaded, "utf8"), result.svg, `${key}: downloaded SVG differs from the preview asset`);
    await page.evaluate(() => Object.defineProperty(navigator, "clipboard", { configurable: true,
      value: { writeText: async text => { window.__copiedMermaid = text; } } }));
    await page.getByRole("button", { name: "复制", exact: true }).click();
    await page.getByRole("button", { name: "已复制", exact: true }).waitFor();
    assert.equal(await page.evaluate(() => window.__copiedMermaid), await page.evaluate(() => window.__mermaidReview.source), `${key}: copy changed the original source`);
  }
  await page.getByRole("button", { name: "源码", exact: true }).click();
  assert.equal(await page.locator(".md-mermaid-source code").textContent(), await page.evaluate(() => window.__mermaidReview.source), `${key}: source toggle changed the original source`);
  await page.getByRole("button", { name: "图表", exact: true }).click();
  await page.getByRole("button", { name: "放大预览图表", exact: true }).waitFor();
}
function record(result, key, extra = {}) {
  const { svg, ...geometry } = result;
  evidence.push({ key, ...extra, ...geometry });
  writeFileSync(join(output, `${key}.svg`), svg);
}

try {
  const chromium = await reviewChromium();
  const chrome = process.env.CHROME_BIN || (process.platform === "darwin" ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : null);
  browser = await chromium.launch({ headless: true, ...(chrome && existsSync(chrome) ? { executablePath: chrome } : {}) });
  console.log(`Mermaid render evidence: ${output}`);
  for (const theme of ["light", "dark"]) for (const dpr of [1, 1.5, 2]) for (const motion of ["no-preference", "reduce"]) {
    const context = await browser.newContext({ viewport: { width: 1024, height: 900 }, deviceScaleFactor: dpr, reducedMotion: motion });
    const page = await context.newPage();
    page.setDefaultTimeout(60000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    try {
      for (const hostile of [false, true]) {
        const key = `${theme}-${dpr}-${motion}-${hostile ? "hostile" : "app"}`;
        await page.goto(`${base}/__ui-review?theme=${theme}&hostile=${hostile ? 1 : 0}`);
        const result = await inspect(page);
        validateMindmap(result, await expectedFixture(page), key);
        const size = baselineSizes.get(dpr);
        if (size) assert.deepEqual(result.viewBox.slice(2).map(Math.round), size, `${key}: theme/motion changed geometry`);
        else baselineSizes.set(dpr, result.viewBox.slice(2).map(Math.round));
        record(result, key);
        if (dpr === 1.5 && !hostile) await page.locator(".md-mermaid-wrapper").screenshot({ path: join(output, `${key}.png`) });
        await verifyActions(page, result, key, { screenshot: dpr === 1.5 && motion === "reduce" && !hostile });
      }
      assert.deepEqual(errors, [], "Browser render errors");
    } finally { await context.close(); }
  }

  for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width: 800, height: 720 }, deviceScaleFactor: 1.5, reducedMotion: "reduce" });
    page.setDefaultTimeout(60000);
    try {
      for (const variant of ["shapes", "directives", "long-circle", "explicit-breaks", "markdown-multiline"]) {
        const key = `${variant}-${theme}`;
        await page.goto(`${base}/__ui-review?variant=${variant}&theme=${theme}&hostile=1`);
        const result = await inspect(page);
        writeFileSync(join(output, `${key}-geometry.json`), JSON.stringify(result, null, 2));
        validateMindmap(result, await expectedFixture(page), key);
        record(result, key);
        await page.locator(".md-mermaid-wrapper").screenshot({ path: join(output, `${key}.png`) });
        await verifyActions(page, result, key, { download: true, screenshot: true });
      }
      const flowchart = await selectFixture(page, "flowchart");
      validateFlowchart(flowchart, await expectedFixture(page), `flowchart-after-mindmap-${theme}`);
      record(flowchart, `flowchart-after-mindmap-${theme}`);
      await page.locator("select").selectOption("invalid");
      await page.getByRole("alert").waitFor();
      assert.ok(await page.locator(".md-mermaid-source").isVisible(), "Render error lost source fallback");
      await page.locator("select").selectOption("tools");
      const recovered = await inspect(page);
      validateMindmap(recovered, await expectedFixture(page), `failure-recovery-${theme}`);
      record(recovered, `failure-recovery-${theme}`);
      // Begin on a fresh document: custom style must work both before and
      // after another diagram kind installs its protected configuration.
      await page.goto(`${base}/__ui-review?variant=flowchart-custom&theme=${theme}&hostile=1`);
      const before = await inspect(page);
      validateFlowchart(before, await expectedFixture(page), `flowchart-custom-before-${theme}`);
      const mindmap = await selectFixture(page, "directives");
      validateMindmap(mindmap, await expectedFixture(page), `isolation-mindmap-${theme}`);
      const after = await selectFixture(page, "flowchart-custom");
      validateFlowchart(after, await expectedFixture(page), `flowchart-custom-after-${theme}`);
      assert.deepEqual(geometrySignature(after), geometrySignature(before), `${theme}: mindmap leaked layout/themeCSS into the next flowchart`);
      record(after, `flowchart-custom-after-${theme}`);
      await verifyActions(page, after, `flowchart-custom-${theme}`, { download: true });
    } finally { await page.close(); }
  }
  // Release the completed browser and its GPU/background processes before
  // exercising the native WebView in this same desktop session.
  await browser.close();
  browser = undefined;
  console.log("Chromium diagram scenarios passed.");
  if (process.platform === "darwin" && process.env.MERMAID_WEBKIT !== "0") {
    const script = fileURLToPath(new URL("./validate-mermaid-webkit.swift", import.meta.url));
    const { stdout } = await promisify(execFile)("/usr/bin/swift", ["-module-cache-path", join(output, "swift-modules"), script, base, output],
      // A cold AppKit/WebKit module cache can take several minutes to compile
      // on a busy machine. The Swift runner separately limits rendering time.
      { timeout: 360000, maxBuffer: 1024 * 1024 });
    console.log(stdout.trim());
    for (const entry of JSON.parse(readFileSync(join(output, "webkit-geometry.json"), "utf8"))) {
      if (entry.expected.kind === "flowchart") validateFlowchart(entry.result, entry.expected, entry.key);
      else validateMindmap(entry.result, entry.expected, entry.key);
      if (entry.baseline) {
        validateFlowchart(entry.baseline, entry.expected, `${entry.key}-before`);
        validateMindmap(entry.between, entry.betweenExpected, `${entry.key}-mindmap`);
        assert.deepEqual(geometrySignature(entry.result), geometrySignature(entry.baseline), `${entry.key}: WKWebView configuration leaked between kinds`);
      }
      record(entry.result, entry.key, { case: entry.case });
    }
  }
  writeFileSync(join(output, "geometry.json"), JSON.stringify(evidence, null, 2));
  console.log(`Passed ${evidence.length} diagram configurations, actual text/card bounds, tree connections, multiline labels, preview, copy, download, configuration isolation and failure recovery.`);
} catch (error) {
  writeFileSync(join(output, "geometry-failure.json"), JSON.stringify(evidence, null, 2));
  throw error;
} finally {
  await browser?.close();
  await server.close();
}
