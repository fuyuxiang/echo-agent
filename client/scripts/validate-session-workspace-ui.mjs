import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { openWorkspaceView } from "./session-review-navigation.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-session-workspace-")));
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true,
    executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  const page = await browser.newPage();
  page.setDefaultTimeout(15000);
  page.setDefaultNavigationTimeout(60000);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  for (const width of [1440, 1024, 390]) for (const theme of ["light", "dark"]) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(`${base}?surface=workbench&theme=${theme}`);
    await page.locator(".session-header").waitFor();
    if (width < 900) await page.getByRole("button", {name:"收起侧边栏",exact:true}).click();
    assert.equal(await page.locator(".session-header").getByRole("button", {name:"打开工作目录"}).count(), 0);
    await page.getByRole("button", {name:/^打开工作目录：/}).click();
    await page.waitForFunction(() => window.workbenchCalls.some(c => c.command === "open_path" && c.args.path.includes("desktop-client")));
    await openWorkspaceView(page, "文件");
    const directory = page.getByRole("region", {name:"本任务工作目录"});
    await directory.waitFor();
    await directory.getByRole("button", { name: "打开文件夹", exact: true }).click();
    await page.waitForFunction(() => window.workbenchCalls.filter(c => c.command === "open_path" && c.args.path.includes("desktop-client")).length === 2);
    const fits = await directory.evaluate(element => {
      const rect = element.getBoundingClientRect();
      return rect.x >= 0 && rect.right <= innerWidth + 1 && element.scrollWidth <= element.clientWidth + 1;
    });
    assert.ok(fits, `long directory path overflow at ${width}`);
    await page.screenshot({path:join(output, `files-${width}-${theme}.png`)});

    await page.goto(`${base}?surface=workbench&workspace=missing&theme=${theme}`);
    await page.getByRole("alert").filter({hasText:"本任务的工作目录已找不到"}).waitFor();
    if (width < 900) await page.getByRole("button", {name:"收起侧边栏",exact:true}).click();
    const input = page.getByRole("textbox").last();
    await input.fill("保留这条草稿");
    await input.press("Enter");
    assert.equal(await page.evaluate(() => window.workbenchCalls.filter(c => c.command === "review_send").length), 0);
    const retry = page.getByRole("button", {name:"重新生成",exact:true}).last();
    assert.ok(await retry.isDisabled(), "an unavailable directory must block reply replacement too");
    assert.equal(await page.evaluate(() => window.workbenchCalls.filter(c => c.command === "rewind_execute").length), 0);
    await page.getByRole("button", {name:"重试连接",exact:true}).click();
    assert.equal(await input.inputValue(), "保留这条草稿");
    await openWorkspaceView(page, "文件");
    const openFolder = directory.getByRole("button", {name:"打开文件夹",exact:true});
    assert.ok(await openFolder.isDisabled());
    await page.screenshot({path:join(output, `missing-${width}-${theme}.png`)});
    await page.evaluate(() => { window.workspaceReady = true; });
    await page.getByRole("button", {name:"恢复连接",exact:true}).click();
    await page.waitForFunction(() => window.workspaceReloaded);
    await page.getByRole("alert").filter({hasText:"本任务的工作目录已找不到"}).waitFor({state:"detached"});
    assert.equal(await input.inputValue(), "保留这条草稿");
    await openFolder.click();
    await page.waitForFunction(() => window.workbenchCalls.some(c => c.command === "open_path" && c.args.path.includes("desktop-client")));
    assert.ok(await retry.isEnabled(), "restoring the directory must restore reply actions");
    await page.getByRole("button", {name:"关闭面板",exact:true}).click();
  }
  await page.setViewportSize({width:1280,height:900});
  await page.goto(`${base}?surface=workbench&state=files`);
  await openWorkspaceView(page, "成果");
  await page.getByRole("button", {name:/^report.md/}).click();
  await page.getByRole("button", {name:"在文件夹中显示",exact:true}).click();
  await page.waitForFunction(() => window.workbenchCalls.some(c => c.command === "reveal_in_folder" && c.args.path.endsWith("report.md")));
  await page.getByRole("button", {name:"用系统应用打开",exact:true}).click();
  await page.waitForFunction(() => window.workbenchCalls.some(c => c.command === "open_path" && c.args.path.endsWith("report.md")));
  await page.goto(`${base}?surface=home&automaticWorkspace=1`);
  const picker = page.getByRole("button", {name:/工作目录：默认任务目录/});
  await picker.click();
  await page.getByRole("menuitemradio", {name:/review\/Other/}).click();
  await page.getByRole("button", {name:/工作目录：\/review\/Other/}).click();
  await page.getByRole("menuitemradio", {name:"默认任务目录",exact:true}).click();
  await picker.waitFor();
  await page.screenshot({path:join(output, "new-task.png")});
  await page.goto(`${base}?surface=home&automaticWorkspace=1&state=loading`);
  assert.ok(await page.getByRole("button", {name:/工作目录：默认任务目录/}).isDisabled());
  assert.deepEqual(errors, []);
  console.log(`Session workspace UI passed: creation, fixed directory, recovery, long paths, desktop/mobile, light/dark. Evidence: ${output}`);
} finally {
  await browser?.close();
  await server.close();
}
