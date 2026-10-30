import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ComponentProps, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/lib/kb-source-storage", () => ({ hydrateKnowledgeSources: vi.fn(async () => {}) }));
vi.mock("@/lib/artifact-catalog", () => ({ indexTaskArtifacts: vi.fn() }));
vi.mock("@/stores/projects-store", async (original) => ({
  ...await original<object>(), hydrateProjectsFromBackend: vi.fn(async () => {}),
}));
vi.mock("../ThemeProvider", () => ({ ThemeProvider: ({ children }: { children: ReactNode }) => children }));
vi.mock("../TitleBar", () => ({ TitleBar: () => null }));
vi.mock("../HomePage", () => ({ HomePage: (props: ComponentProps<typeof import("../HomePage").HomePage>) => <div>首页
  <button onClick={() => void props.onClientSlashCommand?.({ name: "flush", args: "" })}>无任务提取</button>
</div> }));
vi.mock("../SettingsPanel", () => ({ SettingsPanel: (props: ComponentProps<typeof import("../SettingsPanel").SettingsPanel>) => <div data-testid="memory-settings-context">{props.initialSection}:{props.sessionId}:{props.cwd}</div> }));
vi.mock("../PlaceholderPage", () => ({ PlaceholderPage: (props: ComponentProps<typeof import("../PlaceholderPage").PlaceholderPage>) => <button onClick={() => void props.onClientSlashCommand?.({ name: "flush", args: "" })}>非任务页提取</button> }));
vi.mock("../TasksPanel", () => ({ TasksPanel: () => null }));
vi.mock("../SecondarySidebar", () => ({ SecondarySidebar: () => null }));
vi.mock("../TopbarActions", () => ({ TopbarActions: () => null }));
vi.mock("../FolderTrustDialog", () => ({ FolderTrustDialog: () => null }));
vi.mock("../Sidebar", () => ({ Sidebar: ({ onSelect, onDeleteSession, onNavigate }: { onSelect: (id: string) => void; onDeleteSession?: (id: string, cwd: string) => Promise<void>; onNavigate?: (label: string) => void }) => <>
  <button onClick={() => onSelect("source")}>打开项目源会话</button>
  <button onClick={() => onSelect("other")}>打开其他会话</button>
  <button onClick={() => void onDeleteSession?.("source", "/workspace")}>删除项目源会话</button>
  <button onClick={() => onNavigate?.("项目")}>进入项目管理</button>
</> }));
// Real App lifecycle/store/persistence code; child widgets expose their public
// callback. This is a jsdom regression, not a Native or model acceptance.
vi.mock("../ChatView", () => ({ ChatView: (props: ComponentProps<typeof import("../ChatView").ChatView>) => <div>
  <span data-testid="active-title">{props.title}</span>
  <span data-testid="task-refresh">{props.taskRefreshSignal}</span>
  <button onClick={() => {const id=useSessionStore.getState().sessionId; if(id) void props.onPrepareRetry?.(id);}}>预检重试</button>
  <button onClick={() => void props.onForked?.("fork", "source", "/workspace")}>接收真实fork结果</button>
  <button onClick={() => void props.onSend("继续分叉")}>分叉下一轮</button>
  <button onClick={() => void props.onClientSlashCommand?.({ name: "flush", args: "" })}>提取任务记忆</button>
  <button onClick={() => void props.onClientSlashCommand?.({ name: "dream", args: "" })}>整理任务记忆</button>
  <button onClick={() => void props.onClientSlashCommand?.({ name: "flush", args: "invalid" })}>错误参数提取</button>
</div> }));
vi.mock("@/lib/agent-client", async (original) => ({
  ...await original<object>(), agentInit: vi.fn(), agentAuthStatus: vi.fn(), providersList: vi.fn(),
  agentListAllSessions: vi.fn(), agentListWorkspaces: vi.fn(async () => []),
  agentsValidateReferences: vi.fn(async () => {}),
  agentPrepareSend: vi.fn(async () => {}),
  agentLoadSession: vi.fn(async () => "model-a"), agentSend: vi.fn(async () => {}), projectsSave: vi.fn(async () => {}),
  agentDeleteSession: vi.fn(async () => ({ memorySummariesDeleted: 1, memoryCleanupWarning: null })),
  memoryFlush: vi.fn(async () => true), memoryDream: vi.fn(async () => true),
  internalReload: vi.fn(async () => {}), subscribeAgentEvents: vi.fn(async () => () => {}),
}));

