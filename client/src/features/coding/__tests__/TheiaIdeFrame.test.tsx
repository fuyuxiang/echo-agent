import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createRef } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { TheiaIdeFrame, type TheiaIdeFrameHandle } from "../TheiaIdeFrame";
import { workspaceEditor, type DocumentSnapshot, type EditOperation } from "../lib/workspace-editor";

const invoke = vi.fn(async (_command: string, _args?: unknown): Promise<unknown> => ({ url: "http://127.0.0.1:41773/", embedToken: "embed-token" }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string, args: unknown) => invoke(command, args) }));
vi.mock("@/lib/usage-quota", () => ({ isQuotaBlocking: () => false, loadQuotaConfig: () => null, loadUsage: () => ({}), recordUsage: vi.fn() }));

const INSTANCE = "native-instance-1";
const CAPABILITIES = ["document-snapshots", "versioned-edits", "inline-completion", "diagnostics", "terminal-context", "native-diff"];
const expectBackendStarts = (count:number) => expect(invoke.mock.calls.filter(([command]) => command === "coding_theia_start")).toHaveLength(count);
const snapshot: DocumentSnapshot = {
  workspace: "/repo", uri: "file:///repo/a.ts", path: "/repo/a.ts", language: "typescript", exists: true,
  version: 3, hash: "a".repeat(64), dirty: true, source: "buffer", content: "unsaved function", selectedText: "function",
  selection: { start: {line:0,character:8}, end: {line:0,character:16} }, contentOffset: 0,
  totalCharacters: 16, truncated: false, capturedAt: "2026-10-29T00:00:00.000Z",
};

function sendFrameMessage(frame: HTMLIFrameElement, data: Record<string, unknown>) {
  const url = new URL(frame.src);
  const { bridgeToken } = JSON.parse(frame.name.slice("echo-embed:".length)) as { bridgeToken: string };
  act(() => {
    window.dispatchEvent(new MessageEvent("message", {
      origin: url.origin,
      source: frame.contentWindow,
      data: { instanceId: INSTANCE, protocolVersion: 1, capabilities: CAPABILITIES, ...data, token: bridgeToken },
    }));
  });
}

