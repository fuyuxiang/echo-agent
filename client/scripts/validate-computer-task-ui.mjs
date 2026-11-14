import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createUiReviewServer, reviewChromium, reviewOutput } from './ui-review-runtime.mjs';
const output=reviewOutput(mkdtempSync(join(tmpdir(),'echo-computer-task-ui-')));
const server=await createUiReviewServer(0,{fixture:'/scripts/fixtures/computer-task-review.tsx'});
let browser;
try {
  browser=await(await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'});
  const evidence=[];
  for (const [width,height,theme] of [[1024,680,'light'],[1024,680,'dark'],[1440,900,'light']]) {
    const page=await browser.newPage({viewport:{width,height}}), errors=[];
    page.on('pageerror',e=>errors.push(e.message));
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=workbench&state=empty&theme=${theme}&platform=mac`);
    await page.getByRole('button',{name:'停止生成',exact:true}).waitFor();
    assert.equal(await page.locator('.computer-task,.computer-task-window').count(),0);
    for (const name of ['暂停并接管','继续任务','悬浮']) assert.equal(await page.getByRole('button',{name,exact:true}).count(),0);
    await page.getByRole('button',{name:'停止生成',exact:true}).click();
    await page.getByRole('button',{name:'发送',exact:true}).waitFor();
    assert.equal(await page.evaluate(()=>window.workbenchCalls.some(c=>c.command==='review_cancel'&&c.args.action==='stop')),true);
    const input=page.getByRole('textbox',{name:'输入问题或任务'});
    await input.fill('接着完成刚才的任务，保留我的修改'); await input.press('Enter');
    assert.equal(await page.evaluate(()=>window.workbenchCalls.some(c=>c.command==='review_send'&&c.args.text==='接着完成刚才的任务，保留我的修改')),true);
    for (const phase of ['running','result_unknown','completed','stopped']) {
      await page.evaluate(phase=>window.computerTaskReview.setPhase(phase),phase);
      const process=page.getByRole('region',{name:'任务进展'}).last();
      const disclosure=process.getByRole('button',{name:/展开过程|查看过程|收起过程/}).first();
      if(await disclosure.getAttribute('aria-expanded')!=='true') await disclosure.click();
      await process.getByLabel('应用操作情况').waitFor();
      assert.equal(await page.locator('.chatview__footer .computer-task').count(),0);
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
      await page.screenshot({path:join(output,`${width}-${theme}-${phase}.png`)});
      evidence.push({width,height,theme,phase});
    }

    for (const blocker of ['message_text_required','input_conflict']) {
      await page.evaluate(blocker=>window.computerTaskReview.setPhase('waiting_user',blocker),blocker);
      await page.getByText(blocker==='message_text_required'?'请告诉我需要发送的消息内容。':'目标应用正在被使用，可以直接发送消息继续或调整要求。',{exact:true}).first().waitFor();
      assert.equal(await page.getByRole('button',{name:'停止生成',exact:true}).count(),0);
      assert.equal(await page.getByRole('button',{name:'继续任务',exact:true}).count(),0);
      await page.screenshot({path:join(output,`${width}-${theme}-${blocker}.png`)});
      evidence.push({width,height,theme,phase:'waiting_user',blocker});
    }
    assert.equal(await page.evaluate(()=>window.computerTaskReview.calls.some(c=>/automation_(open_control|control_session|resume|pause)/.test(c.command))),false);
    assert.deepEqual(errors,[]); await page.close();
  }
  writeFileSync(join(output,'evidence.json'),JSON.stringify({kind:'synthetic-production-chat',evidence},null,2));
  console.log(JSON.stringify({passed:evidence.length,output}));
} finally {await browser?.close();await server.close();}
