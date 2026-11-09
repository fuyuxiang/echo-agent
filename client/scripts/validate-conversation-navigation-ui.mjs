import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
const output = reviewOutput(mkdtempSync(join(tmpdir(),"echo-conversation-navigation-")));
console.log(`Conversation UI evidence: ${output}`);
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=workbench&platform=mac`;
let browser;
const evidence=[];
try {
  browser = await (await reviewChromium()).launch({headless:true,channel:"chrome"});
  const page = await browser.newPage();
  page.setDefaultTimeout(15000); page.setDefaultNavigationTimeout(60000);
  const errors=[]; page.on("pageerror",e=>errors.push(e.message));
  const menu = async name => {await page.getByRole("button",{name:"任务操作",exact:true}).click();await page.getByRole("menuitem",{name,exact:true}).click();};
  const category = async name => {if(await page.getByRole("button",{name:"展开工作区",exact:true}).count()) await page.getByRole("button",{name:"展开工作区",exact:true}).click();await page.getByRole("tab",{name,exact:true}).click();};
  for(const [width,height,theme] of [[1536,900,"light"],[1280,800,"light"],[1024,768,"dark"],[768,800,"light"],[390,844,"dark"],[320,700,"light"]]) {
    await page.setViewportSize({width,height}); await page.goto(`${base}&state=files&theme=${theme}`);
    await page.getByRole("button",{name:"展开工作区",exact:true}).waitFor();
    if (width < 720) await page.getByRole("button",{name:"收起侧边栏",exact:true}).click();
    const geometry=await page.evaluate(()=>{
      const rect=e=>{const r=e.getBoundingClientRect();return {x:r.x,right:r.right,width:r.width};};
      const header=document.querySelector(".session-header"), composer=document.querySelector(".echo-composer");
      return {overflow:document.documentElement.scrollWidth>innerWidth+1,headerHeight:header.getBoundingClientRect().height,
        actions:[...document.querySelectorAll(".session-header__actions button")].map(e=>({label:e.getAttribute("aria-label"),...rect(e)})),
        composerOverflow:composer.scrollWidth>composer.clientWidth+1};
    });
    console.log(JSON.stringify({width,theme,geometry}));
    await page.screenshot({path:join(output,`${width}-${theme}-chat.png`)});
    assert.equal(geometry.overflow,false); assert.equal(geometry.headerHeight,52); assert.equal(geometry.composerOverflow,false);
    assert.deepEqual(geometry.actions.map(b=>b.label),["任务操作","展开工作区"]);
    for(const b of geometry.actions) assert(b.x>=0 && b.right<=width+1);
    await page.screenshot({path:join(output,`${width}-${theme}-chat.png`)});
    if (width >= 1280) {
      const before = await page.locator(".chatview__scroll").evaluate(e => e.scrollTop);
      await page.locator(".conversation-nav__mark").first().hover();
      const preview = page.locator(".conversation-nav__preview"); await preview.waitFor();
      assert.equal(await page.locator(".chatview__scroll").evaluate(e => e.scrollTop),before,"hover must not move the transcript");
      const action = preview.getByRole("button",{name:"查看该轮",exact:true});
      const bounds = await action.boundingBox();
      await page.mouse.move(bounds.x+bounds.width/2,bounds.y+bounds.height/2,{steps:30});
      assert(await preview.isVisible(),"mouse travel from a mark to its preview must keep the card open");
      assert.equal(await page.locator(".chatview__scroll").evaluate(e => e.scrollTop),before);
      await page.screenshot({path:join(output,`${width}-round-preview.png`)});
      await action.click();
      await page.locator(".msg-wrap--jump-target").waitFor();
      await page.getByRole("button",{name:"回到最新消息并恢复自动跟随"}).click();
    }
    await page.locator(".conversation-nav__directory:visible, .conversation-directory-mobile:visible").click();
    const history=page.getByRole("dialog",{name:"历史提问"});
    assert(await history.getByRole("button").count()>1);
    await history.locator(".question-history__item").last().click();
    await page.locator(".msg-wrap--jump-target").waitFor();
    await page.getByRole("button",{name:"回到最新消息并恢复自动跟随"}).click();
    await menu("查找当前会话"); await page.getByRole("textbox",{name:"查找",exact:true}).fill("蓝牙"); await page.locator(".find-hit-active").first().waitFor(); await page.keyboard.press("Escape");
    await category("成果");
    const panel=page.getByRole("complementary",{name:"工作区面板"});
    assert.deepEqual(await panel.getByRole("tablist",{name:"工作区分类"}).getByRole("tab").allTextContents(),["成果","文件","变更","任务","网页"]);
    await panel.getByRole("button",{name:/^report\.md/}).click();
    await page.getByRole("button",{name:"查看关联工具",exact:true}).waitFor();
    await panel.getByRole("button",{name:"关闭标签 report.md",exact:true}).click();
    assert.equal(await page.locator(".artifact-file-preview:visible").count(),0);
    await panel.getByRole("button",{name:/^report\.md/}).click();
    await page.getByRole("button",{name:"查看关联工具",exact:true}).waitFor();
    await page.getByRole("button",{name:"查看关联工具",exact:true}).click();
    await page.getByText("操作详情",{exact:true}).waitFor();
    await page.getByRole("button",{name:"返回上一视图",exact:true}).click();
    await page.getByRole("button",{name:"查看关联工具",exact:true}).waitFor();
    await category("网页");
    const url=page.getByRole("textbox",{name:"预览网址"}); await url.fill("https://example.com/draft");
    await category("变更"); await category("网页"); assert.equal(await url.inputValue(),"https://example.com/draft");
    await page.getByRole("button",{name:"收起工作区",exact:true}).click();
    await page.getByRole("button",{name:"展开工作区",exact:true}).click();
    await url.waitFor(); assert.equal(await url.inputValue(),"https://example.com/draft");
    const panelGeometry=await panel.evaluate(e=>({width:e.clientWidth,overflow:e.scrollWidth>e.clientWidth+1}));
    assert.equal(panelGeometry.overflow,false);
    await page.screenshot({path:join(output,`${width}-${theme}-workspace.png`)});
    evidence.push({width,height,theme,geometry,panelGeometry});
  }
  // Use the production tree with a nested native directory response; the base
  // empty-tree fixture cannot detect lost expansion or generated-file identity.
  await page.setViewportSize({width:1280,height:800});
  await page.goto(`${base}&state=files`);
  await page.getByRole("button",{name:"展开工作区",exact:true}).waitFor();
  await page.evaluate(() => {
    const native = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command,args) => {
      if (command !== "list_dir") return native(command,args);
      const entry = (name,path,kind) => ({name,path,kind,size:1,modifiedAt:0,isLarge:false,isBinary:false});
      if (args.path.endsWith("/src")) return Array.from({length:40},(_,index) => entry(`nested-${index}.md`,`${args.path}/nested-${index}.md`,"file"));
      return [entry("src",`${args.path}/src`,"directory"),entry("report.md","/review/EchoAgent/report.md","file")];
    };
  });
  await category("文件");
  const directory = page.locator(".file-tree__node--dir");
  await directory.click();
  const nested = page.getByRole("treeitem",{name:/nested-35\.md/});
  await nested.scrollIntoViewIfNeeded();
  const treeScroll = await page.locator(".task-details__list").evaluate(element=>element.scrollTop);
  assert(treeScroll>0,"the tree regression must exercise a scrolled directory");
  await nested.click();
  await page.locator(".artifact-file-preview").waitFor();
  await page.getByRole("button",{name:"返回文件列表",exact:true}).click();
  assert.equal(await directory.getAttribute("aria-expanded"),"true");
  assert.equal(await page.locator(".task-details__list").evaluate(element=>element.scrollTop),treeScroll);
  assert(await nested.isVisible());
  await category("网页"); await category("文件");
  assert.equal(await directory.getAttribute("aria-expanded"),"true");
  assert.equal(await page.locator(".task-details__list").evaluate(element=>element.scrollTop),treeScroll);
  await page.getByRole("treeitem",{name:/report\.md/}).click();
  await page.locator(".artifact-file-preview").waitFor();
  const contentTabs = page.getByRole("tablist",{name:"工作区文件和预览标签"});
  const generatedTab = contentTabs.getByRole("tab",{name:/report\.md/});
  assert.equal(await generatedTab.getAttribute("aria-selected"),"true");
  await generatedTab.focus(); await page.keyboard.press("ArrowLeft");
  assert.equal(await contentTabs.getByRole("tab",{name:/nested-35\.md/}).getAttribute("aria-selected"),"true");
  await page.screenshot({path:join(output,"nested-files-return-and-tabs.png")});
  await page.getByRole("button",{name:"返回文件列表",exact:true}).click();
  const savedTreeScroll = await page.locator(".task-details__list").evaluate(element=>element.scrollTop);
  await page.locator('.sidebar__conv[data-session-id="review-other"] .sidebar__conv-select').click();
  await page.locator('.sidebar__conv[data-session-id="review-conversation"] .sidebar__conv-select').click();
  await nested.waitFor();
  await page.waitForFunction(expected => Math.abs(document.querySelector(".task-details__list").scrollTop-expected)<1,savedTreeScroll);
  assert.equal(await directory.getAttribute("aria-expanded"),"true","returning to a session must restore expanded folders");
  await page.screenshot({path:join(output,"nested-files-session-restored.png")});
  await page.setViewportSize({width:1280,height:800}); await page.goto(`${base}&state=plan`);
  const planShortcut = page.getByRole("button",{name:"查看执行计划，已完成 1/2 步",exact:true});
  await planShortcut.click();
  assert.equal(await page.getByRole("tab",{name:"计划",exact:true}).getAttribute("aria-selected"),"true");
  assert.equal(await planShortcut.getAttribute("aria-expanded"),"true");
  const step=page.getByRole("textbox",{name:"新增步骤",exact:true}); await step.fill("尚未保存的计划草稿");
  await page.getByRole("tab",{name:"团队",exact:true}).click(); await page.getByRole("tab",{name:"计划",exact:true}).click(); assert.equal(await step.inputValue(),"尚未保存的计划草稿");
  await category("文件"); await category("任务"); assert.equal(await step.inputValue(),"尚未保存的计划草稿");
  await page.getByRole("button",{name:"收起工作区",exact:true}).click(); await page.getByRole("button",{name:"展开工作区",exact:true}).click(); assert.equal(await step.inputValue(),"尚未保存的计划草稿");
  if (await page.getByRole("navigation",{name:"当前会话轮次导航"}).isVisible()) {
    const mark=page.locator(".conversation-nav__mark").first(); await mark.hover(); await page.locator(".conversation-nav__preview").waitFor();
    await page.screenshot({path:join(output,"round-preview.png")});
    await mark.click(); await page.locator(".msg-wrap--jump-target").waitFor();
  }
  const touch = await browser.newPage({viewport:{width:1024,height:768},hasTouch:true});
  await touch.emulateMedia({reducedMotion:"reduce"}); await touch.goto(`${base}&state=complete`);
  await touch.locator(".conversation-directory-mobile").waitFor();
  assert.equal(await touch.getByRole("navigation",{name:"当前会话轮次导航"}).isVisible(),false);
  await touch.locator(".conversation-directory-mobile").click(); await touch.getByRole("dialog",{name:"历史提问"}).waitFor();
  await touch.keyboard.press("Escape");
  const touchTargets = await touch.locator(".msg__actions--latest .msg__action-btn").evaluateAll(items=>items.map(item=>({width:item.getBoundingClientRect().width,height:item.getBoundingClientRect().height})));
  assert(touchTargets.every(item=>item.width>=36 && item.height>=36));
  await touch.screenshot({path:join(output,"touch-reduced-motion.png")}); await touch.close();
  assert.deepEqual(errors,[]);
  writeFileSync(join(output,"evidence.json"),JSON.stringify({evidence,errors},null,2));
  console.log(JSON.stringify({ok:true,output,layouts:evidence.length,errors},null,2));
} finally {await browser?.close();await server.close();}
