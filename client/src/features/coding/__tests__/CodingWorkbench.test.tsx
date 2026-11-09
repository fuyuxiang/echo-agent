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
  agentResolvePlanApproval: vi.fn(async () => true),
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

import { CodingWorkbench } from "../CodingWorkbench";
import { codingTaskContextDraftKey, codingTaskDraftKey } from "../lib/task-draft-key";
import type { CodingTask } from "../lib/types";
import { useAiDraftStore } from "../store/ai-draft-store";
import { useTaskStore } from "../store/task-store";
import { SidebarLayoutContext, useAppSidebarLayout } from "@/lib/sidebar-layout";
import { useMessageQueueStore } from "@/stores/message-queue-store";
import { useSessionStore, type ChatMessage } from "@/stores/session-store";
import { codingQueueId } from "../lib/coding-followup";
import { useCodingRuntimeStore } from "../lib/coding-task-runtime";

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

describe("Theia workbench", () => {
  const bridgeToken = (frame: HTMLIFrameElement): string =>
    (JSON.parse(frame.name.slice("echo-embed:".length)) as { bridgeToken: string }).bridgeToken;
  beforeEach(() => {
    useCodingRuntimeStore.setState({documentCancellationReady:true,documentCancellations:{},stopping:{}});
    window.history.replaceState({}, "", "/");
    window.localStorage.removeItem("echo-agent-panel-width");
    window.localStorage.removeItem("echo-coding-intent-v1:/repo:verification-task");
    window.localStorage.removeItem("echo-coding-plan-v1:/repo:verification-task");
    invoke.mockReset();
    invoke.mockImplementation(async (command: string, args?: unknown): Promise<unknown> => {
    if (command === "coding_document_editor_attach") return (args as {root:string} | undefined)?.root;
    if (command === "coding_verification_config_get") return {checks:[]};
    if (["coding_checkpoint_list","coding_git_commit_history","coding_review_list"].includes(command)) return [];
      if (["coding_task_list", "coding_verification_detect", "coding_isolation_list"].includes(command)) return [];
      if (command === "coding_theia_start") {
        return { url: "http://127.0.0.1:41773/", embedToken: "test-embed-token" };
      }
      return null;
    });
    useTaskStore.setState({
      root: "",
      summaries: [],
      task: null,
      changeSet: null,
      verifications: [],
      problems: [],
      orchestrator: null,
      loading: false,
      error: null,
    });
    useAiDraftStore.getState().clear();
    useMessageQueueStore.setState({queues:{}});
  });

  it.each(["", "/repo"])("restores the sidebar from the coding header and returns focus to its toolbar (%s)", async cwd => {
    function Shell() {
      const layout = useAppSidebarLayout();
      return <SidebarLayoutContext.Provider value={layout.context}>
        <div ref={layout.bodyRef}>
          <aside className="sidebar" id="app-sidebar" hidden={layout.collapsed}>
            <button aria-label="收起侧边栏" onClick={layout.context.toggleSidebar}>收起</button>
          </aside>
          <CodingWorkbench cwd={cwd} models={[]} />
        </div>
      </SidebarLayoutContext.Provider>;
    }
    const { container } = render(<Shell />);
    expect(container.querySelector('.coding-workbench__topbar button[aria-label="收起侧边栏"]')).toBeNull();
    const collapse = screen.getByRole("button", { name: "收起侧边栏" });
    collapse.focus();
    fireEvent.click(collapse);
    const expand = screen.getByRole("button", { name: "展开侧边栏" });
    expect(expand).toHaveAttribute("aria-expanded", "false");
    expect(expand).toHaveFocus();
    fireEvent.click(expand);
    expect(screen.queryByRole("button", { name: "展开侧边栏" })).not.toBeInTheDocument();
    expect(collapse).toHaveFocus();
    if (cwd) expect(await screen.findByTitle("Echo Code IDE")).toBeInTheDocument();
    else expect(screen.getByText("打开代码库开始开发")).toBeInTheDocument();
  });

  it("keeps the Agent pane beside Theia and allows resizing independently", async () => {
    const { container } = render(<CodingWorkbench cwd="/repo" models={[]} />);

    expect(await screen.findByTitle("Echo Code IDE")).toHaveAttribute(
      "name",
      expect.stringContaining("test-embed-token"),
    );
    expect((screen.getByTitle("Echo Code IDE") as HTMLIFrameElement).src).not.toContain("echoEmbedToken");
    expect(invoke).toHaveBeenCalledWith("coding_theia_start", { root: "/repo" });
    const agent = container.querySelector(".coding-workbench > .echo-theia-agent") as HTMLElement;
    expect(agent).toBeInTheDocument();
    expect(container.querySelector(".echo-theia-workspace > .echo-theia-agent")).not.toBeInTheDocument();
    const splitter = screen.getByRole("separator", { name: "调整 Agent 面板宽度" });
    expect(splitter).toHaveAttribute("aria-valuenow", "410");
    expect(container.querySelector(".coding-workbench--agent-closed")).not.toBeInTheDocument();
    fireEvent.keyDown(splitter, { key: "ArrowLeft" });
    expect(splitter).toHaveAttribute("aria-valuenow", "430");
    const workbench = container.querySelector(".coding-workbench") as HTMLElement;
    vi.spyOn(workbench, "getBoundingClientRect").mockReturnValue({
      width: 1200, right: 1200, left: 0, top: 0, bottom: 800, height: 800, x: 0, y: 0,
      toJSON: () => ({}),
    });
    const pointer = (type: string, clientX: number) => {
      const event = new Event(type, { bubbles: true });
      Object.assign(event, { clientX, pointerId: 1, button: 0, buttons: type === "pointerup" ? 0 : 1, isPrimary: true });
      return event;
    };
    fireEvent(splitter, pointer("pointerdown", 770));
    fireEvent(window, pointer("pointermove", 700));
    fireEvent(window, pointer("pointerup", 700));
    expect(splitter).toHaveAttribute("aria-valuenow", "500");
    expect(window.localStorage.getItem("echo-agent-panel-width")).toBe("500");
    vi.spyOn(workbench, "getBoundingClientRect").mockReturnValue({
      width: 700, right: 700, left: 0, top: 0, bottom: 800, height: 800, x: 0, y: 0,
      toJSON: () => ({}),
    });
    fireEvent.doubleClick(splitter);
    // Reserve the splitter's 8px as well as the IDE's 320px reading width.
    expect(splitter).toHaveAttribute("aria-valuemax", "372");
    expect(splitter).toHaveAttribute("aria-valuenow", "372");
    expect(screen.queryByRole("tablist", { name: "开发任务面板" })).not.toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "任务描述" })).toBeInTheDocument();
    expect(agent.lastElementChild).toHaveClass("echo-theia-agent__composer");
    fireEvent.click(screen.getByRole("button", { name: "收起 Agent 面板" }));
    expect(container.querySelector(".coding-workbench--agent-closed")).toBeInTheDocument();
  });

  it("blocks an empty folder during verification without opening a follow-up round", async () => {
    render(<CodingWorkbench cwd="/repo" models={[]} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    const url = new URL(frame.src);
    act(() => {
      useTaskStore.setState({ root: "/repo", task: verificationTask() });
    });
    await waitFor(() => {
      act(() => {
        window.dispatchEvent(new MessageEvent("message", {
          origin: url.origin,
          source: frame.contentWindow,
          data: {
            type: "echo/before-mutation", id: "folder", operation: "createFolder",
            paths: ["/repo/new-folder"], token: bridgeToken(frame),
          },
        }));
      });
      expect(postMessage).toHaveBeenCalledWith(
        expect.objectContaining({ id: "folder", ok: false }), url.origin,
      );
    });
    expect(invoke).not.toHaveBeenCalledWith("coding_task_begin_followup", expect.anything());
  });

  it("keeps the task composer available while reviewing changes and verification", async () => {
    const user = userEvent.setup();
    useTaskStore.setState({ root: "/repo", task: verificationTask({ phase: "delivered", sessionId: "session-1" }) });
    const { container } = render(<CodingWorkbench cwd="/repo" sessionId="session-1" models={[{ id: "model-1" }]} defaultModelId="model-1" />);
    await screen.findByTitle("Echo Code IDE");
    const composer = container.querySelector(".echo-theia-agent__composer");
    const textbox = await screen.findByRole("textbox", { name: "补充开发要求" });
    fireEvent.change(textbox, { target: { value: "请处理边界情况" } });
    await user.click(screen.getByRole("button", { name: "变更" }));
    expect(composer).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "补充开发要求" })).toHaveValue("请处理边界情况");
    await user.click(screen.getByRole("button", { name: "检查" }));
    expect(screen.getByRole("textbox", { name: "补充开发要求" })).toHaveValue("请处理边界情况");
  });

  it("adopts only the public completed Plan response, never the previous Ask or its reasoning", async () => {
    const sessionId="plan-result-session";
    useTaskStore.setState({root:"/repo",task:verificationTask({phase:"implementing",sessionId})});
    localStorage.setItem("echo-coding-intent-v1:/repo:verification-task","plan");
    localStorage.removeItem("echo-coding-plan-v1:/repo:verification-task");
    useSessionStore.getState().setSession(sessionId);
    const ask={id:"ask",role:"user" as const,complete:true,agentText:"[回声代码 · ask]\n解释",parts:[{kind:"text" as const,text:"解释"}]};
    const previous={id:"ask-answer",role:"assistant" as const,complete:true,parts:[{kind:"text" as const,text:"<think>Previous internal process</think>Ask answer"}]};
    const setMessages=(messages: ChatMessage[])=>act(()=>useSessionStore.setState(state=>({transcripts:{...state.transcripts,[sessionId]:{...state.transcripts[sessionId],messages}}})));
    setMessages([ask,previous]);
    render(<CodingWorkbench cwd="/repo" sessionId={sessionId} models={[]} />);
    const field=await screen.findByRole("textbox",{name:"编辑开发计划"});
    expect(field).toHaveValue("");
    expect(screen.getAllByRole("textbox",{name:"编辑开发计划"})).toHaveLength(1);
    const plan={...ask,id:"plan",agentText:"[回声代码 · plan · 继续当前会话]\n规划"};
    setMessages([ask,previous,plan,{...previous,id:"planning",complete:false,parts:[{kind:"text",text:"<think>Streaming process"}]}]);
    expect(field).toHaveValue("");
    setMessages([ask,previous,plan,{...previous,id:"planning",parts:[{kind:"text",text:"<think>Plan internal process</think>1. 读取\n2. 修改并验证"}]}]);
    await waitFor(()=>expect(field).toHaveValue("1. 读取\n2. 修改并验证"));
    expect(screen.getAllByRole("textbox",{name:"编辑开发计划"})).toHaveLength(1);
    fireEvent.change(field,{target:{value:"用户修订的最终计划"}});
    setMessages([ask,previous,plan,{...previous,id:"planning",parts:[{kind:"text",text:"迟到的新方案"}]}]);
    expect(field).toHaveValue("用户修订的最终计划");
  });

  it.each(["plan", "agent"].flatMap(intent => ["cancelled", "error", "max_tokens"].map(stopReason => ({ intent, stopReason }))))(
    "does not offer an interrupted $stopReason Plan when the next intent is $intent", async ({ intent, stopReason }) => {
    const sessionId = "interrupted-plan-session";
    useTaskStore.setState({root:"/repo", task:verificationTask({phase:"stopped", sessionId})});
    localStorage.setItem("echo-coding-intent-v1:/repo:verification-task", intent);
    localStorage.setItem("echo-coding-plan-v1:/repo:verification-task", JSON.stringify({
      kind:"echo-coding-plan-draft", text:"Stale automatic plan", edited:false,
    }));
    useSessionStore.getState().setSession(sessionId);
    const messages: ChatMessage[] = [
      {id:"plan",role:"user",complete:true,agentText:"[回声代码 · plan]\n规划",parts:[{kind:"text",text:"规划"}]},
      {id:"partial",role:"assistant",complete:true,stopReason,parts:[{kind:"text",text:"1. 修改数据库\n2. ",phase:"partial_answer"}]},
    ];
    useSessionStore.setState(state => ({transcripts:{...state.transcripts,[sessionId]:{...state.transcripts[sessionId],messages}}}));
    render(<CodingWorkbench cwd="/repo" sessionId={sessionId} models={[]} />);
    if (intent !== "plan") {
      fireEvent.click(screen.getByText("更多", { selector: ".coding-session-actions summary" }));
      fireEvent.click(screen.getByRole("button", { name: "会话计划" }));
    }
    expect(await screen.findByRole("textbox",{name:"编辑开发计划"})).toHaveValue("");
    expect(screen.getByText(/本轮计划未完整生成/)).toBeInTheDocument();
    expect(screen.getByRole("button",{name:"执行最终计划"})).toBeDisabled();
    expect(screen.getByRole("button",{name:"保存到工程…"})).toBeDisabled();
  });

  it("disables queued manual sends throughout verification and explains when they become available", async () => {
    const task=verificationTask({sessionId:"queue-session",modelId:"model-1"});
    useTaskStore.setState({root:"/repo",task});
    useSessionStore.getState().setSession("queue-session");
    useMessageQueueStore.getState().enqueue(codingQueueId("/repo",task.id,task.sessionId)!,"验证后处理边界",[]);
    render(<CodingWorkbench cwd="/repo" sessionId="queue-session" apiReady models={[{id:"model-1"}]} defaultModelId="model-1" />);
    const send=await screen.findByRole("button",{name:"正在验证，完成后可发送"});
    expect(send).toBeDisabled();
    expect(invoke.mock.calls.some(([command])=>command==="coding_task_begin_followup")).toBe(false);
    act(()=>useTaskStore.setState({task:{...task,phase:"delivered"}}));
    expect(await screen.findByRole("button",{name:"立即发送"})).toBeEnabled();
  });

  it("keeps native plan approval reachable while ordinary coding input and queue sends wait", async () => {
    const task=verificationTask({sessionId:"plan-session",modelId:"model-1",phase:"implementing"});
    useTaskStore.setState({root:"/repo",task});
    useSessionStore.getState().setSession("plan-session");
    useSessionStore.getState().requestPlanApproval({requestId:"plan",sessionId:"plan-session",toolCallId:"plan-tool",planContent:"先检查接口契约，再实施修改"});
    render(<CodingWorkbench cwd="/repo" sessionId="plan-session" apiReady models={[{id:"model-1"}]} defaultModelId="model-1" />);
    expect(await screen.findByRole("group",{name:"计划审批"})).toBeInTheDocument();
    expect(screen.getByRole("textbox",{name:"补充开发要求"})).toBeDisabled();
    await userEvent.click(screen.getByRole("button",{name:"批准执行"}));
    await waitFor(()=>expect(screen.queryByRole("group",{name:"计划审批"})).toBeNull());
    expect(screen.getByRole("textbox",{name:"补充开发要求"})).toBeEnabled();
  });

  it("opens the delivery report from a named action in the default IDE", async () => {
    useTaskStore.setState({ root: "/repo", task: verificationTask({ phase: "delivered" }) });
    render(<CodingWorkbench cwd="/repo" models={[]} />);
    await screen.findByTitle("Echo Code IDE");
    await userEvent.click(screen.getByText("更多"));
    await userEvent.click(screen.getByRole("button", { name: "导出摘要" }));
    expect(screen.getByRole("region", { name: "交付报告" })).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("coding_delivery_report", { root: "/repo", taskId: "verification-task" });
  });

  it("opens another folder while a coding task continues in the background", async () => {
    const onSelectWorkspace = vi.fn();
    const onToast = vi.fn();
    useTaskStore.setState({ root: "/repo", task: verificationTask({ phase: "implementing" }) });
    render(<CodingWorkbench cwd="/repo" models={[]} onSelectWorkspace={onSelectWorkspace} onToast={onToast} />);
    await screen.findByTitle("Echo Code IDE");
    await userEvent.click(screen.getByRole("button", { name: "切换项目" }));
    await userEvent.click(screen.getByRole("menuitem", { name: "打开其他文件夹…" }));
    expect(onSelectWorkspace).toHaveBeenCalledWith("/picked");
    expect(onToast).not.toHaveBeenCalledWith(expect.stringContaining("请先停止任务"));
  });

  it("creates an isolated Git worktree for a parallel task in the same project", async () => {
    const onSelectWorkspace = vi.fn();
    const isolated = { root: "/managed/parallel", sourceRoot: "/repo", baseHead: "abc123", taskIds: [], sessionIds: [] };
    const firstTask = verificationTask({ id: "parallel-task", phase: "idle" });
    invoke.mockImplementation(async (command: string, args?: unknown): Promise<unknown> => {
    const root = (args as { root?: string } | undefined)?.root;
    if (command === "coding_document_editor_attach") return (args as {root:string} | undefined)?.root;
    if (command === "coding_verification_config_get") return {checks:[]};
    if (["coding_checkpoint_list","coding_git_commit_history","coding_review_list"].includes(command)) return [];
      if (command === "coding_task_list") return root === "/repo" ? [{ id: "running-task", name: "正在开发", phase: "implementing", updatedAt: "now" }] : [];
      if (command === "coding_verification_detect") return [];
      if (command === "coding_theia_start") return { url: "http://127.0.0.1:41773/", embedToken: "test-embed-token" };
      if (command === "coding_isolation_create") return isolated;
      if (command === "coding_isolation_info") return root === isolated.root ? isolated : null;
      if (command === "coding_task_create" || command === "coding_task_get") return firstTask;
      return null;
    });
    const onStartRun = vi.fn<NonNullable<React.ComponentProps<typeof CodingWorkbench>["onStartRun"]>>(
      async (_workspace, _requirement, _modelId, _contextPaths, bindSession) => {
        await bindSession?.("parallel-session");
        return "parallel-session";
      },
    );
    const { rerender } = render(<CodingWorkbench
      cwd="/repo"
      models={[{ id: "model-1" }]}
      defaultModelId="model-1"
      apiReady
      onStartRun={onStartRun}
      onSelectWorkspace={onSelectWorkspace}
    />);
    const readyEditor = async (workspace: string) => {
      const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
      const origin = new URL(frame.src).origin;
      const token = bridgeToken(frame);
      const send = (data: object) => act(() => window.dispatchEvent(new MessageEvent("message", {origin, source: frame.contentWindow, data: {...data, token, instanceId:"parallel-editor"}})));
      vi.spyOn(frame.contentWindow!, "postMessage").mockImplementation((data: unknown) => {
        const message = data as {type: string; id: string};
        if (message.type === "echo/get-dirty") queueMicrotask(() => send({type: "echo/response", id: message.id, ok: true, value: 0}));
        if (message.type === "echo/get-context") queueMicrotask(() => send({type:"echo/response",id:message.id,ok:true,value:{workspace,document:null,openDocuments:[],diagnostics:[],diagnosticsTruncated:false,terminal:null,truncated:false}}));
      });
      send({type: "echo/ready",protocolVersion:1,capabilities:["document-snapshots"]});
      send({type: "echo/dirty-state", count: 0});
    };
    await readyEditor("/repo");
    expect(await screen.findByText("当前项目有任务正在执行；提交新任务时会创建隔离工作树。")).toBeInTheDocument();
    act(() => useAiDraftStore.getState().setDraft({ prompt: "", contextPaths: ["src/orders.ts"], source: "context-menu" }));
    expect(await screen.findByText("orders.ts")).toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox", { name: "任务描述" }), { target: { value: "实现新的订单流程" } });
    await userEvent.click(screen.getByRole("button", { name: "开始 Agent 任务" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_isolation_create", { root: "/repo" }));
    await waitFor(() => expect(onSelectWorkspace).toHaveBeenCalledWith("/managed/parallel"));
    expect(window.localStorage.getItem(codingTaskDraftKey("/managed/parallel"))).toBe("实现新的订单流程");
    expect(window.localStorage.getItem(codingTaskContextDraftKey("/managed/parallel"))).toBe('["src/orders.ts"]');
    expect(invoke).not.toHaveBeenCalledWith("coding_task_create", expect.anything());

    rerender(<CodingWorkbench
      cwd="/managed/parallel"
      models={[{ id: "model-1" }]}
      defaultModelId="model-1"
      apiReady
      onStartRun={onStartRun}
      onSelectWorkspace={onSelectWorkspace}
    />);
    expect(await screen.findByRole("textbox", { name: "任务描述" })).toHaveValue("实现新的订单流程");
    expect(await screen.findByText("orders.ts")).toBeInTheDocument();
    expect(window.localStorage.getItem(codingTaskContextDraftKey("/managed/parallel"))).toBeNull();
    await readyEditor(isolated.root);
    const start = screen.getByRole("button", { name: "开始 Agent 任务" });
    fireEvent.click(start);
    fireEvent.click(start);
    await waitFor(() => expect(onStartRun).toHaveBeenCalledTimes(1));
    expect(onStartRun).toHaveBeenCalledWith(isolated.root, "实现新的订单流程", "model-1", ["src/orders.ts"], expect.any(Function), expect.any(String), expect.objectContaining({ workspace: isolated.root, taskId: firstTask.id }));
    expect(invoke).toHaveBeenCalledWith("coding_task_bind_runtime", expect.objectContaining({ root: isolated.root, taskId: firstTask.id, sessionId: "parallel-session" }));
    expect(invoke.mock.calls.filter(([command]) => command === "coding_isolation_create")).toHaveLength(1);
    expect(invoke.mock.calls.filter(([command]) => command === "coding_task_create")).toHaveLength(1);
  });

  it("shows the exact source branch, prior commits and affected files before applying an isolation range", async () => {
    const isolated = { root: "/managed/parallel", sourceRoot: "/repo", baseHead: "basehead" };
    const task = verificationTask({ id: "parallel-task", phase: "delivered" });
    const changeSet = { taskId: task.id, changes: [], reviewedFiles: [], createdAt: task.createdAt, committedHash: "taskhash" };
    const plan = {
      sourceRoot: isolated.sourceRoot,
      sourceHead: isolated.baseHead,
      sourceRef: "refs/heads/main",
      commits: [
        { hash: "manualhash", subject: "manual predecessor", files: ["manual.txt"] },
        { hash: "taskhash", subject: "task result", files: ["result.txt"] },
      ],
    };
    const ordinary = invoke.getMockImplementation()!;
    invoke.mockImplementation(async (command, args) => {
      if (command === "coding_isolation_info") return isolated;
      if (command === "coding_task_get") return task;
      if (command === "coding_orchestrator_state") return { task, problems: [], repairRounds: [] };
      if (command === "coding_changeset_get") return changeSet;
      if (command === "coding_isolation_integration_preview") return plan;
      if (command === "coding_isolation_integrate") return { ...isolated, integratedHash: "updated" };
      return ordinary(command, args);
    });
    useTaskStore.setState({ root: isolated.root, task, changeSet });
    render(<CodingWorkbench cwd={isolated.root} models={[]} />);
    await userEvent.click(await screen.findByRole("button", { name: "变更" }));
    const applyButton = await screen.findByRole("button", { name: "应用到原项目" });
    await waitFor(() => expect(applyButton).toBeEnabled());
    await userEvent.click(applyButton);
    expect(await screen.findByRole("dialog", { name: "应用到原项目" })).toBeInTheDocument();
    expect(screen.getByText("目标：/repo · main · basehead")).toBeInTheDocument();
    expect(screen.getByText("manual predecessor", { exact: false })).toBeInTheDocument();
    expect(screen.getByText("manual.txt")).toBeInTheDocument();
    expect(screen.getByText("result.txt")).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith("coding_isolation_integrate", expect.anything());
    await userEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(invoke).not.toHaveBeenCalledWith("coding_isolation_integrate", expect.anything());
    await userEvent.click(screen.getByRole("button", { name: "应用到原项目" }));
    await userEvent.click(await screen.findByRole("button", { name: "应用所列提交" }));
    expect(invoke).toHaveBeenCalledWith("coding_isolation_integrate", { root: isolated.root, taskId: task.id, expected: plan });
  });

  it("saves dirty Theia editors before switching projects", async () => {
    const onSelectWorkspace = vi.fn();
    render(<CodingWorkbench cwd="/repo" codingWorkspaces={[{ cwd: "/next" }]} models={[]} onSelectWorkspace={onSelectWorkspace} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const src = new URL(frame.src);
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    const send = (data: Record<string, unknown>) => {
      act(() => {
        window.dispatchEvent(new MessageEvent("message", {
          origin: src.origin,
          source: frame.contentWindow,
          data: { ...data, token: bridgeToken(frame) },
        }));
      });
    };
    send({ type: "echo/ready" });
    send({ type: "echo/dirty-state", count: 2 });
    expect(screen.getByLabelText("2 个未保存文件")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "切换项目" }));
    await userEvent.click(screen.getByRole("menuitem", { name: "切换到项目 next" }));
    await waitFor(() => expect(postMessage).toHaveBeenCalledWith(expect.objectContaining({ type: "echo/get-dirty" }), src.origin));
    expect(onSelectWorkspace).not.toHaveBeenCalled();
    const dirtyRequest = postMessage.mock.calls.find(([message]) => (message as { type?: string }).type === "echo/get-dirty")![0] as { id: string };
    send({ type: "echo/response", id: dirtyRequest.id, ok: true, value: 2 });

    await userEvent.click(await screen.findByRole("button", { name: "保存并继续" }));
    await waitFor(() => expect(postMessage).toHaveBeenCalledWith(expect.objectContaining({ type: "echo/save-all" }), src.origin));
    expect(onSelectWorkspace).not.toHaveBeenCalled();
    const saveRequest = postMessage.mock.calls.find(([message]) => (message as { type?: string }).type === "echo/save-all")![0] as { id: string };
    send({ type: "echo/response", id: saveRequest.id, ok: true, value: 0 });
    await waitFor(() => expect(onSelectWorkspace).toHaveBeenCalledWith("/next"));
  });

  it("keeps worktree management reachable and requires explicit workspace/model consent for Tab", async () => {
    const user = userEvent.setup();
    const originalInvoke = invoke.getMockImplementation()!;
    let granted = false;
    invoke.mockImplementation(async (command, args) => {
      if (command !== "coding_inline_consent") return originalInvoke(command, args);
      const input = args as {workspace: string; enabled?: boolean};
      if (input.enabled !== undefined) granted = input.enabled;
      return {enabled: input.workspace === "/repo" && granted, destination:"https://fixture.invalid", identity:"model-target"};
    });
    // Older builds automatically persisted "true"; it is never proof of opt-in.
    localStorage.setItem("echo-coding-tab-enabled", "true");
    const {rerender} = render(<CodingWorkbench cwd="/repo" models={[{id:"model",label:"测试模型"}]} defaultModelId="model" />);
    await screen.findByTitle("Echo Code IDE");
    await user.click(screen.getByRole("button", { name: "切换项目" }));
    await user.click(screen.getByRole("menuitem", { name: "管理工作树…" }));
    expect(screen.getByRole("region", { name: "受管理的工作树" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "关闭工作树管理" }));
    await user.click(screen.getByRole("button", { name: "编辑器选项" }));
    const completion = screen.getByRole("menuitemradio", { name: /Tab 代码补全/ });
    expect(completion).toHaveAttribute("aria-checked", "false");
    await user.click(completion);
    expect(await screen.findByText(/fixture\.invalid/)).toHaveTextContent("未保存");
    expect(granted).toBe(false);
    await user.click(screen.getByRole("button", {name:"开启 Tab 补全"}));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_inline_consent", {
      workspace: "/repo", modelId: "model", enabled: true, expectedIdentity: "model-target",
    }));
    await user.click(screen.getByRole("button", { name: "编辑器选项" }));
    expect(screen.getByRole("menuitemradio", { name: /Tab 代码补全/ })).toHaveAttribute("aria-checked", "true");
    rerender(<CodingWorkbench cwd="/other" models={[{id:"model",label:"测试模型"}]} defaultModelId="model" />);
    await user.click(screen.getByRole("button", { name: "编辑器选项" }));
    expect(screen.getByRole("menuitemradio", { name: /Tab 代码补全/ })).toHaveAttribute("aria-checked", "false");
    localStorage.removeItem("echo-coding-tab-enabled");
  });

  it("keeps preview controls compact and lets the editor reclaim the Agent space", async () => {
    const user = userEvent.setup();
    const { container } = render(<CodingWorkbench cwd="/repo" models={[]} />);
    await screen.findByTitle("Echo Code IDE");

    expect(screen.queryByRole("textbox", { name: "开发服务地址" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "开发服务预览" }));
    expect(screen.getByRole("textbox", { name: "开发服务地址" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("textbox", { name: "开发服务地址" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "收起 Agent 面板" }));
    expect(container.querySelector(".coding-workbench--agent-closed")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "展开 Agent 面板" }));
    expect(container.querySelector(".coding-workbench--agent-closed")).not.toBeInTheDocument();
  });
});
