import { invoke } from '@tauri-apps/api/core';
import { normalizeDocument, type DocumentNode } from './document-model';
import { ARTIFACT_CATALOG_EVENT } from './artifact-catalog';
import type { DocumentExportReceipt } from './document-export';

export interface DocumentVersion { revision: number; savedAt: number; actor: string; document: DocumentNode }
export interface EditableDocument {
  id: string; sessionId: string; path: string; title: string; revision: number; exportedRevision: number;
  fileSha256: string; byteSize: number; document: DocumentNode; savedAt: number; actor: string;
  history: DocumentVersion[]; externalChange?: boolean;
}
export interface OpenDocument {
  document: EditableDocument | null; path: string; fileSha256: string; byteSize: number;
  sourceMissing?: boolean;
}
export interface DocumentEditContext { sessionId: string; path: string; title: string; revision: number; selectedText: string }
export interface DocumentSnapshot {
  opened: OpenDocument | null; record: EditableDocument | null; draft: DocumentNode | null;
  phase: 'loading'|'ready'|'dirty'|'saving'|'conflict'|'error'; message: string; publishing: boolean;
  syncError: string;
  operation: 'restore'|'copy'|'reload'|null;
  failure: 'read'|'save'|null;
}
const sessions = new Map<string, DocumentSession>();
function changed(sessionId: string) {
  window.dispatchEvent(new CustomEvent(ARTIFACT_CATALOG_EVENT, {detail:{sessionId,source:'document-export'}}));
}
/** Save queues outlive the panel: switching files must not drop a pending edit. */
export class DocumentSession {
  private state: DocumentSnapshot = {opened:null,record:null,draft:null,phase:'loading',message:'',publishing:false,syncError:'',operation:null,failure:null};
  private listeners = new Set<() => void>();
  private dirty = false;
  private saveTimer?: ReturnType<typeof setTimeout>;
  private publishTimer?: ReturnType<typeof setTimeout>;
  private inflight: Promise<void> | null = null;
  private publication: Promise<void> | null = null;
  private refreshing = false;
  private disposed = false;
  private editEpoch = 0;
  private publishFailures = 0;
  private recoveryKey: string;
  constructor(readonly sessionId: string, readonly path: string, private readonly recoverMissingOnly = false) {
    this.recoveryKey = `echoagent.document-draft:${sessionId}:${path}`;
    void this.refresh();
  }
  subscribe = (listener: () => void) => {this.listeners.add(listener);return () => {this.listeners.delete(listener);};};
  snapshot = () => this.state;
  private update(patch: Partial<DocumentSnapshot>) {if(this.disposed)return;this.state={...this.state,...patch,...(patch.phase && patch.phase!=='error'?{failure:null}:{})};this.listeners.forEach(listener=>listener());}
  private retainRecovery() {
    if (this.disposed || !this.state.record || !this.state.draft) return;
    try { localStorage.setItem(this.recoveryKey,JSON.stringify({revision:this.state.record.revision,document:this.state.draft})); }
    catch { /* Native saves are still queued; failed saves stay visible in the panel. */ }
  }
  private clearRecovery() {try {localStorage.removeItem(this.recoveryKey);} catch { /* optional crash recovery */ }}
  private assertAvailable() {
    if (this.disposed) throw new Error('任务已删除');
    if (this.state.operation) throw new Error('正在处理文档，请稍候再试');
  }
  private assertWritable() {
    this.assertAvailable();
    if (this.recoverMissingOnly || this.state.opened?.sourceMissing) throw new Error('原件缺失，可导出已保存的编辑稿');
  }
  private clearTimers() {
    clearTimeout(this.saveTimer);this.saveTimer=undefined;
    clearTimeout(this.publishTimer);this.publishTimer=undefined;
  }
  private schedulePublish() {
    const record=this.state.record;
    if (this.disposed || this.state.opened?.sourceMissing || this.dirty || this.state.operation || this.state.phase==='conflict'
      || this.publication || this.publishTimer || this.publishFailures>=3
      || !record || record.externalChange || record.revision<=record.exportedRevision) return;
    const delay=this.publishFailures ? [0,2000,5000][this.publishFailures] : 1200;
    this.publishTimer=setTimeout(()=>{this.publishTimer=undefined;void this.publish(true).catch(()=>{});},delay);
  }
  async refresh(discardLocal = false) {
    if (this.disposed || this.refreshing || this.inflight || this.publication || this.state.operation) return;
    this.refreshing=true;
    const epoch=this.editEpoch;
    if(discardLocal){this.clearTimers();this.update({operation:'reload'});}
    try {
      const opened=await invoke<OpenDocument>('document_workspace_open',{
        sessionId:this.sessionId,path:this.path,...(this.recoverMissingOnly?{recoverMissingOnly:true}:{}),
      });
      if(this.disposed)return;
      if (!opened || typeof opened.path!=='string') throw new Error('文档编辑能力暂不可用，请重新打开任务');
      const record=opened.document;
      // A read can return after a save or an exclusive operation started.
      // It must never roll the displayed source/revision back in time.
      if (!discardLocal && this.state.operation
        || record && this.state.record && record.revision<this.state.record.revision) return;
      if (opened.sourceMissing) {
        // Only the native saved revision is readable/exportable without the
        // original. Keep newer in-memory edits parked until it returns.
        this.clearTimers();
        this.update({opened, record:this.dirty?this.state.record:record,
          draft:this.dirty?this.state.draft:record?.document ?? null, phase:'ready', message:'', syncError:''});
        return;
      }
      if (this.dirty && (!discardLocal || epoch!==this.editEpoch)) {
        if (record?.revision!==this.state.record?.revision || discardLocal) this.update({phase:'conflict',message:'文档已有新修改，你的编辑已保留。可以读取最新稿，或将你的修改另存为副本。',opened});
        else if(this.state.opened?.sourceMissing) this.update({opened,phase:'dirty'});
        return;
      }
      this.dirty=false;
      let draft=record?.document ?? null;
      let conflict=false;
      if (!discardLocal && record) {
        try {
          const raw=localStorage.getItem(this.recoveryKey);
          if (raw) {
            const recovery=JSON.parse(raw) as {revision:number;document:unknown};
            const recovered=normalizeDocument(recovery.document);
            if (JSON.stringify(recovered)!==JSON.stringify(record.document)) {
              draft=recovered;this.dirty=true;conflict=recovery.revision!==record.revision;
            } else this.clearRecovery();
          }
        } catch { /* A corrupt recovery never replaces the verified native source. */ }
      }
      if (discardLocal) this.clearRecovery();
      const synchronized=record && record.exportedRevision>=record.revision;
      if(synchronized)this.publishFailures=0;
      this.update({opened,record,draft,phase:conflict?'conflict':this.dirty?'dirty':'ready',message:conflict?'已恢复上次未保存的编辑，但文档已有更新。请另存副本或读取最新稿。':'',syncError:synchronized?'':this.state.syncError});
    } catch(error) {this.update({phase:'error',failure:'read',message:String(error).replace(/^Error:\s*/,'')});}
    finally {
      this.refreshing=false;
      if(discardLocal)this.update({operation:null});
      if(this.dirty && this.state.phase!=='conflict')this.scheduleSave();
      else this.schedulePublish();
    }
  }
  edit(document: DocumentNode) {
    if(this.disposed || this.recoverMissingOnly || this.state.opened?.sourceMissing)return;
    const draft=normalizeDocument(document);
    this.editEpoch+=1;
    this.publishFailures=0;
    this.dirty=true;
    this.update({draft,phase:this.state.phase==='conflict'?'conflict':'dirty',message:this.state.phase==='conflict'?this.state.message:''});
    this.retainRecovery();
    if (this.state.phase!=='conflict') this.scheduleSave();
  }
  private scheduleSave() {
    this.clearTimers();
    if(this.disposed || this.state.opened?.sourceMissing || this.state.operation)return;
    this.saveTimer=setTimeout(()=>{this.saveTimer=undefined;void this.flush().catch(()=>{});},500);
  }
  async flush(): Promise<void> {
    this.assertWritable();
    return this.savePending();
  }
  private async savePending(): Promise<void> {
    clearTimeout(this.saveTimer);
    this.saveTimer=undefined;
    if(this.disposed)throw new Error("任务已删除");
    if (this.state.phase==='conflict') throw new Error(this.state.message);
    if (this.inflight) {await this.inflight;if(this.dirty)return this.savePending();return;}
    this.inflight=(async()=>{
      try {
        while(this.dirty && this.state.record && this.state.draft) {
          const draft=this.state.draft, expectedRevision=this.state.record.revision;
          this.update({phase:'saving'});
          const record=await invoke<EditableDocument>('document_workspace_save',{sessionId:this.sessionId,path:this.state.record.path,expectedRevision,document:draft});
          if(this.disposed)throw new Error('任务已删除');
          if (!record?.revision) throw new Error('编辑稿未得到保存确认');
          this.dirty=this.state.draft!==draft;
          this.update({record,phase:this.dirty?'dirty':'ready',message:''});
          if (this.dirty) this.retainRecovery();else this.clearRecovery();
        }
        this.schedulePublish();
      } catch(error) {
        const message=String(error).replace(/^Error:\s*/,'');
        this.update({phase:message.includes('DOCUMENT_CONFLICT:')?'conflict':'error',failure:'save',message:message.replace('DOCUMENT_CONFLICT:','')});
        this.retainRecovery();throw error;
      }
    })();
    try {await this.inflight;} finally {this.inflight=null;}
  }
  async publish(automatic = false): Promise<void> {
    this.assertWritable();
    if(!automatic)this.publishFailures=0;
    await this.flush();clearTimeout(this.publishTimer);this.publishTimer=undefined;
    this.assertWritable();
    if (this.publication) {await this.publication;if(this.state.record && this.state.record.revision>this.state.record.exportedRevision)return this.publish(automatic);return;}
    const record=this.state.record;if(!record)throw new Error('请先创建可编辑副本');
    if(record.externalChange)throw new Error('Word 文件已在其他位置修改，请将编辑稿另存为副本');
    if(record.exportedRevision>=record.revision){this.update({syncError:''});return;}
    this.update({publishing:true});
    this.publication=(async()=>{
      try {
        const updated=await invoke<EditableDocument>('document_workspace_publish',{sessionId:this.sessionId,path:record.path,expectedRevision:record.revision});
        if(this.disposed)return;
        if (!updated?.revision) throw new Error('Word 文件未得到更新确认');
        this.publishFailures=0;
        if ((this.state.record?.revision ?? 0)<=updated.revision) this.update({record:updated});
        this.update({syncError:''});
        changed(this.sessionId);
      } catch(error) {
        this.publishFailures+=1;
        const message=String(error).replace(/^Error:\s*/,'');
        this.update({syncError:message.replace('DOCUMENT_EXTERNAL_CHANGE:','').replace('DOCUMENT_CONFLICT:',''),
          ...(message.includes('DOCUMENT_EXTERNAL_CHANGE:') && this.state.record ? {record:{...this.state.record,externalChange:true}} : {})});
        throw error;
      }
      finally {this.update({publishing:false});}
    })();
    try {await this.publication;} finally {this.publication=null;this.schedulePublish();}
  }
  async restore(revision:number) {
    this.assertWritable();this.clearTimers();this.update({operation:'restore'});
    try {
      await this.publication?.catch(()=>{});
      await this.savePending();
      if(this.disposed)throw new Error('任务已删除');
      const current=this.state.record;if(!current)return;
      const epoch=this.editEpoch;
      const record=await invoke<EditableDocument>('document_workspace_restore',{sessionId:this.sessionId,path:current.path,expectedRevision:current.revision,revision});
      if(this.disposed)throw new Error('任务已删除');
      if(epoch!==this.editEpoch){
        const message='历史版本已恢复，恢复期间的新输入已保留。请读取最新稿核对，或将你的修改另存为副本。';
        this.update({phase:'conflict',message});this.retainRecovery();throw new Error(message);
      }
      this.dirty=false;this.clearRecovery();this.update({record,draft:record.document,phase:'ready',message:'',syncError:''});
    } finally {this.update({operation:null});if(this.dirty && this.state.phase!=='conflict')this.scheduleSave();}
    await this.publish();
  }
  async export(format:'docx'|'pdf', savedRevision?:number) {
    this.assertAvailable();
    if ((this.state.opened?.sourceMissing || savedRevision!==undefined) && this.state.failure==='read') throw new Error(this.state.message);
    if (!this.state.opened?.sourceMissing && savedRevision===undefined) await this.flush();
    const opened=this.state.opened;
    const record=opened?.sourceMissing?opened.document:this.state.record;
    if(!record)throw new Error('找不到可导出的编辑稿');
    const receipt=await invoke<DocumentExportReceipt|null>('document_workspace_export',{
      sessionId:this.sessionId,path:record.path,expectedRevision:savedRevision ?? record.revision,format,
      ...(savedRevision===undefined ? {} : {recoveredCopy:true}),
    });
    if(receipt)changed(this.sessionId);return receipt;
  }
  dispose() {this.disposed=true;this.dirty=false;this.clearTimers();this.clearRecovery();this.listeners.clear();}
  async copy(document:DocumentNode, sourcePreview?:string) {
    this.assertWritable();this.clearTimers();this.update({operation:'copy'});
    const epoch=this.editEpoch;
    try {
      await this.inflight?.catch(()=>{});
      if(sourcePreview===undefined && !this.state.record)throw new Error('请重新读取原件后创建副本');
      const source=sourcePreview===undefined
        ? {expectedSha256:(await invoke<OpenDocument>('document_workspace_open',{sessionId:this.sessionId,path:this.path})).fileSha256}
        : {sourcePreview};
      if(this.disposed)throw new Error('任务已删除');
      const record=await invoke<EditableDocument>('document_workspace_import',{
        sessionId:this.sessionId,path:this.path,...source,document:normalizeDocument(document),
      });
      if(this.disposed)throw new Error('任务已删除');
      changed(this.sessionId);
      if(epoch!==this.editEpoch)throw new Error(`副本已保存到 ${record.path}。创建期间的新输入仍保留在当前文档中，请核对后继续。`);
      this.dirty=false;this.clearRecovery();return record;
    } finally {this.update({operation:null});if(this.dirty && this.state.phase!=='conflict')this.scheduleSave();else this.schedulePublish();}
  }
}
export function documentSession(sessionId:string,path:string,recoverMissingOnly=false) {
  const key=JSON.stringify([sessionId,path,recoverMissingOnly]);let session=sessions.get(key);
  if(!session){session=new DocumentSession(sessionId,path,recoverMissingOnly);sessions.set(key,session);}
  return session;
}
export function documentEditPrompt(context:DocumentEditContext,instruction:string) {
  return `${instruction}\n\n当前修改的文档：${context.title}\n文档路径：${context.path}\n发起修改时版本：${context.revision}\n${context.selectedText?`用户选中的内容：\n${context.selectedText}\n`:''}请使用 echoagent-office 的 office_document_read 读取最新可编辑稿，再使用 office_document_update 修订原文档。保留未要求修改的内容、表格和格式；使用读取到的 expectedRevision，遇到版本冲突先重新读取并尊重用户的新修改。不要用 shell 或重新生成另一份文件替代本次修订。`;
}

export function forgetDocumentSessions(sessionId:string) {
  for(const [key,session] of sessions)if(session.sessionId===sessionId){session.dispose();sessions.delete(key);}
  try {for(let index=localStorage.length-1;index>=0;index--){const key=localStorage.key(index);if(key?.startsWith(`echoagent.document-draft:${sessionId}:`))localStorage.removeItem(key);}}catch{/* Native task deletion already removed durable sources. */}
}
