import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { describe, expect, it } from 'vitest';
import { JSDOM } from 'jsdom';

const source=readFileSync(new URL('./fixtures/native-coding-workspace-review.js',import.meta.url),'utf8');
const start=source.indexOf('  const scopedSaveQuestion = ');
const end=source.indexOf('\n  const report = ',start);
if(start<0||end<0)throw new Error('Native scoped clarification guard missing');
const scoped=vm.runInNewContext(`${source.slice(start,end)}\nscopedSaveQuestion`);
const question='是否将 double.cjs 的未保存缓冲区及 UNSAVED_A01 注释保存到磁盘？';
const request=text=>({questions:[{question:text}]});
const discoveryStart=source.indexOf('  const scopedDiscovery = ');
const discoveryEnd=source.indexOf('\n  const report = ',discoveryStart);
if(discoveryStart<0||discoveryEnd<0)throw new Error('Native schema discovery guard missing');
const discovery=vm.runInNewContext(`${source.slice(discoveryStart,discoveryEnd)}\nscopedDiscovery`);
const readStart=source.indexOf('  const scopedRead = ');
const readEnd=source.indexOf('\n  const report = ',readStart);
if(readStart<0||readEnd<0)throw new Error('Native fixture read scope missing');
const scopedRead=vm.runInNewContext(`${source.slice(readStart,readEnd)}\nscopedRead`);
const planStart=source.indexOf('  const scopedSessionPlanRead = ');
const planEnd=source.indexOf('\n  const report = ',planStart);
if(planStart<0||planEnd<0)throw new Error('Native current-session plan scope missing');
const planRead=vm.runInNewContext(`${source.slice(planStart,planEnd)}\nscopedSessionPlanRead`);

describe('native Runtime-owned plan read scope',()=>{
  const home='/private/owned/review',workspace=`${home}/workspaces/default`,session='owned-session';
  const path=`${home}/sessions/${encodeURIComponent(workspace)}/${session}/plan.md`;
  it('reads only the exact current-session plan during planning or approved execution',()=>{
    expect(planRead(path,home,workspace,session,'plan')).toBe(true);
    expect(planRead(path,home,workspace,session,'agent')).toBe(true);
  });
  it('rejects other sessions/workspaces, other files, traversal and Ask mode',()=>{
    expect(planRead(path,home,workspace,session,'ask')).toBe(false);
    expect(planRead(path,home,workspace,'foreign-session','plan')).toBe(false);
    expect(planRead(path,home,workspace+'/other',session,'plan')).toBe(false);
    for(const target of [path.replace('plan.md','config.toml'),path+'/../plan.md','file://'+path])expect(planRead(target,home,workspace,session,'plan')).toBe(false);
  });
});

describe('native fixed fixture read scope',()=>{
  const root='/private/owned/workspace';
  it('reads the four seeded files, including the observed completion scratch read',()=>{
    for(const name of ['double.cjs','package.json','test.cjs','completion.cjs']){
      expect(scopedRead(name,root)).toBe(true);expect(scopedRead(`${root}/${name}`,root)).toBe(true);
    }
  });
  it('rejects foreign files, sibling roots, traversal, URIs and malformed paths',()=>{
    for(const path of ['/etc/passwd','../completion.cjs',`${root}-other/completion.cjs`,`${root}/../completion.cjs`,'file:///private/owned/workspace/double.cjs','other.cjs',null,{}])expect(scopedRead(path,root)).toBe(false);
  });
});

