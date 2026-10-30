// Run with the same Node/Playwright installation as scripts/smoke-theia.mjs.
// Screenshots and geometry checks use production components and CSS, with
// native IPC replaced only in the development fixture.
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import assert from "node:assert/strict";
import { validateConversationLayout } from "./validate-conversation-layout.mjs";
import { validateSidebarScroll } from "./validate-sidebar-scroll.mjs";
import { validateOfficeExportMenu } from "./validate-office-export-menu.mjs";
import { validateQueueUi } from "./validate-queue-ui.mjs";
import { validateAutomationEditorUi } from "./validate-automation-editor-ui.mjs";
import { validateExtensionLayouts } from "./validate-extension-layout.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-ui-surfaces-")));
console.log(`UI screenshots: ${output}`);
const server = await createUiReviewServer();
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  const page = await browser.newPage();
page.setDefaultNavigationTimeout(60000);
  const errors = [];
  let layouts = 0;
  page.on("pageerror", error => errors.push(error.message));
  const surfaces = ["memory", "personal-memory", "security", "cloud-storage", "notify-channels", "weixin-connected", "weixin-offline", "weixin-unconnected", "capabilities", "coding", "permission-picker", "organization", "conversation", "meeting"];
  for (const surface of ["collapsed-project", "collapsed-capabilities"]) {
    for (const platform of ["mac", "other"]) {
      for (const width of [1440, 768]) {
        await page.setViewportSize({ width, height: 720 });
        await page.goto(`http://127.0.0.1:1439/__ui-review?surface=${surface}&platform=${platform}`);
        await page.locator(surface === "collapsed-project" ? ".pd-crumb" : ".panel-navigation__links").waitFor();
        const layout = await page.evaluate((surface) => {
          const box = (selector) => document.querySelector(selector).getBoundingClientRect();
          const controls = box(".main-page-controls");
          const heading = box(surface === "collapsed-project" ? ".pd-crumb" : ".panel-navigation__links");
          const content = box(surface === "collapsed-project" ? ".pd-page" : ".panel-section");
          const main = box(".app__main");
          const buttons = [...document.querySelectorAll(".main-page-controls button")].map((element) => element.getBoundingClientRect());
          return { controls: { top: controls.top, bottom: controls.bottom }, heading: { top: heading.top }, content: { top: content.top, bottom: content.bottom }, main: { bottom: main.bottom }, buttons: buttons.map(({ left, right }) => ({ left, right })) };
        }, surface);
        assert.ok(layout.content.top >= layout.controls.bottom - 1, `${surface} ${platform} ${width}: page starts behind controls`);
        assert.ok(layout.heading.top >= layout.controls.bottom - 1, `${surface} ${platform} ${width}: heading overlaps controls`);
        assert.ok(layout.content.bottom <= layout.main.bottom + 1, `${surface} ${platform} ${width}: page extends below main`);
        assert.ok(layout.buttons[0].left >= (platform === "mac" ? 84 : 12), `${surface} ${platform} ${width}: controls overlap window chrome`);
        assert.ok(layout.buttons[1].left >= layout.buttons[0].right, `${surface} ${platform} ${width}: controls overlap each other`);
        layouts += 1;
      }
    }
  }
  for (const [width, height, theme] of [[1440, 900, "light"], [1024, 768, "dark"], [768, 720, "light"]]) {
    await page.setViewportSize({ width, height });
    for (const surface of surfaces) {
      await page.goto(`http://127.0.0.1:1439/__ui-review?surface=${surface}&theme=${theme}`);
      await page.addStyleTag({ content: "*,*::before,*::after{animation:none!important;transition:none!important}" });
      if (surface === "personal-memory") {
        await page.getByRole("heading", { name: "记忆", exact: true }).waitFor();
        assert.equal(await page.getByRole("switch").count(), 1, "memory should expose only its master switch");
        assert.equal(await page.locator(".resources-panel").count(), 0, "legacy documents should be loaded on demand");
        const memoryLayout = await page.evaluate(() => ({
          overflow: document.documentElement.scrollWidth > innerWidth + 1,
          cards: document.querySelectorAll(".personal-memory__card").length,
        }));
        assert.ok(!memoryLayout.overflow, "personal memory causes horizontal page overflow");
        assert.equal(memoryLayout.cards, 2);
        await page.getByRole("button", { name: "添加记忆", exact: true }).click();
        await page.getByLabel("记忆内容").fill("测试偏好：先给结论");
        assert.equal(await page.getByLabel("主题（可选）").isVisible(), false, "advanced fields should be collapsed");
        await page.screenshot({ path: join(output, `personal-memory-${width}-${theme}.png`) });
        layouts += 1;
        await page.getByText("记忆状态与排查", { exact: true }).click();
        const recovery = page.getByRole("note").filter({ hasText: "已隔离 2 项损坏的待整理内容" });
        await recovery.scrollIntoViewIfNeeded();
        assert.ok(await recovery.isVisible(), "isolated checkpoint recovery must remain understandable");
        const recoveryOverflow = await recovery.evaluate(element => element.scrollWidth > element.clientWidth + 1);
        assert.equal(recoveryOverflow, false, "checkpoint recovery instructions overflow the memory panel");
        const statusRows = await page.locator(".personal-memory__health > div > span").evaluateAll(elements => elements.map(element => {
          const rect = element.getBoundingClientRect(); return { top: rect.top, bottom: rect.bottom };
        }));
        assert.ok(statusRows.every((row, index) => index === 0 || row.top >= statusRows[index - 1].bottom), "memory status, completion date and pending count must remain separate readable rows");
        await page.screenshot({ path: join(output, `personal-memory-recovery-${width}-${theme}.png`) });
        layouts += 1;
        continue;
      }
      if (surface === "permission-picker") {
        await page.getByRole("button", { name: /请求批准/ }).click();
        const menu = page.getByRole("menu", { name: "本任务权限" });
        const assertPermissionLayout = async (label) => {
          const geometry = await menu.evaluate(element => {
          const box = element.getBoundingClientRect();
          const rows = [...element.querySelectorAll(".permission-picker__mode")].map(row => {
            const rect = row.getBoundingClientRect();
            return { height: rect.height, top: rect.top, bottom: rect.bottom };
          });
          return { height: box.height, width: box.width, left: box.left, right: box.right, scrollWidth: element.scrollWidth, clientWidth: element.clientWidth, rows };
        });
          assert.ok(geometry.height <= 401, `${label}: permission menu is too tall`);
          assert.ok(geometry.left >= 0 && geometry.right <= width, `${label}: permission menu clips horizontally`);
          assert.ok(geometry.scrollWidth <= geometry.clientWidth + 1, `${label}: permission menu scrolls horizontally`);
          assert.ok(geometry.rows.every(row => row.height < 130), `${label}: permission choices are stretched`);
          assert.ok(geometry.rows.every((row, index) => index === 0 || row.top - geometry.rows[index - 1].bottom < 9), `${label}: permission choices have large gaps`);
        };
        await assertPermissionLayout(`permission ${width} ${theme}`);
        await page.screenshot({ path: join(output, `permission-picker-${width}-${theme}.png`) });
        layouts += 1;
        await page.evaluate(() => { document.documentElement.style.fontSize = "22px"; });
        await assertPermissionLayout(`permission large font ${width} ${theme}`);
        await page.screenshot({ path: join(output, `permission-picker-large-font-${width}-${theme}.png`) });
        layouts += 1;
        await menu.getByRole("menuitemradio", { name: /本任务免审批/ }).click();
        const confirmation = menu.getByRole("alertdialog", { name: "确认本任务免审批" });
        await confirmation.waitFor();
        assert.ok(await menu.evaluate(element => element.scrollWidth <= element.clientWidth + 1), "permission confirmation scrolls horizontally");
        await confirmation.getByRole("button", { name: "取消" }).click();
        assert.equal(await confirmation.count(), 0);
        continue;
      }
      if (surface === "conversation") {
        await page.locator(".conversation-nav__directory:visible, .conversation-directory-mobile:visible").click();
        const dialog = page.getByRole("dialog", { name: "历史提问" });
        await dialog.getByRole("searchbox", { name: "筛选历史提问" }).waitFor();
        const box = await dialog.boundingBox();
        const toolbar = await page.locator(".session-header").boundingBox();
        assert.ok(box && toolbar && box.x >= 0 && box.x + box.width <= width && box.y >= toolbar.y + toolbar.height && box.y + box.height <= height, "question navigator clips or overlaps toolbar");
        assert.equal(await dialog.locator(".question-history__item").count(), 7);
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), "conversation: horizontal page overflow");
        await page.screenshot({ path: join(output, `${surface}-questions-${width}-${theme}.png`) });
        layouts += 1;
        await dialog.getByRole("searchbox", { name: "筛选历史提问" }).fill("蓝牙遥控器");
        assert.equal(await dialog.locator(".question-history__item").count(), 1);
        await dialog.locator(".question-history__item").click();
        await page.locator(".msg-wrap--jump-target", { hasText: "检查蓝牙遥控器" }).waitFor();
        assert.equal(await dialog.count(), 0);
        await page.getByRole("button", { name: "回到最新消息并恢复自动跟随" }).click();
        layouts += await validateOfficeExportMenu(page, output, { width, height, theme });
        continue;
      }
      if (surface === "meeting") {
        await page.getByRole("button", { name: /两者都录/ }).waitFor();
        await page.getByRole("button", { name: /两者都录/ }).click();
        assert.equal(await page.getByRole("button", { name: /两者都录/ }).getAttribute("aria-pressed"), "true");
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), "meeting: horizontal page overflow");
        const cards = await page.locator(".meeting-source__option").evaluateAll(elements => elements.map(element => {
          const box = element.getBoundingClientRect();
          return { left: box.left, right: box.right, width: box.width };
        }));
        assert.equal(cards.length, 3);
        assert.ok(cards.every(card => card.left >= 0 && card.right <= width && card.width >= 120), "meeting: source choices clip");
        await page.screenshot({ path: join(output, `${surface}-${width}-${theme}.png`) });
        layouts += 1;
        continue;
      }
      if (surface === "organization") {
        await page.locator(".org-memory__nav button", { hasText: "文档" }).click();
        await page.getByRole("heading", { name: /共享文档/ }).waitFor();
        const geometry = await page.evaluate(() => {
          const rect = (selector) => {
            const box = document.querySelector(selector).getBoundingClientRect();
            return { left: box.left, right: box.right, top: box.top, bottom: box.bottom };
          };
          return {
            title: rect(".org-memory__page-header > div:first-child"),
            actions: rect(".org-document-library__upload-actions"),
            sectionTitle: rect(".org-document-library__heading h2"),
            filter: rect(".org-document-library__filter"),
          };
        });
        assert.ok(geometry.title.right <= geometry.actions.left + 1 || geometry.title.bottom <= geometry.actions.top + 1, "document upload actions overlap heading");
        assert.ok(geometry.sectionTitle.right <= geometry.filter.left + 1 || geometry.sectionTitle.bottom <= geometry.filter.top + 1, "document scope filter overlaps section title");
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), "organization: horizontal page overflow");
        await page.screenshot({ path: join(output, `${surface}-${width}-${theme}.png`) });
        layouts += 1;
        await page.getByRole("button", { name: "上传文档" }).click();
        const uploadDialog = page.getByRole("dialog", { name: "选择上传位置" });
        const uploadBox = await uploadDialog.locator(".org-document-modal__confirm").boundingBox();
        assert.ok(uploadBox && uploadBox.x >= 0 && uploadBox.y >= 0 && uploadBox.x + uploadBox.width <= width && uploadBox.y + uploadBox.height <= height, "document upload dialog clips at viewport edge");
        await uploadDialog.getByRole("combobox", { name: "文档上传范围" }).waitFor();
        await page.screenshot({ path: join(output, `${surface}-upload-${width}-${theme}.png`) });
        layouts += 1;
        await uploadDialog.getByRole("button", { name: "取消" }).click();
        await page.locator(".org-memory__nav button", { hasText: "Skills" }).click();
        await page.getByText("暂无已发布的 Skill").waitFor();
        const skillsGeometry = await page.evaluate(() => {
          const summary = document.querySelector(".org-skill-library__summary").getBoundingClientRect();
          const actions = document.querySelector(".org-skill-library__actions").getBoundingClientRect();
          return { summary: { right: summary.right, bottom: summary.bottom }, actions: { left: actions.left, top: actions.top } };
        });
        assert.ok(skillsGeometry.summary.right <= skillsGeometry.actions.left + 1 || skillsGeometry.summary.bottom <= skillsGeometry.actions.top + 1, "organization Skills toolbar overlaps");
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), "organization Skills: horizontal page overflow");
        await page.screenshot({ path: join(output, `${surface}-skills-${width}-${theme}.png`) });
        layouts += 1;
        continue;
      }
      if (surface === "capabilities") await page.getByRole("heading", { name: "我的专家" }).waitFor();
      else if (surface === "coding") await page.getByRole("button", { name: "切换项目" }).waitFor();
      else await page.getByRole("dialog", { name: "设置" }).waitFor();
      await page.waitForFunction(() => !document.querySelector("select.form-control:disabled"));
      const controls = await page.locator("select.form-control").evaluateAll(elements => elements.map(element => ({ appearance: getComputedStyle(element).appearance, height: element.getBoundingClientRect().height })));
      for (const control of controls) { assert.equal(control.appearance, "none"); assert.ok(control.height >= 36, `${surface}: short select (${control.height}px)`); }
      if (surface === "memory") {
        assert.equal(await page.getByRole("switch").count(), 1);
        assert.equal(await page.getByRole("switch", { name: "使用记忆", exact: true }).isChecked(), true);
      }
      if (surface === "security") {
        await page.getByText("自定义工具规则", { exact: true }).click();
        await page.locator(".permission-rule-builder").scrollIntoViewIfNeeded();
        const fields = await page.locator(".permission-rule-builder .form-control").evaluateAll(elements => elements.map(element => element.getBoundingClientRect().height));
        assert.ok(Math.max(...fields) - Math.min(...fields) < 1, "permission controls have inconsistent heights");
      }
      if (surface === "weixin-connected") {
        await page.getByText("正在接收微信消息").waitFor();
        assert.equal(await page.getByRole("checkbox").count(), 2);
      }
      if (surface === "weixin-offline") {
        await page.getByText("3 条文字回复待发送，连接恢复后自动重试。").waitFor();
      }
      if (surface === "weixin-unconnected") {
        await page.getByRole("button", { name: "获取绑定二维码" }).click();
        await page.getByRole("img", { name: "微信绑定二维码" }).waitFor();
      }
      if (surface === "capabilities") {
        assert.equal(await page.getByRole("navigation", { name: "专家·技能·连接器管理" }).count(), 1);
        assert.equal(await page.getByRole("tablist", { name: "扩展分类" }).count(), 0);
        const template = await page.getByText("推荐模板", { exact: true }).boundingBox();
        assert.ok(template && template.y < height - 100, "templates pushed below first screen");
        assert.equal(await page.locator(".capability-page").evaluate(element => getComputedStyle(element).overflowY), "auto");
      }
      if (surface === "coding") {
        await page.getByRole("button", { name: "切换开发任务" }).click();
        const menu = page.getByRole("menu", { name: "开发任务" });
        const geometry = await menu.evaluate(element => {
          const box = element.getBoundingClientRect();
          const pane = document.querySelector(".echo-theia-agent").getBoundingClientRect();
          const row = element.querySelector(".coding-task-switcher__row").getBoundingClientRect();
          const hit = document.elementFromPoint(box.right - 12, row.top + row.height / 2);
          return { left: box.left, right: box.right, top: box.top, bottom: box.bottom,
            paneLeft: pane.left, portal: element.parentElement === document.body,
            rowVisible: element.contains(hit) };
        });
        assert.ok(geometry.portal && geometry.left >= geometry.paneLeft && geometry.right <= width
          && geometry.top >= 0 && geometry.bottom <= height && geometry.rowVisible,
        "coding task menu clips or hides behind the Agent pane");
        await page.screenshot({ path: join(output, `coding-task-menu-${width}-${theme}.png`) });
        layouts += 1;
        await page.keyboard.press("Escape");
      }
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `${surface}: horizontal page overflow`);
      await page.screenshot({ path: join(output, `${surface}-${width}-${theme}.png`) });
      layouts += 1;
      if (surface === "weixin-connected" || surface === "weixin-offline") {
        const disclosure = page.locator(".settings-disclosure").filter({has: page.getByText("单独交接的任务", {exact:true})});
        await disclosure.waitFor();
        if (!await disclosure.evaluate(element => element.open)) await disclosure.locator(":scope > summary").click();
        const sharedRow = page.locator(".weixin-channel__shared").last();
        if (await sharedRow.count()) await sharedRow.scrollIntoViewIfNeeded();
        await page.screenshot({ path: join(output, `${surface}-shared-${width}-${theme}.png`) });
        layouts += 1;
      }
      if (surface === "weixin-connected") {
        await page.getByRole("button", { name: "撤销 准备发布说明 的单独交接授权" }).click();
        await page.getByText("暂无单独交接的任务。").waitFor();
      }
      if (surface === "cloud-storage") {
        await page.getByRole("button", { name: "添加存储源" }).click();
        const fields = await page.locator(".storage-panel__field .form-control").evaluateAll(elements => elements.map(element => {
          const rect = element.getBoundingClientRect();
          return { left: rect.left, right: rect.right, height: rect.height };
        }));
        assert.equal(fields.length, 4);
        assert.ok(fields.every(field => field.left >= 0 && field.right <= width && field.height >= 36), "storage form clips fields");
        await page.screenshot({ path: join(output, `cloud-storage-form-${width}-${theme}.png`) });
        layouts += 1;
      }
    }
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("http://127.0.0.1:1439/__ui-review?surface=organization");
  await page.locator(".org-memory__nav button", { hasText: "文档" }).click();
  await page.locator(".org-document-library__row", { hasText: "指标体系模型设计模板.xlsx" }).getByRole("button", { name: "查看" }).click();
  await page.getByText("指标=活跃用户", { exact: false }).waitFor();
  assert.equal(await page.getByText(/预览需要文档解析器/).count(), 0);
  await page.screenshot({ path: join(output, "organization-xlsx-preview.png") });

  await page.goto("http://127.0.0.1:1439/__ui-review?surface=memory");
  await page.getByRole("switch", { name: "使用记忆", exact: true }).locator("..").click();
  await page.getByText("记忆配置已保存，立即用于后续提取和模型请求。").waitFor();
  assert.equal(await page.getByRole("switch", { name: "使用记忆", exact: true }).isChecked(), false);

  await page.goto("http://127.0.0.1:1439/__ui-review?surface=notify-channels");
  await page.getByRole("button", { name: /推送到其他应用/ }).click();
  await page.getByRole("button", { name: "添加渠道", exact: true }).click();
  await page.getByRole("textbox", { name: "通知渠道显示名" }).fill("测试渠道");
  await page.getByRole("textbox", { name: "Webhook URL" }).fill("https://example.com/webhook");
  await page.getByRole("button", { name: "+ 添加", exact: true }).click();
  await page.locator(".notify-panel__row-label", { hasText: "测试渠道" }).waitFor();

  await page.goto("http://127.0.0.1:1439/__ui-review?surface=cloud-storage");
  await page.getByRole("button", { name: "添加存储源" }).click();
  await page.getByRole("textbox", { name: "存储源显示名" }).fill("测试网盘");
  await page.getByRole("textbox", { name: "WebDAV 地址" }).fill("https://example.com/dav");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await page.getByRole("combobox", { name: "选择云存储源" }).selectOption({ label: "测试网盘" });
  await page.getByText("浏览云端文件", { exact: true }).click();
  await page.getByText("空目录", { exact: true }).waitFor();

  await page.goto("http://127.0.0.1:1439/__ui-review?surface=capabilities");
  await page.getByRole("button", { name: "创建专家" }).click();
  await page.getByRole("dialog", { name: "创建专家" }).waitFor();
  await page.getByRole("textbox", { name: "专家名称" }).fill("评审专家");
  await page.getByRole("textbox", { name: "专家描述" }).fill("检查产品与实现的一致性");
  await page.getByRole("textbox", { name: "专家指令" }).fill("仔细检查需求和代码，给出可验证的结论。");
  await page.getByRole("dialog", { name: "创建专家" }).getByRole("button", { name: "创建", exact: true }).click();
  await page.getByRole("button", { name: "查看专家 评审专家 详情" }).waitFor();
  for (const label of ["技能", "连接器", "插件", "专家"]) {
    await page.getByRole("navigation", { name: "专家·技能·连接器管理" }).getByRole("button", { name: label, exact: true }).click();
    await page.waitForFunction(() => !document.querySelector(".placeholder-page[role='status']"));

  }
  await page.getByRole("heading", { name: "我的专家" }).waitFor();
  layouts += await validateExtensionLayouts(page, output);

  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("http://127.0.0.1:1439/__ui-review?surface=expert-entry");
  await page.addStyleTag({ content: "*,*::before,*::after{animation:none!important;transition:none!important}" });
  await page.getByRole("button", { name: "专家·技能·连接器", exact: true }).click();
  await page.getByText("创建你的第一个专家，或选择下方模板开始").waitFor();
  const createEntry = await page.getByRole("button", { name: "创建专家" }).boundingBox();
  assert.ok(createEntry && createEntry.x >= 0 && createEntry.x + createEntry.width <= 1440, "expert creation entry clips viewport");
  await page.screenshot({ path: join(output, "expert-entry-empty-1440.png") });
  await page.getByRole("button", { name: "创建专家" }).click();
  const createDialog = page.getByRole("dialog", { name: "创建专家" });
  await createDialog.waitFor();
  assert.equal(await page.getByRole("navigation", { name: "专家·技能·连接器管理" }).count(), 1);
  await createDialog.getByRole("textbox", { name: "专家名称" }).fill("入口验证专家");
  await createDialog.getByRole("textbox", { name: "专家指令" }).fill("帮助验证专家创建入口。");
  await createDialog.getByRole("button", { name: "创建", exact: true }).click();
  await page.getByRole("button", { name: "查看专家 入口验证专家 详情" }).waitFor();
  await page.getByRole("button", { name: "返回对话" }).click();
  await page.getByRole("button", { name: "专家·技能·连接器", exact: true }).click();
  await page.getByRole("button", { name: "查看专家 入口验证专家 详情" }).waitFor();

  await page.setViewportSize({ width: 1024, height: 768 });
  await page.goto("http://127.0.0.1:1439/__ui-review?surface=memory");
  await page.getByRole("dialog", { name: "设置" }).waitFor();
  const normalLabelSize = await page.locator(".settings-navigation__label").first().evaluate(element => parseFloat(getComputedStyle(element).fontSize));
  await page.evaluate(() => { document.documentElement.style.fontSize = `${(16 * 18) / 13}px`; });
  const enlargedLabelSize = await page.locator(".settings-navigation__label").first().evaluate(element => parseFloat(getComputedStyle(element).fontSize));
  assert.ok(enlargedLabelSize > normalLabelSize * 1.3, "font preference does not scale settings text");
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), "large font causes horizontal overflow");
  await page.screenshot({ path: join(output, "memory-large-font-1024.png") });

  await page.goto("http://127.0.0.1:1439/__ui-review?surface=coding");
  await page.getByRole("button", { name: "切换项目" }).focus();
  await page.keyboard.press("ArrowDown");
  await page.getByRole("menu", { name: "项目列表" }).waitFor();
  await page.keyboard.press("Escape");
  assert.equal(await page.getByRole("menu", { name: "项目列表" }).count(), 0);
  await page.setViewportSize({ width: 1024, height: 768 });
  await page.goto("http://127.0.0.1:1439/__ui-review?surface=workspace-picker&theme=light");
  const workspaceTrigger = page.locator(".workspace-picker__trigger");
  await workspaceTrigger.hover();
  await page.getByRole("tooltip").waitFor();
  await workspaceTrigger.click();
  const workspaceMenu = page.getByRole("menu", { name: "选择工作目录" });
  await workspaceMenu.waitFor();
  await page.waitForFunction(() => document.activeElement?.matches('.workspace-picker__item[aria-checked="true"]'));
  await page.waitForTimeout(450);
  assert.equal(await page.getByRole("tooltip").count(), 0, "workspace menu must not retain an opening tooltip");
  await page.screenshot({ path: join(output, "workspace-picker-open.png") });
  await page.locator('.workspace-picker__item[title]').first().hover();
  assert.ok((await page.getByRole("tooltip").innerText({ timeout: 3000 })).includes("Taurus"), "hovering a workspace choice should still reveal its full path");
  await page.mouse.move(20, 20);
  await page.getByRole("tooltip").waitFor({ state: "hidden" });
  await page.keyboard.press("Escape");
  assert.equal(await page.getByRole("tooltip").count(), 0, "closing the menu should not revive its hint");
  await page.mouse.click(20, 20);
  await workspaceTrigger.focus();
  await page.getByRole("tooltip").waitFor();
  await page.keyboard.press("ArrowDown");
  await page.getByRole("menu", { name: "选择工作目录" }).waitFor();
  await page.waitForTimeout(450);
  assert.equal(await page.getByRole("tooltip").count(), 0, "keyboard opening must not cover workspace choices");
  layouts += 1;
  layouts += await validateConversationLayout(page, output);
  layouts += await validateSidebarScroll(page, output);
  layouts += await validateQueueUi(page, output);
  layouts += await validateAutomationEditorUi(page, output);
  await page.setViewportSize({ width: 1024, height: 720 });
  await page.goto("http://127.0.0.1:1439/__ui-review?surface=automation-edit");
  await page.locator("#automation-workspace-path").focus();
  assert.equal(await page.locator(".atm-workspace-input__dropdown").count(), 0, "selected workspace should not suggest itself on focus");
  const workspaceRow = await page.evaluate(() => {
    const input = document.querySelector("#automation-workspace-path").getBoundingClientRect();
    const browse = document.querySelector(".atm-workspace-input__browse").getBoundingClientRect();
    return { inputRight: input.right, browseLeft: browse.left, browseRight: browse.right, viewportWidth: innerWidth };
  });
  assert.ok(workspaceRow.inputRight < workspaceRow.browseLeft && workspaceRow.browseRight < workspaceRow.viewportWidth, "workspace picker should fit beside the path");
  const modelTrigger = page.locator(".atm-prompt-chip").filter({ hasText: /^Auto/ });
  await modelTrigger.click();
  assert.equal(await modelTrigger.getAttribute("aria-expanded"), "true", "automation model popup must expose expanded state");
  await page.waitForFunction(() => document.activeElement?.matches(".atm-chip-option"));
  await page.keyboard.press("Escape");
  assert.equal(await page.locator(".atm-chip-dropdown__menu").count(), 0, "Escape must dismiss automation model popup");
  assert.ok(await modelTrigger.evaluate(element => element === document.activeElement), "popup must restore keyboard focus");
  await modelTrigger.click();
  await page.keyboard.press("Tab");
  assert.equal(await page.locator(".atm-chip-dropdown__menu").count(), 0, "Tab leaving automation popup must dismiss it");
  await page.screenshot({ path: join(output, "automation-workspace-input.png") });
  layouts += 1;
  assert.deepEqual(errors, [], "browser runtime errors");
  console.log(JSON.stringify({ passed: true, screenshots: output, layouts, interactions: ["meeting source selection", "organization document preview", "conversation question navigation", "office export scope, keyboard navigation and font scaling", "memory", "notification", "storage", "expert creation", "expert entry and refresh", "extension navigation", "font scaling", "project keyboard navigation", "workspace menu tooltip dismissal", "cross-session scroll restoration", "inline background task summary", "sidebar font scaling and hover", "independent sidebar list scrolling"] }, null, 2));
} finally {
  await browser?.close();
  await server.close();
}
