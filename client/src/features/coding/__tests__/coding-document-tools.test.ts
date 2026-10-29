import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { waitFor } from "@testing-library/react";
import { confirmedRunPaths, executeDocumentTool, listenDocumentTools, releaseDocumentRun, type DocumentToolRequest } from "../lib/coding-document-tools";
import { registerWorkspaceEditor, type ApplyResult, type DocumentSnapshot, type EditOperation } from "../lib/workspace-editor";

const harness = vi.hoisted(() => ({
  transcripts: {} as Record<string, {streamingMessageId:string|null;messages:{id:string;promptId:string}[]}>,
  owners: [] as {sessionId:string;cwd:string}[],
  invoke: vi.fn(),
  listeners: new Map<string,(event:{payload:unknown}) => void>(),
  unlisten: vi.fn(),
  task: null as {id:string;sessionId:string} | null,
  preserveBuffer: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({invoke:(command:string,args:unknown) => harness.invoke(command,args)}));
vi.mock("@tauri-apps/api/event", () => ({listen:vi.fn(async (name:string,callback:(event:{payload:unknown})=>void) => {
  harness.listeners.set(name,callback); return harness.unlisten;
})}));
vi.mock("@/stores/session-store", () => ({useSessionStore:{getState:() => ({transcripts:harness.transcripts})}}));
vi.mock("@/stores/sessions-store", () => ({useSessionsStore:{getState:() => ({independent:harness.owners})}}));
vi.mock("../store/task-store", () => ({useTaskStore:{getState:() => ({task:harness.task})}}));
vi.mock("../lib/tauri-api", () => ({codingApi:{preserveBuffer:(...args:unknown[]) => harness.preserveBuffer(...args)}}));

let nextIdentity = 0;
let request: DocumentToolRequest;
const cleanup: (() => void)[] = [];
const makeSnapshot = (overrides:Partial<DocumentSnapshot> = {}): DocumentSnapshot => ({
  workspace:"/repo-a",uri:"file:///repo-a/a.ts",path:"/repo-a/a.ts",language:"typescript",exists:true,
  source:"buffer",version:3,hash:"a".repeat(64),dirty:true,selection:{start:{line:0,character:0},end:{line:0,character:3}},
  selectedText:"old",content:"old",contentOffset:0,totalCharacters:3,truncated:false,capturedAt:"2026-10-29T00:00:00.000Z",...overrides,
});
const applied = (operation:EditOperation, overrides:Partial<ApplyResult> = {}): ApplyResult => ({
  operationId:operation.operationId,sessionId:operation.sessionId,runId:operation.runId,status:"applied",
  files:[{uri:operation.files[0].uri,status:"applied",version:4,hash:"b".repeat(64),dirty:false,undoGroup:"native-undo"}],...overrides,
});
function editorFixture(snapshot = makeSnapshot()) {
  const editor = {getDirtyCount:vi.fn(async () => 1),saveAll:vi.fn(async () => true),lockEditing:vi.fn(),
    getDocument:vi.fn(async (_path?:string,_full?:boolean) => snapshot),
    applyEdits:vi.fn(async (operation:EditOperation) => applied(operation)),cancelRun:vi.fn(async () => true)};
  cleanup.push(registerWorkspaceEditor(request.workspace,editor));
  return editor;
}
const tool = (kind:string, input:Record<string,unknown> = {}) => ({...request,operationId:`${request.operationId}-${kind}`,toolKind:kind,
  toolName:kind === "read" ? "functions.read_file" : `functions.${kind}_file`,input:{target_file:"a.ts",...input}});

beforeEach(() => {
  nextIdentity++;
  harness.invoke.mockReset(); harness.invoke.mockResolvedValue(true); harness.unlisten.mockClear(); harness.listeners.clear();
  harness.transcripts = {}; harness.owners = [];
  harness.preserveBuffer.mockReset(); harness.preserveBuffer.mockResolvedValue({});
  request = {operationId:`operation-${nextIdentity}`,sessionId:`session-${nextIdentity}`,runId:`run-${nextIdentity}`,
    workspace:"/repo-a",toolName:"functions.read_file",toolKind:"read",phase:"before",input:{target_file:"a.ts"}};
  harness.owners.push({sessionId:request.sessionId,cwd:request.workspace});
  harness.task = {id:`task-${nextIdentity}`,sessionId:request.sessionId};
  harness.transcripts[request.sessionId] = {streamingMessageId:"streaming",messages:[{id:"streaming",promptId:request.runId}]};
});
afterEach(() => {
  cleanup.splice(0).forEach(remove => remove());
  releaseDocumentRun(request.sessionId,request.runId);
});

describe("runtime document tools share the versioned native editor", () => {
  it("reads an unsaved buffer with accurate lines without saving, locking or applying edits", async () => {
    const editor = editorFixture(makeSnapshot({content:"first\nunsaved function\nthird",totalCharacters:28}));
    const result = await executeDocumentTool(tool("read",{offset:2,limit:1}));
    expect(editor.getDocument).toHaveBeenCalledWith("/repo-a/a.ts",true);
    expect(result).toEqual({handled:true,text:expect.stringContaining("来源=buffer dirty=true version=3")});
    expect(result.text).toContain("2|unsaved function"); expect(result.text).not.toContain("1|first");
    expect(editor.saveAll).not.toHaveBeenCalled(); expect(editor.lockEditing).not.toHaveBeenCalled();
    expect(editor.getDirtyCount).not.toHaveBeenCalled(); expect(editor.applyEdits).not.toHaveBeenCalled();
    expect(harness.preserveBuffer).not.toHaveBeenCalled();
    expect(confirmedRunPaths(request.sessionId,request.runId)).toEqual([]);
  });

  it("binds write to the snapshot originally read and reports a newer native version as conflict", async () => {
    const editor = editorFixture();
    await executeDocumentTool(tool("read"));
    editor.getDocument.mockResolvedValue(makeSnapshot({content:"user typed",version:9,hash:"c".repeat(64)}));
    editor.applyEdits.mockImplementation(async operation => applied(operation,{status:"conflict",files:[{uri:operation.files[0].uri,status:"conflict",error:"newer version"}]}));
    await expect(executeDocumentTool(tool("write",{content:"agent text"}))).rejects.toThrow("conflict");
    expect(editor.getDocument).toHaveBeenCalledTimes(1);
    expect(editor.applyEdits).toHaveBeenCalledWith(expect.objectContaining({workspace:request.workspace,sessionId:request.sessionId,runId:request.runId,
      files:[expect.objectContaining({expectedVersion:3,expectedHash:"a".repeat(64),save:true})]}));
    expect(confirmedRunPaths(request.sessionId,request.runId)).toEqual([]);
  });

  it("does not permit a write to an existing file that was never read", async () => {
    const editor = editorFixture();
    await expect(executeDocumentTool(tool("write",{content:"blind overwrite"}))).rejects.toThrow("必须先读取");
    expect(editor.applyEdits).not.toHaveBeenCalled(); expect(editor.saveAll).not.toHaveBeenCalled();
  });

  it("creates a new file using an explicit missing-file snapshot and operation identities", async () => {
    const editor = editorFixture(makeSnapshot({exists:false,hash:"missing",content:"",version:null,source:"disk",dirty:false}));
    await executeDocumentTool(tool("write",{content:"created"}));
    expect(editor.applyEdits).toHaveBeenCalledWith({operationId:`${request.operationId}-write`,sessionId:request.sessionId,runId:request.runId,workspace:"/repo-a",
      files:[{uri:"file:///repo-a/a.ts",expectedVersion:null,expectedHash:"missing",save:true,
        edits:[{range:{start:{line:0,character:0},end:{line:0,character:0}},text:"created"}]}]});
    expect(confirmedRunPaths(request.sessionId,request.runId)).toEqual(["a.ts"]);
  });

  it("uses the native workspace spelling for Windows apply while preserving the session's native grant", async () => {
    request.workspace = String.raw`\\?\C:\项目\代码`;
    harness.owners[0].cwd = request.workspace;
    const nativeWorkspace = String.raw`C:\项目\代码`;
    const editor = editorFixture(makeSnapshot({workspace:nativeWorkspace,uri:"file:///c:/项目/代码/a.ts",path:String.raw`C:\项目\代码\a.ts`}));
    await executeDocumentTool(tool("read"));
    await executeDocumentTool(tool("edit",{old_string:"old",new_string:"new"}));
    expect(editor.applyEdits).toHaveBeenCalledWith(expect.objectContaining({workspace:nativeWorkspace,
      sessionId:request.sessionId,runId:request.runId}));
    expect(harness.preserveBuffer).toHaveBeenCalledWith(request.workspace,harness.task!.id,request.runId,"a.ts","old");
  });

  it("preserves the original unsaved buffer in the owned run checkpoint before applying an edit", async () => {
    const editor = editorFixture(); await executeDocumentTool(tool("read"));
    await executeDocumentTool(tool("edit",{old_string:"old",new_string:"agent"}));
    expect(harness.preserveBuffer).toHaveBeenCalledWith("/repo-a",harness.task!.id,request.runId,"a.ts","old");
    expect(harness.preserveBuffer.mock.invocationCallOrder[0]).toBeLessThan(editor.applyEdits.mock.invocationCallOrder[0]);
  });

  it("refuses to overwrite dirty content when its checkpoint cannot be bound to the same session", async () => {
    const editor = editorFixture(); await executeDocumentTool(tool("read"));
    harness.task = {id:"other-task",sessionId:"other-session"};
    await expect(executeDocumentTool(tool("edit",{old_string:"old",new_string:"agent"}))).rejects.toThrow("恢复点归属");
    expect(harness.preserveBuffer).not.toHaveBeenCalled(); expect(editor.applyEdits).not.toHaveBeenCalled();
  });

  it("stops before apply if preserving the user's unsaved checkpoint fails", async () => {
    const editor = editorFixture(); await executeDocumentTool(tool("read"));
    harness.preserveBuffer.mockRejectedValueOnce(new Error("checkpoint disk unavailable"));
    await expect(executeDocumentTool(tool("edit",{old_string:"old",new_string:"agent"}))).rejects.toThrow("checkpoint disk unavailable");
    expect(editor.applyEdits).not.toHaveBeenCalled();
  });

  it("does not claim a native edit was saved when the IDE returns saveError", async () => {
    const editor = editorFixture(); await executeDocumentTool(tool("read"));
    editor.applyEdits.mockImplementation(async operation => applied(operation,{files:[{uri:operation.files[0].uri,status:"applied",version:4,hash:"b".repeat(64),dirty:true,saveError:"disk changed"}]}));
    await expect(executeDocumentTool(tool("edit",{old_string:"old",new_string:"agent"}))).rejects.toThrow("disk changed");
    expect(confirmedRunPaths(request.sessionId,request.runId)).toEqual(["a.ts"]);
  });

  it("binds the Runtime receipt to native Agent text before asynchronous save formatting or later user typing", async () => {
    const editor = editorFixture(); await executeDocumentTool(tool("read"));
    editor.applyEdits.mockImplementation(async operation => applied(operation,{files:[{
      uri:operation.files[0].uri,status:"applied",version:5,hash:"c".repeat(64),checkpointHash:"b".repeat(64),dirty:false,
    }]}));
    const result = await executeDocumentTool(tool("edit",{old_string:"old",new_string:"agent"}));
    expect(result.appliedDocuments).toEqual([{path:"a.ts",beforeHash:"a".repeat(64),afterHash:"b".repeat(64)}]);
    expect((result.appliedDocuments as Array<{afterHash:string}>)[0].afterHash).not.toBe("c".repeat(64));
  });

  it("requires another real read after save formatting instead of mixing new hash with the proposed text", async () => {
    const editor = editorFixture(); await executeDocumentTool(tool("read"));
    await executeDocumentTool(tool("edit",{old_string:"old",new_string:"unformatted"}));
    editor.getDocument.mockResolvedValue(makeSnapshot({content:"formatted actual buffer",hash:"c".repeat(64),version:7,dirty:false}));
    await expect(executeDocumentTool({...tool("edit",{old_string:"unformatted",new_string:"bad"}),operationId:"unread-next-edit"})).rejects.toThrow("必须先读取");
    expect(editor.applyEdits).toHaveBeenCalledTimes(1);
    await executeDocumentTool({...tool("read"),operationId:"read-formatted"});
    await executeDocumentTool({...tool("edit",{old_string:"formatted",new_string:"updated"}),operationId:"edit-formatted"});
    expect(editor.applyEdits).toHaveBeenLastCalledWith(expect.objectContaining({files:[expect.objectContaining({expectedHash:"c".repeat(64),expectedVersion:7,
      edits:[expect.objectContaining({text:"updated actual buffer"})]})]}));
  });

  it("fences a stopped run after an awaited read and before any edit", async () => {
    const editor = editorFixture(); let finish!: (snapshot:DocumentSnapshot) => void;
    editor.getDocument.mockImplementation(() => new Promise(resolve => {finish = resolve;}));
    const pending = executeDocumentTool(tool("read"));
    const rejected = expect(pending).rejects.toThrow("已停止");
    harness.transcripts[request.sessionId].streamingMessageId = null; finish(makeSnapshot()); await rejected;
    expect(editor.applyEdits).not.toHaveBeenCalled(); expect(editor.saveAll).not.toHaveBeenCalled();
  });

  it("rechecks the runtime operation's active ACK before applying a cached snapshot", async () => {
    const editor = editorFixture(); await executeDocumentTool(tool("read"));
    harness.invoke.mockResolvedValueOnce(false);
    await expect(executeDocumentTool(tool("edit",{old_string:"old",new_string:"agent"}))).rejects.toThrow("操作已取消");
    expect(harness.invoke).toHaveBeenCalledWith("coding_document_tool_is_active",{operationId:`${request.operationId}-edit`});
    expect(editor.applyEdits).not.toHaveBeenCalled();
  });

  it("rejects a replaced runtime run even if the same session and editor remain open", async () => {
    const editor = editorFixture();
    harness.transcripts[request.sessionId].messages[0].promptId = "replacement-run";
    await expect(executeDocumentTool(tool("read"))).rejects.toThrow("旧工具操作");
    expect(editor.getDocument).not.toHaveBeenCalled();
  });

  it("rejects another workspace and traversal before touching either project's editor", async () => {
    const editor = editorFixture();
    const other = {getDirtyCount:vi.fn(async () => 0),saveAll:vi.fn(async () => true),getDocument:vi.fn()};
    cleanup.push(registerWorkspaceEditor("/repo-b",other));
    await expect(executeDocumentTool({...tool("read"),workspace:"/repo-b"})).rejects.toThrow("会话归属不匹配");
    await expect(executeDocumentTool(tool("read",{target_file:"../outside.ts"}))).rejects.toThrow("当前工程内");
    expect(editor.getDocument).not.toHaveBeenCalled(); expect(other.getDocument).not.toHaveBeenCalled();
  });

  it("blocks a command with dirty buffers while allowing read-only search without implicit saves", async () => {
    const editor = editorFixture();
    await expect(executeDocumentTool(tool("shell",{command:"npm test"}))).rejects.toThrow("未保存");
    await expect(executeDocumentTool(tool("search",{query:"symbol"}))).resolves.toEqual({handled:false});
    expect(editor.saveAll).not.toHaveBeenCalled(); expect(editor.applyEdits).not.toHaveBeenCalled();
  });

  it("coalesces duplicate in-flight/completed bridge events into one actual native apply", async () => {
    const editor = editorFixture(); await executeDocumentTool(tool("read"));
    let finish!: (result:ApplyResult) => void;
    editor.applyEdits.mockImplementation(() => new Promise(resolve => {finish = resolve;}));
    const unlisten = await listenDocumentTools();
    const callback = harness.listeners.get("agent://document-tool")!;
    const editRequest = tool("edit",{old_string:"old",new_string:"agent"});
    callback({payload:editRequest}); callback({payload:editRequest});
    await waitFor(() => expect(editor.applyEdits).toHaveBeenCalledTimes(1));
    const operation = editor.applyEdits.mock.calls[0][0]; finish(applied(operation));
    await waitFor(() => expect(harness.invoke.mock.calls.filter(([command]) => command === "coding_document_tool_respond")).toHaveLength(2));
    callback({payload:editRequest});
    await waitFor(() => expect(harness.invoke.mock.calls.filter(([command]) => command === "coding_document_tool_respond")).toHaveLength(3));
    expect(editor.applyEdits).toHaveBeenCalledTimes(1);
    expect(confirmedRunPaths(request.sessionId,request.runId)).toEqual(["a.ts"]);
    releaseDocumentRun(request.sessionId,request.runId); expect(confirmedRunPaths(request.sessionId,request.runId)).toEqual([]);
    unlisten(); expect(harness.unlisten).toHaveBeenCalled();
  });

  it("reads a saved, unmounted workspace through the pending native protocol without consulting another project", async () => {
    const other = {getDirtyCount:vi.fn(async () => 1),saveAll:vi.fn(async () => true),getDocument:vi.fn()};
    cleanup.push(registerWorkspaceEditor("/repo-b",other));
    const disk = makeSnapshot({source:"disk",version:null,dirty:false});
    harness.invoke.mockImplementation(async command => command === "coding_closed_document_snapshot" ? disk
      : command === "coding_document_editor_present" ? false : true);
    await expect(executeDocumentTool(tool("read"))).resolves.toEqual({handled:true,text:expect.stringContaining("来源=disk dirty=false version=null")});
    expect(harness.invoke).toHaveBeenCalledWith("coding_closed_document_snapshot",{workspace:"/repo-a",sessionId:request.sessionId,runId:request.runId,
      operationId:`${request.operationId}-read`,path:"/repo-a/a.ts"});
    expect(other.getDocument).not.toHaveBeenCalled(); expect(other.saveAll).not.toHaveBeenCalled();
    await expect(executeDocumentTool(tool("shell",{command:"npm test"}))).resolves.toEqual({handled:false});
    expect(other.getDirtyCount).not.toHaveBeenCalled();
  });

  it("refuses unattended commands when native still owns an IDE lease that the renderer registry lost", async () => {
    harness.invoke.mockResolvedValue(true);
    await expect(executeDocumentTool(tool("shell",{command:"npm test"}))).rejects.toThrow("编辑器已挂载但协调连接不可用");
    expect(harness.invoke).toHaveBeenCalledWith("coding_document_editor_present",{root:request.workspace});
    expect(harness.invoke.mock.calls.some(([command]) => command === "coding_closed_document_apply")).toBe(false);
  });

  it("keeps the original expected hash and checkpoint when a buffered read is followed by leaving the project", async () => {
    const editor = editorFixture(); await executeDocumentTool(tool("read"));
    cleanup[cleanup.length-1](); // The leave guard has saved this buffer and released its actual IDE owner.
    harness.invoke.mockImplementation(async (command,args:{operation?:EditOperation}) => command === "coding_closed_document_apply" ? applied(args.operation!) : true);
    await executeDocumentTool(tool("edit",{old_string:"old",new_string:"agent"}));
    expect(editor.applyEdits).not.toHaveBeenCalled();
    expect(harness.invoke).toHaveBeenCalledWith("coding_closed_document_apply",{operation:expect.objectContaining({sessionId:request.sessionId,runId:request.runId,
      files:[expect.objectContaining({expectedHash:"a".repeat(64),expectedVersion:3})]})});
    expect(harness.preserveBuffer).toHaveBeenCalledWith("/repo-a",harness.task!.id,request.runId,"a.ts","old");
  });

  it("does not let a late disk snapshot revive an unmounted run after stop", async () => {
    let finish!: (snapshot:DocumentSnapshot) => void;
    harness.invoke.mockImplementation(command => command === "coding_closed_document_snapshot"
      ? new Promise(resolve => {finish = resolve;}) : Promise.resolve(true));
    const pending = executeDocumentTool(tool("read"));
    const rejected = expect(pending).rejects.toThrow("已停止");
    harness.transcripts[request.sessionId].streamingMessageId = null;finish(makeSnapshot({source:"disk",version:null,dirty:false}));await rejected;
    expect(harness.invoke.mock.calls.some(([command]) => command === "coding_closed_document_apply")).toBe(false);
  });
});

describe("document protocol regression coverage",()=>{
  it("returns content receipts even when native save fails",async()=>{
    const editor=editorFixture(makeSnapshot({dirty:false}));
    await executeDocumentTool(tool("read"));
    editor.applyEdits.mockImplementation(async operation=>applied(operation,{files:[{uri:operation.files[0].uri,status:"applied",hash:"b".repeat(64),dirty:true,saveError:"external conflict"}]}));
    const stop=await listenDocumentTools();
    harness.listeners.get("agent://document-tool")!({payload:tool("edit",{old_string:"old",new_string:"new"})});
    await waitFor(()=>expect(harness.invoke).toHaveBeenCalledWith("coding_document_tool_respond",expect.objectContaining({result:expect.objectContaining({error:expect.stringContaining("external conflict"),appliedDocuments:[{path:"a.ts",beforeHash:"a".repeat(64),afterHash:"b".repeat(64)}]})})));
    stop();
  });
  it("preserves the Runtime reader for large clean files and PDF/image inputs",async()=>{
    const editor=editorFixture(makeSnapshot({dirty:false,truncated:true,totalCharacters:3000000}));
    await expect(executeDocumentTool(tool("read",{offset:120000,limit:1}))).resolves.toEqual({handled:false});
    editor.getDocument.mockClear();
    for(const target_file of ["design.pdf","screenshot.png","requirements.docx"])
      await expect(executeDocumentTool(tool("read",{target_file}))).resolves.toEqual({handled:false});
    expect(editor.getDocument).not.toHaveBeenCalled();
  });
  it("reads a bounded range of a large dirty buffer without switching to disk",async()=>{
    const editor=editorFixture(makeSnapshot({truncated:true,totalCharacters:3000000}));
    editor.getDocument.mockResolvedValueOnce(makeSnapshot({truncated:true,totalCharacters:3000000}))
      .mockResolvedValueOnce(makeSnapshot({content:"unsaved late line",truncated:true,firstLine:120000}));
    const result=await executeDocumentTool(tool("read",{offset:120000,limit:1}));
    expect(result.text).toContain("120000|unsaved late line");
    expect(editor.getDocument).toHaveBeenLastCalledWith("/repo-a/a.ts",false,{offset:120000,limit:1});
    expect(editor.saveAll).not.toHaveBeenCalled();
  });
  it("applies the native patch preview to an unsaved versioned snapshot",async()=>{
    const editor=editorFixture();
    const patch="*** Begin Patch\n*** Update File: a.ts\n@@\n-old\n+new\n*** End Patch";
    harness.invoke.mockImplementation(async(command,args)=>command==="coding_document_patch_preview"
      ? [{path:"a.ts",content:(args as {originals:unknown}).originals?"new":null,delete:false,moveTo:null}] : true);
    await executeDocumentTool({...request,toolKind:"edit",toolName:"apply_patch",input:{patch}});
    expect(harness.preserveBuffer).toHaveBeenCalled();
    expect(editor.applyEdits).toHaveBeenCalledWith(expect.objectContaining({files:[expect.objectContaining({expectedVersion:3,expectedHash:"a".repeat(64),edits:[expect.objectContaining({text:"new"})]})]}));
  });
});
