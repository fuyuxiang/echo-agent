/** Chat pause and continuation share the trusted send/cancel path.
 * All modes share turn feedback and the ordinary Composer; natural pause
 * still cancels desktop execution between model turns.
 */
import { afterEach, describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, act, within } from "@testing-library/react";
import { useMessageQueueStore } from "@/stores/message-queue-store";
import type { Plan } from "@/lib/types";

const automationState = vi.hoisted(() => ({ status: null as import("@/lib/automation-client").AutomationStatus | null,
  approvals: [] as import("@/lib/automation-client").AutomationApproval[], error: null as string | null }));
vi.mock("@/lib/use-session-automation", () => ({ useSessionAutomation: (id: string | null) => ({
  status: automationState.status?.sessionId === id ? automationState.status : null,
  approvals: automationState.approvals.filter(item => item.sessionId === id), error: automationState.error,
  isActive: () => true, refresh: vi.fn(async () => {}), reconnect: vi.fn(), setStatus: vi.fn(), removeApproval: vi.fn(),
}) }));
beforeEach(() => { automationState.status = null; automationState.approvals = []; automationState.error = null; });
const artifactMocks = vi.hoisted(() => ({ load: vi.fn(async () => [] as import("@/lib/session-artifacts").SessionArtifact[]) }));
const workspaceMock = vi.hoisted(() => ({ missing: false }));
vi.mock("@/lib/use-session-workspace", async original => {
  const actual = await original<typeof import("@/lib/use-session-workspace")>();
  return {useSessionWorkspace: (...args: Parameters<typeof actual.useSessionWorkspace>) => {
    const workspace = actual.useSessionWorkspace(...args);
    return workspaceMock.missing ? {...workspace,available:false,cwd:"/missing",value:{cwd:"/missing",status:"missing" as const}} : workspace;
  }};
});
vi.mock("@/lib/artifact-catalog", async (importOriginal) => ({
  ...await importOriginal<typeof import("@/lib/artifact-catalog")>(),
  loadSessionDocumentArtifacts: artifactMocks.load,
}));

// session-store mock:可控的 streaming / sessionId / messages。
let storeState: {
  messages: unknown[];
  streaming: boolean;
  sendNowPending?: boolean;
  streamingMessageId: string | null;
  error: string | null;
  runtimeErrorContext?: {sessionId: string; promptId: string} | null;
  plan: Plan | null;
  planRevisionDirty?: boolean;
  planApproval?: {requestId: string; sessionId: string; toolCallId: string; planContent?: string};
  transcripts?: Record<string, { planRevisionDirty: boolean }>;
  sessionId: string | null;
  agentMode: "default" | "browser_use" | "computer_use";
  control?: {
    action: "pause" | "stop";
    phase: "pausing" | "paused" | "stopping" | "stopped";
    requestedAt: number;
  };
  resumeSession: (sessionId: string) => void;
  setAgentMode: (mode: "default" | "browser_use" | "computer_use") => void;
  discardMessagesFrom: ReturnType<typeof vi.fn>;
} = {
  messages: [],
  streaming: false,
  streamingMessageId: null,
  error: null,
  plan: null,
  sessionId: "s1",
  agentMode: "default",
  control: undefined,
  resumeSession: () => {
    storeState = { ...storeState, control: undefined };
  },
  setAgentMode: (agentMode) => {
    storeState = { ...storeState, agentMode };
  },
  discardMessagesFrom: vi.fn(),
};
vi.mock("@/stores/session-store", () => {
  const useSessionStore = Object.assign(
    (sel: (s: typeof storeState) => unknown) => sel(storeState),
    { getState: () => storeState },
  );
  return { useSessionStore };
});
vi.mock("@/stores/sessions-store", () => {
  const value = {
    drafts: {} as Record<string, string>,
    setDraft: () => {},
    independent: [] as unknown[],
    upsert: vi.fn(),
  };
  const useSessionsStore = Object.assign(
    (sel: (s: typeof value) => unknown) => sel(value),
    { getState: () => value },
  );
  return { useSessionsStore, HOME_DRAFT_KEY: "home" };
});
vi.mock("@/lib/agent-client", async () => {
  // 用空实现铺满所有被引用的导出,避免「No export defined」。
  const mod: Record<string, unknown> = {};
  const handler = () => undefined;
  const asyncEmpty = async () => undefined;
  const asyncArr = async () => [];
  for (const name of [
    "rewindExecute", "rewindPoints", "agentInit", "agentNewSession", "agentSend",
    "agentCancel", "agentLoadSession", "agentListAllSessions", "agentListSessions", "agentListWorkspaces",
    "agentRenameSession", "agentSetModel", "agentSetSessionExpert", "agentAuthStatus",
    "providersList", "flattenModels", "notificationAppend", "subscribeAgentEvents",
    "commandsList", "promptHistory", "tasksList", "taskKill", "teamSnapshot", "permissionList",
    "permissionSave", "permissionModeGet", "permissionModeSet", "memoryList",
    "memoryGet", "memorySave", "memoryAppend", "memoryDelete", "memoryClearSessionSummaries", "memoryRewrite", "memoryFlush", "memoryDream",
    "memoryConfigGet", "memoryConfigSave",
    "sessionSearch", "sessionFork", "agentsList", "agentsGet", "agentsSave",
    "agentsDelete", "agentsTemplate", "mcpList", "mcpUpsert", "mcpDelete",
    "mcpToggle", "mcpConfigPath", "mcpConfigRead", "mcpConfigSave", "mcpAuthTrigger",
    "mcpAuthStatus", "togglePlanMode", "setCodingMode", "internalReload", "automationsSnapshot",
    "automationsSave", "automationsDelete", "automationsSetStatus", "automationsRun",
    "automationRecordsArchive", "automationRecordsDelete",
    "agentsDefaultsGet", "agentsDefaultsSave",
    "pluginsList", "pluginsAction", "notificationList", "notificationMarkRead", "notificationMarkAllRead",
    "notificationClear", "exportTextFile", "openUrl", "folderTrustRespond",
    "agentDeleteSession", "agentSetSessionPinned", "agentSetSessionArchived",
    "agentSessionInfo", "agentSessionUsage", "agentResolvePermission",
    "agentResolveQuestion", "agentClearSessionExpert", "expertAvatar",
    "skillsList", "skillsAdd", "skillsRemove", "skillsToggle",
  ]) {
    mod[name] = name.startsWith("on") ? handler : asyncArr;
  }
  // 个别需要特定返回。
  mod.sessionFork = vi.fn(async () => "new-fork");
  mod.rewindExecute = vi.fn(asyncEmpty);
  mod.rewindPoints = vi.fn(asyncArr);
  mod.setCodingMode = vi.fn(asyncEmpty);
  mod.providersList = async () => ({ providers: [], models: [] });
  mod.flattenModels = () => [];
  mod.agentAuthStatus = async () => ({ ready: true, providers: [] });
  mod.subscribeAgentEvents = async () => () => {};
  mod.onTeamsChanged = async () => () => {};
  mod.permissionModeGet = async () => "ask";
  mod.notificationList = asyncArr;
  mod.agentsList = asyncArr;
  mod.mcpList = asyncArr;
  mod.tasksList = asyncArr;
  mod.commandsList = vi.fn(asyncArr);
  mod.promptHistory = asyncArr;
  return mod;
});

vi.mock("@/lib/automation-client", async (importOriginal) => ({
  ...await importOriginal<typeof import("@/lib/automation-client")>(),
  automationResume: vi.fn(async () => undefined),
  automationPause: vi.fn(async () => undefined),
}));
import { automationResume, automationPause } from "@/lib/automation-client";
import { ChatView } from "../ChatView";
import { ThemeProvider } from "../ThemeProvider";
import { rewindExecute, rewindPoints, sessionFork, setCodingMode, commandsList } from "@/lib/agent-client";
import { useSubagentStore } from "@/stores/subagent-store";

function workspaceItem(name: string) {
  const expand = screen.queryByRole("button", { name: "展开工作区" });
  if (expand) fireEvent.click(expand);
  const labels: Record<string, string> = {"文件变更":"变更", "任务产物":"成果", "网页预览":"网页", "文件.*工作目录":"文件", "执行计划":"计划", "管理任务团队":"团队", "运行与协作":"运行"};
  const target = labels[name] ?? name;
  if (["计划", "运行", "团队"].includes(target) && !screen.queryByRole("tab", { name: target })) fireEvent.click(screen.getByRole("tab", {name:"任务"}));
  fireEvent.click(screen.getByRole("tab", { name: target }));
}
function sessionItem(name: string) {
  if (name === "对话目录") {
    fireEvent.click(screen.getByRole("button", {name:/^对话目录，共/}));
    return;
  }
  fireEvent.click(screen.getByRole("button", { name: "任务操作" }));
  fireEvent.click(screen.getByRole("menuitem", { name }));
}
function replyItem(name: string) {
  fireEvent.click(screen.getByRole("button", { name }));
}

/** 用 ThemeProvider 包裹(ChatView 内的 MessageItem/Markdown 需要 useTheme)。 */
function renderChat() {
  return render(
    <ThemeProvider>
      <ChatView {...baseProps} />
    </ThemeProvider>,
  );
}

const baseProps = {
  onSend: vi.fn(),
  onPrepareRetry: vi.fn(async () => true),
  onRetrySend: vi.fn((_request: import("@/lib/message-retry").MessageRetrySendRequest): boolean | Promise<boolean> => true),
  onCancel: vi.fn(),
  modelId: "m1",
  onToast: vi.fn(),
};

const scrollIntoViewMock = vi.fn();

function setStore(patch: Partial<typeof storeState>) {
  storeState = { ...storeState, ...patch };
}

