// Monaco paints line decorations in sibling layers, outside a token's DOM
// ancestor chain. Sample the actual background with glyph ink temporarily
// hidden; computed parent backgrounds alone are insufficient for diff text.
import assert from "node:assert/strict";
import { reviewPng } from "./ui-review-tools.mjs";
const PNG = reviewPng();
const luminance = rgb => rgb.map(channel => channel / 255).map(value => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4).reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
export async function inspectEditorPaint(page, target, path, { diffHighlights = false } = {}) {
  if (diffHighlights) {
    // Text tokenization can finish before the diff worker. Never accept a
    // plain editor screenshot as evidence that diff decorations are readable.
    await target.waitForFunction(() => ["line-insert", "line-delete", "char-insert", "char-delete"].every(name =>
      [...document.querySelectorAll(`.monaco-diff-editor .${name}`)].some(element => element.getBoundingClientRect().width > 2)));
  }
  const ranges = diffHighlights ? await target.evaluate(() => {
    const result = [];
    for (const kind of ["insert", "delete"]) {
      const lines = [...document.querySelectorAll(`.monaco-diff-editor .line-${kind}`)].map(element => element.getBoundingClientRect());
      for (const element of document.querySelectorAll(`.monaco-diff-editor .char-${kind}`)) {
        const rect = element.getBoundingClientRect();
        if (rect.width < 5 || rect.height < 5 || rect.left < 0 || rect.right > innerWidth || rect.top < 0 || rect.bottom > innerHeight) continue;
        const line = lines.find(line => Math.abs(line.top - rect.top) < 1 && line.width > rect.width + 12 && line.left <= rect.left && line.right >= rect.right);
        if (!line) continue;
        const lineX = rect.left - line.left > 6 ? rect.left - 4 : rect.right + 4;
        const style = getComputedStyle(element);
        result.push({ kind, character: { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 },
          line: { x: lineX, y: rect.top + rect.height / 2 }, border: style.borderBottomColor,
          bordered: parseFloat(style.borderBottomWidth) >= 1 && style.borderBottomStyle !== "none" });
        break;
      }
    }
    return result;
  }) : [];
  if (diffHighlights) assert.equal(ranges.length, 2, "Both inserted and removed character ranges must be sampled inside changed lines");
  const geometry = await target.evaluate(() => {
    const tokens = [...document.querySelectorAll(".monaco-editor .view-lines span[class*=mtk]")].filter(element => !element.children.length && element.textContent.trim());
    return tokens.flatMap(element => {
      const rect = element.getBoundingClientRect(), style = getComputedStyle(element);
      if (!rect.width || !rect.height || style.visibility !== "visible") return [];
      let left = Math.max(0, rect.left), right = Math.min(innerWidth, rect.right);
      let top = Math.max(0, rect.top + rect.height * .25), bottom = Math.min(innerHeight, rect.bottom - rect.height * .25), opacity = 1;
      for (let parent = element; parent; parent = parent.parentElement) {
        const css = getComputedStyle(parent), box = parent.getBoundingClientRect();
        opacity *= Number(css.opacity);
        if (/hidden|auto|scroll|clip/.test(css.overflowX)) { left = Math.max(left, box.left); right = Math.min(right, box.right); }
        if (/hidden|auto|scroll|clip/.test(css.overflowY)) { top = Math.max(top, box.top); bottom = Math.min(bottom, box.bottom); }
      }
      if (right <= left || bottom <= top || !opacity) return [];
      return [{ text: element.textContent, color: style.color, opacity, rect: { left, right, top, bottom }, threshold: 4.5 }];
    });
  });
  assert(geometry.length >= 8, "Real syntax tokens must be painted and sampled");
  assert(geometry.some(token => token.text.includes("保留")), "The real code comment must be sampled");
  let offset = { x: 0, y: 0 };
  if (target !== page) {
    const element = await target.frameElement(), box = await element.boundingBox();
    const border = await element.evaluate(element => ({ x: element.clientLeft, y: element.clientTop }));
    assert(box, "The real editor frame must be visible");
    offset = { x: box.x + border.x, y: box.y + border.y };
  }
  const hostWidth = await page.evaluate(() => innerWidth);
  const stable = await target.addStyleTag({ content: ".monaco-editor,.monaco-editor *{animation:none!important;transition:none!important}.monaco-editor .cursors-layer{visibility:hidden!important}" });
  let png, ink, hidden;
  try {
    ink = PNG.sync.read(await page.screenshot());
    hidden = await target.addStyleTag({ content: ".monaco-editor .view-lines span{-webkit-text-fill-color:transparent!important;text-shadow:none!important}" });
    png = PNG.sync.read(await page.screenshot({ path }));
  } finally {
    await hidden?.evaluate(element => element.remove());
    await stable.evaluate(element => element.remove());
  }
  const scale = png.width / hostWidth;
  const samples = geometry.map(token => {
    assert.equal(token.opacity, 1, "Partial group opacity needs an explicit compositing case");
    assert.match(token.color, /^rgb/);
    const channels = token.color.match(/[\d.]+/g).map(Number), alpha = channels[3] ?? 1;
    let minimum = Infinity, backdrop, pixels = 0;
    for (let y = Math.max(0, Math.ceil((token.rect.top + offset.y) * scale)); y < Math.min(png.height, Math.floor((token.rect.bottom + offset.y) * scale)); y++) {
      for (let x = Math.max(0, Math.ceil((token.rect.left + offset.x) * scale)); x < Math.min(png.width, Math.floor((token.rect.right + offset.x) * scale)); x++) {
        const i = (y * png.width + x) * 4, bg = [...png.data.subarray(i, i + 3)];
        // Borders and guides can touch a token's rectangle without lying
        // behind a glyph. The real ink difference locates painted text only.
        if (bg.reduce((difference, channel, index) => difference + Math.abs(channel - ink.data[i + index]), 0) < 5) continue;
        const foreground = channels.slice(0, 3).map((channel, index) => channel * alpha + bg[index] * (1 - alpha));
        const [light, dark] = [luminance(foreground), luminance(bg)].sort((a, b) => b - a);
        const ratio = (light + .05) / (dark + .05); pixels++;
        if (ratio < minimum) { minimum = ratio; backdrop = bg; }
      }
    }
    return { ...token, minimum, backdrop, pixels };
  }).filter(token => token.pixels);
  assert(samples.length >= 8, "Clipping must not eliminate all syntax samples");
  const at = point => {
    const x = Math.floor((point.x + offset.x) * scale), y = Math.floor((point.y + offset.y) * scale);
    assert(x >= 0 && x < png.width && y >= 0 && y < png.height, "The changed range must be inside the screenshot");
    return [...png.data.subarray((y * png.width + x) * 4, (y * png.width + x) * 4 + 3)];
  };
  const highlights = ranges.map(range => {
    const lineBackground = at(range.line), characterBackground = at(range.character);
    const channels = range.border.match(/[\d.]+/g).map(Number), alpha = channels[3] ?? 1;
    const border = channels.slice(0, 3).map((channel, index) => channel * alpha + characterBackground[index] * (1 - alpha));
    const [light, dark] = [luminance(border), luminance(characterBackground)].sort((a, b) => b - a);
    const borderContrast = (light + .05) / (dark + .05);
    const tintDifference = Math.max(...lineBackground.map((channel, index) => Math.abs(channel - characterBackground[index])));
    return { kind: range.kind, lineBackground, characterBackground, tintDifference, bordered: range.bordered, borderContrast };
  });
  return { samples, failures: samples.filter(token => token.minimum < token.threshold),
    pixels: samples.reduce((sum, token) => sum + token.pixels, 0), highlights,
    highlightFailures: highlights.filter(sample => sample.tintDifference < 8 || !sample.bordered || sample.borderContrast < 3) };
}
