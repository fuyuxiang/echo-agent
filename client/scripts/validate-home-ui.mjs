import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-home-review-")));
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
const initialHint = "从一个问题、一个想法，或一份文件开始";
const evidence = [];
let browser;
try {
  browser = await (await reviewChromium()).launch({
    headless: true,
    executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  });
  for (const [width, height, theme, font] of [
    [1440, 900, "light", 13],
    [1280, 800, "dark", 13],
    [1024, 680, "light", 13],
    [1024, 680, "dark", 18],
    [768, 720, "light", 18],
    [360, 640, "dark", 13],
  ]) {
    const page = await browser.newPage({ viewport: { width, height } });
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    page.setDefaultNavigationTimeout(60000);
    const url = `${base}?${new URLSearchParams({ surface: "home", platform: "mac", theme, font: String(font) })}`;
    await page.goto(url, { waitUntil: "networkidle" });
    const input = page.getByRole("textbox", { name: "输入问题或任务", exact: true });
    await input.waitFor();
    const applyFont = () => page.evaluate(font => {
      document.documentElement.style.fontSize = `${16 * font / 13}px`;
    }, font);
    await applyFont();
    if (width < 1024) await page.getByRole("button", { name: "收起侧边栏", exact: true }).click();

    const inspect = async state => {
      const geometry = await page.evaluate(() => {
        const title = document.querySelector(".home__title");
        const composer = document.querySelector(".home .echo-composer");
        const home = document.querySelector(".home");
        const rect = element => {
          const { x, y, width, height, bottom, right } = element.getBoundingClientRect();
          return { x, y, width, height, bottom, right };
        };
        return {
          title: rect(title), composer: rect(composer), home: rect(home),
          titleAlign: getComputedStyle(title).textAlign,
          inputAlign: getComputedStyle(composer.querySelector("textarea")).textAlign,
          descriptionCount: document.querySelectorAll(".home__description").length,
          rootFontSize: parseFloat(getComputedStyle(document.documentElement).fontSize),
          overflow: document.documentElement.scrollWidth > innerWidth + 1 || home.scrollWidth > home.clientWidth + 1,
        };
      });
      assert.equal(geometry.titleAlign, "center");
      assert.ok(Math.abs(geometry.rootFontSize - 16 * font / 13) < 0.01, "Font preference must actually be applied");
      assert.equal(geometry.descriptionCount, 0);
      assert.equal(geometry.overflow, false, `${width}/${theme}/${state}: horizontal overflow`);
      assert.ok(["start", "left"].includes(geometry.inputAlign));
      assert.ok(Math.abs(geometry.title.x + geometry.title.width / 2 - geometry.composer.x - geometry.composer.width / 2) <= 1,
        `${width}/${theme}/${state}: title and composer must share a center line`);
      assert.ok(geometry.title.y >= geometry.home.y - 1, `${state}: title must remain reachable`);
      assert.ok(geometry.composer.x >= geometry.home.x && geometry.composer.right <= geometry.home.right + 1);
      const gap = geometry.composer.y - geometry.title.bottom;
      assert.ok(gap >= 20 && gap <= 40, `${state}: unexpected title spacing ${gap}`);
      await page.screenshot({ path: join(output, `home-${width}-${theme}-${font}-${state}.png`) });
      evidence.push({ width, height, theme, font, state, geometry });
    };

    assert.equal(await input.getAttribute("placeholder"), initialHint);
    assert.equal(await input.inputValue(), "");
    assert.equal(await page.getByRole("button", { name: "发送", exact: true }).isEnabled(), false);
    await inspect("empty");

    // Replace only file-selection IPC; the actual HomePage, admission logic,
    // attachment chips, draft storage and Composer all remain in use.
    await page.evaluate(() => {
      const original = window.__TAURI_INTERNALS__.invoke;
      let selection = 0;
      window.__TAURI_INTERNALS__.invoke = async (command, args = {}) => {
        if (command === "filesystem_pick_files") return selection++ === 0
          ? ["/review/项目资料.txt"]
          : ["/review/项目资料.txt", "/review/会议纪要.md"];
        if (command === "filesystem_attachment_stats") return {
          files: args.paths.map(path => ({ inputPath: path, path, sizeBytes: 32 })), rejected: [],
        };
        return original(command, args);
      };
    });
    const addFile = async hint => {
      await page.getByRole("button", { name: "添加", exact: true }).click();
      await page.getByRole("menuitem", { name: "添加文件", exact: true }).click();
      await page.waitForFunction(hint => document.querySelector(".echo-composer__input").placeholder === hint, hint);
    };
    await addFile("想如何处理这份文件？");
    await inspect("one-file");
    await input.fill("整理成项目要点，保留我的原始要求。");
    await addFile("想基于这些文件完成什么？");
    assert.equal(await page.getByRole("button", { name: "移除附件", exact: true }).count(), 2);
    assert.equal(await input.inputValue(), "整理成项目要点，保留我的原始要求。");
    await inspect("multiple-files");

    // Reloading restores both text and admitted files, without writing hint
    // text into the draft. Removal reverses the hint and returns input focus.
    await page.reload({ waitUntil: "networkidle" });
    await applyFont();
    assert.equal(await input.inputValue(), "整理成项目要点，保留我的原始要求。");
    assert.equal(await input.getAttribute("placeholder"), "想基于这些文件完成什么？");
    await page.getByRole("button", { name: "移除附件", exact: true }).last().click();
    assert.equal(await input.getAttribute("placeholder"), "想如何处理这份文件？");
    await page.getByRole("button", { name: "移除附件", exact: true }).click();
    assert.equal(await input.getAttribute("placeholder"), initialHint);
    assert.ok(await input.evaluate(element => element === document.activeElement));
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await page.waitForFunction(() => document.querySelector(".echo-composer__input").value === "");
    assert.equal(await input.getAttribute("placeholder"), initialHint);
    assert.equal(await page.getByRole("button", { name: "发送", exact: true }).isEnabled(), false);
    assert.deepEqual(errors, []);
    await page.close();
  }
  writeFileSync(join(output, "review.json"), JSON.stringify(evidence, null, 2));
  console.log(`Passed ${evidence.length} home layouts, attachment hints, duplicate filtering, draft restoration, removal/focus and submission. Evidence: ${output}`);
} finally {
  await browser?.close();
  await server.close();
}