describe("ChatView pause/yield/resume 闭环", () => {
  beforeEach(() => {
    workspaceMock.missing = false;
    artifactMocks.load.mockReset().mockResolvedValue([]);
    useSubagentStore.setState({ bySession: {} });
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
      configurable: true,
      value: scrollIntoViewMock,
    });
    setStore({
      messages: [{ id: "u1", role: "user", complete: true, parts: [{ kind: "text", text: "hi" }] }],
      streaming: false,
      sendNowPending: false,
      streamingMessageId: null,
      error: null,
      runtimeErrorContext: null,
      plan: null,
      planApproval: undefined,
      planRevisionDirty: false,
      transcripts: {},
      sessionId: "s1",
      agentMode: "default",
      control: undefined,
    });
    vi.mocked(automationResume).mockReset().mockResolvedValue(undefined as never);
    vi.mocked(automationPause).mockReset().mockResolvedValue(undefined as never);
    baseProps.onSend.mockClear();
    baseProps.onPrepareRetry.mockClear().mockResolvedValue(true);
    baseProps.onRetrySend.mockClear().mockReturnValue(true);
    baseProps.onCancel.mockReset();
    baseProps.onToast.mockClear();
    storeState.discardMessagesFrom.mockClear();
    scrollIntoViewMock.mockClear();
    vi.mocked(rewindExecute).mockReset().mockResolvedValue({ targetPromptIndex: 0 });
    vi.mocked(rewindPoints).mockReset().mockResolvedValue([]);
    vi.mocked(setCodingMode).mockClear();
  });

  it("目录不可用时保留原回复，并禁用全部消息级重试", () => {
    workspaceMock.missing = true;
    setStore({messages:[
      {id:"u1",role:"user",complete:true,promptIndex:0,parts:[{kind:"text",text:"生成报告"}]},
      {id:"a1",role:"assistant",complete:true,stopReason:"end_turn",parts:[{kind:"text",text:"原有报告"}]},
    ]});
    renderChat();
    const retry = screen.getByRole("button", {name:"重新生成"});
    expect(retry).toBeDisabled();
    expect(retry).toHaveAttribute("title", expect.stringContaining("恢复本任务的工作目录"));
    fireEvent.click(retry);
    expect(baseProps.onPrepareRetry).not.toHaveBeenCalled();
    expect(rewindExecute).not.toHaveBeenCalled();
    expect(storeState.discardMessagesFrom).not.toHaveBeenCalled();
    expect(screen.getByText("原有报告")).toBeVisible();
  });

  it("目录不可用时不能通过暂停状态栏绕过发送限制，恢复后可继续", async () => {
    workspaceMock.missing = true;
    setStore({control:{action:"pause",phase:"paused",requestedAt:1}});
    const view = renderChat();
    const resume = screen.getByRole("button", {name:"继续任务"});
    expect(resume).toBeDisabled();
    fireEvent.click(resume);
    expect(baseProps.onSend).not.toHaveBeenCalled();
    workspaceMock.missing = false;
    view.rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    fireEvent.click(screen.getByRole("button", {name:"继续任务"}));
    await waitFor(() => expect(baseProps.onSend).toHaveBeenCalledExactlyOnceWith("继续", []));
  });

  it("等待可恢复位置时目录失效，不再回退历史", async () => {
    let finish!: (points: import("@/lib/types").RewindPoint[]) => void;
    vi.mocked(rewindPoints).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    setStore({messages:[
      {id:"u1",role:"user",complete:true,promptIndex:0,parts:[{kind:"text",text:"生成报告"}]},
      {id:"a1",role:"assistant",complete:true,parts:[{kind:"text",text:"原有报告"}]},
    ]});
    const view = renderChat();
    fireEvent.click(screen.getByRole("button", {name:"重新生成"}));
    await waitFor(() => expect(rewindPoints).toHaveBeenCalledOnce());
    workspaceMock.missing = true;
    view.rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    await act(async () => finish([{promptIndex:0,promptPreview:"生成报告"}]));
    expect(rewindExecute).not.toHaveBeenCalled();
    expect(screen.getByText("原有报告")).toBeVisible();
  });

  it("工作目录失效会撤销已打开的重新执行确认", async () => {
    setStore({messages:[
      {id:"u1",role:"user",complete:true,parts:[{kind:"text",text:"生成报告"}]},
      {id:"a1",role:"assistant",complete:true,parts:[{kind:"tool_call",toolCall:{toolCallId:"tool",title:"生成报告",kind:"execute",status:"completed",content:[]}}]},
    ]});
    const view = renderChat();
    fireEvent.click(screen.getByRole("button", {name:"重新执行"}));
    expect(screen.getByRole("alertdialog", {name:"重新执行本轮任务？"})).toBeVisible();
    workspaceMock.missing = true;
    view.rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    await waitFor(() => expect(screen.queryByRole("alertdialog", {name:"重新执行本轮任务？"})).toBeNull());
    expect(rewindExecute).not.toHaveBeenCalled();
  });

  it("有草稿时通过顶部任务菜单停止，保留输入并阻止重复操作", async () => {
    let finish!: (accepted: boolean) => void;
    baseProps.onCancel.mockImplementationOnce(() => new Promise<boolean>(resolve => { finish = resolve; }));
    setStore({streaming:true,streamingMessageId:"a1"});
    renderChat();
    fireEvent.change(screen.getByRole("textbox"), {target:{value:"保留补充要求"}});
    expect(screen.queryByRole("button", {name:"发送选项"})).toBeNull();
    expect(screen.queryByRole("button", {name:"停止生成"})).toBeNull();
    sessionItem("停止当前执行");
    expect(baseProps.onCancel).toHaveBeenCalledExactlyOnceWith("stop");
    expect(screen.getByRole("button", {name:"加入待发送队列"})).toBeDisabled();
    fireEvent.click(screen.getByRole("button", {name:"任务操作"}));
    expect(screen.getByRole("menuitem", {name:"正在停止…"})).toBeDisabled();
    fireEvent.click(screen.getByRole("menuitem", {name:"正在停止…"}));
    expect(baseProps.onCancel).toHaveBeenCalledOnce();
    await act(async () => finish(false));
    expect(screen.getByRole("button", {name:"加入待发送队列"})).toBeEnabled();
    expect(screen.getByRole("textbox")).toHaveValue("保留补充要求");
    expect(baseProps.onSend).not.toHaveBeenCalled();
  });

  it("普通任务只有一个输入框主按钮，有草稿时停止入口由顶部菜单承接", async () => {
    setStore({ streaming: true, streamingMessageId: "a1", messages: [
      ...storeState.messages,
      { id: "a1", role: "assistant", complete: false, startedAt: Date.now(), parts: [] },
    ] });
    const { container } = renderChat();
    expect(within(container.querySelector<HTMLElement>(".session-header")!).queryByRole("status")).toBeNull();
    expect(screen.getByRole("status")).toHaveTextContent(/正在请求模型|正在等待模型响应/);
    expect(screen.queryByText("正在执行，可继续补充要求")).toBeNull();
    expect(screen.queryByRole("group", { name: "当前轮次执行控制" })).toBeNull();
    expect(screen.queryByRole("button", { name: "暂停" })).toBeNull();
    expect(screen.queryByRole("button", { name: "停止" })).toBeNull();
    expect(screen.getAllByRole("button", { name: "停止生成" })).toHaveLength(1);
    const input = container.querySelector(".echo-composer__input")!;
    fireEvent.change(input, { target: { value: "请补充出处" } });
    expect(screen.queryByRole("button", { name: "停止生成" })).toBeNull();
    expect(screen.getByRole("button", { name: "加入待发送队列" })).toBeEnabled();
    sessionItem("停止当前执行");
    await waitFor(() => expect(baseProps.onCancel).toHaveBeenCalledExactlyOnceWith("stop"));
    expect(baseProps.onSend).not.toHaveBeenCalled();
    expect(input).toHaveValue("请补充出处");
  });

  it("生成回复期间页头保持简洁，计划确认与错误提醒仍能随状态切换显示", async () => {
    const message = { id: "a1", role: "assistant", complete: false, parts: [{ kind: "text", text: "正在整理资料" }] };
    setStore({ streaming: true, streamingMessageId: message.id, messages: [...storeState.messages, message] });
    const { container, rerender } = renderChat();
    const header = () => within(container.querySelector<HTMLElement>(".session-header")!);
    expect(header().queryByRole("status")).toBeNull();
    expect(screen.getByText("正在整理资料")).toBeVisible();
    expect(screen.getByRole("button", { name: "停止生成" })).toBeEnabled();

    setStore({ planApproval: { requestId: "plan-confirm", sessionId: "s1", toolCallId: "plan" } });
    await act(async () => { rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>); });
    expect(header().queryByRole("status")).toBeNull();
    expect(screen.getByText("执行计划等待确认")).toBeVisible();
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "查看并确认计划" })); });
    expect(screen.getByRole("tab", { name: "计划" })).toHaveAttribute("aria-selected", "true");

    setStore({ planApproval: undefined });
    await act(async () => { rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>); });
    expect(header().queryByRole("status")).toBeNull();
    expect(screen.getByRole("button", { name: "停止生成" })).toBeEnabled();

    setStore({ streaming: false, streamingMessageId: null, error: "模型连接中断" });
    await act(async () => { rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>); });
    expect(header().queryByRole("status")).toBeNull();
    expect(screen.getByRole("alert")).toHaveTextContent("模型连接中断");
  });

  it("同一轮的结束事件和错误事件只在回复附近提示一次，展开不新增警报", () => {
    setStore({ messages: [...storeState.messages, { id: "failed-inline", promptId: "failed-prompt", role: "assistant", complete: true,
      stopReason: "error", agentResult: "upstream connection lost", parts: [{ kind: "text", text: "保留的阶段结果" }] }],
      error: "upstream connection lost", runtimeErrorContext: {sessionId:"s1",promptId:"failed-prompt"} });
    const { container } = renderChat();
    expect(screen.getAllByRole("alert")).toHaveLength(1);
    expect(screen.getByRole("alert")).toHaveTextContent("连接中断");
    expect(container.querySelector(".chatview__error-banner")).toBeNull();
    expect(container.querySelector(".session-header [role=status]")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "查看原因" }));
    expect(screen.getByText("upstream connection lost")).toBeVisible();
    expect(screen.getAllByRole("alert")).toHaveLength(1);
  });

  it.each(["加载历史失败", "停止任务失败"])("旧轮次失败后新发生的%s保持独立可见", detail => {
    setStore({messages:[{id:"previous",role:"assistant",promptId:"old",complete:true,stopReason:"rate_limit",agentResult:"rate limit exceeded",parts:[{kind:"text",text:"保留的结果"}]}],
      streaming:false, error:detail,runtimeErrorContext:null});
    const {container} = renderChat();
    expect(container.querySelector(".chatview__error-banner")).toHaveTextContent(detail);
    fireEvent.click(screen.getByRole("button",{name:"查看原因"}));
    expect(screen.getByText("rate limit exceeded")).toBeVisible();
  });

  it("不将其他会话或旧轮次的运行时错误并入当前回复", () => {
    setStore({messages:[{id:"current",role:"assistant",promptId:"new",complete:true,stopReason:"error",agentResult:"new failure",parts:[]}],
      error:"old failure",runtimeErrorContext:{sessionId:"s1",promptId:"old"}});
    const {container}=renderChat();
    expect(container.querySelector(".chatview__error-banner")).toHaveTextContent("old failure");
  });

  it("检查并继续保留当前草稿与原记录，不走回溯或重新执行路径", async () => {
    setStore({ messages: [...storeState.messages, { id: "delivery-followup", role: "assistant", complete: true,
      stopReason: "end_turn", parts: [{ kind: "tool_call", toolCall: { toolCallId: "send-result-unknown",
        kind: "send_message", title: "发送结果", status: "in_progress", content: [] } }, { kind: "text", text: "阶段结果" }] }] });
    renderChat();
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "尚未发送的补充要求" } });
    fireEvent.click(screen.getByRole("button", { name: "检查并继续" }));
    await waitFor(() => expect(baseProps.onSend).toHaveBeenCalledWith(expect.stringContaining("不要直接重新提交"), []));
    expect(baseProps.onRetrySend).not.toHaveBeenCalled();
    expect(rewindExecute).not.toHaveBeenCalled();
    expect(screen.getByRole("textbox")).toHaveValue("尚未发送的补充要求");
    expect(screen.getByText("阶段结果")).toBeVisible();
  });

  it.each(["completed", "stopped", "switched"])("点击入队前状态变为 %s 时不会留下无人续发的队列消息", async state => {
    setStore({streaming:true,streamingMessageId:"a1"});
    useMessageQueueStore.getState().clear("s1");
    renderChat();
    fireEvent.change(screen.getByRole("textbox"),{target:{value:"最新补充"}});
    setStore({streaming:false,streamingMessageId:null,
      ...(state === "stopped" ? {control:{action:"stop",phase:"stopped",requestedAt:1}} : {}),
      ...(state === "switched" ? {sessionId:"s2"} : {}),
    });
    fireEvent.click(screen.getByRole("button",{name:"加入待发送队列"}));
    await act(async () => {});
    expect(useMessageQueueStore.getState().getQueue("s1")).toHaveLength(0);
    if (state === "completed") {
      expect(baseProps.onSend).toHaveBeenCalledExactlyOnceWith("最新补充",[]);
      expect(screen.getByRole("textbox")).toHaveValue("");
    } else if (state === "stopped") {
      expect(baseProps.onSend).not.toHaveBeenCalled();
      expect(screen.getByRole("textbox")).toHaveValue("最新补充");
    } else {
      expect(baseProps.onSend).not.toHaveBeenCalled();
      expect(screen.getByRole("textbox")).toHaveValue("");
    }
  });

  it("已打开计划清空后仍保留真实编辑器和新增草稿，收起后可重开", () => {
    setStore({ plan: { entries: [{ content: "最后一步", priority: "medium", status: "pending" }] }, transcripts: { s1: { planRevisionDirty: false } } });
    const { rerender } = renderChat();
    workspaceItem("执行计划");
    fireEvent.change(screen.getByRole("textbox", { name: "新增步骤" }), { target: { value: "保留待确认的新增草稿" } });
    setStore({ plan: { entries: [] }, planRevisionDirty: true, transcripts: { s1: { planRevisionDirty: true } } });
    rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    expect(screen.getByRole("textbox", { name: "新增步骤" })).toHaveValue("保留待确认的新增草稿");
    fireEvent.click(screen.getByRole("button", { name: "关闭面板" }));
    expect(screen.queryByRole("textbox", { name: "新增步骤" })).not.toBeInTheDocument();
    workspaceItem("执行计划");
    expect(screen.getByRole("textbox", { name: "新增步骤" })).toHaveValue("保留待确认的新增草稿");
    workspaceItem("网页预览");
    workspaceItem("执行计划");
    expect(screen.getByRole("textbox", { name: "新增步骤" })).toHaveValue("保留待确认的新增草稿");
  });

  it("页头显示计划进度并可直接打开和收起计划", () => {
    setStore({plan:{entries:[{content:"已完成步骤",priority:"medium",status:"completed"},{content:"下一步",priority:"medium",status:"pending"}]}});
    renderChat();
    const shortcut = screen.getByRole("button",{name:"查看执行计划，已完成 1/2 步"});
    expect(shortcut).toHaveTextContent("计划 1/2");
    expect(shortcut).toHaveAttribute("aria-expanded","false");
    fireEvent.click(shortcut);
    expect(screen.getByRole("tab",{name:"计划"})).toHaveAttribute("aria-selected","true");
    expect(shortcut).toHaveAttribute("aria-expanded","true");
    fireEvent.click(shortcut);
    expect(shortcut).toHaveAttribute("aria-expanded","false");
    expect(screen.queryByRole("tab",{name:"计划"})).toBeNull();
  });

  it("CORE19 真实 ACP flat plan 经生产 store 后显示可打开的计划入口", async () => {
    const actual = await vi.importActual<typeof import("@/stores/session-store")>("@/stores/session-store");
    actual.useSessionStore.setState({ sessionId: null, transcripts: {}, plan: null });
    const productionStore = actual.useSessionStore.getState();
    productionStore.setSession("s1");
    productionStore.applyUpdate({
      sessionUpdate: "plan",
      entries: [
        { content: "批准后保存实际成果", priority: "medium", status: "pending" },
        { content: "核对成果内容", priority: "high", status: "pending" },
      ],
      __sessionId: "s1",
    } as never);
    setStore({ plan: actual.useSessionStore.getState().plan });
    renderChat();
    workspaceItem("执行计划");
    expect(screen.getByText("批准后保存实际成果")).toBeInTheDocument();
    expect(screen.getByText("核对成果内容")).toBeInTheDocument();
  });

  afterEach(() => {
    delete (HTMLElement.prototype as { scrollIntoView?: unknown }).scrollIntoView;
  });

  it("普通对话只呈现统一头部，工作视图与会话操作通过分组入口访问", () => {
    const { container } = renderChat();
    expect(container.querySelectorAll(".session-header")).toHaveLength(1);
    expect(container.querySelector(".chatview__utility-actions")).toBeNull();
    sessionItem("查找当前会话");
    expect(screen.getByRole("textbox", {name:"查找"})).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "子代理" })).toBeNull();
    workspaceItem("文件变更");
    expect(container.querySelector(".file-changes")).toBeInTheDocument();
    expect(container.querySelector(".rewind-bar")).toBeNull();
  });

  it.each(["变更", "团队"])("跨会话关闭%s面板不会恢复旧会话的位置或停止跟随", (label) => {
    const { container, rerender } = renderChat();
    const viewport = container.querySelector<HTMLElement>(".chatview__scroll")!;
    Object.defineProperty(viewport, "scrollHeight", { configurable: true, value: 2000 });
    Object.defineProperty(viewport, "clientHeight", { configurable: true, value: 400 });
    // User reads an older message in session A before opening the panel.
    viewport.scrollTop = 600;
    fireEvent.wheel(viewport, { deltaY: -1 });
    fireEvent.scroll(viewport);
    workspaceItem(label === "团队" ? "管理任务团队" : "文件变更");
    setStore({ sessionId: "s2", messages: [...storeState.messages] });
    rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    expect(viewport.scrollTop).toBe(1600);
    expect(screen.queryByRole("region", { name: "任务工作视图" })).toBeNull();
    workspaceItem(label === "团队" ? "管理任务团队" : "文件变更");
    fireEvent.click(screen.getByRole("button", { name: "关闭面板" }));
    fireEvent.scroll(viewport);
    expect(viewport.scrollTop).toBe(1600);
    expect(screen.queryByRole("button", { name: "回到最新消息并恢复自动跟随" })).toBeNull();
    Object.defineProperty(viewport, "scrollHeight", { configurable: true, value: 2200 });
    setStore({ messages: [...storeState.messages], streaming: true });
    rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    expect(viewport.scrollTop).toBe(1800);
  });

  it("在同一会话关闭面板会恢复历史阅读位置", () => {
    const { container } = renderChat();
    const viewport = container.querySelector<HTMLElement>(".chatview__scroll")!;
    Object.defineProperty(viewport, "scrollHeight", { configurable: true, value: 2000 });
    Object.defineProperty(viewport, "clientHeight", { configurable: true, value: 400 });
    viewport.scrollTop = 600;
    fireEvent.wheel(viewport, { deltaY: -1 });
    fireEvent.scroll(viewport);
    workspaceItem("文件变更");
    expect(viewport.scrollTop).toBe(600);
    fireEvent.click(screen.getByRole("button", {name: "关闭面板"}));
    expect(viewport.scrollTop).toBe(600);
    expect(screen.getByRole("button", { name: "回到最新消息并恢复自动跟随" })).toBeInTheDocument();
  });

  it("从子代理返回后恢复展开条目和原阅读位置", async () => {
    useSubagentStore.getState().applyEvent({ sessionId: "s1", phase: "finished", subagentId: "child",
      childSessionId: "child", description: "核验任务", status: "completed" });
    let rowTop = 150;
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (this.classList.contains("task-details__body")) return { top: 50 } as DOMRect;
      if (this.classList.contains("subagent-panel__row")) return { top: rowTop } as DOMRect;
      return originalRect.call(this);
    });
    try {
      const openChild = vi.fn();
      const props = { ...baseProps, cwd: "/workspace", onOpenSubagentSession: openChild };
      const { container, rerender } = render(<ThemeProvider><ChatView {...props} /></ThemeProvider>);
      const viewport = container.querySelector<HTMLElement>(".chatview__scroll")!;
      Object.defineProperty(viewport, "scrollHeight", { configurable: true, value: 1000 });
      Object.defineProperty(viewport, "clientHeight", { configurable: true, value: 400 });
      workspaceItem("运行与协作");
      fireEvent.click(screen.getByRole("button", { name: /核验任务/ }));
      const detailsViewport = container.querySelector<HTMLElement>(".task-details__body")!;
      detailsViewport.scrollTop = 350;
      fireEvent.click(screen.getByRole("button", { name: "打开完整工作记录" }));
      expect(openChild).toHaveBeenCalledWith("child", "/workspace", {
        parentSessionId: "s1", parentCwd: "/workspace", subagentKey: "child", scrollTop: 350, rowOffset: 100,
      });

      setStore({ sessionId: "child" });
      rerender(<ThemeProvider><ChatView {...props} /></ThemeProvider>);
      rowTop = 250;
      setStore({ sessionId: "s1" });
      const restorePoint = { parentSessionId: "s1", parentCwd: "/workspace", childSessionId: "child",
        subagentKey: "child", scrollTop: 350, rowOffset: 100, sequence: 1 };
      rerender(<ThemeProvider><ChatView {...props} subagentScrollRestore={restorePoint} /></ThemeProvider>);
      await waitFor(() => expect(screen.getByRole("button", { name: /核验任务/ })).toHaveAttribute("aria-expanded", "true"));
      await waitFor(() => expect(container.querySelector<HTMLElement>(".task-details__body")!.scrollTop).toBe(450));
    } finally {
      rectSpy.mockRestore();
    }
  });

  it("已有任务也从输入框 + 菜单开启网页操作", async () => {
    renderChat();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "添加" }));
    });
    fireEvent.click(screen.getByRole("menuitemradio", { name: /操作网页/ }));

    await waitFor(() => expect(setCodingMode).toHaveBeenCalledWith("s1", "browser_use"));
    expect(baseProps.onToast).not.toHaveBeenCalledWith("已启用操作网页");
  });

  it("查询变化后首个真实命中立即定位，后续导航平滑滚动", async () => {
    setStore({
      messages: [{
        id: "a-find",
        role: "assistant",
        complete: true,
        parts: [{ kind: "text", text: "第一只鸟，第二只鸟" }],
      }],
    });
    renderChat();
    sessionItem("查找当前会话");
    fireEvent.change(screen.getByRole("textbox", { name: "查找" }), {
      target: { value: "鸟" },
    });

    await waitFor(() => {
      expect(scrollIntoViewMock).toHaveBeenCalledWith({
        behavior: "auto",
        block: "center",
      });
    });
    scrollIntoViewMock.mockClear();
    fireEvent.keyDown(screen.getByRole("textbox", { name: "查找" }), { key: "Enter" });
    await waitFor(() => {
      expect(scrollIntoViewMock).toHaveBeenCalledWith({
        behavior: "smooth",
        block: "center",
      });
    });
  });

  it("从历史提问跳转时暂停自动跟随，并可回到最新消息", async () => {
    setStore({
      messages: [
        { id: "u1", role: "user", complete: true, parts: [{ kind: "text", text: "最初的问题" }] },
        { id: "a1", role: "assistant", complete: true, parts: [{ kind: "text", text: "已回答" }] },
        { id: "u2", role: "user", complete: true, parts: [{ kind: "text", text: "后来的问题" }] },
        { id: "a2", role: "assistant", complete: false, parts: [{ kind: "text", text: "正在回答" }] },
      ],
      streaming: true,
      streamingMessageId: "a2",
    });
    renderChat();

    sessionItem("对话目录");
    const dialog = screen.getByRole("dialog", { name: "历史提问" });
    expect(within(dialog).getAllByRole("button").map((button) => button.textContent)).toEqual([
      "", "2后来的问题", "1最初的问题",
    ]);
    fireEvent.click(within(dialog).getByRole("button", { name: /最初的问题/ }));

    expect(screen.queryByRole("dialog", { name: "历史提问" })).toBeNull();
    expect(scrollIntoViewMock).toHaveBeenCalledWith({ behavior: "smooth", block: "center" });
    expect(screen.getByText("最初的问题").closest("[data-msg-id]")).toHaveClass("msg-wrap--jump-target");
    const jumpButton = screen.getByRole("button", { name: "回到最新消息并恢复自动跟随" });
    fireEvent.click(jumpButton);
    await waitFor(() => expect(screen.queryByRole("button", { name: "回到最新消息并恢复自动跟随" })).toBeNull());
  });

  it("切换会话后清空历史提问浮层和旧查询", async () => {
    const { rerender } = renderChat();
    sessionItem("对话目录");
    fireEvent.change(screen.getByRole("searchbox", { name: "筛选历史提问" }), { target: { value: "hi" } });

    setStore({
      sessionId: "s2",
      messages: [{ id: "u-next", role: "user", complete: true, parts: [{ kind: "text", text: "新会话的问题" }] }],
    });
    rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "历史提问" })).toBeNull());
    sessionItem("对话目录");
    const dialog = screen.getByRole("dialog", { name: "历史提问" });
    expect(within(dialog).getByRole("searchbox", { name: "筛选历史提问" })).toHaveValue("");
    expect(within(dialog).getByRole("button", { name: /新会话的问题/ })).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: /hi/ })).toBeNull();
  });

  it("运行中输入暂停指令走可信取消路径，不发送模型消息", async () => {
    setStore({ streaming: true, streamingMessageId: "a1" });
    renderChat();
    const input = screen.getByRole("textbox");
    fireEvent.change(input, {target:{value:"暂停"}});
    fireEvent.keyDown(input, {key:"Enter"});
    await waitFor(() => expect(baseProps.onCancel).toHaveBeenCalledExactlyOnceWith("pause"));
    expect(baseProps.onSend).not.toHaveBeenCalled();
    expect(useMessageQueueStore.getState().getQueue("s1")).toHaveLength(0);
  });

  it("流式时复制历史消息会暂停自动跟随，可手动回到最新", async () => {
    setStore({ streaming: true, streamingMessageId: "a1" });
    renderChat();

    fireEvent.pointerDown(screen.getByRole("button", { name: "复制" }));

    const jumpButton = await screen.findByRole("button", {
      name: "回到最新消息并恢复自动跟随",
    });
    fireEvent.click(jumpButton);
    await waitFor(() => {
      expect(screen.queryByRole("button", {
        name: "回到最新消息并恢复自动跟随",
      })).toBeNull();
    });
  });

  it("暂停后保留输入框，通过自然语言继续，无双重恢复入口", async () => {
    setStore({ streaming: true, streamingMessageId: "a1" });
    const { rerender } = renderChat();
    const input = screen.getByRole("textbox");
    fireEvent.change(input, {target:{value:"暂停"}});
    fireEvent.keyDown(input, {key:"Enter"});
    await waitFor(() => expect(baseProps.onCancel).toHaveBeenCalledWith("pause"));
    setStore({
      streaming: false,
      streamingMessageId: null,
      control: { action: "pause", phase: "paused", requestedAt: 1 },
    });
    rerender(
      <ThemeProvider>
        <ChatView {...baseProps} />
      </ThemeProvider>,
    );
    await waitFor(() => expect(screen.getByText("任务已暂停，可继续执行")).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: "恢复" })).toBeNull();
    expect(screen.getByRole("textbox")).toBeEnabled();
    expect(screen.queryByRole("button", { name: "恢复并继续" })).toBeNull();
  });

  it("普通暂停的继续通过可信发送且不消耗未发送草稿，等待时不能双发", async () => {
    let accepted!: (value: boolean) => void;
    baseProps.onSend.mockImplementationOnce(() => new Promise<boolean>(resolve => { accepted = resolve; }));
    setStore({control:{action:"pause",phase:"paused",requestedAt:1}});
    renderChat();
    const input = screen.getByRole("textbox");
    fireEvent.change(input,{target:{value:"尚未发送的补充要求"}});
    const resume = screen.getByRole("button", {name:"继续任务"});
    fireEvent.click(resume); fireEvent.click(resume);
    expect(baseProps.onSend).toHaveBeenCalledExactlyOnceWith("继续", []);
    expect(input).toHaveValue("尚未发送的补充要求");
    await act(async()=>accepted(false));
    expect(screen.getByRole("button",{name:"继续任务"})).toBeEnabled();
    expect(input).toHaveValue("尚未发送的补充要求");
    expect(baseProps.onToast).toHaveBeenCalledWith("暂时无法继续，请检查当前任务状态后重试");
    expect(automationResume).not.toHaveBeenCalled();
  });

  it("输出前暂停后继续会恢复原始请求和附件，不向空历史只发送继续", async () => {
    setStore({
      messages: [
        {id:"u-empty",role:"user",complete:true,promptIndex:0,
          agentText:"<project-context>original</project-context>\n只回复 ORIGINAL_GOAL",
          attachments:["/workspace/selected-input.txt"],
          parts:[{kind:"text",text:"只回复 ORIGINAL_GOAL"}]},
        {id:"a-empty",role:"assistant",complete:true,stopReason:"cancelled",parts:[]},
      ],
      control:{action:"pause",phase:"paused",requestedAt:1},
    });
    renderChat();
    fireEvent.click(screen.getByRole("button", {name:"继续任务"}));
    await waitFor(() => expect(baseProps.onRetrySend).toHaveBeenCalledExactlyOnceWith({
      sessionId:"s1", displayText:"只回复 ORIGINAL_GOAL",
      promptText:"<project-context>original</project-context>\n只回复 ORIGINAL_GOAL",
      attachments:["/workspace/selected-input.txt"], kind:"retry",
    }));
    expect(baseProps.onPrepareRetry).toHaveBeenCalledExactlyOnceWith("s1");
    expect(baseProps.onSend).not.toHaveBeenCalled();
  });

  it.each([
    {parts:[{kind:"text",text:"已经完成部分内容"}]},
    {parts:[{kind:"thought",text:"已经取得本轮上下文"}]},
    {parts:[{kind:"tool_call",toolCall:{toolCallId:"write-once",title:"Write file",kind:"edit",status:"completed",content:[]}}]},
  ])("有文本、思考或工具进展的暂停只继续，不重放已经执行的请求 %j", async ({parts}) => {
    setStore({messages:[
      {id:"u-progress",role:"user",complete:true,parts:[{kind:"text",text:"只执行一次写入"}]},
      {id:"a-progress",role:"assistant",complete:true,stopReason:"cancelled",parts},
    ],control:{action:"pause",phase:"paused",requestedAt:1}});
    renderChat();
    fireEvent.click(screen.getByRole("button", {name:"继续任务"}));
    await waitFor(() => expect(baseProps.onSend).toHaveBeenCalledExactlyOnceWith("继续", []));
    expect(baseProps.onRetrySend).not.toHaveBeenCalled();
    expect(rewindExecute).not.toHaveBeenCalled();
  });

  it.each([undefined, 1])("已有历史但本轮未持久化时，只重发本轮原题并保留旧历史：%s", async (promptIndex) => {
    vi.mocked(rewindPoints).mockResolvedValue([{promptIndex:0,promptPreview:"旧请求",toolNames:["write_file"]}]);
    vi.mocked(rewindExecute).mockResolvedValue({targetPromptIndex:0,promptText:"旧请求"});
    setStore({messages:[
      {id:"old-user",role:"user",complete:true,promptIndex:0,parts:[{kind:"text",text:"旧请求"}]},
      {id:"old-answer",role:"assistant",complete:true,parts:[{kind:"text",text:"旧回复必须保留"}]},
      {id:"new-user",role:"user",complete:true,promptIndex,agentText:"<context>本轮上下文</context>\n新请求",attachments:["/workspace/本轮.txt"],parts:[{kind:"text",text:"新请求"}]},
      {id:"new-answer",role:"assistant",complete:true,stopReason:"cancelled",parts:[]},
    ],control:{action:"pause",phase:"paused",requestedAt:1}});
    renderChat();
    fireEvent.click(screen.getByRole("button", {name:"继续任务"}));
    await waitFor(() => expect(baseProps.onRetrySend).toHaveBeenCalledExactlyOnceWith({
      sessionId:"s1",displayText:"新请求",promptText:"<context>本轮上下文</context>\n新请求",attachments:["/workspace/本轮.txt"],kind:"retry",
    }));
    expect(rewindExecute).not.toHaveBeenCalled();
    expect(storeState.discardMessagesFrom).toHaveBeenCalledExactlyOnceWith("s1", "new-user");
    expect(screen.getByText("旧回复必须保留")).toBeInTheDocument();
  });

  it("已持久化的空取消轮次只回溯本轮，保留原始模型提示", async () => {
    vi.mocked(rewindPoints).mockResolvedValue([{promptIndex:0}, {promptIndex:1}]);
    vi.mocked(rewindExecute).mockResolvedValue({targetPromptIndex:1,promptText:"本轮原始模型提示"});
    setStore({messages:[
      {id:"old-user",role:"user",complete:true,promptIndex:0,parts:[{kind:"text",text:"旧请求"}]},
      {id:"new-user",role:"user",complete:true,promptIndex:1,parts:[{kind:"text",text:"新请求"}]},
      {id:"new-answer",role:"assistant",complete:true,stopReason:"cancelled",parts:[]},
    ],control:{action:"pause",phase:"paused",requestedAt:1}});
    renderChat();
    fireEvent.click(screen.getByRole("button", {name:"继续任务"}));
    await waitFor(() => expect(rewindExecute).toHaveBeenCalledExactlyOnceWith("s1", 1, "conversation_only", true));
    expect(baseProps.onRetrySend).toHaveBeenCalledWith({sessionId:"s1",displayText:"新请求",promptText:"本轮原始模型提示",attachments:[],kind:"retry"});
    expect(storeState.discardMessagesFrom).not.toHaveBeenCalled();
  });

  it("隐藏的中间轮次不阻塞尚未持久化的新请求恢复", async () => {
    vi.mocked(rewindPoints).mockResolvedValue([0,1,2,3].map(promptIndex => ({promptIndex})));
    setStore({messages:[
      {id:"previous-user",role:"user",complete:true,promptIndex:3,parts:[{kind:"text",text:"最近的已入模请求"}]},
      {id:"new-user",role:"user",complete:true,parts:[{kind:"text",text:"未入模的新请求"}]},
      {id:"new-answer",role:"assistant",complete:true,stopReason:"cancelled",parts:[]},
    ],control:{action:"pause",phase:"paused",requestedAt:1}});
    renderChat();
    fireEvent.click(screen.getByRole("button", {name:"继续任务"}));
    await waitFor(() => expect(baseProps.onRetrySend).toHaveBeenCalledWith(expect.objectContaining({promptText:"未入模的新请求"})));
    expect(rewindExecute).not.toHaveBeenCalled();
  });

  it("准备恢复期间保留可编辑草稿，同时阻止新发送和队列发送竞争历史", async () => {
    let finish!: (points: import("@/lib/types").RewindPoint[]) => void;
    vi.mocked(rewindPoints).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    setStore({messages:[
      {id:"u-paused",role:"user",complete:true,parts:[{kind:"text",text:"恢复本轮"}]},
      {id:"a-paused",role:"assistant",complete:true,stopReason:"cancelled",parts:[]},
    ],control:{action:"pause",phase:"paused",requestedAt:1}});
    useMessageQueueStore.getState().enqueue("s1","待发送",[]);
    try {
      renderChat();
      const input = screen.getByRole("textbox");
      fireEvent.change(input,{target:{value:"未发送的新草稿"}});
      fireEvent.click(screen.getByRole("button", {name:"继续任务"}));
      await waitFor(() => expect(rewindPoints).toHaveBeenCalledOnce());
      expect(input).toBeEnabled();
      expect(input).toHaveValue("未发送的新草稿");
      expect(within(screen.getByRole("list",{name:"待发送队列"})).getByRole("button",{name:"立即发送"})).toBeDisabled();
      fireEvent.keyDown(input,{key:"Enter",code:"Enter"});
      expect(baseProps.onSend).not.toHaveBeenCalled();
      await act(async () => finish([]));
      expect(baseProps.onRetrySend).toHaveBeenCalledExactlyOnceWith({sessionId:"s1",displayText:"恢复本轮",promptText:"恢复本轮",attachments:[],kind:"retry"});
      expect(input).toHaveValue("未发送的新草稿");
    } finally { act(() => useMessageQueueStore.getState().clear("s1")); }
  });

  it("尚未绑定会话的新任务仍能发送首条消息", async () => {
    setStore({sessionId:null,messages:[],control:undefined});
    renderChat();
    const input = screen.getByRole("textbox");
    fireEvent.change(input,{target:{value:"新任务请求"}});
    expect(screen.getByRole("button",{name:"发送"})).toBeEnabled();
    fireEvent.keyDown(input,{key:"Enter",code:"Enter"});
    await waitFor(() => expect(baseProps.onSend).toHaveBeenCalledExactlyOnceWith("新任务请求", []));
  });

  it.each(["rewind", "send"])("在 %s 期间切换任务后，重试结果提醒原任务继续且不抢回当前任务", async phase => {
    let finish: (() => void) | undefined;
    vi.mocked(rewindPoints).mockResolvedValue([{ promptIndex: 0 }]);
    if (phase === "rewind") {
      vi.mocked(rewindExecute).mockImplementationOnce(() => new Promise(resolve => {
        finish = () => resolve({ targetPromptIndex: 0 });
      }));
    } else {
      baseProps.onRetrySend.mockImplementationOnce(() => new Promise(resolve => {
        finish = () => resolve(true);
      }));
    }
    setStore({ messages: [
      { id: "user", role: "user", complete: true, promptIndex: 0, parts: [{ kind: "text", text: "原任务" }] },
      { id: "answer", role: "assistant", complete: true, stopReason: "cancelled", parts: [] },
    ] });
    const view = renderChat();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(finish).toBeDefined());
    setStore({ sessionId: "s2", messages: [] });
    view.rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    await act(async () => finish!());
    expect(baseProps.onToast).toHaveBeenCalledWith("已在原任务中继续执行");
    expect(baseProps.onRetrySend).toHaveBeenCalledWith(expect.objectContaining({ sessionId: "s1" }));
    expect(storeState.sessionId).toBe("s2");
  });

  it("跨任务重试分别保留发送屏障，另一任务完成不会解锁原任务", async () => {
    const pending = new Map<string, (points: import("@/lib/types").RewindPoint[]) => void>();
    vi.mocked(rewindPoints).mockImplementation(sid => new Promise(resolve => { pending.set(sid, resolve); }));
    const cancelled = (sid: string) => [
      {id:`${sid}-user`,role:"user",complete:true,parts:[{kind:"text",text:`${sid} 的请求`}]},
      {id:`${sid}-answer`,role:"assistant",complete:true,stopReason:"cancelled",parts:[]},
    ];
    const aMessages = cancelled("s1");
    setStore({messages:aMessages});
    const view = renderChat();
    fireEvent.click(screen.getByRole("button",{name:"重试"}));
    await waitFor(() => expect(pending.has("s1")).toBe(true));
    setStore({sessionId:"s2",messages:cancelled("s2")});
    view.rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    fireEvent.click(screen.getByRole("button",{name:"重试"}));
    await waitFor(() => expect(pending.has("s2")).toBe(true));
    await act(async () => pending.get("s2")!([]));
    setStore({sessionId:"s1",messages:aMessages});
    view.rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    const input = screen.getByRole("textbox");
    fireEvent.change(input,{target:{value:"新草稿"}});
    expect(screen.getByRole("button",{name:"发送"})).toBeDisabled();
    expect(screen.getByRole("button",{name:"正在重试…"})).toBeDisabled();
    await act(async () => pending.get("s1")!([]));
    expect(screen.getByRole("button",{name:"发送"})).toBeEnabled();
    expect(baseProps.onRetrySend.mock.calls.map(([request]) => request.sessionId)).toEqual(["s2","s1"]);
  });

  it("缺少本轮身份且存在未归属的后台记录时，保留现场并提示同步记录", async () => {
    vi.mocked(rewindPoints).mockResolvedValue([{promptIndex:0,promptPreview:"同名请求"}, {promptIndex:1,promptPreview:"同名请求"}]);
    setStore({messages:[
      {id:"old-user",role:"user",complete:true,promptIndex:0,parts:[{kind:"text",text:"同名请求"}]},
      {id:"new-user",role:"user",complete:true,parts:[{kind:"text",text:"同名请求"}]},
      {id:"new-answer",role:"assistant",complete:true,stopReason:"cancelled",parts:[]},
    ],control:{action:"pause",phase:"paused",requestedAt:1}});
    renderChat();
    fireEvent.click(screen.getByRole("button", {name:"继续任务"}));
    await waitFor(() => expect(baseProps.onToast).toHaveBeenCalledWith(expect.stringContaining("重新打开任务")));
    expect(rewindExecute).not.toHaveBeenCalled();
    expect(baseProps.onRetrySend).not.toHaveBeenCalled();
    expect(storeState.discardMessagesFrom).not.toHaveBeenCalled();
  });

  it("等待回溯点期间新请求已进入会话时，不再替换旧请求", async () => {
    let finish!: (points: import("@/lib/types").RewindPoint[]) => void;
    vi.mocked(rewindPoints).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    setStore({messages:[
      {id:"first-user",role:"user",complete:true,promptIndex:0,parts:[{kind:"text",text:"旧请求"}]},
      {id:"first-answer",role:"assistant",complete:true,stopReason:"cancelled",parts:[]},
    ],control:{action:"pause",phase:"paused",requestedAt:1}});
    renderChat();
    fireEvent.click(screen.getByRole("button", {name:"继续任务"}));
    await waitFor(() => expect(rewindPoints).toHaveBeenCalledOnce());
    setStore({messages:[...storeState.messages, {id:"newer-user",role:"user",complete:true,parts:[{kind:"text",text:"更新的请求"}]}]});
    await act(async () => finish([{promptIndex:0}]));
    expect(rewindExecute).not.toHaveBeenCalled();
    expect(baseProps.onRetrySend).not.toHaveBeenCalled();
  });

  it("暂停且计划待确认时，继续与队列发送也必须等待当前决策", () => {
    setStore({control:{action:"pause",phase:"paused",requestedAt:1},planApproval:{requestId:"blocked-plan",sessionId:"s1",toolCallId:"plan",planContent:"待确认方案"}});
    useMessageQueueStore.getState().enqueue("s1","待发送",[]);
    try {
      renderChat();
      expect(screen.getByRole("button",{name:"继续任务"})).toBeDisabled();
      expect(screen.getByRole("textbox")).toBeDisabled();
      expect(within(screen.getByRole("list",{name:"待发送队列"})).getByRole("button",{name:"立即发送"})).toBeDisabled();
      expect(screen.getByRole("button",{name:"查看并确认计划"})).toBeInTheDocument();
      expect(baseProps.onSend).not.toHaveBeenCalled();
    } finally { useMessageQueueStore.getState().clear("s1"); setStore({planApproval:undefined}); }
  });

  it("过渡态禁止重复输入，已停止后允许用新消息继续", () => {
    setStore({
      streaming: false,
      streamingMessageId: null,
      control: { action: "stop", phase: "stopping", requestedAt: 1 },
    });
    const { rerender } = renderChat();
    expect(screen.getByText("正在停止任务…")).toBeInTheDocument();
    expect(screen.getByRole("textbox")).toBeDisabled();

    setStore({
      control: { action: "stop", phase: "stopped", requestedAt: 1 },
    });
    rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    expect(screen.getByText("已停止（发送新消息可继续此任务）")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "继续此任务" })).toBeNull();
    expect(screen.getByRole("textbox")).toBeEnabled();
  });

  it("取消失败且没有 control 时不会伪装成已暂停", async () => {
    setStore({ streaming: true, streamingMessageId: "a1" });
    baseProps.onCancel.mockResolvedValueOnce(false);
    const { rerender } = renderChat();
    const input = screen.getByRole("textbox");
    fireEvent.change(input, {target:{value:"暂停"}});
    fireEvent.keyDown(input, {key:"Enter"});
    await waitFor(() => expect(baseProps.onCancel).toHaveBeenCalledWith("pause"));
    setStore({ streaming: true, streamingMessageId: "a1", control: undefined });
    rerender(
      <ThemeProvider>
        <ChatView {...baseProps} />
      </ThemeProvider>,
    );
    expect(screen.queryByText("任务已暂停，可继续执行")).toBeNull();
    expect(input).toHaveValue("暂停");
  });

  it("运行中切换网页工具先等待暂停确认，失败时不切换", async () => {
    setStore({ streaming: true, streamingMessageId: "a1" });
    baseProps.onCancel.mockResolvedValueOnce(false);
    renderChat();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "添加" })));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /操作网页/ }));
    await waitFor(() => expect(baseProps.onCancel).toHaveBeenCalledWith("pause"));
    expect(setCodingMode).not.toHaveBeenCalled();
  });

  it("运行中切换网页工具在暂停确认后才生效，等待时保留原模式", async () => {
    let confirmPause!: (value: boolean) => void;
    baseProps.onCancel.mockImplementationOnce(() => new Promise<boolean>(resolve => { confirmPause = resolve; }));
    setStore({ streaming: true, streamingMessageId: "a1" });
    renderChat();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "添加" })));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /操作网页/ }));
    await waitFor(() => expect(baseProps.onCancel).toHaveBeenCalledWith("pause"));
    expect(setCodingMode).not.toHaveBeenCalled();
    expect(storeState.agentMode).toBe("default");
    await act(async () => confirmPause(true));
    await waitFor(() => expect(setCodingMode).toHaveBeenCalledWith("s1", "browser_use"));
    expect(storeState.agentMode).toBe("browser_use");
    expect(baseProps.onSend).not.toHaveBeenCalled();
  });

  it("请求未进入 Runtime 时，原位清理失败轮次后重试", async () => {
    setStore({
      messages: [
        { id: "u1", role: "user", complete: true, parts: [{ kind: "text", text: "生成每日报告" }] },
        { id: "a1", role: "assistant", complete: true, parts: [] },
      ],
      error: "未选择模型，请选择模型",
    });
    renderChat();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    await waitFor(() => expect(baseProps.onRetrySend).toHaveBeenCalledWith({
      sessionId: "s1",
      displayText: "生成每日报告",
      promptText: "生成每日报告",
      attachments: [],
      kind: "retry",
    }));
    expect(storeState.discardMessagesFrom).toHaveBeenCalledWith("s1", "u1");
    expect(baseProps.onSend).not.toHaveBeenCalled();
  });

  it("无法安全替换纯文本回复时保留原回复，不追加重复消息", async () => {
    setStore({
      messages: [
        { id: "u1", role: "user", complete: true, parts: [{ kind: "text", text: "执行任务" }] },
        { id: "a1", role: "assistant", complete: true, parts: [{ kind: "text", text: "已执行一部分" }] },
      ],
    });
    renderChat();

    replyItem("重新生成");

    await waitFor(() => expect(rewindPoints).toHaveBeenCalledWith("s1"));
    expect(baseProps.onRetrySend).not.toHaveBeenCalled();
    expect(storeState.discardMessagesFrom).not.toHaveBeenCalled();
    expect(baseProps.onToast).toHaveBeenCalledWith(
      "重新生成失败：暂时无法替换这条回复，原回复已保留。请稍后重试。",
    );
  });

  it("重新生成使用被替换轮次的原始模型提示", async () => {
    setStore({
      messages: [
        {
          id: "u1",
          role: "user",
          complete: true,
          promptIndex: 4,
          agentText: "<hidden>project contract</hidden>\n\n执行任务",
          attachments: ["/tmp/方案.docx"],
          parts: [{ kind: "text", text: "执行任务" }],
        },
        { id: "a1", role: "assistant", complete: true, parts: [{ kind: "text", text: "旧回答" }] },
      ],
    });
    vi.mocked(rewindPoints).mockResolvedValue([
      { promptIndex: 2 },
      { promptIndex: 4 },
      { promptIndex: 3 },
    ]);
    vi.mocked(rewindExecute).mockResolvedValue({
      targetPromptIndex: 4,
      promptText: "<hidden>project contract</hidden>\n\n执行任务"
        + "\n\n附件（图片已作为多模态内容附加；其他文件请使用 read_file 读取）：\n- @/tmp/方案.docx",
    });
    renderChat();

    replyItem("重新生成");

    await waitFor(() => expect(rewindExecute).toHaveBeenCalledWith("s1", 4, "conversation_only", true));
    expect(baseProps.onRetrySend).toHaveBeenCalledWith({
      sessionId: "s1",
      displayText: "执行任务",
      promptText: "<hidden>project contract</hidden>\n\n执行任务",
      attachments: ["/tmp/方案.docx"],
      kind: "regenerate",
    });
  });

  it("工具轮次必须确认后才会重新执行，且只回溯对话", async () => {
    setStore({
      messages: [
        { id: "u1", role: "user", complete: true, promptIndex: 0, parts: [{ kind: "text", text: "帮我查查 ls" }] },
        {
          id: "a1",
          role: "assistant",
          complete: true,
          parts: [
            {
              kind: "tool_call",
              toolCall: {
                toolCallId: "tool-1",
                title: "运行终端命令",
                kind: "terminal",
                status: "completed",
                content: [],
              },
            },
            { kind: "text", text: "目录已列出。" },
          ],
        },
      ],
    });
    vi.mocked(rewindPoints).mockResolvedValue([{ promptIndex: 0 }]);
    renderChat();

    replyItem("重新执行");
    const dialog = screen.getByRole("alertdialog", { name: "重新执行本轮任务？" });
    expect(dialog).toBeInTheDocument();
    expect(screen.getByText(/1 个工具/)).toBeInTheDocument();
    expect(dialog).toHaveTextContent("将从当前工作区状态开始");
    expect(dialog).not.toHaveTextContent("回溯");
    expect(rewindPoints).not.toHaveBeenCalled();

    fireEvent.click(within(dialog).getByRole("button", { name: "重新执行" }));
    await waitFor(() => expect(rewindExecute).toHaveBeenCalledWith("s1", 0, "conversation_only", true));
    expect(baseProps.onRetrySend).toHaveBeenCalledWith({
      sessionId: "s1",
      displayText: "帮我查查 ls",
      promptText: "帮我查查 ls",
      attachments: [],
      kind: "reexecute",
    });
  });
});

