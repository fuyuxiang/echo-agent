// Production knowledge/memory pages with native model installation mocked only in the fixture.
import {createUiReviewServer,reviewChromium,reviewOutput} from "./ui-review-runtime.mjs";
import {mkdtempSync} from "node:fs";
import {tmpdir} from "node:os";
import {join} from "node:path";
import assert from "node:assert/strict";
const output=reviewOutput(mkdtempSync(join(tmpdir(),"echo-local-retrieval-ui-")));
const server=await createUiReviewServer(0);let browser;
const base=`http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
async function openMemoryModels(page) {
  await page.getByRole("button",{name:"管理",exact:true}).click();
  await page.getByRole("button",{name:"更多",exact:true}).click();
  await page.getByRole("menuitem",{name:"记忆状态与排查",exact:true}).click();
}
try {
  browser=await (await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"});
  const page=await browser.newPage();const errors=[];page.on("pageerror",error=>errors.push(error.message));
  page.setDefaultNavigationTimeout(60000);
  page.setDefaultTimeout(60000);
  const go=query=>page.goto(`${base}?${query}`,{waitUntil:"commit"});
  let layouts=0;
  for (const surface of ["personal-memory","management-knowledge"]) {
    for (const [width,theme] of [[1024,"light"],[768,"dark"]]) {
      await page.setViewportSize({width,height:900});
      if (surface==="management-knowledge") {
        await go(`surface=${surface}&theme=${theme}&retrievalModels=ready`);
        const knowledge=page.getByRole("region",{name:"知识库",exact:true});
        await knowledge.getByRole("heading",{name:"还没有添加知识源"}).waitFor();
        assert.equal(await knowledge.getByRole("region",{name:"本地语义检索"}).count(),0);
        await knowledge.screenshot({path:join(output,`${surface}-${theme}-${width}.png`)});layouts++;
        continue;
      }
      for (const state of ["missing","downloading","verifying","error","ready"]) {
        await go(`surface=${surface}&theme=${theme}&retrievalModels=${state}`);
        await openMemoryModels(page);
        const card=page.getByRole("region",{name:"本地语义检索"});await card.waitFor();
        if (state==="ready") await page.getByText("本地语义检索已就绪").waitFor();
        if (state==="error") await card.getByRole("button",{name:"重试修复模型"}).waitFor();
        if (state==="downloading"||state==="verifying") await card.getByRole("button",{name:"暂停修复"}).waitFor();
        assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1),false,`${surface} ${state} overflows`);
        await card.screenshot({path:join(output,`${surface}-${state}-${theme}-${width}.png`)});layouts++;
        if (state==="ready") {
          assert.match(await card.innerText(), /客户端已内置模型，无需下载/);
          await card.getByText("模型维护", {exact:true}).click();
          await card.getByRole("button",{name:"检查并修复模型"}).waitFor();
          assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1),false,`${surface} maintenance overflows`);
          await card.screenshot({path:join(output,`${surface}-maintenance-${theme}-${width}.png`)});layouts++;
          await card.getByRole("button",{name:"检查并修复模型"}).click();
          await card.getByRole("button",{name:"暂停修复"}).waitFor();
        }
        if (state==="missing") {
          await card.getByRole("button",{name:"修复本地模型"}).click();
          await card.getByRole("button",{name:"暂停修复"}).waitFor();
          await card.getByRole("button",{name:"暂停修复"}).click();
          await card.getByRole("button",{name:"修复本地模型"}).waitFor();
        }
      }
      await go(`surface=${surface}&theme=${theme}&retrievalModels=ready&retrievalImportFailure=1&pickedFolder=/review/invalid-models`);
      await openMemoryModels(page);
      const card=page.getByRole("region",{name:"本地语义检索"});
      await card.getByText("本地语义检索已就绪").waitFor();
      await card.getByText("模型维护",{exact:true}).click();
      await card.getByRole("button",{name:"导入离线模型"}).click();
      await card.getByRole("alert").filter({hasText:"当前模型仍可使用"}).waitFor();
      assert.equal(await card.getByText("本地语义检索已就绪").isVisible(),true);
      assert.equal(await card.getByText("本地模型需要修复").count(),0);
      assert.equal(await card.evaluate(el=>el.scrollWidth>el.clientWidth+1),false);
      await card.screenshot({path:join(output,`${surface}-import-failed-${theme}-${width}.png`)});layouts++;
      await card.getByRole("button",{name:"检查并修复模型"}).click();
      await card.getByRole("button",{name:"暂停修复"}).waitFor();
      assert.equal(await card.getByRole("alert").count(),0);
    }
  }
  await page.setViewportSize({width:1024,height:680});
  await go("surface=management-knowledge&knowledgeSources=1&retrievalModels=missing");
  const knowledge=page.getByRole("region",{name:"知识库",exact:true});
  const search=knowledge.getByRole("textbox",{name:"搜索知识库",exact:true});
  const status=knowledge.getByRole("status",{name:"本地检索状态",exact:true});
  await status.getByText(/仍可使用关键词搜索/).waitFor();
  await search.fill("差旅");
  await knowledge.getByRole("button",{name:"打开知识条目：差旅政策",exact:true}).waitFor();
  await status.getByRole("button",{name:"模型维护",exact:true}).click();
  const settings=page.getByRole("dialog",{name:"设置",exact:true});
  await settings.getByRole("heading",{name:"记忆状态与排查",exact:true}).waitFor();
  await settings.getByRole("button",{name:"修复本地模型",exact:true}).click();
  await settings.getByRole("button",{name:"暂停修复",exact:true}).waitFor();
  await status.getByText(/正在准备本地检索模型/).waitFor();
  await page.evaluate(()=>{ window.__retrievalModelState="ready"; });
  await status.waitFor({state:"hidden"});
  await settings.getByRole("button",{name:"关闭设置",exact:true}).click();
  await settings.waitFor({state:"hidden"});
  assert.equal(await search.inputValue(),"差旅");
  assert.ok(await search.evaluate(element=>element===document.activeElement),"return focus to search when repair resolves the notice");
  assert.deepEqual(errors,[]);console.log(JSON.stringify({layouts,output,errors,flows:["knowledge keyword fallback and model maintenance"]}));
} finally {await browser?.close();await server.close();}
