import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { EditorContent, useEditor } from '@tiptap/react';
import StarterKit from '@tiptap/starter-kit';
import { TableKit } from '@tiptap/extension-table';
import TextAlign from '@tiptap/extension-text-align';
import { Bold, Italic, Underline, List, ListOrdered, AlignLeft, AlignCenter, AlignRight, Undo2, Redo2, Table2, Download, History, Sparkles, Pencil, Check, Copy, Plus, Trash2 } from 'lucide-react';
import { DocumentCanvas } from './DocumentCanvas';
import { FilePreview } from './FilePreview';
import { ActionMenu } from './ActionMenu';
import { documentSession, type DocumentEditContext, type EditableDocument, type DocumentSnapshot } from '@/lib/document-workspace';
import { markdownToDocument, markdownDocumentLimitations, normalizeDocument, documentText } from '@/lib/document-model';
import { importDocxDocument } from '@/lib/docx-document';
import { readZipFromBase64, makeDocZipReader } from '@/lib/zip-reader';
import { copyShareText } from '@/lib/share';
import { registerDocumentArtifact } from '@/lib/artifact-catalog';
import { useSessionsStore } from '@/stores/sessions-store';
import type { SessionArtifact } from '@/lib/session-artifacts';
import { listen } from '@tauri-apps/api/event';

export interface DocumentWorkbenchProps {
  path: string; sessionId: string; cwd?: string; content: string;
  active?: boolean;
  onToast?: (message:string)=>void;
  onAskAgent?: (context:DocumentEditContext)=>void;
  onOpenArtifact?: (artifact:SessionArtifact)=>void;
  onEditModeChange?: (editing:boolean)=>void;
  onReload?: ()=>void;
  agentDisabled?: boolean;
}

function documentStatus(state:DocumentSnapshot):string {
  if(state.operation==='restore')return '正在恢复历史版本…';
  if(state.operation==='copy')return '正在创建副本…';
  if(state.operation==='reload')return '正在读取最新稿…';
  if(state.phase==='loading')return '正在读取编辑稿…';
  if(state.phase==='saving')return '正在保存编辑稿…';
  if(state.phase==='dirty')return '正在保存修改…';
  if(state.phase==='conflict')return '存在版本冲突';
  if(state.phase==='error') {
    if(state.failure==='save')return '编辑稿保存失败 · 修改已保留';
    return state.record?'读取最新稿失败 · 当前内容已保留':'文档读取失败';
  }
  if(!state.record)return '原件内容预览';
  if(state.publishing)return '正在更新 Word…';
  if(state.syncError)return '编辑稿已保存 · Word 同步失败';
  return state.record.revision===state.record.exportedRevision?'已保存 · Word 已同步':'编辑稿已保存 · Word 待同步';
}