it("restored native export receipts become reachable after history loading binds the session", async () => {
  storeState = { ...storeState, sessionId: "export-history", messages: [], streaming: false, error: null, plan: null };
  artifactMocks.load.mockReset().mockResolvedValue([{ id: "manual", path: "/synthetic/report.docx", kind: "document_export",
    title: "报告 · DOCX", toolCallId: "document-export:receipt", status: "completed", verifiedOutput: true }]);
  localStorage.clear();
  const { rerender } = render(<ThemeProvider><ChatView {...baseProps} modelLoading /></ThemeProvider>);
  expect(artifactMocks.load).not.toHaveBeenCalled();
  rerender(<ThemeProvider><ChatView {...baseProps} modelLoading={false} /></ThemeProvider>);
  await screen.findByLabelText("1 项成果");
  const button = screen.getByRole("button", { name: "展开工作区" });
  expect(artifactMocks.load).toHaveBeenCalledWith("export-history");
  fireEvent.click(button);
  expect((await screen.findAllByText("report.docx")).length).toBeGreaterThan(0);
});


it("历史任务使用自己的目录发现命令，新建动作不会重绑旧任务", async () => {
  storeState = { ...storeState, sessionId: "session-a", messages: [], streaming: false, error: null, plan: null };
  vi.mocked(commandsList).mockClear();
  vi.mocked(commandsList).mockResolvedValue([{name:"project-a-skill", description:"A 的技能"}]);
  const onNewTask = vi.fn();
  const onSend = vi.fn();
  const {rerender} = render(<ThemeProvider><ChatView {...baseProps} onSend={onSend} cwd="/workspace/A" navigation={{ sidebarCollapsed: true, onNewTask }} onNewTask={onNewTask} /></ThemeProvider>);
  expect(screen.queryByRole("button", {name:"打开工作目录"})).toBeNull();
  const input = screen.getByRole("textbox");
  fireEvent.change(input, {target:{value:"/project-a"}});
  await waitFor(() => expect(commandsList).toHaveBeenLastCalledWith("session-a", "/workspace/A"));
  fireEvent.click(screen.getByRole("button", {name:"新建任务"}));
  expect(onNewTask).toHaveBeenCalledOnce();
  // A future task may choose C, while this mounted historical task stays A.
  rerender(<ThemeProvider><ChatView {...baseProps} onSend={onSend} cwd="/workspace/A" navigation={{ sidebarCollapsed: true, onNewTask }} onNewTask={onNewTask} commandRefreshKey={1} /></ThemeProvider>);
  await waitFor(() => expect(commandsList).toHaveBeenLastCalledWith("session-a", "/workspace/A"));
  expect(screen.queryByRole("button", {name:"打开工作目录"})).toBeNull();
  expect(await screen.findByText("/project-a-skill")).toBeInTheDocument();
});


