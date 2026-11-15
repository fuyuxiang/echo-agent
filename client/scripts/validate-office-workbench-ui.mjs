import assert from 'node:assert/strict';
import { mkdtempSync, copyFileSync, existsSync, writeFileSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { Document, Packer, Paragraph, TextRun, Table, TableRow, TableCell, WidthType, Header, Footer, PageNumber, ImageRun } from 'docx';
import ExcelJS from 'exceljs';
import JSZip from 'jszip';
import PptxGenJS from 'pptxgenjs';
import { PDFDocument, StandardFonts, rgb } from 'pdf-lib';
import { createUiReviewServer, reviewChromium, reviewOutput } from './ui-review-runtime.mjs';
import { stageDocumentRuntime } from './stage-document-runtime.mjs';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { assertNativeReviewSessionUnlocked } from './lib/native-review-window.mjs';
const output = reviewOutput(mkdtempSync(join(tmpdir(), 'echo-office-workbench-')));
const directory = mkdtempSync(join(tmpdir(), 'echo-office-fixtures-'));
stageDocumentRuntime();
const spring = process.env.OFFICE_REVIEW_SOURCE_DIR;
const deck = new PptxGenJS(); deck.layout = 'LAYOUT_WIDE';
const flower = '<svg xmlns="http://www.w3.org/2000/svg" width="400" height="280"><rect width="400" height="280" rx="20" fill="#E7F2E8"/><path d="M204 245 Q175 155 220 70" fill="none" stroke="#427658" stroke-width="8"/><ellipse cx="173" cy="178" rx="38" ry="17" fill="#8EC69B" transform="rotate(35 173 178)"/><ellipse cx="233" cy="141" rx="36" ry="17" fill="#9BD2A7" transform="rotate(-25 233 141)"/><g fill="#E5A3B3"><circle cx="220" cy="57" r="24"/><circle cx="244" cy="77" r="24"/><circle cx="234" cy="106" r="24"/><circle cx="204" cy="106" r="24"/><circle cx="196" cy="77" r="24"/></g><circle cx="220" cy="83" r="14" fill="#E7CA7F"/></svg>';
let slide = deck.addSlide(); slide.background = { color: 'FAFBF7' }; slide.addText('春天 · 万物生长', { x: .7, y: 1.1, w: 6.3, h: .8, fontSize: 34, bold: true, color: '2F6B57', fontFace: 'PingFang SC' }); slide.addText('保留真实的版式、颜色、图像与文字位置', { x: .7, y: 2.25, w: 6.4, h: .8, fontSize: 18, color: '657766', fontFace: 'PingFang SC' }); slide.addImage({ data: `data:image/svg+xml;base64,${Buffer.from(flower).toString('base64')}`, x: 7.6, y: 1.1, w: 4.6, h: 3.22 }); slide.addShape(deck.ShapeType.rect, { x: .7, y: 3.6, w: 1.2, h: .06, line: { color: '94BFA0' }, fill: { color: '94BFA0' } });
slide = deck.addSlide(); slide.addText('春日计划', { x: .7, y: .5, w: 10, h: .6, fontSize: 28, color: '2F6B57' }); slide.addTable([['月份','主题','安排'],['三月','新芽','观察与记录'],['四月','花开','踏青与阅读']], { x: .7, y: 1.6, w: 11, h: 2.2, border: { type: 'solid', color: 'B7CABB', pt: 1 }, fontSize: 18, fontFace: 'PingFang SC', color: '284E38', fill: 'F3F8F0' });
await deck.writeFile({ fileName: join(directory, '图文演示.pptx') });
if (spring && existsSync(join(spring, '春天.pptx'))) copyFileSync(join(spring, '春天.pptx'), join(directory, '春天.pptx')); else copyFileSync(join(directory, '图文演示.pptx'), join(directory, '春天.pptx'));
const cell = (text, fill, bold = false) => new TableCell({ shading: fill ? { fill } : undefined, children: [new Paragraph({ children: [new TextRun({ text, bold, color: bold ? 'FFFFFF' : '294B3B' })] })] });
const word = new Document({ styles: { default: { document: { run: { font: 'PingFang SC', size: 22, color: '333B36' }, paragraph: { spacing: { after: 180 } } } } }, sections: [
  { properties: {}, headers: { default: new Header({ children: [new Paragraph({ children: [new TextRun({ text: '春日工作手册', color: '65816B', size: 18 })] })] }) }, footers: { default: new Footer({ children: [new Paragraph({ children: [new TextRun({ children: ['第 ', PageNumber.CURRENT, ' 页'] })], alignment: 'center' })] }) }, children: [new Paragraph({ children: [new TextRun({ text: '春日计划', bold: true, color: '2F6B57', size: 42 })], spacing: { before: 180, after: 320 } }), new Paragraph('春天的意义，在于重新开始。通过阅读、观察与记录，发现日常生活里的变化。'), new Paragraph({ children: [new TextRun({ text: '本周安排', bold: true, size: 28, color: '427658' })] }), new Table({ width: { size: 100, type: WidthType.PERCENTAGE }, rows: [new TableRow({ children: [cell('时间','427658',true),cell('内容','427658',true),cell('完成标准','427658',true)] }), new TableRow({ children: [cell('星期一','EDF5ED'),cell('观察树木的新芽','EDF5ED'),cell('完成一页记录','EDF5ED')] }), new TableRow({ children: [cell('星期三'),cell('阅读与摘抄'),cell('整理三条感想')] }), new TableRow({ children: [cell('星期五','EDF5ED'),cell('复盘与分享','EDF5ED'),cell('记录新的发现','EDF5ED')] })] }), new Paragraph({ children: [new ImageRun({ type: 'png', data: readFileSync(resolve('src-tauri/icons/128x128.png')), transformation: { width: 20, height: 20 } })] })] },
  { properties: {}, children: [new Paragraph({ children: [new TextRun({ text: '阅读与记录', bold: true, color: '2F6B57', size: 36 })] }), new Paragraph('第二页用于验证真正的 Word 分页，保留原始字体、颜色、表格和页眉页脚。'), new Paragraph({ children: [new TextRun({ text: '让每一次观察都有迹可循。', italics: true, color: '657766' })] })] }
] });
writeFileSync(join(directory, '春日计划.docx'), await Packer.toBuffer(word));
const longWord=new Document({sections:[{children:[...Array.from({length:75},(_,i)=>new Paragraph({children:[new TextRun({text:`观察记录 ${i+1}。`+'逐日阅读与观察，保留完整段落并按原始纸张高度分页。'.repeat(3),size:24})],spacing:{after:200}})),new Paragraph('长文结束标记')]}]});
writeFileSync(join(directory,'分页长文.docx'),await Packer.toBuffer(longWord));
const mixedPdf=await PDFDocument.create();const mixedFont=await mixedPdf.embedFont(StandardFonts.Helvetica);
for(const size of [[595,842],[842,595]]){const p=mixedPdf.addPage(size);p.drawText(`Mixed page ${size.join(' x ')}`,{x:60,y:size[1]-80,font:mixedFont,size:24});}
writeFileSync(join(directory,'混合纸张.pdf'),await mixedPdf.save());
const book = new ExcelJS.Workbook();
const sheet = book.addWorksheet('经营数据'); sheet.columns = [{width:20},{width:19},{width:19},{width:19},{width:19},{width:22}];
sheet.mergeCells('A1:F1'); sheet.getCell('A1').value = '2026 春季经营数据'; sheet.getCell('A1').font = { name: 'PingFang SC', size: 18, bold: true, color: { argb: 'FFFFFFFF' } }; sheet.getCell('A1').fill = { type: 'pattern', pattern: 'solid', fgColor: { argb: 'FF356C53' } }; sheet.getRow(1).height = 40;
sheet.getRow(2).values = ['月份','收入','成本','利润','利润率','日期']; sheet.getRow(2).height = 29;
for (const c of sheet.getRow(2).values.keys()) if (c) { const x=sheet.getRow(2).getCell(c); x.fill = {type:'pattern',pattern:'solid',fgColor:{argb:'FFE4F0E6'}};x.font={bold:true,name:'PingFang SC',size:11,color:{argb:'FF315F46'}}; }
for (const [i, income, cost] of [[3,120500.5,72000],[4,146800,84600],[5,158400,87120]]) { sheet.getRow(i).values = [`${i}月`,income,cost,{formula:`B${i}-C${i}`,result:income-cost},{formula:`D${i}/B${i}`,result:(income-cost)/income},new Date(`2026-${String(i).padStart(2,'0')}-01T00:00:00Z`)]; sheet.getRow(i).height = 27; for (const col of ['B','C','D']) sheet.getCell(`${col}${i}`).numFmt = '¥#,##0.00'; sheet.getCell(`E${i}`).numFmt = '0.0%'; sheet.getCell(`F${i}`).numFmt = 'yyyy-mm-dd'; }
sheet.getCell('A5000').value = '远行号也可导航';
sheet.getRow(6).height = 60; sheet.getCell('B6').fill = { type: 'pattern', pattern: 'solid', fgColor: { argb: 'FFFFCC00' } };
sheet.getCell('B6').border = { bottom: { style: 'thin', color: { argb: 'FF112233' } } };
const second = book.addWorksheet('阅读说明'); second.getColumn(1).width=64;second.getCell('A1').value='保留合并单元格、列宽、日期、金额与公式缓存值。';second.getCell('A1').font={size:12,color:{argb:'FF356C53'}};
await book.xlsx.writeFile(join(directory, '经营数据.xlsx'));
const unsafe = await JSZip.loadAsync(readFileSync(join(directory, '经营数据.xlsx')));
const unsafeXml = await unsafe.file('xl/worksheets/sheet1.xml').async('string');
unsafe.file('xl/worksheets/sheet1.xml', unsafeXml.replace('</mergeCells>', '<mergeCell ref="A100:XFD1048576"/></mergeCells>'));
writeFileSync(join(directory, '超大合并.xlsx'), await unsafe.generateAsync({ type: 'nodebuffer', compression: 'DEFLATE' }));
if (spring && existsSync(join(spring, '春天.pdf'))) copyFileSync(join(spring, '春天.pdf'), join(directory, '春天.pdf')); else { const pdf=await PDFDocument.create(); const font=await pdf.embedFont(StandardFonts.Helvetica);for(let i=1;i<=2;i++){const p=pdf.addPage([595,842]);p.drawText(`Spring PDF preview - page ${i}`,{x:60,y:750,size:24,font,color:rgb(.18,.42,.32)});}writeFileSync(join(directory,'春天.pdf'),await pdf.save()); }
const names=['春天.pptx','春日计划.docx','经营数据.xlsx','春天.pdf','图文演示.pptx','分页长文.docx','混合纸张.pdf','超大合并.xlsx'];
const routes=Object.fromEntries(names.map(name=>[`/__office-fixture/${encodeURIComponent(name)}`,join(directory,name)]));
const server=await createUiReviewServer(0,{fixture:'/scripts/fixtures/office-workbench-review.tsx',fixtureRoutes:routes});
const browser=await(await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'});
const page=await browser.newPage({viewport:{width:1120,height:850}});page.setDefaultTimeout(30000);
const errors=[];page.on('pageerror',e=>errors.push(e.message));
const base=`http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
try {
  await page.goto(base);await page.locator('.office-workbench__stage .office-page[data-page-rendered=true]').waitFor({timeout:60000});
  console.log('PPT loaded',await page.locator('.office-workbench__page-count').innerText());
  await page.screenshot({path:join(output,'pptx-light.png')});
  await page.getByRole('button',{name:'下一页',exact:true}).click();await page.locator('.office-workbench__stage .office-page[data-page-index="1"][data-page-rendered=true]').waitFor();
  await page.getByRole('button',{name:'搜索文档',exact:true}).click();await page.getByRole('textbox',{name:'搜索文档内容'}).fill('春');await page.locator('.office-search-result').first().waitFor();assert(await page.locator('.office-search-result').count()>0);
  await page.getByRole('button',{name:'关闭文档搜索'}).click();await page.getByRole('button',{name:'全屏预览'}).click();await page.locator('.office-workbench--expanded').waitFor();await page.keyboard.press('Escape');assert.equal(await page.locator('.office-workbench--expanded').count(),0);
  await page.getByRole('tab',{name:'图文演示.pptx',exact:true}).click();await page.locator('.office-workbench__stage .office-page[data-page-rendered=true]').waitFor();await page.screenshot({path:join(output,'pptx-images.png')});
  await page.getByRole('tab',{name:'春日计划.docx',exact:true}).click();await page.locator('.office-workbench__stage .office-page[data-page-rendered=true]').waitFor();
  const wordFrame=page.frameLocator('.office-workbench__stage iframe');assert(await wordFrame.getByText('春日计划',{exact:true}).isVisible());assert.equal(await wordFrame.locator('table tr').count(),4);assert.match(await page.locator('.office-workbench__page-count').innerText(),/1 \/ 2/,'Word section and page boundaries must be preserved');await page.screenshot({path:join(output,'docx-light.png')});
  await page.getByRole('tab',{name:'经营数据.xlsx',exact:true}).click();await page.getByRole('grid').waitFor();assert.match(await page.locator('[data-cell-address="B3"]').innerText(),/120,500\.50/);assert.match(await page.locator('[data-cell-address="E3"]').innerText(),/40\.2%/);
  await page.locator('[data-cell-address="D3"]').click();assert.match(await page.getByRole('textbox',{name:'单元格内容'}).inputValue(),/^=B3-C3$/);
  const blank = page.locator('[data-cell-address="B6"]');assert.equal(Math.round((await blank.boundingBox()).height),80,'blank row height must be preserved');
  assert.equal(await blank.evaluate(element=>getComputedStyle(element).backgroundColor),'rgb(255, 204, 0)','blank formatted cells must retain their fill');
  await page.getByRole('button',{name:'搜索文档',exact:true}).click();await page.getByRole('textbox',{name:'搜索文档内容'}).fill('远行号也可导航');
  await page.getByRole('button',{name:/经营数据 · A5000/}).click();await page.getByRole('gridcell',{name:/A5000：远行号/}).waitFor();
  assert.equal(await page.getByRole('textbox',{name:'跳转到单元格'}).inputValue(),'A5000','search must reveal its actual cell');
  const beforeFullscreen = await page.getByRole('grid').evaluate(element=>element.scrollTop);
  await page.getByRole('button',{name:'全屏预览',exact:true}).click();assert.equal(await page.getByRole('textbox',{name:'跳转到单元格'}).inputValue(),'A5000');
  await page.keyboard.press('Escape');assert(Math.abs(await page.getByRole('grid').evaluate(element=>element.scrollTop)-beforeFullscreen)<200,'fullscreen must preserve the grid viewport within the resized page');
  assert(await page.getByRole('gridcell',{name:/A5000：远行号/}).isVisible(),'fullscreen must retain the selected far cell');
  await page.getByRole('button',{name:'关闭文档搜索'}).click();
  await page.getByRole('textbox',{name:'跳转到单元格'}).fill('A1');await page.getByRole('textbox',{name:'跳转到单元格'}).press('Enter');
  await page.screenshot({path:join(output,'xlsx-light.png')});await page.getByRole('textbox',{name:'跳转到单元格'}).fill('A5000');await page.getByRole('textbox',{name:'跳转到单元格'}).press('Enter');await page.getByRole('gridcell',{name:/A5000：远行号/}).waitFor();
  assert(await page.getByRole('gridcell').count()<1000,'spreadsheet grid must virtualize far rows');await page.getByRole('tab',{name:'阅读说明',exact:true}).click();await page.getByRole('gridcell',{name:/A1：保留合并/}).waitFor();
  await page.getByRole('tab',{name:'超大合并.xlsx',exact:true}).click();await page.getByRole('alert').filter({hasText:'合并区域合计超过'}).waitFor();
  await page.getByRole('tab',{name:'春天.pdf',exact:true}).click();await page.locator('.office-workbench__stage .office-page[data-page-rendered=true]').waitFor({timeout:60000});
  await page.getByRole('slider',{name:'文档缩放'}).fill('150');await page.getByRole('slider',{name:'文档缩放'}).fill('80');
  await page.getByRole('button',{name:'下一页',exact:true}).click();await page.getByRole('button',{name:'上一页',exact:true}).click();
  await page.locator('.office-workbench__stage .office-page[data-page-index="0"][data-page-rendered=true]').waitFor();
  const ink=await page.locator('.office-workbench__stage canvas').evaluate(canvas=>{const pixels=canvas.getContext('2d').getImageData(0,0,canvas.width,canvas.height).data;let ink=0;for(let i=0;i<pixels.length;i+=16)if(pixels[i]<210||pixels[i+1]<210||pixels[i+2]<210)ink++;return ink;});assert(ink>300,'real PDF page must have painted content');await page.screenshot({path:join(output,'pdf-light.png')});
  await page.getByRole('tab',{name:'分页长文.docx',exact:true}).click();await page.locator('.office-workbench__stage .office-page[data-page-rendered=true]').waitFor();
  const longCount=Number((await page.locator('.office-workbench__page-count').innerText()).split('/')[1]);assert(longCount>3,'Word must paginate overflowing original paragraphs');
  await page.getByRole('spinbutton',{name:'跳转到页码'}).fill(String(longCount));await page.frameLocator('.office-workbench__stage iframe').getByText('长文结束标记',{exact:true}).waitFor();
  await page.getByRole('tab',{name:'混合纸张.pdf',exact:true}).click();await page.locator('.office-workbench__stage .office-page[data-page-rendered=true]').waitFor();
  const geometry=()=>page.locator('.office-workbench__stage .office-page').boundingBox();const portrait=await geometry();assert(portrait.height>portrait.width);
  await page.getByRole('button',{name:'下一页',exact:true}).click();await page.locator('.office-workbench__stage .office-page[data-page-index="1"][data-page-rendered=true]').waitFor();const landscape=await geometry();assert(landscape.width>landscape.height,'each PDF page must use its own original geometry');
  for(const [file,theme,width] of [['春天.pptx','dark',1120],['经营数据.xlsx','dark',900],['春日计划.docx','light',440],['春天.pdf','dark',440]]){
    await page.setViewportSize({width,height:800});await page.goto(`${base}?file=${encodeURIComponent(file)}&theme=${theme}`);await page.locator(file.endsWith('xlsx')?'.office-sheet__viewport':'.office-workbench__stage .office-page[data-page-rendered=true]').waitFor({timeout:60000});
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1),false,'document workspace must not overflow the app window');await page.screenshot({path:join(output,`${file.split('.').pop()}-${theme}-${width}.png`)});
  }
  await page.setViewportSize({width:1120,height:850});await page.goto(`${base}?attachment=1&file=${encodeURIComponent('春日计划.docx')}`);
  await page.locator('.office-workbench__stage .office-page[data-page-rendered=true]').waitFor();
  await page.getByRole('button',{name:'全屏预览',exact:true}).click();
  for(let tab=0;tab<24;tab++){await page.keyboard.press('Tab');assert(await page.evaluate(()=>!!document.activeElement?.closest('.office-workbench--expanded')),'fullscreen Tab must stay above the attachment dialog');}
  await page.getByRole('button',{name:'退出全屏预览',exact:true}).focus();await page.keyboard.press('Escape');
  assert.equal(await page.getByRole('dialog',{name:'春日计划.docx',exact:true}).count(),1,'Escape must close only fullscreen');
  assert(await page.getByRole('button',{name:'全屏预览',exact:true}).evaluate(element=>element===document.activeElement),'fullscreen must restore focus');
  const previousReads=await page.evaluate(()=>window.officeReview.fileReads);
  writeFileSync(join(directory,'春日计划.docx'),await Packer.toBuffer(new Document({sections:[{children:[new Paragraph('附件原件已更新，刷新必须读取新版本')]}]})));
  await page.getByRole('button',{name:'重新读取文档',exact:true}).click();
  await page.frameLocator('.office-workbench__stage iframe').getByText('附件原件已更新，刷新必须读取新版本',{exact:true}).waitFor();
  assert.equal(await page.evaluate(()=>window.officeReview.fileReads),previousReads+1,'attachment refresh must re-read its binary');
  await page.getByRole('button',{name:'全屏预览',exact:true}).focus();await page.keyboard.press('Escape');assert.equal(await page.getByRole('dialog').count(),0,'second Escape closes the attachment');
  // Restore the original fixture for the independent native WebKit checks.
  writeFileSync(join(directory,'春日计划.docx'),await Packer.toBuffer(word));
  assert.deepEqual(errors,[]);console.log(`Office workbench: PPT/DOCX/XLSX/PDF previews, navigation, search, fullscreen, formulas, virtualization, dark/narrow layouts passed. Screenshots: ${output}`);
  if(process.argv.includes('--webkit') && process.platform==='darwin') {
    assertNativeReviewSessionUnlocked();
    const executable=join(output,'office-webkit-review'), run=promisify(execFile);
    await run('swiftc',[resolve('scripts/validate-office-workbench-webkit.swift'),'-o',executable]);
    const result=await run(executable,[base,output],{timeout:250000});console.log(result.stdout);
  }
}catch(error){await page.screenshot({path:join(output,'failure.png')});console.error('Office UI failure',output,await page.locator('body').innerText());throw error;}
finally{await browser.close();await server.close();}
