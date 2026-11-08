import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { inspectRenderedColors } from "./ui-color-audit.mjs";

const baseline = process.argv.includes("--baseline");
const auditOnly = process.argv.includes("--audit-only");
const output = reviewOutput(join(tmpdir(),"echo-color-audit",baseline ? "before" : "after"));
const server = await createUiReviewServer(0, { baseline });
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
const selected = process.argv.find(arg => arg.startsWith("--surface="))?.slice(10);
const surfaces = ["workbench", "home", "home:unconfigured", "home:loading", "home:error", "conversation", "search", "browser-toolbar", "capabilities", "page:技能", "page:连接器", "page:插件", "page:项目", "collapsed-project", "page:自动化", "automation-edit", "page:知识库", "organization", "organization-login", "meeting", "coding", "model", "general", "personalize", "memory", "personal-memory", "agent-settings", "shortcuts", "usage", "cloud-storage", "data", "archived", "security", "help", "agent-mail", "notify-channels", "weixin-unconnected", "weixin-connected", "weixin-offline", "modal-long", "modal-error", "about", "feedback", "update", "results", "details", "trust", "execution", "runtime", "queue", "controls", "page:技能:error", "page:连接器:error", "page:插件:error", "trust:error", "execution:complete", "execution:error", "results:code", "results:diagram", "projects", "management-experts", "management-knowledge", "management-schedules", "organization-shell", "expert-entry", "workspace-picker", "permission-picker", "toast", "content-menus", "coding-question", ...(baseline ? [] : ["color-system"])].filter(spec => !selected || selected.split(",").includes(spec)).sort((a,b) => selected ? selected.split(",").indexOf(a) - selected.split(",").indexOf(b) : 0);
const screenshots = new Set(["home", "conversation", "page:连接器", "page:技能", "page:项目", "organization", "coding", "general", "modal-error", "execution", "controls", "meeting", "color-system", "model", "organization-shell", "management-schedules", "workspace-picker", "content-menus", "search"]);
const scenarios = process.argv.includes("--full") ? [[1024,680,11], [1024,680,18], [1280,800,13], [1920,1080,11], [1920,1080,18]] : [[1280,800,13]];
const evidence = [];
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true,
    ...(process.env.CHROME_BIN ? { executablePath: process.env.CHROME_BIN } : {}) });
  // Reference cases keep compositing, disabled exemptions and complex-paint
  // reporting honest without asserting implementation details of production CSS.
  const reference = await browser.newPage();
  await reference.setContent('<body style="background:white"><span id="opaque" style="color:black">reference</span><span id="alpha" style="color:rgba(0,0,0,.5)">alpha</span><div style="opacity:.5"><span id="group" style="color:black">group</span></div><button disabled>disabled</button><span style="opacity:0">hidden</span><span style="background:linear-gradient(white,gray)">gradient</span></body>');
  const known = await reference.evaluate(inspectRenderedColors);
  assert.equal(known.text.find(e => e.selector === "span#opaque").ratio,21);
  assert(Math.abs(known.text.find(e => e.selector === "span#alpha").ratio - 3.976653024912438) < 1e-10);
  assert(Math.abs(known.text.find(e => e.selector === "span#group").ratio - 3.976653024912438) < 1e-10);
  assert.equal(known.disabled.length,1);
  assert.equal(known.manual.length,1);
  assert.ok(!known.text.some(e => e.text === "hidden"));
  // Actual values can differ from markup (textarea edits) or option values
  // (select labels). Invisible text must fail even when its border passes.
  await reference.setContent('<body style="background:white"><select id="unreadable-select" style="background:white;color:white"><option value="team">团队工作区</option></select><textarea id="unreadable-textarea" style="background:white;color:white">旧内容</textarea></body>');
  await reference.locator("textarea").fill("修改后的内容");
  const unreadable = await reference.evaluate(inspectRenderedColors);
  assert.deepEqual(unreadable.failures.map(item => [item.selector,item.text,item.ratio]), [
    ["select#unreadable-select","团队工作区",1], ["textarea#unreadable-textarea","修改后的内容",1],
  ]);
  await reference.setContent('<body style="background:white"><span class="meeting-recorder__pulse"><svg width="20" height="20" fill="none" stroke="white"><path d="M1 1L19 19" /></svg></span><span class="meeting-global-recorder__dot"><svg width="14" height="14" fill="none" stroke="white"><path d="M1 1L13 13" /></svg></span></body>');
  const invisibleIcons = await reference.evaluate(inspectRenderedColors);
  assert.equal(invisibleIcons.controlFailures.filter(item=>item.role==="status-icon" && item.ratio===1).length,2,"Both recording glyphs must be checked even when they use currentColor strokes");
  await reference.setContent('<body style="background:white"><span style="color:color(srgb 0 0 0 / .53466)">临界文字</span><span class="knowledge-picker__checkbox" style="color:white"><svg width="20" height="20" fill="none" stroke="currentColor"><path d="M1 1L19 19" /></svg></span></body>');
  const negative = await reference.evaluate(inspectRenderedColors);
  const borderline = negative.failures.find(item => item.text === "临界文字");
  assert(borderline && borderline.ratio < 4.5 && Number(borderline.ratio.toFixed(3)) === 4.5, "A below-threshold ratio that rounds to 4.500 must still fail");
  assert(negative.controlFailures.some(item => item.role === "selection-mark" && item.ratio === 1), "Invisible selected glyphs must fail independently of their text");
  await reference.close();
  if (!baseline && surfaces.includes("color-system")) {
    // The ordinary grid freezes motion. Exercise the real recording animation
    // separately, including its dimmest midpoint and reduced-motion behavior.
    const motionServer = await createUiReviewServer(0,{freezeMotion:false});
    const motionPage = await browser.newPage({viewport:{width:1280,height:800}});
    const motionEvidence = [];
    motionPage.setDefaultNavigationTimeout(90000);
    try {
      for (const theme of ["light","dark"]) {
        await motionPage.emulateMedia({reducedMotion:"no-preference"});
        await motionPage.goto(`http://127.0.0.1:${motionServer.httpServer.address().port}/__ui-review?surface=color-system&theme=${theme}`,{waitUntil:"networkidle"});
        const glyph = motionPage.locator(".meeting-global-recorder__dot");
        await glyph.waitFor();
        assert.equal(await glyph.evaluate(e=>e.getAnimations().length),1,"The recording animation must actually run");
        for (const phase of [0,.25,.5,.75]) {
          await glyph.evaluate((e,phase)=>{for(const animation of e.getAnimations()){animation.pause();animation.currentTime=Number(animation.effect.getTiming().duration)*phase;}},phase);
          const colors = await motionPage.evaluate(inspectRenderedColors);
          assert.equal(await glyph.evaluate(e=>getComputedStyle(e).opacity),"1","Recording motion must not fade its source glyph");
          assert.equal(colors.controls.filter(item=>item.role==="status-icon").length,2);
          assert.deepEqual(colors.controlFailures,[],`${theme}: recording contrast fails during its animation`);
        }
        // Timeline sampling above deliberately overrides animation playback.
        // A script-paused animation may survive animation-name:none. Reload
        // before observing the untouched production response to media changes.
        await motionPage.reload({waitUntil:"networkidle"});
        await glyph.waitFor();
        assert.equal(await glyph.evaluate(e=>e.getAnimations().length),1,"The fresh recording animation must actually run");
        assert.equal(await glyph.evaluate(e=>e.getAnimations()[0].playState),"running","Reduced motion must be tested without a script playback override");
        await motionPage.emulateMedia({reducedMotion:"reduce"});
        const reduced = await glyph.evaluate(e=>({ matches:matchMedia("(prefers-reduced-motion: reduce)").matches,
          animation:getComputedStyle(e).animationName, count:e.getAnimations().length }));
        motionEvidence.push({theme,...reduced});
        writeFileSync(join(output,"recording-motion.json"),JSON.stringify(motionEvidence,null,2));
        assert.equal(reduced.matches,true);
        assert.equal(reduced.animation,"none");
        assert.equal(reduced.count,0,"Reduced motion must stop the recording halo");
      }
    } finally {await motionPage.close();await motionServer.close();}
  }
  for (const [width,height,font] of scenarios) for (const theme of ["light", "dark"]) {
    for (const spec of surfaces) {
      // Independent surface observations must not inherit document/module
      // state from dozens of previous full-page navigations.
      const page = await browser.newPage({ viewport: {width,height}, colorScheme: theme });
      const errors = [];
      page.on("pageerror", error => errors.push(error.message));
      page.setDefaultNavigationTimeout(60000);
      const [surface,variant,extension] = spec.split(":");
      const params = new URLSearchParams({surface,theme,extensionState:"populated",settingsDesign:"1"});
      if (variant) params.set(surface === "page" ? "label" : surface === "results" ? "resultVariant" : "state", variant);
      if (extension) params.set("extensionState",extension);
      if (surface === "agent-mail") params.set("notificationState","records");
      if (surface === "notify-channels") params.set("notificationState","configured");
      if (surface === "conversation" && process.argv.includes("--plan-review")) {
        params.set("planReview","clean"); params.set("pausedQueueReview","1");
      }
      if (surface === "color-system" && process.argv.includes("--profile-review")) params.set("profileDangerReview","1");
      await page.goto(`${base}?${params}`, {waitUntil:"networkidle"});
      await page.locator("#root > *").first().waitFor();
      await page.evaluate(font => document.documentElement.style.fontSize = `${16*font/13}px`,font);
      // Lazy page imports and IPC-backed lists can outlive network idle.
      if (spec === "page:自动化") await page.locator(".atm-template-section").waitFor();
      if (spec === "management-schedules") await page.getByText("每日 AI 新闻推送",{exact:true}).first().waitFor();
      const result = await page.evaluate(inspectRenderedColors);
      if (surface === "color-system") {
        assert(result.text.some(item=>item.selector==="b" && item.text==="3"),"The real dirty-file count must be visible and measured");
        assert.equal(result.controls.filter(item=>item.role==="status-icon").length,2,"Both recording indicators must be visible and measured");
      }
      evidence.push({spec,theme,width,height,font,state:"default",...result});
      if (screenshots.has(spec) && font === 13) await page.screenshot({path:join(output,`${spec.replace(":","-")}-${theme}-${width}.png`)});
      // Exercise real page controls in addition to each default surface.
      const control = page.locator('button:not(:disabled):not([aria-disabled="true"])').filter({visible:true}).first();
      if (await control.count()) {
        await control.hover();
        await control.focus();
        evidence.push({spec,theme,width,height,font,state:"hover-focus",...await page.evaluate(inspectRenderedColors)});
      }
      if (surface === "controls" || surface === "color-system" && process.argv.includes("--profile-review")) {
        const danger = page.locator(surface === "controls" ? ".echo-button--danger" : ".colleague-profile-btn--danger").first();
        await danger.hover();
        evidence.push({spec,theme,width,height,font,state:"danger-hover",...await page.evaluate(inspectRenderedColors)});
        await page.mouse.down();
        evidence.push({spec,theme,width,height,font,state:"danger-active",...await page.evaluate(inspectRenderedColors)});
        await page.mouse.up();
      }
      if (surface === "automation-edit") {
        await page.getByRole("button",{name:"高级设置",exact:true}).click();
        evidence.push({spec,theme,width,height,font,state:"advanced",...await page.evaluate(inspectRenderedColors)});
      }
      if (surface === "color-system") {
        await page.getByRole("textbox",{name:"交付说明",exact:true}).fill("修改后的交付说明");
        await page.getByRole("combobox",{name:"交付工作区",exact:true}).selectOption("team");
        const edited = await page.evaluate(inspectRenderedColors);
        const measured = [...edited.text,...edited.manual];
        for (const text of ["修改后的交付说明","团队交付工作区"]) assert(measured.some(item => item.text === text),`Edited control text was skipped: ${text}`);
        evidence.push({spec,theme,width,height,font,state:"edited-controls",...edited});
      }
      if (surface === "conversation" && process.argv.includes("--plan-review")) {
        assert.equal(await page.locator(".queue-row--paused").count(),1,"The review must seed its paused message before approval");
        await page.getByRole("button",{name:"查看并确认计划",exact:true}).click();
        await page.getByRole("group",{name:"计划审批",exact:true}).getByRole("button",{name:"批准执行",exact:true}).click();
        await page.getByRole("group",{name:"计划审批",exact:true}).waitFor({state:"hidden"});
        await page.locator(".queue-row--paused").first().waitFor();
        evidence.push({spec,theme,width,height,font,state:"approved-paused-queue",...await page.evaluate(inspectRenderedColors)});
      }
      if (process.argv.includes("--open-overlays")) {
        const capture = async state => {
          evidence.push({spec,theme,width,height,font,state,...await page.evaluate(inspectRenderedColors)});
          if (font === 13 || width === 1024 && font === 18) await page.screenshot({path:join(output,`${spec.replace(":","-")}-${state}-${theme}-${width}-${font}.png`)});
        };
        if (surface === "workbench") {
          await page.getByRole("button", {name:"筛选任务",exact:true}).click();
          await page.getByRole("menu", {name:"任务筛选",exact:true}).waitFor();
          const glyphs = await page.evaluate(inspectRenderedColors);
          assert(glyphs.controls.filter(control=>control.role==="selection-mark").length >= 2, "Both sidebar filter marks must be measured");
          await capture("sidebar-selected-filter");
          await page.keyboard.press("Escape");
        }
        if (surface === "workspace-picker") {
          await page.locator(".workspace-picker__trigger").click();
          await page.getByRole("menu",{name:"选择工作目录",exact:true}).waitFor();
          await capture("workspace-menu");
        }
        if (surface === "permission-picker") {
          await page.getByRole("button",{name:/请求批准/}).click();
          const menu=page.getByRole("menu",{name:"本任务权限",exact:true});
          await menu.waitFor(); await capture("permission-menu");
          await menu.getByRole("menuitemradio",{name:/本任务免审批/}).click();
          await menu.getByRole("alertdialog",{name:"确认本任务免审批",exact:true}).waitFor();
          await capture("permission-confirmation");
        }
        if (surface === "content-menus") {
          await page.getByRole("button",{name:"操作菜单示例",exact:true}).click();
          const menu=page.getByRole("menu",{name:"操作菜单示例",exact:true});
          await menu.waitFor(); await capture("action-menu");
          await menu.getByRole("menuitem",{name:"查看导出格式",exact:true}).click();
          await capture("expanded-action-menu");
          await page.keyboard.press("Escape");
          await page.getByRole("button",{name:/^示例模型/}).click();
          await page.getByRole("listbox",{name:"示例模型",exact:true}).waitFor();
          await capture("model-menu");
          await page.keyboard.press("Escape");
          await page.getByRole("button",{name:/^工作目录：/}).click();
          await page.getByRole("menu",{name:"选择工作目录",exact:true}).waitFor();
          await capture("directory-menu");
        }
        if (surface === "toast") {
          await page.getByRole("button",{name:"模拟连接失败",exact:true}).click();
          await page.locator(".toast").first().waitFor(); await capture("error-toast");
          await page.locator(".toast").first().getByText("查看详情",{exact:true}).click();
          await capture("toast-diagnostics");
          await page.getByRole("button",{name:"归档示例",exact:true}).click();
          await capture("action-toast");
        }
      }
      assert.deepEqual(errors,[],`${spec}/${theme}: browser errors`);
      await page.close();
    }
    // Checkpoints preserve review evidence even if a later integration fails.
    writeFileSync(join(output,"evidence.json"),JSON.stringify(evidence,null,2));
    const failures = evidence.flatMap(e => [...e.failures,...e.controlFailures].map(f => ({spec:e.spec,theme:e.theme,state:e.state,width:e.width,font:e.font,...f})));
    writeFileSync(join(output,"failures.json"),JSON.stringify(failures,null,2));
    console.log(`${baseline ? "Before" : "After"}: ${surfaces.length} surfaces, ${theme}, ${width}×${height}, font ${font}; cumulative failures: ${failures.length}`);
  }
  writeFileSync(join(output,"evidence.json"),JSON.stringify(evidence,null,2));
  const failures = evidence.flatMap(e => [...e.failures,...e.controlFailures].map(f => ({spec:e.spec,theme:e.theme,state:e.state,width:e.width,font:e.font,...f})));
  writeFileSync(join(output,"failures.json"),JSON.stringify(failures,null,2));
  console.log(JSON.stringify({observations:evidence.length,text:evidence.reduce((n,e)=>n+e.text.length,0),failures:failures.length,manual:evidence.reduce((n,e)=>n+e.manual.length,0),output}));
  if (!auditOnly && !baseline) assert.deepEqual(failures,[],`Rendered text contrast failures; see ${output}/failures.json`);
} finally { await browser?.close(); await server.close(); }
