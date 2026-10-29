import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
const output=reviewOutput(mkdtempSync(join(tmpdir(),"echo-unified-tool-modes-")));
console.log(`Unified tool modes evidence: ${output}`);
const server=await createUiReviewServer(0),base=`http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
let browser,page;const evidence=[],errors=[];
try {
  browser=await(await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"});
  page=await browser.newPage();page.setDefaultTimeout(15000);page.setDefaultNavigationTimeout(60000);page.on("pageerror",error=>errors.push(error.message));
  await page.addInitScript(()=>{localStorage.removeItem("echoagent.drafts.v1");localStorage.removeItem("echoagent.session-controls.v1");});
  for(const [width,height,theme] of [[1440,900,"light"],[390,844,"dark"]]) for(const mode of ["default","browser_use","computer_use"]) for(const state of ["running","paused","complete","approval"]) {
    await page.setViewportSize({width,height});await page.goto(`${base}?surface=workbench&state=${state==="approval"?"running":state}&automationMode=${mode}&theme=${theme}${state==="approval"?"&automationApproval=1":""}`);
    await page.locator(".session-header").waitFor();
    if(width<720) await page.getByRole("button",{name:"收起侧边栏",exact:true}).click();
    const composer=page.locator(".echo-composer");await composer.waitFor();
    assert.equal(await page.locator(".task-execution-status, .automation-control").count(),0);
    assert.equal(await page.locator(".chatview__footer .session-turn-feedback").count(),0);
    assert.equal(await page.getByText(/正在处理任务，可随时说|可继续补充任务要求|浏览器数据与访问范围/).count(),0);
    if(state==="running") {
      await page.getByRole("button",{name:"停止生成",exact:true}).waitFor();
      assert.equal(await page.locator(".session-turn-feedback").count(),0);
      await page.locator(".echo-composer__input").fill("运行期间继续补充的要求");
      await page.getByRole("button",{name:"加入待发送队列",exact:true}).waitFor();
    }
    if(state==="paused") {
      const feedback=page.locator(".chatview__scroll .session-turn-feedback");await feedback.getByRole("button",{name:"继续任务",exact:true}).waitFor();
      assert.equal(await page.locator(".session-turn-feedback").count(),1);
      await feedback.scrollIntoViewIfNeeded();await feedback.getByRole("button",{name:"继续任务",exact:true}).click({trial:true});
    }
    if(state==="approval") {
      const card=page.getByRole("region",{name:"操作授权"});await card.waitFor();
      assert.equal(await card.getAttribute("class"),"perm-inline");
      assert.equal(await card.locator("details[open]").count(),0);
      assert.ok(!(await card.textContent()).includes("hidden-token"));
      assert.equal(await card.getByText("https://example.com/send",{exact:true}).isVisible(),true,"approval destination is visible before expanding technical details");
      assert.equal(await card.getByText("发送",{exact:true}).isVisible(),true,"approval target is visible");
      const geometry=await card.evaluate(el=>{const r=el.getBoundingClientRect();const c=document.querySelector(".echo-composer").getBoundingClientRect();return{width:r.width,composerWidth:c.width,overflow:el.scrollWidth>el.clientWidth+1};});
      assert.ok(Math.abs(geometry.width-geometry.composerWidth)<1);assert.equal(geometry.overflow,false);
      await page.screenshot({path:join(output,`${mode}-${state}-${width}-${theme}.png`)});
      await card.getByRole("button",{name:"允许一次",exact:true}).click();await card.waitFor({state:"hidden"});
    } else await page.screenshot({path:join(output,`${mode}-${state}-${width}-${theme}.png`)});
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1),false);
    evidence.push({width,height,theme,mode,state});
  }
  for(const [width,theme] of [[1280,"light"],[390,"dark"]]) {
    await page.setViewportSize({width,height:900});await page.goto(`${base}?surface=security&theme=${theme}&networkApproval=1`);
    await page.getByRole("heading",{name:"安全与权限",exact:true}).waitFor();
    await page.evaluate(async()=>{
      const {useSessionStore}=await import("/src/stores/session-store.ts");
      const {useSessionsStore}=await import("/src/stores/sessions-store.ts");
      useSessionStore.getState().setSession("browser-a");
      useSessionsStore.getState().setIndependent([{sessionId:"browser-a",title:"资料整理",cwd:"/review/a",status:"completed"},{sessionId:"browser-b",title:"另一个任务",cwd:"/review/b",status:"completed"}]);
      const original=window.__TAURI_INTERNALS__.invoke;window.browserSettingsCalls=[];
      window.__TAURI_INTERNALS__.invoke=async(command,args)=>{if(["automation_set_private_network","automation_clear_browser_data"].includes(command))window.browserSettingsCalls.push({command,args});return original(command,args);};
    });
    await page.locator("summary").filter({hasText:"浏览器数据与访问范围"}).click();
    const selector=page.getByRole("combobox",{name:"管理浏览器数据的任务"});await selector.selectOption("browser-b");
    const checkbox=page.getByRole("checkbox",{name:/允许访问本机与内网/});await checkbox.waitFor();
    assert.equal(await checkbox.isChecked(),false);await checkbox.click();
    const approval=page.getByRole("region",{name:"操作授权"});await approval.waitFor();
    assert.equal(await checkbox.isChecked(),false,"network access remains disabled until explicit approval");
    assert.equal(await approval.evaluate(el=>Boolean(el.closest(".browser-task-settings"))),true,"the request must appear inside the settings modal");
    await approval.getByRole("button",{name:"允许一次",exact:true}).click();await approval.waitFor({state:"hidden"});
    await page.waitForFunction(()=>document.querySelector(".browser-task-settings input[type=checkbox]")?.checked===true);
    await page.waitForFunction(()=>window.browserSettingsCalls.length===1);
    assert.deepEqual(await page.evaluate(()=>window.browserSettingsCalls[0].args),{sessionId:"browser-b",allowed:true});
    await page.getByRole("button",{name:"清除浏览器数据",exact:true}).click();
    const dialog=page.getByRole("alertdialog",{name:"清除该任务的浏览器数据？"});await dialog.waitFor();
    await dialog.getByRole("button",{name:"取消",exact:true}).click();
    assert.equal(await page.evaluate(()=>window.browserSettingsCalls.length),1);
    await page.getByRole("button",{name:"清除浏览器数据",exact:true}).click();await dialog.getByRole("button",{name:"清除数据",exact:true}).click();
    await page.getByText("该任务的浏览器数据已清除。",{exact:true}).waitFor();
    assert.deepEqual(await page.evaluate(()=>window.browserSettingsCalls[1].args),{sessionId:"browser-b"});
    assert.equal(await page.getByRole("button",{name:"清除浏览器数据",exact:true}).isEnabled(),false);
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1),false);
    await page.screenshot({path:join(output,`browser-settings-${width}-${theme}.png`)});
    evidence.push({width,theme,state:"browser-settings"});
  }
  assert.deepEqual(errors,[]);writeFileSync(join(output,"evidence.json"),JSON.stringify({layouts:evidence,errors},null,2));
  console.log(JSON.stringify({ok:true,output,layouts:evidence.length,errors},null,2));
} catch(error) { if(page)await page.screenshot({path:join(output,"failure.png")}).catch(()=>{});throw error; }
finally {await browser?.close();await server.close();}