export default function DocumentWorkbench({path,sessionId,cwd,content,active=true,onToast,onAskAgent,onOpenArtifact,onEditModeChange,onReload,agentDisabled}:DocumentWorkbenchProps) {
  const session=useMemo(()=>documentSession(sessionId,path),[sessionId,path]);
  const state=useSyncExternalStore(session.subscribe,session.snapshot,session.snapshot);
  const [editing,setEditing]=useState(false),[busy,setBusy]=useState(false),[showHistory,setShowHistory]=useState(false);
  const [,transaction]=useState(0);
  const selection=useRef('');
  const viewEpoch=useRef(0);
  useLayoutEffect(()=>{viewEpoch.current++;return()=>{viewEpoch.current++;};},[session,active]);
  // A hidden view cannot navigate, but its pending action must still unlock
  // this document after completion, including when the panel has reopened.
  const actions=useMemo(()=>({pending:0,mounted:false}),[session]);
  useLayoutEffect(()=>{actions.mounted=true;setBusy(actions.pending>0);return()=>{actions.mounted=false;};},[actions]);
  const limitationsId=useId();
  const markdown=/\.(md|markdown|mdown|mdx)$/i.test(path);
  const locked=busy || Boolean(state.operation);
  const initial=useMemo(()=>{
    try {
      if (markdown) return {document:markdownToDocument(content),limitations:markdownDocumentLimitations(content)};
      return importDocxDocument(makeDocZipReader(readZipFromBase64(content)));
    } catch {return null;}
  },[path,content,markdown]);
  const model=state.draft ?? initial?.document;
  const editor=useEditor({
    extensions:[StarterKit.configure({heading:{levels:[1,2,3]},link:{openOnClick:false}}),TableKit.configure({table:{resizable:false,allowTableNodeSelection:true}}),TextAlign.configure({types:['heading','paragraph']})],
    content:model ?? {type:'doc',content:[{type:'paragraph'}]},
    editable:false,
    editorProps:{attributes:{class:'document-paper document-editor', 'aria-label':'文档编辑区',role:'textbox','aria-multiline':'true'}},
    onUpdate:({editor:current})=>{if(!current.isEditable || !session.snapshot().record)return;try {session.edit(normalizeDocument(current.getJSON()));}catch(error){const previous=session.snapshot().draft;if(previous)current.commands.setContent(previous,{emitUpdate:false});onToast?.(String(error));}},
    onSelectionUpdate:({editor:current})=>{selection.current=current.state.doc.textBetween(current.state.selection.from,current.state.selection.to,'\n').slice(0,8000);transaction(value=>value+1);},
    onTransaction:()=>transaction(value=>value+1),
  },[session]);
  useLayoutEffect(()=>{editor?.setEditable(active && editing && Boolean(state.record) && !locked, false);},[editor,active,editing,state.record?.id,locked]);
  useEffect(()=>{
    if(!editor || !state.draft)return;
    if(JSON.stringify(normalizeDocument(editor.getJSON()))!==JSON.stringify(state.draft)) editor.commands.setContent(state.draft,{emitUpdate:false});
  },[editor,state.draft]);
  useEffect(()=>{onEditModeChange?.(active && editing);return()=>onEditModeChange?.(false);},[active,editing,onEditModeChange]);
  useEffect(()=>{
    if(!active)return;
    void session.refresh();
    const interval=setInterval(()=>{if(document.visibilityState!=='hidden')void session.refresh();},2500);
    const refresh=()=>{void session.refresh();};window.addEventListener('focus',refresh);
    let disposed=false,unlisten:(()=>void)|undefined;
    void listen<{sessionId:string;path:string}>('document-workspace-changed',event=>{if(event.payload.sessionId===sessionId && event.payload.path===state.record?.path)refresh();}).then(stop=>{if(disposed)stop();else unlisten=stop;}).catch(()=>{});
    return()=>{clearInterval(interval);window.removeEventListener('focus',refresh);disposed=true;unlisten?.();};
  },[active,session,sessionId,state.record?.path]);
  // DocumentSession owns durable saves. Only the initiating view may navigate,
  // inject composer context, or report a transient result after they finish.
  const run=async(action:(isCurrent:()=>boolean)=>Promise<unknown>)=>{
    if(!active)return;
    const epoch=viewEpoch.current,isCurrent=()=>actions.mounted && viewEpoch.current===epoch;
    actions.pending++;
    setBusy(true);
    try{await action(isCurrent);}catch(error){if(isCurrent())onToast?.(String(error).replace(/^Error:\s*/,''));}
    finally{actions.pending--;if(actions.mounted)setBusy(actions.pending>0);}
  };
  const openRecord=(record:EditableDocument,navigate=true)=>{
    const title=useSessionsStore.getState().independent.find(item=>item.sessionId===sessionId)?.title ?? record.title;
    const artifact=registerDocumentArtifact({path:record.path,format:'docx',sha256:record.fileSha256,byteSize:record.byteSize},sessionId,title,cwd ?? '');
    if(navigate)onOpenArtifact?.(artifact);
  };
  const createCopy=()=>run(async isCurrent=>{if(!model)return;const record=await session.copy(model);openRecord(record,isCurrent());if(isCurrent())onToast?.('已创建可编辑副本，原件已保留');});
  const askAgent=()=>run(async isCurrent=>{
    await session.flush();if(!isCurrent())return;const record=session.snapshot().record;if(!record)return;
    if(record.externalChange)throw new Error('原 Word 文件已有外部修改，请先另存为副本');
    onAskAgent?.({sessionId,path:record.path,title:record.title,revision:record.revision,selectedText:selection.current});
    setEditing(false);
  });
  const status=documentStatus(state);
  const retryRead=state.failure==='read'||!state.record;
  if(!model)return <div className="document-workbench"><FilePreview filename={path.split(/[\\/]/).pop() ?? path} content={content}/><p className="document-workbench__hint">此文档暂不支持编辑，请使用系统应用查看原件。</p></div>;
  return <section className="document-workbench" aria-label="文档工作区">
    <div className="document-workbench__actions">
      <span className="document-workbench__status" role="status">{status}</span>
      {state.record ? <>
        <button type="button" disabled={locked} aria-pressed={editing} onClick={()=>setEditing(value=>!value)}>{editing?<Check size={14}/>:<Pencil size={14}/>}{editing?'完成编辑':'编辑'}</button>
        {onAskAgent && <button type="button" disabled={locked||agentDisabled||state.phase==='conflict'} onMouseDown={event=>event.preventDefault()} onClick={()=>void askAgent()}><Sparkles size={14}/>让 Agent 修改{selection.current?'选中内容':''}</button>}
        <button type="button" disabled={locked||state.publishing||state.phase==='conflict'||state.record.externalChange} onClick={()=>void run(()=>session.publish())}>{state.syncError?'重试更新 Word':'保存到 Word'}</button>
        <ActionMenu busy={locked||state.phase==='conflict'} label="导出文档" menuLabel="文档导出格式" trigger={<Download size={15}/>}>
          {({close})=><>{(['docx','pdf'] as const).map(format=><button type="button" role="menuitem" disabled={locked||state.phase==='conflict'} className="topbar-actions__item" key={format} onClick={()=>{close(true);void run(async isCurrent=>{const receipt=await session.export(format);if(receipt){if(isCurrent())onToast?.('已导出当前版本');if(format==='docx')openRecord({...state.record!,path:receipt.path,fileSha256:receipt.sha256,byteSize:receipt.byteSize},isCurrent());}});}}>另存为 {format==='docx'?'Word':'PDF'}</button>)}</>}
        </ActionMenu>
        <button type="button" aria-label="文档版本历史" aria-expanded={showHistory} onClick={()=>setShowHistory(value=>!value)}><History size={15}/></button>
      </> : <button type="button" disabled={locked||state.phase==='loading'||Boolean(initial?.limitations.length)||!state.opened} aria-describedby={initial?.limitations.length?limitationsId:undefined} onClick={()=>void createCopy()}><Pencil size={14}/>{locked?'正在创建副本…':'创建可编辑副本'}</button>}
      <button type="button" aria-label="复制文本" onClick={()=>{if(!active)return;const epoch=viewEpoch.current;void copyShareText(markdown&&!state.record?content:documentText(model)).then(ok=>{if(actions.mounted && viewEpoch.current===epoch)onToast?.(ok?'已复制文件内容':'复制失败');});}}><Copy size={14}/></button>
    </div>
    {!state.record && <p id={limitationsId} className="document-workbench__hint">{initial?.limitations.length?`此文件包含${initial.limitations.join('、')}，暂不能完整转换为可编辑 Word 副本。${markdown?'可继续阅读原文，或让 Agent 根据原文件另行整理。':'请用系统应用查看完整原件。'}`:'可查看段落和表格；创建可编辑副本后可直接调整，原文件会保留。'}</p>}
    {(state.message||state.record?.externalChange) && <div className="document-workbench__notice" role="alert">
      <p>{state.message || 'Word 文件已在其他位置修改。编辑稿已保留，请另存为副本，避免覆盖外部修改。'}</p>
      {state.phase==='conflict' && <button type="button" disabled={locked} onClick={()=>void run(()=>session.refresh(true))}>放弃本地改动，读取最新稿</button>}
      {(state.phase==='conflict'||state.record?.externalChange) && <button type="button" disabled={locked} onClick={()=>void createCopy()}>将我的修改另存为副本</button>}
      {state.phase==='error' && <button type="button" disabled={locked} onClick={()=>void run(()=>retryRead?session.refresh():session.flush())}>{retryRead?'重试读取':'重试保存'}</button>}
      {!state.record && onReload && <button type="button" onClick={onReload}>重新读取文件</button>}
    </div>}
    {state.syncError && !state.record?.externalChange && <div className="document-workbench__notice" role="alert">
      <p>编辑稿已保留，Word 文件尚未更新：{state.syncError}</p>
      <button type="button" disabled={locked||state.publishing||state.phase==='conflict'} onClick={()=>void run(()=>session.publish())}>重试同步 Word</button>
    </div>}
    {showHistory && state.record && <div className="document-workbench__history" aria-label="文档版本列表">
      <p>恢复历史会创建新版本，当前版本仍会保留。</p>
      {[...state.record.history].reverse().map(version=><div key={version.revision}><span>版本 {version.revision} · {version.actor} · {new Date(version.savedAt).toLocaleTimeString()}</span><button type="button" disabled={locked||state.phase==='conflict'} onClick={()=>void run(()=>session.restore(version.revision))}>恢复此版本</button></div>)}
      {!state.record.history.length && <p>首次修改后会在这里保留历史版本。</p>}
    </div>}
    {editing && editor && <fieldset disabled={locked} className="document-workbench__format" role="toolbar" aria-label="文档格式">
      <select aria-label="段落样式" value={editor.isActive('heading',{level:1})?'1':editor.isActive('heading',{level:2})?'2':editor.isActive('heading',{level:3})?'3':'0'} onChange={event=>{const level=Number(event.target.value);if(level)editor.chain().focus().toggleHeading({level:level as 1|2|3}).run();else editor.chain().focus().setParagraph().run();}}><option value="0">正文</option><option value="1">标题 1</option><option value="2">标题 2</option><option value="3">标题 3</option></select>
      {[
        ['加粗',Bold,()=>editor.chain().focus().toggleBold().run(),editor.isActive('bold')],
        ['斜体',Italic,()=>editor.chain().focus().toggleItalic().run(),editor.isActive('italic')],
        ['下划线',Underline,()=>editor.chain().focus().toggleUnderline().run(),editor.isActive('underline')],
        ['无序列表',List,()=>editor.chain().focus().toggleBulletList().run(),editor.isActive('bulletList')],
        ['有序列表',ListOrdered,()=>editor.chain().focus().toggleOrderedList().run(),editor.isActive('orderedList')],
        ['左对齐',AlignLeft,()=>editor.chain().focus().setTextAlign('left').run(),editor.isActive({textAlign:'left'})],
        ['居中对齐',AlignCenter,()=>editor.chain().focus().setTextAlign('center').run(),editor.isActive({textAlign:'center'})],
        ['右对齐',AlignRight,()=>editor.chain().focus().setTextAlign('right').run(),editor.isActive({textAlign:'right'})],
      ].map(([label,Icon,action,active])=>{const ButtonIcon=Icon as typeof Bold;return <button type="button" key={String(label)} aria-label={String(label)} data-tip={String(label)} aria-pressed={Boolean(active)} onMouseDown={event=>event.preventDefault()} onClick={()=>{(action as ()=>void)();transaction(value=>value+1);}}><ButtonIcon size={15}/></button>;})}
      <button type="button" aria-label="撤销编辑" data-tip="撤销" disabled={!editor.can().undo()} onClick={()=>editor.chain().focus().undo().run()}><Undo2 size={15}/></button>
      <button type="button" aria-label="重做编辑" data-tip="重做" disabled={!editor.can().redo()} onClick={()=>editor.chain().focus().redo().run()}><Redo2 size={15}/></button>
      <button type="button" aria-label="插入表格" data-tip="插入表格" onClick={()=>editor.chain().focus().insertTable({rows:3,cols:3,withHeaderRow:true}).run()}><Table2 size={15}/></button>
      {editor.isActive('table') && <ActionMenu busy={locked} label="表格操作" menuLabel="表格编辑操作" trigger={<Plus size={15}/>}>
        {({close})=><>{[['在下方添加行',()=>editor.chain().focus().addRowAfter().run()],['在右侧添加列',()=>editor.chain().focus().addColumnAfter().run()],['删除当前行',()=>editor.chain().focus().deleteRow().run()],['删除当前列',()=>editor.chain().focus().deleteColumn().run()],['删除整个表格',()=>editor.chain().focus().deleteTable().run()]].map(([label,action])=><button type="button" role="menuitem" className="topbar-actions__item" key={String(label)} onClick={()=>{close();(action as ()=>void)();}}>{String(label).startsWith('删除')?<Trash2 size={14}/>:<Plus size={14}/>}{String(label)}</button>)}</>}
      </ActionMenu>}
    </fieldset>}
    <div className="document-workbench__canvas" onMouseUp={()=>{if(!editing){const selected=window.getSelection();selection.current=selected?.anchorNode && selected.focusNode && selected.rangeCount && selected.getRangeAt(0).commonAncestorContainer.parentElement?.closest('.document-workbench__canvas')?selected.toString().slice(0,8000):'';transaction(value=>value+1);}}}>
      {editing?<EditorContent editor={editor}/>:markdown&&!state.record?<FilePreview filename={path.split(/[\\/]/).pop() ?? path} content={content}/>:<DocumentCanvas document={model}/>}
    </div>
    <small className="document-workbench__footnote">{markdown&&!state.record?'按 Markdown 原文阅读，创建可编辑副本不会修改原文件。':'内容与基础格式实时呈现。Word 的分页和复杂排版以导出文件为准。'}</small>
  </section>;
}
