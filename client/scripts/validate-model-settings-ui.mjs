import assert from 'node:assert/strict';
import {mkdtempSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createUiReviewServer,reviewChromium,reviewOutput} from './ui-review-runtime.mjs';
const output=reviewOutput(mkdtempSync(join(tmpdir(),'echo-model-settings-')));
console.log('Evidence: '+output);
const server=await createUiReviewServer(0);
const base=`http://127.0.0.1:${server.httpServer.address().port}`;
const evidence=[];let browser;
async function capture(page,name){
  for(const selector of ['.settings-modal__panel','.model-connections',...(await page.locator('.model-connection-editor').count()?['.model-connection-editor']:[])])
    assert.equal(await page.locator(selector).evaluate(el=>el.scrollWidth>el.clientWidth+1),false,`${name}: ${selector} overflow`);
  await page.screenshot({path:join(output,name+'.png')});evidence.push(name);
}
async function wheelToBottom(page) {
  const root=page.locator('.model-connections');
  await root.hover();
  await page.mouse.wheel(0,100000);
  await page.waitForFunction(()=>{
    const root=document.querySelector('.model-connections');
    return root.scrollHeight>root.clientHeight && root.scrollTop+root.clientHeight>=root.scrollHeight-2;
  });
}
async function assertInModelViewport(locator,label) {
  assert.equal(await locator.evaluate(element=>{
    const root=element.closest('.model-connections').getBoundingClientRect();
    const box=element.getBoundingClientRect();
    return box.top>=root.top-1 && box.bottom<=root.bottom+1;
  }),true,`${label}: cannot be reached by scrolling`);
}
try{
  browser=await(await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'});
  for(const [width,height,theme,font] of [[1440,1000,'light',13],[1024,768,'dark',13],[768,900,'light',18],[390,844,'dark',18]]){
    const page=await browser.newPage({viewport:{width,height}});page.setDefaultTimeout(15000);page.setDefaultNavigationTimeout(60000);
    const errors=[];page.on('pageerror',error=>errors.push(error.message));
    await page.addInitScript(font=>{localStorage.setItem('echoagent.fontSize',String(font));const observer=new MutationObserver(()=>{if(document.documentElement){document.documentElement.style.fontSize=`${16*font/13}px`;observer.disconnect();}});observer.observe(document,{childList:true,subtree:true});},font);
    await page.goto(base+`/__ui-review?surface=model&modelSettingsReview=1&theme=${theme}`,{waitUntil:'networkidle'});
    const selector=page.getByRole('combobox',{name:'默认模型',exact:true});await selector.waitFor();
    assert.equal(await page.getByRole('region',{name:'组织模型状态'}).count(),0);
    assert.equal(await page.getByRole('list',{name:'模型列表',exact:true}).count(),0);
    assert.equal(await page.getByRole('button',{name:'添加模型连接',exact:true}).count(),0);
    await capture(page,`overview-${width}-${theme}`);
    await selector.selectOption('review-personal/backup');
    await page.getByRole('status').filter({hasText:'已将「备用模型」设为默认模型'}).waitFor();
    assert.equal(await page.evaluate(()=>window.modelSettingsReviewCalls.defaults),1);
    await page.getByRole('button',{name:'管理模型连接',exact:true}).click();
    await page.getByRole('heading',{name:'连接管理',exact:true}).waitFor();
    assert.equal(await page.getByRole('region',{name:'组织模型状态'}).count(),0);
    await capture(page,`connections-${width}-${theme}`);
    await page.getByRole('group',{name:'模型连接',exact:true}).getByRole('button').first().click();
    await page.getByRole('region',{name:'工作模型服务连接详情'}).waitFor();
    await capture(page,`detail-${width}-${theme}`);
    await page.getByRole('button',{name:'编辑连接',exact:true}).click();
    const edit=page.getByRole('dialog',{name:'编辑连接',exact:true});
    assert.match(await edit.getByLabel('已配置模型',{exact:true}).textContent(),/main-model.*backup-model/s);
    assert.equal(await edit.getByRole('button',{name:'添加更多模型',exact:true}).count(),0);
    await capture(page,`edit-${width}-${theme}`);
    await edit.getByRole('button',{name:'取消',exact:true}).click();
    await page.getByRole('button',{name:'返回连接管理',exact:true}).click();
    await page.getByRole('button',{name:'添加模型连接',exact:true}).click();
    const dialog=page.getByRole('dialog',{name:'添加模型连接',exact:true});
    await dialog.getByLabel('模型服务商',{exact:true}).selectOption('kimi');
    assert.equal(await dialog.getByLabel('连接名称',{exact:true}).count(),0);
    assert.equal(await dialog.getByLabel('服务地址（Base URL）',{exact:true}).count(),0);
    assert.equal(await dialog.getByText(/语音|录音/).count(),0);
    assert.equal(await dialog.getByRole('button',{name:'测试此模型',exact:true}).count(),0);
    assert.equal(await dialog.getByRole('button',{name:'添加更多模型',exact:true}).count(),0);
    await capture(page,`form-${width}-${theme}`);
    await dialog.getByRole('button',{name:'保存',exact:true}).click();
    assert.equal(await dialog.getByLabel('API 密钥',{exact:true}).getAttribute('aria-invalid'),'true');
    assert.equal(await page.evaluate(()=>window.modelSettingsReviewCalls.saves),0);
    await dialog.getByLabel('API 密钥',{exact:true}).fill('isolated-review-key');
    await dialog.getByRole('button',{name:'从服务获取模型',exact:true}).click();
    await dialog.getByRole('radio',{name:'new-model',exact:true}).check();
    assert.equal(await dialog.getByLabel('模型 ID',{exact:true}).inputValue(),'new-model');
    await dialog.getByLabel('模型 ID',{exact:true}).fill('failed-model');
    await dialog.getByRole('button',{name:'更多设置',exact:true}).click();
    await dialog.getByLabel('连接名称',{exact:true}).waitFor();
    await capture(page,`advanced-${width}-${theme}`);
    await dialog.getByRole('button',{name:'保存',exact:true}).click();
    await dialog.waitFor({state:'hidden'});
    const detail=page.getByRole('region',{name:'Kimi连接详情',exact:true});await detail.waitFor();
    assert.equal(await page.evaluate(()=>window.modelSettingsReviewCalls.saves),1);
    assert.equal(await page.evaluate(()=>window.modelSettingsReviewCalls.defaults),1);
    assert.equal(await page.evaluate(()=>window.modelSettingsReviewCalls.tests.length),0);
    await detail.getByRole('button',{name:'测试模型 failed-model',exact:true}).click();
    await detail.getByRole('status').filter({hasText:'上次测试失败'}).waitFor();
    assert.equal(await page.evaluate(()=>window.modelSettingsReviewCalls.saves),1);
    await capture(page,`test-failure-${width}-${theme}`);
    await page.getByRole('button',{name:'返回连接管理',exact:true}).click();
    await page.getByRole('button',{name:'返回模型设置',exact:true}).click();
    assert.equal(await page.getByRole('combobox',{name:'默认模型',exact:true}).inputValue(),'review-personal/backup');
    await page.goto(base+`/__ui-review?surface=model&modelSettingsReview=1&modelLongCatalog=1&initialModelId=review-personal/main&theme=${theme}`,{waitUntil:'networkidle'});
    await page.getByRole('heading',{name:'连接详情',exact:true}).waitFor();
    await wheelToBottom(page);
    const lastModel=page.locator('.model-connections__model-list li').last();
    await assertInModelViewport(lastModel,'Last configured model');
    assert.equal(await lastModel.getByRole('button',{name:'编辑模型 extra-23',exact:true}).isEnabled(),true);
    await capture(page,`long-model-list-${width}-${theme}`);
    const info=page.locator('.model-connections__detail > .settings-disclosure');
    await info.locator(':scope > summary').click();
    await wheelToBottom(page);
    await assertInModelViewport(info.locator('.settings-hint'),'Expanded connection information');
    await capture(page,`long-model-information-${width}-${theme}`);
    await page.getByRole('button',{name:'返回连接管理',exact:true}).click();
    assert.equal(await page.locator('.model-connections').evaluate(element=>element.scrollTop),0);
    await wheelToBottom(page);
    await assertInModelViewport(page.locator('.model-settings__connection').last(),'Last connection');
    await capture(page,`long-connections-${width}-${theme}`);
    const navigation=page.getByRole('navigation');
    await navigation.getByRole('button',{name:'通用',exact:true}).click();
    await navigation.getByRole('button',{name:'模型',exact:true}).click();
    await page.getByRole('combobox',{name:'默认模型',exact:true}).waitFor();
    assert.equal(await page.getByRole('heading',{name:'连接详情',exact:true}).count(),0);
    assert.deepEqual(errors,[]);console.log(`Model settings passed at ${width}×${height}, ${theme}, font ${font}`);await page.close();
  }
  const page=await browser.newPage({viewport:{width:1280,height:850}});page.setDefaultTimeout(15000);page.setDefaultNavigationTimeout(60000);
  for(const account of ['none','mixed','expired','org-empty']){
    await page.goto(base+`/__ui-review?surface=model&modelSettingsReview=1&modelAccount=${account}&theme=light`,{waitUntil:'networkidle'});
    if(account==='none'){
      await page.getByText('还没有可用模型',{exact:true}).waitFor();assert.equal(await page.getByRole('button',{name:'添加模型连接',exact:true}).count(),1);
    }else{
      await page.getByRole('button',{name:'管理模型连接',exact:true}).click();
      if(account==='expired') {await page.getByRole('button',{name:'前往组织登录',exact:true}).click();assert.equal(await page.evaluate(()=>window.modelSettingsReviewCalls.logins),1);}
      if(account==='org-empty'){assert.equal(await page.getByText('组织暂未提供模型',{exact:true}).count(),0);await page.getByRole('button',{name:'刷新组织模型',exact:true}).click();await page.getByRole('region',{name:'组织模型状态'}).filter({hasText:'组织暂未提供模型'}).waitFor();}
      if(account==='mixed'){await page.getByRole('group',{name:'模型连接'}).getByRole('button').filter({hasText:'团队模型服务'}).click();assert.equal(await page.getByRole('button',{name:'编辑连接',exact:true}).count(),0);assert.equal(await page.getByRole('button',{name:'删除连接',exact:true}).count(),0);}
    }
    await capture(page,'account-'+account);
  }
  await page.goto(base+'/__ui-review?surface=model&modelSettingsReview=1&modelReadError=1',{waitUntil:'networkidle'});
  await page.getByRole('alert').filter({hasText:'配置暂不可读'}).waitFor();assert.equal(await page.getByText('还没有可用模型',{exact:true}).count(),0);
  await page.getByRole('button',{name:'重新读取',exact:true}).click();await page.getByRole('combobox',{name:'默认模型',exact:true}).waitFor();
  await capture(page,'read-recovery');await page.close();
  writeFileSync(join(output,'evidence.json'),JSON.stringify({layouts:evidence},null,2));console.log(JSON.stringify({layouts:evidence.length,output}));
}finally{await browser?.close();await server.close();}
