import type { CodingTurnOptions } from "@/lib/agent-client";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn(async (command: string, _args?: unknown): Promise<unknown> => {
  if (command === "coding_task_list") return [];
  if (command === "coding_verification_detect") return [];
  return null;
});
const eventListeners = new Map<string, (event: { payload: unknown }) => void>();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args: unknown) => invoke(command, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, callback: (event: { payload: unknown }) => void) => {
    eventListeners.set(name, callback);
    return () => {
      if (eventListeners.get(name) === callback) eventListeners.delete(name);
    };
  }),
}));
vi.mock("@/components/workspace-panel/FileTreeView", () => ({
  FileTreeView: ({
    rootPath,
    onFileSelect,
  }: {
    rootPath: string;
    onFileSelect: (path: string) => void;
  }) => (
    <div data-testid="file-tree">
      <button type="button" onClick={() => onFileSelect(`${rootPath}/src/a.ts`)}>
        打开源文件
      </button>
    </div>
  ),
}));
const readDocument = vi.fn(async (_root: string, path: string) => ({
  path,
  relativePath: path.replace("/repo/", ""),
  content: "disk content",
  hash: "h1",
  size: 12,
  modifiedAt: 0,
  language: "typescript",
  lineEnding: "LF" as const,
}));
const writeDocument = vi.fn(async (_root: string, path: string, content: string, _hash?: string) => ({
  path,
  relativePath: path.replace("/repo/", ""),
  content,
  hash: "h2",
  size: content.length,
  modifiedAt: 0,
  language: "typescript",
  lineEnding: "LF" as const,
}));

vi.mock("@/lib/agent-client", () => ({
  filesystemPickDirectory: vi.fn(async () => "/picked"),
  listDir: vi.fn(async () => []),
  codingReadDocument: (root: string, path: string) => readDocument(root, path),
  codingWriteDocument: (root: string, path: string, content: string, hash: string) =>
    writeDocument(root, path, content, hash),
}));

