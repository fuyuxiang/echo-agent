import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { inspectRenderedColors } from "./ui-color-audit.mjs";

const baseline = process.argv.includes("--baseline");
const output = reviewOutput(`/private/tmp/echo-product-${baseline ? "before" : "after"}`);
const baselineRevision = process.env.UI_REVIEW_BASELINE_REVISION || "f6ccbd59f0807113990222fd1cfab3f75276f376";
console.log(`Starting ${baseline ? "historical" : "current"} product acceptance; output: ${output}`);
const server = await createUiReviewServer(0, { baseline, baselineRevision, currentFixtures:true, fixture:"/scripts/fixtures/product-experience-review.tsx" });
const browser = await (await reviewChromium()).launch({headless:true, executablePath:process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"});
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
const evidence = [], flows = [], errors = [];
const inspect = async (page, id, condition) => {
  const geometry = await page.evaluate(() => {
    const selectors = [".conversation-search-modal", ".conversation-search-modal__retry", ".kb-panel__head", ".kb-panel__search", '[role="alert"]'];
    return { overflow:document.documentElement.scrollWidth > innerWidth + 1,
      observations:{results:document.querySelectorAll(".conversation-search-modal__item").length,
        directories:document.querySelectorAll(".conversation-search-modal__item-cwd").length,
        alerts:[...document.querySelectorAll('[role="alert"]')].map(e=>e.textContent),
        sourceLabels:[...document.querySelectorAll(".kb-panel__row-source")].map(e=>e.textContent)},
      elements:selectors.flatMap(selector=>[...document.querySelectorAll(selector)].map(element=>{
        const r=element.getBoundingClientRect(); return {selector,x:r.x,y:r.y,right:r.right,bottom:r.bottom};
      })) };
  });
  assert.equal(geometry.overflow, false);
  assert(geometry.elements.every(r=>r.x>=0 && r.right<=condition.width+1 && r.y>=0 && r.bottom<=condition.height+1), JSON.stringify(geometry));
  const colors = await page.evaluate(inspectRenderedColors);
  const isKnowledge = geometry.elements.some(element => element.selector === ".kb-panel__head");
  const observedContent = colors.text.some(item => isKnowledge
    ? item.selector.startsWith("h1.") && item.text === "知识库"
    : item.selector.includes(".conversation-search-modal__item-title"));
  assert(observedContent, "Color audit did not observe the actual page content");
  if (!baseline && id.startsWith("PX02")) assert(colors.text.some(item=>item.selector.includes("conversation-search-modal__item-cwd")), "Directory text was not measured");
  if (!baseline) assert.deepEqual([...colors.failures,...colors.controlFailures],[]);
  const filename = `${id}-${condition.width}-${condition.theme}-${condition.font}.png`;
  await page.screenshot({path:join(output, filename)});
  evidence.push({id,condition,filename,geometry,colors});
};
try {
  for (const [width,height,theme,font] of [[1280,800,"light",13],[1024,680,"light",13],[1024,680,"dark",18],[1280,800,"dark",13]]) {
    const condition = {width,height,theme,font};
    console.log(`Checking ${width}×${height}, ${theme}, font ${font}`);
    let creationTimer;
    const page = await Promise.race([
      browser.newPage({viewport:{width,height}}),
      new Promise((_,reject)=>{ creationTimer=setTimeout(()=>reject(new Error("Browser page creation exceeded 60s")),60000); }),
    ]).finally(()=>clearTimeout(creationTimer));
    page.setDefaultTimeout(30000);
    page.setDefaultNavigationTimeout(60000);
    page.on("pageerror",error=>errors.push(error.message));
    const goto = async extra => { await page.goto(`${base}?${new URLSearchParams({theme,font:String(font),...extra})}`, {waitUntil:"networkidle"}); };
    await goto({surface:"search"});
    const input = page.getByRole("textbox",{name:"搜索会话标题或内容"});
    await input.waitFor();
    await inspect(page,"PX01-recent",condition);
    const first = await page.locator(".conversation-search-modal__item").first().innerText();
    if (!baseline) {
      assert(first.includes("历史任务 65"), "Recent sessions are not sorted by activity");
      await page.getByRole("button",{name:/显示更多本地会话/}).click();
      assert.equal(await page.locator(".conversation-search-modal__item").count(),60);
      await page.getByRole("button",{name:/显示更多本地会话/}).click();
      assert.equal(await page.locator(".conversation-search-modal__item").count(),65);
    } else assert.equal(await page.locator(".conversation-search-modal__item").count(),30);
    await input.fill("交付复盘");
    await page.getByText("搜索中…",{exact:true}).waitFor({state:"hidden"});
    await inspect(page,"PX02-duplicate-titles",condition);
    if (baseline) assert.equal(await page.locator(".conversation-search-modal__item-cwd").count(),0);
    if (!baseline) {
      assert.equal(await page.locator(".conversation-search-modal__item-cwd").count(),2);
      await input.press("ArrowDown");
      assert(await page.locator(".conversation-search-modal__item").first().evaluate(e=>e===document.activeElement));
      await page.keyboard.press("ArrowDown");
      await page.keyboard.press("Enter");
      await page.getByRole("dialog").waitFor({state:"hidden"});
      assert.equal(await page.locator("output").innerText(),"review-0 | /review/客户甲/资料");
      await page.getByRole("button",{name:"打开搜索",exact:true}).click();
      await input.waitFor();
      await input.fill("正文");
      await page.getByText("正文命中：已确认交付范围与后续行动。").waitFor();
      assert.equal(await page.locator(".conversation-search-modal__item").count(),1);
      await input.fill("失败");
      await page.getByRole("alert").waitFor();
      assert(await page.getByRole("button",{name:"重试搜索",exact:true}).isEnabled());
      await input.fill("不存在的词");
      await page.getByText("没有匹配的会话",{exact:true}).waitFor();
      await page.keyboard.press("Escape");
      await page.getByRole("dialog").waitFor({state:"hidden"});
      await goto({surface:"search",failure:"load"});
      await page.locator(".conversation-search-modal__item").first().click();
      await page.getByRole("alert").filter({hasText:"会话加载失败"}).waitFor();
      assert(await page.getByRole("dialog").isVisible());
      await page.getByRole("button",{name:"重试打开",exact:true}).click();
      await page.getByRole("dialog").waitFor({state:"hidden"});
      const selected=await page.locator("output").innerText();
      assert.equal(selected,"review-64 | /review/客户甲/资料");
      flows.push({id:"PX06",condition,errorKeptSearchOpen:true,retryClosedSearch:true,selected});
      await goto({surface:"search",delayedMatch:"1"});
      await input.fill("交付复盘");
      await input.press("ArrowDown");
      assert(await page.locator(".conversation-search-modal__item").first().evaluate(e=>e===document.activeElement));
      await page.waitForFunction(()=>window.productReview.searchPending());
      await page.evaluate(()=>window.productReview.finishSearch());
      await page.getByText("延迟返回的正文命中",{exact:true}).waitFor();
      assert(await page.locator(".conversation-search-modal__item--remote").evaluate(e=>e===document.activeElement),"Async FTS merge lost the selected task's keyboard focus");
      await page.keyboard.press("Enter");
      await page.getByRole("dialog").waitFor({state:"hidden"});
      const mergedSelection=await page.locator("output").innerText();
      assert.equal(mergedSelection,"review-1 | /review/客户乙/资料");
      flows.push({id:"PX08",condition,focusPreserved:true,selected:mergedSelection});
      await goto({surface:"search",failure:"load",delayedMatch:"1"});
      // Send change + Enter in one event-loop task so this deterministically
      // exercises opening before the 250ms debounce, even on a busy machine.
      await input.evaluate(element => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,"value").set.call(element,"交付复盘");
        element.dispatchEvent(new Event("input",{bubbles:true}));
        element.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true}));
      });
      await page.getByRole("alert").filter({hasText:"会话加载失败"}).waitFor();
      await page.waitForFunction(()=>window.productReview.searchPending());
      await page.evaluate(()=>window.productReview.finishSearch());
      await page.getByText("延迟返回的正文命中",{exact:true}).waitFor();
      assert(await page.getByRole("button",{name:"重试打开",exact:true}).isEnabled(),"Debounced search erased the task-open retry");
      await inspect(page,"PX10-search-open-error",condition);
      await page.getByRole("button",{name:"重试打开",exact:true}).click();
      await page.getByRole("dialog").waitFor({state:"hidden"});
      assert.equal(await page.locator("output").innerText(),"review-1 | /review/客户乙/资料");
      flows.push({id:"PX10",condition,openRetrySurvivedDebounce:true,retryOpenedOriginalTask:true});
      await goto({surface:"search",failure:"load",longError:"1"});
      await page.locator(".conversation-search-modal__item").first().click();
      await page.getByRole("alert").filter({hasText:"会话加载失败"}).waitFor();
      await inspect(page,"PX10-search-long-error",condition);
      const longErrorRetry=page.getByRole("button",{name:"重试打开",exact:true});
      assert(await longErrorRetry.isVisible());
      assert(await longErrorRetry.isEnabled());
      assert(await longErrorRetry.evaluate(element=>{
        const r=element.getBoundingClientRect();
        return element.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2));
      }),"Long error text covered the retry button");
      await longErrorRetry.click();
      await page.getByRole("dialog").waitFor({state:"hidden"});
      assert.equal(await page.locator("output").innerText(),"review-64 | /review/客户甲/资料");
    }
    await goto({surface:"knowledge",failure:"picker"});
    const add = page.getByRole("button",{name:/添加本地文件夹/});
    await add.click();
    if (!baseline) await page.getByRole("alert").waitFor();
    else await page.waitForTimeout(150);
    await inspect(page,"PX03-picker-error",condition);
    if (baseline) assert.equal(await page.getByRole("alert").count(),0);
    if (!baseline) {
      await page.getByRole("button",{name:"重试操作",exact:true}).click();
      await page.getByRole("button",{name:"选择文件夹中…",exact:true}).waitFor();
      assert(await page.getByRole("button",{name:"选择文件夹中…",exact:true}).isDisabled());
      assert(await page.getByRole("button",{name:/刷新索引/}).isDisabled());
      await page.evaluate(()=>window.productReview.finishPick(null));
      await add.waitFor();
      assert.equal(await page.getByRole("alert").count(),0);
      assert.equal(await page.evaluate(()=>window.productReview.calls.filter(c=>c.command==="filesystem_pick_directory").length),2);
      await goto({surface:"knowledge",failure:"remove"});
      await page.getByRole("button",{name:"移除知识源 产品资料",exact:true}).click();
      await page.getByRole("alert").waitFor();
      assert(await page.getByRole("button",{name:"移除知识源 产品资料",exact:true}).isVisible());
      await page.getByRole("button",{name:"重试操作",exact:true}).click();
      await page.getByRole("heading",{name:"还没有添加知识源",exact:true}).waitFor();
      await goto({surface:"knowledge",failure:"open"});
    }
    await page.getByRole("textbox",{name:"搜索知识库",exact:true}).fill("交付");
    const doc = page.getByRole("button",{name:"打开知识条目：交付说明",exact:true});
    await doc.waitFor();
    await inspect(page,"PX04-readable-source",condition);
    if (baseline) assert.equal(await page.locator(".kb-panel__row-source").innerText(),"review-source");
    if (!baseline) {
      assert.equal(await page.locator(".kb-panel__row-source").innerText(),"产品资料");
      await doc.click();
      await page.getByRole("alert").filter({hasText:"文件暂时不可读"}).waitFor();
      await page.getByRole("button",{name:"重试操作",exact:true}).click();
      assert.equal(await page.locator("output").innerText(),"/review/产品资料/交付.md");
      assert.equal(await page.getByRole("alert").count(),0);
      await goto({surface:"knowledge",delayedRebuild:"1"});
      await page.getByRole("textbox",{name:"搜索知识库",exact:true}).fill("交付");
      await doc.waitFor();
      await page.getByRole("button",{name:/刷新索引/}).click();
      await page.waitForFunction(()=>window.productReview.rebuildPending());
      assert(await page.getByRole("button",{name:"刷新中…",exact:true}).isDisabled());
      assert(await doc.isEnabled(),"Semantic rebuild blocked an existing document");
      await doc.click();
      assert.equal(await page.locator("output").innerText(),"/review/产品资料/交付.md");
      await inspect(page,"PX11-rebuild-reading",condition);
      await page.getByRole("button",{name:"移除知识源 产品资料",exact:true}).click();
      await page.getByRole("heading",{name:"还没有添加知识源",exact:true}).waitFor();
      assert(await page.evaluate(()=>window.productReview.calls.some(c=>c.command==="org_local_kb_sources_set" && c.args.sources.length===0)),"Source removal was not persisted while rebuilding");
      await page.evaluate(()=>window.productReview.finishRebuild());
      await page.getByText("正在刷新索引，可继续查看资料和管理知识源。",{exact:true}).waitFor({state:"hidden"});
      assert.equal(await page.getByRole("alert").count(),0);
      assert.equal(await page.getByText("已移除知识源，原文件仍保留在原目录",{exact:true}).count(),1);
      assert.equal(await page.getByText("知识库已更新，可以搜索最新内容",{exact:true}).count(),0);
      flows.push({id:"PX11",condition,readDuringRebuild:true,removalPersisted:true,staleCompletionIgnored:true});
      await goto({surface:"knowledge",delayedOpen:"1"});
      const kbInput=page.getByRole("textbox",{name:"搜索知识库",exact:true});
      await kbInput.fill("交付");
      await doc.click();
      await page.waitForFunction(()=>window.productReview.openPending());
      await kbInput.fill("后续");
      const nextDoc=page.getByRole("button",{name:"打开知识条目：后续资料",exact:true});
      await nextDoc.waitFor();
      await page.evaluate(()=>window.productReview.failOpen());
      await nextDoc.waitFor({state:"visible"});
      assert(await nextDoc.isEnabled());
      assert.equal(await page.getByRole("alert").count(),0,"Old document error returned under a new query");
      assert.equal(await page.getByRole("button",{name:"重试操作",exact:true}).count(),0);
      await nextDoc.click();
      assert.equal(await page.locator("output").innerText(),"/review/产品资料/后续.md");
      flows.push({id:"PX12",condition,staleOpenFailureIgnored:true,newDocumentOpened:true});
    }
    await page.close();
  }
  assert.deepEqual(errors,[]);
  console.log(JSON.stringify({baseline,captures:evidence.length,errors,output}));
} finally {
  writeFileSync(join(output,"evidence.json"),JSON.stringify({baseline,baselineRevision:baseline ? baselineRevision : null,evidence,flows,errors},null,2));
  await browser.close(); await server.close();
}
