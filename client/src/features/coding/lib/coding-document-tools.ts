import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useSessionStore } from "@/stores/session-store";
import { useTaskStore } from "../store/task-store";
import { codingApi } from "./tauri-api";
import { useSessionsStore } from "@/stores/sessions-store";
import {
  workspaceEditor,
  type WorkspaceEditor,
  type DocumentSnapshot,
  type EditOperation,
  type ApplyResult,
} from "./workspace-editor";
import { relativeToWorkspace } from "./windows-path";

export interface DocumentToolRequest {
  operationId: string;
  sessionId: string;
  runId: string;
  workspace: string;
  toolName: string;
  toolKind: string;
  phase: "before" | "after";
  input: Record<string, unknown>;
  succeeded?: boolean;
}
const snapshots = new Map<string, DocumentSnapshot>();
const appliedPaths = new Map<string, Set<string>>();
const completed = new Map<string, Record<string, unknown>>();
const inflight = new Map<string, Promise<Record<string, unknown>>>();
class DocumentApplyError extends Error {
  constructor(message: string, readonly appliedDocuments: Array<{path:string;beforeHash:string;afterHash?:string}>) {
    super(message);
  }
}
const key = (request: Pick<DocumentToolRequest, "sessionId" | "runId">) =>
  JSON.stringify([request.sessionId, request.runId]);
export function confirmedRunPaths(sessionId: string, runId: string): string[] {
  return [...(appliedPaths.get(key({ sessionId, runId })) ?? [])];
}
export function releaseDocumentRun(sessionId: string, runId: string): void {
  const scope = key({ sessionId, runId });
  appliedPaths.delete(scope);
  for (const id of snapshots.keys())
    if (id.startsWith(scope)) snapshots.delete(id);
}
function assertActive(request: DocumentToolRequest): void {
  const owner = useSessionsStore
    .getState()
    .independent.find((session) => session.sessionId === request.sessionId);
  if (
    !owner?.cwd ||
    (relativeToWorkspace(owner.cwd, request.workspace) !== "" &&
      (!workspaceEditor(owner.cwd) ||
        workspaceEditor(owner.cwd) !== workspaceEditor(request.workspace)))
  )
    throw new Error("工具工作区与会话归属不匹配");
  const transcript = useSessionStore.getState().transcripts[request.sessionId];
  const message = transcript?.messages.find(
    (message) => message.id === transcript.streamingMessageId,
  );
  if (!transcript?.streamingMessageId || message?.promptId !== request.runId) {
    throw new Error("本轮已停止或已被替代，拒绝旧工具操作");
  }
}
function endPosition(text: string) {
  const lines = text.split("\n");
  return { line: lines.length - 1, character: lines[lines.length - 1].length };
}
function rememberPath(request: DocumentToolRequest, path: string) {
  const scope = key(request);
  const paths = appliedPaths.get(scope) ?? new Set<string>();
  paths.add(path);
  appliedPaths.set(scope, paths);
}

function checkedPath(request:DocumentToolRequest, raw:string):string {
  const path=raw.startsWith("/")||/^[A-Za-z]:[\\/]/.test(raw)?raw:`${request.workspace.replace(/[\\/]$/,"")}/${raw}`;
  if(relativeToWorkspace(request.workspace,path)===null||path.replaceAll("\\","/").split("/").includes(".."))
    throw new Error("只能访问当前工程内文件");
  return path;
}

