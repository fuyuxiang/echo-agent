import assert from 'node:assert/strict';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createUiReviewServer, reviewChromium, reviewOutput } from './ui-review-runtime.mjs';
const output=reviewOutput(mkdtempSync(join(tmpdir(),'echo-browser-connection-ui-')));
const server=await createUiReviewServer(0,{fixture:'/scripts/fixtures/browser-connection-review.tsx'});
let browser;
try {
  browser=await(await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'});
  for(const [width,height,theme] of [[1024,768,'light'],[1024,768,'dark'],[390,844,'light']]){
    const page=await browser.newPage({viewport:{width,height}}),errors=[];
    page.setDefaultTimeout(20000);page.on('pageerror',error=>errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=security&theme=${theme}`,{waitUntil:'networkidle',timeout:60000});
    await page.locator('[data-setting-id="browser-data"] > summary').click();
    const panel=page.locator('.browser-task-settings');
    await panel.getByText('当前使用任务独立浏览器',{exact:true}).waitFor();
    await panel.getByRole('button',{name:'连接已登录浏览器',exact:true}).click();
    await panel.getByRole('textbox',{name:'浏览器连接地址',exact:true}).waitFor();
    assert.match(await panel.textContent(),/网页的请求不经过任务独立浏览器的网络隔离/);
    assert.match(await panel.getByRole('checkbox').getAttribute('type'),/checkbox/);
    await panel.getByRole('button',{name:'使用独立浏览器',exact:true}).scrollIntoViewIfNeeded();
    assert.equal(await panel.evaluate(el=>el.scrollWidth>el.clientWidth+1),false,'connection settings overflow their column');
    await page.screenshot({path:join(output,`${width}-${theme}-pairing.png`)});
    await panel.getByRole('button',{name:'使用独立浏览器',exact:true}).click();
    await panel.getByText('当前使用任务独立浏览器',{exact:true}).waitFor();
    assert.equal(await panel.getByRole('textbox',{name:'浏览器连接地址',exact:true}).count(),0);
    await page.evaluate(()=>window.browserConnectionReview.expireNext());
    await panel.getByRole('button',{name:'连接已登录浏览器',exact:true}).click();
    await panel.getByText('连接地址已过期，请重新生成连接地址，或使用独立浏览器。',{exact:true}).waitFor();
    await panel.getByRole('button',{name:'重新生成连接地址',exact:true}).click();
    await panel.getByRole('textbox',{name:'浏览器连接地址',exact:true}).waitFor();
    await page.evaluate(()=>window.browserConnectionReview.connect());
    await panel.getByText('已连接所选浏览器标签页',{exact:true}).waitFor();
    assert.equal(await panel.getByRole('textbox',{name:'浏览器连接地址',exact:true}).count(),0);
    await panel.getByRole('button',{name:'断开浏览器连接',exact:true}).click();
    await panel.getByText('浏览器尚未连接，可重新连接或使用独立浏览器',{exact:true}).waitFor();
    assert.equal(await page.evaluate(()=>window.browserConnectionReview.calls.filter(c=>c.command==='automation_use_isolated_browser').length),1,'disconnect must not silently change browser source');
    await page.screenshot({path:join(output,`${width}-${theme}-disconnected.png`)});
    assert.deepEqual(errors,[]);await page.close();
  }
  console.log(JSON.stringify({passed:3,output}));
} finally {await browser?.close();await server.close();}
