// Supplement computed-color checks with the actual rasterized background.
// Uses the PNG reader bundled in the existing optional Playwright installation;
// no browser tooling or image dependency is added to the application bundle.
import assert from "node:assert/strict";
import { join } from "node:path";
import { writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { annotateUiReviewFailure, createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { inspectRenderedColors } from "./ui-color-audit.mjs";
import { reviewPng } from "./ui-review-tools.mjs";

const PNG = reviewPng();
const output = reviewOutput(join(tmpdir(),"echo-complex-color-audit"));
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
const evidence = [], manual = [];
const projectHeader = [
  { className: "management-toolbar__title", text: "项目" },
  { className: "echo-button__content", text: "新建项目" },
];
const headerClasses = ["capability-page__title", "capability-page__description",
  ...projectHeader.map(({ className }) => className)];
const rgba = color => {
  assert.match(color,/^(?:rgba?\(|color\(srgb )/);
  const c = color.match(/[\d.]+/g).map(Number);
  return [...c.slice(0,3).map(v => color.startsWith("color(srgb ") ? v*255 : v),c[3] ?? 1];
};
const luminance = rgb => rgb.slice(0,3).map(c => c/255).map(c => c <= .04045 ? c/12.92 : ((c+.055)/1.055)**2.4).reduce((s,c,i) => s+c*[.2126,.7152,.0722][i],0);
const contrast = (fg,bg) => {
  const painted = fg.slice(0,3).map((c,i) => c*fg[3]+bg[i]*(1-fg[3]));
  const [a,b] = [luminance(painted),luminance(bg)].sort((a,b)=>b-a);
  return (a+.05)/(b+.05);
};
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true,
    ...(process.env.CHROME_BIN ? { executablePath: process.env.CHROME_BIN } : {}) });
  for (const [width,height,font] of [[1024,680,11],[1024,680,18],[1280,800,13],[1920,1080,11],[1920,1080,18]]) for (const theme of ["light","dark"]) {
    const page = await browser.newPage({viewport:{width,height},deviceScaleFactor:1,colorScheme:theme});
    page.setDefaultNavigationTimeout(60_000);
    const errors = [];
    page.on("pageerror",error=>errors.push(error.message));
    for (const spec of ["home","projects","meeting","organization-shell","controls","color-system","cloud-storage"]) {
      const [surface,label] = spec.split(":");
      const params = new URLSearchParams({surface,theme,extensionState:"populated"});
      if (label) params.set("label",label);
      if (surface === "cloud-storage") params.set("settingsDesign","1");
      await page.goto(`${base}?${params}`,{waitUntil:"networkidle"});
      await page.evaluate(font=>document.documentElement.style.fontSize=`${16*font/13}px`,font);
      if (surface === "projects") {
        const toolbar = page.locator(".project-management .management-toolbar");
        await toolbar.getByRole("heading", { name: "项目", exact: true }).waitFor();
        await toolbar.getByRole("button", { name: "新建项目", exact: true }).waitFor();
      }
      if (surface === "color-system") {
        await page.getByRole("textbox",{name:"交付说明",exact:true}).fill("修改后的交付说明");
        await page.getByRole("combobox",{name:"交付工作区",exact:true}).selectOption("team");
      }
      const storageLabel = surface === "cloud-storage"
        ? await page.getByRole("combobox",{name:"选择云存储源",exact:true}).evaluate(element=>element.selectedOptions[0]?.label) : null;
      const colors = await page.evaluate(inspectRenderedColors);
      const samples = await page.evaluate(items => items.flatMap(item => {
        return [...document.querySelectorAll(item.selector)].flatMap(e => {
          const direct = [...e.childNodes].filter(n=>n.nodeType===Node.TEXT_NODE).map(n=>n.textContent).join("").trim();
          const placeholder = (e instanceof HTMLInputElement || e instanceof HTMLTextAreaElement) && !e.value && e.placeholder;
          const value = e instanceof HTMLInputElement || e instanceof HTMLTextAreaElement ? e.value
            : e instanceof HTMLSelectElement ? [...e.selectedOptions].map(option=>option.label).join(" ") : "";
          if (String(placeholder || value || direct).slice(0,90) !== item.text) return [];
          const style = getComputedStyle(e);
          const complexForeground = style.backgroundClip === "text" || style.webkitBackgroundClip === "text";
          let opacity = 1;
          for (let p=e;p;p=p.parentElement) opacity *= Number(getComputedStyle(p).opacity);
          const rects = [];
          if (placeholder || value) {
            const r=e.getBoundingClientRect();
            rects.push({left:r.left+parseFloat(style.paddingLeft)+1,right:r.right-parseFloat(style.paddingRight)-1,top:r.top+r.height/2-parseFloat(style.fontSize)/2,bottom:r.top+r.height/2+parseFloat(style.fontSize)/2});
          } else for (const node of e.childNodes) if (node.nodeType===Node.TEXT_NODE && node.textContent.trim()) {
            const range=document.createRange(); range.selectNodeContents(node);
            rects.push(...[...range.getClientRects()].map(r=>({left:r.left,right:r.right,top:r.top,bottom:r.bottom})));
          }
          return [{...item,opacity,complexForeground,rects}];
        });
      }),[...colors.manual, ...colors.text.filter(item => item.selector.split(".").some(name => headerClasses.includes(name)))]
        .filter((item,index,items) => items.findIndex(other => other.selector === item.selector && other.text === item.text) === index));
      // Hide glyph paint only: inherited color, backgrounds, native boundaries,
      // geometry and currentColor icons retain their original computed values.
      await page.addStyleTag({content:"*{ -webkit-text-fill-color:transparent!important;text-shadow:none!important } *::placeholder{-webkit-text-fill-color:transparent!important}"});
      const image = PNG.sync.read(await page.screenshot({path:join(output,`${spec.replace(":","-")}-${theme}-${width}-${font}-background.png`)}));
      for (const sample of samples) {
        if (sample.complexForeground) { manual.push({spec,theme,width,font,selector:sample.selector,reason:"gradient glyph paint requires separate visual review"}); continue; }
        assert.equal(sample.opacity,1,"Group opacity requires a separate compositing measurement");
        const fg=rgba(sample.color); let minimum=Infinity, pixels=0, darkestBackdrop;
        for (const rect of sample.rects) for (let y=Math.max(0,Math.ceil(rect.top));y<Math.min(height,Math.floor(rect.bottom));y++) for (let x=Math.max(0,Math.ceil(rect.left));x<Math.min(width,Math.floor(rect.right));x++) {
          const i=(y*image.width+x)*4, bg=[...image.data.subarray(i,i+3)];
          const measured=contrast(fg,bg); pixels++;
          if (measured<minimum) { minimum=measured; darkestBackdrop=bg; }
        }
        if (!pixels) continue;
        evidence.push({spec,theme,width,height,font,selector:sample.selector,text:sample.text,color:sample.color,threshold:sample.threshold,minimum,worstBackdrop:darkestBackdrop,pixels});
      }
      if (surface === "cloud-storage") assert(storageLabel && evidence.some(item=>item.spec===spec && item.theme===theme && item.width===width && item.font===font && item.selector.startsWith("select.") && item.text===storageLabel),"The visible storage source label must be raster-sampled outside its decorative arrow");
      if (surface === "projects") for (const { className, text } of projectHeader) {
        assert(evidence.some(item=>item.spec===spec && item.theme===theme && item.width===width && item.font===font
          && item.text === text && item.selector.split(".").includes(className)),
        `Project header must be raster-sampled at ${width}/${theme}/${font}: ${text}`);
      }
      if (surface === "color-system") assert(evidence.some(item=>item.spec===spec && item.theme===theme && item.width===width && item.font===font && item.text==="团队交付工作区"),"The selected label must be sampled outside its native arrow");
      assert.deepEqual(errors,[],`${spec}/${theme}: browser errors`);
    }
    await page.close();
  }
  writeFileSync(join(output,"evidence.json"),JSON.stringify({evidence,manual},null,2));
  assert(evidence.length>0,"Raster samples must exercise real complex backgrounds");
  assert.deepEqual(evidence.filter(e=>e.minimum<e.threshold),[],"Complex-background text contrast failures");
  console.log(JSON.stringify({observations:evidence.length,minimum:Math.min(...evidence.map(e=>e.minimum)),pixels:evidence.reduce((n,e)=>n+e.pixels,0),manual:manual.length,output}));
} catch (error) {
  annotateUiReviewFailure(error);
  throw error;
} finally {
  await browser?.close();
  await server.close();
}