type PatchFile = {path:string;content:string|null;delete:boolean;moveTo:string|null};
async function executePatch(request:DocumentToolRequest,editor:WorkspaceEditor,patch:string):Promise<Record<string,unknown>> {
  if(!editor.getDocument||!editor.applyEdits)throw new Error("IDE 不支持版本化文档协议");
  const targets=await invoke<PatchFile[]>("coding_document_patch_preview",{patch,originals:null});
  assertActive(request);
  const originals:Record<string,string>={};
  const before:DocumentSnapshot[]=[];
  for(const target of targets){
    const path=checkedPath(request,target.path);
    const snapshot=await editor.getDocument(path,true);
    assertActive(request);
    if(snapshot.truncated)throw new Error(`补丁文件 ${target.path} 超过文本编辑预算`);
    if((target.delete||target.moveTo)&&snapshot.dirty)throw new Error("删除或移动前请先保存该文件，未保存内容已保留");
    if(snapshot.hash==="missing"&&(target.delete||target.content===null))throw new Error(`补丁源文件不存在：${target.path}`);
    if(snapshot.hash!=="missing"&&target.content!==null)throw new Error(`补丁新增目标已存在：${target.path}`);
    if(target.moveTo){
      const destination=await editor.getDocument(checkedPath(request,target.moveTo),true);
      assertActive(request);
      if(destination.hash!=="missing")throw new Error(`移动目标已存在：${target.moveTo}`);
    }
    originals[target.path]=snapshot.content;
    before.push(snapshot);
  }
  const prepared=await invoke<PatchFile[]>("coding_document_patch_preview",{patch,originals});
  assertActive(request);
  for(let index=0;index<before.length;index++){
    if(!before[index].dirty)continue;
    const task=useTaskStore.getState().task;
    if(task?.sessionId!==request.sessionId)throw new Error("无法确认未保存内容的恢复点归属");
    await codingApi.preserveBuffer(request.workspace,task.id,request.runId,
      relativeToWorkspace(request.workspace,before[index].path)!,before[index].content);
    assertActive(request);
  }
  const operation:EditOperation={workspace:before[0].workspace,sessionId:request.sessionId,runId:request.runId,operationId:request.operationId,
    files:prepared.map((target,index)=>({uri:before[index].uri,expectedVersion:before[index].version,expectedHash:before[index].hash,
      save:true,delete:target.delete,moveTo:target.moveTo?checkedPath(request,target.moveTo):undefined,
      edits:target.delete?[]:[{range:{start:{line:0,character:0},end:endPosition(before[index].content)},text:target.content!}]}))};
  if(!await invoke<boolean>("coding_document_tool_is_active",{operationId:request.operationId}))throw new Error("文档操作已取消");
  assertActive(request);
  const result=await editor.applyEdits(operation);
  const receipts:Array<{path:string;beforeHash:string;afterHash?:string}>=[];
  for(const file of result.files){
    if(file.status!=="applied")continue;
    const index=before.findIndex(snapshot=>snapshot.uri===file.uri);
    if(index<0)continue;
    const path=relativeToWorkspace(request.workspace,before[index].path)!;
    receipts.push({path,beforeHash:before[index].hash,afterHash:file.checkpointHash??file.hash});
    rememberPath(request,path);
    snapshots.delete(`${key(request)}:${before[index].path}`);
    if(file.movedTo&&file.movedHash&&prepared[index].moveTo){
      const moved=relativeToWorkspace(request.workspace,checkedPath(request,prepared[index].moveTo!))!;
      receipts.push({path:moved,beforeHash:"missing",afterHash:file.movedHash});rememberPath(request,moved);
    }
  }
  if(result.status!=="applied"||result.files.some(file=>file.saveError))throw new DocumentApplyError(JSON.stringify(result),receipts);
  return {handled:true,appliedDocuments:receipts,text:`已通过版本化文档协议应用补丁，共 ${result.files.length} 个文件`};
}

