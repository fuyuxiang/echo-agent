import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

// Render production components with the existing isolated IPC fixtures. The
// baseline is pinned to the revision preceding the workspace redesign.
const baseline = process.argv.includes("--baseline");
const output = reviewOutput(`/private/tmp/echo-workspace-design/${baseline ? "before" : "after"}`);
const revision = process.env.UI_REVIEW_BASELINE_REVISION || "c44d09e513472b22a64f607d758c861a0e6ddf42";
const server = await createUiReviewServer(0, { baseline, baselineRevision: revision, currentFixtures: true });
let browser;
const evidence = [];
const surfaces = (process.env.UI_REVIEW_SURFACES || "home,workbench,projects,management-experts,management-knowledge,automation-templates,resource-groups,organization-shell,page:技能,page:连接器,page:插件,meeting,coding,general,model,personal-memory,automation-edit,execution,modal-long,trust,search").split(",");
const scenarios = process.env.UI_REVIEW_SCENARIOS ? JSON.parse(process.env.UI_REVIEW_SCENARIOS)
  : [[1440,900,"light",13],[1440,900,"dark",13],[1280,800,"light",13],[1280,800,"dark",13],[1024,680,"light",13],[1024,680,"dark",13],[1024,680,"dark",18]];
try {
  browser = await (await reviewChromium()).launch({ headless:true, executablePath:process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const [width,height,theme,font] of scenarios) {
    const page = await browser.newPage({ viewport:{width,height}, colorScheme:theme });
    await page.addInitScript(font => {
      // Match the real persisted preference before components initialize.
      // Sandboxed artifact frames intentionally have no local storage access.
      try { localStorage.setItem("echoagent.fontSize", String(font)); } catch {}
    }, font);
    page.setDefaultNavigationTimeout(60000);
    for (const spec of surfaces) {
      const errors = [];
      const onError = error => errors.push(error.message);
      page.on("pageerror", onError);
      const [surface,label] = spec.split(":");
      const params = new URLSearchParams({surface,theme,font:String(font),platform:"mac",extensionState:"populated"});
      if (label) params.set("label",label);
      await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?${params}`, {waitUntil:"networkidle"});
      await page.locator("#root > *").first().waitFor();
      await page.evaluate(font => { document.documentElement.style.fontSize = `${16 * font / 13}px`; }, font);
      // Font preferences and container queries reflow the native window row.
      // Capture the settled paint, as the interactive layout checks do.
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await page.mouse.move(0,0);
      const geometry = await page.evaluate(() => ({
        overflow:document.documentElement.scrollWidth > innerWidth + 1,
        fontPreference:localStorage.getItem("echoagent.fontSize"),
        main:document.querySelector(".app__main")?.getBoundingClientRect().toJSON(),
        dialogs:[...document.querySelectorAll('[aria-modal="true"]')].map(e=>e.getBoundingClientRect().toJSON()),
        headings:[...document.querySelectorAll("h1,h2")].filter(e=>e.getBoundingClientRect().height>0).map(e=>({text:e.textContent,size:getComputedStyle(e).fontSize})),
        headerControls:[...document.querySelectorAll(".capability-page__header button,.capability-page__header input,.settings-modal__toolbar button,.meeting-workbench__toolbar button,.session-header button,.coding-workbench__topbar button")]
          .filter(e=>e.checkVisibility()).map(e=>{
            const r=e.getBoundingClientRect();
            return {label:e.getAttribute("aria-label") || e.textContent,reachable:e.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)),...r.toJSON()};
          }),
        windowControls:[...document.querySelectorAll(".window-sidebar-navigation button")].map(e=>{
          const r=e.getBoundingClientRect();
          return {label:e.getAttribute("aria-label"),visible:e.checkVisibility(),reachable:e.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)),...r.toJSON()};
        }),
      }));
      const filename = `${spec.replaceAll(":","-")}-${width}-${theme}-${font}.png`;
      await page.screenshot({path:join(output,filename)});
      evidence.push({spec,width,height,theme,font,filename,geometry,errors});
      writeFileSync(join(output,"evidence.json"),JSON.stringify({baseline,revision,currentRevision:execFileSync("git",["rev-parse","HEAD"],{encoding:"utf8"}).trim(),evidence},null,2));
      if (!baseline) {
        assert.deepEqual(errors,[],`${filename}: browser errors`);
        assert.equal(geometry.fontPreference,String(font),`${filename}: reading preference was reset`);
        assert(!geometry.overflow,`${filename}: document overflow`);
        if (geometry.main && geometry.windowControls.length) {
          assert.equal(geometry.main.y,0,`${filename}: main column must reach the window top`);
          assert.equal(geometry.main.bottom,height,`${filename}: main column must fill the window height`);
        }
        for (const r of geometry.dialogs) assert(r.x>=-1 && r.y>=-1 && r.right<=width+1 && r.bottom<=height+1,`${filename}: dialog outside window`);
        for (const r of geometry.headerControls) {
          assert(r.x>=-1 && r.y>=-1 && r.right<=width+1 && r.bottom<=height+1,`${filename}: header action outside window: ${r.label}`);
          assert(r.reachable,`${filename}: header action is covered: ${r.label}`);
        }
        for (const r of geometry.windowControls) assert(r.visible && r.reachable,`${filename}: window navigation is covered: ${r.label}`);
      }
      page.off("pageerror",onError);
    }
    console.log(`${baseline?"Before":"After"}: ${width}×${height} ${theme} font ${font}, ${surfaces.length} surfaces`);
    await page.close();
  }
  console.log(`Saved ${evidence.length} screenshots and geometry records to ${output}`);
} finally { await browser?.close(); await server.close(); }
