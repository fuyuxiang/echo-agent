import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-loading-tips-")));
console.log(`Loading tips evidence: ${output}`);
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=workbench&platform=mac`;
const evidence = [];
let browser;
try {
  browser = await (await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"});
  const page = await browser.newPage();
  await page.addInitScript(() => {
    const font = Number(new URL(location.href).searchParams.get("reviewFont") || 16);
    const apply = () => {
      if (!document.documentElement) return false;
      document.documentElement.style.fontSize = `${font}px`;
      return true;
    };
    if (!apply()) {
      const observer = new MutationObserver(() => { if (apply()) observer.disconnect(); });
      observer.observe(document,{childList:true,subtree:true});
    }
  });
  page.setDefaultTimeout(15_000);
  page.setDefaultNavigationTimeout(60_000);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  const epoch = Date.parse("2026-10-26T00:00:00Z");
  await page.clock.install({time:new Date(epoch)});
  await page.clock.pauseAt(new Date(epoch + 1_000));
  const tip = page.locator(".msg__loading-tip");
  const load = async (width, height, theme, state = "running", font = 16) => {
    await page.setViewportSize({width,height});
    await page.goto(`${base}&state=${state}&theme=${theme}&reviewFont=${font}`);
    await page.locator(".session-header").waitFor();
    if (width < 720) await page.getByRole("button",{name:"收起侧边栏",exact:true}).click();
  };
  const chunk = async kind => page.evaluate(async kind => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    const store = useSessionStore.getState();
    store.clearReplaySuppression("review-conversation");
    store.applyUpdate(kind === "tool" ? {
      sessionUpdate:"tool_call",toolCallId:"loading-review-tool",kind:"read",title:"读取交付文档",status:"in_progress",__sessionId:"review-conversation",
    } : {
      sessionUpdate:kind === "thought" ? "agent_thought_chunk" : "agent_message_chunk",
      content:{type:"text",text:kind === "thought" ? "正在分析实际需求" : "开始输出正式回答"},__sessionId:"review-conversation",
    });
  },kind);

  for (const [width,height,theme,font,kind] of [
    [1440,900,"light",16,"text"], [1280,800,"dark",16,"thought"],
    [1024,768,"light",20,"tool"], [390,844,"dark",16,"text"],
    [320,700,"light",20,"thought"], [390,844,"light",16,"tool"],
  ]) {
    await page.emulateMedia({reducedMotion:"reduce"});
    await load(width,height,theme,"running",font);
    await page.locator(".msg__loading-main").waitFor();
    assert.equal(await tip.count(),0,"fast requests should not flash companion copy");
    await page.clock.runFor(2_999);
    assert.equal(await tip.count(),0);
    await page.clock.runFor(1);
    await tip.waitFor();
    const first = await tip.textContent();
    assert(first);
    assert.equal(await tip.getAttribute("aria-live"),"off","secondary copy should not interrupt screen readers");
    assert.equal(await page.locator(".msg__loading-main").textContent(),"正在等待模型响应");
    const input = page.locator(".echo-composer__input");
    await input.fill("等待期间保留的输入草稿");
    await page.clock.runFor(5_000);
    assert.equal(await tip.textContent(),first);
    await page.getByText("可切换会话，任务会继续运行",{exact:true}).waitFor();
    const geometry = await page.locator(".msg__loading").evaluate(element => {
      const rect = element => { const r = element.getBoundingClientRect(); return {x:r.x,y:r.y,right:r.right,bottom:r.bottom}; };
      const tip = element.querySelector(".msg__loading-tip");
      return {overflow:document.documentElement.scrollWidth>innerWidth+1,tip:rect(tip),status:rect(element.querySelector(".msg__loading-status")),
        viewport:rect(document.querySelector(".chatview__scroll")),clipped:tip.scrollWidth>tip.clientWidth+1,animationCount:tip.getAnimations().length};
    });
    assert.equal(geometry.overflow,false);
    assert.equal(geometry.clipped,false);
    assert.equal(geometry.animationCount,0);
    assert(geometry.tip.y >= geometry.status.bottom,"secondary copy must stay below real status");
    assert(geometry.tip.x >= 0 && geometry.tip.right <= width+1,"tip must fit the window");
    assert(geometry.tip.y >= geometry.viewport.y && geometry.tip.bottom <= geometry.viewport.bottom+1,"a following transcript must keep the companion copy visible");
    await page.screenshot({path:join(output,`waiting-${width}-${theme}-${font}.png`)});
    await page.clock.runFor(6_999);
    assert.equal(await tip.textContent(),first,"tips must stay stable for 12 seconds");
    await page.clock.runFor(1);
    assert.notEqual(await tip.textContent(),first,"adjacent tips must differ");
    assert.equal(await input.inputValue(),"等待期间保留的输入草稿");
    assert(await input.evaluate(element => element === document.activeElement),"rotation must not steal input focus");
    if (width === 1440) {
      await page.locator('.sidebar__conv[data-session-id="review-other"] .sidebar__conv-select').click();
      assert.equal(await tip.count(),0);
      await page.clock.runFor(30_000);
      assert.equal(await tip.count(),0,"background tasks must not leak tips into the active transcript");
      await page.locator('.sidebar__conv[data-session-id="review-conversation"] .sidebar__conv-select').click();
      await page.locator(".msg__loading-main").waitFor();
      assert.equal(await tip.count(),0,"restoring a session starts a fresh presentation delay");
      await page.clock.runFor(3_000);
      await tip.waitFor();
    }
    await chunk(kind);
    await page.locator(".msg__loading").waitFor({state:"hidden"});
    if (kind === "text") await page.getByText("开始输出正式回答",{exact:true}).waitFor();
    else await page.locator(".execution-process--running").waitFor();
    await page.clock.runFor(30_000);
    assert.equal(await tip.count(),0,"tips must stay dismissed after the first streamed output");
    evidence.push({width,height,theme,font,kind,geometry});
  }

  for (const state of ["permission","paused","failed"]) {
    await load(1280,800,"light",state);
    await page.clock.runFor(15_000);
    assert.equal(await page.locator(".msg__loading").count(),0,`${state} should show its real state instead of model-wait copy`);
  }
  await load(1280,800,"light");
  await page.clock.runFor(3_000);
  await tip.waitFor();
  await page.evaluate(async () => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    useSessionStore.getState().requestControl("review-conversation","pause");
  });
  await page.locator(".msg__loading").waitFor({state:"hidden"});
  await page.getByText("正在暂停任务…",{exact:true}).waitFor();
  assert.equal(await page.locator(".session-header [role=status]").count(), 0);
  assert.deepEqual(errors,[]);
  writeFileSync(join(output,"evidence.json"),JSON.stringify({layouts:evidence,errors},null,2));
  console.log(JSON.stringify({ok:true,output,layouts:evidence.length,checks:["3s delay","12s nonrepeating rotation","input focus","session switching","first output dismissal","permission/pause/error priority","reduced motion"],errors},null,2));
} finally { await browser?.close(); await server.close(); }