import App from "@/App";
import * as client from "@/lib/agent-client";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";

const catalog = [
  { sessionId: "source", cwd: "/workspace", title: "项目源标题", currentModelId: "model-a", permissionMode: "ask" as const },
  { sessionId: "other", cwd: "/workspace", title: "其他标题", currentModelId: "model-a" },
];
const fork = { ...catalog[0], sessionId: "fork" };
const auth = { ready: true, runtimeReady: true, synchronized: true, providers: ["model-a"], runtimeModels: ["model-a"], defaultModelId: "model-a" };
const project = (pending: boolean): ProjectMeta => ({
  id: "project", name: "交付项目", cwd: "/project-workspace", instructions: "保留当前项目契约", createdAt: "2026-09-01T00:00:00Z",
  connectors: [], experts: [], skills: [], assets: [], members: [],
  plans: [{ id: "plan", title: "原计划", status: "completed", sessionId: "source" }],
  tasks: [{ id: "task", title: "原任务", scope: "personal", source: "user", status: "completed", sessionId: "source" }],
  conversations: [{ sessionId: "source", title: "项目源标题", createdAt: "2026-09-01T00:00:00Z", modelId: "model-a", pendingProjectContext: pending }],
});
async function start(pending = true) {
  useProjectsStore.setState({ projects: [project(pending)] }); render(<App />);
  await screen.findByText("首页"); await waitFor(() => expect(client.agentListAllSessions).toHaveBeenCalled()); await act(async () => {});
  fireEvent.click(screen.getByRole("button", { name: "打开项目源会话" })); await screen.findByTestId("active-title");
  await waitFor(() => expect(useSessionsStore.getState().currentSessionId).toBe("source"));
  // A fork copies persisted history. Its next prompt is not its first user turn.
  act(() => useSessionStore.getState().setMessages([{ id: "original-user", role: "user", complete: true, parts: [{ kind: "text", text: "已有历史" }] }]));
}
function completeFork() {
  vi.mocked(client.agentListAllSessions).mockResolvedValue([...catalog, fork]);
  fireEvent.click(screen.getByRole("button", { name: "接收真实fork结果" }));
}
beforeEach(() => {
  vi.clearAllMocks(); localStorage.clear();
  vi.mocked(client.agentDeleteSession).mockResolvedValue({ memorySummariesDeleted: 1, memoryCleanupWarning: null });
  useSessionsStore.setState(useSessionsStore.getInitialState()); useSessionStore.setState(useSessionStore.getInitialState());
  useProjectsStore.setState({ ...useProjectsStore.getInitialState(), projects: [] });
  vi.mocked(client.agentInit).mockResolvedValue({ ok: true, cwd: "/workspace", auth } as never);
  vi.mocked(client.agentAuthStatus).mockResolvedValue(auth);
  vi.mocked(client.providersList).mockResolvedValue({ providers: [], models: [{ modelId: "model-a", name: "模型 A", providerId: "p" }] } as never);
  vi.mocked(client.agentListAllSessions).mockResolvedValue(catalog);
  vi.mocked(client.agentLoadSession).mockReset().mockResolvedValue("model-a");
  vi.mocked(client.projectsSave).mockReset().mockResolvedValue(undefined);
  vi.mocked(client.agentSend).mockReset().mockResolvedValue(undefined);
  vi.mocked(client.agentPrepareSend).mockReset().mockResolvedValue(undefined);
  vi.mocked(client.memoryFlush).mockReset().mockResolvedValue(true);
  vi.mocked(client.memoryDream).mockReset().mockResolvedValue(true);
  vi.mocked(invoke).mockImplementation(async (command) => command === "notification_take_pending_opens" ? [] : null);
});

