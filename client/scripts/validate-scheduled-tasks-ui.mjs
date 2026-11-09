import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-scheduled-tasks-")));
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
              ].every((el) => el.scrollWidth <= el.clientWidth + 1) &&
              [...document.querySelectorAll('.atm-task-card')].every(card => {
                const rect = card.getBoundingClientRect();
                const main = card.querySelector('.atm-task-card__main').getBoundingClientRect();
                const actions = card.querySelector('.atm-task-card__actions')?.getBoundingClientRect();
                return !actions || (actions.left >= rect.left && actions.right <= rect.right + 1
                  && actions.bottom <= rect.bottom + 1
                  && (actions.top >= main.bottom - 1 || actions.left >= main.right - 1));
              }),
          );
          assert(value, `overflow ${width} ${theme}`);
        };
        await assertFits();
        const layout = await page.locator(".atm-task-cards").evaluate((grid) => {
          const cards = [...grid.children].map((card) => {
            const { x, y, width, height } = card.getBoundingClientRect();
            return { x, y, width, height };
          });
          return { cards, collectionWidth: grid.parentElement.clientWidth };
        });
        const expectedColumns = layout.collectionWidth >= 62 * font ? 3 : layout.collectionWidth >= 40 * font ? 2 : 1;
        assert.equal(layout.cards.filter(card => Math.abs(card.y - layout.cards[0].y) < 1).length, expectedColumns, `card columns ${width}/${font}`);
        assert(layout.cards.every(card => card.width >= 270), `cards must remain readable ${width}/${font}`);
        assert.equal(await page.locator(".atm-task-list--cards > :first-child").getAttribute("aria-label"), "任务状态筛选");
        for (const label of ["新建定时任务", "从模板添加"]) {
          const entry = page.getByRole("button", { name: label, exact: true });
          assert(await entry.isVisible(), `${label}: direct entry`);
          assert.equal(await entry.evaluate(el => getComputedStyle(el).backgroundColor), "rgba(0, 0, 0, 0)", `${label}: no solid fill`);
        }
        await page.screenshot({
          path: join(output, `cards-${width}-${theme}-${font}.png`),
        });
        const listView = page.getByRole('button', { name: '列表视图', exact: true });
        await listView.focus();
        await page.keyboard.press('Enter');
        assert(await listView.evaluate(el => el === document.activeElement), 'switching retains keyboard focus');
        await page.locator('.atm-task-cards[data-view="list"]').waitFor();
        const listRows = await page.locator('.atm-task-card').evaluateAll(cards => cards.map(card => {
          const { x, y, width, height } = card.getBoundingClientRect();
          return { x, y, width, height };
        }));
        assert.equal(listRows.length, 3);
        assert(listRows.slice(1).every((row, index) => row.y >= listRows[index].y + listRows[index].height - 1), 'list rows remain vertical');
        if (expectedColumns > 1) {
          assert(listRows[0].width > layout.cards[0].width * 1.5, 'cards and list must have different geometry');
          assert(listRows[0].height < layout.cards[0].height, 'wide list rows are more compact');
        }
        await assertFits();
        await page.screenshot({ path: join(output, `list-${width}-${theme}-${font}.png`) });
        await page.reload();
        await page.locator('.atm-task-cards[data-view="list"]').waitFor();
        await page.evaluate(value => { document.documentElement.style.fontSize = `${value}px`; }, font);
        const cardsView = page.getByRole('button', { name: '卡片视图', exact: true });
        await cardsView.focus();
        await page.keyboard.press('Space');
        assert(await cardsView.evaluate(el => el === document.activeElement), 'returning to cards retains keyboard focus');
        await page.locator('.atm-task-cards[data-view="cards"]').waitFor();
        await page.reload();
        await page.locator('.atm-task-cards[data-view="cards"]').waitFor();
        await page.evaluate(value => { document.documentElement.style.fontSize = `${value}px`; }, font);
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
        for (const view of ['列表视图', '卡片视图']) {
          await page.getByRole('button', { name: view, exact: true }).click();
          assert(await page.getByRole('checkbox', { name: '选择定时任务「每周工作回顾」', exact: true }).isChecked(), 'switching views preserves batch selection');
          assert.equal(await page.locator('.atm-task-card').count(), 3);
          assert.equal(await page.getByRole('button', { name: /^立即运行「/ }).count(), 0);
          await assertFits();
        }
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
        assert.equal(await menu.getByRole("group").count(), 2, "Only organization and personal model groups remain");
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
            "cards/list/keyboard/persistence/batch-selection/ended/detail/notification/editor/models/session-mode/run/reset",
          errors,
        });
        await page.close();
      }

  // Long content remains readable in a real grid and in full-width list rows.
  const grid = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  await grid.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?many=1`);
  await grid.getByRole("button", { name: "每日 AI 新闻简报", exact: true }).waitFor();
  for (const view of ['cards', 'list']) {
    await grid.getByRole('button', { name: view === 'cards' ? '卡片视图' : '列表视图', exact: true }).click();
    const rows = await grid.locator(".atm-task-card").evaluateAll(cards => cards.map(card => {
      const rect = card.getBoundingClientRect();
      const actions = card.querySelector(".atm-task-card__actions").getBoundingClientRect();
      const main = card.querySelector('.atm-task-card__main').getBoundingClientRect();
      return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom, actionsRight: actions.right, actionsBottom: actions.bottom, actionsTop: actions.top, mainBottom: main.bottom, width: rect.width, fits: card.scrollWidth <= card.clientWidth + 1 };
    }));
    assert.equal(rows.length, 6);
    for (const [index, row] of rows.entries()) {
      assert(row.fits);
      assert(Math.abs(row.width - rows[0].width) < 1, "tasks share the same column width");
      assert(row.actionsRight <= row.right && row.actionsBottom <= row.bottom, "task controls stay inside the row");
      if (view === 'cards') assert(row.actionsTop >= row.mainBottom - 1, 'card footer never overlaps long content');
      if (view === 'list' && index) assert(row.top >= rows[index - 1].bottom - 1, "tasks remain in a vertical list");
    }
    assert.equal(rows.filter(row => Math.abs(row.top - rows[0].top) < 1).length, view === 'cards' ? 3 : 1, 'wide cards form three columns; list remains one column');
    await grid.screenshot({ path: join(output, `${view}-1440-long-content.png`) });
  }
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
  for (const [width, theme, font] of [[350, "light", 22], [768, "dark", 22], [1280, "light", 16]]) {
    const page = await browser.newPage({viewport:{width,height:900}});
    const errors=[];page.on("pageerror",error=>errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?retired=1&theme=${theme}`);
    await page.evaluate(font=>{document.documentElement.style.fontSize=`${font}px`;},font);
    const card=page.getByRole("listitem",{name:"定时任务 每日 AI 新闻简报",exact:true});
    await card.getByText("待选模型",{exact:true}).waitFor();
    assert.equal(await card.getByRole("switch").isEnabled(),false);
    assert.equal(await card.getByRole("button",{name:"立即运行「每日 AI 新闻简报」"}).isEnabled(),false);
    assert.equal(await card.locator(".atm-task-card__next").count(),0);
    assert.equal(await card.evaluate(el=>el.scrollWidth>el.clientWidth+1),false);
    await card.screenshot({path:join(output,`retired-model-${width}-${theme}.png`)});
    await card.getByRole("button",{name:"编辑定时任务「每日 AI 新闻简报」"}).click();
    const editor=page.getByRole("dialog",{name:"编辑定时任务",exact:true});await editor.waitFor();
    await editor.getByRole("button",{name:"跟随默认模型",exact:true}).waitFor();
    assert.equal(await editor.getByRole("button",{name:"高级设置",exact:true}).getAttribute("aria-expanded"),"false");
    const modelField=editor.getByRole("region",{name:"重新选择任务模型"});
    assert.equal(await modelField.isVisible(),true);
    assert.equal(await editor.getByRole("button",{name:"保存并立即运行",exact:true}).isEnabled(),false);
    assert.equal(await editor.evaluate(el=>el.scrollWidth>el.clientWidth+1),false);
    await page.screenshot({path:join(output,`retired-model-editor-${width}-${theme}.png`)});
    await editor.getByRole("button",{name:"跟随默认模型",exact:true}).click();
    await editor.getByRole("button",{name:"保存修改",exact:true}).click();
    await editor.waitFor({state:"hidden"});
    const saved=await page.evaluate(()=>window.scheduledReview.snapshot().tasks.find(task=>task.id==="news"));
    assert.equal(saved.status,"PAUSED");assert.equal(saved.modelId,undefined);
    assert.equal(await page.evaluate(()=>window.scheduledReview.calls.some(call=>["automations_run","automations_set_status"].includes(call.command))),false);
    assert.deepEqual(errors,[]);await page.close();
  }
  writeFileSync(
    join(output, "evidence.json"),
    JSON.stringify(evidence, null, 2),
  );
  console.log(JSON.stringify({ output, layouts: evidence.length, extraFlows: ["pending-approval/stop/future-plan", "create/unsaved/default-session/no-auto-run", "direct-template/back/prefilled-draft/no-auto-run", "late-admission/live-progress/read-failure/final-result", "retired-model/explicit-reselection/save-paused/no-auto-run"] }));
} finally {
  await browser?.close();
  await server.close();
}
