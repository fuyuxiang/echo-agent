import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createUiReviewServer, reviewChromium, reviewOutput } from './ui-review-runtime.mjs';
const output=reviewOutput(mkdtempSync(join(tmpdir(),'echo-organization-workspace-')));
const server=await createUiReviewServer(0);
const base=`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=organization-shell&platform=mac`;
let browser, activePage;
const evidence=[],errors=[];
// Use persisted reading sizes, including the supported minimum window with
// both the default and widest sidebar. Empty overview pages miss list collapse.
const layouts = [
  [1440,900,'light',13,false,248], [1100,760,'dark',13,false,248],
  [768,720,'light',18,true,248], [390,760,'dark',13,true,248],
  [1024,680,'light',18,false,248], [1024,680,'dark',18,false,248],
  [1024,680,'light',18,false,400], [1024,680,'dark',18,false,400],
  ...(process.argv.includes('--extended') ? [
    [768,720,'light',18,false,248], [1024,768,'light',13,false,248],
    [1920,1080,'dark',18,false,248], [1920,1080,'light',13,true,248],
  ] : []),
];
try {
  browser=await (await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'});
  for(const [width,height,theme,font,collapsed,sidebar] of (process.argv.includes('--flows-only')?[]:layouts)){
    const page=await browser.newPage({viewport:{width,height}});
    activePage=page;
    page.setDefaultTimeout(15000);
    page.setDefaultNavigationTimeout(45000);
    page.on('pageerror',error=>errors.push(error.message));
    await page.addInitScript(({font,sidebar})=>{
      localStorage.setItem('echoagent.fontSize',String(font));
      localStorage.setItem('app-sidebar-width',String(sidebar));
    },{font,sidebar});
    await page.goto(`${base}&orgState=many&theme=${theme}&collapsed=${collapsed?1:0}`);
    await page.getByLabel('组织知识概况').waitFor();
    await page.addStyleTag({content:`html {font-size:${16*font/13}px!important}`});
    const screenshotSuffix=`${width}-${height}-${theme}-${font}-${sidebar}-${collapsed?'collapsed':'expanded'}`;
    for(const [tab,label] of [['overview','概览'],['memories','经验'],['documents','文档'],['skills','Skills']]){
      await page.locator('.org-memory__nav button').filter({hasText:label}).click();
      await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
      const geometry=await page.evaluate(()=>{
        const root=document.querySelector('.org-workspace'),main=root.querySelector('.org-memory__content'),nav=root.querySelector('.org-memory__nav'),scroll=root.querySelector('.org-resource-scroll');
        const viewport=document.querySelector('.app__main').getBoundingClientRect();
        const overflow=[...root.querySelectorAll('*')].filter(el=>{
          if(!el.checkVisibility()||el.closest('[role="dialog"], [role="menu"]'))return false;
          const rect=el.getBoundingClientRect(),style=getComputedStyle(el);
          return rect.width>0&&rect.height>0&&style.position!=='fixed'&&(rect.right>viewport.right+2||rect.left<viewport.left-2);
        }).map(el=>el.className).slice(0,10);
        const unreachableControls=[...root.querySelectorAll('.org-workspace__page-actions button,.org-resource-filters input,.org-resource-filters select,.org-resource-filters button,.org-resource-pagination button,.org-resource-pagination select')].filter(el=>{
          if(!el.checkVisibility())return false;
          const rect=el.getBoundingClientRect();
          return rect.top<viewport.top-1||rect.bottom>viewport.bottom+1||!el.contains(document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2));
        }).map(el=>el.getAttribute('aria-label')||el.textContent.trim());
        return {overflow,unreachableControls,outerScroll:root.scrollHeight>root.clientHeight+2,contentHeight:scroll.clientHeight,scrollHeight:scroll.scrollHeight,navTop:nav.getBoundingClientRect().top,headerTop:main.querySelector('header').getBoundingClientRect().top,bottom:root.getBoundingClientRect().bottom};
      });
      assert.deepEqual(geometry.overflow,[],`${width} ${tab}: horizontal overflow`);
      assert.deepEqual(geometry.unreachableControls,[],`${screenshotSuffix} ${tab}: controls are covered or clipped`);
      assert(!geometry.outerScroll,`${width} ${tab}: outer page must not scroll`);
      const minimumReadingHeight=width>=1024?Math.max(200,height*.23):height*.23;
      assert(geometry.contentHeight>minimumReadingHeight,`${screenshotSuffix} ${tab}: too little reading space (${geometry.contentHeight})`);
      await page.locator('.org-resource-scroll').evaluate(el=>el.scrollTop=180);
      assert.equal(await page.locator('.org-memory__nav').evaluate(el=>el.getBoundingClientRect().top),geometry.navTop);
      assert.equal(await page.locator('.org-workspace__page-header').evaluate(el=>el.getBoundingClientRect().top),geometry.headerTop);
      await page.locator('.org-resource-scroll').evaluate(el=>el.scrollTop=0);
      await page.screenshot({path:join(output,`${tab}-${screenshotSuffix}.png`)});
      if(tab!=='overview'){
        const action=page.locator(tab==='memories'?'.org-memory-summary':tab==='documents'?'.org-document-library__row .org-library__actions button':'.org-skill-card footer button').first();
        await action.scrollIntoViewIfNeeded();
        assert(await action.evaluate(el=>{
          const rect=el.getBoundingClientRect(),viewport=el.closest('.org-resource-scroll').getBoundingClientRect();
          return rect.top>=viewport.top-1&&rect.bottom<=viewport.bottom+1&&el.contains(document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2));
        }),`${screenshotSuffix} ${tab}: list action cannot be fully reached`);
        await page.getByRole('button',{name:'我的提交',exact:true}).click();
        await page.screenshot({path:join(output,`${tab}-mine-${screenshotSuffix}.png`)});
        await page.getByRole('button',{name:'下一页',exact:true}).click();
        await page.waitForFunction(()=>document.querySelector('.org-resource-pagination>div>span')?.textContent?.startsWith('2 /'));
        await page.getByRole('button',{name:'已发布',exact:true}).click();
      }
      evidence.push({width,height,theme,font,sidebar,collapsed,tab,...geometry});
    }
    console.log(`Organization layouts passed: ${screenshotSuffix}`);
    await page.close();
  }
  const page=await browser.newPage({viewport:{width:1440,height:900}});
  activePage=page;
  page.setDefaultTimeout(15000);
    page.setDefaultNavigationTimeout(45000);
  page.on('pageerror',error=>errors.push(error.message));
  console.log('interaction: independent pagination');
  await page.goto(`${base}&orgState=many`);
  await page.getByLabel('组织知识概况').waitFor();
  await page.locator('.org-memory__nav button').filter({hasText:'经验'}).click();
  await page.getByRole('button',{name:'下一页',exact:true}).click();
  await page.getByText('21. 发布前先核对部署版本和数据库迁移状态，再验证关键接口。确认影响范围后执行回滚。',{exact:true}).waitFor();
  await page.locator('.org-resource-scroll').evaluate(el=>el.scrollTop=180);
  await page.locator('.org-memory__nav button').filter({hasText:'文档'}).click();
  await page.locator('.org-memory__nav button').filter({hasText:'经验'}).click();
  assert.equal(await page.locator('.org-resource-scroll').evaluate(el=>el.scrollTop),180);
  await page.getByRole('button',{name:'刷新',exact:true}).click();
  await page.getByRole('button',{name:'刷新',exact:true}).waitFor({state:'visible'});
  await page.waitForFunction(()=>!document.querySelector('[aria-label="刷新"]').disabled);
  assert.equal(await page.locator('.org-resource-pagination>div>span').innerText(),'2 / 13');
  assert.equal(await page.locator('.org-resource-scroll').evaluate(el=>el.scrollTop),180);
  console.log('interaction: full search');
  await page.getByRole('searchbox',{name:'搜索经验'}).fill('末页记录');
  await page.getByText('共 1 项',{exact:true}).waitFor();
  await page.getByRole('button',{name:/245\. 末页记录/}).click();
  await page.getByRole('dialog',{name:'经验详情'}).waitFor();
  await page.keyboard.press('Escape');
  await page.getByRole('button',{name:'清空搜索',exact:true}).click();
  await page.getByText('共 245 项',{exact:true}).waitFor();
  console.log('interaction: history');
  await page.getByRole('button',{name:'我的提交',exact:true}).click();
  assert.equal(await page.locator('.org-submissions>div').count(),20);
  for(let n=0;n<5;n++) {await page.getByRole('button',{name:'下一页',exact:true}).click();await page.waitForFunction(n=>document.querySelector('.org-resource-pagination>div>span')?.textContent?.startsWith(`${n+2} /`),n);}
  await page.getByText('经验提交 111：记录任务处理和交付步骤。',{exact:true}).waitFor();
  console.log('interaction: draft');
  await page.getByRole('button',{name:'提交经验',exact:true}).click();
  const form=page.getByRole('dialog',{name:'提交经验',exact:true});
  await form.getByRole('textbox',{name:'经验内容',exact:true}).fill('发布前检查回滚脚本');
  await page.keyboard.press('Escape');
  await form.waitFor({state:'hidden'});
  console.log('interaction: draft');
  await page.getByRole('button',{name:'提交经验',exact:true}).click();
  await form.waitFor();
  assert.equal(await form.getByRole('textbox',{name:'经验内容',exact:true}).inputValue(),'发布前检查回滚脚本');
  await form.getByRole('button',{name:'提交',exact:true}).click();
  await form.waitFor({state:'hidden'});
  console.log('interaction: install');
  await page.locator('.org-memory__nav button').filter({hasText:'Skills'}).click();
  await page.getByRole('combobox',{name:'安装状态'}).selectOption('available');
  await page.locator('.org-skill-card').first().waitFor();
  await page.waitForFunction(()=>!document.querySelector('.org-resource-results').classList.contains('is-loading'));
  await page.locator('.org-skill-card').first().getByRole('button',{name:'安装',exact:true}).click();
  await page.waitForFunction(()=>!document.querySelector('[aria-label="刷新"]').disabled);
  await page.getByRole('combobox',{name:'安装状态'}).selectOption('installed');
  await page.waitForFunction(()=>!document.querySelector('.org-resource-results').classList.contains('is-loading'));
  assert(await page.getByRole('button',{name:'卸载',exact:true}).count()>0);
  await page.locator('.org-memory__nav button').filter({hasText:'文档'}).click();
  console.log('interaction: document menu');
  await page.getByRole('button',{name:/更多文档操作/}).first().click();
  await page.getByRole('menuitem',{name:'发布副本',exact:true}).click();
  await page.getByRole('dialog',{name:'发布文档副本'}).waitFor();
  await page.keyboard.press('Escape');
  assert(await page.getByRole('button',{name:/更多文档操作/}).first().evaluate(el=>el===document.activeElement));
  console.log('interaction: logout');
  await page.getByRole('button',{name:'组织账号：界面检查',exact:true}).click();
  await page.getByRole('menuitem',{name:'退出登录',exact:true}).click();
  await page.getByRole('heading',{name:'连接组织',exact:true}).waitFor();
  await page.goto(`${base}&orgError=1`);
  await page.getByRole('alert').waitFor();
  assert.equal(await page.getByRole('button',{name:'刷新',exact:true}).count(),0);
  await page.getByRole('button',{name:'重试',exact:true}).click();
  await page.getByLabel('组织知识概况').waitFor();
  assert.equal(await page.getByRole('alert').count(),0);
  for(const [width,height] of [[390,620],[768,720]]) {
    const modalPage=await browser.newPage({viewport:{width,height}});
    activePage=modalPage;
    modalPage.setDefaultTimeout(15000);
    modalPage.setDefaultNavigationTimeout(45000);
    await modalPage.goto(`${base}&orgState=many&orgPending=1&collapsed=1`);
    await modalPage.getByLabel('组织知识概况').waitFor();
    await modalPage.addStyleTag({content:'html{font-size:22px!important}'});
    await modalPage.locator('.org-memory__nav button').filter({hasText:'经验'}).click();
    await modalPage.getByRole('button',{name:'提交经验',exact:true}).click();
    const form=modalPage.getByRole('dialog',{name:'提交经验',exact:true});
    const box=await form.locator('.org-editor-dialog').boundingBox();
    assert(box.x>=0&&box.y>=0&&box.x+box.width<=width+1&&box.y+box.height<=height+1,`${width}: memory editor exceeds viewport`);
    assert(await form.getByRole('button',{name:'提交',exact:true}).isVisible());
    await modalPage.screenshot({path:join(output,`memory-editor-${width}.png`)});
    await modalPage.keyboard.press('Escape');
    await form.waitFor({state:'hidden'});
    await modalPage.locator('.org-memory__nav button').filter({hasText:'Skills'}).click();
    await modalPage.getByRole('button',{name:'提交 Skill',exact:true}).click();
    const skillDialog=modalPage.getByRole('dialog',{name:'提交 Skill',exact:true});
    const skillBox=await skillDialog.locator('.org-document-modal__confirm').boundingBox();
    assert(skillBox.x>=0&&skillBox.y>=0&&skillBox.x+skillBox.width<=width+1&&skillBox.y+skillBox.height<=height+1,`${width}: skill scope dialog exceeds viewport`);
    await modalPage.screenshot({path:join(output,`skill-dialog-${width}.png`)});
    await modalPage.keyboard.press('Escape');
    for (const label of ['文档', 'Skills']) {
      await modalPage.locator('.org-memory__nav button').filter({hasText:label}).click();
      await modalPage.getByRole('button',{name:'我的提交',exact:true}).click();
      await modalPage.getByText('检查通过，待组织处理',{exact:true}).waitFor();
      assert.equal(await modalPage.getByRole('note').count(),1,'pending publication guidance should appear once');
      assert(await modalPage.getByRole('note').innerText().then(text=>text.includes('请联系组织管理员')));
      assert.equal(await modalPage.getByText('安全检查与发布中',{exact:true}).count(),0);
      assert(await modalPage.locator('.org-workspace').evaluate(root=>root.scrollWidth<=root.clientWidth+2),`${width} ${label}: pending states overflow`);
      await modalPage.getByText('检查通过，待组织处理',{exact:true}).scrollIntoViewIfNeeded();
      const stateBox=await modalPage.getByText('检查通过，待组织处理',{exact:true}).boundingBox();
      assert(stateBox.y>=0&&stateBox.y+stateBox.height<=height,`${width} ${label}: pending state is not readable`);
      await modalPage.screenshot({path:join(output,`pending-${label==='Skills'?'skills':'documents'}-${width}.png`)});
    }
    await modalPage.close();
  }
  for (const mode of [
    { parameter: 'orgKeywordOnly', filter: 'keywordOnly', label: '可检索 · 关键词模式', note: '语义检索暂不可用' },
    { parameter: 'orgSemanticPartial', filter: 'semanticPartial', label: '可检索 · 部分语义可用', note: '部分内容支持语义检索' },
  ]) for (const [width,height] of [[390,760],[1440,900]]) {
    const degradedPage=await browser.newPage({viewport:{width,height}});
    activePage=degradedPage;
    degradedPage.setDefaultTimeout(15000);
    await degradedPage.goto(`${base}&orgState=many&${mode.parameter}=1&collapsed=1`);
    await degradedPage.getByRole('note').filter({hasText:mode.note}).waitFor();
    await degradedPage.locator('.org-memory__nav button').filter({hasText:'文档'}).click();
    await degradedPage.getByText(mode.label,{exact:true}).waitFor();
    await degradedPage.getByRole('combobox',{name:'文档状态'}).selectOption(mode.filter);
    await degradedPage.getByText('共 1 项',{exact:true}).waitFor();
    assert.equal(await degradedPage.getByRole('button',{name:'查看',exact:true}).count(),1);
    assert.equal(await degradedPage.getByRole('button',{name:'下载',exact:true}).count(),1);
    assert(await degradedPage.locator('.org-workspace').evaluate(root=>root.scrollWidth<=root.clientWidth+2),`${width}: ${mode.filter} overflows`);
    await degradedPage.screenshot({path:join(output,`${mode.filter}-${width}.png`)});
    await degradedPage.close();
  }
  for (const [width,height,theme] of [[1440,900,'light'],[1100,760,'dark'],[390,760,'light']]) {
    const foldersPage=await browser.newPage({viewport:{width,height}});
    activePage=foldersPage;
    foldersPage.setDefaultTimeout(15000);
    foldersPage.on('pageerror',error=>errors.push(error.message));
    await foldersPage.goto(`${base}&orgFolders=1&collapsed=1&theme=${theme}`);
    await foldersPage.getByLabel('组织知识概况').waitFor();
    await foldersPage.locator('.org-memory__nav button').filter({hasText:'文档'}).click();
    await foldersPage.getByRole('button',{name:/项目资料.*文件夹/}).waitFor();
    assert(await foldersPage.getByText('已有文档.xlsx',{exact:true}).isVisible());
    assert(await foldersPage.locator('.org-workspace').evaluate(root=>root.scrollWidth<=root.clientWidth+2),`${width}: folder root overflows`);
    const header=await foldersPage.locator('.org-workspace__page-header').boundingBox();
    const before=await foldersPage.locator('.org-resource-scroll').boundingBox();
    await foldersPage.getByRole('button',{name:'更多上传方式',exact:true}).click();
    const menu=await foldersPage.getByRole('menu',{name:'上传方式',exact:true}).boundingBox();
    const trigger=await foldersPage.getByRole('button',{name:'更多上传方式',exact:true}).boundingBox();
    assert.equal((await foldersPage.locator('.org-resource-scroll').boundingBox()).y,before.y,'upload menu moved the list');
    assert(Math.abs(menu.x+menu.width-trigger.x-trigger.width)<2,'upload menu is not right aligned');
    assert(menu.width<=200,'one-item upload menu is too wide');
    assert.equal((await foldersPage.locator('.org-workspace__page-header').boundingBox()).height,header.height);
    await foldersPage.screenshot({path:join(output,`folders-upload-${width}-${theme}.png`)});
    await foldersPage.keyboard.press('Escape');
    await foldersPage.getByRole('button',{name:/项目资料.*文件夹/}).click();
    await foldersPage.getByRole('button',{name:/设计.*文件夹/}).waitFor();
    await foldersPage.getByRole('button',{name:/设计.*文件夹/}).click();
    await foldersPage.getByText('需求说明.docx',{exact:true}).waitFor();
    const breadcrumbs=foldersPage.getByRole('navigation',{name:'文档目录'});
    assert(await breadcrumbs.getByRole('button',{name:'项目资料',exact:true}).isVisible());
    assert.equal(await breadcrumbs.getByRole('button',{name:'设计',exact:true}).getAttribute('aria-current'),'page');
    await foldersPage.screenshot({path:join(output,`folders-nested-${width}-${theme}.png`)});
    await foldersPage.getByRole('searchbox',{name:'搜索文档'}).fill('需求');
    await foldersPage.getByRole('button',{name:'打开所在目录：项目资料/设计',exact:true}).waitFor();
    await foldersPage.getByRole('button',{name:'打开所在目录：项目资料/设计',exact:true}).click();
    assert.equal(await foldersPage.getByRole('searchbox',{name:'搜索文档'}).inputValue(),'');
    await breadcrumbs.getByRole('button',{name:'产品研发团队',exact:true}).click();
    await foldersPage.getByText('已有文档.xlsx',{exact:true}).waitFor();
    await foldersPage.close();
  }
  assert.deepEqual(errors,[]);
  writeFileSync(join(output,'evidence.json'),JSON.stringify(evidence,null,2));
  console.log(JSON.stringify({output,layouts:evidence.length,flows:'fixed navigation and toolbar / independent scroll and query / full search / paginated submissions / draft / install / menu focus / logout / retry'}));
} catch(error) { if(activePage&&!activePage.isClosed()){await activePage.screenshot({path:join(output,'failure.png')});writeFileSync(join(output,'failure.txt'),await activePage.locator('body').innerText());}console.log(JSON.stringify({output}));throw error;} finally {await browser?.close();await server.close();}