describe("manual memory commands", () => {
  it.each([true, false])("flush reports completion=%s without promising new content", async completed => {
    vi.mocked(client.memoryFlush).mockResolvedValueOnce(completed);
    await start(false);
    fireEvent.click(screen.getByRole("button", { name: "提取任务记忆" }));
    await waitFor(() => expect(client.memoryFlush).toHaveBeenCalledWith("source"));
    expect(await screen.findByText(completed
      ? "记忆提取已完成，可在记忆页面查看结果；没有新增内容时不会重复保存"
      : "本次提取未完成，请查看记忆处理状态后重试")).toBeVisible();
    expect(screen.getByRole("button", { name: "查看记忆" })).toBeInTheDocument();
    expect(screen.queryByText("正在提取记忆，完成后会提示结果")).toBeNull();
    expect(client.agentSend).not.toHaveBeenCalled();
  });

  it.each([true, false])("dream reports changed=%s without assuming an empty input", async changed => {
    vi.mocked(client.memoryDream).mockResolvedValueOnce(changed);
    await start(false);
    fireEvent.click(screen.getByRole("button", { name: "整理任务记忆" }));
    await waitFor(() => expect(client.memoryDream).toHaveBeenCalledWith("source"));
    expect(await screen.findByText(changed ? "长期记忆已更新" : "本次未更新长期记忆，可在记忆页面查看处理状态")).toBeVisible();
    expect(client.agentSend).not.toHaveBeenCalled();
  });

  it("reports failures with diagnostics and allows retry", async () => {
    vi.mocked(client.memoryFlush).mockRejectedValueOnce(new Error("TypeError: Failed to fetch"));
    await start(false);
    fireEvent.click(screen.getByRole("button", { name: "提取任务记忆" }));
    expect(await screen.findByText("记忆提取失败：暂时无法连接服务，请检查网络后重试")).toBeVisible();
    expect(screen.getByText("查看详情").closest("details")).toHaveTextContent("Failed to fetch");
    fireEvent.click(screen.getByRole("button", { name: "提取任务记忆" }));
    await waitFor(() => expect(client.memoryFlush).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("记忆提取已完成，可在记忆页面查看结果；没有新增内容时不会重复保存")).toBeVisible();
  });

  it("deduplicates in-flight maintenance and opens results in the originating task only after a click", async () => {
    let finish!: (completed: boolean) => void;
    vi.mocked(client.memoryFlush).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    await start(false);
    fireEvent.click(screen.getByRole("button", { name: "提取任务记忆" }));
    fireEvent.click(screen.getByRole("button", { name: "整理任务记忆" }));
    expect(client.memoryFlush).toHaveBeenCalledTimes(1);
    expect(client.memoryDream).not.toHaveBeenCalled();
    expect(screen.getByText("正在提取记忆，完成后会提示结果")).toBeVisible();
    expect(screen.getByText("当前任务正在处理记忆，请稍候")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "打开其他会话" }));
    await waitFor(() => expect(useSessionsStore.getState().currentSessionId).toBe("other"));
    await act(async () => finish(true));
    expect(useSessionsStore.getState().currentSessionId).toBe("other");
    expect(screen.queryByTestId("memory-settings-context")).toBeNull();
    expect(screen.getByText(/任务：项目源标题/)).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "查看记忆" }));
    expect(await screen.findByTestId("memory-settings-context")).toHaveTextContent("memory:source:/workspace");
  });

  it("background error copy is classified before adding a title containing an HTTP code", async () => {
    let fail!: (error: Error) => void;
    vi.mocked(client.memoryFlush).mockImplementationOnce(() => new Promise((_resolve, reject) => { fail = reject; }));
    await start(false);
    act(() => useSessionsStore.getState().upsert({ sessionId: "source", title: "HTTP 403 问题排查" }));
    fireEvent.click(screen.getByRole("button", { name: "提取任务记忆" }));
    fireEvent.click(screen.getByRole("button", { name: "打开其他会话" }));
    await waitFor(() => expect(useSessionsStore.getState().currentSessionId).toBe("other"));
    await act(async () => fail(new Error("TypeError: Failed to fetch")));
    expect(screen.getByText("记忆提取失败：暂时无法连接服务，请检查网络后重试（任务：HTTP 403 问题排查）")).toBeVisible();
    expect(screen.queryByText("没有访问权限，请检查授权")).toBeNull();
  });

  it("rejects missing task scope and unsupported arguments before invoking maintenance", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "无任务提取" }));
    expect(await screen.findByText("/flush 需要先打开任务会话")).toBeInTheDocument();
    expect(client.memoryFlush).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "打开项目源会话" }));
    await screen.findByTestId("active-title");
    fireEvent.click(screen.getByRole("button", { name: "错误参数提取" }));
    expect(screen.getByText("用法：/flush")).toBeInTheDocument();
    expect(client.memoryFlush).not.toHaveBeenCalled();
  });

  it("keeps automatic memory events quiet", async () => {
    await start(false);
    const subscriptions = vi.mocked(client.subscribeAgentEvents).mock.calls;
    const handlers = subscriptions[subscriptions.length - 1][0];
    act(() => {
      handlers.onUpdate?.({ sessionUpdate: "memory_flush_completed", result: "written", __sessionId: "source" } as never);
      handlers.onUpdate?.({ sessionUpdate: "memory_dream_completed", result: "written", __sessionId: "source" } as never);
    });
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("project management cannot run maintenance on a task left in the background", async () => {
    await start(false);
    fireEvent.click(screen.getByRole("button", { name: "进入项目管理" }));
    fireEvent.click(await screen.findByRole("button", { name: "非任务页提取" }));
    expect(screen.getByText("/flush 需要先打开任务会话")).toBeInTheDocument();
    expect(client.memoryFlush).not.toHaveBeenCalled();
  });
});

