import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ComponentProps, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";

const coding = vi.hoisted(() => ({ bind: vi.fn(async () => {}) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/lib/kb-source-storage", () => ({ hydrateKnowledgeSources: vi.fn(async () => {}) }));
vi.mock("@/lib/artifact-catalog", () => ({ indexTaskArtifacts: vi.fn() }));
vi.mock("@/stores/projects-store", async (original) => ({
  ...await original<object>(), hydrateProjectsFromBackend: vi.fn(async () => {}),
}));
vi.mock("@/components/ThemeProvider", () => ({ ThemeProvider: ({ children }: { children: ReactNode }) => children }));
vi.mock("@/components/TitleBar", () => ({ TitleBar: () => null }));
vi.mock("@/components/HomePage", () => ({HomePage: () => <div>首页</div>}));
vi.mock("@/components/PlaceholderPage", () => ({PlaceholderPage: (props: ComponentProps<typeof import("@/components/PlaceholderPage").PlaceholderPage>) =>
  <><button onClick={() => {const project=useProjectsStore.getState().projects[0]; if(project) void Promise.resolve(props.onStartProject?.(project, "ask")).catch(error => props.onToast?.(`启动项目失败：${String(error).replace(/^Error:\s*/, "")}`));}}>启动项目</button>
  <button onClick={() => void props.onStartProjectConversation?.("project-no-cwd", "项目任务", "model-a", {
    cwd: useProjectsStore.getState().projects[0]?.cwd ?? null,
    expectedPermissionMode: "ask",
  }).catch(error => props.onToast?.(`发送失败：${String(error).replace(/^Error:\s*/, "")}`))}>发送项目草稿</button>
  <button onClick={() => void props.onStartCodingRun?.("/work/code", "代码任务", "model-a", [], coding.bind)}>启动 Code 任务</button>
  <button onClick={props.onExitCodingWorkspace}>退出 Code</button>
  <output data-testid="coding-active">{String(props.codingActive)}</output></>}));
vi.mock("@/components/TasksPanel", () => ({ TasksPanel: () => null }));
vi.mock("@/components/SecondarySidebar", () => ({ SecondarySidebar: () => null }));
vi.mock("@/components/TopbarActions", () => ({ TopbarActions: () => null }));
vi.mock("@/components/FolderTrustDialog", () => ({ FolderTrustDialog: () => null }));
vi.mock("@/components/Sidebar", () => ({Sidebar: (props: ComponentProps<typeof import("@/components/Sidebar").Sidebar>) => <aside>
  <button onClick={() => props.onNavigate?.("项目")}>查看项目</button>
  <button onClick={() => props.onNewSession()}>返回首页</button>
  <button onClick={() => props.onNavigate?.("代码开发")}>打开 Code</button>
</aside>}));
vi.mock("@/components/ChatView", () => ({ ChatView: (props: ComponentProps<typeof import("@/components/ChatView").ChatView>) => <div>已进入会话<span data-testid="session-cwd">{props.cwd}</span><button onClick={props.onNewTask}>从任务新建</button></div> }));
vi.mock("@/lib/agent-client", async (original) => ({
  ...await original<object>(), agentInit: vi.fn(), agentAuthStatus: vi.fn(), providersList: vi.fn(),
  agentListAllSessions: vi.fn(), agentListWorkspaces: vi.fn(async () => []),
  agentsValidateReferences: vi.fn(async () => {}),
  agentPrepareTaskWorkspace: vi.fn(async () => "/workspace/Tasks/default-task"),
  agentNewSession: vi.fn(async () => "new-session"),
  setCodingMode: vi.fn(async () => {}),
  agentLoadSession: vi.fn(async () => "model-a"), agentSend: vi.fn(async () => {}), projectsSave: vi.fn(async () => {}),
  internalReload: vi.fn(async () => {}), subscribeAgentEvents: vi.fn(async () => () => {}),
}));

import App from "@/App";
import * as client from "@/lib/agent-client";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";
import { useProjectsStore } from "@/stores/projects-store";
import { useKnowledgeStore } from "@/stores/knowledge-store";
import { resetOrgSessionMirror, useOrgSessionStore } from "@/stores/org-session-store";

import { usePendingExpertStore } from "@/stores/pending-expert-store";
const auth = { ready:true,runtimeReady:true,synchronized:true,providers:["model-a"],runtimeModels:["model-a"],defaultModelId:"model-a" };
beforeEach(() => {
  vi.clearAllMocks(); localStorage.clear();
  coding.bind.mockReset().mockResolvedValue(undefined);
  vi.mocked(client.setCodingMode).mockReset().mockResolvedValue(undefined);
  resetOrgSessionMirror();
  useKnowledgeStore.setState({ defaultSources: [], defaultOrganizationScopeIds: [], sessionSources: {}, sessionOrganizationScopeIds: {} });
  useSessionsStore.setState(useSessionsStore.getInitialState()); useSessionStore.setState(useSessionStore.getInitialState());
  useProjectsStore.setState({ ...useProjectsStore.getInitialState(), projects: [] }); usePendingExpertStore.getState().clear();
  vi.mocked(client.agentInit).mockResolvedValue({ ok:true,cwd:"/workspace",auth } as never);
  vi.mocked(client.agentAuthStatus).mockResolvedValue(auth);
  vi.mocked(client.providersList).mockResolvedValue({providers:[],models:[{modelId:"model-a",name:"Model A",providerId:"p"}]} as never);
  vi.mocked(client.agentListAllSessions).mockResolvedValue([]);
  vi.mocked(client.agentNewSession).mockReset().mockResolvedValue("new-session");
  vi.mocked(client.agentPrepareTaskWorkspace).mockReset().mockImplementation(async id => `/workspace/Tasks/${id}`);
  vi.mocked(invoke).mockImplementation(async command => ["notification_take_pending_opens", "coding_task_list"].includes(String(command)) ? [] : null);
});
async function start() {
  useProjectsStore.setState({projects:[{id:"project-no-cwd",name:"Project",createdAt:"2026-10-26T00:00:00Z",connectors:[],experts:[],skills:[],assets:[],members:[],plans:[],tasks:[],conversations:[]}]});
  render(<App />); await screen.findByText("首页"); await waitFor(()=>expect(client.providersList).toHaveBeenCalled()); await act(async()=>{}); }


async function create(entry: "发送项目草稿" | "启动项目") {
  if (!screen.queryByRole("button",{name:entry})) {
    fireEvent.click(screen.getByRole("button",{name:"查看项目"}));
    await screen.findByRole("button",{name:entry});
  }
  fireEvent.click(screen.getByRole("button",{name:entry}));
}

it.each(["binding", "mode"])("Code %s 等待期间切换组织身份，停止首条发送并标记启动失败", async stage => {
  await start();
  let finish!: () => void;
  const pending = new Promise<void>(resolve => { finish = resolve; });
  if (stage === "binding") coding.bind.mockReturnValueOnce(pending);
  else vi.mocked(client.setCodingMode).mockReturnValueOnce(pending);
  act(() => {
    useOrgSessionStore.getState().setSession({ loggedIn: true, serverUrl: "https://memory.example.com", user: { id: "alice", username: "alice", displayName: "Alice", role: "member", clearance: 1 } });
    useKnowledgeStore.getState().setDefaultSources(["organization"]);
  });
  fireEvent.click(screen.getByRole("button", { name: "打开 Code" }));
  fireEvent.click(await screen.findByRole("button", { name: "启动 Code 任务" }));
  await waitFor(() => expect(stage === "binding" ? coding.bind : client.setCodingMode).toHaveBeenCalled());
  act(() => useOrgSessionStore.getState().setSession({ loggedIn: true, serverUrl: "https://memory.example.com", user: { id: "bob", username: "bob", displayName: "Bob", role: "member", clearance: 1 } }));
  await act(async () => { finish(); });
  await waitFor(() => expect(useSessionsStore.getState().independent.find(item => item.sessionId === "new-session")?.status).toBe("failed"));
  expect(client.agentSend).not.toHaveBeenCalled();
  expect(useSessionStore.getState().streaming).toBe(false);
  expect(await screen.findByText(/启动代码开发失败：组织账号或权限已改变/)).toBeInTheDocument();
});

it("个人 Code 任务不因准备期间组织身份变化而中断", async () => {
  await start();
  let finish!: () => void;
  coding.bind.mockReturnValueOnce(new Promise(resolve => { finish = resolve; }));
  act(() => useKnowledgeStore.getState().setDefaultSources(["personal"]));
  fireEvent.click(screen.getByRole("button", { name: "打开 Code" }));
  fireEvent.click(await screen.findByRole("button", { name: "启动 Code 任务" }));
  await waitFor(() => expect(coding.bind).toHaveBeenCalled());
  act(() => useOrgSessionStore.getState().clearSession());
  await act(async () => { finish(); });
  await waitFor(() => expect(client.agentSend).toHaveBeenCalledOnce());
  expect(useKnowledgeStore.getState().sessionSources["new-session"]).toEqual(["personal"]);
});

it("从 Code 的退出入口离开后，保留的工作台变为不活跃且迟到创建不抢回页面", async () => {
  await start();
  let finish!: (id: string) => void;
  vi.mocked(client.agentNewSession).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  fireEvent.click(screen.getByRole("button", { name: "打开 Code" }));
  expect(await screen.findByTestId("coding-active")).toHaveTextContent("true");
  fireEvent.click(screen.getByRole("button", { name: "启动 Code 任务" }));
  await waitFor(() => expect(client.agentNewSession).toHaveBeenCalled());
  fireEvent.click(screen.getByRole("button", { name: "退出 Code" }));
  expect(screen.getByTestId("coding-active")).toHaveTextContent("false");
  await act(async () => { finish("exited-code-task"); });
  await waitFor(() => expect(client.agentSend).toHaveBeenCalledOnce());
  expect(useSessionsStore.getState().currentSessionId).toBeNull();
  expect(screen.getByText("首页")).toBeInTheDocument();
});

it("Code 会话创建期间返回首页，原任务仍发送到原代码目录且不抢走页面", async () => {
  await start();
  let finish!: (id: string) => void;
  vi.mocked(client.agentNewSession).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  fireEvent.click(screen.getByRole("button", { name: "打开 Code" }));
  fireEvent.click(await screen.findByRole("button", { name: "启动 Code 任务" }));
  await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledWith("/work/code", "model-a", expect.any(String)));
  fireEvent.click(screen.getByRole("button", { name: "返回首页" }));
  await act(async () => { finish("background-code-task"); });
  await waitFor(() => expect(client.agentSend).toHaveBeenCalled());
  expect(useSessionsStore.getState().currentSessionId).toBeNull();
  expect(useSessionStore.getState().sessionId).toBeNull();
  expect(useSessionsStore.getState().independent.find(entry => entry.sessionId === "background-code-task")?.cwd).toBe("/work/code");
  expect(vi.mocked(client.agentSend).mock.calls[0][0]).toBe("background-code-task");
});
describe.each(["发送项目草稿","启动项目"] as const)("%s creation recovery", entry => {
  it("创建期间离开项目，迟到结果注册原项目但不重新选择会话", async () => {
    await start();
    let finish!: (id: string) => void;
    vi.mocked(client.agentNewSession).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    await create(entry);
    await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("button", { name: "返回首页" }));
    await act(async () => { finish("background-project-task"); });
    expect(useSessionsStore.getState().currentSessionId).toBeNull();
    expect(useSessionStore.getState().sessionId).toBeNull();
    expect(useProjectsStore.getState().projects[0].conversations.some(item => item.sessionId === "background-project-task")).toBe(true);
    expect(vi.mocked(client.agentSend).mock.calls[0][0]).toBe("background-project-task");
    expect(useSessionStore.getState().transcripts["background-project-task"].messages.some(message => message.role === "user")).toBe(true);
  });
  it("超时重试沿用原目录，成功后的新任务才分配另一目录", async () => {
    await start();
    vi.mocked(client.agentNewSession).mockRejectedValueOnce(new Error("创建超时，请重试"))
      .mockResolvedValueOnce("reclaimed").mockResolvedValueOnce("next-task");
    await create(entry);
    await screen.findByText(/失败：创建超时/);
    const originalCwd = vi.mocked(client.agentNewSession).mock.calls[0][0];
    await create(entry);
    await waitFor(() => expect(useSessionsStore.getState().currentSessionId).toBe("reclaimed"));
    expect(vi.mocked(client.agentNewSession).mock.calls[1][0]).toBe(originalCwd);
    expect(useProjectsStore.getState().projects[0].conversations.filter(c => c.sessionId === "reclaimed")).toHaveLength(1);
    await create(entry);
    await waitFor(() => expect(useSessionsStore.getState().currentSessionId).toBe("next-task"));
    expect(vi.mocked(client.agentNewSession).mock.calls[2][0]).not.toBe(originalCwd);
  });
  it("创建尚未完成时连续点击不会重复创建或发送首条消息", async () => {
    await start();
    let finish!: (id: string) => void;
    vi.mocked(client.agentNewSession).mockImplementationOnce(() => new Promise(resolve => {finish=resolve;}));
    await create(entry);
    await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledOnce());
    await create(entry);
    expect(client.agentNewSession).toHaveBeenCalledOnce();
    await act(async () => finish("created-once"));
    await waitFor(() => expect(useSessionsStore.getState().currentSessionId).toBe("created-once"));
    expect(useProjectsStore.getState().projects[0].conversations).toHaveLength(1);
    expect(client.agentSend).toHaveBeenCalledOnce();
  });
  it("手动指定的项目目录保持固定且不分配默认目录", async () => {
    await start();
    act(() => useProjectsStore.setState({projects:useProjectsStore.getState().projects.map(project => ({...project,cwd:"/manual-repository"}))}));
    await create(entry);
    await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledWith("/manual-repository", "model-a", undefined, "ask"));
    expect(client.agentPrepareTaskWorkspace).not.toHaveBeenCalled();
  });
});
