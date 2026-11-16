import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium } from "./ui-review-runtime.mjs";

const output = mkdtempSync(join(tmpdir(), "echo-team-project-ui-"));
const server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/team-project-review.tsx" });
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const width of [420, 1280]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width, height: 1000 } });
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?theme=${theme}`);
    await page.getByRole("button", { name: "新成员快速了解项目" }).click();
    await page.getByLabel("项目问答结果").waitFor();
    await page.getByRole("button", { name: "cit-1 · 接口联调工作项" }).click();
    await page.getByLabel("项目来源原文").waitFor();
    await page.getByRole("button", { name: "关闭来源" }).click();
    await page.locator(".team-projects--detail").evaluate(node => { node.scrollTop = 0; });
    await page.screenshot({ path: join(output, `${width}-${theme}-overview.png`), fullPage: true });
    for (const tab of ["工作项", "执行记录", "资料与成果"]) {
      await page.getByRole("navigation", { name: "团队项目页面" }).getByRole("button", { name: tab }).click();
      await page.screenshot({ path: join(output, `${width}-${theme}-${tab}.png`), fullPage: true });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false, `${width} ${theme} ${tab}: horizontal overflow`);
    }
    assert.match(await page.getByText(/上一有效版本仍可用于问答/).textContent(), /正在处理/);
    await page.getByRole("navigation", { name: "团队项目页面" }).getByRole("button", { name: "执行记录" }).click();
    assert.match(await page.getByLabel("项目执行记录").textContent(), /已验证超时/);
    await page.getByRole("button", { name: "打开本机执行" }).click();
    assert.deepEqual(errors, []);
    await page.close();
  }
  console.log(`Team project UI verified in light/dark at 420/1280px: ${output}`);
} finally { await browser?.close(); await server.close(); }
