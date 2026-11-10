import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium } from "./ui-review-runtime.mjs";

// Isolated native fixtures: no credentials, paid requests or settings writes.
const output = mkdtempSync(join(tmpdir(), "echo-model-connections-"));
const server = await createUiReviewServer(0);
const evidence = [];
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const [width, height, font] of [[1280, 850, 16], [1024, 680, 22], [390, 844, 22]]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width, height } });
    page.setDefaultNavigationTimeout(60000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.addInitScript(font => {
      localStorage.setItem("echoagent.fontSize", String(font));
      window.scrolledModelIds = [];
      const scrollIntoView = Element.prototype.scrollIntoView;
      Element.prototype.scrollIntoView = function (options) {
        if (this.dataset?.modelId) window.scrolledModelIds.push(this.dataset.modelId);
        return scrollIntoView.call(this, options);
      };
    }, font);
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=model&theme=${theme}&modelTesting=1&initialModelId=test-personal/b`);
    const detail = page.getByRole("region", { name: "我的网关连接详情" });
    await detail.waitFor();
    await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
    const row = detail.locator('[data-model-id="test-personal/b"]');
    assert.ok(await page.evaluate(() => window.scrolledModelIds.includes("test-personal/b")));
    assert.deepEqual(await row.evaluate(element => ({
      focused: document.activeElement === element,
      outline: getComputedStyle(element).outlineStyle,
      outlineWidth: getComputedStyle(element).outlineWidth,
      sameBorder: getComputedStyle(element).borderColor === getComputedStyle(element.previousElementSibling).borderColor,
    })), { focused: true, outline: "solid", outlineWidth: "2px", sameBorder: true });
    const test = detail.getByRole("button", { name: "测试模型 备用模型", exact: true });
    await page.keyboard.press("Tab");
    assert.equal(await test.evaluate(element => element.matches(":focus-visible")), true);
    await test.click();
    assert.equal(await test.isDisabled(), true);
    assert.equal(await detail.getByRole("button", { name: "测试模型 个人模型", exact: true }).isEnabled(), true);
    await row.getByRole("status").waitFor();
    assert.match(await row.getByRole("status").textContent(), /上次测试失败.*429/s);
    await row.scrollIntoViewIfNeeded();
    await page.screenshot({ path: join(output, `failure-${width}-${theme}.png`) });
    await test.click();
    await row.getByRole("status").waitFor();
    const success = await row.getByRole("status").textContent();
    assert.match(success, /测试通过 · 耗时 [0-9]+\.[0-9] 秒/);
    assert.ok(Number(success.match(/耗时 ([0-9.]+) 秒/)[1]) >= 1.1);
    assert.doesNotMatch(await row.textContent(), /429/);
    for (const [connection, name, providerId, modelId] of [["研发团队", "团队模型", "test-org", "team-a"]]) {
      await page.getByRole("button",{name:"返回连接管理",exact:true}).click();
      await page.getByRole("group",{name:"模型连接",exact:true}).getByRole("button").filter({hasText:connection}).click();
      await page.getByRole("button", { name: `测试模型 ${name}`, exact: true }).click();
      await page.getByRole("status").filter({ hasText: "测试通过" }).waitFor();
      assert.equal(await page.getByRole("button", { name: "编辑连接", exact: true }).count(), 0);
      assert.ok(await page.evaluate(({providerId,modelId}) => window.modelTestCalls.some(call => call.providerId === providerId && call.modelId === modelId), {providerId,modelId}));
    }
    await page.getByRole("button",{name:"返回连接管理",exact:true}).click();
    await page.getByRole("group",{name:"模型连接",exact:true}).getByRole("button").filter({hasText:"我的网关"}).click();
    assert.match(await row.getByRole("status").textContent(), /测试通过/);
    const layout = await page.locator(".model-connections").evaluate(root => ({
      fits: root.scrollWidth <= root.clientWidth + 1,
      pageFits: document.documentElement.scrollWidth <= innerWidth + 1,
      actionsFit: [...root.querySelectorAll("button")].every(el => el.scrollWidth <= el.clientWidth + 1),
    }));
    assert.ok(layout.fits && layout.pageFits && layout.actionsFit, JSON.stringify({ width, theme, layout }));
    assert.equal(await page.evaluate(() => window.modelTestCalls.filter(call => call.modelId === "personal-b").length), 2);
    await row.scrollIntoViewIfNeeded();
    await page.screenshot({ path: join(output, `success-${width}-${theme}.png`) });
    assert.deepEqual(errors, []);
    evidence.push({ width, height, theme, font, layout });
    await page.close();
  }
  writeFileSync(join(output, "evidence.json"), JSON.stringify(evidence, null, 2));
  console.log(`Model connection tests passed: ${evidence.length} layouts, exact per-model routing, pending, failure/retry, personal and organization sources and management targeting. Screenshots: ${output}`);
} finally { await browser?.close(); await server.close(); }
