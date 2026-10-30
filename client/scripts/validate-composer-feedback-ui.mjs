import assert from "node:assert/strict";
import { join } from "node:path";
import { writeFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-composer-feedback-")));
const server = await createUiReviewServer(0, { freezeMotion: false });
const base = `http://127.0.0.1:${server.httpServer.address().port}`;
let browser;
const evidence = [];
const centeredEvidence = [];
const selectionEvidence = [];
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const [width, height] of [[1024,680],[1024,768],[1280,600],[1280,800],[1440,900],[1920,1080],[2045,1592],[2508,1648]]) {
    for (const font of [13,18]) for (const theme of ["light","dark"]) {
      const page = await browser.newPage({viewport:{width,height},deviceScaleFactor:font === 18 ? 1.5 : 1});
      page.setDefaultNavigationTimeout(60000);
      const errors = []; page.on("pageerror", error => errors.push(error.message));
      await page.addInitScript(size => {
        window.__echoCompositionActive = false;
        document.addEventListener("compositionstart", () => { window.__echoCompositionActive = true; });
        document.addEventListener("compositionend", () => { window.__echoCompositionActive = false; });
        const observer = new MutationObserver(() => { if (document.documentElement) { document.documentElement.style.fontSize = `${16*size/13}px`; observer.disconnect(); } });
        observer.observe(document,{childList:true,subtree:true});
      }, font);
      await page.goto(`${base}/__ui-review?surface=home&theme=${theme}`, {waitUntil:"networkidle"});
      const input = page.locator(".echo-composer__input");
      await input.waitFor();
      await input.click();
      assert.equal(await input.evaluate(e => getComputedStyle(e).outlineStyle), "none", "Textarea stacks a second focus ring");
      assert.equal(await input.inputValue(), "", "Placeholder became actual draft content");
      await input.press("Tab");
      assert.equal(await page.evaluate(() => getComputedStyle(document.activeElement).outlineStyle), "solid", "Toolbar keyboard focus lost its fallback indicator");
      await input.click();
      await input.press("ControlOrMeta+A");
      assert.deepEqual(await input.evaluate(e => [e.selectionStart,e.selectionEnd]), [0,0]);
      for (const collapsed of [false,true]) {
        if (collapsed) await page.getByRole("button", {name:"收起侧边栏",exact:true}).click();
        // Empty task entry must be centered in the available main pane,
        // independently of sidebar width, display scale and text size.
        await page.waitForFunction(() => document.querySelector("textarea").clientHeight < 200);
        const centered = await page.evaluate(() => {
          const rect = selector => { const r=document.querySelector(selector).getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height,right:r.right,bottom:r.bottom}; };
          // Collapsing the sidebar reserves a separate window-controls row.
          return {main:rect(".home"),group:rect(".home__inner"),card:rect(".echo-composer"),header:rect(".home__header")};
        });
        const context = JSON.stringify({width,height,font,theme,collapsed,...centered});
        assert(Math.abs(centered.group.x + centered.group.width/2 - centered.main.x - centered.main.width/2) < 2, `Home entry is not horizontally centered: ${context}`);
        assert(Math.abs(centered.group.y + centered.group.height/2 - centered.main.y - centered.main.height/2) < 2, `Home entry is not vertically centered: ${context}`);
        assert(centered.card.width <= 880.5, "Home card is too wide on large windows");
        assert(centered.card.y - centered.header.bottom >= 23.5, "Home title and composer lack breathing room");
        centeredEvidence.push({width,height,font,theme,collapsed,...centered});
        if (!collapsed && ((font===13 && theme==="light") || (width===1440 && font===18 && theme==="dark"))) {
          await input.evaluate(element => element.blur());
          await page.waitForFunction(() => !document.querySelector(".echo-composer").getAnimations().some(animation => animation.playState === "running"));
          await page.screenshot({path:join(output,`home-centered-${width}-${height}-${font}-${theme}.png`)});
        }
        const draft = Array.from({length:30},(_,i)=>`${i+1}. 中文长提示，检查工作目录、权限与发送按钮。`).join("\n");
        await input.fill(draft);
        const geometry = await page.evaluate(() => {
          const rect = selector => { const r=document.querySelector(selector).getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height,right:r.right,bottom:r.bottom}; };
          return {viewport:{width:innerWidth,height:innerHeight,dpr:devicePixelRatio},card:rect(".echo-composer"),input:rect(".echo-composer__input"),send:rect(".echo-composer__send"),workspace:rect(".workspace-picker__trigger"),permission:rect(".permission-picker"),overflow:document.documentElement.scrollWidth>innerWidth+1};
        });
        assert.equal(geometry.overflow,false, "Horizontal overflow");
        for (const control of [geometry.card,geometry.send,geometry.workspace,geometry.permission]) {
          assert(control.x>=0 && control.right<=width+1 && control.y>=0 && control.bottom<=height+1, `Control clipped: ${JSON.stringify({width,height,font,theme,collapsed,control})}`);
        }
        assert(geometry.input.height>160, "Long home draft is still capped at 160px");
        assert.equal(await input.inputValue(),draft);
        evidence.push({font,theme,collapsed,...geometry});
        await input.fill("");
      }
      await page.getByRole("button", {name:"展开侧边栏",exact:true}).click();
      const handle = page.locator("[data-sidebar-resize]");
      const bounds = await handle.boundingBox();
      await page.mouse.move(bounds.x+bounds.width/2,bounds.y+100);
      await page.mouse.down(); await page.mouse.move(375,bounds.y+100); await page.mouse.up();
      assert.equal(await page.locator(".app__body--sidebar-resizing").count(),0);
      await input.fill("拖动后输入和选择保持可用");
      await input.press("ControlOrMeta+A");
      assert.deepEqual(await input.evaluate(e=>[e.selectionStart,e.selectionEnd]),[0,"拖动后输入和选择保持可用".length]);
      await input.fill("");
      // Drag-selection from outside the empty card must not insert placeholder.
      const box = await input.boundingBox();
      await page.mouse.move(box.x-10,box.y+10); await page.mouse.down(); await page.mouse.move(box.x+250,box.y+20); await page.mouse.up();
      assert.equal(await input.inputValue(),"");
      selectionEvidence.push(await page.evaluate(() => {
        const input = document.querySelector("textarea");
        return {viewport:{width:innerWidth,height:innerHeight,dpr:devicePixelRatio},value:input.value,selectionStart:input.selectionStart,selectionEnd:input.selectionEnd,documentSelection:document.getSelection()?.toString(),activeElement:document.activeElement?.tagName,composing:window.__echoCompositionActive};
      }));
      if (font===13 && theme==="light") await page.screenshot({path:join(output,`home-${width}-${height}.png`)});
      assert.deepEqual(errors,[]);
      await page.close();
    }
  }
  const page = await browser.newPage({viewport:{width:1280,height:800}});
  await page.goto(`${base}/__ui-review?surface=coding`, {waitUntil:"networkidle"});
  const splitter = page.locator(".echo-theia-agent__splitter");
  await splitter.waitFor();
  const before = await splitter.evaluate(e => getComputedStyle(e).backgroundColor);
  await splitter.focus();
  assert.equal(await splitter.evaluate(e=>getComputedStyle(e).outlineStyle), "none", "IDE splitter regained a large outline");
  assert.notEqual(await splitter.evaluate(e=>getComputedStyle(e).backgroundColor), before, "IDE splitter lacks its own focus indicator");
  await page.goto(`${base}/__ui-review?surface=notify-channels`, {waitUntil:"networkidle"});
  const desktop = page.getByRole("switch", {name:"桌面通知", exact:true});
  assert(await desktop.isChecked());
  await page.getByRole("button",{name:"发送测试通知", exact:true}).click();
  await page.getByRole("status").filter({hasText:"已向系统提交通知"}).waitFor();
  // The native checkbox is visually replaced by its labelled switch track.
  await desktop.locator("..").click();
  await page.getByText("已关闭", {exact:true}).waitFor();
  assert(!await desktop.isChecked());
  assert(await page.getByRole("button",{name:"发送测试通知", exact:true}).isDisabled());
  await page.screenshot({path:join(output,"notifications.png")});
  writeFileSync(join(output,"geometry.json"),JSON.stringify({centered:centeredEvidence,layouts:evidence,selections:selectionEvidence},null,2));
  console.log(`Passed ${centeredEvidence.length} centered home layouts, ${evidence.length} long-draft layouts and desktop notification interaction; evidence: ${output}`);
} finally { await browser?.close(); await server.close(); }
