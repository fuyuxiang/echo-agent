import { join } from "node:path";
import { writeFileSync } from "node:fs";
import assert from "node:assert/strict";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

/** Browser verification of production components with isolated result data. */
export async function validateResultReview(page, output, baseline = false) {
  const evidence = [];
  const narrowEvidence = [];
  const origin = new URL(page.url()).origin;
  const base = origin === "null" ? "http://127.0.0.1:1439" : origin;
  const variants = ["short", "markdown", "code", "sheet", "empty", "error", "image", "diagram"];
  for (const [width, height] of [[1024, 680], [1280, 800], [1920, 1080]]) {
    for (const theme of ["light", "dark"]) {
      await page.setViewportSize({ width, height });
      for (const variant of variants) {
        await page.goto(`${base}/__ui-review?surface=results&theme=${theme}&resultVariant=${variant}`);
        await page.locator(".file-preview").waitFor();
        await page.addStyleTag({ content: "*,*::before,*::after{animation:none!important;transition:none!important}" });
        if (variant === "image") await page.locator(".file-preview__img").evaluate((image) => image.complete || new Promise((resolve) => image.addEventListener("load", resolve, { once: true })));
        if (variant === "error" && !baseline) {
          try {
            await page.getByRole("alert").waitFor({ timeout: 8000 });
          } catch (error) {
            const actual = await page.evaluate(() => ({
              url: location.href,
              text: document.querySelector(".file-preview")?.textContent,
              previewClass: document.querySelector(".file-preview")?.className,
              image: [...document.querySelectorAll(".file-preview img")].map((image) => ({ src: image.getAttribute("src"), complete: image.complete, naturalWidth: image.naturalWidth })),
            }));
            const failureName = `results-error-failure-${width}-${theme}`;
            await page.screenshot({ path: join(output, `${failureName}.png`) });
            writeFileSync(join(output, `${failureName}.json`), JSON.stringify(actual, null, 2));
            throw new Error(`Result image error did not render a visible alert: ${JSON.stringify(actual)}`, { cause: error });
          }
        }
        if (variant === "diagram") await page.getByRole("button", { name: "放大预览图表" }).waitFor();
        const geometry = await page.evaluate(() => {
          const body = document.querySelector(".file-preview__body");
          const card = document.querySelector(".file-preview");
          const box = card.getBoundingClientRect();
          return {
            overflow: document.documentElement.scrollWidth > innerWidth + 1,
            card: { x: box.x, width: box.width, right: box.right },
            ...(body ? { font: getComputedStyle(body).fontFamily, fontSize: getComputedStyle(body).fontSize, whiteSpace: getComputedStyle(body).whiteSpace } : {}),
            nestedVerticalScrollers: [...document.querySelectorAll(".tool-side-panel__body, .file-preview__body, .file-preview__code, .file-preview__doc-body")].filter((element) => element.scrollHeight > element.clientHeight + 1 && ["auto", "scroll"].includes(getComputedStyle(element).overflowY)).length,
          };
        });
        if (!baseline) {
          assert.ok(!geometry.overflow, `results ${variant} ${width} ${theme}: horizontal page overflow`);
          assert.ok(geometry.card.x >= 0 && geometry.card.right <= width + 1, `results ${variant}: file card exceeds window`);
          assert.ok(geometry.nestedVerticalScrollers <= 1, `results ${variant}: nested vertical scrolling`);
          if (["short", "markdown"].includes(variant)) {
            assert.equal(geometry.whiteSpace, "normal", "Markdown preview inherits preformatted whitespace");
            assert.ok(!/mono|Menlo|Consolas/i.test(geometry.font), "Markdown preview inherits a code font");
            assert.ok(parseFloat(geometry.fontSize) >= 13, "Markdown text is too small");
          }
          if (variant === "empty") assert.ok(await page.getByText("未提取到可阅读的文本").isVisible());
          if (variant === "error") assert.ok(await page.getByRole("alert").isVisible());
        }
        const filename = `results-${variant}-${width}-${theme}.png`;
        await page.screenshot({ path: join(output, filename) });
        evidence.push({ variant, width, height, theme, filename, ...geometry });
        if (variant === "image") {
          await page.locator(".file-preview__img").click();
          if (!baseline) {
            const dialog = page.getByRole("dialog");
            await dialog.waitFor();
            const box = await dialog.boundingBox();
            assert.ok(box.x >= 0 && box.y >= 0 && box.x + box.width <= width + 1 && box.y + box.height <= height + 1, "image preview dialog exceeds window");
            await page.getByRole("button", { name: "原始大小" }).click();
            assert.ok(await page.getByRole("button", { name: "关闭图片预览" }).isVisible());
          }
          await page.screenshot({ path: join(output, `results-image-preview-${width}-${theme}.png`) });
          if (!baseline) {
            await page.keyboard.press("Escape");
            assert.equal(await page.getByRole("dialog").count(), 0, "image preview Escape did not close");
            assert.ok(await page.getByRole("button", { name: /放大预览/ }).evaluate((element) => element === document.activeElement), "image preview did not restore focus");
          }
        }
        if (variant === "code") {
          await page.getByRole("checkbox", { name: "模拟复制失败" }).check();
          await page.getByRole("button", { name: "复制内容" }).click();
          if (!baseline) {
            await page.getByRole("alert").waitFor();
            assert.ok(!(await page.getByText("已复制", { exact: true }).count()), "failed copy displays success");
          }
          await page.screenshot({ path: join(output, `results-copy-failure-${width}-${theme}.png`) });
        }
      }
      if (!baseline) {
        await page.goto(`${base}/__ui-review?surface=results&theme=${theme}`);
        const first = page.getByRole("tab").first();
        await first.focus();
        await page.keyboard.press("ArrowRight");
        assert.ok(await page.getByRole("tab").nth(1).evaluate((element) => element === document.activeElement && element.getAttribute("aria-selected") === "true"));
        await page.keyboard.press("End");
        assert.ok(await page.getByRole("tab").last().evaluate((element) => element === document.activeElement));
        const count = await page.getByRole("tab").count();
        await page.keyboard.press("Delete");
        assert.equal(await page.getByRole("tab").count(), count - 1);
        assert.ok(await page.getByRole("tab").last().evaluate((element) => element === document.activeElement), "closing a tab loses keyboard focus");
      }
    }
  }
  // ToolSidePanel permits a 280px minimum width. Verify the real preview/header
  // components in that narrow host and at 360px, including the 22px root scale.
  for (const panelWidth of [280, 360]) for (const theme of ["light", "dark"]) for (const variant of ["markdown", "diagram"]) {
    await page.setViewportSize({ width: 1024, height: 680 });
    await page.goto(`${base}/__ui-review?surface=results&theme=${theme}&resultVariant=${variant}`);
    await page.locator(".file-preview").waitFor();
    if (variant === "diagram") await page.getByRole("button", { name: "放大预览图表" }).waitFor();
    await page.evaluate((width) => {
      document.documentElement.style.fontSize = "22px";
      document.querySelector(".result-review-fixture").style.width = `${width}px`;
    }, panelWidth);
    await page.addStyleTag({ content: "*,*::before,*::after{animation:none!important;transition:none!important}" });
    const header = page.locator(".md-code-header").first();
    await header.scrollIntoViewIfNeeded();
    const geometry = await page.evaluate(() => {
      const rect = (element) => { const { x, y, width, height, right, bottom } = element.getBoundingClientRect(); return { x, y, width, height, right, bottom }; };
      const host = document.querySelector(".result-review-fixture");
      const header = document.querySelector(".md-code-header");
      const buttons = [...header.querySelectorAll("button")].map((element) => {
        const box = element.getBoundingClientRect();
        const hit = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2);
        return { ...rect(element), label: element.getAttribute("aria-label") || element.textContent, receivesPointer: hit === element || element.contains(hit) };
      });
      return {
        host: rect(host), header: rect(header), card: rect(document.querySelector(".file-preview")), buttons,
        language: rect(header.querySelector(".md-code-lang")),
        overflow: document.documentElement.scrollWidth > innerWidth + 1,
        hostOverflow: host.scrollWidth > host.clientWidth + 1,
        tableWrappers: [...document.querySelectorAll(".md-table-wrapper")].map((element) => ({ ...rect(element), scrollWidth: element.scrollWidth, clientWidth: element.clientWidth })),
      };
    });
    const filename = `results-narrow-${variant}-${panelWidth}-font22-${theme}.png`;
    await page.screenshot({ path: join(output, filename) });
    narrowEvidence.push({ panelWidth, theme, variant, filename, ...geometry });
    if (!baseline) {
      assert.ok(!geometry.overflow && !geometry.hostOverflow, `narrow ${variant} ${panelWidth}: title/table expands preview host`);
      assert.ok(geometry.card.right <= panelWidth + 1, `narrow ${variant}: file card exceeds panel`);
      assert.ok(geometry.language.width >= 65, `narrow ${variant}: code language is squeezed to an unreadable sliver`);
      assert.ok(geometry.tableWrappers.every((table) => table.right <= panelWidth + 1), `narrow ${variant}: table scroll wrapper exceeds panel`);
      assert.ok(geometry.buttons.every((button) => button.x >= geometry.header.x - 1 && button.right <= geometry.header.right + 1 && button.y >= geometry.header.y - 1 && button.bottom <= geometry.header.bottom + 1 && button.y >= 0 && button.bottom <= 681 && button.receivesPointer), `narrow ${variant} ${panelWidth}: code action is clipped or covered`);
      const actions = header.getByRole("button");
      for (let index = 0; index < await actions.count(); index += 1) await actions.nth(index).click({ trial: true });
      if (variant === "markdown") {
        await header.getByRole("button", { name: "复制", exact: true }).click();
        await header.getByRole("button", { name: /^(已复制|复制失败，点击重试)$/ }).waitFor();
      } else {
        await header.getByRole("button", { name: "源码", exact: true }).click();
        assert.ok(await page.locator(".md-mermaid-wrapper .md-code-pre").isVisible(), "narrow Mermaid source toggle did not show code");
        await header.getByRole("button", { name: "图表", exact: true }).click();
        await header.getByRole("button", { name: "放大", exact: true }).click();
        await page.getByRole("dialog").waitFor();
        await page.keyboard.press("Escape");
        assert.equal(await page.getByRole("dialog").count(), 0, "narrow Mermaid preview did not close");
      }
    }
  }
  const screenshots = evidence.length + 12 + narrowEvidence.length;
  writeFileSync(join(output, "result-review.json"), JSON.stringify({ baseline, screenshots, checks: baseline ? "baseline capture only" : ["responsive card layout", "Markdown typography", "one vertical scroll owner", "empty/error states", "copy failure", "image sizing/zoom/Escape/focus", "keyboard tabs", "280/360px result host at 22px font", "code actions pointer visibility", "Mermaid source and preview controls"], evidence, narrowEvidence }, null, 2));
  return { baseline, screenshots, layouts: evidence.length + narrowEvidence.length };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const { createUiReviewServer, reviewChromium, reviewOutput } = await import("./ui-review-runtime.mjs");
  const output = reviewOutput("/private/tmp/echo-result-review");
  const server = await createUiReviewServer();
  const browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  try {
    const page = await browser.newPage(); page.setDefaultNavigationTimeout(60000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    const results = await validateResultReview(page, output);
    assert.deepEqual(errors, [], "result browser runtime errors");
    console.log(JSON.stringify({ passed: true, results, errors }));
  } finally { await browser.close(); await server.close(); }
}
