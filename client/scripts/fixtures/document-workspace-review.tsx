import React, {useState} from 'react';
import {createRoot} from 'react-dom/client';
import JSZip from 'jszip';
import '../../src/styles/index.css';
import {ToolSidePanel} from '../../src/components/ToolSidePanel';
import {markdownToDocument} from '../../src/lib/document-model';
import type {EditableDocument,DocumentEditContext} from '../../src/lib/document-workspace';
import type {SessionArtifact} from '../../src/lib/session-artifacts';
document.documentElement.setAttribute("data-theme",new URLSearchParams(location.search).get("theme") ?? "light");
const scenario=new URLSearchParams(location.search).get('scenario');
const path=scenario?.startsWith('markdown')?'/review/文档说明.md':'/review/家庭亲子作息表.docx';
const markdownContent=scenario==='markdown-simple'?'# 可编辑说明\n\n**保留格式**与普通段落。':'# 原文阅读\n\n![架构示意](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aGHkAAAAASUVORK5CYII=)\n\n公式 $x^2+1$\n\n```mermaid\ngraph LR; A-->B\n```';
const initial:EditableDocument={id:'review-doc',sessionId:'document-review',path,title:'家庭亲子作息表',revision:1,exportedRevision:1,fileSha256:'a'.repeat(64),byteSize:100,document:markdownToDocument('# 家庭亲子作息表\n\n让每天的安排清晰、从容。\n\n| 时间 | 儿童安排 | 家长安排 |\n| --- | --- | --- |\n| 6:30 | 起床、洗漱 | 准备早餐 |\n| 7:00 | 早餐、整理书包 | 陪伴用餐 |'),history:[],actor:'生成文档',savedAt:Date.now()};
const records:Record<string,EditableDocument>=JSON.parse(localStorage.getItem('document-review-records') ?? '{}');
if(!scenario?.startsWith('markdown'))records[path]??=initial;
const calls:{command:string;args:any}[]=[];
const commit=()=>localStorage.setItem('document-review-records',JSON.stringify(records));
const clone=<T,>(value:T):T=>JSON.parse(JSON.stringify(value));
const review={calls,records,delayRestore:false,delayCopy:false,delayDraftSave:false,failPublication:false,failDraftSave:false,
  releaseRestore:null as null|(()=>void),releaseCopy:null as null|(()=>void),releaseDraftSave:null as null|(()=>void),agentUpdate:(path:string,text:string)=>{
  const record=records[path];record.history.push({revision:record.revision,document:clone(record.document),actor:record.actor,savedAt:record.savedAt});
  record.document=JSON.parse(JSON.stringify(record.document).replaceAll('6:30',text).replaceAll('7:00',text));record.revision++;record.exportedRevision=record.revision;record.actor='Agent 修改';record.savedAt=Date.now();commit();
}};
Object.assign(window,{documentReview:review,__TAURI_INTERNALS__:{transformCallback:()=>1,unregisterCallback:()=>{},invoke:async(command:string,args:any={})=>{
  calls.push({command,args});
  const record=records[args.path ?? path];
  if(command==='path_stat')return {absolute:args.path};
  if(command==='session_export_authorize')return;
  if(command==='read_file_base64'){
    const zip=new JSZip();zip.file('word/document.xml','<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>作息表</w:t></w:r></w:p></w:body></w:document>');return zip.generateAsync({type:'base64'});
  }
  if(command==='read_text_file')return markdownContent;
  if(command==='document_workspace_open')return clone({document:record ?? null,path:args.path,fileSha256:record?.fileSha256 ?? 'a'.repeat(64),byteSize:record?.byteSize ?? 100});
  if(command==='document_workspace_save'){
    if(review.delayDraftSave)await new Promise<void>(resolve=>{review.releaseDraftSave=()=>{review.releaseDraftSave=null;resolve();};});
    if(review.failDraftSave)throw new Error('编辑稿暂时不可写，修改仍保留在本地');
    if(record.revision!==args.expectedRevision)throw new Error('DOCUMENT_CONFLICT:文档已有新修改');
    record.history.push({revision:record.revision,document:clone(record.document),actor:record.actor,savedAt:record.savedAt});
    record.document=clone(args.document);record.revision++;record.savedAt=Date.now();record.actor='手动编辑';commit();return clone(record);
  }
  if(command==='document_workspace_publish'){if(review.failPublication)throw new Error('磁盘暂时不可写');if(record.revision!==args.expectedRevision)throw new Error('DOCUMENT_CONFLICT:文档已有新修改');record.exportedRevision=record.revision;commit();return clone(record);}
  if(command==='document_workspace_restore'){
    if(review.delayRestore)await new Promise<void>(resolve=>{review.releaseRestore=resolve;});
    if(record.revision!==args.expectedRevision)throw new Error('DOCUMENT_CONFLICT:文档已有新修改');
    const version=record.history.find(item=>item.revision===args.revision)!;const previous=clone(record.document);
    record.document=clone(version.document);record.history.push({revision:record.revision,document:previous,actor:record.actor,savedAt:record.savedAt});record.revision++;record.actor='恢复历史';commit();return clone(record);
  }
  if(command==='document_workspace_import'){
    if(review.delayCopy)await new Promise<void>(resolve=>{review.releaseCopy=()=>{review.releaseCopy=null;resolve();};});
    const next={...clone(record ?? initial),id:'copy',path:'/review/家庭亲子作息表-可编辑副本.docx',document:clone(args.document),revision:1,exportedRevision:1,history:[]};records[next.path]=next;commit();return clone(next);
  }
  if(command==='document_workspace_export')return {path:`/review/当前版本.${args.format}`,format:args.format,sha256:'b'.repeat(64),byteSize:100};
  if(command==='plugin:event|listen')return 1;
  if(command==='plugin:event|unlisten')return;
  return [];
}},__TAURI_EVENT_PLUGIN_INTERNALS__:{unregisterListener:()=>{}}});
function Review(){
  const [artifact,setArtifact]=useState<SessionArtifact>({id:path,path,kind:'file',title:'家庭亲子作息表',toolCallId:'',status:'completed'});
  const [context,setContext]=useState<DocumentEditContext|null>(null);
  const [toast,setToast]=useState('');
  const [panelOpen,setPanelOpen]=useState(true);
  return <div className="app" style={{height:'100%',display:'flex',flexDirection:'row'}}>
    <main className="app__main" style={{position:'relative'}}><div className="chatview"><div className="chatview__main" style={{padding:32}}><h2>文档修订任务</h2><p>检查作息安排，并继续调整文档。</p>{context&&<div role="status" data-testid="document-context">正在修改：{context.title}<textarea aria-label="修改要求" placeholder="描述你希望调整的内容"/></div>}{toast&&<div role="status">{toast}</div>}</div>
    {!panelOpen&&<button type="button" onClick={()=>setPanelOpen(true)}>打开文档工作区</button>}
    <ToolSidePanel open={panelOpen} mode="artifacts" artifacts={[artifact]} previewPath={artifact.path} previewRequestId={1} cwd="/review" sessionId="document-review"
      onClose={()=>setPanelOpen(false)} onSelectTool={()=>{}} onSelectArtifact={next=>{setArtifact(next);setPanelOpen(true);}} onOpenArtifacts={()=>{}} onToast={setToast} onEditDocument={next=>{setContext(next);setPanelOpen(false);Object.assign(window,{documentEditContext:next});}}/>
    </div></main></div>;
}
createRoot(document.getElementById('root')!).render(<Review/>);