describe("自动化任务恢复", () => {
  beforeEach(() => {
    setStore({sessionId:"s1",messages:[],streaming:false,streamingMessageId:null,error:null,plan:null,agentMode:"computer_use",control:undefined});
    baseProps.onSend.mockReset();
    baseProps.onToast.mockClear();
    vi.mocked(automationResume).mockReset().mockResolvedValue(undefined as never);
    vi.mocked(automationPause).mockReset().mockResolvedValue(undefined as never);
    artifactMocks.load.mockReset().mockResolvedValue([]);
  });
it("电脑任务可直接输入继续保存，恢复只由同一次可信发送处理", async () => {
  setStore({agentMode:"computer_use", streaming:false, control:{action:"pause",phase:"paused",requestedAt:1}});
  baseProps.onSend.mockResolvedValueOnce(true);
  renderChat();
  const input=screen.getByRole("textbox");
  expect(input).toBeEnabled();
  expect(screen.queryByRole("button",{name:"恢复"})).toBeNull();
  expect(screen.queryByRole("button",{name:"恢复并继续"})).toBeNull();
  const continuation = screen.getByRole("button",{name:"继续任务"});
  expect(continuation.closest(".chatview__scroll")).not.toBeNull();
  expect(continuation.closest(".chatview__footer")).toBeNull();
  fireEvent.change(input,{target:{value:"继续保存"}});
  fireEvent.keyDown(input,{key:"Enter",code:"Enter"});
  await waitFor(()=>expect(baseProps.onSend).toHaveBeenCalledWith("继续保存",[]));
  expect(automationResume).not.toHaveBeenCalled();
});
it("两轮模型调用之间仍能通过停一下撤销电脑执行", async () => {
  setStore({agentMode:"computer_use",streaming:false,control:undefined});
  renderChat();
  const input=screen.getByRole("textbox");
  fireEvent.change(input,{target:{value:"停一下"}});
  fireEvent.keyDown(input,{key:"Enter",code:"Enter"});
  await waitFor(()=>expect(baseProps.onCancel).toHaveBeenCalledWith("pause"));
  expect(baseProps.onSend).not.toHaveBeenCalled();
});
});


