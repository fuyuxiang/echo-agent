import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DocumentSession, type EditableDocument } from '../document-workspace';
import type { DocumentNode } from '../document-model';
const native=vi.hoisted(()=>({invoke:vi.fn()}));
vi.mock('@tauri-apps/api/core',()=>({invoke:native.invoke}));
const text=(value:string):DocumentNode=>({type:'doc',content:[{type:'paragraph',content:[{type:'text',text:value}]}]});
let server:EditableDocument;
beforeEach(()=>{
  vi.useFakeTimers();localStorage.clear();native.invoke.mockReset();
  server={id:'id',sessionId:'s',path:'/work/report.docx',title:'报告',revision:1,exportedRevision:1,fileSha256:'a'.repeat(64),byteSize:10,document:text('原稿'),history:[],savedAt:1,actor:'生成'};
  native.invoke.mockImplementation(async(command:string,args:Record<string,unknown>)=>{
    if(command==='document_workspace_open')return {document:structuredClone(server),path:server.path,fileSha256:server.fileSha256,byteSize:10};
    if(command==='document_workspace_save'){
      if(args.expectedRevision!==server.revision)throw new Error('DOCUMENT_CONFLICT:新修改已保存');
      server={...server,revision:server.revision+1,document:args.document as DocumentNode};return structuredClone(server);
    }
    if(command==='document_workspace_publish'){server={...server,exportedRevision:server.revision};return structuredClone(server);}
    throw new Error(`Unexpected native command ${command}`);
  });
});
afterEach(()=>{vi.clearAllTimers();vi.useRealTimers();});
async function open(){const session=new DocumentSession('s',server.path);await vi.waitFor(()=>expect(session.snapshot().record?.revision).toBe(1));return session;}
describe('document save and recovery workflow',()=>{
  it('serializes rapid edits and saves the newest draft after an earlier response arrives',async()=>{
    const session=await open();let release!:(value:EditableDocument)=>void;
    native.invoke.mockImplementationOnce(()=>new Promise(resolve=>{release=resolve;}));
    session.edit(text('第一处修改'));const saving=session.flush();
    session.edit(text('第一处修改和第二处修改'));
    server={...server,revision:2,document:text('第一处修改')};release(structuredClone(server));
    await saving;
    expect(server.revision).toBe(3);expect(server.document).toEqual(text('第一处修改和第二处修改'));
    expect(session.snapshot().phase).toBe('ready');expect(session.snapshot().draft).toEqual(server.document);
  });
  it('retains manual content when the Agent has already committed a newer revision',async()=>{
    const session=await open();session.edit(text('用户未保存的修改'));
    server={...server,revision:2,document:text('Agent的新版本')};
    await expect(session.flush()).rejects.toThrow('DOCUMENT_CONFLICT');
    expect(session.snapshot().phase).toBe('conflict');expect(session.snapshot().draft).toEqual(text('用户未保存的修改'));
    expect(server.document).toEqual(text('Agent的新版本'));
    expect(localStorage.getItem('echoagent.document-draft:s:/work/report.docx')).toContain('用户未保存的修改');
  });
  it('keeps failed saves recoverable and retries without losing the draft',async()=>{
    const session=await open();session.edit(text('磁盘保存重试'));
    native.invoke.mockRejectedValueOnce(new Error('暂时无法写入'));
    await expect(session.flush()).rejects.toThrow('暂时无法写入');
    expect(session.snapshot().draft).toEqual(text('磁盘保存重试'));
    await session.flush();expect(server.document).toEqual(text('磁盘保存重试'));
    expect(localStorage.getItem('echoagent.document-draft:s:/work/report.docx')).toBeNull();
  });
  it('auto-saves even after the panel unsubscribes and restores that draft when reopened',async()=>{
    const session=await open();const unsubscribe=session.subscribe(()=>{});
    session.edit(text('关闭面板前的修改'));unsubscribe();await vi.advanceTimersByTimeAsync(600);
    expect(server.document).toEqual(text('关闭面板前的修改'));
    const reopened=new DocumentSession('s',server.path);await vi.waitFor(()=>expect(reopened.snapshot().draft).toEqual(server.document));
  });
  it('publishes the current saved source and keeps the file revision visible',async()=>{
    const session=await open();session.edit(text('保存到Word的内容'));await session.publish();
    expect(server.document).toEqual(text('保存到Word的内容'));
    expect(session.snapshot().record?.exportedRevision).toBe(session.snapshot().record?.revision);
  });
  it('restores a crash recovery draft and does not replace a newer Agent version silently',async()=>{
    localStorage.setItem('echoagent.document-draft:s:/work/report.docx',JSON.stringify({revision:1,document:text('崩溃前内容')}));
    server.revision=2;const session=new DocumentSession('s',server.path);
    await vi.waitFor(()=>expect(session.snapshot().phase).toBe('conflict'));
    expect(session.snapshot().draft).toEqual(text('崩溃前内容'));expect(server.document).toEqual(text('原稿'));
  });
  it('deleting the owner cancels pending edits and removes crash recovery',async()=>{
    const session=await open();session.edit(text('待保存的修改'));session.dispose();
    await vi.advanceTimersByTimeAsync(2500);
    expect(server.document).toEqual(text('原稿'));
    expect(localStorage.getItem('echoagent.document-draft:s:/work/report.docx')).toBeNull();
    await expect(session.flush()).rejects.toThrow('任务已删除');
  });
  it('keeps input received while a historical restore is pending, including crash recovery',async()=>{
    const session=await open();const original=native.invoke.getMockImplementation()!;
    let release!:(record:EditableDocument)=>void;
    native.invoke.mockImplementation((command,args)=>command==='document_workspace_restore'
      ? new Promise(resolve=>{release=resolve;}) : original(command,args));
    const restoring=session.restore(1);
    await vi.waitFor(()=>expect(release).toBeTypeOf('function'));
    expect(session.snapshot().operation).toBe('restore');
    session.edit(text('恢复期间输入，不能丢失'));
    await vi.advanceTimersByTimeAsync(700);
    expect(native.invoke.mock.calls.some(([command])=>command==='document_workspace_save')).toBe(false);
    server={...server,revision:2,document:text('恢复的历史稿')};release(structuredClone(server));
    await expect(restoring).rejects.toThrow('新输入已保留');
    expect(session.snapshot()).toMatchObject({operation:null,phase:'conflict',draft:text('恢复期间输入，不能丢失')});
    expect(JSON.parse(localStorage.getItem('echoagent.document-draft:s:/work/report.docx')!)).toEqual({revision:1,document:text('恢复期间输入，不能丢失')});
    const reopened=new DocumentSession('s',server.path);
    await vi.waitFor(()=>expect(reopened.snapshot().phase).toBe('conflict'));
    expect(reopened.snapshot().draft).toEqual(text('恢复期间输入，不能丢失'));
  });
  it('ignores a background read that returns after a newer source has been saved',async()=>{
    const session=await open();const stale=structuredClone(server);let release!:(value:unknown)=>void;
    native.invoke.mockImplementationOnce(()=>new Promise(resolve=>{release=resolve;}));
    const refresh=session.refresh();
    session.edit(text('刚刚保存的新版本'));await session.flush();
    release({document:stale,path:stale.path,fileSha256:stale.fileSha256,byteSize:10});await refresh;
    expect(session.snapshot()).toMatchObject({phase:'ready',record:{revision:2},draft:text('刚刚保存的新版本')});
    expect(server.document).toEqual(text('刚刚保存的新版本'));
  });
  it('publishes a restored version as a new source and resumes editing afterwards',async()=>{
    const session=await open();const original=native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation(async(command,args)=>{
      if(command!=='document_workspace_restore')return original(command,args);
      server={...server,revision:2,document:text('历史稿')};return structuredClone(server);
    });
    await session.restore(1);
    expect(session.snapshot()).toMatchObject({operation:null,phase:'ready',draft:text('历史稿'),record:{revision:2,exportedRevision:2}});
    session.edit(text('恢复后继续编辑'));await session.flush();expect(server.document).toEqual(text('恢复后继续编辑'));
  });
  it('does not discard input arriving during an explicit reload',async()=>{
    const session=await open();session.edit(text('旧草稿'));
    let release!:(value:unknown)=>void;
    native.invoke.mockImplementationOnce(()=>new Promise(resolve=>{release=resolve;}));
    const reload=session.refresh(true);session.edit(text('读取期间的新输入'));
    release({document:{...server,revision:2,document:text('最新稿')},path:server.path,fileSha256:server.fileSha256,byteSize:10});
    await reload;
    expect(session.snapshot()).toMatchObject({operation:null,phase:'conflict',draft:text('读取期间的新输入')});
    expect(localStorage.getItem('echoagent.document-draft:s:/work/report.docx')).toContain('读取期间的新输入');
  });
  it('does not consume newer edits when a copy finishes',async()=>{
    const session=await open();session.edit(text('副本中的内容'));const original=native.invoke.getMockImplementation()!;
    let release!:(record:EditableDocument)=>void;
    native.invoke.mockImplementation((command,args)=>command==='document_workspace_import'
      ? new Promise(resolve=>{release=resolve;}) : original(command,args));
    const copying=session.copy(session.snapshot().draft!);
    await vi.waitFor(()=>expect(release).toBeTypeOf('function'));
    session.edit(text('副本请求之后的新输入'));
    release({...server,path:'/work/copy.docx',document:text('副本中的内容')});
    await expect(copying).rejects.toThrow('新输入仍保留');
    expect(session.snapshot().draft).toEqual(text('副本请求之后的新输入'));
    await vi.advanceTimersByTimeAsync(600);
    expect(server.document).toEqual(text('副本请求之后的新输入'));
  });
  it('keeps publication errors visible across refresh and retries transient failures',async()=>{
    const session=await open();session.edit(text('待同步内容'));await session.flush();
    native.invoke.mockRejectedValueOnce(new Error('磁盘暂不可写'));
    await expect(session.publish()).rejects.toThrow('磁盘暂不可写');
    await session.refresh();
    expect(session.snapshot()).toMatchObject({phase:'ready',syncError:'磁盘暂不可写',record:{revision:2,exportedRevision:1}});
    await vi.advanceTimersByTimeAsync(2100);
    expect(session.snapshot()).toMatchObject({syncError:'',record:{revision:2,exportedRevision:2}});
  });
  it('bounds automatic sync retries and permits an explicit retry after repair',async()=>{
    const session=await open();const original=native.invoke.getMockImplementation()!;let failing=true;
    native.invoke.mockImplementation((command,args)=>{
      if(command==='document_workspace_publish' && failing)return Promise.reject(new Error('磁盘空间不足'));
      return original(command,args);
    });
    session.edit(text('本机编辑稿'));await vi.advanceTimersByTimeAsync(20000);
    expect(native.invoke.mock.calls.filter(([command])=>command==='document_workspace_publish')).toHaveLength(3);
    await session.refresh();await vi.advanceTimersByTimeAsync(20000);
    expect(session.snapshot().syncError).toBe('磁盘空间不足');
    expect(native.invoke.mock.calls.filter(([command])=>command==='document_workspace_publish')).toHaveLength(3);
    failing=false;await session.publish();expect(session.snapshot().syncError).toBe('');expect(server.exportedRevision).toBe(server.revision);
  });
  it('resumes a durable pending Word revision when reopened',async()=>{
    server={...server,revision:2,exportedRevision:1,document:text('退出前已经保存的编辑稿')};
    const session=new DocumentSession('s',server.path);
    await vi.advanceTimersByTimeAsync(1500);
    expect(session.snapshot().record?.exportedRevision).toBe(2);
    expect(server.document).toEqual(text('退出前已经保存的编辑稿'));
  });
  it('keeps external changes protected without automatic overwrite or retry loops',async()=>{
    const session=await open();session.edit(text('我的编辑稿'));await session.flush();
    server={...server,externalChange:true};native.invoke.mockRejectedValueOnce(new Error('DOCUMENT_EXTERNAL_CHANGE:原文件已有外部修改'));
    await expect(session.publish()).rejects.toThrow('DOCUMENT_EXTERNAL_CHANGE');
    await session.refresh();await vi.advanceTimersByTimeAsync(10000);
    expect(session.snapshot()).toMatchObject({syncError:'原文件已有外部修改',record:{externalChange:true,exportedRevision:1}});
    expect(native.invoke.mock.calls.filter(([command])=>command==='document_workspace_publish')).toHaveLength(1);
  });
});