describe("App deletion feedback", () => {
  it.each([false, true])("deletes the task and project references, warning only for incomplete cleanup (%s)", async partial => {
    vi.mocked(client.agentDeleteSession).mockResolvedValue({ memorySummariesDeleted: 1, memoryCleanupWarning: partial ? "database busy" : null });
    await start(false);
    fireEvent.click(screen.getByRole("button", { name: "删除项目源会话" }));
    await waitFor(() => expect(useSessionsStore.getState().independent.some(s => s.sessionId === "source")).toBe(false));
    expect(client.agentDeleteSession).toHaveBeenCalledWith("source", "/workspace");
    expect(useProjectsStore.getState().projects[0].conversations).toHaveLength(0);
    expect(useSessionStore.getState().sessionId).toBeNull();
    expect(screen.queryByText(/对话与自动摘要已删除/)).not.toBeInTheDocument();
    if (partial) {
      expect(screen.getByRole("status")).toHaveTextContent("对话已删除，部分记忆未能清理");
      expect(screen.getByRole("button", { name: "查看记忆" })).toBeInTheDocument();
    } else expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});

describe("project fork continuation", () => {
  it("回复预检失败后刷新目录状态，并保留已有历史", async () => {
    await start();
    await waitFor(() => expect(Number(screen.getByTestId("task-refresh").textContent)).toBeGreaterThan(0));
    const before = Number(screen.getByTestId("task-refresh").textContent);
    const messages = useSessionStore.getState().messages;
    vi.mocked(client.agentPrepareSend).mockRejectedValueOnce(new Error("原目录已丢失"));
    fireEvent.click(screen.getByRole("button",{name:"预检重试"}));
    await waitFor(() => expect(client.agentPrepareSend).toHaveBeenCalledWith("source","/workspace"));
    await waitFor(() => expect(Number(screen.getByTestId("task-refresh").textContent)).toBeGreaterThan(before));
    expect(useSessionStore.getState().messages).toEqual(messages);
    expect(await screen.findByText("暂时无法重试：原目录已丢失")).toBeVisible();
  });
  it.each([true, false])("inherits pending=%s without cloning work items; next turn follows that exact contract", async (pending) => {
    await start(pending); const before = structuredClone(useProjectsStore.getState().projects[0]); completeFork();
    await waitFor(() => expect(useProjectsStore.getState().projects[0].conversations).toEqual(expect.arrayContaining([
      expect.objectContaining({ sessionId: "fork", title: "项目源标题", modelId: "model-a", pendingProjectContext: pending }),
    ])));
    await waitFor(() => expect(client.projectsSave).toHaveBeenCalled());
    await waitFor(() => expect(useSessionsStore.getState().currentSessionId).toBe("fork"));
    act(() => useSessionStore.getState().setMessages([{ id: "copied-user", role: "user", complete: true, parts: [{ kind: "text", text: "已有历史" }] }]));
    fireEvent.click(screen.getByRole("button", { name: "分叉下一轮" }));
    await waitFor(() => expect(client.agentSend).toHaveBeenCalled());
    const calls = vi.mocked(client.agentSend).mock.calls;
    const sent = calls[calls.length - 1];
    expect(sent[0]).toBe("fork"); expect(sent[1].includes("保留当前项目契约")).toBe(pending);
    if (pending) expect(client.agentsValidateReferences).toHaveBeenCalledWith("/workspace", []);
    expect(useSessionsStore.getState().independent.find(s => s.sessionId === "fork")?.cwd).toBe("/workspace");
    const saved = useProjectsStore.getState().projects[0];
    expect(saved.plans).toEqual(before.plans); expect(saved.tasks).toEqual(before.tasks);
    expect(saved.conversations.find((item) => item.sessionId === "source")).toEqual(before.conversations[0]);
  });

  it("failed association leaves the created fork independent with an actionable retry message", async () => {
    await start(); vi.mocked(client.projectsSave).mockRejectedValueOnce(new Error("disk full")); completeFork();
    await waitFor(() => expect(client.projectsSave).toHaveBeenCalled());
    await waitFor(() => expect(useSessionsStore.getState().currentSessionId).toBe("fork"));
    expect(useSessionsStore.getState().independent.some((item) => item.sessionId === "fork")).toBe(true);
    expect(useProjectsStore.getState().projects[0].conversations.map((item) => item.sessionId)).toEqual(["source"]);
    expect(await screen.findByText(/分叉已创建.*独立会话.*移入项目/)).toBeInTheDocument();
    expect(client.agentLoadSession).toHaveBeenCalledWith("fork", "/workspace");
  });

  it("awaits real project-save ACK and a slow association cannot steal another selected session", async () => {
    await start(); let release!: () => void;
    vi.mocked(client.projectsSave).mockImplementationOnce(() => new Promise<void>((accept) => { release = accept; }));
    completeFork(); await waitFor(() => expect(client.projectsSave).toHaveBeenCalled());
    expect(useSessionsStore.getState().currentSessionId).toBe("source");
    fireEvent.click(screen.getByRole("button", { name: "打开其他会话" }));
    await waitFor(() => expect(useSessionsStore.getState().currentSessionId).toBe("other"));
    await act(async () => { release(); });
    expect(useSessionsStore.getState().currentSessionId).toBe("other");
    expect(vi.mocked(client.agentLoadSession).mock.calls.some(([id]) => id === "fork")).toBe(false);
    expect(useProjectsStore.getState().projects[0].conversations.some((item) => item.sessionId === "fork")).toBe(true);
  });
});
