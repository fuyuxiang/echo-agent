import { openWorkspaceView } from "./session-review-navigation.mjs";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-session-redesign-")));
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=workbench&platform=mac`;
const evidence = [];
let browser;
try {
  browser = await (await reviewChromium()).launch({headless:true, executablePath:process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"});
  const page = await browser.newPage();
  page.setDefaultNavigationTimeout(60000);
  const errors = []; page.on("pageerror", error => errors.push(error.message));
  const openArtifacts = () => openWorkspaceView(page, "成果");
  // Identical states, content, scale and window sizes to the pre-change capture.
  for (const [name,state,width,height,theme,font] of [
    ["conversation","complete",1280,800,"light",16], ["running","running",1280,800,"light",16],
    ["paused","paused",1280,800,"light",16], ["permission","permission",1280,800,"light",16],
    ["artifacts","files",1280,800,"light",16], ["narrow-large","files",1024,680,"light",20],
    ["dark","files",1280,800,"dark",16], ["wide","files",1536,900,"light",16],
  ]) {
    await page.setViewportSize({width,height});
    await page.goto(`${base}&state=${state}&theme=${theme}`);
    await page.locator(".session-header").waitFor();
    if (font !== 16) await page.evaluate(size => document.documentElement.style.fontSize=`${size}px`,font);
    if (state === "files") await openArtifacts();
    const geometry = await page.evaluate(() => {
      const panel=document.querySelector(".task-details"), list=document.querySelector(".task-details__list--split"), preview=document.querySelector(".task-details__body");
      const composer=document.querySelector(".echo-composer");
      return {overflow:document.documentElement.scrollWidth>innerWidth+1,header:document.querySelector(".session-header").getBoundingClientRect().height,
        list:list?.getBoundingClientRect().width,preview:preview?.clientWidth,panel:panel?.clientWidth, tools:composer?.scrollWidth>composer?.clientWidth+1};
    });
    assert.equal(geometry.overflow,false,`${name} document overflow`);
    assert.equal(geometry.header,56);
    assert.equal(geometry.tools,false,`${name} composer overflow`);
    if (geometry.list) {assert(geometry.preview>=360,`${name} unreadable split`); assert(geometry.list>=180 && geometry.list<=220);}
    const contrast = await page.evaluate(() => {
      const main=document.querySelector(".app__main");
      const probe=document.createElement("a");
      probe.style.color="var(--md-link)"; main.append(probe);
      const rgb=value=>value.match(/[\d.]+/g).map(Number);
      const composite=(front,back)=>front.slice(0,3).map((v,i)=>v*(front[3]??1)+back[i]*(1-(front[3]??1)));
      let background=[255,255,255];
      const chain=[]; for(let node=probe.parentElement;node;node=node.parentElement) chain.unshift(node);
      for(const node of chain) background=composite(rgb(getComputedStyle(node).backgroundColor),background);
      const foreground=composite(rgb(getComputedStyle(probe).color),background);
      const luminance=values=>values.map(v=>v/255).map(v=>v<=.04045?v/12.92:((v+.055)/1.055)**2.4).reduce((n,v,i)=>n+v*[.2126,.7152,.0722][i],0);
      const l1=luminance(foreground),l2=luminance(background); probe.remove();
      return {ratio:(Math.max(l1,l2)+.05)/(Math.min(l1,l2)+.05),foreground,background};
    });
    assert(contrast.ratio>=4.5,`${name} link contrast ${contrast.ratio}`);
    await page.screenshot({path:join(output,`${name}.png`)});
    evidence.push({name,state,width,height,theme,font,geometry,contrast});
    if (state === "files") {
      const panel=page.getByRole("complementary",{name:"工作区面板"});
      await panel.getByRole("button",{name:/^report\.md/}).click();
      await page.getByRole("button",{name:"查看关联工具",exact:true}).waitFor();
      await page.screenshot({path:join(output,`${name}-preview.png`)});
      if (width===1280) {
        assert.equal(await page.locator(".task-details__list:visible").count(),0,"default panel must show only the preview");
        await page.getByRole("button",{name:"返回成果列表",exact:true}).click();
        await panel.getByRole("button",{name:/^report\.md/}).waitFor();
        await panel.getByRole("button",{name:/^report\.md/}).click();
        await page.getByRole("button",{name:"查看关联工具",exact:true}).waitFor();
        await openWorkspaceView(page, "文件");
        await panel.locator(".task-details__list").waitFor();
        assert.equal(await panel.locator(".artifact-file-preview").count(),0,"header navigation must show the file list");
        await openArtifacts();
        await panel.getByRole("button",{name:/^report\.md/}).click();
        await panel.getByRole("tab",{name:"文件",exact:true}).click();
        await panel.locator(".task-details__list").waitFor();
        assert.equal(await panel.locator(".artifact-file-preview").count(),0,"view navigation must show the requested list");
        await panel.getByRole("tab",{name:"成果",exact:true}).click();
        await panel.getByRole("button",{name:/^report\.md/}).waitFor();
      }
    }
    if (state === "permission") {
      await page.getByRole("button",{name:"允许一次",exact:true}).click({trial:true});
      assert(await page.locator(".echo-composer__input").isDisabled(),"ordinary Enter cannot bypass a decision");
      assert.equal(await page.locator(".tasks-panel").count(),0);
    }
  }
  // Exercise production callbacks explicitly; the fixture records and accepts/rejects,
  // but does not call a native model or fabricate a real runtime completion.
  await page.goto(`${base}&state=running&rejectSend=1`);
  const input=page.locator(".echo-composer__input");
  await input.fill("下一步核验"); await input.press("Enter");
  await page.waitForFunction(async () => {
    const {useMessageQueueStore}=await import("/src/stores/message-queue-store.ts");
    return useMessageQueueStore.getState().getQueue("review-conversation").some(item=>item.text==="下一步核验");
  });
  assert.equal(await page.evaluate(()=>window.workbenchCalls.filter(c=>c.command==="review_send_now").length),0);
  await input.fill("中断后改为生成报告");
  await page.getByRole("button",{name:"加入待发送队列",exact:true}).click();
  const queuedReplacement=page.getByRole("list",{name:"待发送队列"}).locator(".queue-row").filter({hasText:"中断后改为生成报告"});
  await queuedReplacement.getByRole("button",{name:"中断当前回复并立即发送这条",exact:true}).click();
  await page.waitForFunction(()=>window.workbenchCalls.filter(c=>c.command==="review_send_now").length===1);
  assert.equal(await input.inputValue(),"");
  assert.equal(await queuedReplacement.count(),1,"rejected immediate send retains the queued message");
  await page.screenshot({path:join(output,"running-draft.png")});
  await page.goto(`${base}&state=paused`);
  await input.fill("暂存的补充要求");
  await page.getByRole("button",{name:"继续任务",exact:true}).click();
  assert.equal(await input.inputValue(),"暂存的补充要求");
  assert.deepEqual(await page.evaluate(()=>window.workbenchCalls.filter(c=>c.command==="review_send").map(c=>c.args.text)),["继续"]);

  // File selection, tabs and transcript drafts belong to their own session.
  await page.goto(`${base}&state=files`); await openArtifacts();
  await page.getByRole("complementary",{name:"工作区面板"}).getByRole("button",{name:/^report\.md/}).click();
  await input.fill("A 的草稿");
  await page.evaluate(async () => {const {useSessionStore}=await import("/src/stores/session-store.ts"); useSessionStore.getState().setSession("review-b");});
  await page.getByRole("button",{name:"关闭标签 report.md",exact:true}).waitFor({state:"hidden"});
  assert.equal(await input.inputValue(),"");
  await page.evaluate(async () => {const {useSessionStore}=await import("/src/stores/session-store.ts"); useSessionStore.getState().setSession("review-conversation");});
  await page.getByRole("button",{name:"关闭标签 report.md",exact:true}).waitFor();
  assert.equal(await input.inputValue(),"A 的草稿");
  assert.deepEqual(errors,[]);
  writeFileSync(join(output,"evidence.json"),JSON.stringify(evidence,null,2));
  console.log(`Passed ${evidence.length} screenshot states, readable previews, queue/interruption, continuation and A/B isolation; evidence: ${output}`);
} finally {await browser?.close(); await server.close();}
