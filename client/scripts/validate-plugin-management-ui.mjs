import assert from "node:assert/strict";
import {mkdtempSync} from "node:fs";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {createUiReviewServer,reviewChromium} from "./ui-review-runtime.mjs";
const output=mkdtempSync(join(tmpdir(),"echo-plugin-management-"));
const server=await createUiReviewServer(0);
let browser;
try {
 browser=await (await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"});
 for(const width of [420,1280]) for(const theme of ["light","dark"]) {
  const page=await browser.newPage({viewport:{width,height:850}}),errors=[];
  page.on("pageerror",error=>errors.push(error.message));
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=capabilities&label=${encodeURIComponent("插件")}&extensionState=populated&pluginManagement=1&pluginRemoveFailure=1&theme=${theme}`);
  await page.getByRole("button",{name:"插件 需求评审 01更多操作",exact:true}).click();
  await page.getByRole("menuitem",{name:"卸载插件 需求评审 01",exact:true}).click();
  const dialog=page.getByRole("alertdialog");
  await dialog.getByRole("button",{name:"卸载插件",exact:true}).click();
  await dialog.getByRole("alert").getByText(/安装记录保留/).waitFor();
  assert.equal(await page.locator(".plugins-panel__item").filter({hasText:"需求评审 01"}).count(),1);
  await page.screenshot({path:join(output,`failure-${width}-${theme}.png`)});
  await dialog.getByRole("button",{name:"卸载插件",exact:true}).click();
  await dialog.waitFor({state:"detached"});
  assert.equal(await page.locator(".plugins-panel__item").filter({hasText:"需求评审 01"}).count(),0);
  await page.getByRole("button",{name:"插件 文档整理 02更多操作",exact:true}).click();
  await page.getByRole("menuitem",{name:"移除插件 文档整理 02",exact:true}).click();
  await page.getByRole("alertdialog").getByRole("button",{name:"移除配置",exact:true}).click();
  await page.getByRole("alertdialog").waitFor({state:"detached"});
  assert.equal(await page.locator(".plugins-panel__item").filter({hasText:"文档整理 02"}).count(),0);
  await page.locator(".plugins-panel__item-name").filter({hasText:"数据分析 03"}).waitFor();
  await page.getByRole("button",{name:"插件 数据分析 03更多操作",exact:true}).click();
  await page.getByRole("menuitem",{name:"来源目录",exact:true}).waitFor();
  await page.screenshot({path:join(output,`sources-${width}-${theme}.png`)});
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),"plugin actions must fit narrow width");
  assert.deepEqual(errors,[]);await page.close();
 }
 console.log(`Plugin management UI passed: uninstall failure/retry, configured-path removal, source ownership, light/dark and narrow/wide layouts. Screenshots: ${output}`);
}finally{await browser?.close();await server.close();}
