import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { inspectButtonBackgrounds, assertButtonBackgrounds } from "./ui-button-audit.mjs";
import { assertSettingsActionsSeparated } from "./ui-settings-audit.mjs";

// Read the working tree, never HEAD: this is post-change acceptance evidence.
assert.notEqual(process.env.UI_REVIEW_BASELINE, "1", "Post-change validation must use the working tree");
const output = reviewOutput("/private/tmp/echo-design-system");
const server = await createUiReviewServer(0);
const groupServer = await createUiReviewServer(0, { fixture: "/scripts/fixtures/group-review.tsx" });
const address = s => `http://127.0.0.1:${s.httpServer.address().port}`;
let browser;
const evidence = [];
const scenarios = [
  [1280, 800, "light", 13], [1280, 800, "dark", 13],
  [1024, 680, "light", 11], [1024, 680, "dark", 18],
  [1920, 1080, "light", 18], [1920, 1080, "dark", 11],
];
const surfaces = process.argv.includes("--controls-only") ? ["page:技能", "page:连接器", "page:插件", "page:项目"] : [
  "home", "home:unconfigured", "home:loading", "home:error", "conversation", "search", "browser-toolbar",
  "capabilities", "page:技能", "page:连接器", "page:插件", "page:项目", "collapsed-project",
  "page:自动化", "automation-edit", "page:知识库", "organization", "organization-login", "meeting", "coding",
  "model", "general", "personalize", "memory", "personal-memory", "agent-settings", "shortcuts",
  "usage", "cloud-storage", "data", "archived", "security", "help", "agent-mail", "notify-channels", "weixin-unconnected",
  "modal-long", "modal-error", "about", "feedback", "update", "results", "details", "trust", "execution", "runtime", "queue",
  "weixin-connected", "weixin-offline",
];
function checkBrandAction({ background, color }, label) {
  const rgb = value => value.match(/[\d.]+/g).slice(0, 3).map(channel => Number(channel) * (value.startsWith("color(srgb") ? 255 : 1));
  const luminance = value => rgb(value).map(channel => {
    const c = channel / 255;
    return c <= .04045 ? c / 12.92 : ((c + .055) / 1.055) ** 2.4;
  }).reduce((sum, channel, index) => sum + channel * [.2126, .7152, .0722][index], 0);
  const [r, g, b] = rgb(background);
  assert.ok(g > r + 10 && g > b, `${label}: primary action lost its mint identity`);
  const levels = [luminance(background), luminance(color)].sort((a, b) => b - a);
  assert.ok((levels[0] + .05) / (levels[1] + .05) >= 4.5, `${label}: primary label contrast is below 4.5:1`);
}
async function checkSearch(page) {
  const input = page.locator(".capability-search input");
  const header = page.locator(".capability-page__header");
  const before = await header.boundingBox();
  await input.fill("用于检查清除按钮位置的搜索词");
  const boxes = await page.locator(".capability-search").evaluate(root => {
    const box = e => { const r=e.getBoundingClientRect(); return {x:r.x,y:r.y,right:r.right,bottom:r.bottom,width:r.width,height:r.height}; };
    return {input:box(root.querySelector("input")),clear:box(root.querySelector("button")),icon:box(root.querySelector("svg")),position:getComputedStyle(root.querySelector("button")).position};
  });
  assert.equal(boxes.position,"absolute","Clear button joins normal flow");
  assert.ok(Math.abs((await header.boundingBox()).height-before.height)<1,"Typing changes toolbar height");
  assert.ok(boxes.clear.x>boxes.icon.right && boxes.clear.right<=boxes.input.right,"Search icons overlap or leave the field");
  assert.ok(Math.abs(boxes.clear.y+boxes.clear.height/2-boxes.input.y-boxes.input.height/2)<1,"Clear button is not centered");
  await input.focus(); await page.keyboard.press("Tab");
  assert.equal(await page.locator(".capability-search__clear").evaluate(e=>e===document.activeElement),true);
  await page.keyboard.press("Enter");
  assert.equal(await input.inputValue(),"");
  assert.equal(await input.evaluate(e=>e===document.activeElement),true,"Clear loses keyboard focus");
}
async function checkPlans(page, base, scenario) {
  for (const [state,action,outcome] of [["clean","批准执行","approved"],["dirty","同步修改并重新审批","cancelled"],["clean","修改计划","cancelled"],["clean","放弃计划","abandoned"]]) {
    await page.goto(`${base}/__ui-review?surface=conversation&planReview=${state}&theme=${scenario[2]}`,{waitUntil:"networkidle"});
    await page.getByRole("button", {name:"查看并确认计划",exact:true}).click();
    const approval=page.getByRole("group",{name:"计划审批",exact:true}); await approval.waitFor();
    const buttons=await approval.locator("button").evaluateAll(elements=>elements.map(e=>{
      const r=e.getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height,right:r.right,bottom:r.bottom,scroll:e.scrollWidth,client:e.clientWidth};
    }));
    assert.equal(buttons.length,3);
    assert.ok(buttons[0].bottom<=buttons[1].y && buttons[1].y===buttons[2].y,"Approval action hierarchy is lost");
    assert.ok(buttons[1].width>=100 && buttons[2].width>=100,"Secondary approval actions become vertical text");
    assert.ok(buttons.every(b=>b.height<=scenario[3]*3.5 && b.scroll<=b.client+1),"Approval action clips or wraps excessively");
    if(action==="同步修改并重新审批") await page.screenshot({path:join(output,`plan-approval-${scenario.join("-")}.png`)});
    await approval.getByRole("button",{name:action,exact:true}).click();
    await approval.waitFor({state:"hidden"});
    assert.equal(await page.evaluate(()=>window.planReviewOutcomes.at(-1)?.outcome),outcome,"Approval action sends the wrong outcome");
    await inspect(page,`plan-${action}`,scenario);
  }
}
async function inspect(page, spec, scenario, capture = false) {
  const buttonAudit = await inspectButtonBackgrounds(page, scenario[2]);
  assertButtonBackgrounds(buttonAudit, `${spec} ${scenario}`);
  const geometry = await page.evaluate(() => {
    const rect = e => { const r = e.getBoundingClientRect(); return { x:r.x,y:r.y,width:r.width,height:r.height,right:r.right,bottom:r.bottom }; };
    const titles = [...document.querySelectorAll(".capability-page__title,.settings-section__title,.model-connections__header h2,.meeting-panel__header h1,.org-memory__header h1")]
      .map(e => ({ text:e.textContent, size:getComputedStyle(e).fontSize, line:getComputedStyle(e).lineHeight, ...rect(e) }));
    const dialogs = [...document.querySelectorAll('[aria-modal="true"]')].map(rect);
    return { overflow:document.documentElement.scrollWidth > innerWidth+1, titles, dialogs };
  });
  assert.ok(!geometry.overflow, `${spec} ${scenario}: document overflows horizontally`);
  for(const box of geometry.dialogs) assert.ok(box.x >= -1 && box.y >= -1 && box.right <= scenario[0]+1 && box.bottom <= scenario[1]+1, `${spec}: modal outside viewport`);
  if(capture) await page.screenshot({ path:join(output, `${spec.replaceAll(":","-")}-${scenario.join("-")}.png`) });
  evidence.push({ spec, scenario, ...geometry, buttons:buttonAudit.buttons });
}
try {
  browser = await (await reviewChromium()).launch({ headless:true, executablePath:process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  console.log("Checking baseline source isolation");
  const baselineServer=await createUiReviewServer(0,{baseline:true});
  let baselinePage;
  try {
    const page=baselinePage=await browser.newPage(); const errors=[];page.on("pageerror",e=>errors.push(e.message));
    // HEAD uses a fresh dependency cache; allow the same cold-compilation
    // navigation budget as the other browser review entries.
    page.setDefaultNavigationTimeout(60000);
    await page.goto(`${address(baselineServer)}/__ui-review?surface=conversation`,{waitUntil:"networkidle"});
    await page.locator(".chatview").waitFor();
    const styles=await page.locator("style[data-vite-dev-id]").evaluateAll(elements=>elements.map(e=>e.getAttribute("data-vite-dev-id")).filter(id=>id.includes("/src/") && id.endsWith(".css")));
    assert.ok(styles.length>0 && styles.every(id=>!id.startsWith(process.cwd())),"Baseline imports working-tree CSS");
    assert.deepEqual(errors,[],"Baseline cannot render HEAD source and styles");
  } finally { await baselinePage?.close(); await baselineServer.close(); }
  console.log("Baseline source isolation passed");
  for(const scenario of scenarios) {
    console.log(`Checking ${surfaces.length} surfaces: ${scenario.join(" / ")}`);
    const [width,height,theme,font] = scenario;
    const page = await browser.newPage({ viewport:{width,height}, colorScheme:theme });
    const errors=[]; page.on("pageerror", error=>errors.push(error.message));
    await page.addInitScript(size => {
      const apply = () => { if (!document.documentElement) return false; document.documentElement.style.fontSize=`${16*size/13}px`; return true; };
      if (!apply()) { const observer = new MutationObserver(() => { if (apply()) observer.disconnect(); }); observer.observe(document,{childList:true,subtree:true}); }
    },font);
    for(const spec of surfaces) {
      const [surface,variant]=spec.split(":");
      const params=new URLSearchParams({ surface,theme,extensionState:"populated" });
      if(variant) params.set(surface==="page"?"label":"state",variant);
      await page.goto(`${address(server)}/__ui-review?${params}`, {waitUntil:"networkidle"});
      await page.locator("#root > *").first().waitFor();
      if(await page.locator(".capability-search input").count()) await checkSearch(page);
      if(spec === "page:项目") {
        const image = page.getByRole("img", {name:"项目工作台插画"});
        assert.ok(await image.isVisible(), "Project introduction lost its illustration");
        assert.ok(await image.evaluate(e => e.complete && e.naturalWidth > 0), "Project illustration failed to load");
        // A wide sidebar reduces available content independently of the window.
        const container = page.locator(".project-management");
        await container.evaluate(e => e.style.width = "calc(100% - 360px)");
        const bounds = await container.evaluate(e => {
          const area = e.getBoundingClientRect();
          return [...e.querySelectorAll(".capability-page__header img, .capability-page__controls input, .capability-page__actions button")].map(child => {
            const box = child.getBoundingClientRect();
            return {visible:box.width > 0 && box.height > 0, inside:box.left >= area.left && box.right <= area.right};
          });
        });
        assert.ok(bounds.every(box => box.visible && box.inside), "Project artwork or actions overflow a narrow content area");
        await container.evaluate(e => e.style.removeProperty("width"));
      }
      if(surface==="automation-edit") {
        const toggles=await page.locator(".echo-switch--checked").evaluateAll(elements=>elements.map(e=>({track:getComputedStyle(e).backgroundColor,thumb:getComputedStyle(e.querySelector(".echo-switch-thumb")).backgroundColor})));
        assert.ok(toggles.length>0 && toggles.every(toggle=>toggle.track!==toggle.thumb),"Checked switch thumb disappears into its track");
      }
      await inspect(page,spec,scenario,["capabilities","page:项目","general","modal-long","coding","meeting"].includes(spec));
      if(surface === "model") await assertSettingsActionsSeparated(page);
      if(surface==="organization") for(const tab of ["经验","文档","Skills"]) {
        await page.locator(".org-memory__nav button").filter({hasText:tab}).click();
        await inspect(page,`organization-${tab}`,scenario,true);
      }
    }
    // Compare actual final metrics across all six production compatibility paths.
    await page.goto(`${address(server)}/__ui-review?surface=controls&theme=${theme}`, {waitUntil:"networkidle"});
    const metrics=await page.locator("main > button").evaluateAll(elements=>elements.slice(0,6).map(e=>{
      const s=getComputedStyle(e);return { height:e.getBoundingClientRect().height,font:s.fontSize,line:s.lineHeight,radius:s.borderRadius,padding:s.padding,color:s.color,background:s.backgroundColor };
    }));
    assert.equal(metrics.length,6);
    for(const metric of metrics) assert.deepEqual(metric,metrics[0],`Button aliases drift at ${scenario}`);
    for(const metric of metrics) checkBrandAction(metric, `Primary alias at ${scenario}`);
    const save=page.getByRole("button",{name:"保存这段较长的说明"});
    const initial=await save.boundingBox();
    const expectedState = async name => save.evaluate((element, name) => {
      const probe=document.createElement("span"); probe.style.backgroundColor=`var(${name})`; element.append(probe);
      const color=getComputedStyle(probe).backgroundColor; probe.remove(); return color;
    },name);
    await save.hover();
    assert.equal(await save.evaluate(e=>getComputedStyle(e).backgroundColor),await expectedState("--button-hover"),"Hover loses primary semantics");
    checkBrandAction(await save.evaluate(e=>({background:getComputedStyle(e).backgroundColor,color:getComputedStyle(e).color})), "Hovered primary");
    await page.mouse.down();
    assert.equal(await save.evaluate(e=>getComputedStyle(e).backgroundColor),await expectedState("--button-active"),"Active loses primary semantics");
    checkBrandAction(await save.evaluate(e=>({background:getComputedStyle(e).backgroundColor,color:getComputedStyle(e).color})), "Pressed primary");
    await page.mouse.up();
    assert.equal(await save.getAttribute("aria-busy"),"true");
    assert.deepEqual(await save.boundingBox(),initial,"Loading changes button geometry");
    await page.getByLabel("名称",{exact:true}).focus();
    const focus=await page.getByLabel("名称",{exact:true}).evaluate(e=>({style:getComputedStyle(e).outlineStyle,width:getComputedStyle(e).outlineWidth}));
    assert.equal(focus.style,"solid");assert.equal(focus.width,"2px");
    const fields=await page.locator("main > input,main > select").evaluateAll(elements=>elements.map(e=>e.getBoundingClientRect().height));
    assert.ok(fields.every(height=>Math.abs(height-metrics[0].height)<1),"Form and standard buttons have different heights");
    assert.equal(await page.getByLabel("工作区",{exact:true}).evaluate(e=>getComputedStyle(e).paddingRight),"34px","Select text intrudes into its arrow");
    for(const [label,property,token] of [["错误名称","borderColor","--echo-status-error"],["只读名称","backgroundColor","--echo-bg-secondary"]]) {
      const field=page.getByLabel(label,{exact:true});
      const colors=await field.evaluate((e,{property,token})=>{const probe=document.createElement("span");probe.style.color=`var(${token})`;e.after(probe);const expected=getComputedStyle(probe).color;probe.remove();return {actual:getComputedStyle(e)[property],expected};},{property,token});
      assert.equal(colors.actual,colors.expected,`${label} state is overridden`);
    }
    evidence.push({spec:"controls",scenario,metrics,focus,fields});
    await page.screenshot({path:join(output,`controls-${scenario.join("-")}.png`)});
    await checkPlans(page,address(server),scenario);
    assert.deepEqual(errors,[],`${scenario}: browser errors`);
    // Group list is rendered by the existing IPC-isolated production fixture.
    await page.goto(`${address(groupServer)}/__ui-review?theme=${theme}&groupShell=1`,{waitUntil:"networkidle"});
    await page.getByRole("heading",{name:"专家团",exact:true}).waitFor();
    await inspect(page,"groups",scenario,true);
    const search=page.getByRole("textbox",{name:"搜索专家团",exact:true});await search.focus();
    assert.equal(await search.evaluate(e=>getComputedStyle(e).outlineStyle),"solid");
    await checkSearch(page);
    // Navigation order must not introduce CSS or alter management header metrics.
    const before=await page.locator(".capability-page__title").evaluate(e=>getComputedStyle(e).fontSize);
    await page.locator(".panel-navigation").getByRole("button",{name:"技能",exact:true}).click();
    await page.getByRole("heading",{name:"已安装技能",exact:true}).waitFor();
    await page.locator(".panel-navigation").getByRole("button",{name:"专家团",exact:true}).click();
    await page.getByRole("heading",{name:"专家团",exact:true}).waitFor();
    assert.equal(await page.locator(".capability-page__title").evaluate(e=>getComputedStyle(e).fontSize),before);
    // Use the actual available pane width, including a maximum-width sidebar.
    await page.evaluate(()=>document.querySelector(".app").style.setProperty("--app-sidebar-width","400px"));
    await inspect(page,"groups-sidebar-400",scenario,true);
    await page.evaluate(()=>document.querySelector(".app").style.setProperty("--app-sidebar-width","264px"));
    await page.getByRole("button",{name:"创建专家团",exact:true}).click();
    await page.getByRole("textbox",{name:"专家团名称",exact:true}).fill("长中文项目名称与复杂工作目录的协作交付验证专家团");
    await page.getByRole("textbox",{name:"协作目标",exact:true}).fill("核对长中文标题、长错误说明和深层目录，持续保留工作记录并形成可验收的交付结果。");
    await page.getByRole("checkbox",{name:/代码专家/}).check();
    await inspect(page,"groups-create",scenario,true);
    await page.getByRole("button",{name:"创建并进入专家团",exact:true}).click();
    const composer=page.locator(".echo-composer__input");await composer.waitFor();
    await composer.fill("请检查 /review/一个很长的中文目录/项目资料与历史版本/交付说明/需求与验收标准.md，并整理错误恢复步骤。");
    await page.getByRole("button",{name:"发送",exact:true}).click();
    await page.evaluate(()=>window.groupReview.complete());
    await page.getByText("已完成需求分析与交付检查。",{exact:false}).first().waitFor();
    await inspect(page,"groups-chat",scenario,true);
    const details=page.getByRole("button",{name:"专家团详情",exact:true});
    if(await details.getAttribute("aria-expanded")==="false") await details.click();
    await page.locator(".group-tabs").getByRole("button",{name:"设置",exact:true}).click();
    await page.getByRole("button",{name:"保存专家团设置",exact:true}).scrollIntoViewIfNeeded();
    await inspect(page,"groups-settings",scenario,true);
    await page.close();
    console.log(`Design-system surfaces passed: ${scenario.join(" / ")}`);
  }
  // First-paint script and ThemeProvider must agree on light/dark/system.
  for(const system of ["light","dark"]) for(const preference of ["light","dark","system"]) {
    const page=await browser.newPage({colorScheme:system});
    await page.addInitScript(preference=>localStorage.setItem("echoagent.theme",preference),preference);
    await page.route("**/src/main.tsx",route=>route.abort());
    await page.goto(address(server));
    const expected=preference==="system"?system:preference;
    assert.equal(await page.locator("html").getAttribute("data-theme"),expected,`First paint ${preference}/${system}`);
    await page.reload();assert.equal(await page.locator("html").getAttribute("data-theme"),expected);
    await page.unroute("**/src/main.tsx");
    await page.goto(`${address(server)}/__ui-review?surface=personalize&theme=system`,{waitUntil:"networkidle"});
    assert.equal(await page.locator("html").getAttribute("data-theme"),system);
    const opposite=system==="dark"?"light":"dark";await page.emulateMedia({colorScheme:opposite});
    await page.waitForFunction(theme=>document.documentElement.dataset.theme===theme,opposite);
    assert.equal(await page.evaluate(()=>localStorage.getItem("echoagent.theme")),"system");
    await page.close();
  }
  writeFileSync(join(output,"evidence.json"),JSON.stringify(evidence,null,2));
  console.log(`Design system passed: ${evidence.length} surface/state observations; screenshots and computed styles: ${output}`);
} finally { await browser?.close();await groupServer.close();await server.close(); }
