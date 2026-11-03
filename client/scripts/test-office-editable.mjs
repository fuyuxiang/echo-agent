import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,readFile,rm,stat} from 'node:fs/promises';
import {existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import JSZip from 'jszip';
import {PDFDocument,PDFName,decodePDFRawStream} from 'pdf-lib';
const directory=await mkdtemp(join(tmpdir(),'echo-editable-office-'));
const root=resolve(import.meta.dirname,'..'),worker=join(root,'src-tauri/resources/office/worker.mjs'),font=join(root,'src-tauri/resources/office/fonts/NotoSansCJKsc-Regular.otf');
const staged=join(root,'src-tauri/resources/theia/node',process.platform==='win32'?'node.exe':'bin/node');
const p=(text,marks)=>({type:'paragraph',content:text?[{type:'text',text,...(marks?{marks}:{})}]:[]});
const cell=(text,header=false)=>({type:header?'tableHeader':'tableCell',attrs:{colspan:1,rowspan:1,colwidth:null},content:[p(text)]});
const document={type:'doc',content:[{type:'heading',attrs:{level:1,textAlign:'center'},content:[{type:'text',text:'当前修订稿'}]},p('人工加粗的内容',[{type:'bold'},{type:'underline'}]),{type:'table',content:[{type:'tableRow',content:[cell('时间',true),cell('儿童安排',true),cell('家长安排',true)]},{type:'tableRow',content:[cell('7:30'),cell('起床、洗漱'),cell('准备早餐')]},{type:'tableRow',content:[cell('8:00'),cell(''),cell('送学')] }]}]};
async function run(format,model=document,markdown){
  const output=join(directory,'current.'+format);
  const child=spawn(existsSync(staged)?staged:process.execPath,[worker,output,font],{stdio:['pipe','ignore','pipe']});let errors='';child.stderr.on('data',chunk=>{errors+=chunk;});
  child.stdin.end(JSON.stringify({format,title:'当前修订稿',...(markdown===undefined?{document:model}:{markdown})}));
  const status=await new Promise((resolve,reject)=>{child.on('error',reject);child.on('close',resolve);});
  return {output,status,errors};
}
function pdfRuns(document){
  const maps=new Map();const decode=stream=>new TextDecoder().decode(decodePDFRawStream(stream).decode());const text=[];
  for(const page of document.getPages()){
    const fonts=page.node.Resources().lookup(PDFName.of('Font'));const contents=page.node.Contents();let font,x,y;
    for(let i=0;i<contents.size();i++)for(const match of decode(document.context.lookup(contents.get(i))).matchAll(/\/([^\s]+)\s+[\d.]+\s+Tf|1 0 0 1 ([\d.-]+) ([\d.-]+) Tm|<([\da-f]+)>\s*Tj/gi)){
      if(match[1]){const unicode=fonts.lookup(PDFName.of(match[1])).lookup(PDFName.of('ToUnicode'));if(!maps.has(unicode)){const map=new Map();for(const section of decode(unicode).matchAll(/beginbfchar([\s\S]*?)endbfchar/g))for(const pair of section[1].matchAll(/<([\da-f]{4})>\s*<([\da-f]+)>/gi))map.set(pair[1].toUpperCase(),String.fromCharCode(...pair[2].match(/.{4}/g).map(code=>parseInt(code,16))));maps.set(unicode,map);}font=maps.get(unicode);}
      else if(match[2]){x=Number(match[2]);y=Number(match[3]);}
      else text.push({x,y,text:match[4].match(/.{4}/g).map(code=>font.get(code.toUpperCase())).join('')});
    }
  }return text;
}
const pdfText=document=>pdfRuns(document).map(run=>run.text).join('\n');
try{
  const word=await run('docx');assert.equal(word.status,0,word.errors);assert((await stat(word.output)).size>1000);
  const zip=await JSZip.loadAsync(await readFile(word.output));const xml=await zip.file('word/document.xml').async('string');
  for(const value of ['当前修订稿','人工加粗的内容','7:30','准备早餐','8:00','送学'])assert(xml.includes(value),'DOCX lost '+value);
  assert(!xml.includes('6:30'),'DOCX contains a stale time');
  assert.equal((xml.match(/<w:tr>/g)??[]).length,3);assert.equal((xml.match(/<w:tc>/g)??[]).length,9);
  assert(xml.includes('<w:b/>')&&xml.includes('<w:u w:val="single"/>'),'manual formatting was not exported');
  assert(xml.includes('<w:jc w:val="center"/>'),'paragraph alignment was not exported');
  const saved=JSON.parse(await readFile(word.output+'.document.json','utf8'));assert.equal(saved.content[2].content[1].content[0].content[0].content[0].text,'7:30');
  const pdf=await run('pdf');assert.equal(pdf.status,0,pdf.errors);const text=pdfText(await PDFDocument.load(await readFile(pdf.output)));
  assert(text.includes('7:30')&&text.includes('准备早餐')&&!text.includes('6:30'),'PDF must reflect the current source');
  const merged={type:'doc',content:[{type:'table',content:[
    {type:'tableRow',content:[cell('项目',true),cell('负责人',true),cell('金额',true)]},
    {type:'tableRow',content:[{...cell('合并说明'),attrs:{colspan:2,rowspan:1,colwidth:null}},cell('100元')]},
    {type:'tableRow',content:[cell('普通项目'),cell('张三'),{...cell('第一段'),content:[p('第一段'),p('第二段')]}]},
  ]}]};
  const mergedWord=await run('docx',merged);assert.equal(mergedWord.status,0,mergedWord.errors);
  const mergedZip=await JSZip.loadAsync(await readFile(mergedWord.output));
  assert((await mergedZip.file('word/document.xml').async('string')).includes('<w:gridSpan w:val="2"/>'),'Word must retain horizontal merges');
  const mergedPdf=await run('pdf',merged);assert.equal(mergedPdf.status,0,mergedPdf.errors);
  const runs=pdfRuns(await PDFDocument.load(await readFile(mergedPdf.output)));
  const amount=runs.find(run=>run.text==='100元'),header=runs.find(run=>run.text==='金额'),owner=runs.find(run=>run.text==='负责人');
  assert(amount&&header&&owner,'PDF must contain the data and headers');
  assert(Math.abs(amount.x-header.x)<.01 && amount.x>owner.x,'amount after a merged cell must stay under the amount column');
  const first=runs.find(run=>run.text==='第一段'),second=runs.find(run=>run.text==='第二段');
  assert(first&&second&&first.x===second.x&&first.y>second.y,'cell paragraph boundaries must survive export');
  const linked='# 链接报告\n\n[结论](#结论) [附件](EchoAgent成果/report.docx) [网站](https://example.com)\n\n## 结论\n\n正文\n\n| 项目 | 金额 |\n| --- | --- |\n| 收入 | 100 |';
  for(const format of ['docx','pdf','xlsx','pptx']){
    const result=await run(format,undefined,linked);assert.equal(result.status,0,`${format}: ${result.errors}`);assert((await stat(result.output)).size>1000);
    if(format==='docx'){
      const source=JSON.parse(await readFile(result.output+'.document.json','utf8'));
      assert(JSON.stringify(source).includes('#结论')&&JSON.stringify(source).includes('EchoAgent成果/report.docx'),'Word must keep local destinations readable');
      assert(!JSON.stringify(source).includes('"href":"#'),'anchors must not become executable file hyperlinks');
    }
  }
  const invalid=await run('docx',{type:'doc',content:[{type:'script',content:[]}]});assert.notEqual(invalid.status,0,'unsupported document structures must be rejected');
  console.log('Editable document export passed: current content/formatting, merged-column PDF positions, cell paragraphs, four-format local links, durable source and invalid-input rejection.');
}finally{await rm(directory,{recursive:true,force:true});}
