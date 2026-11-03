import assert from 'node:assert/strict';
import {mkdtempSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createUiReviewServer,reviewChromium,reviewOutput} from './ui-review-runtime.mjs';
const output=reviewOutput(mkdtempSync(join(tmpdir(),'echo-document-workspace-')));
const server=await createUiReviewServer(0,{fixture:'/scripts/fixtures/document-workspace-review.tsx'});
let browser, page;
try{
  browser=await(await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'});
  page=await browser.newPage({viewport:{width:1536,height:960}});page.setDefaultTimeout(20000);page.setDefaultNavigationTimeout(60000);
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review`);
  await page.getByRole('button',{name:'编辑',exact:true}).waitFor();
  await page.locator('.document-paper table tr').nth(2).waitFor();
  assert.equal(await page.locator('.document-paper table tr').count(),3,'real table structure must be visible');
  await page.waitForTimeout(1800);
  assert.equal(await page.evaluate(()=>window.documentReview.calls.filter(call=>call.command==='document_workspace_save').length),0,'opening a read-only preview must never save an editor normalization');
  await page.screenshot({path:join(output,'document-preview.png')});
  await page.getByRole('button',{name:'编辑',exact:true}).click();
  const editor=page.getByRole('textbox',{name:'文档编辑区'});await editor.waitFor();
  const cell=editor.locator('td').filter({hasText:'6:30'}).locator('p');
  await cell.click();await cell.evaluate(element=>{const range=document.createRange();range.selectNodeContents(element);const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);document.dispatchEvent(new Event('selectionchange'));});await page.keyboard.insertText('7:30');
  await page.waitForFunction(()=>JSON.stringify(window.documentReview.records['/review/家庭亲子作息表.docx'].document).includes('7:30'));
  await page.getByText('已保存 · Word 已同步',{exact:true}).waitFor();
  assert(await editor.isVisible(),'publishing must not unmount the editor');
  assert(await editor.getByText('准备早餐',{exact:true}).isVisible(),'editing a cell must preserve neighboring cells');
  await page.getByRole('button',{name:'表格操作',exact:true}).click();
  await page.getByRole('menuitem',{name:'在下方添加行',exact:true}).click();
  assert.equal(await editor.locator('tr').count(),4);
  await page.getByRole('button',{name:'保存到 Word',exact:true}).click();
  await page.getByText('已保存 · Word 已同步',{exact:true}).waitFor();
  await page.screenshot({path:join(output,'document-editing.png')});
  await page.getByRole('button',{name:'完成编辑',exact:true}).click();
  await page.getByRole('button',{name:'让 Agent 修改',exact:true}).click();
  await page.getByTestId('document-context').waitFor();
  const context=await page.evaluate(()=>window.documentEditContext);
  assert.equal(context.path,'/review/家庭亲子作息表.docx');assert(context.revision>=2);
  await page.reload();await page.getByRole('button',{name:'编辑',exact:true}).waitFor();
  assert(await page.locator('.document-paper').getByText('7:30',{exact:true}).isVisible(),'saved source must survive reload');
  await page.evaluate(()=>window.documentReview.agentUpdate('/review/家庭亲子作息表.docx','8:00'));
  await page.locator('.document-paper').getByText('8:00',{exact:true}).waitFor();
  await page.getByRole('button',{name:'文档版本历史',exact:true}).click();
  await page.getByRole('button',{name:'恢复此版本',exact:true}).first().click();
  await page.getByText('已保存 · Word 已同步',{exact:true}).waitFor();
  await page.locator('.document-paper').getByText('7:30',{exact:true}).waitFor();
  await page.getByRole('button',{name:'文档版本历史',exact:true}).click();
  await page.getByRole('button',{name:'编辑',exact:true}).click();
  const again=page.getByRole('textbox',{name:'文档编辑区'});
  const localCell=again.locator('td').filter({hasText:'7:30'}).locator('p');await localCell.click();await localCell.evaluate(element=>{const range=document.createRange();range.selectNodeContents(element);const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);document.dispatchEvent(new Event('selectionchange'));});await page.keyboard.insertText('9:00');
  await page.evaluate(()=>window.documentReview.agentUpdate('/review/家庭亲子作息表.docx','8:30'));
  await page.getByRole('alert').filter({hasText:'文档已有新修改'}).waitFor();
  assert(await again.getByText('9:00',{exact:true}).isVisible(),'conflict must preserve the local edit');
  await page.getByRole('button',{name:'将我的修改另存为副本',exact:true}).click();
  await page.getByText('已创建可编辑副本，原件已保留',{exact:true}).waitFor();
  await page.waitForFunction(()=>JSON.stringify(window.documentReview.records['/review/家庭亲子作息表-可编辑副本.docx']?.document).includes('9:00'));
  await page.setViewportSize({width:900,height:800});
  await page.screenshot({path:join(output,'document-narrow-copy.png')});
  const overflow=await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1);assert.equal(overflow,false,'narrow window must not overflow');
  await page.setViewportSize({width:1536,height:960});
  await page.evaluate(()=>{document.documentElement.setAttribute('data-theme','dark');document.querySelector('.app').setAttribute('data-theme','dark');document.documentElement.style.fontSize='20px';});
  await page.getByRole('button',{name:'编辑',exact:true}).click();
  await page.getByRole('textbox',{name:'文档编辑区'}).waitFor();
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1),false,'dark theme with large type must not overflow');
  await page.screenshot({path:join(output,'document-dark-editing.png')});
  await page.getByRole('button',{name:'文档版本历史',exact:true}).click();
  // Create a saved revision in the copy so it has a real historical version.
  const edit=page.getByRole('textbox',{name:'文档编辑区'});
  const timeCell=edit.locator('td').filter({hasText:'9:00'}).locator('p');await timeCell.click();
  await timeCell.evaluate(element=>{const range=document.createRange();range.selectNodeContents(element);const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);document.dispatchEvent(new Event('selectionchange'));});
  await page.keyboard.insertText('10:00');await page.getByText('已保存 · Word 已同步',{exact:true}).waitFor();
  await page.evaluate(()=>window.documentReview.delayRestore=true);
  await page.getByRole('button',{name:'恢复此版本',exact:true}).first().click();
  await page.getByText('正在恢复历史版本…',{exact:true}).waitFor();
  assert.equal(await edit.getAttribute('contenteditable'),'false','the editor must be locked for the whole restore request');
  assert.equal(await page.getByRole('button',{name:'插入表格'}).isDisabled(),true);
  await page.evaluate(()=>{window.documentReview.delayRestore=false;window.documentReview.releaseRestore();});
  await page.getByText('已保存 · Word 已同步',{exact:true}).waitFor();
  assert.equal(await edit.getAttribute('contenteditable'),'true','editing must resume after restoring');
  await page.getByRole('button',{name:'文档版本历史',exact:true}).click();
  await page.evaluate(()=>window.documentReview.failPublication=true);
  const failingCell=edit.locator('td').filter({hasText:'9:00'}).locator('p');await failingCell.click();
  await failingCell.evaluate(element=>{const range=document.createRange();range.selectNodeContents(element);const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);document.dispatchEvent(new Event('selectionchange'));});await page.keyboard.insertText('11:00');
  await page.getByRole('alert').filter({hasText:'Word 文件尚未更新'}).waitFor();
  await page.waitForTimeout(3000);
  assert(await page.getByRole('alert').filter({hasText:'Word 文件尚未更新'}).isVisible(),'background refresh must retain the sync error');
  await page.evaluate(()=>window.documentReview.failPublication=false);
  await page.getByRole('button',{name:'重试同步 Word',exact:true}).click();
  await page.getByText('已保存 · Word 已同步',{exact:true}).waitFor();
  for(const scenario of ['markdown','markdown-simple']){
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?scenario=${scenario}`);
    const create=page.getByRole('button',{name:'创建可编辑副本',exact:true});await create.waitFor();
    if(scenario==='markdown'){
      await page.getByRole('button',{name:'放大预览：架构示意',exact:true}).waitFor();
      await page.locator('.katex').first().waitFor();
      await page.locator('.md-mermaid-diagram img').waitFor();
      assert(await create.isDisabled(),'lossy Markdown conversion must not silently drop images, math or diagrams');
      assert(await page.getByText(/暂不能完整转换为可编辑 Word 副本/).isVisible());
    }else{
      await create.click();await page.getByRole('button',{name:'编辑',exact:true}).waitFor();
      assert(await page.locator('.document-paper strong').filter({hasText:'保留格式'}).isVisible());
    }
  }
  assert.deepEqual(errors,[]);
  console.log(`Document preview/editing, durable saves, restore locking, sync recovery, Agent/history/conflict, faithful Markdown reading and conversion checks passed. Evidence: ${output}`);
}catch(error){
  if(page){await page.screenshot({path:join(output,'failure.png')});console.log('Failure evidence:',output);console.log(await page.locator('body').innerText());console.log(await page.evaluate(()=>({records:window.documentReview?.records,errors:window.documentReview?.calls.slice(-8)})));}
  throw error;
}finally{await browser?.close();await server.close();}
