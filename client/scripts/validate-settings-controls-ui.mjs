import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { inspectButtonBackgrounds, assertButtonBackgrounds } from "./ui-button-audit.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-settings-controls-")));
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}`;
const evidence = [];
const surfaces = ["model", "notify-channels", "agent-mail", "weixin-unconnected", "weixin-connected", "general", "personalize", "shortcuts", "memory", "personal-memory", "agent-settings", "usage", "cloud-storage", "data", "archived", "security", "help"];
const variants = [[1440,1000,"light",13], [1440,1000,"dark",13], [1024,680,"light",18], [1024,680,"dark",18]];
let browser;
async function inspect(page, context) {
  await page.mouse.move(0,0);
  const audit = await inspectButtonBackgrounds(page, context.theme);
  assertButtonBackgrounds(audit, JSON.stringify(context));
  const controls = await page.locator(".settings-modal").evaluate(root => {
    const visible = element => element.getClientRects().length && getComputedStyle(element).visibility !== "hidden";
    const probe = document.createElement("span"); root.append(probe);
    const token = (property, value) => { probe.style[property] = value; return getComputedStyle(probe)[property]; };
    const palette = {
      primary: token("backgroundColor", "var(--echo-button-primary-bg)"),
      secondary: token("backgroundColor", "var(--echo-button-secondary-bg)"),
      border: token("borderTopColor", "var(--echo-button-secondary-border)"),
      foreground: token("color", "var(--echo-button-secondary-fg)"),
      danger: token("color", "var(--echo-button-danger-fg)"),
      selected: token("backgroundColor", "var(--echo-brand-soft)"),
      radius: token("borderTopLeftRadius", "var(--echo-radius-control)"),
      fontSize: token("fontSize", "var(--echo-control-font-size)"),
      fontFamily: getComputedStyle(root).fontFamily,
    };
    probe.remove();
    const buttons = [...root.querySelectorAll("button:is(.settings-btn,.form-button,.echo-button,.btn)")].filter(visible).map(element => {
      const css = getComputedStyle(element);
      const ghost = /(?:settings-btn|form-button|echo-button|btn)--ghost/.test(element.className);
      const primary = /(?:settings-btn|form-button|echo-button|btn)--primary/.test(element.className);
      const danger = /(?:settings-btn|form-button|echo-button|btn)--danger(?:\s|$)/.test(element.className);
      const selected = element.matches('.notify-panel__btn[aria-checked="true"]');
      const expectedBackground = ghost ? "rgba(0, 0, 0, 0)" : primary ? palette.primary : selected ? palette.selected : palette.secondary;
      return { name: (element.getAttribute("aria-label") || element.textContent).trim(), classes:element.className,
        background:css.backgroundColor, expectedBackground, border:css.borderTopColor,
        expectedBorder:ghost || primary ? "rgba(0, 0, 0, 0)" : palette.border,
        color:css.color, danger, expectedDanger:palette.danger, radius:css.borderTopLeftRadius,
        fontSize:css.fontSize, fontFamily:css.fontFamily, height:element.getBoundingClientRect().height, opacity:css.opacity, disabled:element.disabled,
        clipped:element.scrollWidth > element.clientWidth + 2,
      };
    });
    const exceptions = ".settings-search__results > button,.personal-memory__toolbar [role=tab],.settings-navigation__item,.settings-view-tabs__item,.settings-modal__close,.model-connections__source,.models-settings-panel__editor-close,.model-connection-editor__chips button,.models-settings-panel__advanced-toggle,.theme-toggle__btn,.resources-panel__tab,.memory-editor__close,.archived-settings__scope,.notification-filter,.notification-row__mark,.notification-row__session,.notify-advanced__toggle,.quota-panel__period-btn,.storage-panel__entry-name--button,.help-card";
    const unmanaged = [...root.querySelectorAll("button")].filter(visible).filter(element => !element.matches(`.settings-btn,.form-button,.echo-button,.btn,${exceptions}`)).map(element => ({name:element.textContent.trim(),classes:element.className}));
    const fields = [...root.querySelectorAll("input:not([type=checkbox]):not([type=radio]):not([type=range]):not([type=hidden]),select,textarea")].filter(visible).filter(element => !element.closest(".archived-settings__search")).map(element => {
      const css = getComputedStyle(element);
      return { name:element.getAttribute("aria-label") || element.id || element.getAttribute("placeholder"), classes:element.className,
        border:css.borderTopColor, radius:css.borderTopLeftRadius,
        focused:element === document.activeElement, invalid:element.getAttribute("aria-invalid") === "true",
      };
    });
    const overflow = root.scrollWidth > root.clientWidth + 1;
    return {palette,buttons,fields,unmanaged,overflow};
  });
  evidence.push({context,...controls});
  writeFileSync(join(output,"controls.json"),JSON.stringify(evidence,null,2));
  assert.deepEqual(controls.unmanaged,[],`Unmanaged action: ${JSON.stringify(context)}`);
  assert(!controls.overflow, `Settings overflow: ${JSON.stringify(context)}`);
  for (const button of controls.buttons) {
    const label = `${JSON.stringify(context)} ${button.name}`;
    assert.equal(button.background,button.expectedBackground,`${label}: background`);
    assert.equal(button.border,button.expectedBorder,`${label}: border`);
    assert.equal(button.radius,controls.palette.radius,`${label}: radius`);
    assert.equal(button.fontSize,controls.palette.fontSize,`${label}: typography`);
    assert.equal(button.fontFamily,controls.palette.fontFamily,`${label}: font family`);
    if(button.danger) assert.equal(button.color,button.expectedDanger,`${label}: danger foreground`);
    assert(!button.clipped,`${label}: clipped label`);
    assert(button.height >= 31.5, `${label}: insufficient target height`);
    if(button.disabled) assert.equal(button.opacity,"0.5",`${label}: disabled state`);
  }
  for (const field of controls.fields) {
    if(!field.focused && !field.invalid) assert.equal(field.border,controls.palette.border,`${JSON.stringify(context)} ${field.name}: field border`);
    assert.equal(field.radius,controls.palette.radius,`${JSON.stringify(context)} ${field.name}: field radius`);
  }
}
try {
  browser = await (await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"});
  for (const [width,height,theme,font] of variants) {
    const page = await browser.newPage({viewport:{width,height}});
    page.setDefaultNavigationTimeout(60000);
    const errors=[]; page.on("pageerror",error=>errors.push(error.message));
    await page.addInitScript(font=>{
      localStorage.setItem("echoagent.fontSize", String(font));
      const observer=new MutationObserver(()=>{if(document.documentElement){document.documentElement.style.fontSize=`${16*font/13}px`;observer.disconnect();}});
      observer.observe(document,{childList:true,subtree:true});
    },font);
    for(const surface of surfaces) {
      await page.goto(`${base}/__ui-review?surface=${surface}&theme=${theme}&settingsDesign=1&notificationState=records`,{waitUntil:"networkidle"});
      await page.locator(".settings-modal").waitFor();
      assert(Math.abs(await page.evaluate(() => parseFloat(getComputedStyle(document.documentElement).fontSize)) - 16*font/13) < .01,`${surface}: requested reading size was reset`);
      await inspect(page,{surface,width,height,theme,font});
      if(width===1440 && ["model","agent-mail","weixin-unconnected","notify-channels","general","personalize","memory","usage","data","security","help","agent-settings","weixin-connected","cloud-storage"].includes(surface)) {
        await page.locator(".settings-modal__location").click();
        await page.screenshot({path:join(output,`${surface}-${theme}.png`)});
      }
      if (width === 1024 && ["general", "usage", "security"].includes(surface)) await page.screenshot({path:join(output,`${surface}-${theme}-1024.png`)});
      if (surface === "cloud-storage") {
        await page.getByRole("combobox", { name: "选择云存储源" }).selectOption("review-dav");
        await page.getByText("浏览云端文件", { exact: true }).waitFor();
      }
      const secondarySelector = {
        model: ".model-connections__facts",
        memory: ".personal-memory__legacy .resources-panel",
        security: ".permission-rule-builder",
        data: ".settings-path-value",
        "agent-settings": 'input[aria-label="OTLP 遥测端点"]',
        usage: ".quota-panel__config-grid",
        "cloud-storage": ".storage-panel__breadcrumb",
      }[surface];
      if (secondarySelector) assert.equal(await page.locator(secondarySelector).isVisible(),false,`${surface}: secondary details crowd the initial page`);
      if (surface === "memory") await page.getByText("高级：记忆文档与会话摘要", {exact:true}).click();
      const disclosures = page.locator(".settings-disclosure");
      for (let index = 0; index < await disclosures.count(); index += 1) {
        const details = disclosures.nth(index);
        if (!await details.evaluate(element => element.open)) await details.locator(":scope > summary").click();
      }
      if (secondarySelector) await page.locator(secondarySelector).waitFor({state:"visible"});
      if (secondarySelector) assert.equal(await page.locator(secondarySelector).isVisible(),true,`${surface}: secondary controls cannot be reached`);
      if (await disclosures.count()) await inspect(page,{surface:`${surface}-details`,width,height,theme,font});
      if(surface==="model") {
        await page.getByRole("button",{name:"添加个人连接",exact:true}).first().click();
        await inspect(page,{surface:"model-editor",width,height,theme,font});
        await page.getByRole("dialog",{name:"添加个人连接",exact:true}).getByRole("button",{name:"高级设置",exact:true}).click();
        await inspect(page,{surface:"model-editor-advanced",width,height,theme,font});
      }
      if(surface==="cloud-storage") {
        await page.getByRole("button",{name:"添加存储源",exact:true}).click();
        await inspect(page,{surface:"cloud-editor",width,height,theme,font});
      }
      if(surface==="notify-channels") {
        await page.getByRole("button",{name:/推送到其他应用/}).click();
        await page.getByRole("button",{name:"添加渠道",exact:true}).click();
        await inspect(page,{surface:"notify-editor",width,height,theme,font});
      }
      if(surface==="personal-memory") {
        await page.getByRole("button",{name:"添加记忆",exact:true}).click();
        await inspect(page,{surface:"memory-editor",width,height,theme,font});
        await page.getByText("更多选项", {exact:true}).click();
        await page.getByLabel("主题（可选）").waitFor();
        await inspect(page,{surface:"memory-editor-advanced",width,height,theme,font});
      }
    }
    assert.deepEqual(errors,[]);
    console.log(`Settings palette and controls passed at ${width}×${height}, ${theme}, font ${font}`);
    await page.close();
  }
  const stateChecks=[];
  for(const theme of ["light","dark"]) {
    const page=await browser.newPage({viewport:{width:1440,height:1000}});
    await page.goto(`${base}/__ui-review?surface=agent-mail&notificationState=records&theme=${theme}`,{waitUntil:"networkidle"});
    for(const name of ["刷新","清空"]) {
      const button=page.getByRole("button",{name,exact:true});
      await button.hover();
      const hovered=await button.evaluate(element=>{
        const css=getComputedStyle(element), probe=document.createElement("span");
        const danger=element.classList.contains("settings-btn--danger");
        probe.style.backgroundColor=danger?"var(--echo-button-danger-bg-soft-hover)":"var(--echo-button-secondary-bg-hover)";
        element.append(probe); const expected=getComputedStyle(probe).backgroundColor; probe.remove();
        return {actual:css.backgroundColor,expected,border:css.borderTopColor};
      });
      assert.equal(hovered.actual,hovered.expected,`${theme} ${name}: hover drift`);
      await page.keyboard.press("Tab"); await button.focus();
      assert.equal(await button.evaluate(element=>getComputedStyle(element).outlineStyle),"solid",`${theme} ${name}: keyboard focus missing`);
      stateChecks.push({theme,name,hovered});
    }
    await page.goto(`${base}/__ui-review?surface=weixin-unconnected&theme=${theme}`,{waitUntil:"networkidle"});
    const primary=page.getByRole("button",{name:"获取绑定二维码",exact:true});
    await primary.hover();
    const primaryHover=await primary.evaluate(element=>{
      const probe=document.createElement("span");probe.style.backgroundColor="var(--echo-button-primary-bg-hover)";element.append(probe);
      const expected=getComputedStyle(probe).backgroundColor;probe.remove();return {actual:getComputedStyle(element).backgroundColor,expected};
    });
    assert.equal(primaryHover.actual,primaryHover.expected,`${theme}: primary hover drift`);
    const bounds=await primary.boundingBox();
    await page.mouse.move(bounds.x+bounds.width/2,bounds.y+bounds.height/2);await page.mouse.down();
    const pressed=await primary.evaluate(element=>{
      const probe=document.createElement("span");probe.style.backgroundColor="var(--echo-button-primary-bg-active)";element.append(probe);
      const expected=getComputedStyle(probe).backgroundColor;probe.remove();return {actual:getComputedStyle(element).backgroundColor,expected};
    });
    assert.equal(pressed.actual,pressed.expected,`${theme}: primary pressed drift`);
    await page.mouse.move(0,0);await page.mouse.up();
    await page.goto(`${base}/__ui-review?surface=agent-settings&theme=${theme}`,{waitUntil:"networkidle"});
    const field=page.locator(".settings-input").first();await field.focus();
    assert.equal(await field.evaluate(element=>getComputedStyle(element).outlineStyle),"solid",`${theme}: field focus missing`);
    await page.close();
  }
  // Settings must keep the project workspace's existing teal action palette.
  const page=await browser.newPage({viewport:{width:1440,height:1000}});
  await page.goto(`${base}/__ui-review?surface=projects`,{waitUntil:"networkidle"});
  assertButtonBackgrounds(await inspectButtonBackgrounds(page,"light"),"Project palette after settings changes");
  await page.close();
  writeFileSync(join(output,"states.json"),JSON.stringify(stateChecks,null,2));
  console.log(`Passed ${evidence.length} settings page/editor checks, hover/pressed/focus states and the project palette check; evidence: ${output}`);
} finally {await browser?.close();await server.close();}