describe("session workbench entry paths", () => {
  beforeEach(() => {
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
    artifactMocks.load.mockReset().mockResolvedValue([]);
    useSubagentStore.setState({bySession:{}});
    setStore({planApproval:undefined,planRevisionDirty:false,agentMode:"default",sessionId:"workbench",messages:[{id:"u-entry",role:"user",complete:true,parts:[{kind:"text",text:"完成交付"}]}],streaming:false,sendNowPending:false,streamingMessageId:null,error:null,plan:null,control:undefined});
    vi.mocked(rewindExecute).mockClear();
    vi.mocked(sessionFork).mockClear();
  });
  afterEach(() => { delete (HTMLElement.prototype as { scrollIntoView?: unknown }).scrollIntoView; });
  it("空任务保留文件和网页浏览，只隐藏没有内容的查找、交付与历史操作", () => {
    setStore({messages:[]});
    render(<ThemeProvider><ChatView {...baseProps} cwd="/bound/workspace" /></ThemeProvider>);
    expect(screen.queryByRole("button",{name:"查找当前会话"})).toBeNull();
    expect(screen.getByRole("button",{name:"展开工作区"})).toBeVisible();
    expect(screen.queryByRole("button", {name:"打开工作目录"})).toBeNull();
    expect(screen.getByRole("textbox")).toBeVisible();
    workspaceItem("文件.*工作目录");
    expect(screen.getByRole("complementary",{name:"工作区面板"})).toBeVisible();
    workspaceItem("网页预览");
    expect(screen.getByRole("textbox",{name:"预览网址"})).toBeVisible();
    expect(screen.queryByRole("button",{name:"任务操作"})).toBeNull();
    expect(screen.queryByRole("menuitem",{name:"导出整个会话…"})).toBeNull();
    expect(screen.queryByRole("menuitem",{name:"回溯…"})).toBeNull();
  });
  it("仅附件提问的历史定位不依赖查找按钮，关闭后返回目录入口", async () => {
    setStore({messages:[{id:"attachment-question",role:"user",complete:true,parts:[],attachments:["/bound/report.docx"]}]});
    renderChat();
    expect(screen.queryByRole("button",{name:"查找当前会话"})).toBeNull();
    sessionItem("对话目录");
    const history=screen.getByRole("dialog",{name:"历史提问"});
    expect(history).toBeVisible();
    expect(within(history).getByRole("button",{name:/report.docx/})).toBeVisible();
    fireEvent.keyDown(document,{key:"Escape"});
    expect(screen.queryByRole("dialog",{name:"历史提问"})).toBeNull();
    await waitFor(()=>expect(screen.getByRole("button",{name:/^对话目录，共/})).toHaveFocus());
  });
  it("计划、网页预览和变更保持互斥，并提供相同关闭路径", () => {
    setStore({plan:{entries:[{content:"执行交付核验",status:"pending",priority:"high"}]}});
    renderChat();
    workspaceItem("执行计划");
    expect(screen.getByText("执行交付核验")).toBeVisible();
    workspaceItem("网页预览");
    expect(screen.getByText("执行交付核验")).not.toBeVisible();
    expect(screen.getByRole("complementary",{name:"工作区面板"})).toBeVisible();
    workspaceItem("文件变更");
    expect(screen.getByRole("complementary",{name:"工作区面板"})).toBeVisible();
    expect(screen.getByRole("region",{name:"文件变更"})).toBeVisible();
    fireEvent.click(screen.getByRole("button",{name:"关闭面板"}));
    expect(screen.queryByRole("region",{name:"文件变更"})).toBeNull();
  });
  it("只有文本计划的审批也直接提示，并可进入原计划审批组件", () => {
    setStore({plan:null,planApproval:{requestId:"plan-text",sessionId:"workbench",toolCallId:"plan-tool",planContent:"先核对当前任务，再生成交付文档"}});
    renderChat();
    fireEvent.click(screen.getByRole("button",{name:"查看并确认计划"}));
    expect(screen.getByText("先核对当前任务，再生成交付文档")).toBeVisible();
    expect(screen.getByRole("group",{name:"计划审批"})).toBeVisible();
  });
  it("返回列表后可重复打开底部同一成果，视图切换不会自动打开旧文件", async () => {
    const width=window.innerWidth;
    Object.defineProperty(window,"innerWidth",{configurable:true,value:1280});
    try {
    artifactMocks.load.mockResolvedValue(["report.md","second.txt","third.txt","fourth.txt"].map(name=>({id:name,path:`/bound/${name}`,kind:"file",title:name,toolCallId:"",status:"completed" as const})));
    const view=render(<ThemeProvider><ChatView {...baseProps} cwd="/bound" /></ThemeProvider>);
    workspaceItem("任务产物");
    const panel=screen.getByRole("complementary",{name:"工作区面板"});
    fireEvent.click(await within(panel).findByRole("button",{name:/^report\.md/}));
    expect(await screen.findByRole("button",{name:"查看关联工具"})).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button",{name:"返回成果列表"}));
    fireEvent.click(within(panel).getByRole("button",{name:/^report\.md/}));
    expect(await screen.findByRole("button",{name:"查看关联工具"})).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button",{name:"返回成果列表"}));
    expect(view.container.querySelector(".task-details__list")).toBeInTheDocument();
    expect(view.container.querySelector(".artifact-file-preview")).toBeNull();
    fireEvent.click(within(panel).getByRole("button",{name:/^report\.md/}));
    workspaceItem("文件.*工作目录");
    await waitFor(()=>expect(view.container.querySelector(".task-details__list")).toBeInTheDocument());
    expect(view.container.querySelector(".artifact-file-preview")).toBeNull();
    workspaceItem("任务产物");
    fireEvent.click(within(panel).getByRole("button",{name:/^report\.md/}));
    fireEvent.click(within(panel).getByRole("tab",{name:"文件"}));
    await waitFor(()=>expect(view.container.querySelector(".task-details__list")).toBeInTheDocument());
    expect(view.container.querySelector(".artifact-file-preview")).toBeNull();
    fireEvent.click(within(panel).getByRole("tab",{name:"成果"}));
    expect(within(panel).getByRole("button",{name:/^report\.md/})).toBeInTheDocument();
    } finally { Object.defineProperty(window,"innerWidth",{configurable:true,value:width}); }
  });
  it("从浏览器标签切回产物时，选择回调不会覆盖同一批次选定的视图", async () => {
    artifactMocks.load.mockResolvedValue([{ id: "workbench-report", path: "/bound/report.md", kind: "file", title: "report.md", toolCallId: "", status: "completed" }]);
    render(<ThemeProvider><ChatView {...baseProps} cwd="/bound" /></ThemeProvider>);
    workspaceItem("任务产物");
    fireEvent.click(await within(screen.getByRole("complementary", {name:"工作区面板"})).findByRole("button", { name: /^report\.md/ }));
    expect(await screen.findByRole("button", { name: "关闭标签 report.md" })).toBeInTheDocument();
    workspaceItem("网页预览");
    fireEvent.change(screen.getByRole("textbox",{name:"预览网址"}), {target:{value:"https://example.com"}});
    fireEvent.click(screen.getByRole("button",{name:"预览"}));
    fireEvent.click(screen.getByRole("tab", { name: /report\.md/ }));
    expect(screen.getByRole("tab", { name: "成果" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /report\.md/ })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("button", { name: "查看关联工具" })).toBeInTheDocument();
  });
  it("从更多进入回溯，仅在确认范围后调用当前会话并刷新", async () => {
    vi.mocked(rewindPoints).mockResolvedValue([{promptIndex:3,promptPreview:"交付核验"}]);
    const onRewound=vi.fn();
    render(<ThemeProvider><ChatView {...baseProps} onRewound={onRewound} /></ThemeProvider>);
    sessionItem("回溯…");
    fireEvent.click(screen.getByRole("button",{name:"仅文件"}));
    fireEvent.click(await screen.findByRole("button",{name:/回溯到此处/}));
    expect(rewindExecute).not.toHaveBeenCalled();
    expect(screen.getByRole("alertdialog",{name:"确认回溯（仅文件）？"})).toHaveTextContent("第 4 步执行前：交付核验");
    expect(screen.getByRole("alertdialog",{name:"确认回溯（仅文件）？"})).toHaveTextContent("保留对话");
    fireEvent.click(screen.getByRole("button",{name:"确认回溯"}));
    await waitFor(()=>expect(rewindExecute).toHaveBeenCalledWith("workbench",3,"files_only",true));
    expect(onRewound).toHaveBeenCalledWith("workbench");
  });
  it("分叉取消后可以重新进入，确认后沿用原工作目录导航", async () => {
    const onForked=vi.fn();
    render(<ThemeProvider><ChatView {...baseProps} cwd="/bound/workspace" onForked={onForked} /></ThemeProvider>);
    sessionItem("分叉…");
    fireEvent.click(screen.getByRole("button",{name:"取消"}));
    await waitFor(()=>expect(screen.queryByRole("dialog",{name:"分叉此会话？"})).toBeNull());
    expect(sessionFork).not.toHaveBeenCalled();
    sessionItem("分叉…");
    fireEvent.click(screen.getByRole("button",{name:"创建分叉"}));
    await waitFor(()=>expect(onForked).toHaveBeenCalledWith("new-fork","workbench","/bound/workspace"));
    expect(sessionFork).toHaveBeenCalledWith("workbench","/bound/workspace");
  });

  const busyStates = [
    ["开始运行", { streaming: true }, false],
    ["立即发送切换中", { sendNowPending: true }, false],
    ["正在暂停", { control: { action: "pause", phase: "pausing", requestedAt: 1 } }, false],
    ["正在停止", { control: { action: "stop", phase: "stopping", requestedAt: 1 } }, false],
    ["加载会话或模型", {}, true],
  ] as const;
  describe.each(["rewind", "fork"] as const)("%s 确认的状态失效", action => {
    it.each(busyStates)("%s 时立即关闭，任务稳定后也不复活旧确认", async (_label, patch, loading) => {
      vi.mocked(rewindPoints).mockResolvedValue([{ promptIndex: 0, promptPreview: "待回溯步骤" }]);
      const view = renderChat();
      sessionItem(action === "rewind" ? "回溯…" : "分叉…");
      if (action === "rewind") fireEvent.click(await screen.findByRole("button", { name: /回溯到此处/ }));
      expect(screen.getByRole("button", { name: action === "rewind" ? "确认回溯" : "创建分叉" })).toBeEnabled();
      setStore(patch);
      view.rerender(<ThemeProvider><ChatView {...baseProps} modelLoading={loading} /></ThemeProvider>);
      expect(screen.queryByRole("button", { name: "确认回溯" })).toBeNull();
      expect(screen.queryByRole("button", { name: "创建分叉" })).toBeNull();
      fireEvent.click(screen.getByRole("button", { name: "任务操作" }));
      expect(screen.getByRole("menuitem", { name: "回溯…" })).toBeDisabled();
      expect(screen.getByRole("menuitem", { name: "分叉…" })).toBeDisabled();
      setStore({ streaming: false, sendNowPending: false, control: undefined });
      view.rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
      await act(async () => {});
      expect(screen.queryByRole("button", { name: "确认回溯" })).toBeNull();
      expect(screen.queryByRole("button", { name: "创建分叉" })).toBeNull();
      expect(rewindExecute).not.toHaveBeenCalled();
      expect(sessionFork).not.toHaveBeenCalled();
    });
  });

  it("React 提交更新前也会检查最新运行状态，不发送强制回溯", async () => {
    vi.mocked(rewindPoints).mockResolvedValue([{ promptIndex: 0 }]);
    renderChat();
    sessionItem("回溯…");
    fireEvent.click(await screen.findByRole("button", { name: /回溯到此处/ }));
    setStore({ streaming: true });
    fireEvent.click(screen.getByRole("button", { name: "确认回溯" }));
    await act(async () => {});
    expect(rewindExecute).not.toHaveBeenCalled();
  });

  it.each([true, false])("消息工具入口离开工作视图后消费旧快照，并重新记录后续阅读位置（跟随=%s）", following => {
    setStore({ messages: [
      { id: "u-tool", role: "user", complete: true, parts: [{ kind: "text", text: "更新文档" }] },
      { id: "a-tool", role: "assistant", complete: true, parts: [{ kind: "tool_call", toolCall: {
        toolCallId: "write-report", kind: "edit", title: "更新交付文档", status: "completed", content: [],
      } }, { kind: "text", text: "已完成文档" }] },
    ] });
    const view = renderChat();
    const viewport = view.container.querySelector<HTMLElement>(".chatview__scroll")!;
    Object.defineProperty(viewport, "scrollHeight", { configurable: true, value: 2000 });
    Object.defineProperty(viewport, "clientHeight", { configurable: true, value: 400 });
    setStore({ messages: [...storeState.messages] });
    view.rerender(<ThemeProvider><ChatView {...baseProps} /></ThemeProvider>);
    if (!following) {
      viewport.scrollTop = 600;
      fireEvent.wheel(viewport, { deltaY: -1 });
      fireEvent.scroll(viewport);
    }
    workspaceItem("文件变更");
    expect(viewport.scrollTop).toBe(following ? 1600 : 600);
    const process = view.container.querySelector<HTMLButtonElement>(".execution-process__header")!;
    if (process.getAttribute("aria-expanded") !== "true") fireEvent.click(process);
    fireEvent.click(screen.getByRole("button", { name: /edit 更新交付文档 完成/ }));
    expect(viewport.scrollTop).toBe(following ? 1600 : 600);
    expect(screen.queryByRole("region", { name: "任务工作视图" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "关闭面板" }));
    viewport.scrollTop = 900;
    fireEvent.wheel(viewport, { deltaY: -1 });
    fireEvent.scroll(viewport);
    workspaceItem("文件变更");
    fireEvent.click(screen.getByRole("button", { name: "关闭面板" }));
    expect(viewport.scrollTop).toBe(900);
    expect(screen.getByRole("button", { name: "回到最新消息并恢复自动跟随" })).toBeInTheDocument();
  });
});


