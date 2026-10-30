import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-notification-settings-")));
const server = await createUiReviewServer(0);
const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
const layouts = [];
let browser;
async function verifyLayout(page, context) {
  const overflow = await page.locator(".notify-panel, .notify-advanced, .notify-panel__editor, .notification-row").evaluateAll(elements => elements.filter(element => element.scrollWidth > element.clientWidth + 1).map(element => ({ className: element.className, width: element.clientWidth, scrollWidth: element.scrollWidth })));
  assert.deepEqual(overflow, [], `Horizontal overflow: ${JSON.stringify(context)}`);
  layouts.push(context);
}
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const [width, height] of [[800, 640], [1024, 768], [1440, 1000]]) {
    for (const font of [13, 18]) for (const theme of ["light", "dark"]) {
      const page = await browser.newPage({ viewport: { width, height } });
      page.setDefaultNavigationTimeout(60000);
      const errors = []; page.on("pageerror", error => errors.push(error.message));
      await page.addInitScript(font => {
        const observer = new MutationObserver(() => {
          if (document.documentElement) { document.documentElement.style.fontSize = `${16 * font / 13}px`; observer.disconnect(); }
        });
        observer.observe(document, { childList: true, subtree: true });
      }, font);
      await page.goto(`${origin}/__ui-review?surface=notify-channels&theme=${theme}`, { waitUntil: "networkidle" });
      const desktop = page.getByRole("switch", { name: "桌面通知", exact: true });
      assert(await desktop.isChecked());
      assert.equal(await page.getByRole("textbox").count(), 0, "Webhook form should not dominate the initial settings view");
      const privacy = page.getByRole("switch", { name: "在通知中显示任务标题" });
      assert(!await privacy.isChecked());
      await privacy.locator("..").click();
      assert(await privacy.isChecked(), "Privacy preference did not save");
      const advanced = page.getByRole("button", { name: /推送到其他应用/ });
      assert.equal(await advanced.getAttribute("aria-expanded"), "false");
      await verifyLayout(page, { width, height, font, theme, view: "preferences" });
      if (width === 1440 && font === 13 && theme === "light") {
        await privacy.locator("..").click();
        assert(!await privacy.isChecked());
        await page.screenshot({ path: join(output, "notification-preferences.png") });
      }
      await privacy.focus();
      await page.keyboard.press("Tab");
      assert(await advanced.evaluate(element => element === document.activeElement));
      assert.equal(await advanced.evaluate(element => getComputedStyle(element).outlineStyle), "solid");
      await page.keyboard.press("Enter");
      await page.getByRole("button", { name: "添加渠道", exact: true }).click();
      await page.getByRole("textbox", { name: "通知渠道显示名" }).fill("团队日报与任务结果");
      await page.getByRole("textbox", { name: "Webhook URL" }).fill("https://hooks.example.test/private-review-token");
      await verifyLayout(page, { width, height, font, theme, view: "channel-editor" });
      await page.getByRole("button", { name: "+ 添加", exact: true }).click();
      await page.getByText("团队日报与任务结果", { exact: true }).waitFor();
      assert.equal(await page.getByRole("form").count(), 0);
      assert.equal(await page.getByText(/private-review-token/).count(), 0, "Channel list leaked the submitted credential");
      await verifyLayout(page, { width, height, font, theme, view: "configured" });
      assert.deepEqual(errors, []);
      await page.close();
    }
    console.log(`Notification settings passed at ${width}×${height}`);
  }
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  page.setDefaultNavigationTimeout(60000);
  for (const failure of ["channel-error", "desktop-error"]) {
    await page.goto(`${origin}/__ui-review?surface=notify-channels&notificationState=${failure}`, { waitUntil: "networkidle" });
    await page.getByRole("button", { name: /推送到其他应用/ }).click();
    if (failure === "channel-error") {
      assert(await page.getByRole("button", { name: "发送测试通知" }).isEnabled());
      await page.getByRole("button", { name: "重试", exact: true }).click();
      await page.getByText("尚未添加外部推送，日常桌面提醒无需配置这里。", { exact: true }).waitFor();
    } else {
      assert(await page.getByRole("switch", { name: "桌面通知", exact: true }).isDisabled());
      assert(await page.getByRole("button", { name: "添加渠道", exact: true }).isEnabled());
      await page.getByRole("button", { name: "重试读取桌面提醒" }).click();
      await page.getByRole("button", { name: "发送测试通知" }).click();
      await page.getByRole("status").filter({ hasText: "已向系统提交通知" }).waitFor();
    }
  }
  await page.goto(`${origin}/__ui-review?surface=notify-channels&notificationState=configured&theme=dark`, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: /推送到其他应用/ }).click();
  const legacy = page.locator(".notify-panel__row").filter({ hasText: "旧邮件配置" });
  assert.equal(await legacy.getByRole("button").count(), 1);
  assert.equal(await legacy.getByRole("switch").count(), 0);
  assert.equal(await page.getByText(/private-review-token/).count(), 0);
  await page.locator(".notify-advanced").scrollIntoViewIfNeeded();
  await page.locator(".notify-advanced").screenshot({ path: join(output, "notification-advanced-dark.png") });
  await page.goto(`${origin}/__ui-review?surface=agent-mail&notificationState=records`, { waitUntil: "networkidle" });
  assert.equal(await page.getByRole("checkbox").count(), 0, "Notification records should not contain preferences");
  assert.equal(await page.getByRole("heading", { name: "通知概览" }).count(), 0);
  const details = page.locator(".notification-row__details");
  assert.equal(await details.getAttribute("open"), null);
  assert(!await details.locator("pre").isVisible());
  await details.locator("summary").click();
  assert(await details.locator("pre").isVisible());
  await details.locator("summary").click();
  await page.getByRole("button", { name: "需要处理", exact: true }).click();
  assert.equal(await page.locator(".notification-row").count(), 1);
  await page.getByRole("button", { name: "异常", exact: true }).click();
  assert.equal(await page.locator(".notification-row").count(), 1);
  await page.getByRole("button", { name: "系统事件", exact: true }).click();
  assert.equal(await page.locator(".notification-row").count(), 1);
  await page.getByRole("button", { name: "全部", exact: true }).click();
  await page.screenshot({ path: join(output, "notification-records.png") });
  await verifyLayout(page, { view: "records" });
  writeFileSync(join(output, "layouts.json"), JSON.stringify(layouts, null, 2));
  console.log(`Passed ${layouts.length} layouts, channel setup, privacy save, independent failure recovery and notification record filters; evidence: ${output}`);
} finally { await browser?.close(); await server.close(); }
