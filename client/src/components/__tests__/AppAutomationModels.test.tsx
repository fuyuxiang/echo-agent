import { useAutomationEditorStore } from "@/stores/automation-editor-store";
import { buildDraft } from "../automation/schedule-utils";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ComponentProps, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
const recorder = vi.hoisted(() => ({
  current: vi.fn(), stop: vi.fn(), subscribe: vi.fn(() => () => {}),
  syncNative: vi.fn(async () => {}), setPaused: vi.fn(async () => {}),
  prepareForExit: vi.fn(async () => {}), resumeAfterExitFailure: vi.fn(),
}));
vi.mock("@/lib/meeting-minutes", async (original) => ({
  ...await original<object>(), meetingRecorder: recorder,
}));
vi.mock("@/lib/kb-source-storage", () => ({ hydrateKnowledgeSources: vi.fn(async () => {}) }));
vi.mock("@/lib/artifact-catalog", () => ({ indexTaskArtifacts: vi.fn() }));
vi.mock("@/stores/projects-store", async (original) => ({
  ...await original<object>(), hydrateProjectsFromBackend: vi.fn(async () => {}),
}));
vi.mock("../ThemeProvider", () => ({ ThemeProvider: ({ children }: { children: ReactNode }) => children }));
vi.mock("../TitleBar", () => ({ TitleBar: () => null }));
vi.mock("../HomePage", () => ({ HomePage: () => <div>首页</div> }));
vi.mock("../PlaceholderPage", () => ({ PlaceholderPage: ({
  label,
  onStartProjectConversation,
}: {
  label: string;
  onStartProjectConversation?: (projectId: string, message: string, modelId?: string) => Promise<string | undefined>;
}) => label === "项目" ? (
  <button onClick={() => void onStartProjectConversation?.("project-1", "执行项目任务", "model-a")}>
    执行项目任务
  </button>
) : null }));
vi.mock("../TasksPanel", () => ({ TasksPanel: () => null }));
vi.mock("../SecondarySidebar", () => ({ SecondarySidebar: () => null }));
vi.mock("../TopbarActions", () => ({ TopbarActions: () => null }));
vi.mock("../FolderTrustDialog", () => ({ FolderTrustDialog: () => null }));
vi.mock("../Sidebar", () => ({ Sidebar: ({
  onSelect,
  onNavigate,
}: {
  onSelect: (id: string) => void;
  onNavigate: (label: string) => void;
}) => (
  <>
    <button onClick={() => onSelect("auto-1")}>打开自动化</button>
    <button onClick={() => onSelect("other")}>打开其他任务</button>
    <button onClick={() => onNavigate("项目")}>打开项目页</button>
  </>
) }));
// Keep the real shell, event handlers and stores; expose its composer contract.
vi.mock("../ChatView", () => ({ ChatView: (props: ComponentProps<typeof import("../ChatView").ChatView>) => (
  <div>
    <span data-testid="model">{props.modelId ?? "unknown"}</span>
    {!props.apiReady && <div role="status">{props.setupHint}</div>}
    <button disabled={!props.apiReady} onClick={() => props.onSend("继续解释")}>追问</button>
    <button onClick={() => props.onModelChange?.("model-b")}>切换模型</button>
    <button onClick={() => void props.onCancel("stop")}>停止执行</button>
  </div>
) }));
vi.mock("@/lib/agent-client", async (original) => ({
  ...await original<object>(),
  agentInit: vi.fn(), agentAuthStatus: vi.fn(), providersList: vi.fn(),
  agentListAllSessions: vi.fn(async () => []), agentListWorkspaces: vi.fn(async () => []),
  agentPrepareTaskWorkspace: vi.fn(async () => "/workspace/Tasks/project-task"),
  agentLoadSession: vi.fn(), agentNewSession: vi.fn(async () => "project-session"),
  agentSend: vi.fn(async () => {}), agentSetModel: vi.fn(async () => {}),
  projectsSave: vi.fn(async () => {}),
  internalReload: vi.fn(async () => {}), subscribeAgentEvents: vi.fn(),
}));

import App from "@/App";
import { saveRecoverySessionId, readRecoverySessionId } from "@/lib/recovery-context";
import * as client from "@/lib/agent-client";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";
import { useProjectsStore } from "@/stores/projects-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";
import { useMessageQueueStore } from "@/stores/message-queue-store";

let handlers: Parameters<typeof client.subscribeAgentEvents>[0];
const auth = { ready: true, runtimeReady: true, synchronized: true, providers: ["model-a", "model-b"], runtimeModels: ["model-a", "model-b"], defaultModelId: "model-b" };
function created(currentModelId?: string) {
  act(() => handlers.onAutomationUpdate?.({ phase: "sessionCreated", automationId: "a", automationName: "每日任务", recordId: "r", status: "running", sessionId: "auto-1", cwd: "/workspace", currentModelId }));
}
async function start() {
  render(<App />);
  await screen.findByText("首页");
  await waitFor(() => expect(client.providersList).toHaveBeenCalled());
  // Flush both catalog promises before selecting a task.
  await act(async () => {});
}
beforeEach(() => {
  useAutomationEditorStore.getState().close();
  vi.clearAllMocks();
  localStorage.clear();
  useSessionsStore.setState(useSessionsStore.getInitialState());
  useSessionStore.setState(useSessionStore.getInitialState());
  usePermissionStore.getState().clearAll();
  useQuestionStore.getState().clearAll();
  useMessageQueueStore.setState({ queues: {} });
  recorder.current.mockReturnValue({ meeting: null, active: false, pendingSave: false });
  useProjectsStore.setState({
    ...useProjectsStore.getInitialState(),
    projects: [],
  });
  vi.mocked(client.agentInit).mockResolvedValue({ ok: true, cwd: "/workspace", auth } as never);
  vi.mocked(client.agentAuthStatus).mockResolvedValue(auth);
  vi.mocked(client.providersList).mockResolvedValue({ providers: [], models: [{ modelId: "model-a", name: "模型 A", providerId: "p" }, { modelId: "model-b", name: "模型 B", providerId: "p" }] } as never);
  vi.mocked(client.agentLoadSession).mockResolvedValue("model-a");
  vi.mocked(client.agentListAllSessions).mockResolvedValue([]);
  vi.mocked(client.subscribeAgentEvents).mockImplementation(async (value) => { handlers = value; return () => {}; });
  vi.mocked(listen).mockImplementation(async () => () => {});
  vi.mocked(invoke).mockImplementation(async (command) => command === "notification_take_pending_opens" ? [] : null);
});