describe("所有工具模式统一使用会话交互", () => {
  beforeEach(() => {
    setStore({ sessionId:"unified",messages:[{id:"u",role:"user",complete:true,parts:[{kind:"text",text:"整理报告"}]},
      {id:"a",role:"assistant",complete:false,parts:[]}],streaming:true,streamingMessageId:"a",sendNowPending:false,error:null,plan:null,planApproval:undefined,control:undefined });
    baseProps.onSend.mockReset();baseProps.onCancel.mockReset();baseProps.onToast.mockClear();
  });
  it.each(["default","browser_use","computer_use"] as const)("%s 运行时只显示普通停止按钮，没有专属状态栏", mode => {
    setStore({agentMode:mode});const {container}=renderChat();
    expect(screen.getByRole("button",{name:"停止生成"})).toBeEnabled();
    expect(container.querySelector(".task-execution-status")).toBeNull();
    expect(container.querySelector(".chatview__footer .session-turn-feedback")).toBeNull();
    expect(screen.queryByText(/正在处理任务，可随时说|可继续补充任务要求|浏览器数据与访问范围/)).toBeNull();
  });
  it.each(["default","browser_use","computer_use"] as const)("%s 暂停反馈位于对应回复，继续只可信发送一次且保留草稿", async mode => {
    setStore({agentMode:mode,streaming:false,streamingMessageId:null,control:{action:"pause",phase:"paused",requestedAt:1},
      messages:[{id:"a",role:"assistant",complete:true,stopReason:"cancelled",parts:[{kind:"text",text:"已整理的内容"}]}]});
    let finish!:(value:boolean)=>void;baseProps.onSend.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;}));
    const {container}=renderChat();const input=screen.getByRole("textbox");fireEvent.change(input,{target:{value:"保留的草稿"}});
    const resume=screen.getByRole("button",{name:"继续任务"});expect(resume.closest('[data-msg-id="a"]')).not.toBeNull();expect(resume.closest(".chatview__footer")).toBeNull();
    fireEvent.click(resume);fireEvent.click(resume);expect(baseProps.onSend).toHaveBeenCalledTimes(1);expect(baseProps.onSend).toHaveBeenCalledWith("继续",[]);
    await act(async()=>finish(false));expect(input).toHaveValue("保留的草稿");expect(screen.getByRole("button",{name:"继续任务"})).toBeEnabled();
    expect(container.querySelectorAll(".session-turn-feedback")).toHaveLength(1);expect(automationResume).not.toHaveBeenCalled();
    expect(screen.queryByRole("button",{name:/^重试$|^重新执行$/})).toBeNull();
  });
  it("重启后的电脑任务通过普通轮次入口恢复，切换会话不泄露旧暂停状态", async () => {
    setStore({agentMode:"computer_use",streaming:false,streamingMessageId:null,messages:[{id:"a",role:"assistant",complete:true,parts:[{kind:"text",text:"已有内容"}]}]});
    automationState.status={sessionId:"unified",mode:"computer_use",paused:true,task:{phase:"paused",reason:"application_restarted",scope:"编辑报告",authorized:false,generation:2},
      allowPrivateNetwork:false,browser:{available:true},browserRunning:false,browserHasData:false,computer:{available:true,platform:"macOS",screenCapture:true,inputControl:true}};
    baseProps.onSend.mockResolvedValue(true);const view=renderChat();
    fireEvent.click(screen.getByRole("button",{name:"继续任务"}));await waitFor(()=>expect(baseProps.onSend).toHaveBeenCalledWith("继续",[]));
    setStore({sessionId:"other",messages:[],control:undefined});view.rerender(<ThemeProvider><ChatView {...baseProps}/></ThemeProvider>);
    expect(screen.queryByRole("button",{name:"继续任务"})).toBeNull();
  });
  it("具体自动化授权沿用普通卡片，等待期间不能提前发送队列消息", () => {
    setStore({agentMode:"browser_use"});automationState.approvals=[{requestId:"r",sessionId:"unified",tool:"browser_click",title:"是否允许发送？",description:"核对收件人",details:{}}];
    renderChat();expect(screen.getAllByRole("region",{name:"操作授权"})).toHaveLength(1);expect(screen.queryByText("正在等待模型响应")).toBeNull();
    fireEvent.change(screen.getByRole("textbox"),{target:{value:"待发送的消息"}});
    expect(screen.getByRole("button",{name:"加入待发送队列"})).toBeDisabled();
  });
  it("没有可恢复的电脑任务上下文时引导补充要求，不提供无效的继续按钮", () => {
    setStore({agentMode:"computer_use",streaming:false,streamingMessageId:null,control:{action:"pause",phase:"paused",requestedAt:1}});
    automationState.status={sessionId:"unified",mode:"computer_use",paused:true,allowPrivateNetwork:false,browser:{available:true},browserRunning:false,browserHasData:false,computer:{available:true,platform:"macOS",screenCapture:true,inputControl:true}};
    renderChat();expect(screen.getByText("任务已暂停，可发送新消息说明接下来要做什么")).toBeInTheDocument();
    expect(screen.queryByRole("button",{name:"继续任务"})).toBeNull();expect(screen.getByRole("textbox")).toBeEnabled();
  });

});