describe("Theia IDE bridge", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (command,args) => command === "coding_document_editor_attach"
      ? (args as {root:string}).root : {url:"http://127.0.0.1:41773/",embedToken:"embed-token"});
  });

  it("rejects every mutation whose path list includes a file outside the active project", async () => {
    const onBeforeMutation = vi.fn(async () => ({ taskId: "task-1", closeRound: false }));
    render(<TheiaIdeFrame root="/repo" onBeforeMutation={onBeforeMutation} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");

    sendFrameMessage(frame, { type: "echo/before-mutation", id: "mixed", operation: "move", paths: ["/repo/a.ts", "/other/b.ts"] });
    await waitFor(() => expect(postMessage).toHaveBeenCalledWith(expect.objectContaining({ id: "mixed", ok: false }), new URL(frame.src).origin));
    expect(onBeforeMutation).not.toHaveBeenCalled();

    sendFrameMessage(frame, { type: "echo/before-mutation", id: "inside", operation: "write", paths: ["/repo/a.ts"] });
    await waitFor(() => expect(onBeforeMutation).toHaveBeenCalledWith("write", ["/repo/a.ts"]));
    await waitFor(() => expect(postMessage).toHaveBeenCalledWith(expect.objectContaining({ id: "inside", ok: true }), new URL(frame.src).origin));
  });

  it("reports Theia dirty files and waits for an acknowledged save before leaving", async () => {
    const ref = createRef<TheiaIdeFrameHandle>();
    const onDirtyChange = vi.fn();
    render(<TheiaIdeFrame ref={ref} root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} onDirtyChange={onDirtyChange} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    sendFrameMessage(frame, { type: "echo/ready" });
    sendFrameMessage(frame, { type: "echo/dirty-state", count: 2 });
    expect(onDirtyChange).toHaveBeenLastCalledWith(2);

    const dirty = ref.current!.getDirtyCount();
    await waitFor(() => expect(postMessage).toHaveBeenCalledWith(expect.objectContaining({ type: "echo/get-dirty" }), new URL(frame.src).origin));
    const dirtyRequest = postMessage.mock.calls.find(([message]) => (message as { type?: string }).type === "echo/get-dirty")![0] as { id: string };
    sendFrameMessage(frame, { type: "echo/response", id: dirtyRequest.id, ok: true, value: 2 });
    await expect(dirty).resolves.toBe(2);

    const saved = ref.current!.saveAll();
    await waitFor(() => expect(postMessage).toHaveBeenCalledWith(expect.objectContaining({ type: "echo/save-all" }), new URL(frame.src).origin));
    const saveRequest = postMessage.mock.calls.find(([message]) => (message as { type?: string }).type === "echo/save-all")![0] as { id: string };
    sendFrameMessage(frame, { type: "echo/response", id: saveRequest.id, ok: true, value: 0 });
    await expect(saved).resolves.toBe(true);
    expect(onDirtyChange).toHaveBeenLastCalledWith(0);
  });

  it("accepts only local preview URLs and symbols from the active workspace", async () => {
    const onPreviewUrl = vi.fn();
    const onActiveSymbol = vi.fn();
    render(<TheiaIdeFrame root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} onPreviewUrl={onPreviewUrl} onActiveSymbol={onActiveSymbol} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;

    sendFrameMessage(frame, { type: "echo/active-symbol", path: "/other/a.ts", symbol: "run" });
    expect(onActiveSymbol).toHaveBeenLastCalledWith(null);
    sendFrameMessage(frame, { type: "echo/active-symbol", path: "/repo/a.ts", symbol: "run" });
    expect(onActiveSymbol).toHaveBeenLastCalledWith({ path: "/repo/a.ts", symbol: "run" });

    sendFrameMessage(frame, { type: "echo/preview-url", url: "https://example.com:5173/" });
    expect(onPreviewUrl).not.toHaveBeenCalled();
    sendFrameMessage(frame, { type: "echo/preview-url", url: "http://localhost:5173/" });
    expect(onPreviewUrl).toHaveBeenCalledWith("http://localhost:5173/");
  });

  it("starts a fresh iframe session when the project changes", async () => {
    const callbacks = { onBeforeMutation: vi.fn(), onAfterMutation: vi.fn() };
    const { rerender } = render(<TheiaIdeFrame root="/repo-one" {...callbacks} />);
    const first = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const firstToken = JSON.parse(first.name.slice("echo-embed:".length)).bridgeToken as string;

    rerender(<TheiaIdeFrame root="/repo-two" {...callbacks} />);
    const second = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    await waitFor(() => expect(new URL(second.src).hash).toBe("#/repo-two"));
    expect(JSON.parse(second.name.slice("echo-embed:".length)).bridgeToken).not.toBe(firstToken);
    expect(invoke).toHaveBeenCalledWith("coding_theia_start", { root: "/repo-two" });
  });

  it("restarts a lost local IDE backend and reloads its frame", async () => {
    render(<TheiaIdeFrame root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    sendFrameMessage(frame, { type: "echo/ready" });
    sendFrameMessage(frame, { type: "echo/dirty-state", count: 0 });
    invoke.mockResolvedValueOnce({ url: "http://127.0.0.1:41774/", embedToken: "restarted-token" });

    vi.useFakeTimers();
    try {
      sendFrameMessage(frame, { type: "echo/backend-connection", online: false });
      await act(async () => { await vi.advanceTimersByTimeAsync(1_500); });
      expectBackendStarts(2);
      expect((screen.getByTitle("Echo Code IDE") as HTMLIFrameElement).src).toContain(":41774/");
      expect(screen.getByText("正在启动 Echo Code IDE")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps unsaved editor content accessible when the backend disconnects", async () => {
    render(<TheiaIdeFrame root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    sendFrameMessage(frame, { type: "echo/ready" });
    sendFrameMessage(frame, { type: "echo/dirty-state", count: 2 });

    vi.useFakeTimers();
    try {
      sendFrameMessage(frame, { type: "echo/backend-connection", online: false });
      await act(async () => { await vi.advanceTimersByTimeAsync(1_500); });
      expectBackendStarts(1);
      expect(screen.getByTitle("Echo Code IDE")).toBe(frame);
      expect(screen.getByRole("alert")).toHaveTextContent("2 个未保存文件");
      await act(async () => { fireEvent.click(screen.getByRole("button", { name: "重新连接" })); });
      expectBackendStarts(1);
      expect(screen.getByRole("alertdialog")).toHaveTextContent("重新连接会丢失");
      fireEvent.click(screen.getByRole("button", { name: "保留编辑内容" }));
      expect(screen.getByTitle("Echo Code IDE")).toBe(frame);
      expectBackendStarts(1);
      fireEvent.click(screen.getByRole("button", { name: "重新连接" }));
      await act(async () => { fireEvent.click(screen.getByRole("button", { name: "已保留内容，重新连接" })); });
      expectBackendStarts(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not discard an editor whose save state was never reported", async () => {
    render(<TheiaIdeFrame root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    sendFrameMessage(frame, { type: "echo/ready" });
    vi.useFakeTimers();
    try {
      sendFrameMessage(frame, { type: "echo/backend-connection", online: false });
      await act(async () => { await vi.advanceTimersByTimeAsync(1_500); });
      expectBackendStarts(1);
      expect(screen.getByTitle("Echo Code IDE")).toBe(frame);
      expect(screen.getByRole("alert")).toHaveTextContent("无法确认是否有未保存文件");
      fireEvent.click(screen.getByRole("button", { name: "重新连接" }));
      expectBackendStarts(1);
      expect(screen.getByRole("alertdialog")).toHaveTextContent("无法确认");
      fireEvent.keyDown(document, { key: "Escape" });
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
      expect(screen.getByTitle("Echo Code IDE")).toBe(frame);
    } finally {
      vi.useRealTimers();
    }
  });

  it("opens a Chinese Windows workspace from a verbatim native path", async () => {
    const root = String.raw`\\?\C:\项目\代码开发`;
    const onBeforeMutation = vi.fn(async () => ({ taskId: "task-1", closeRound: false }));
    const onActiveFile = vi.fn();
    const onToast = vi.fn();
    render(<TheiaIdeFrame root={root} onBeforeMutation={onBeforeMutation} onAfterMutation={vi.fn()} onActiveFile={onActiveFile} onToast={onToast} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    expect(decodeURI(new URL(frame.src).hash)).toBe("#C:/项目/代码开发");

    sendFrameMessage(frame, { type: "echo/workspace", path: String.raw`C:\项目\代码开发` });
    expect(onToast).not.toHaveBeenCalled();
    sendFrameMessage(frame, { type: "echo/active-file", path: String.raw`C:\项目\代码开发\src\main.ts` });
    expect(onActiveFile).toHaveBeenLastCalledWith(String.raw`C:\项目\代码开发\src\main.ts`);
    sendFrameMessage(frame, { type: "echo/before-mutation", id: "win-write", operation: "write", paths: [String.raw`C:\项目\代码开发\src\main.ts`] });
    await waitFor(() => expect(onBeforeMutation).toHaveBeenCalledWith("write", [String.raw`C:\项目\代码开发\src\main.ts`]));
    sendFrameMessage(frame, { type: "echo/active-file", path: String.raw`c:\项目\代码开发\src\other.ts` });
    expect(onActiveFile).toHaveBeenLastCalledWith(String.raw`c:\项目\代码开发\src\other.ts`);
  });

  it("guards an error retry after an editor had unsaved content", async () => {
    render(<TheiaIdeFrame root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    sendFrameMessage(frame, { type: "echo/ready" });
    sendFrameMessage(frame, { type: "echo/dirty-state", count: 0 });
    invoke.mockRejectedValueOnce(new Error("Owned backend restart failed"));
    vi.useFakeTimers();
    try {
      sendFrameMessage(frame, { type: "echo/backend-connection", online: false });
      await act(async () => { await vi.advanceTimersByTimeAsync(1_500); });
      expect(screen.getByText(/自动恢复失败/)).toBeInTheDocument();
      // A delayed dirty-state message must still protect the retained frame
      // when the user retries a failed automatic restart.
      sendFrameMessage(frame, { type: "echo/dirty-state", count: 1 });
      fireEvent.click(screen.getByRole("button", { name: "重试" }));
      expectBackendStarts(2);
      expect(screen.getByRole("alertdialog")).toHaveTextContent("1 个未保存文件");
      fireEvent.click(screen.getByRole("button", { name: "保留编辑内容" }));
      expect(screen.getByTitle("Echo Code IDE")).toBe(frame);
    } finally {
      vi.useRealTimers();
    }
  });

  it("invalidates a pending reconnect confirmation when the original connection recovers", async () => {
    render(<TheiaIdeFrame root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    sendFrameMessage(frame, { type: "echo/ready" });
    sendFrameMessage(frame, { type: "echo/dirty-state", count: 1 });
    vi.useFakeTimers();
    try {
      sendFrameMessage(frame, { type: "echo/backend-connection", online: false });
      await act(async () => { await vi.advanceTimersByTimeAsync(1_500); });
      fireEvent.click(screen.getByRole("button", { name: "重新连接" }));
      expect(screen.getByRole("alertdialog")).toBeInTheDocument();
      sendFrameMessage(frame, { type: "echo/backend-connection", online: true });
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
      expect(screen.getByTitle("Echo Code IDE")).toBe(frame);
      expectBackendStarts(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("retains an edit reported while an automatic backend restart is pending", async () => {
    render(<TheiaIdeFrame root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    sendFrameMessage(frame, { type: "echo/ready" });
    sendFrameMessage(frame, { type: "echo/dirty-state", count: 0 });
    let finish!: (endpoint: { url: string; embedToken: string }) => void;
    invoke.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    vi.useFakeTimers();
    try {
      sendFrameMessage(frame, { type: "echo/backend-connection", online: false });
      await act(async () => { await vi.advanceTimersByTimeAsync(1_500); });
      expectBackendStarts(2);
      sendFrameMessage(frame, { type: "echo/dirty-state", count: 1 });
      await act(async () => { finish({ url: "http://127.0.0.1:41775/", embedToken: "new-backend" }); });
      expect(screen.getByTitle("Echo Code IDE")).toBe(frame);
      expect(screen.getByRole("alert")).toHaveTextContent("1 个未保存文件");
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps a pending disconnect notification when parent callbacks change", async () => {
    const callbacks = () => ({ onBeforeMutation: vi.fn(), onAfterMutation: vi.fn(), onToast: vi.fn() });
    const { rerender } = render(<TheiaIdeFrame root="/repo" {...callbacks()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    sendFrameMessage(frame, { type: "echo/ready" });
    sendFrameMessage(frame, { type: "echo/dirty-state", count: 1 });
    vi.useFakeTimers();
    try {
      sendFrameMessage(frame, { type: "echo/backend-connection", online: false });
      await act(async () => { await vi.advanceTimersByTimeAsync(500); });
      rerender(<TheiaIdeFrame root="/repo" {...callbacks()} />);
      await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
      expect(screen.getByRole("alert")).toHaveTextContent("1 个未保存文件");
      expect(screen.getByTitle("Echo Code IDE")).toBe(frame);
      expectBackendStarts(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("reads an unsaved buffer and exact selection through the current instance without requesting save", async () => {
    const ref = createRef<TheiaIdeFrameHandle>();
    const onBeforeMutation = vi.fn();
    render(<TheiaIdeFrame ref={ref} root="/repo" onBeforeMutation={onBeforeMutation} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    sendFrameMessage(frame, {type:"echo/ready"});
    const pending = ref.current!.getDocument("/repo/a.ts", true);
    const request = postMessage.mock.calls.map(([message]) => message as Record<string, unknown>).find(message => message.type === "echo/get-document")!;
    expect(request).toEqual(expect.objectContaining({path:"/repo/a.ts",full:true,instanceId:INSTANCE}));
    sendFrameMessage(frame, {type:"echo/response",id:request.id,ok:true,value:snapshot});
    await expect(pending).resolves.toEqual(snapshot);
    expect(onBeforeMutation).not.toHaveBeenCalled();
    expect(postMessage.mock.calls.some(([message]) => (message as {type?:string}).type === "echo/save-all")).toBe(false);
  });

  it("preserves operation/session/run/version identities and exposes native conflicts to the caller", async () => {
    const ref = createRef<TheiaIdeFrameHandle>();
    render(<TheiaIdeFrame ref={ref} root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    sendFrameMessage(frame, {type:"echo/ready"});
    const operation: EditOperation = {operationId:"op-1",sessionId:"session-1",runId:"run-1",workspace:"/repo",files:[{
      uri:snapshot.uri,expectedVersion:3,expectedHash:snapshot.hash,save:true,
      edits:[{range:{start:{line:0,character:0},end:{line:0,character:16}},text:"agent change"}],
    }]};
    const pending = ref.current!.applyEdits(operation);
    const request = postMessage.mock.calls.map(([message]) => message as Record<string, unknown>).find(message => message.type === "echo/apply-edits")!;
    expect(request.operation).toEqual(operation);
    sendFrameMessage(frame, {type:"echo/response",id:request.id,ok:true,instanceId:"previous-instance",value:{status:"applied"}});
    const conflict = {operationId:"op-1",sessionId:"session-1",runId:"run-1",status:"conflict",files:[{uri:snapshot.uri,status:"conflict",error:"newer native version"}]};
    sendFrameMessage(frame, {type:"echo/response",id:request.id,ok:true,value:conflict});
    await expect(pending).resolves.toEqual(conflict);
    expect(postMessage.mock.calls.some(([message]) => (message as {type?:string}).type === "echo/save-all")).toBe(false);
  });

  it("opens native Diff through the authenticated instance and waits for the native opener ACK without saving", async () => {
    const ref = createRef<TheiaIdeFrameHandle>();
    render(<TheiaIdeFrame ref={ref} root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    sendFrameMessage(frame, {type:"echo/ready"});
    let opened = false;
    const pending = ref.current!.openDiff("/repo/a.ts", "original before run", "a.ts · 本轮修改").then(() => {opened = true;});
    const request = postMessage.mock.calls.map(([message]) => message as Record<string, unknown>).find(message => message.type === "echo/open-diff")!;
    expect(request).toEqual(expect.objectContaining({path:"/repo/a.ts",original:"original before run",title:"a.ts · 本轮修改",instanceId:INSTANCE}));
    await Promise.resolve(); expect(opened).toBe(false);
    sendFrameMessage(frame, {type:"echo/response",id:request.id,ok:true});
    await expect(pending).resolves.toBeUndefined(); expect(opened).toBe(true);
    await expect(ref.current!.openDiff("/other/a.ts", "")).rejects.toThrow("当前工程");
    expect(postMessage.mock.calls.some(([message]) => (message as {type?:string}).type === "echo/save-all")).toBe(false);
    sendFrameMessage(frame, {type:"echo/ready",capabilities:CAPABILITIES.filter(value => value !== "native-diff")});
    await expect(ref.current!.openDiff("/repo/a.ts", "")).rejects.toThrow("native-diff");
  });

  it("rejects a pending document request when the native instance changes and ignores the old response", async () => {
    const ref = createRef<TheiaIdeFrameHandle>();
    render(<TheiaIdeFrame ref={ref} root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    sendFrameMessage(frame, {type:"echo/ready"});
    const old = ref.current!.getDocument("/repo/a.ts");
    const rejected = expect(old).rejects.toThrow("实例已更换");
    const oldRequest = postMessage.mock.calls.map(([message]) => message as Record<string, unknown>).find(message => message.type === "echo/get-document")!;
    sendFrameMessage(frame, {type:"echo/ready",instanceId:"native-instance-2"}); await rejected;
    const next = ref.current!.getDocument("/repo/a.ts");
    const nextRequest = postMessage.mock.calls.map(([message]) => message as Record<string, unknown>).filter(message => message.type === "echo/get-document").slice(-1)[0]!;
    sendFrameMessage(frame, {type:"echo/response",id:oldRequest.id,ok:true,value:{...snapshot,content:"old stale"}});
    sendFrameMessage(frame, {type:"echo/response",id:nextRequest.id,instanceId:"native-instance-2",ok:true,value:snapshot});
    await expect(next).resolves.toEqual(snapshot);
  });

  it("fails versioned operations promptly when the connected IDE did not advertise their capability", async () => {
    const ref = createRef<TheiaIdeFrameHandle>();
    render(<TheiaIdeFrame ref={ref} root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    sendFrameMessage(frame, {type:"echo/ready",capabilities:[]});
    await expect(ref.current!.getDocument("/repo/a.ts")).rejects.toThrow(/能力|不支持|重新构建/);
    expect(postMessage.mock.calls.some(([message]) => (message as {type?:string}).type === "echo/get-document")).toBe(false);
  });

  it("waits for the native run-cancellation ACK instead of marking an in-flight write stopped immediately", async () => {
    render(<TheiaIdeFrame root="/repo" onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    sendFrameMessage(frame, {type:"echo/ready"});
    let acknowledged = false;
    const pending = workspaceEditor("/repo")!.cancelRun!("active-run").then(value => {acknowledged = true;return value;});
    const request = postMessage.mock.calls.map(([message]) => message as Record<string,unknown>).find(message => message.type === "echo/cancel-run")!;
    expect(request).toEqual(expect.objectContaining({runId:"active-run",instanceId:INSTANCE}));
    await Promise.resolve(); expect(acknowledged).toBe(false);
    sendFrameMessage(frame, {type:"echo/response",id:request.id,ok:true,value:true});
    await expect(pending).resolves.toBe(true); expect(acknowledged).toBe(true);
  });

  it("routes native Tab to the configured model without creating a coding task, and cancellation suppresses late text", async () => {
    render(<TheiaIdeFrame root="/repo" modelId="existing-model" completionEnabled onBeforeMutation={vi.fn()} onAfterMutation={vi.fn()} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    sendFrameMessage(frame, {type:"echo/ready"});
    expect(postMessage).toHaveBeenCalledWith(expect.objectContaining({type:"echo/set-inline-completion",enabled:true,instanceId:INSTANCE}), new URL(frame.src).origin);
    let complete!: (result: unknown) => void;
    invoke.mockImplementationOnce(command => command === "coding_inline_complete"
      ? new Promise(resolve => { complete = resolve; }) : Promise.resolve(undefined));
    sendFrameMessage(frame, {type:"echo/inline-completion",id:"inline-message",requestId:"completion-1",workspace:"/repo",path:"/repo/a.ts",uri:snapshot.uri,
      version:3,prefix:"before cursor",suffix:"after cursor",language:"typescript"});
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_inline_complete", {request:expect.objectContaining({requestId:"completion-1",modelId:"existing-model",workspace:"/repo",documentVersion:3})}));
    sendFrameMessage(frame, {type:"echo/cancel-inline-completion",requestId:"completion-1"});
    await act(async () => { complete({text:"stale suggestion",requestId:"completion-1",documentVersion:3,modelId:"existing-model",elapsedMs:40,inputTokens:5,outputTokens:2,cached:false}); });
    expect(invoke).toHaveBeenCalledWith("coding_inline_cancel", {requestId:"completion-1"});
    expect(invoke.mock.calls.some(([command]) => command.includes("task_create") || command.includes("prompt_send"))).toBe(false);
    expect(postMessage.mock.calls.some(([message]) => (message as {id?:string}).id === "inline-message")).toBe(false);
  });
});
