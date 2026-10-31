import { openWorkspaceView } from "./session-review-navigation.mjs";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { validateWorkbenchTransitions } from "./validate-session-workbench-transitions.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-session-workbench-")));
console.log(`Session workbench evidence: ${output}`);
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=workbench`;
let browser;
const evidence = [];
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  const page = await browser.newPage();
  page.setDefaultNavigationTimeout(60000);
  const errors = []; page.on("pageerror", error => errors.push(error.message));
  const openView = label => openWorkspaceView(page, label);
  const sessionAction = async label => {
    await page.getByRole("button", { name: "任务操作", exact: true }).click();
    await page.getByRole("menuitem", { name: label, exact: true }).click();
  };
  for (const width of [1536, 1280, 900]) for (const theme of ["light", "dark"]) for (const platform of ["mac", "other"]) {
    await page.setViewportSize({width, height: 900});
    await page.goto(`${base}&theme=${theme}&platform=${platform}`);
    await page.locator(".session-header").waitFor();
    for (const collapsed of [false, true]) {
      if (collapsed) await page.getByRole("button", {name:"收起侧边栏",exact:true}).click();
      const geometry = await page.evaluate(() => {
        const rect = element => { const r = element.getBoundingClientRect(); return {x:r.x,y:r.y,right:r.right,bottom:r.bottom,width:r.width,height:r.height}; };
        const header = document.querySelector(".session-header");
        return { header: rect(header), scroll: rect(document.querySelector(".chatview__scroll")), buttons: [...header.querySelectorAll("button")].filter(e => getComputedStyle(e).display !== "none").map(e => ({label:e.getAttribute("aria-label"),...rect(e)})),
          overflow: document.documentElement.scrollWidth > innerWidth + 1, oldToolbars: document.querySelectorAll(".chatview__utility-bar").length, title: rect(document.querySelector(".main-topbar__title-area")) };
      });
      assert.equal(geometry.overflow, false, `horizontal overflow at ${width}`);
      assert.equal(geometry.oldToolbars, 0);
      assert.equal(geometry.header.height, 56);
      assert.ok(geometry.scroll.y >= geometry.header.bottom, "message history starts behind header");
      for (const button of geometry.buttons) assert.ok(button.x >= geometry.header.x && button.right <= geometry.header.right + 1, `header button clipped: ${JSON.stringify(button)}`);
      if (platform === "mac" && collapsed) assert.ok(geometry.buttons[0].x >= 84, "native traffic-light safe area");
      const before = geometry.header;
      await page.locator(".chatview__scroll").evaluate(e => { e.scrollTop = 100; e.dispatchEvent(new WheelEvent("wheel", {deltaY:-1})); });
      assert.deepEqual(await page.locator(".session-header").evaluate(e => { const r=e.getBoundingClientRect(); return {x:r.x,y:r.y,right:r.right,bottom:r.bottom,width:r.width,height:r.height}; }), before, "scroll moved session header");
      await page.screenshot({path:join(output, `workbench-${width}-${theme}-${platform}-${collapsed ? "collapsed" : "expanded"}.png`)});
      evidence.push({width,theme,platform,collapsed,...geometry});
    }
    await page.getByRole("button", {name:"新建任务",exact:true}).click();
    assert.ok(await page.evaluate(() => window.workbenchNewTask), "collapsed new-task path");
    await page.getByRole("button", {name:"任务操作",exact:true}).focus();
    await page.keyboard.press("ArrowDown");
    const menu = page.getByRole("menu", {name:"当前会话操作"});
    assert.ok(await menu.getByRole("menuitem").first().evaluate(e=>e===document.activeElement));
    await page.keyboard.press("End"); await page.keyboard.press("Escape");
    assert.ok(await page.getByRole("button", {name:"任务操作",exact:true}).evaluate(e=>e===document.activeElement));
  }
  await page.setViewportSize({width:1280,height:900});
  await page.goto(`${base}&theme=light&platform=mac`);
  await page.getByRole("button",{name:"任务操作",exact:true}).click();
  const taskMenu = page.getByRole("menu",{name:"当前会话操作",exact:true});
  assert.deepEqual(await taskMenu.getByRole("menuitem").evaluateAll(items => items.map(item => item.getAttribute("aria-label") ?? item.textContent)), ["查找当前会话","回溯…","分叉…","导出整个会话…"]);
  await page.screenshot({path:join(output,"current-conversation-tools.png")});
  await page.keyboard.press("Escape");
  await page.getByRole("button",{name:"编辑标题",exact:true}).click();
  await page.getByRole("textbox", {name:"会话标题"}).fill("验收后的任务标题");
  await page.getByRole("textbox", {name:"会话标题"}).press("Enter");
  await page.getByText("验收后的任务标题",{exact:true}).first().waitFor();
  await page.locator('.sidebar__conv[data-session-id="review-conversation"]').hover();
  await page.getByRole("button",{name:"验收后的任务标题的会话操作",exact:true}).click();
  const sessionMenu = page.getByRole("menu",{name:"验收后的任务标题 会话操作",exact:true});
  for (const name of ["重命名","取消置顶","归档","在微信继续"]) {
    assert.equal(await sessionMenu.getByRole("menuitem",{name,exact:true}).count(),1);
  }
  await page.screenshot({path:join(output,"sidebar-session-management.png")});
  await sessionMenu.getByRole("menuitem",{name:"取消置顶",exact:true}).click();
  await page.waitForFunction(() => window.workbenchCalls.some(c => c.command === "agent_set_session_pinned" && c.args.sessionId === "review-conversation" && c.args.pinned === false));
  await sessionAction("导出整个会话…");
  const share = page.getByRole("dialog",{name:"分享选项"});
  await share.getByRole("button",{name:"导出 Markdown",exact:true}).click();
  await page.waitForFunction(() => window.workbenchCalls.some(c=>c.command==="export_text_file" && c.args.content.includes("评审投影仪")));
  await page.keyboard.press("ControlOrMeta+f");
  await page.getByRole("textbox",{name:"查找",exact:true}).fill("服务端");
  await page.locator(".find-hit-active").waitFor();
  await page.keyboard.press("Escape");
  await openView("网页预览");
  await page.getByRole("complementary",{name:"工作区面板"}).waitFor();
  await openView("文件变更");
  assert.equal(await page.getByRole("complementary",{name:"工作区面板"}).count(),1,"one shared details container");
  await page.getByRole("button",{name:"关闭面板",exact:true}).click();
  await page.locator(".conversation-nav__directory:visible, .conversation-directory-mobile:visible").click();
  await page.getByRole("searchbox",{name:"筛选历史提问"}).fill("蓝牙");
  await page.locator(".question-history__item").click();
  await page.locator(".msg-wrap--jump-target").waitFor();
  await sessionAction("回溯…");
  await page.getByRole("button",{name:"仅文件",exact:true}).click();
  await page.getByRole("button",{name:/回溯到此处/}).click();
  assert.equal(await page.evaluate(()=>window.workbenchCalls.filter(c=>c.command==="rewind_execute").length),0,"rewind must await confirmation");
  await page.getByRole("button",{name:"取消",exact:true}).click();
  await page.keyboard.press("Escape");
  for (const state of ["empty","files","plan","running","permission","collaboration","paused","failed"]) {
    await page.goto(`${base}&theme=light&platform=mac&state=${state}`);
    await page.locator(".session-header").waitFor();
    if (state === "empty") { assert.equal(await page.getByRole("button",{name:"查找当前会话"}).count(),0); assert.equal(await page.getByRole("button",{name:"任务操作",exact:true}).count(),0); }
    if (state === "files") { await openView("文件变更"); await page.getByRole("region",{name:"文件变更",exact:true}).waitFor(); }
    if (state === "plan") { await openView("执行计划"); await page.getByText("验证工作台交互与交付",{exact:true}).waitFor(); }
    if (state === "collaboration") { await openView("运行与协作"); await page.getByText("核对交付文档",{exact:true}).waitFor(); }
    if (state === "permission") {
      await page.getByRole("button",{name:"允许一次",exact:true}).waitFor();
      assert.equal(await page.locator(".session-header [role=status]").count(), 0);
      assert.equal(await page.locator(".tasks-panel").count(), 0, "background details must not cover decisions");
      await page.getByRole("button", {name:/后台任务/}).waitFor();
      const allow = page.getByRole("button",{name:"允许一次",exact:true});
      await allow.click({trial:true});
      await page.getByRole("button",{name:"拒绝本次",exact:true}).click({trial:true});
    }
    if (state === "running") {
      await page.getByRole("button",{name:"停止生成",exact:true}).waitFor();
      assert.equal(await page.locator(".session-header [role=status]").count(),0,"routine activity stays with the message and composer");
      await page.locator(".msg__loading-main").waitFor();
      assert.equal(await page.getByRole("group",{name:"当前轮次执行控制",exact:true}).count(),0);
      assert.equal(await page.getByText("正在执行，可继续补充要求",{exact:true}).count(),0);
      assert.equal(await page.getByRole("button",{name:"暂停",exact:true}).count(),0);
      assert.equal(await page.getByRole("button",{name:"停止",exact:true}).count(),0);
    }
    if (state === "paused") await page.getByText("任务已暂停，可继续执行",{exact:true}).waitFor();
    if (state === "failed") await page.getByRole("alert").filter({hasText:"本轮任务失败"}).waitFor();
    await page.screenshot({path:join(output,`state-${state}.png`)});
  }
  for (const [width,font] of [[1440,16],[900,16],[390,16],[320,16],[390,20],[320,20]]) for (const theme of ["light","dark"]) {
    await page.setViewportSize({width,height:900});
    await page.goto(`${base}&theme=${theme}&platform=mac&state=running&composerModel=long`);
    await page.locator(".session-header").waitFor();
    await page.evaluate(font => { document.documentElement.style.fontSize=`${font}px`; },font);
    if (width < 720) await page.getByRole("button",{name:"收起侧边栏",exact:true}).click();
    const stop = page.getByRole("button",{name:"停止生成",exact:true});
    assert.equal(await stop.count(),1,"only the composer should expose a stop button");
    assert.equal(await page.getByRole("button",{name:"发送选项",exact:true}).count(),0);
    const input = page.locator(".echo-composer__input");
    await input.fill("请保留这条补充要求");
    assert.equal(await stop.count(),0,"a draft changes the primary action to queue");
    assert.equal(await page.locator(".echo-composer__send").count(),1);
    await page.getByRole("button",{name:"加入待发送队列",exact:true}).click({trial:true});
    await page.getByRole("button",{name:"任务操作",exact:true}).click();
    await page.getByRole("menuitem",{name:"停止当前执行",exact:true}).click({trial:true});
    await page.keyboard.press("Escape");
    const geometry = await page.locator(".echo-composer__footer").evaluate(element => {
      const r=element.getBoundingClientRect();
      return {overflow:document.documentElement.scrollWidth>innerWidth+1,
        clipped:[...element.querySelectorAll("button")].filter(button=>button.getClientRects().length).some(button=>{
          const b=button.getBoundingClientRect(); return b.x<r.x-1 || b.right>r.right+1;
        })};
    });
    assert.equal(geometry.overflow,false,`draft overflow at ${width} ${theme}`);
    assert.equal(geometry.clipped,false,`composer controls clipped at ${width} ${theme}`);
    await page.screenshot({path:join(output,`running-draft-${width}-${theme}-${font}.png`)});
    await page.getByRole("button",{name:"任务操作",exact:true}).click();
    await page.getByRole("menuitem",{name:"停止当前执行",exact:true}).click();
    await page.getByText("已停止（发送新消息可继续此任务）",{exact:true}).waitFor();
    assert.equal(await input.inputValue(),"请保留这条补充要求","stopping must preserve the draft");
    assert.equal(await page.evaluate(()=>window.workbenchCalls.some(c=>c.command==="review_send" || c.command==="review_send_now")),false);
    await page.goto(`${base}&theme=${theme}&platform=mac&state=running&composerModel=long`);
    await page.locator(".session-header").waitFor();
    await page.evaluate(font => { document.documentElement.style.fontSize=`${font}px`; },font);
    if (width < 720) await page.getByRole("button",{name:"收起侧边栏",exact:true}).click();
    await page.evaluate(async () => {
      const {useMessageQueueStore}=await import("/src/stores/message-queue-store.ts");
      useMessageQueueStore.getState().clear("review-conversation");
    });
    await input.fill("补充一份摘要");
    await page.getByRole("button",{name:"加入待发送队列",exact:true}).dblclick();
    await stop.waitFor();
    const queue = page.getByRole("list",{name:"待发送队列"});
    assert.equal(await queue.locator(".queue-row").count(),1,"double-click admits one row without stopping");
    assert.equal(await page.evaluate(()=>window.workbenchCalls.filter(c=>c.command==="review_cancel" || c.command==="review_send_now").length),0);
    assert.equal(await input.inputValue(),"");
    await input.fill("优先检查数据");
    await input.press("Enter");
    assert.equal(await queue.locator(".queue-row").count(),2);
    await queue.getByRole("button",{name:"中断当前回复并立即发送这条",exact:true}).nth(1).click();
    await page.waitForFunction(()=>window.workbenchCalls.filter(c=>c.command==="review_send_now").length===1);
    assert.deepEqual(await page.evaluate(()=>window.workbenchCalls.filter(c=>c.command==="review_send_now").map(c=>c.args.text)),["优先检查数据"]);
    await page.screenshot({path:join(output,`queued-followups-${width}-${theme}-${font}.png`)});
    await stop.click();
    await page.getByText("队列已暂停，恢复条目后可发送",{exact:true}).waitFor();
    assert.equal(await queue.locator(".queue-row").count(),2,"stopping preserves waiting follow-ups");
    assert.equal(await queue.getByRole("button",{name:"恢复",exact:true}).count(),2);

  }
  // Narrow-window cases persist panel widths; verify default-layout transitions
  // in a fresh context so those preferences cannot contaminate the fixture.
  const transitionPage = await browser.newPage();
  transitionPage.setDefaultNavigationTimeout(60000);
  transitionPage.on("pageerror", error => errors.push(error.message));
  try { await validateWorkbenchTransitions(transitionPage, base, output); }
  finally { await transitionPage.close(); }
  assert.deepEqual(errors,[]);
  writeFileSync(join(output,"geometry.json"),JSON.stringify(evidence,null,2));
  console.log(`Passed ${evidence.length} header layouts, navigation, native-export bridge, find/history, rewind cancellation, mutually exclusive workspaces, 8 task states and 12 running-draft/queued-follow-up control layouts.`);
} finally { await browser?.close(); await server.close(); }
