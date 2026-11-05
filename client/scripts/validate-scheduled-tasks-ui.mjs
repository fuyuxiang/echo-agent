import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium } from "./ui-review-runtime.mjs";
const output = mkdtempSync(join(tmpdir(), "echo-scheduled-tasks-"));
const server = await createUiReviewServer(0, {
  fixture: "/scripts/fixtures/scheduled-tasks-review.tsx",
});
let browser;
const evidence = [];
try {
  browser = await (
    await reviewChromium()
  ).launch({
    headless: true,
    executablePath:
      "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  });
  for (const [width, height] of [
    [1280, 900],
    [768, 800],
    [350, 760],
  ])
    for (const theme of ["light", "dark"])
      for (const font of [16, 22]) {
        const page = await browser.newPage({ viewport: { width, height } });
        const errors = [];
        page.on("pageerror", (error) => errors.push(error.message));
        await page.goto(
          `http://127.0.0.1:${server.httpServer.address().port}/__ui-review?theme=${theme}`,
        );
        await page
          .getByRole("button", { name: "每日 AI 新闻简报", exact: true })
          .waitFor();
        await page.evaluate((value) => {
          document.documentElement.style.fontSize = `${value}px`;
        }, font);
        const assertFits = async () => {
          const value = await page.evaluate(
            () =>
              document.documentElement.scrollWidth <= innerWidth + 1 &&
              [
                ...document.querySelectorAll(
                  ".atm-task-card,.atm-editor-dialog,.atm-editor-footer,.atm-task-detail",
                ),
              ].every((el) => el.scrollWidth <= el.clientWidth + 1),
          );
          assert(value, `overflow ${width} ${theme}`);
        };
        await assertFits();
        const layout = await page.locator(".atm-task-cards").evaluate((grid) => {
          const cards = [...grid.children].map((card) => {
            const { x, y, width, height } = card.getBoundingClientRect();
            return { x, y, width, height };
          });
          const runtime = document.querySelector(".atm-runtime").getBoundingClientRect();
          return { cards, runtimeHeight: runtime.height };
        });
        const expectedColumns = 1;
        assert.equal(layout.cards.filter(card => Math.abs(card.y - layout.cards[0].y) < 1).length, expectedColumns, `card columns ${width}/${font}`);
        assert(layout.cards.every(card => card.width >= 270), `cards must remain readable ${width}/${font}`);
        if (width === 1280 && font === 16) assert(layout.runtimeHeight <= 52, "runtime summary must remain compact");
        for (const label of ["新建定时任务", "从模板添加"]) {
          const entry = page.getByRole("button", { name: label, exact: true });
          assert(await entry.isVisible(), `${label}: direct entry`);
          assert.equal(await entry.evaluate(el => getComputedStyle(el).backgroundColor), "rgba(0, 0, 0, 0)", `${label}: no solid fill`);
        }
        await page.screenshot({
          path: join(output, `list-${width}-${theme}-${font}.png`),
        });
        await page.getByRole("button", { name: /^已结束\s*1$/ }).click();
        assert.equal(await page.locator(".atm-task-card").count(), 1);
        assert.equal(Math.round((await page.locator(".atm-task-card").boundingBox()).width), Math.round(layout.cards[0].width), "filtering must preserve card width");
        assert.match(
          await page.locator(".atm-task-card").innerText(),
          /已错过/,
        );
        await page.getByRole("button", { name: /^全部/ }).click();
        await page.getByRole("button", { name: "批量管理", exact: true }).click();
        for (const [name, state] of [["每日 AI 新闻简报", "已启用"], ["每周工作回顾", "已暂停"], ["已结束的一次性报告", "已错过"]]) {
          const row = page.getByRole("listitem", { name: `定时任务 ${name}`, exact: true });
          assert(await row.getByText(state, { exact: true }).isVisible(), "batch deletion must retain task state");
        }
        await page.getByRole("checkbox", { name: "选择定时任务「每周工作回顾」", exact: true }).check();
        assert.equal(await page.getByRole("switch").count(), 0, "batch selection does not expose live switches");
        await assertFits();
        await page.screenshot({ path: join(output, `batch-${width}-${theme}-${font}.png`) });
        await page.getByRole("button", { name: "退出管理", exact: true }).click();
        await page
          .getByRole("button", { name: "每日 AI 新闻简报", exact: true })
          .click();
        await page.getByText("今日简报", { exact: true }).waitFor();
        assert.match(
          await page.getByLabel("本次运行结果").innerText(),
          /已完成/,
        );
        assert.match(
          await page.getByLabel("本次运行结果").innerText(),
          /通知投递未全部成功/,
        );
        await page.screenshot({
          path: join(output, `detail-${width}-${theme}-${font}.png`),
        });
        await page
          .getByRole("button", { name: "编辑任务", exact: true })
          .click();
        await page
          .getByRole("dialog", { name: "编辑定时任务", exact: true })
          .waitFor();
        assert.equal(
          await page
            .getByRole("switch", { name: "每次运行都开启新会话" })
            .count(),
          0,
          "advanced options initially collapsed",
        );
        await page
          .getByRole("button", { name: "高级设置", exact: true })
          .click();
        const fresh = page.getByRole("switch", {
          name: "每次运行都开启新会话",
        });
        assert.equal(await fresh.getAttribute("aria-checked"), "true");
        await fresh.click();
        await page.getByRole("button", { name: /^选择定时任务模型：/ }).click();
        const menu = page.getByRole("listbox", { name: "选择定时任务模型" });
        await menu.waitFor();
        assert.equal(await menu.getByRole("group").count(), 3);
        await menu
          .getByRole("option", { name: "团队模型", exact: true })
          .click();
        await page.keyboard.press("Escape");
        // Escape after closing the picker opens unsaved protection; dismiss it and
        // retain the form before saving.
        if (await page.getByRole("alertdialog").count())
          await page
            .getByRole("alertdialog")
            .getByRole("button", { name: "继续编辑", exact: true })
            .click();
        await assertFits();
        await page.screenshot({
          path: join(output, `editor-${width}-${theme}-${font}.png`),
        });
        await page
          .getByRole("button", { name: "保存修改", exact: true })
          .click();
        await page
          .getByRole("heading", { name: "每日 AI 新闻简报", exact: true })
          .waitFor();
        assert.match(
          await page.locator(".atm-task-detail").innerText(),
          /沿用任务专属会话/,
        );
        await page
          .getByRole("button", { name: "立即运行", exact: true })
          .click();
        await page.getByText(/第 1 次运行的独立结果/).waitFor();
        await page
          .getByRole("button", { name: "立即运行", exact: true })
          .click();
        await page.getByText(/第 2 次运行的独立结果/).waitFor();
        await page
          .getByRole("button", { name: "重置会话上下文", exact: true })
          .click();
        await page
          .getByRole("dialog", { name: "重置任务会话上下文？" })
          .getByRole("button", { name: "重置上下文" })
          .click();
        assert.equal(await page.locator(".atm-task-history__row").count(), 3);
        await assertFits();
        const calls = await page.evaluate(() => window.scheduledReview.calls);
        assert.equal(
          calls.filter((call) => call.command === "automations_run").length,
          2,
        );
        assert(
          calls.find(
            (call) =>
              call.command === "automations_save" &&
              call.args.automation.newSessionEachRun === false &&
              call.args.automation.modelId === "org/m",
          ),
        );
        assert.equal(
          calls.filter((call) => call.command === "automations_reset_session")
            .length,
          1,
        );
        assert.deepEqual(errors, []);
        evidence.push({
          width,
          height,
          theme,
          font,
          columns: expectedColumns,
          flows:
            "list/ended/detail/notification/editor/models/session-mode/run/reset",
          errors,
        });
        await page.close();
      }

  // Long content stays in a full-width row; controls remain inside each row.
  const grid = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  await grid.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?many=1`);
  await grid.getByRole("button", { name: "每日 AI 新闻简报", exact: true }).waitFor();
  const rows = await grid.locator(".atm-task-card").evaluateAll(cards => cards.map(card => {
    const rect = card.getBoundingClientRect();
    const actions = card.querySelector(".atm-task-card__actions").getBoundingClientRect();
    return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom, actionsRight: actions.right, actionsBottom: actions.bottom, width: rect.width, fits: card.scrollWidth <= card.clientWidth + 1 };
  }));
  assert.equal(rows.length, 6);
  for (const [index, row] of rows.entries()) {
    assert(row.fits);
    assert(Math.abs(row.left - rows[0].left) < 1 && Math.abs(row.width - rows[0].width) < 1, "task rows share the same width");
    assert(row.actionsRight <= row.right && row.actionsBottom <= row.bottom, "task controls stay inside the row");
    if (index) assert(row.top >= rows[index - 1].bottom - 1, "tasks remain in a vertical list");
  }
  const help = grid.getByRole("button", { name: "运行说明", exact: true });
  await help.click();
  assert.equal(await help.getAttribute("aria-expanded"), "true");
  assert(await grid.getByText(/电脑需保持开机且 EchoAgent/).isVisible());
  await help.click();
  assert.equal(await help.getAttribute("aria-expanded"), "false");
  await grid.screenshot({ path: join(output, "list-1440-multiple-rows.png") });
  await grid.close();

  // A blocked run has a clear route to its real conversation. Stopping the
  // occurrence does not pause or rewrite the future schedule.
  const blocked = await browser.newPage({ viewport: { width: 1024, height: 800 } });
  await blocked.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?state=permission`);
  await blocked.getByRole('button', { name: '每日 AI 新闻简报', exact: true }).click();
  assert.match(await blocked.getByLabel('本次运行结果').innerText(), /待确认/);
  await blocked.getByRole('button', { name: '前往处理', exact: true }).click();
  await blocked.getByTestId('review-feedback').filter({ hasText: '已打开会话：session-old' }).waitFor();
  await blocked.getByRole('button', { name: '停止本次运行', exact: true }).click();
  await blocked.getByLabel('本次运行结果').getByText('已停止', { exact: true }).waitFor();
  const afterStop = await blocked.evaluate(() => window.scheduledReview.snapshot());
  assert.equal(afterStop.tasks.find(task => task.id === 'news').status, 'ACTIVE');
  assert.equal(afterStop.records.find(run => run.id === 'old').status, 'stopped');
  await blocked.close();

  const creation = await browser.newPage({ viewport: { width: 1024, height: 800 } });
  await creation.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review`);
  await creation.getByRole('button', { name: '新建定时任务', exact: true }).click();
  await creation.getByRole('textbox', { name: '名称', exact: true }).fill('新的独立任务');
  await creation.keyboard.press('Escape');
  await creation.getByRole('alertdialog').getByRole('button', { name: '继续编辑', exact: true }).click();
  assert.equal(await creation.getByRole('textbox', { name: '名称', exact: true }).inputValue(), '新的独立任务');
  await creation.getByRole('textbox', { name: '任务要求', exact: true }).fill('整理今天的进展，附上来源。');
  await creation.getByRole('button', { name: '高级设置', exact: true }).click();
  assert.equal(await creation.getByRole('switch', { name: '每次运行都开启新会话' }).getAttribute('aria-checked'), 'true');
  await creation.getByRole('button', { name: '创建任务', exact: true }).click();
  await creation.getByRole('heading', { name: '新的独立任务', exact: true }).waitFor();
  const creationCalls = await creation.evaluate(() => window.scheduledReview.calls);
  assert.equal(creationCalls.filter(call => call.command === 'automations_save').length, 1);
  assert.equal(creationCalls.filter(call => call.command === 'automations_run').length, 0, 'saving must not execute a task');
  await creation.close();
  const templates = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  await templates.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review`);
  await templates.getByRole("button", { name: "从模板添加", exact: true }).click();
  await templates.locator(".template-card").first().waitFor();
  await templates.getByRole("button", { name: "定时任务", exact: true }).click();
  assert.equal(await templates.locator(".atm-task-card").count(), 3);
  await templates.getByRole("button", { name: "从模板添加", exact: true }).click();
  await templates.locator(".template-card").first().click();
  await templates.getByRole("dialog", { name: "新建定时任务", exact: true }).waitFor();
  assert(await templates.getByRole("textbox", { name: "名称", exact: true }).inputValue());
  assert(await templates.getByRole("textbox", { name: "任务要求", exact: true }).inputValue());
  assert.equal(await templates.evaluate(() => window.scheduledReview.calls.filter(call => ["automations_save", "automations_run"].includes(call.command)).length), 0, "template choice prepares a draft without saving or running");
  await templates.close();
  for (const [theme, width, font] of [["light", 1280, 16], ["dark", 1024, 22]]) {
    const live = await browser.newPage({ viewport: { width, height: 800 } });
    const errors = []; live.on("pageerror", error => errors.push(error.message));
    await live.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?state=live-preview&theme=${theme}`);
    await live.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
    await live.getByRole("button", { name: "每日 AI 新闻简报", exact: true }).click();
    await live.getByText("正在等待执行准备完成，开始后会自动显示进度。").waitFor();
    await live.evaluate(() => window.scheduledReview.advance("admitted"));
    await live.getByRole("button", { name: "查看实时过程", exact: true }).waitFor();
    await live.evaluate(() => window.scheduledReview.advance("progress"));
    await live.getByText("已核实第一项来源，正在整理报告。", { exact: true }).waitFor();
    assert.match(await live.getByLabel("本次执行进度").innerText(), /已处理 1\/2/);
    assert.equal(await live.locator(".atm-task-detail").evaluate(el => el.scrollWidth > el.clientWidth + 1), false);
    await live.screenshot({ path: join(output, `live-progress-${theme}.png`) });
    await live.evaluate(() => window.scheduledReview.advance("read-error"));
    await live.getByRole("alert").filter({ hasText: "读取执行内容失败" }).waitFor();
    assert(await live.getByText("已核实第一项来源，正在整理报告。", { exact: true }).isVisible(), "read failure erased partial content");
    await live.evaluate(() => window.scheduledReview.advance("completed"));
    await live.getByRole("heading", { name: "唯一最终结论", exact: true }).waitFor();
    await live.getByRole("button", { name: "查看完整会话", exact: true }).waitFor();
    assert.equal(await live.getByRole("heading", { name: "唯一最终结论", exact: true }).count(), 1);
    assert.equal(await live.getByText("已核实第一项来源，正在整理报告。", { exact: true }).count(), 0);
    await live.screenshot({ path: join(output, `live-completed-${theme}.png`) });
    assert.deepEqual(errors, []);
    await live.close();
  }
  writeFileSync(
    join(output, "evidence.json"),
    JSON.stringify(evidence, null, 2),
  );
  console.log(JSON.stringify({ output, layouts: evidence.length, extraFlows: ["pending-approval/stop/future-plan", "create/unsaved/default-session/no-auto-run", "direct-template/back/prefilled-draft/no-auto-run", "late-admission/live-progress/read-failure/final-result"] }));
} finally {
  await browser?.close();
  await server.close();
}
