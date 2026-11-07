import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-help-settings-")));
const server = await createUiReviewServer(0);
const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
const layouts = [];
let browser;

async function showUpdate(page, state) {
  // Only the isolated UI fixture is running here; no native update is executed.
  await page.evaluate(async state => {
    const { useUpdateStore } = await import("/src/stores/update-store.ts");
    useUpdateStore.setState({ update: undefined, downloaded: 0, total: undefined,
      checkedAt: new Date().toISOString(), error: undefined, errorStage: undefined, ...state });
  }, state);
}

async function checkLayout(page, name) {
  for (const selector of [".settings-modal__panel", ".help-settings"]) {
    assert.equal(await page.locator(selector).evaluate(element => element.scrollWidth > element.clientWidth + 1), false, `${name}: ${selector} overflow`);
  }
  const buttons = await page.locator(".help-settings button").evaluateAll(elements => elements.map(element => ({
    name: element.textContent.trim(), clipped: element.scrollWidth > element.clientWidth + 2,
  })));
  assert(buttons.every(button => !button.clipped), `${name}: clipped action ${JSON.stringify(buttons)}`);
  await page.screenshot({ path: join(output, `${name}.png`) });
  layouts.push(name);
}

try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const [width, height, theme, font] of [[1440, 1000, "light", 13], [1024, 768, "dark", 13], [768, 900, "light", 18], [390, 844, "dark", 18]]) {
    const page = await browser.newPage({ viewport: { width, height } });
    page.setDefaultTimeout(15000);
    page.setDefaultNavigationTimeout(60000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.addInitScript(font => {
      localStorage.setItem("echoagent.fontSize", String(font));
      const observer = new MutationObserver(() => {
        if (document.documentElement) { document.documentElement.style.fontSize = `${16 * font / 13}px`; observer.disconnect(); }
      });
      observer.observe(document, { childList: true, subtree: true });
    }, font);
    await page.goto(`${origin}/__ui-review?surface=help&theme=${theme}`, { waitUntil: "networkidle" });
    await showUpdate(page, { status: "up-to-date" });
    await page.getByRole("status").filter({ hasText: "当前已是最新版本" }).waitFor();
    assert.equal(await page.locator(".help-card").count(), 0);
    assert.equal(await page.locator(".help-settings__resource").count(), 2);
    assert.equal(await page.getByText(/VPN/).count(), 0);
    assert.equal(await page.locator('[data-setting-id="help-faq"]').evaluate(element => element.open), false);
    assert.equal(await page.getByRole("button", { name: "检查更新", exact: true }).locator(".lucide-refresh-cw").count(), 1);
    assert(await page.locator(".help-settings").evaluate(element => element.getBoundingClientRect().width <= 841));
    await checkLayout(page, `overview-${width}-${theme}`);

    const search = page.getByRole("combobox", { name: "搜索设置" });
    await search.fill("常见问题");
    await search.press("Enter");
    await page.getByText("确认模型配置", { exact: true }).waitFor();
    assert(await page.locator('[data-setting-id="help-faq"]').evaluate(element => element.open));
    assert.equal(await page.getByRole("link", { name: /ACP 协议规范/ }).count(), 0);
    const technical = page.locator(".help-settings__technical > summary");
    await technical.focus();
    await page.keyboard.press("Enter");
    await page.getByRole("link", { name: /ACP 协议规范/ }).waitFor();
    await checkLayout(page, `troubleshooting-${width}-${theme}`);
    await page.locator('[data-setting-id="help-faq"] > summary').click();

    for (const state of [
      { status: "idle", checkedAt: undefined },
      { status: "checking" },
      { status: "available", update: { version: "0.3.12", mandatory: false, notes: "改进任务处理与桌面体验。\n修复已知问题，提升使用稳定性。" } },
      { status: "downloading", update: { version: "0.3.12", mandatory: false }, downloaded: 45, total: 100 },
      { status: "installing", downloaded: 100, total: 100 },
      { status: "error", errorStage: "check", error: "无法连接更新服务，请检查企业内网或 VPN 连接。" },
      { status: "error", errorStage: "download", error: "下载中断：https://example.invalid/" + "long-diagnostic-path/".repeat(12) },
      { status: "error", errorStage: "install", error: "无法写入应用目录，请确认当前用户有安装权限。" },
    ]) {
      await showUpdate(page, state);
      const status = page.locator(".help-settings__status");
      const text = state.status === "idle" ? "启动时自动检查更新" : state.status === "checking" ? "正在检查更新" : state.status === "available" ? "发现新版本" : state.status === "downloading" ? "正在下载" : state.status === "installing" ? "正在安装" : state.errorStage === "check" ? "检查更新失败" : state.errorStage === "download" ? "下载更新失败" : "安装更新失败";
      await status.filter({ hasText: text }).waitFor();
      if (["checking", "downloading", "installing"].includes(state.status)) assert(await page.locator(".help-settings button").isDisabled());
      if (state.status === "available") {
        await page.getByRole("button", { name: "下载并安装", exact: true }).waitFor();
        await page.getByText("更新内容", { exact: true }).click();
      }
      if (state.status === "error") {
        await page.getByRole("button", { name: "重新检查", exact: true }).waitFor();
        await page.getByText("查看详情", { exact: true }).click();
      }
      await checkLayout(page, `${state.status}-${state.errorStage ?? "normal"}-${width}-${theme}`);
    }
    assert.deepEqual(errors, []);
    console.log(`Help settings passed at ${width}×${height}, ${theme}, font ${font}`);
    await page.close();
  }
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ layouts }, null, 2));
  console.log(JSON.stringify({ layouts: layouts.length, output }));
} finally { await browser?.close(); await server.close(); }