// These shared components reach for session/permission plumbing that is out of
// scope here; the workbench only needs to render them.
vi.mock("@/components/PermissionPicker", () => ({
  PermissionPicker: () => <div data-testid="permission-picker" />,
}));
vi.mock("@/components/ModelSelector", () => ({
  ModelSelector: () => <div data-testid="model-selector" />,
}));
vi.mock("@/components/PermissionDialog", () => ({
  PermissionInlineCard: () => <div data-testid="permission-card" />,
}));
vi.mock("@/components/QuestionInlineCard", () => ({
  QuestionInlineCard: () => <div data-testid="question-card" />,
}));
vi.mock("@/components/ExecutionProcess", () => ({
  ExecutionProcess: () => <div data-testid="execution" />,
}));
vi.mock("@/components/Markdown", () => ({
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));

import { TaskEvidenceStrip } from "@/features/coding/agent/TaskEvidenceStrip";
import { CodingWorkbench } from "@/features/coding/CodingWorkbench";
import type { ChangeKind, ChangeSet, CodingTask } from "@/features/coding/lib/types";
import { useAiDraftStore } from "@/features/coding/store/ai-draft-store";
import { useTaskStore } from "@/features/coding/store/task-store";
import { codingRuntimeKey, useCodingRuntimeStore } from "@/features/coding/lib/coding-task-runtime";
import { registerWorkspaceEditor } from "@/features/coding/lib/workspace-editor";
import type { ApplyResult, DocumentSnapshot, EditOperation } from "@/features/coding/lib/workspace-editor";

function verificationTask(overrides: Partial<CodingTask> = {}): CodingTask {
  return {
    schemaVersion: 2,
    id: "verification-task",
    name: "验证布局",
    requirement: "验证工作台布局",
    phase: "verifying",
    acceptanceCriteria: [],
    taskNodes: [],
    planIssues: [],
    globalConstraints: [],
    createdAt: "2026-09-13T00:00:00Z",
    updatedAt: "2026-09-13T00:00:01Z",
    ...overrides,
  };
}



function readyEditor(frame: HTMLIFrameElement, initialDirty = 0, saveFailure = false, onSave?: () => Promise<void>, context: Record<string, unknown> = {},
  editing?: {document:DocumentSnapshot;apply?:(operation:EditOperation)=>ApplyResult}) {
  let dirty = initialDirty;
  const origin = new URL(frame.src).origin;
  const token = JSON.parse(frame.name.slice("echo-embed:".length)).bridgeToken;
  const instanceId = "test-native-editor";
  const send = (data: object) => act(() => { window.dispatchEvent(new MessageEvent("message", {origin, source: frame.contentWindow, data: {...data, token, instanceId}})); });
  const post = vi.spyOn(frame.contentWindow!, "postMessage").mockImplementation((data: unknown) => {
    const message = data as {type: string; id: string;operation:EditOperation};
    if (editing && ["echo/open-diff","echo/get-document","echo/apply-edits"].includes(message.type)) {
      queueMicrotask(()=>send({type:"echo/response",id:message.id,ok:true,value:message.type === "echo/get-document" ? editing.document
        : message.type === "echo/apply-edits" ? editing.apply?.(message.operation) ?? {operationId:message.operation.operationId,sessionId:message.operation.sessionId,
          runId:message.operation.runId,status:"applied",files:[{uri:message.operation.files[0].uri,status:"applied",version:8,hash:"b".repeat(64),dirty:false,undoGroup:"native-file-discard"}]}
        : undefined}));
      return;
    }
    if (message.type === "echo/get-context") {
      queueMicrotask(() => send({type:"echo/response",id:message.id,ok:true,value:{workspace:"/repo",document:{workspace:"/repo",uri:"file:///repo/a.ts",path:"/repo/a.ts",language:"typescript",version:7,hash:"dirty-buffer-hash",dirty:true,source:"buffer",exists:true,selection:{start:{line:1,character:0},end:{line:1,character:13}},selectedText:"return value;",content:"function a() {\nreturn value;\n}",contentOffset:0,totalCharacters:30,truncated:false,capturedAt:"now"},openDocuments:[],diagnostics:[],diagnosticsTruncated:false,terminal:null,truncated:false,...context}}));
      return;
    }
    if (!["echo/get-dirty", "echo/save-all", "echo/reload-clean"].includes(message.type)) return;
    queueMicrotask(async () => {
      if (message.type === "echo/save-all") await onSave?.();
      if (message.type === "echo/save-all" && saveFailure) send({type: "echo/response", id: message.id, ok: false, error: "disk full"});
      else { if (message.type === "echo/save-all") dirty = 0; send({type: "echo/response", id: message.id, ok: true, value: dirty}); }
    });
  });
  send({type: "echo/ready",instanceId,protocolVersion:1,capabilities:["document-snapshots","versioned-edits","inline-completion","native-diff"]}); send({type: "echo/dirty-state", count: dirty});
  return post;
}
function installApi(task: CodingTask) {
  invoke.mockImplementation(async (command: string, args?: unknown): Promise<unknown> => {
    if (command === "coding_document_editor_attach") return (args as {root:string} | undefined)?.root;
    if (command === "coding_verification_config_get") return {checks:[]};
    if (["coding_checkpoint_list","coding_git_commit_history","coding_review_list"].includes(command)) return [];
    if (["coding_task_list", "coding_verification_list", "coding_diagnostics_list", "coding_task_execution_ledger"].includes(command)) return [];
    if (command === "coding_verification_detect") return [{kind: "test", command: "pnpm test", label: "Tests"}];
    if (command === "coding_theia_start") return {url: "http://127.0.0.1:41773/", embedToken: "test-token"};
    if (command === "coding_task_create" || command === "coding_task_get") return task;
    if (command === "coding_task_begin_verification") return {...task, phase: "verifying"};
    if (command === "coding_orchestrator_state") return {task, problems: [], repairRounds: [], changedFileCount: 1, maxRepairRounds: 3};
    if (command === "coding_verification_run") return {id: "check", taskId: task.id, status: "passed"};
    return null;
  });
}
const discardSnapshot = (overrides:Partial<DocumentSnapshot> = {}):DocumentSnapshot => ({workspace:"/repo",uri:"file:///repo/src/a.ts",path:"/repo/src/a.ts",
  language:"typescript",version:7,hash:"a".repeat(64),dirty:false,source:"buffer",exists:true,selection:null,selectedText:"",
  content:"changed 😀\r\nlast",contentOffset:0,totalCharacters:16,truncated:false,capturedAt:"captured",...overrides});
function installDiscardApi(canApplyText=true,kind:ChangeKind="modified",binary=false,failFileOperation=false) {
  const task=verificationTask({phase:"implementing",sessionId:"bound"});installApi(task);
  const base=invoke.getMockImplementation()!;
  const changes:ChangeSet={taskId:task.id,changes:[{path:"src/a.ts",kind,added:1,removed:1,preExisting:false}],createdAt:"now",reviewedFiles:[]};
  const diff={original:"baseline\r\nrestored",modified:discardSnapshot().content,modifiedHash:`100755:${"a".repeat(64)}`,binary,canApplyText};
  invoke.mockImplementation(async(command,args)=>{
    if(command === "coding_changeset_get") return changes;
    if(command === "coding_changeset_diff") return diff;
    if(command === "coding_delivery_commit_hunks") return [{id:"hunk-one",preview:"@@ -1 +1 @@\n-baseline\n+changed"}];
    if(command === "coding_changeset_revert_hunk_preview") return {path:"src/a.ts",original:diff.modified,modified:"hunk restored\r\nlast"};
    if(command === "coding_changeset_discard_file" && failFileOperation) throw new Error("real file restoration failed");
    if(command === "coding_changeset_discard_file" || command === "coding_changeset_sync") return {...changes,changes:[]};
    return base(command,args);
  });
  useTaskStore.setState({root:"/repo",task,changeSet:changes});return {task,diff};
}
async function requestFileDiscard() {
  await userEvent.click(screen.getByRole("button",{name:/^变更/}));
  await userEvent.click(await screen.findByRole("button",{name:"丢弃 src/a.ts 的改动"}));
  await userEvent.click(await screen.findByRole("button",{name:"丢弃改动"}));
}
async function requestHunkDiscard() {
  await userEvent.click(screen.getByRole("button",{name:/^变更/}));
  await userEvent.click(await screen.findByRole("button",{name:"选择 src/a.ts 的任务差异块"}));
  await userEvent.click(await screen.findByRole("button",{name:"丢弃 src/a.ts 的差异块 1"}));
}
describe("coding remediation: editor ownership and delivery evidence", () => {
  beforeEach(() => {
    useCodingRuntimeStore.setState({documentCancellationReady:true,documentCancellations:{},stopping:{}});
    invoke.mockReset(); eventListeners.clear(); localStorage.clear();
    useTaskStore.setState({root: "", task: null, summaries: [], changeSet: null, verifications: [], problems: [], ledger: [], orchestrator: null});
    useAiDraftStore.getState().clear();
  });
  it("keeps the selected dirty buffer in Ask after focus leaves Theia for the composer, without saving", async () => {
    installApi(verificationTask({phase:"idle"}));
    const onStartRun=vi.fn(async (_root,_request,_model,_paths,bind,_prompt?:string,_coding?:CodingTurnOptions)=>{await bind("bound");return "bound";});
    render(<CodingWorkbench cwd="/repo" apiReady models={[{id:"test-model"}]} defaultModelId="test-model" onStartRun={onStartRun}/>);
    const frame=await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const post=readyEditor(frame,1,true);
    const token=JSON.parse(frame.name.slice("echo-embed:".length)).bridgeToken;
    act(()=>window.dispatchEvent(new MessageEvent("message",{origin:new URL(frame.src).origin,source:frame.contentWindow,data:{type:"echo/active-file",token,instanceId:"test-native-editor",path:"/repo/a.ts"}})));
    act(()=>window.dispatchEvent(new MessageEvent("message",{origin:new URL(frame.src).origin,source:frame.contentWindow,data:{type:"echo/active-file",token,instanceId:"test-native-editor",path:null}})));
    fireEvent.change(screen.getByRole("combobox",{name:"开发意图"}),{target:{value:"ask"}});
    fireEvent.change(screen.getByRole("textbox",{name:"任务描述"}),{target:{value:"解释选中的函数"}});
    await userEvent.click(screen.getByRole("button",{name:"开始 Agent 任务"}));
    await waitFor(()=>expect(onStartRun).toHaveBeenCalledOnce());
    expect(onStartRun.mock.calls[0][5]).toContain("return value;");
    expect(onStartRun.mock.calls[0][5]).toContain('"version":7');
    expect(onStartRun.mock.calls[0][5]).toContain('"dirty":true');
    expect(onStartRun.mock.calls[0][5]).toContain('"selectedText":"return value;"');
    expect(post.mock.calls.some(([data])=>(data as {type:string}).type==="echo/get-context")).toBe(true);
    expect(onStartRun.mock.calls[0][6]).toMatchObject({mode:"ask",documentToolBridge:true});
    expect(post.mock.calls.some(([data])=>(data as {type:string}).type==="echo/save-all")).toBe(false);
    expect(invoke).not.toHaveBeenCalledWith("coding_write_document",expect.anything());
  });

  it("shows an unconfirmed editor cancellation after Runtime stops, blocks new work across sessions, and retains read navigation", async () => {
    const task=verificationTask({phase:"stopped",sessionId:"bound"});installApi(task);
    useTaskStore.setState({root:"/repo",task});
    const pending={workspace:"/repo",sessionId:"older-session",runId:"awaiting-editor",pending:true};
    useCodingRuntimeStore.getState().setDocumentCancellation(pending);
    const onSendMessage=vi.fn();
    render(<CodingWorkbench cwd="/repo" sessionId="bound" onSendMessage={onSendMessage} />);
    readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement);
    expect(screen.getAllByText(/Runtime 已停，编辑器或外部工具尚未确认结束，仍在停止或状态未知/).length).toBeGreaterThan(0);
    expect(screen.queryByText("任务已停止")).not.toBeInTheDocument();
    expect(screen.queryByRole("button",{name:"继续执行"})).not.toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox",{name:"补充开发要求"}),{target:{value:"等待确认后继续"}});
    expect(screen.getByRole("button",{name:"发送给 Agent"})).toBeDisabled();
    await userEvent.click(screen.getByRole("button",{name:"检查"}));
    expect(await screen.findByRole("button",{name:"运行全部验证"})).toBeDisabled();
    expect(screen.getByRole("button",{name:"返回编辑器"})).not.toBeDisabled();
    await userEvent.click(screen.getByRole("button",{name:"返回编辑器"}));
    act(()=>useCodingRuntimeStore.getState().setStopping("/repo",task.id,false));
    expect(screen.getByRole("button",{name:"发送给 Agent"})).toBeDisabled();
    expect(invoke).not.toHaveBeenCalledWith("coding_task_resume",expect.anything());
    expect(invoke).not.toHaveBeenCalledWith("coding_verification_run",expect.anything());
    expect(onSendMessage).not.toHaveBeenCalled();
    act(()=>useCodingRuntimeStore.getState().setDocumentCancellation({...pending,pending:false}));
    expect(screen.getByText("任务已停止")).toBeInTheDocument();
    expect(screen.getByRole("button",{name:"继续执行"})).not.toBeDisabled();
    expect(screen.getByRole("button",{name:"发送给 Agent"})).not.toBeDisabled();
  });

  it("loads a closed ordinary file into the native Diff model and discards its text through versioned native undo", async () => {
    const {diff}=installDiscardApi();const onToast=vi.fn();
    render(<CodingWorkbench cwd="/repo" onToast={onToast}/>);
    const post=readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement,0,false,undefined,{}, {document:discardSnapshot()});
    await requestFileDiscard();
    await waitFor(()=>expect(onToast).toHaveBeenCalledWith("已恢复 src/a.ts 的文本，可在编辑器中撤销"));
    const messages=post.mock.calls.map(([message])=>message as {type:string;path:string;original:string;operation:EditOperation});
    const opening=messages.find(message=>message.type==="echo/open-diff")!;
    expect(opening).toMatchObject({path:"/repo/src/a.ts",original:diff.original});
    const apply=messages.find(message=>message.type==="echo/apply-edits")!;
    expect(messages.indexOf(opening)).toBeLessThan(messages.findIndex(message=>message.type==="echo/get-document"));
    expect(apply.operation).toMatchObject({workspace:"/repo",sessionId:"bound",runId:expect.stringMatching(/^manual:/),files:[{
      uri:"file:///repo/src/a.ts",expectedVersion:7,expectedHash:"a".repeat(64),save:true,
      edits:[{range:{start:{line:0,character:0},end:{line:1,character:4}},text:diff.original}],
    }]});
    expect(invoke).not.toHaveBeenCalledWith("coding_changeset_discard_file",expect.anything());
    expect(messages.some(message=>message.type==="echo/reload-clean")).toBe(false);
  });

  it.each(["newer text","version conflict","save failure"])("does not fall back to disk or report successful discard after %s", async scenario => {
    installDiscardApi();const onToast=vi.fn();
    const snapshot=discardSnapshot(scenario === "newer text" ? {content:"new USER input",version:9,dirty:true} : {});
    render(<CodingWorkbench cwd="/repo" onToast={onToast}/>);
    const post=readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement,0,false,undefined,{}, {document:snapshot,apply:operation=>({
      operationId:operation.operationId,sessionId:operation.sessionId,runId:operation.runId,status:scenario === "version conflict" ? "conflict" : "applied",
      files:[{uri:operation.files[0].uri,status:scenario === "version conflict" ? "conflict" : "applied",saveError:scenario === "save failure" ? "disk changed during save" : undefined}],
    })});
    await requestFileDiscard();
    await waitFor(()=>expect(onToast).toHaveBeenCalledWith(expect.stringMatching(/^丢弃失败/)));
    expect(invoke).not.toHaveBeenCalledWith("coding_changeset_discard_file",expect.anything());
    expect(onToast.mock.calls.some(([message])=>message.includes("已恢复"))).toBe(false);
    if(scenario === "newer text") expect(post.mock.calls.some(([message])=>(message as {type:string}).type==="echo/apply-edits")).toBe(false);
  });

  it.each(["added","deleted","metadata","binary"])("preserves the real file operation for %s instead of replacing it as ordinary text", async scenario => {
    installDiscardApi(false,scenario === "added" || scenario === "deleted" ? scenario : "modified",scenario === "binary");const onToast=vi.fn();
    render(<CodingWorkbench cwd="/repo" onToast={onToast}/>);
    const post=readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement);
    await requestFileDiscard();
    await waitFor(()=>expect(invoke).toHaveBeenCalledWith("coding_changeset_discard_file",{root:"/repo",taskId:"verification-task",path:"src/a.ts"}));
    await waitFor(()=>expect(onToast).toHaveBeenCalledWith(expect.stringContaining("未加入文本撤销栈")));
    expect(post.mock.calls.some(([message])=>(message as {type:string}).type==="echo/apply-edits")).toBe(false);
  });

  it("reports a real metadata restoration failure without claiming that a file was discarded", async () => {
    installDiscardApi(false,"modified",false,true);const onToast=vi.fn();
    render(<CodingWorkbench cwd="/repo" onToast={onToast}/>);
    readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement);
    await requestFileDiscard();
    await waitFor(()=>expect(onToast).toHaveBeenCalledWith(expect.stringContaining("real file restoration failed")));
    expect(onToast.mock.calls.some(([message])=>message.startsWith("已通过文件操作恢复"))).toBe(false);
  });

  it.each(["modified","added"] as const)("loads an unopened %s file into the native Diff model before restoring a hunk with native undo", async kind => {
    installDiscardApi(kind === "modified",kind);const onToast=vi.fn();
    render(<CodingWorkbench cwd="/repo" onToast={onToast}/>);
    const post=readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement,0,false,undefined,{}, {document:discardSnapshot()});
    await requestHunkDiscard();
    await waitFor(()=>expect(onToast).toHaveBeenCalledWith("已撤销所选差异块，可在编辑器中撤销此操作"));
    const messages=post.mock.calls.map(([message])=>message as {type:string;path:string;original:string;operation:EditOperation});
    const opening=messages.find(message=>message.type === "echo/open-diff")!;
    expect(opening).toMatchObject({path:"/repo/src/a.ts",original:"hunk restored\r\nlast"});
    expect(messages.indexOf(opening)).toBeLessThan(messages.findIndex(message=>message.type === "echo/get-document"));
    expect(messages.find(message=>message.type === "echo/apply-edits")!.operation.files).toEqual([{
      uri:"file:///repo/src/a.ts",expectedVersion:7,expectedHash:"a".repeat(64),save:true,
      edits:[{range:{start:{line:0,character:0},end:{line:1,character:4}},text:"hunk restored\r\nlast"}],
    }]);
    expect(invoke).not.toHaveBeenCalledWith("coding_changeset_discard_file",expect.anything());
  });

  it.each(["disk only","deleted after preview","save failure"])("does not claim native hunk undo after %s", async scenario => {
    installDiscardApi();const onToast=vi.fn();
    const snapshot=discardSnapshot(scenario === "disk only" ? {source:"disk",version:null} : scenario === "deleted after preview" ? {exists:false,hash:"missing",source:"disk",version:null,content:""} : {});
    render(<CodingWorkbench cwd="/repo" onToast={onToast}/>);
    const post=readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement,0,false,undefined,{}, {document:snapshot,apply:operation=>({
      operationId:operation.operationId,sessionId:operation.sessionId,runId:operation.runId,status:"applied",
      files:[{uri:operation.files[0].uri,status:"applied",saveError:"external disk change"}],
    })});
    await requestHunkDiscard();
    await waitFor(()=>expect(onToast).toHaveBeenCalledWith(expect.stringMatching(/^撤销差异块失败/)));
    expect(onToast.mock.calls.some(([message])=>message.startsWith("已撤销所选"))).toBe(false);
    if(scenario !== "save failure") expect(post.mock.calls.some(([message])=>(message as {type:string}).type === "echo/apply-edits")).toBe(false);
    expect(invoke).not.toHaveBeenCalledWith("coding_changeset_discard_file",expect.anything());
  });

  it("keeps deleted-file hunk submission available and directs restoration to the real file operation", async () => {
    installDiscardApi(false,"deleted");
    render(<CodingWorkbench cwd="/repo"/>);
    const post=readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement);
    await userEvent.click(screen.getByRole("button",{name:/^变更/}));
    await userEvent.click(await screen.findByRole("button",{name:"选择 src/a.ts 的任务差异块"}));
    expect(await screen.findByRole("button",{name:"丢弃 src/a.ts 的差异块 1"})).toBeDisabled();
    expect(screen.getByText(/已删除文件请按文件恢复，以保留原有权限和可执行位/)).toBeInTheDocument();
    expect(screen.getByRole("checkbox",{name:"任务差异块 1"})).toBeEnabled();
    expect(screen.getByRole("button",{name:"丢弃 src/a.ts 的改动"})).toBeEnabled();
    expect(invoke).not.toHaveBeenCalledWith("coding_changeset_revert_hunk_preview",expect.anything());
    expect(post.mock.calls.some(([message])=>(message as {type:string}).type === "echo/apply-edits")).toBe(false);
  });
  it("sends only selected diagnostics and captured terminal output with their real source metadata", async () => {
    installApi(verificationTask({phase:"idle"}));
    const onStartRun=vi.fn(async (_root,_request,_model,_paths,bind,_prompt?:string,_coding?:CodingTurnOptions)=>{await bind("bound");return "bound";});
    render(<CodingWorkbench cwd="/repo" apiReady models={[{id:"test-model"}]} defaultModelId="test-model" onStartRun={onStartRun}/>);
    const frame=await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    readyEditor(frame,0,false,undefined,{diagnostics:[
      {workspace:"/repo",uri:"file:///repo/a.ts",source:"ts",range:{start:{line:3,character:1},end:{line:3,character:4}},severity:"error",message:"Selected problem",version:9},
      {workspace:"/repo",uri:"file:///repo/b.ts",source:"ts",range:{start:{line:0,character:0},end:{line:0,character:1}},severity:"warning",message:"Unselected problem",version:2}],
      terminal:{workspace:"/repo",terminalId:"terminal-7",source:"terminal-selection",cwd:"/repo/pkg",text:"test failed: real captured output",truncated:false,exitCode:1}});
    await userEvent.click(screen.getByRole("button",{name:"添加上下文"}));
    await userEvent.click(screen.getByRole("menuitem",{name:"诊断（Problems）"}));
    await userEvent.click(await screen.findByRole("checkbox",{name:/Selected problem/}));
    await userEvent.click(screen.getByRole("button",{name:"关闭诊断选择"}));
    await userEvent.click(screen.getByRole("button",{name:"添加上下文"}));
    await userEvent.click(screen.getByRole("menuitem",{name:"终端输出"}));
    await screen.findByRole("region",{name:"上下文预览"});
    await userEvent.click(screen.getByRole("button",{name:"关闭上下文预览"}));
    fireEvent.change(screen.getByRole("combobox",{name:"开发意图"}),{target:{value:"ask"}});
    fireEvent.change(screen.getByRole("textbox",{name:"任务描述"}),{target:{value:"解释选择的问题和终端失败"}});
    await userEvent.click(screen.getByRole("button",{name:"开始 Agent 任务"}));
    await waitFor(()=>expect(onStartRun).toHaveBeenCalledOnce());
    const prompt=onStartRun.mock.calls[0][5];
    expect(prompt).toContain("Selected problem");
    expect(prompt).not.toContain("Unselected problem");
    expect(prompt).toContain("terminal-7");
    expect(prompt).toContain("/repo/pkg");
    expect(prompt).toContain("test failed: real captured output");
    expect(prompt).toContain("version");
  });
  it("requires acknowledged save before manual verification, and checks clean state again", async () => {
    const task = verificationTask({phase: "delivered"}); installApi(task);
    useTaskStore.setState({root: "/repo", task});
    render(<CodingWorkbench cwd="/repo" />);
    const post = readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement, 1);
    await userEvent.click(screen.getByRole("button", {name: "检查"}));
    await userEvent.click(await screen.findByRole("button", {name: "运行全部验证"}));
    await screen.findByRole("button", {name: "保存并继续"});
    expect(invoke).not.toHaveBeenCalledWith("coding_task_begin_verification", expect.anything());
    await userEvent.click(screen.getByRole("button", {name: "保存并继续"}));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_verification_run", expect.objectContaining({root: "/repo"})));
    expect(post.mock.calls.filter(([data]) => (data as {type: string}).type === "echo/get-dirty")).toHaveLength(2);
    expect(post.mock.calls.some(([data]) => (data as {type: string}).type === "echo/save-all")).toBe(true);
  });
  it("reads the revision after the real before/after mutation save bridge", async () => {
    let latest = verificationTask({phase: "verifying"});
    installApi(latest);
    const ordinary = invoke.getMockImplementation()!;
    invoke.mockImplementation(async (command, args) => {
    if (command === "coding_verification_config_get") return {checks:[]};
    if (["coding_checkpoint_list","coding_git_commit_history","coding_review_list"].includes(command)) return [];
      if (command === "coding_task_report_interrupted") { latest = {...latest,phase:"paused",updatedAt:"paused-before-save"}; return latest; }
      if (command === "coding_task_begin_followup") { latest = {...latest,phase:"implementing",updatedAt:"followup"}; return latest; }
      if (command === "coding_orchestrator_report_implementation") { latest = {...latest,phase:"verifying",updatedAt:"saved-by-bridge"}; return latest; }
      if (command === "coding_task_get") return latest;
      if (command === "coding_orchestrator_state") return {task:latest,problems:[],repairRounds:[],changedFileCount:1,maxRepairRounds:3};
      return ordinary(command,args);
    });
    useTaskStore.setState({root:"/repo",task:latest});
    render(<CodingWorkbench cwd="/repo" />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const origin = new URL(frame.src).origin;
    const token = JSON.parse(frame.name.slice("echo-embed:".length)).bridgeToken;
    const send = (data: object) => act(() => window.dispatchEvent(new MessageEvent("message",{origin,source:frame.contentWindow,data:{...data,token,instanceId:"test-native-editor"}})));
    const post = readyEditor(frame,1,false,async () => {
      send({type:"echo/before-mutation",id:"save-before",operation:"write",paths:["/repo/src/a.ts"]});
      await waitFor(() => expect(post).toHaveBeenCalledWith(expect.objectContaining({id:"save-before",ok:true}),origin));
      const response = post.mock.calls.map(([value]) => value as {id:string;value:unknown}).find(value => value.id === "save-before")!;
      send({type:"echo/after-mutation",id:"save-after",ticket:response.value,success:true});
      await waitFor(() => expect(post).toHaveBeenCalledWith(expect.objectContaining({id:"save-after",ok:true}),origin));
    });
    await userEvent.click(screen.getByRole("button",{name:"检查"}));
    await userEvent.click(await screen.findByRole("button",{name:"运行全部验证"}));
    await userEvent.click(await screen.findByRole("button",{name:"保存并继续"}));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_task_begin_verification",expect.objectContaining({expectedUpdatedAt:"saved-by-bridge"})));
    expect(invoke).toHaveBeenCalledWith("coding_task_report_interrupted",expect.objectContaining({interruption:"paused"}));
  });

  it("preserves the other task editor block when a clean editor reply arrives late", async () => {
    const original = verificationTask({id:"original",phase:"paused",sessionId:"bound"}); installApi(original);
    useTaskStore.setState({root:"/repo",task:original});
    render(<CodingWorkbench cwd="/repo" sessionId="bound" onSendMessage={vi.fn()} />);
    readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement);
    let finish!: (value:number) => void;
    const getDirtyCount = vi.fn(() => new Promise<number>(resolve => {finish=resolve;}));
    const unregister = registerWorkspaceEditor("/repo",{getDirtyCount,saveAll:async()=>true});
    await userEvent.click(screen.getByRole("button",{name:"继续执行"}));
    await waitFor(() => expect(getDirtyCount).toHaveBeenCalledOnce());
    act(() => {
      useTaskStore.setState({task:verificationTask({id:"other",phase:"paused"})});
      useCodingRuntimeStore.getState().setEditorBlock("/repo","other","保留另一任务的保存错误");
    });
    await act(async () => {finish(0);});
    expect(useCodingRuntimeStore.getState().editorBlocks[codingRuntimeKey("/repo","other")]).toBe("保留另一任务的保存错误");
    expect(invoke).not.toHaveBeenCalledWith("coding_task_resume",expect.anything());
    unregister();
  });

  it("does not claim delivery before current gates are satisfied and offers the pending action", async () => {
    const task = verificationTask({phase: "delivered"}); const onOpenReport = vi.fn();
    invoke.mockResolvedValue({task, deliverable: false, gates: [{id: "acceptance", title: "人工验收", summary: "确认原始需求", status: "not_satisfied"}, {id: "test", status: "not_applicable"}]});
    const view = render(<TaskEvidenceStrip root="/repo-a" task={task} revision={0} needsInput={false} onOpenPlan={() => {}} onOpenReport={onOpenReport} />);
    expect(await screen.findByText("门禁 0/1")).toBeInTheDocument();
    expect(screen.getByText("本轮完成")).toBeInTheDocument(); expect(screen.queryByText("已交付")).toBeNull();
    expect(screen.getByText("工作区：/repo-a")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", {name: "下一步：人工验收"})); expect(onOpenReport).toHaveBeenCalledOnce();
    invoke.mockResolvedValue({task, deliverable: true, gates: [{id: "acceptance", status: "satisfied"}, {id: "test", status: "not_applicable"}]});
    view.rerender(<TaskEvidenceStrip root="/repo-a" task={task} revision={1} needsInput={false} onOpenPlan={() => {}} onOpenReport={onOpenReport} />);
    expect(await screen.findByText("本轮完成")).toBeInTheDocument();
    expect(screen.getByText("门禁 1/1")).toBeInTheDocument();
  });
  it("cancels a preflight started in A after switching to B without creating in either root", async () => {
    const task = verificationTask({phase: "idle"}); installApi(task);
    const ordinary = invoke.getMockImplementation()!; let hold = false; let entered = false; let release!: (value: unknown) => void;
    const pending = new Promise(resolve => { release = resolve; });
    invoke.mockImplementation(async (command, args) => command === "coding_task_list" && hold && (args as {root: string}).root === "/a" ? (entered = true, pending) : ordinary(command, args));
    const onStartRun = vi.fn(); const props = {apiReady: true, models: [{id: "test-model"}], defaultModelId: "test-model", onStartRun};
    const view = render(<CodingWorkbench cwd="/a" {...props} />);
    readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement);
    await waitFor(() => expect(useTaskStore.getState().root).toBe("/a")); hold = true;
    fireEvent.change(screen.getByRole("textbox", {name: "任务描述"}), {target: {value: "Only A"}});
    await userEvent.click(screen.getByRole("button", {name: "开始 Agent 任务"}));
    await waitFor(() => expect(entered).toBe(true));
    view.rerender(<CodingWorkbench cwd="/b" {...props} />);
    await act(async () => { release([]); await pending; });
    expect(useTaskStore.getState().root).toBe("/b");
    expect(invoke).not.toHaveBeenCalledWith("coding_task_create", expect.anything()); expect(onStartRun).not.toHaveBeenCalled();
  });
  it("captures a disk recovery baseline while leaving the current buffer unsaved for the versioned editor", async () => {
    installApi(verificationTask({id:"saved-task",phase:"discovering",sessionId:"bound"}));
    const onStartRun=vi.fn(async (_root,_requirement,_model,_paths,bind,_prompt?:string,_coding?:CodingTurnOptions)=>{await bind("bound");return "bound";});
    render(<CodingWorkbench cwd="/saved" apiReady models={[{id:"model"}]} defaultModelId="model" onStartRun={onStartRun}/>);
    const post=readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement,1);
    fireEvent.change(screen.getByRole("textbox",{name:"任务描述"}),{target:{value:"Use buffer B"}});
    await userEvent.click(screen.getByRole("button",{name:"开始 Agent 任务"}));
    await waitFor(()=>expect(onStartRun).toHaveBeenCalledOnce());
    expect(invoke).toHaveBeenCalledWith("coding_changeset_capture_baseline",expect.objectContaining({root:"/saved",taskId:"saved-task"}));
    expect(post.mock.calls.some(([data])=>(data as {type:string}).type==="echo/save-all")).toBe(false);
    expect(onStartRun.mock.calls[0][6]).toMatchObject({mode:"agent",documentToolBridge:true});
  });
  it("records an already-created A task failure in A after switching to B", async () => {
    const task = verificationTask({id: "created-a", phase: "idle"}); installApi(task);
    const ordinary = invoke.getMockImplementation()!; let resolveCreated!: (value: unknown) => void;
    const created = new Promise(resolve => {resolveCreated = resolve;});
    invoke.mockImplementation(async (command, args) => command === "coding_task_create" ? created : ordinary(command, args));
    const onStartRun = vi.fn(); const props = {apiReady: true, models: [{id: "model"}], defaultModelId: "model", onStartRun};
    const view = render(<CodingWorkbench cwd="/a" {...props} />);
    readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement);
    fireEvent.change(screen.getByRole("textbox", {name: "任务描述"}), {target: {value: "A only"}});
    await userEvent.click(screen.getByRole("button", {name: "开始 Agent 任务"}));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_task_create", expect.objectContaining({root: "/a"})));
    view.rerender(<CodingWorkbench cwd="/b" {...props} />);
    await act(async () => {resolveCreated(task); await created;});
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_task_report_start_failed", expect.objectContaining({root: "/a", taskId: "created-a"})));
    expect(useTaskStore.getState().root).toBe("/b"); expect(useTaskStore.getState().task).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith("coding_task_set_context", expect.anything()); expect(onStartRun).not.toHaveBeenCalled();
  });

  it("does not resume an interrupted task when saving the current buffer fails", async () => {
    const task = verificationTask({id: "paused", phase: "paused", sessionId: "bound"}); installApi(task);
    useTaskStore.setState({root: "/repo", task}); const onSendMessage = vi.fn();
    render(<CodingWorkbench cwd="/repo" sessionId="bound" onSendMessage={onSendMessage} />);
    readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement, 1, true);
    await userEvent.click(screen.getByRole("button", {name: "继续执行"}));
    await userEvent.click(await screen.findByRole("button", {name: "保存并继续"}));
    await waitFor(() => expect(screen.getByRole("button", {name: "继续执行"})).not.toBeDisabled());
    expect(invoke).not.toHaveBeenCalledWith("coding_task_resume", expect.anything()); expect(onSendMessage).not.toHaveBeenCalled();
  });

  it("does not dispatch a late resume after selecting another task in the same workspace", async () => {
    const original = verificationTask({id:"original",phase:"paused",sessionId:"bound"}); installApi(original);
    const ordinary = invoke.getMockImplementation()!;
    let complete!: (value: unknown) => void;
    invoke.mockImplementation(async (command,args) => command === "coding_task_resume" ? new Promise(resolve => {complete=resolve;}) : ordinary(command,args));
    useTaskStore.setState({root:"/repo",task:original});
    const onSendMessage = vi.fn();
    const view = render(<CodingWorkbench cwd="/repo" sessionId="bound" onSendMessage={onSendMessage} />);
    readyEditor(await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement);
    await userEvent.click(screen.getByRole("button",{name:"继续执行"}));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_task_resume",{root:"/repo",taskId:"original"}));
    act(() => useTaskStore.setState({task:verificationTask({id:"other",phase:"paused",sessionId:"other-bound"})}));
    view.rerender(<CodingWorkbench cwd="/repo" sessionId="other-bound" onSendMessage={onSendMessage} />);
    await act(async () => {complete({...original,phase:"implementing"});});
    expect(onSendMessage).not.toHaveBeenCalled();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_task_report_interrupted",{root:"/repo",taskId:"original",interruption:"paused"}));
  });

});
