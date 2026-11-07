// Production knowledge/memory pages with native model installation mocked only in the fixture.
import {createUiReviewServer,reviewChromium,reviewOutput} from "./ui-review-runtime.mjs";
import {mkdtempSync} from "node:fs";
import {tmpdir} from "node:os";
import {join} from "node:path";
import assert from "node:assert/strict";
const output=reviewOutput(mkdtempSync(join(tmpdir(),"echo-local-retrieval-ui-")));
const server=await createUiReviewServer();let browser;
try {
  browser=await (await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"});
  const page=await browser.newPage();const errors=[];page.on("pageerror",error=>errors.push(error.message));
  let layouts=0;
  for (const surface of ["personal-memory","management-knowledge"]) {
    for (const [width,theme] of [[1024,"light"],[768,"dark"]]) {
      await page.setViewportSize({width,height:900});
      for (const state of ["missing","downloading","verifying","error","ready"]) {
        await page.goto(`http://127.0.0.1:1439/__ui-review?surface=${surface}&theme=${theme}&retrievalModels=${state}`);
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
      await page.goto(`http://127.0.0.1:1439/__ui-review?surface=${surface}&theme=${theme}&retrievalModels=ready&retrievalImportFailure=1&pickedFolder=/review/invalid-models`);
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
  assert.deepEqual(errors,[]);console.log(JSON.stringify({layouts,output,errors}));
} finally {await browser?.close();await server.close();}
