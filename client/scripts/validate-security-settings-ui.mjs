import assert from 'node:assert/strict';
import {mkdtempSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createUiReviewServer,reviewChromium,reviewOutput} from './ui-review-runtime.mjs';
const output=reviewOutput(mkdtempSync(join(tmpdir(),'echo-security-settings-')));
console.log('Evidence: '+output);
const server=await createUiReviewServer(0);
const base=`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=security&securitySettingsReview=1`;
const layouts=[];let browser;
const original=[{action:'deny',tool:'bash',pattern:'rm *'},{action:'ask',tool:'legacy-tool',pattern:'existing:*'}];
async function capture(page,name){
  for(const selector of ['.settings-modal__panel','.security-settings']) assert.equal(await page.locator(selector).evaluate(el=>el.scrollWidth>el.clientWidth+1),false,`${name}: ${selector} overflow`);
  const clipped=await page.locator('.security-settings button:visible').evaluateAll(elements=>elements.filter(el=>el.scrollWidth>el.clientWidth+1).map(el=>el.textContent));
  assert.deepEqual(clipped,[],`${name}: clipped action`);
  await page.screenshot({path:join(output,name+'.png')});layouts.push(name);
}
try{
  browser=await(await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'});
  for(const [width,height,theme,font] of [[1440,1000,'light',13],[1024,768,'dark',13],[768,900,'light',18],[390,844,'dark',18]]){
    const page=await browser.newPage({viewport:{width,height}});page.setDefaultTimeout(15000);page.setDefaultNavigationTimeout(60000);
    const errors=[];page.on('pageerror',error=>errors.push(error.message));
    await page.addInitScript(font=>{localStorage.setItem('echoagent.fontSize',String(font));const observer=new MutationObserver(()=>{if(document.documentElement){document.documentElement.style.fontSize=`${16*font/13}px`;observer.disconnect();}});observer.observe(document,{childList:true,subtree:true});},font);
    await page.goto(base+`&theme=${theme}&securityState=save-error`,{waitUntil:'networkidle'});
    await page.getByRole('group',{name:'电脑操作状态',exact:true}).getByText('已授权',{exact:true}).waitFor();
    assert.equal(await page.locator('.security-settings details[open]').count(),0);
    assert.deepEqual(await page.evaluate(()=>window.securitySettingsReviewCalls.saves),[]);
    assert.equal(await page.evaluate(()=>window.securitySettingsReviewCalls.requests),0);
    await capture(page,`overview-${width}-${theme}`);
    await page.locator('[data-setting-id="permission-rules"] > summary').click();
    const list=page.getByRole('list',{name:'权限规则列表',exact:true});assert.match(await list.textContent(),/拒绝.*执行命令.*rm \*/s);
    await page.getByRole('combobox',{name:'工具类型',exact:true}).selectOption('read');
    await page.getByRole('textbox',{name:'匹配范围（可选）',exact:true}).fill('/work/*');
    await page.getByRole('button',{name:'添加规则',exact:true}).click();
    assert.deepEqual(await page.evaluate(()=>window.securitySettingsReviewCalls.saves),[]);
    await capture(page,`rules-draft-${width}-${theme}`);
    await page.getByRole('button',{name:'保存修改',exact:true}).click();
    await page.getByRole('alert').filter({hasText:'配置暂不可写'}).waitFor();
    assert.equal(await list.getByRole('listitem').count(),3);
    await capture(page,`rules-failure-${width}-${theme}`);
    await page.getByRole('button',{name:'保存修改',exact:true}).click();
    await page.getByRole('status').filter({hasText:'规则已保存'}).waitFor();
    const expected=[...original,{action:'ask',tool:'read',pattern:'/work/*'}];
    assert.deepEqual(await page.evaluate(()=>window.securitySettingsReviewCalls.saves),[expected,expected]);
    assert.equal(await page.getByRole('button',{name:'保存修改',exact:true}).isDisabled(),true);
    await page.locator('[data-setting-id="permission-rules"] > summary').click();
    await page.evaluate(async()=>{
      const {useSessionStore}=await import('/src/stores/session-store.ts');
      const {useSessionsStore}=await import('/src/stores/sessions-store.ts');
      useSessionStore.getState().setSession('browser-a');
      useSessionsStore.getState().setIndependent([{sessionId:'browser-a',title:'资料整理',cwd:'/review/a',status:'completed'},{sessionId:'browser-b',title:'另一个任务',cwd:'/review/b',status:'completed'}]);
    });
    await page.locator('[data-setting-id="browser-data"] > summary').click();
    await page.getByRole('combobox',{name:'管理浏览器数据的任务',exact:true}).selectOption('browser-b');
    await page.getByRole('button',{name:'清除浏览器数据',exact:true}).waitFor();
    await capture(page,`task-browser-${width}-${theme}`);
    assert.deepEqual(errors,[]);console.log(`Security settings passed: ${width}×${height}, ${theme}, font ${font}`);await page.close();
  }
  const page=await browser.newPage({viewport:{width:1280,height:850}});page.setDefaultTimeout(15000);page.setDefaultNavigationTimeout(60000);
  for(const state of ['needs-permissions','desktop-unavailable','unsupported','check-error','rules-error','windows']){
    await page.goto(base+'&securityState='+state,{waitUntil:'networkidle'});
    if(state==='needs-permissions'){
      await page.getByText('需要授权',{exact:true}).waitFor();
      assert.equal(await page.evaluate(()=>window.securitySettingsReviewCalls.requests),0);
      await capture(page,state);
      await page.getByRole('button',{name:'前往授权',exact:true}).click();await page.getByText('已授权',{exact:true}).waitFor();
      assert.equal(await page.evaluate(()=>window.securitySettingsReviewCalls.requests),1);
    }else if(state==='check-error'){
      await page.getByRole('alert').filter({hasText:'操作状态暂不可读'}).waitFor();await capture(page,state);
      await page.getByRole('button',{name:'重试检查权限',exact:true}).click();await page.getByText('已授权',{exact:true}).waitFor();
    }else if(state==='rules-error'){
      await page.getByRole('alert').filter({hasText:'权限规则暂不可读'}).waitFor();assert.equal(await page.getByRole('button',{name:'保存修改',exact:true}).isDisabled(),true);
      await page.getByRole('button',{name:'重试',exact:true}).click();await page.getByText('existing:*',{exact:true}).waitFor();await capture(page,state);
    }else{
      await page.getByRole('group',{name:'电脑操作状态',exact:true}).waitFor();
      if(state!=='windows')assert.equal(await page.getByText('已授权',{exact:true}).count(),0);
      if(state==='windows')assert.match(await page.getByRole('group',{name:'电脑操作状态',exact:true}).textContent(),/已就绪/);
      assert.equal(await page.getByRole('button',{name:'前往授权',exact:true}).count(),0);await capture(page,state);
    }
    assert.deepEqual(await page.evaluate(()=>window.securitySettingsReviewCalls.saves),[]);
  }
  await page.close();writeFileSync(join(output,'evidence.json'),JSON.stringify({layouts},null,2));console.log(JSON.stringify({layouts:layouts.length,output}));
}finally{await browser?.close();await server.close();}
