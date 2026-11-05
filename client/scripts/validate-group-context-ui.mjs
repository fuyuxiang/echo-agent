import assert from "node:assert/strict";
import { existsSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-group-context-")));
const server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/group-review.tsx" });
const errors = [], results = [];
console.log(`Group context evidence: ${output}`);
let browser;
try {
  const chrome = process.env.CHROME_BIN || (process.platform === "darwin" ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : null);
  browser = await (await reviewChromium()).launch({ headless: true, ...(chrome && existsSync(chrome) ? { executablePath: chrome } : {}) });
  for (const [width, font, theme, height = 900] of [[1280, 16, "light"], [1280, 16, "dark"], [390, 22, "light"], [390, 22, "dark"], [390, 22, "light", 680], [390, 22, "dark", 680]]) {
    const page = await browser.newPage({ viewport: { width, height } });
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?theme=${theme}`);
    await page.getByRole("button", { name: "创建第一个专家团", exact: true }).waitFor();
    await page.evaluate(async () => {
      const { useGroupStore } = await import("/src/features/groups/group-store.ts");
      useGroupStore.getState().fromTeam("source-session", { teamId: "来源团队", members: ["代码专家", "文档助手"], roles: {}, description: "保留来源与引用", createdAt: 1, sessionId: "source-session", promptIds: [] }, "来源规范：必须支持离线登录");
      const original = window.__TAURI_INTERNALS__.invoke;
      window.__TAURI_INTERNALS__.invoke = async (command, args) => {
        const result = await original(command, args);
        if (command === "group_create") { sessionStorage.setItem("first-request-id", args.input.requestId); throw new Error("隔离验收：创建已成功，响应丢失"); }
        return result;
      };
    });
    await page.getByRole("checkbox", { name: /代码专家/ }).waitFor();
    await page.getByText("引用来源对话", { exact: true }).click();
    await page.getByRole("checkbox", { name: "把下列内容共享给成员" }).check();
    await page.getByRole("button", { name: "创建并进入专家团", exact: true }).click();
    await page.getByText("隔离验收：创建已成功，响应丢失", { exact: true }).waitFor();
    await page.reload();
    await page.getByRole("checkbox", { name: /代码专家/ }).waitFor();
    await page.getByText("引用来源对话", { exact: true }).click();
    assert(await page.getByRole("checkbox", { name: "把下列内容共享给成员" }).isChecked());
    await page.getByText("来源规范：必须支持离线登录", { exact: true }).waitFor();
    await page.getByRole("button", { name: "创建并进入专家团", exact: true }).click();
    await page.locator(".group-chat__heading").getByRole("heading", { name: "来源团队", exact: true }).waitFor();
    const created = await page.evaluate(() => ({ groups: JSON.parse(sessionStorage.getItem("group-review")), first: sessionStorage.getItem("first-request-id") }));
    assert.equal(created.groups.length, 1, "a lost successful response must not create a second group");
    assert.equal(created.groups[0].id, created.first);
    assert.equal(created.groups[0].sourceSessionId, "source-session");
    assert.equal(created.groups[0].messages[0].text, "来源规范：必须支持离线登录");
    const composer = page.locator(".echo-composer__input");
    await composer.fill("不要修改后端接口，请实现前端登录页面");
    assert.equal(await page.getByText("已识别只读要求", { exact: true }).count(), 0);
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await page.waitForFunction(() => JSON.parse(sessionStorage.getItem("group-review"))[0].works.length > 0);
    assert.equal(await page.evaluate(() => JSON.parse(sessionStorage.getItem("group-review"))[0].works.at(-1).discussion), false);
    await page.evaluate(() => window.groupReview.complete());
    await page.evaluate(async () => {
      window.groupReview.seedAttachments(20);
      const { useGroupStore } = await import("/src/features/groups/group-store.ts");
      const { saveDraftAttachments } = await import("/src/lib/draft-attachments.ts");
      saveDraftAttachments(`group:${useGroupStore.getState().currentId}`, ["/review/新增资料.txt"]);
    });
    await page.getByText("沿用此前任务附件（已选 20/20）", { exact: true }).waitFor();
    await composer.fill("结合新资料继续完善");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await page.getByRole("alert").filter({ hasText: "新附件与沿用的历史附件合计最多 20 个" }).waitFor();
    assert.equal(await composer.inputValue(), "结合新资料继续完善");
    assert.equal(await page.evaluate(() => JSON.parse(sessionStorage.getItem("group-review"))[0].works.length), 1);
    await page.getByText("沿用此前任务附件（已选 20/20）", { exact: true }).click();
    await page.getByRole("checkbox", { name: "资料-0.txt", exact: true }).uncheck();
    await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), "attachment choices fit the viewport");
    const sendBox = await page.getByRole("button", { name: "发送", exact: true }).boundingBox();
    assert(sendBox && sendBox.y >= 0 && sendBox.y + sendBox.height <= height + 1, "expanded attachment choices keep Send visible");
    const permission = page.getByRole("button", { name: "专家团权限：请求批准", exact: true });
    assert(await permission.evaluate(element => element.scrollWidth <= element.clientWidth + 1), "permission controls stay legible in a narrow footer");
    assert(await page.locator(".group-chat__messages").evaluate(element => element.clientHeight >= 60), "expanded options preserve readable chat space");
    await page.screenshot({ path: join(output, `attachment-selection-${width}-${height}-${theme}.png`) });
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await page.waitForFunction(() => JSON.parse(sessionStorage.getItem("group-review"))[0].works.length === 2);
    const followup = await page.evaluate(() => JSON.parse(sessionStorage.getItem("group-review"))[0].messages.filter(message => message.kind === "user").at(-1));
    assert.equal(followup.sourceAttachmentPaths.length, 19);
    assert(!followup.sourceAttachmentPaths.includes("/review/资料-0.txt"));
    assert.deepEqual(followup.attachments, ["/review/新增资料.txt"]);
    await page.evaluate(() => window.groupReview.complete());
    await page.locator(".group-task-bar").getByText("已回复", { exact: true }).waitFor();
    await composer.fill("@全体 更新：请立即生成本周的周报");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await page.waitForFunction(() => JSON.parse(sessionStorage.getItem("group-review"))[0].works.length === 4);
    const assigned = await page.evaluate(() => JSON.parse(sessionStorage.getItem("group-review"))[0].messages.filter(message => message.kind === "user").at(-1).memberIds);
    assert.equal(assigned.length, 2, "an explicit all-members action is not redirected to the coordinator");
    results.push({ width, height, font, theme, scenarios: ["creation-response-loss", "source-recovery", "execution-intent", "attachment-limit-recovery", "explicit-recipient-routing"] });
    await page.close();
  }
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "results.json"), JSON.stringify({ results, errors }, null, 2));
  console.log(JSON.stringify({ passed: true, layouts: results.length, scenarios: results.length * 5, output, errors }));
} finally {
  await browser?.close();
  await server.close();
}