export async function executeDocumentTool(
  request: DocumentToolRequest,
): Promise<Record<string, unknown>> {
  assertActive(request);
  const editor: WorkspaceEditor = workspaceEditor(request.workspace) ?? {
    // Leaving a project saves its buffers first. Native checks its editor lease again,
    // so a missing/aliased renderer registry can never authorize a disk overwrite.
    getDirtyCount: async () => {
      if (
        await invoke<boolean>("coding_document_editor_present", {
          root: request.workspace,
        })
      )
        throw new Error(
          "工程编辑器已挂载但协调连接不可用，请重新连接后执行命令",
        );
      return 0;
    },
    saveAll: async () => true,
    getDocument: (path) =>
      invoke<DocumentSnapshot>("coding_closed_document_snapshot", {
        workspace: request.workspace,
        sessionId: request.sessionId,
        runId: request.runId,
        operationId: request.operationId,
        path,
      }),
    applyEdits: (operation) =>
      invoke<ApplyResult>("coding_closed_document_apply", { operation }),
  };
  if (request.phase === "after") {
    // Shell/MCP effects cannot be attributed solely from a filesystem watcher.
    // They remain external observations in the run checkpoint.
    return { handled: true, text: "命令已结束；磁盘变化将在检查点中核对" };
  }
  const kind = request.toolKind;
  if(kind==="edit"&&typeof request.input.patch==="string")return executePatch(request,editor,request.input.patch);
  const rawPath = request.input.target_file ?? request.input.file_path ?? request.input.filePath;
  if (!["read", "edit", "write"].includes(kind)) {
    if (
      !["search", "list_dir"].includes(kind) &&
      (await editor.getDirtyCount()) > 0
    ) {
      throw new Error(
        "命令可能读写磁盘，但编辑器仍有未保存内容。请先保存相关文件后重试；当前内容已保留",
      );
    }
    assertActive(request);
    return { handled: false };
  }
  if (typeof rawPath !== "string" || !rawPath) throw new Error("缺少文件路径");
  const path =
    rawPath.startsWith("/") || /^[A-Za-z]:[\\/]/.test(rawPath)
      ? rawPath
      : `${request.workspace.replace(/[\\/]$/, "")}/${rawPath}`;
  const relative = relativeToWorkspace(request.workspace, path);
  if (relative === null || path.replaceAll("\\", "/").split("/").includes(".."))
    throw new Error("只能访问当前工程内文件");
  if (!editor.getDocument || !editor.applyEdits)
    throw new Error("IDE 不支持版本化文档协议，请重新构建并连接");
  const snapshotKey = `${key(request)}:${path}`;
  if (kind === "read") {
    // Preserve the Runtime's image/PDF/Office readers and their typed outputs.
    if (/\.(?:png|jpe?g|gif|webp|bmp|ico|pdf|docx?|xlsx?|pptx?|odt|ods|odp)$/i.test(path)) return {handled:false};
    let snapshot:DocumentSnapshot;
    try { snapshot = await editor.getDocument(path, true); }
    catch(error) {
      if(await editor.getDirtyCount()===0){assertActive(request);return {handled:false};}
      throw error;
    }
    assertActive(request);
    snapshots.set(snapshotKey, snapshot);
    if (snapshot.hash === "missing") throw new Error("文件不存在");
    const requestedOffset =
      typeof request.input.offset === "number" ? request.input.offset : 1;
    let start =
      requestedOffset < 0
        ? Math.max(0, snapshot.content.split("\n").length + requestedOffset)
        : Math.max(0, requestedOffset - 1);
    const limit =
      typeof request.input.limit === "number"
        ? Math.max(1, Math.min(2000, request.input.limit))
        : 2000;
    if(snapshot.truncated){
      if(!snapshot.dirty)return {handled:false};
      snapshot=await editor.getDocument(path,false,{offset:requestedOffset,limit});
      assertActive(request);
      if(snapshot.firstLine===undefined)throw new Error("IDE 不支持大文件缓冲区范围读取，请重新连接编辑器");
      start=0;
    }
    const lines = snapshot.content.split("\n");
    const body = lines
      .slice(start, start + limit)
      .map((line, i) => `${i + start + (snapshot.firstLine ?? 1)}|${line}`)
      .join("\n");
    return {
      handled: true,
      text: `来源=${snapshot.source} dirty=${snapshot.dirty} version=${snapshot.version} hash=${snapshot.hash}\n${body}`,
    };
  }
  let snapshot = snapshots.get(snapshotKey);
  // File creation must still prove absence immediately before the operation.
  if (!snapshot) {
    const current = await editor.getDocument(path, true);
    assertActive(request);
    if (current.hash !== "missing")
      throw new Error("修改前必须先读取当前文件；拒绝未绑定版本的写入");
    snapshot = current;
  }
  if (snapshot.truncated)
    throw new Error("截断快照不能作为全文件修改依据，请按范围重新读取");
  let replacement: string;
  if (kind === "write") {
    if (typeof request.input.content !== "string")
      throw new Error("缺少写入内容");
    replacement = request.input.content;
  } else {
    const oldText = request.input.old_string ?? request.input.oldString;
    const newText = request.input.new_string ?? request.input.newString;
    const replaceAll = request.input.replace_all === true || request.input.replaceAll === true;
    if (typeof oldText !== "string" || typeof newText !== "string")
      throw new Error("编辑参数无效");
    if (!oldText && snapshot.hash !== "missing")
      throw new Error("已有文件不接受无范围追加，请先读取并提供准确旧内容");
    const count = oldText ? snapshot.content.split(oldText).length - 1 : 1;
    if (count === 0 || (count > 1 && !replaceAll))
      throw new Error("旧内容不存在或不唯一，请重新读取并缩小编辑范围");
    replacement = oldText
      ? replaceAll
        ? snapshot.content.split(oldText).join(newText)
        : snapshot.content.replace(oldText, () => newText)
      : newText;
  }
  assertActive(request);
  if (snapshot.dirty) {
    const task = useTaskStore.getState().task;
    if (!task || task.sessionId !== request.sessionId)
      throw new Error("无法确认未保存内容的恢复点归属");
    await codingApi.preserveBuffer(
      request.workspace,
      task.id,
      request.runId,
      relative,
      snapshot.content,
    );
    assertActive(request);
  }
  const operation: EditOperation = {
    workspace: snapshot.workspace,
    sessionId: request.sessionId,
    runId: request.runId,
    operationId: request.operationId,
    files: [
      {
        uri: snapshot.uri,
        expectedVersion: snapshot.version,
        expectedHash: snapshot.hash,
        save: true,
        edits: [
          {
            range: {
              start: { line: 0, character: 0 },
              end: endPosition(snapshot.content),
            },
            text: replacement,
          },
        ],
      },
    ],
  };
  if (
    !(await invoke<boolean>("coding_document_tool_is_active", {
      operationId: request.operationId,
    }))
  )
    throw new Error("文档操作已取消");
  assertActive(request);
  const result = await editor.applyEdits(operation);
  const file = result.files[0];
  if (file?.status === "applied") {
    rememberPath(request, relative);
    snapshots.delete(snapshotKey); // Format-on-save may change text. Require another real read before the next edit.
  }
  if (result.status !== "applied" || file?.saveError)
    throw new DocumentApplyError(JSON.stringify(result), file?.status === "applied"
      ? [{path: relative, beforeHash:snapshot.hash, afterHash:file.checkpointHash ?? file.hash}] : []);
  return {
    handled: true,
    appliedPaths: [relative],
    appliedDocuments: [{ path: relative, beforeHash: snapshot.hash, afterHash:file.checkpointHash ?? file.hash }],
    text: `已通过 Theia 应用 ${relative}，版本 ${file.version ?? "disk"}；undo组 ${file.undoGroup ?? request.operationId}`,
  };
}

/** One listener lives above page navigation; duplicate events share a single promise. */
export async function listenDocumentTools(): Promise<() => void> {
  return listen<DocumentToolRequest>("agent://document-tool", (event) => {
    const request = event.payload;
    const operationKey = JSON.stringify([
      request.workspace,
      request.sessionId,
      request.runId,
      request.operationId,
      request.phase,
    ]);
    const old = completed.get(operationKey);
    let work = old ? Promise.resolve(old) : inflight.get(operationKey);
    if (!work) {
      work = executeDocumentTool(request).catch((error) => ({
        handled: true,
        error: String(error).replace(/^Error:\s*/, ""),
        ...(error instanceof DocumentApplyError ? {appliedDocuments:error.appliedDocuments} : {}),
      }));
      inflight.set(operationKey, work);
    }
    void work
      .then((result) => {
        inflight.delete(operationKey);
        completed.set(operationKey, result);
        while (completed.size > 512)
          completed.delete(completed.keys().next().value!);
        return invoke("coding_document_tool_respond", {
          operationId: request.operationId,
          result,
        });
      })
      .catch(() => undefined);
  });
}
