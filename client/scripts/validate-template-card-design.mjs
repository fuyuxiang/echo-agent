import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output=reviewOutput(mkdtempSync(join(tmpdir(),"echo-template-cards-")));
const server=await createUiReviewServer(0);
const origin=`http://127.0.0.1:${server.httpServer.address().port}`;
const layouts=[];
let browser;
const presentation=async page=>page.locator(".template-card").first().evaluate(card=>{
  const style=(element,keys)=>{const css=getComputedStyle(element);return Object.fromEntries(keys.map(key=>[key,css[key]]));};
  const typography=["fontFamily","fontSize","fontWeight","lineHeight","color"];
  const grid=getComputedStyle(card.parentElement);
  return {
    card:style(card,["minHeight","padding","gap","borderRadius","borderColor","borderWidth","backgroundColor","boxShadow"]),
    icon:style(card.querySelector(".resource-icon"),["width","height","borderRadius","backgroundColor","color"]),
    iconGraphic:style(card.querySelector(".resource-icon svg"),["width","height","color"]),
    title:style(card.querySelector(".resource-template__title"),typography),
    description:style(card.querySelector(".resource-template__description"),typography),
    grid:{columns:grid.gridTemplateColumns.split(" ").length,gap:grid.gap},
  };
});
try {
  browser=await(await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"});
  for(const [width,height] of [[1024,768],[1280,800],[1440,1080],[1920,1080]]) {
    for(const font of [13,18]) for(const theme of ["light","dark"]) {
      const page=await browser.newPage({viewport:{width,height}});
      page.setDefaultNavigationTimeout(60000);
      await page.addInitScript(font=>{
        const observer=new MutationObserver(()=>{if(document.documentElement){document.documentElement.style.fontSize=`${16*font/13}px`;observer.disconnect();}});
        observer.observe(document,{childList:true,subtree:true});
      },font);
      const errors=[];page.on("pageerror",error=>errors.push(error.message));
      await page.goto(`${origin}/__ui-review?surface=projects&theme=${theme}`,{waitUntil:"networkidle"});
      await page.locator(".template-card").first().waitFor();
      const project={};
      for(const collapsed of [false,true]) {
        if(collapsed) await page.getByRole("button",{name:"收起侧边栏",exact:true}).click();
        project[String(collapsed)]=await presentation(page);
      }
      await page.getByRole("button",{name:"展开侧边栏",exact:true}).click();
      await page.goto(`${origin}/__ui-review?surface=automation-templates&theme=${theme}`,{waitUntil:"networkidle"});
      await page.getByRole("heading",{name:"自动化任务模板",exact:true}).waitFor();
      assert.equal(await page.locator(".template-card").count(),12);
      for(const collapsed of [false,true]) {
        if(collapsed) await page.getByRole("button",{name:"收起侧边栏",exact:true}).click();
        const automation=await presentation(page);
        assert.deepEqual(automation,project[String(collapsed)],`Template styles differ: ${JSON.stringify({width,height,font,theme,collapsed})}`);
        assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),"Template grid causes horizontal overflow");
        layouts.push({width,height,font,theme,collapsed,presentation:automation});
        if(!collapsed && width===1440 && ((font===13 && theme==="light") || (font===18 && theme==="dark"))) await page.screenshot({path:join(output,`automation-templates-${font}-${theme}.png`)});
      }
      const first=page.locator(".template-card").first();
      await first.focus();
      await page.keyboard.press("Tab");
      const next=page.locator(".template-card").nth(1);
      assert(await next.evaluate(element=>element===document.activeElement),"Keyboard cannot move between template choices");
      assert.equal(await next.evaluate(element=>getComputedStyle(element).outlineStyle),"solid","Keyboard focus is missing from template choices");
      assert.deepEqual(errors,[]);
      await page.close();
    }
    console.log(`Template presentation passed at ${width}×${height}`);
  }
  // Exercise every real template against the production editor without saving.
  const page=await browser.newPage({viewport:{width:1280,height:800}});
  page.setDefaultNavigationTimeout(60000);
  await page.goto(`${origin}/__ui-review?surface=automation-templates`,{waitUntil:"networkidle"});
  const templates=await page.evaluate(async()=>{
    const {createAutomationTemplates}=await import("/src/components/automation/template-config.ts");
    return createAutomationTemplates().map(({title,prompt})=>({title,prompt}));
  });
  for(const template of templates) {
    await page.goto(`${origin}/__ui-review?surface=automation-templates`,{waitUntil:"networkidle"});
    await page.locator(".template-card").filter({has:page.getByText(template.title,{exact:true})}).click();
    assert.equal(await page.getByRole("textbox",{name:"名称",exact:true}).inputValue(),template.title);
    assert.equal(await page.getByRole("textbox",{name:"任务要求",exact:true}).inputValue(),template.prompt);
  }
  writeFileSync(join(output,"presentation.json"),JSON.stringify(layouts,null,2));
  console.log(`Passed ${layouts.length} project/automation style comparisons and ${templates.length} template editor prefills; evidence: ${output}`);
} finally {await browser?.close();await server.close();}