describe("自动化会话模型同步", () => {
  it("沿用专属会话的新轮次同步实际模型，旧停止事件不能覆盖新轮次", async () => {
    await start(); created("model-a");
    act(() => handlers.onAutomationUpdate?.({ phase: "sessionCreated", automationId: "a", automationName: "每日任务", recordId: "r-next", status: "running", sessionId: "auto-1", cwd: "/workspace", currentModelId: "model-b" }));
    expect(useSessionsStore.getState().independent[0].currentModelId).toBe("model-b");
    act(() => handlers.onAutomationUpdate?.({ phase: "finished", automationId: "a", automationName: "每日任务", recordId: "r", status: "stopped", sessionId: "auto-1", cwd: "/workspace" }));
    expect(useSessionsStore.getState().independent[0].status).toBe("working");
    act(() => handlers.onAutomationUpdate?.({ phase: "finished", automationId: "a", automationName: "每日任务", recordId: "r-next", status: "stopped", sessionId: "auto-1", cwd: "/workspace" }));
    expect(useSessionsStore.getState().independent[0].status).toBe("stopped");
  });

  it("上一轮停止后新计划轮次仍接收消息、授权和问题，重复入场不解除本轮停止", async () => {
    await start(); created("model-a");
    act(() => {
      useSessionStore.getState().startStreaming("auto-1", "p-first");
      handlers.onComplete?.({ sessionId: "auto-1", promptId: "p-first", stopReason: "cancelled", cancelTrigger: "stop" });
      handlers.onAutomationUpdate?.({ phase: "finished", automationId: "a", automationName: "每日任务", recordId: "r", promptId: "p-first", status: "stopped", sessionId: "auto-1", cwd: "/workspace" });
    });
    expect(useSessionStore.getState().transcripts["auto-1"].control?.phase).toBe("stopped");
    const admission = { phase: "sessionCreated", automationId: "a", automationName: "每日任务", recordId: "r-next", promptId: "p-next", status: "running", sessionId: "auto-1", cwd: "/workspace", currentModelId: "model-b" };
    act(() => handlers.onAutomationUpdate?.(admission));
    expect(useSessionStore.getState().transcripts["auto-1"].pendingSendNowPromptId).toBeNull();
    expect(useSessionStore.getState().transcripts["auto-1"].streamingMessageId).not.toBeNull();
    act(() => {
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "user_message_chunk", content: { type: "text", text: "第二轮要求" }, _meta: { promptId: "p-next" } });
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "agent_message_chunk", content: { type: "text", text: "第二轮结果" }, _meta: { promptId: "p-next" } });
      handlers.onPermission?.({ sessionId: "auto-1", requestId: "permission-next", toolCallId: "tool", title: "第二轮授权", toolKind: "execute", options: [] });
      handlers.onQuestion?.({ sessionId: "auto-1", requestId: "question-next", toolCallId: "question", title: "第二轮问题", mode: "default", questions: [] });
    });
    const transcript = useSessionStore.getState().transcripts["auto-1"];
    expect(transcript.control).toBeUndefined();
    expect(transcript.messages.find(message => message.promptId === "p-next")?.parts).toContainEqual({ kind: "text", text: "第二轮结果" });
    expect(usePermissionStore.getState().queues["auto-1"]).toHaveLength(1);
    expect(useQuestionStore.getState().queues["auto-1"]).toHaveLength(1);
    act(() => {
      useSessionStore.getState().requestControl("auto-1", "stop", "p-next");
      handlers.onAutomationUpdate?.(admission);
    });
    expect(useSessionStore.getState().transcripts["auto-1"].control?.promptId).toBe("p-next");
  });

  it("运行完成通知迟到时不能覆盖后续手动追问的状态", async () => {
    await start(); created("model-a");
    act(() => {
      useSessionStore.getState().startStreaming("auto-1", "manual-followup");
      handlers.onAutomationUpdate?.({ phase: "finished", automationId: "a", automationName: "每日任务", recordId: "r", promptId: "scheduled", status: "success", sessionId: "auto-1", cwd: "/workspace" });
    });
    expect(useSessionsStore.getState().independent[0].status).toBe("working");
  });

  it.each(["pause", "stop"] as const)("入场事件迟到时不能解除用户对同一轮次的 %s", async (action) => {
    await start();
    act(() => {
      useSessionStore.getState().startStreaming("auto-1", "already-admitted");
      useSessionStore.getState().requestControl("auto-1", action, "already-admitted");
      useSessionStore.getState().confirmControl("auto-1", action);
      useSessionsStore.getState().upsert({ sessionId: "auto-1", cwd: "/workspace", status: action === "pause" ? "paused" : "stopped" });
      handlers.onAutomationUpdate?.({ phase: "sessionCreated", automationId: "a", automationName: "每日任务", recordId: "r-late", promptId: "already-admitted", status: "running", sessionId: "auto-1", cwd: "/workspace" });
    });
    expect(useSessionStore.getState().transcripts["auto-1"].control?.action).toBe(action);
    expect(useSessionsStore.getState().independent[0].status).toBe(action === "pause" ? "paused" : "stopped");
  });

  it("运行时错误关联真实轮次，迟到的旧错误不能覆盖新执行", async () => {
    await start();
    created("model-a");
    fireEvent.click(screen.getByText("打开自动化"));
    await waitFor(() => expect(useSessionStore.getState().sessionId).toBe("auto-1"));
    act(() => useSessionStore.getState().startStreaming("auto-1", "new-turn"));
    act(() => handlers.onTurnError?.({sessionId:"auto-1",promptId:"old-turn",kind:"error",detail:"stale failure"}));
    expect(useSessionStore.getState().error).toBeNull();
    act(() => handlers.onTurnError?.({sessionId:"auto-1",promptId:"new-turn",kind:"error",detail:"current failure"}));
    expect(useSessionStore.getState().runtimeErrorContext).toEqual({sessionId:"auto-1",promptId:"new-turn"});
    act(() => useSessionStore.getState().setError("加载历史失败"));
    expect(useSessionStore.getState().runtimeErrorContext).toBeNull();
  });
  it("模型额度错误保留实际原因，不猜测等待时间或丢失服务返回的说明", async () => {
    await start();
    created("model-a");
    fireEvent.click(screen.getByText("打开自动化"));
    await waitFor(() => expect(useSessionStore.getState().sessionId).toBe("auto-1"));
    act(() => handlers.onTurnError?.({ sessionId: "auto-1", kind: "rate_limit", detail: "five-hour quota exhausted" }));
    expect(useSessionStore.getState().error).toContain("额度已用尽");
    expect(useSessionStore.getState().error).not.toMatch(/1-2|新建会话/);
  });

  it.each(["cancelled", "error", "stop-race"])("%s 后待发送消息保持暂停，后续正常对话也不会擅自续发", async scenario => {
    await start();
    created("model-a");
    act(() => {
      useSessionStore.getState().startStreaming("auto-1", "current-prompt");
      useMessageQueueStore.getState().enqueue("auto-1", "等待用户恢复", ["/tmp/方案.docx"]);
      if (scenario === "stop-race") useSessionStore.getState().requestControl("auto-1", "stop", "current-prompt");
      handlers.onComplete?.({sessionId:"auto-1",promptId:"current-prompt",stopReason:scenario === "stop-race" ? "end_turn" : scenario});
    });
    expect(useMessageQueueStore.getState().getQueue("auto-1")).toMatchObject([
      {text:"等待用户恢复",status:"paused",attachments:["/tmp/方案.docx"]},
    ]);
    act(() => {
      useSessionStore.getState().startStreaming("auto-1", "later-prompt");
      handlers.onComplete?.({sessionId:"auto-1",promptId:"later-prompt",stopReason:"end_turn"});
    });
    expect(client.agentSend).not.toHaveBeenCalled();
    expect(useMessageQueueStore.getState().getQueue("auto-1")[0].status).toBe("paused");
  });

  it("停止确认先于完成事件到达时，也会暂停并保留待发送消息", async () => {
    await start();
    created("model-a");
    fireEvent.click(screen.getByRole("button",{name:"打开自动化"}));
    await screen.findByTestId("model");
    act(() => {
      useSessionStore.getState().startStreaming("auto-1", "current-prompt");
      useMessageQueueStore.getState().enqueue("auto-1", "保留的下一条");
    });
    fireEvent.click(screen.getByRole("button",{name:"停止执行"}));
    await waitFor(() => expect(useSessionStore.getState().transcripts["auto-1"].control?.phase).toBe("stopped"));
    expect(useMessageQueueStore.getState().getQueue("auto-1")).toMatchObject([{text:"保留的下一条",status:"paused"}]);
  });

  it.each(["durable-first", "public-first"])("双完成事件只推进一次队列且不干扰下一轮：%s", async order => {
    await start();
    created("model-a");
    act(() => {
      const queue = useMessageQueueStore.getState();
      queue.enqueue("auto-1", "首轮");
      queue.claimNext("auto-1", "first-prompt");
      queue.enqueue("auto-1", "下一轮");
      useSessionStore.getState().startStreaming("auto-1", "first-prompt");
      const durable = () => handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "turn_completed", prompt_id: "first-prompt", stop_reason: "end_turn" });
      const complete = () => handlers.onComplete?.({ sessionId: "auto-1", promptId: "first-prompt", stopReason: "end_turn" });
      if (order === "durable-first") { durable(); complete(); } else { complete(); durable(); }
      complete(); durable();
    });
    expect(client.agentSend).toHaveBeenCalledTimes(1);
    const next = useMessageQueueStore.getState().getQueue("auto-1");
    expect(next).toMatchObject([{ text: "下一轮", status: "sending" }]);
    const transcript = useSessionStore.getState().transcripts["auto-1"];
    expect(transcript.messages.find(message => message.id === transcript.streamingMessageId))
      .toMatchObject({ promptId: next[0].promptId, complete: false });
    expect(useSessionsStore.getState().independent.find(session => session.sessionId === "auto-1")?.status).toBe("working");
  });

  it("原生新轮等待审批时旧工具结果只更新历史工具卡", async () => {
    await start();
    created("model-a");
    act(() => {
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "tool_call", toolCallId: "old-tool", title: "读取旧文件", _meta: { promptId: "old-prompt" } });
      handlers.onComplete?.({ sessionId: "auto-1", promptId: "old-prompt", stopReason: "end_turn" });
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "tool_call", toolCallId: "new-tool", title: "执行方案", _meta: { promptId: "new-prompt" } });
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "plan_approval_request", requestId: "new-plan", toolCallId: "new-tool" });
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "tool_call_update", toolCallId: "old-tool", status: "completed", _meta: { promptId: "old-prompt" } });
    });
    const transcript = useSessionStore.getState().transcripts["auto-1"];
    expect(transcript.planApprovals).toHaveLength(1);
    expect(transcript.messages.flatMap(message => message.parts)).toContainEqual(expect.objectContaining({
      kind: "tool_call", toolCall: expect.objectContaining({ toolCallId: "old-tool", status: "completed" }),
    }));
    expect(useSessionsStore.getState().independent.find(session => session.sessionId === "auto-1")?.status).toBe("awaiting_approval");
  });

  it.each(["approval", "permission", "question"])("本轮工具进度不会掩盖仍待处理的 %s，关闭后才恢复运行", async interaction => {
    await start();
    created("model-a");
    const expectedStatus = interaction === "approval" ? "awaiting_approval" : interaction === "permission" ? "awaiting_permission" : "awaiting_answer";
    const progress = () => handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "tool_call_update", toolCallId: "current-tool", status: "in_progress", _meta: { promptId: "current-prompt" } });
    act(() => {
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "tool_call", toolCallId: "current-tool", title: "本轮工具", _meta: { promptId: "current-prompt" } });
      if (interaction === "approval") handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "plan_approval_request", requestId: "pending", toolCallId: "current-tool" });
      else if (interaction === "permission") handlers.onPermission?.({ sessionId: "auto-1", requestId: "pending", toolCallId: "current-tool", toolKind: "read", title: "需要授权", options: [] });
      else handlers.onQuestion?.({ sessionId: "auto-1", requestId: "pending", toolCallId: "current-tool", title: "需要回答", mode: "default", questions: [] });
      progress();
    });
    expect(useSessionsStore.getState().independent.find(session => session.sessionId === "auto-1")?.status).toBe(expectedStatus);
    act(() => {
      if (interaction === "approval") handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "plan_approval_resolved", requestId: "pending" });
      else if (interaction === "permission") handlers.onPermissionClosed?.({ sessionId: "auto-1", requestId: "pending" });
      else handlers.onQuestionClosed?.({ sessionId: "auto-1", requestId: "pending" });
      progress();
    });
    expect(useSessionsStore.getState().independent.find(session => session.sessionId === "auto-1")?.status).toBe("working");
  });

  it("并行交互逐一关闭时始终呈现仍需用户处理的状态", async () => {
    await start();
    created("model-a");
    const status = () => useSessionsStore.getState().independent.find(session => session.sessionId === "auto-1")?.status;
    act(() => {
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "plan_approval_request", requestId: "plan", toolCallId: "plan-tool" });
      handlers.onPermission?.({ sessionId: "auto-1", requestId: "permission", toolCallId: "permission-tool", toolKind: "read", title: "需要授权", options: [] });
      handlers.onQuestion?.({ sessionId: "auto-1", requestId: "question", toolCallId: "question-tool", title: "需要回答", mode: "default", questions: [] });
    });
    expect(status()).toBe("awaiting_answer");
    act(() => handlers.onQuestionClosed?.({ sessionId: "auto-1", requestId: "question" }));
    expect(status()).toBe("awaiting_permission");
    act(() => handlers.onPermissionClosed?.({ sessionId: "auto-1", requestId: "permission" }));
    expect(status()).toBe("awaiting_approval");
    act(() => handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "plan_approval_resolved", requestId: "plan" }));
    expect(status()).toBe("working");
  });

  it("迟到的无身份立即发送完成不能消费新轮正在发送的队列项", async () => {
    await start();
    created("model-a");
    act(() => {
      const store = useSessionStore.getState();
      store.setSession("auto-1");
      useMessageQueueStore.getState().enqueue("auto-1", "正在发送的新轮");
      useMessageQueueStore.getState().claimNext("auto-1", "current-prompt");
      store.startStreaming("auto-1", "current-prompt");
      handlers.onComplete?.({ sessionId: "auto-1", promptId: "", stopReason: "cancelled", cancelTrigger: "send_now" });
    });
    expect(useMessageQueueStore.getState().getQueue("auto-1")).toMatchObject([
      { status: "sending", promptId: "current-prompt" },
    ]);
    const transcript = useSessionStore.getState().transcripts["auto-1"];
    expect(transcript.messages.find(message => message.id === transcript.streamingMessageId))
      .toMatchObject({ promptId: "current-prompt", complete: false });
    expect(useSessionsStore.getState().independent.find(session => session.sessionId === "auto-1")?.status).toBe("working");
    expect(client.agentSend).not.toHaveBeenCalled();
  });

  it("迟到的具名完成仍结算自己的队列项并保留替代轮", async () => {
    await start();
    created("model-a");
    act(() => {
      const queue = useMessageQueueStore.getState();
      queue.enqueue("auto-1", "旧轮");
      queue.claimNext("auto-1", "old-prompt");
      queue.enqueue("auto-1", "替代轮");
      queue.claimNext("auto-1", "current-prompt");
      useSessionStore.getState().setSession("auto-1");
      useSessionStore.getState().startStreaming("auto-1", "current-prompt");
      handlers.onComplete?.({ sessionId: "auto-1", promptId: "old-prompt", stopReason: "end_turn" });
    });
    expect(useMessageQueueStore.getState().getQueue("auto-1")).toMatchObject([
      { status: "sending", promptId: "current-prompt" },
    ]);
    expect(useSessionsStore.getState().independent.find(session => session.sessionId === "auto-1")?.status).toBe("working");
    expect(client.agentSend).not.toHaveBeenCalled();
  });

  it.each(["end_turn", "cancelled", "error"])("旧轮迟到的 %s 不覆盖新轮状态或推进队列", async stopReason => {
    await start();
    created("model-a");
    const store = useSessionStore.getState();
    act(() => {
      store.setSession("auto-1");
      store.startStreaming("auto-1", "old-prompt");
      store.markComplete({ sessionId: "auto-1", promptId: "old-prompt", stopReason: "end_turn" });
      store.startStreaming("auto-1", "current-prompt");
      useSessionsStore.getState().upsert({ sessionId: "auto-1", status: "working" });
      useMessageQueueStore.getState().enqueue("auto-1", "尚未授权续发的下一条");
      handlers.onComplete?.({ sessionId: "auto-1", promptId: "old-prompt", stopReason });
    });
    expect(useSessionsStore.getState().independent.find(s => s.sessionId === "auto-1")?.status).toBe("working");
    const transcript = useSessionStore.getState().transcripts["auto-1"];
    const current = transcript.messages.find(message => message.promptId === "current-prompt");
    expect(current).toMatchObject({ complete: false });
    expect(transcript.streamingMessageId).toBe(current?.id);
    expect(useMessageQueueStore.getState().getQueue("auto-1")).toMatchObject([{ status: "queued" }]);
    expect(client.agentSend).not.toHaveBeenCalled();
  });

  it("空回复的重复完成信号不能再次推进队列", async () => {
    await start();
    created("model-a");
    const store = useSessionStore.getState();
    act(() => {
      store.setSession("auto-1");
      store.startStreaming("auto-1", "empty-prompt");
      handlers.onComplete?.({ sessionId: "auto-1", promptId: "empty-prompt", stopReason: "end_turn" });
      useMessageQueueStore.getState().enqueue("auto-1", "留待用户操作");
      handlers.onComplete?.({ sessionId: "auto-1", promptId: "empty-prompt", stopReason: "end_turn" });
    });
    expect(client.agentSend).not.toHaveBeenCalled();
    expect(useMessageQueueStore.getState().getQueue("auto-1")).toMatchObject([{ status: "queued" }]);
  });

  it("已关闭审批的迟到请求不恢复 App 等待状态", async () => {
    await start();
    created("model-a");
    act(() => {
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "plan_approval_resolved", requestId: "closed-plan" });
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "plan_approval_request", requestId: "closed-plan", toolCallId: "plan-tool" });
    });
    expect(useSessionStore.getState().transcripts["auto-1"].planApprovals).toEqual([]);
    expect(useSessionsStore.getState().independent.find(s => s.sessionId === "auto-1")?.status).toBe("working");
  });

  it("原生验收就绪通知不依赖可被后台窗口暂停的动画帧", async () => {
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    try {
      await start();
      await waitFor(() => expect(invoke).toHaveBeenCalledWith("desktop_validation_ready"));
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("正常退出先确认未保存的自动化草稿，取消或退出失败都保留输入", async () => {
    const desktop = new Map<string, (payload: unknown) => void>();
    vi.mocked(listen).mockImplementation(async (event, handler) => {
      desktop.set(event, payload => handler({ payload } as never)); return () => {};
    });
    vi.mocked(invoke).mockImplementation(async command => command === "desktop_exit_prepared" ? true
      : command === "notification_take_pending_opens" ? [] : null);
    const editor = useAutomationEditorStore.getState();
    editor.begin(buildDraft(undefined, "/workspace"));
    editor.setDraft(draft => draft && ({ ...draft, name: "退出前保留" }));
    await start();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("desktop_exit_listener_ready"));
    act(() => desktop.get("desktop://prepare-exit")?.({ requestId: "draft-cancel" }));
    let dialog = await screen.findByRole("alertdialog", { name: "退出前舍弃自动化草稿？" });
    expect(screen.queryByText("正在保存录音与任务数据并准备退出…")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith("desktop_exit_prepared", { requestId: "draft-cancel", error: null });
    fireEvent.click(within(dialog).getByRole("button", { name: "继续编辑" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("desktop_exit_prepared", { requestId: "draft-cancel", error: "退出已取消，自动化草稿已保留。" }));
    act(() => desktop.get("desktop://exit-failed")?.({ requestId: "draft-cancel", message: "退出已取消" }));
    expect(useAutomationEditorStore.getState().draft?.name).toBe("退出前保留");
    act(() => desktop.get("desktop://prepare-exit")?.({ requestId: "draft-confirm" }));
    dialog = await screen.findByRole("alertdialog", { name: "退出前舍弃自动化草稿？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "舍弃修改并退出" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("desktop_exit_prepared", { requestId: "draft-confirm", error: null }));
    act(() => desktop.get("desktop://exit-failed")?.({ requestId: "draft-confirm", message: "保存其他任务失败" }));
    expect(useAutomationEditorStore.getState().draft?.name).toBe("退出前保留");
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("退出请求超时会关闭草稿确认，迟到确认不能停止录音或确认旧退出", async () => {
    const desktop = new Map<string, (payload: unknown) => void>();
    vi.mocked(listen).mockImplementation(async (event, handler) => {
      desktop.set(event, payload => handler({ payload } as never)); return () => {};
    });
    const editor = useAutomationEditorStore.getState();
    editor.begin(buildDraft()); editor.setDraft(draft => draft && ({ ...draft, name: "超时仍保留" }));
    await start();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("desktop_exit_listener_ready"));
    act(() => desktop.get("desktop://prepare-exit")?.({ requestId: "draft-timeout" }));
    await screen.findByRole("alertdialog", { name: "退出前舍弃自动化草稿？" });
    act(() => desktop.get("desktop://exit-failed")?.({ requestId: "draft-timeout", message: "退出准备超时" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(useAutomationEditorStore.getState().draft?.name).toBe("超时仍保留");
    expect(recorder.stop).not.toHaveBeenCalled();
    expect(vi.mocked(invoke).mock.calls.some(([name, args]) => name === "desktop_exit_prepared" && (args as { requestId?: string })?.requestId === "draft-timeout")).toBe(false);
  });

  it("已删除会话的迟到事件不能重建消息、审批或问题", async () => {
    await start();
    created("model-a");
    act(() => {
      useSessionsStore.getState().remove("auto-1");
      useSessionStore.getState().dropSessionCache("auto-1");
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "agent_message_chunk", content: { type: "text", text: "迟到内容" } } as never);
      handlers.onComplete?.({ sessionId: "auto-1", stopReason: "end_turn" } as never);
      handlers.onSummary?.({ sessionId: "auto-1", title: "已删除任务" });
      handlers.onPermission?.({ sessionId: "auto-1", requestId: "late-permission", options: [] } as never);
      handlers.onQuestion?.({ sessionId: "auto-1", requestId: "late-question", questions: [] } as never);
    });
    expect(useSessionStore.getState().transcripts["auto-1"]).toBeUndefined();
    expect(usePermissionStore.getState().queues["auto-1"]).toBeUndefined();
    expect(useQuestionStore.getState().queues["auto-1"]).toBeUndefined();
    expect(useSessionsStore.getState().independent.some((entry) => entry.sessionId === "auto-1")).toBe(false);
  });

  it("实际App退出监听将保存失败关联请求ID，并在失败后解除遮罩", async () => {
    const desktop = new Map<string, (payload: unknown) => void>();
    vi.mocked(listen).mockImplementation(async (event, handler) => {
      desktop.set(event, (payload) => handler({ payload } as never));
      return () => {};
    });
    vi.mocked(invoke).mockImplementation(async (command) => command === "desktop_exit_prepared" ? true
      : command === "notification_take_pending_opens" ? [] : null);
    recorder.current.mockReturnValue({ meeting: { id: "m" }, active: true, pendingSave: true });
    recorder.stop.mockRejectedValueOnce(new Error("磁盘已满"));
    await start();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("desktop_exit_listener_ready"));
    act(() => desktop.get("desktop://prepare-exit")?.({ requestId: "recording-exit" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("desktop_exit_prepared", { requestId: "recording-exit", error: "磁盘已满" }));
    expect(screen.getByText("正在保存录音与任务数据并准备退出…")).toBeInTheDocument();
    act(() => desktop.get("desktop://exit-failed")?.({ requestId: "recording-exit", message: "未退出：录音保存失败" }));
    expect(screen.queryByText("正在保存录音与任务数据并准备退出…")).toBeNull();
    expect(await screen.findByText("未退出：录音保存失败")).toBeInTheDocument();
  });

  it("实际App监听注册后重放已挂起退出请求，不漏掉早期通知", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => command === "desktop_exit_listener_ready"
      ? { requestId: "early-exit", recordingSaved: false }
      : command === "desktop_exit_prepared" ? true
      : command === "notification_take_pending_opens" ? [] : null);
    await start();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("desktop_exit_prepared", { requestId: "early-exit", error: null }));
    expect(screen.getByText("正在保存录音与任务数据并准备退出…")).toBeInTheDocument();
    expect(recorder.stop).not.toHaveBeenCalled();
  });

  it("安装失败后的Runtime恢复重新加载当前任务而非仅刷新模型", async () => {
    const desktop = new Map<string, () => void>();
    vi.mocked(listen).mockImplementation(async (event, handler) => {
      desktop.set(event, () => handler({ payload: null } as never));
      return () => {};
    });
    await start();
    created("model-a");
    act(() => {
      useSessionsStore.getState().setCurrent("auto-1");
      useSessionStore.getState().setSession("auto-1");
    });
    vi.mocked(client.agentListAllSessions).mockResolvedValue([...useSessionsStore.getState().independent]);
    act(() => desktop.get("desktop://runtime-restart-required")?.());
    await waitFor(() => expect(client.agentInit).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(client.agentLoadSession).toHaveBeenCalledWith("auto-1", "/workspace"));
    expect(useSessionsStore.getState().currentSessionId).toBe("auto-1");
  });

  it("引擎异常退出后可在原应用重试初始化，并保留会话目录", async () => {
    await start();
    created("model-a");
    act(() => handlers.onAgentDied?.({ reason: "isolated runtime failure" }));
    const retry = await screen.findByRole("button", { name: "重试初始化" });
    expect(useSessionsStore.getState().independent.find((entry) => entry.sessionId === "auto-1")?.status).toBe("failed");
    // Reinitialization reloads the durable native catalog, rather than treating
    // renderer state as the only copy of an actual saved session.
    vi.mocked(client.agentListAllSessions).mockResolvedValue([...useSessionsStore.getState().independent]);
    fireEvent.click(retry);
    await screen.findByText("首页");
    expect(client.agentInit).toHaveBeenCalledTimes(2);
    expect(useSessionsStore.getState().independent.some((entry) => entry.sessionId === "auto-1")).toBe(true);
  });

  it("新文档恢复原任务，核验目录并加载历史后才允许继续发送", async () => {
    saveRecoverySessionId("auto-1");
    useSessionsStore.getState().setDraft("auto-1", "尚未发送的草稿");
    vi.mocked(invoke).mockImplementation(async command => command === "desktop_recovery_status"
      ? { phase: "restored" } : command === "notification_take_pending_opens" ? [] : null);
    vi.mocked(client.agentListAllSessions).mockResolvedValue([
      { sessionId: "auto-1", cwd: "/actual-workspace", title: "原任务", currentModelId: "model-a" },
    ]);
    let finish!: (model: string) => void;
    vi.mocked(client.agentLoadSession).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    render(<App />);
    await waitFor(() => expect(client.agentLoadSession).toHaveBeenCalledWith("auto-1", "/actual-workspace"));
    expect(screen.queryByText("追问")).toBeNull();
    await act(async () => finish("model-a"));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    expect(useSessionsStore.getState().currentSessionId).toBe("auto-1");
    expect(useSessionsStore.getState().drafts["auto-1"]).toBe("尚未发送的草稿");
    expect(client.agentSend).not.toHaveBeenCalled();
    expect(client.agentInit).toHaveBeenCalledTimes(1);
  });

  it.each(["stopped", "working"] as const)("恢复 %s 任务时只结束已中断的历史轮次，保留仍在执行的任务", async status => {
    saveRecoverySessionId("auto-1");
    vi.mocked(invoke).mockImplementation(async command => command === "desktop_recovery_status"
      ? { phase: "restored" } : command === "notification_take_pending_opens" ? [] : null);
    vi.mocked(client.agentListAllSessions).mockResolvedValue([
      { sessionId: "auto-1", cwd: "/workspace", title: "原任务", status, currentModelId: "model-a" },
    ]);
    vi.mocked(client.agentLoadSession).mockImplementationOnce(async () => {
      handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "agent_message_chunk",
        content: { type: "text", text: "恢复的回复" }, _meta: { isReplay: true } } as never);
      return "model-a";
    });
    render(<App />);
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    expect(useSessionStore.getState().streaming).toBe(status === "working");
    expect(client.agentSend).not.toHaveBeenCalled();
  });

  it("恢复首次回放中途失败后，重试重新加载完整历史而不保留半份缓存", async () => {
    saveRecoverySessionId("auto-1");
    vi.mocked(invoke).mockImplementation(async command => command === "desktop_recovery_status"
      ? { phase: "restored" } : command === "notification_take_pending_opens" ? [] : null);
    vi.mocked(client.agentListAllSessions).mockResolvedValue([
      { sessionId: "auto-1", cwd: "/workspace", title: "原任务", status: "stopped", currentModelId: "model-a" },
    ]);
    const replay = (text: string) => handlers.onUpdate?.({ __sessionId: "auto-1", sessionUpdate: "agent_message_chunk",
      content: { type: "text", text }, _meta: { isReplay: true, promptIndex: 3 } } as never);
    vi.mocked(client.agentLoadSession).mockImplementationOnce(async () => {
      replay("不完整历史"); throw new Error("历史读取中断");
    }).mockImplementationOnce(async () => { replay("完整历史"); return "model-a"; });
    render(<App />);
    await screen.findByText(/历史读取中断/);
    fireEvent.click(screen.getByRole("button", { name: "重试初始化" }));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    const texts = useSessionStore.getState().messages.flatMap(message => message.parts.flatMap(part => part.kind === "text" ? [part.text] : []));
    expect(texts).toEqual(["完整历史"]);
    expect(client.agentSend).not.toHaveBeenCalled();
  });

  it("普通启动保留首页，不因上次任务记录自动加载会话", async () => {
    saveRecoverySessionId("auto-1");
    vi.mocked(invoke).mockImplementation(async command => command === "desktop_recovery_status"
      ? { phase: "ready" } : command === "notification_take_pending_opens" ? [] : null);
    await start();
    expect(client.agentLoadSession).not.toHaveBeenCalled();
    expect(useSessionsStore.getState().currentSessionId).toBeNull();
  });

  it("原任务已归档时回到首页并说明原因，不恢复已归档任务", async () => {
    saveRecoverySessionId("auto-1");
    vi.mocked(invoke).mockImplementation(async command => command === "desktop_recovery_status"
      ? { phase: "restored" } : command === "notification_take_pending_opens" ? [] : null);
    vi.mocked(client.agentListAllSessions).mockResolvedValue([
      { sessionId: "auto-1", cwd: "/workspace", title: "已归档任务", archived: true },
    ]);
    await start();
    expect(client.agentLoadSession).not.toHaveBeenCalled();
    expect(readRecoverySessionId()).toBeNull();
    expect(screen.getByText(/原任务已删除、归档或无法访问/)).toBeVisible();
  });

  it("恢复期间用户主动导航不会被迟到的恢复结果拉回旧任务", async () => {
    saveRecoverySessionId("auto-1");
    vi.mocked(invoke).mockImplementation(async command => command === "desktop_recovery_status"
      ? { phase: "restored" } : command === "notification_take_pending_opens" ? [] : null);
    let complete!: (entries: Awaited<ReturnType<typeof client.agentListAllSessions>>) => void;
    vi.mocked(client.agentListAllSessions).mockImplementationOnce(() => new Promise(resolve => { complete = resolve; }));
    render(<App />);
    await waitFor(() => expect(client.agentListAllSessions).toHaveBeenCalled());
    fireEvent.click(screen.getByText("打开项目页"));
    await act(async () => complete([{ sessionId: "auto-1", cwd: "/workspace", title: "原任务" }]));
    await screen.findByText("执行项目任务");
    expect(client.agentLoadSession).not.toHaveBeenCalled();
    expect(useSessionsStore.getState().currentSessionId).toBeNull();
  });

  it("引擎异常退出后自动重建并加载原会话，不重新发送任务", async () => {
    await start(); created("model-a");
    act(() => {
      useSessionsStore.getState().setCurrent("auto-1");
      useSessionStore.getState().setSession("auto-1");
    });
    vi.mocked(client.agentListAllSessions).mockResolvedValue([...useSessionsStore.getState().independent]);
    act(() => handlers.onAgentDied?.({ reason: "isolated automatic recovery" }));
    await screen.findByRole("button", { name: "停止自动恢复" });
    await waitFor(() => expect(client.agentInit).toHaveBeenCalledTimes(2), { timeout: 5000 });
    await waitFor(() => expect(client.agentLoadSession).toHaveBeenCalledWith("auto-1", "/workspace"));
    expect(client.agentSend).not.toHaveBeenCalled();
    expect(useSessionsStore.getState().currentSessionId).toBe("auto-1");
  });

  it("用户可以停止自动恢复，停止后不会在后台再次初始化", async () => {
    await start();
    vi.useFakeTimers();
    try {
      await act(async () => handlers.onAgentDied?.({ reason: "stopped recovery" }));
      fireEvent.click(screen.getByRole("button", { name: "停止自动恢复" }));
      await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
      expect(client.agentInit).toHaveBeenCalledTimes(1);
      expect(screen.getByRole("button", { name: "重试初始化" })).toBeVisible();
    } finally { vi.useRealTimers(); }
  });

  it("崩溃恢复先加载当前会话，加载完成后才允许继续发送", async () => {
    await start();
    created("model-a");
    fireEvent.click(screen.getByText("打开自动化"));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    let finishLoad!: (model: string) => void;
    vi.mocked(client.agentLoadSession).mockImplementationOnce(() => new Promise((resolve) => { finishLoad = resolve; }));
    vi.mocked(client.agentListAllSessions).mockResolvedValue([...useSessionsStore.getState().independent]);
    act(() => handlers.onAgentDied?.({ reason: "isolated crash" }));
    fireEvent.click(await screen.findByRole("button", { name: "重试初始化" }));
    await waitFor(() => expect(client.agentLoadSession).toHaveBeenCalledTimes(2));
    expect(screen.queryByText("追问")).toBeNull();
    await act(async () => finishLoad("model-a"));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    fireEvent.click(screen.getByText("追问"));
    expect(client.agentSend).toHaveBeenCalledWith("auto-1", "继续解释", [], "继续解释", expect.any(String));
  });

  it("会话恢复失败后保留上下文，并能再次重试恢复", async () => {
    await start(); created("model-a");
    fireEvent.click(screen.getByText("打开自动化"));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    vi.mocked(client.agentListAllSessions).mockResolvedValue([...useSessionsStore.getState().independent]);
    vi.mocked(client.agentLoadSession).mockRejectedValueOnce(new Error("会话读取暂时失败"));
    act(() => handlers.onAgentDied?.({ reason: "isolated crash" }));
    fireEvent.click(await screen.findByRole("button", { name: "重试初始化" }));
    await screen.findByText(/会话读取暂时失败/);
    expect(useSessionsStore.getState().currentSessionId).toBe("auto-1");
    fireEvent.click(screen.getByRole("button", { name: "重试初始化" }));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    expect(client.agentLoadSession).toHaveBeenCalledTimes(3);
  });

  it("定时任务运行事件不会把用户已暂停的会话改回运行中", async () => {
    await start();
    act(() => useSessionsStore.getState().upsert({
      sessionId: "auto-1",
      cwd: "/workspace",
      title: "已暂停任务",
      status: "paused",
    }));
    created("model-a");
    expect(useSessionsStore.getState().independent[0]).toMatchObject({
      sessionId: "auto-1",
      status: "paused",
      currentModelId: "model-a",
    });
  });

  it("自动化事件保存本次实际模型，打开后可以追问，不改用当前默认模型", async () => {
    await start();
    created("model-a");
    expect(useSessionsStore.getState().independent[0].currentModelId).toBe("model-a");
    fireEvent.click(screen.getByText("打开自动化"));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    expect(screen.getByTestId("model")).toHaveTextContent("model-a");
    expect(useSessionStore.getState().error).toBeNull();
    fireEvent.click(screen.getByText("追问"));
    expect(client.agentSend).toHaveBeenCalledWith("auto-1", "继续解释", [], "继续解释", expect.any(String));
  });

  it("旧事件缺少模型时，加载响应补齐；加载期间不误报未配置", async () => {
    let complete!: (id: string) => void;
    vi.mocked(client.agentLoadSession).mockImplementation(() => new Promise((resolve) => { complete = resolve; }));
    await start();
    created();
    fireEvent.click(screen.getByText("打开自动化"));
    expect(await screen.findByRole("status")).toHaveTextContent("正在加载会话信息");
    expect(screen.getByText("追问")).toBeDisabled();
    expect(useSessionStore.getState().error).toBeNull();
    await act(async () => complete("model-a"));
    expect(screen.getByText("追问")).toBeEnabled();
    expect(useSessionsStore.getState().independent[0].currentModelId).toBe("model-a");
  });

  it("重启后从持久化列表打开，运行时模型优先于陈旧摘要", async () => {
    vi.mocked(client.agentListAllSessions).mockResolvedValueOnce([{ sessionId: "auto-1", cwd: "/workspace", title: "历史自动化", currentModelId: "model-b" }]);
    await start();
    fireEvent.click(screen.getByText("打开自动化"));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    expect(screen.getByTestId("model")).toHaveTextContent("model-a");
  });

  it("兼容未返回模型的加载响应，保留创建事件的确定模型", async () => {
    vi.mocked(client.agentLoadSession).mockResolvedValue(null);
    await start();
    created("model-a");
    fireEvent.click(screen.getByText("打开自动化"));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    expect(screen.getByTestId("model")).toHaveTextContent("model-a");
  });

  it("没有任何权威模型信息时不猜默认模型，并允许用户恢复", async () => {
    vi.mocked(client.agentLoadSession).mockResolvedValue(null);
    await start();
    created();
    fireEvent.click(screen.getByText("打开自动化"));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("未能获取此会话的模型信息"));
    expect(screen.getByText("追问")).toBeDisabled();
    expect(useSessionStore.getState().error).toBeNull();
    fireEvent.click(screen.getByText("切换模型"));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
  });

  it("快速切换会话，迟到的加载响应不覆盖新会话模型", async () => {
    let complete!: (id: string) => void;
    vi.mocked(client.agentLoadSession).mockImplementation((id) => id === "auto-1" ? new Promise((resolve) => { complete = resolve; }) : Promise.resolve("model-b"));
    await start();
    created();
    act(() => useSessionsStore.getState().upsert({ sessionId: "other", cwd: "/workspace", currentModelId: "model-b" }));
    fireEvent.click(screen.getByText("打开自动化"));
    // Group ownership is resolved before loading. Establish the in-flight
    // load explicitly so this still tests a late model response, not a task
    // selection superseded before it ever reaches the Runtime.
    await waitFor(() => expect(client.agentLoadSession).toHaveBeenCalledWith("auto-1", "/workspace"));
    fireEvent.click(screen.getByText("打开其他任务"));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    await act(async () => complete("model-a"));
    expect(screen.getByTestId("model")).toHaveTextContent("model-b");
    expect(screen.getByText("追问")).toBeEnabled();
  });

  it("模型确实不可用时只显示恢复提示，选择有效模型后恢复追问", async () => {
    vi.mocked(client.agentLoadSession).mockResolvedValue("removed-model");
    await start();
    created("removed-model");
    fireEvent.click(screen.getByText("打开自动化"));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("removed-model"));
    expect(screen.getByText("追问")).toBeDisabled();
    fireEvent.click(screen.getByText("切换模型"));
    await waitFor(() => expect(screen.getByText("追问")).toBeEnabled());
    // A duplicate creation event must not undo a subsequent user model switch.
    created("model-a");
    expect(screen.getByTestId("model")).toHaveTextContent("model-b");
  });

  it("项目任务将用户指定的模型原样传入新会话并留存执行凭证", async () => {
    useProjectsStore.setState({
      projects: [{
        id: "project-1",
        name: "测试项目",
        cwd: "/workspace",
        defaultModelId: "model-b",
        createdAt: new Date().toISOString(),
        connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [], conversations: [],
      }],
    });
    await start();
    fireEvent.click(screen.getByText("打开项目页"));
    fireEvent.click(await screen.findByRole("button", { name: "执行项目任务" }));

    await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledWith("/workspace", "model-a"));
    await waitFor(() => expect(client.agentSend).toHaveBeenCalledWith(
      "project-session",
      expect.stringContaining("执行项目任务"),
      [],
      "执行项目任务",
      expect.any(String),
    ));
    await waitFor(() => expect(useProjectsStore.getState().projects[0].conversations[0]).toMatchObject({
      sessionId: "project-session",
      modelId: "model-a",
    }));
    expect(useSessionsStore.getState().independent).toEqual(expect.arrayContaining([
      expect.objectContaining({ sessionId: "project-session", currentModelId: "model-a" }),
    ]));
  });

  it("点击系统通知打开关联任务，并把通知标记为已读", async () => {
    let onNotificationOpen: (() => void) | undefined;
    let pending: Array<{ id: string; sessionId: string }> = [];
    vi.mocked(listen).mockImplementation(async (event, handler) => {
      if (event === "notification://opened") onNotificationOpen = () => handler({} as never);
      return () => {};
    });
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "notification_take_pending_opens") {
        const opens = pending;
        pending = [];
        return opens;
      }
      return null;
    });
    await start();
    act(() => useSessionsStore.getState().upsert({ sessionId: "auto-1", cwd: "/workspace", title: "日报" }));
    expect(onNotificationOpen).toBeDefined();
    pending = [{ id: "42", sessionId: "auto-1" }];
    await act(async () => onNotificationOpen?.());
    await waitFor(() => expect(client.agentLoadSession).toHaveBeenCalledWith("auto-1", "/workspace"));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("notification_mark_read", { id: 42 }));
  });
});