describe('native local schema discovery scope',()=>{
  const input={variant:'SearchTool',query:'editor save document buffer write file',limit:15};
  it('accepts the observed bounded editor-schema search in Agent',()=>expect(discovery(input,'agent')).toBe(true));
  it('accepts only the observed local verification-schema query without executing it',()=>{
    expect(discovery({...input,query:'local check run project verification',limit:8},'agent')).toBe(true);
    expect(discovery({...input,query:'local check run project verification /etc/passwd'},'agent')).toBe(false);
  });
  it('rejects another mode, actual execution and extra execution fields',()=>{
    expect(discovery(input,'plan')).toBe(false);expect(discovery(input,'ask')).toBe(false);
    expect(discovery({...input,variant:'MCPTool'},'agent')).toBe(false);
    expect(discovery({...input,tool_name:'write',tool_input:{}},'agent')).toBe(false);
  });
  it('rejects unrelated, network and filesystem queries',()=>{
    for(const query of ['slack send message','editor https://example.com','editor /etc/passwd','editor; write file','file'])expect(discovery({...input,query},'agent')).toBe(false);
  });
  it('rejects unbounded or malformed requests',()=>{
    for(const value of [null,{}, {...input,query:'editor '.repeat(30)},...[-1,0,16,1.5,'5'].map(limit=>({...input,limit}))])expect(discovery(value,'agent')).toBe(false);
  });
});

describe('native Plan buffer-save clarification scope',()=>{
  it('enters the actual question input and existing composer textarea with native input events',()=>{
    const dom=new JSDOM('<input><textarea></textarea>');
    try {
      const start=source.indexOf('  const text = (element, value) => {');
      const end=source.indexOf('\n  const inline = ',start);
      const enter=vm.runInNewContext(`${source.slice(start,end)}\ntext`,{
        HTMLInputElement:dom.window.HTMLInputElement,HTMLTextAreaElement:dom.window.HTMLTextAreaElement,Event:dom.window.Event,
      });
      for(const field of dom.window.document.querySelectorAll('input,textarea')) {
        let inputEvents=0;field.addEventListener('input',()=>inputEvents++);
        enter(field,'保留未保存注释');
        expect(field.value).toBe('保留未保存注释');expect(inputEvents).toBe(1);
      }
    }finally{dom.window.close();}
  });
  it('accepts the actual fixed-file save clarification in Plan',()=>expect(scoped(request(question),'plan')).toBe(true));
  it('recognizes the actual save sign-off with the fixed filename in an option',()=>{
    expect(scoped({questions:[{question:'上面的两步方案（改 n * 2 并连同 UNSAVED_A01 一起保存，验证由你点击本地检查）是否照此执行？',options:[
      {description:'Step 1 修改并保存 double.cjs（n * 2 + 保留 UNSAVED_A01），Step 2 由你点击本地检查验证。'},
      {description:'需要补充后再给出最终方案。'},
    ]}]},'plan')).toBe(true);
  });
  it('rejects an extra file even when double.cjs is also named',()=>{
    expect(scoped(request(question+'同时保存 other.cjs。'),'plan')).toBe(false);
    expect(scoped(request(question+'同时保存 test.cjs。'),'plan')).toBe(false);
  });
  it('recognizes the observed plan sign-off whose buffer marker is in an option',()=>{
    expect(scoped({questions:[{question:'两步只读方案是否可以定稿？定稿后才进入执行轮保存 double.cjs',options:[
      {description:'保留 UNSAVED_A01，执行另行确认'}, {description:'需要调整步骤内容'},
    ]}]},'plan')).toBe(true);
  });
  it('handles the observed save plus local-validation clarification without authorizing commands',()=>{
    const validation={question:'执行阶段（第 2 步）是否允许运行命令做动态校验？',options:[
      {description:'仅静态检查'}, {description:'执行 node --test test.cjs 与 node --check double.cjs'},
    ]};
    expect(scoped({questions:[validation,{question}]},'plan')).toBe(true);
    expect(scoped({questions:[{...validation,options:[{description:'运行其他命令'}]}]},'plan')).toBe(false);
  });
  it.each(['ask','agent',undefined])('rejects a question in mode %s',mode=>expect(scoped(request(question),mode)).toBe(false));
  it('rejects another file and unrelated decisions',()=>{
    expect(scoped(request(question.replace('double.cjs','other.cjs')),'plan')).toBe(false);
    expect(scoped(request('是否发布项目并发送账号信息？'),'plan')).toBe(false);
  });
  it('rejects malformed, excessive and unbounded questions',()=>{
    for(const value of [null,{},request(null),{questions:Array.from({length:4},()=>({question}))},request(question+'a'.repeat(600))])expect(scoped(value,'plan')).toBe(false);
  });
});
