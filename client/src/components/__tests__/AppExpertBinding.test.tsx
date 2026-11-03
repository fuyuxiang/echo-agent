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
  <span data-testid="new-task-cwd">{props.cwd}</span>
  <button onClick={() => props.onSelectWorkspace?.("/workspace/B")}>新任务选择 B</button>
  <button onClick={() => props.onSelectWorkspace?.("/workspace/C")}>新任务选择 C</button>
  <button onClick={() => props.onSelectWorkspace?.("")}>恢复默认目录</button>
  <button onClick={() => props.onSelectExpert?.({name:"Review",description:"review",scope:"user",path:"/fixture/agents/review.md",raw:"---\nname: Review\n---\nVerify the task"})}>输入菜单选择专家</button>
  <button onClick={() => void props.onSend("my unsent task")}>发送专家任务</button>
  {props.sendError && <div role="alert">{props.sendError}</div>}
</div> }));
vi.mock("../PlaceholderPage", () => ({ PlaceholderPage: () => null }));
vi.mock("../TasksPanel", () => ({ TasksPanel: () => null }));
vi.mock("../SecondarySidebar", () => ({ SecondarySidebar: () => null }));
vi.mock("../TopbarActions", () => ({ TopbarActions: () => null }));
vi.mock("../FolderTrustDialog", () => ({ FolderTrustDialog: () => null }));
vi.mock("../Sidebar", () => ({ Sidebar: (props: ComponentProps<typeof import("../Sidebar").Sidebar>) => <aside><button onClick={() => void props.onSelect("history-A", "/workspace/A")}>打开历史 A</button><button onClick={() => props.onNewSession()}>返回首页新建</button></aside> }));
vi.mock("../ChatView", () => ({ ChatView: (props: ComponentProps<typeof import("../ChatView").ChatView>) => <div>已进入会话<span data-testid="session-cwd">{props.cwd}</span><button onClick={props.onNewTask}>从任务新建</button></div> }));
vi.mock("@/lib/agent-client", async (original) => ({
  ...await original<object>(), agentInit: vi.fn(), agentAuthStatus: vi.fn(), providersList: vi.fn(),
  agentListAllSessions: vi.fn(), agentListWorkspaces: vi.fn(async () => []),
  agentPrepareTaskWorkspace: vi.fn(async () => "/workspace/Tasks/default-task"),
  agentNewSession: vi.fn(async () => "new-session"), agentDeleteSession: vi.fn(async () => ({memorySummariesDeleted:0})), agentSetSessionExpert: vi.fn(async () => true),
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
  useSessionsStore.setState(useSessionsStore.getInitialState()); useSessionStore.setState(useSessionStore.getInitialState());
  useProjectsStore.setState({ ...useProjectsStore.getInitialState(), projects: [] }); usePendingExpertStore.getState().clear();
  useKnowledgeStore.setState({ defaultSources: [], defaultOrganizationScopeIds: [], sessionSources: {}, sessionOrganizationScopeIds: {} });
  resetOrgSessionMirror();
  vi.mocked(client.agentInit).mockResolvedValue({ ok:true,cwd:"/workspace",auth } as never);
  vi.mocked(client.agentAuthStatus).mockResolvedValue(auth);
  vi.mocked(client.providersList).mockResolvedValue({providers:[],models:[{modelId:"model-a",name:"Model A",providerId:"p"}]} as never);
  vi.mocked(client.agentListAllSessions).mockResolvedValue([]);
  vi.mocked(client.agentSetSessionExpert).mockReset().mockResolvedValue(true);
  vi.mocked(client.agentPrepareTaskWorkspace).mockReset().mockImplementation(async id => `/workspace/Tasks/${id}`);
  vi.mocked(invoke).mockImplementation(async command => command === "notification_take_pending_opens" ? [] : null);
});
async function start() { render(<App />); await screen.findByText("首页"); await waitFor(()=>expect(client.providersList).toHaveBeenCalled()); await act(async()=>{}); }

describe("expert task persistent binding admission", () => {
  it("组织恢复请求持续等待时，个人模型任务仍可初始化并发送", async () => {
    vi.mocked(invoke).mockImplementation(async command => {
      if (command === "org_session") return new Promise(() => {});
      return command === "notification_take_pending_opens" ? [] : null;
    });
    await start();
    fireEvent.click(screen.getByRole("button", { name: "发送专家任务" }));
    await waitFor(() => expect(client.agentSend).toHaveBeenCalledWith(
      "new-session", "my unsent task", [], "my unsent task", expect.any(String),
    ));
    expect(client.internalReload).not.toHaveBeenCalled();
  });
  it("创建期间保留发送时的知识来源，后续首页选择不会被消费", async () => {
    let complete!: (id: string) => void;
    vi.mocked(client.agentNewSession).mockImplementationOnce(() => new Promise(resolve => { complete = resolve; }));
    await start();
    act(() => useKnowledgeStore.getState().setDefaultSources(["personal"]));
    fireEvent.click(screen.getByRole("button", { name: "发送专家任务" }));
    await waitFor(() => expect(client.agentNewSession).toHaveBeenCalled());
    act(() => useKnowledgeStore.getState().setDefaultSources(["organization"]));
    await act(async () => { complete("snapshot-task"); });
    expect(useKnowledgeStore.getState().sessionSources["snapshot-task"]).toEqual(["personal"]);
    expect(useKnowledgeStore.getState().defaultSources).toEqual(["organization"]);
  });
  it("组织来源任务创建期间发生身份变化，保留输入且不在新身份下发送", async () => {
    let complete!: (id: string) => void;
    vi.mocked(client.agentNewSession).mockImplementationOnce(() => new Promise(resolve => { complete = resolve; }));
    await start();
    act(() => useKnowledgeStore.getState().setDefaultSources(["organization"]));
    fireEvent.click(screen.getByRole("button", { name: "发送专家任务" }));
    await waitFor(() => expect(client.agentNewSession).toHaveBeenCalled());
    act(() => useOrgSessionStore.getState().clearSession());
    await act(async () => { complete("obsolete-org-task"); });
    expect(client.agentSend).not.toHaveBeenCalled();
    expect(client.agentDeleteSession).toHaveBeenCalledWith("obsolete-org-task", expect.any(String));
    expect(useSessionsStore.getState().currentSessionId).toBeNull();
    expect(screen.getAllByRole("alert").some(alert => alert.textContent?.includes("组织账号或权限已改变"))).toBe(true);
  });
  it("创建期间打开历史会话，迟到的新任务在后台发送且不抢走页面", async () => {
    let complete!: (id: string) => void;
    vi.mocked(client.agentNewSession).mockImplementationOnce(() => new Promise(resolve => { complete = resolve; }));
    vi.mocked(client.agentListAllSessions).mockResolvedValue([{ sessionId: "history-A", title: "History", cwd: "/workspace/A", currentModelId: "model-a" }]);
    await start();
    fireEvent.click(screen.getByRole("button", { name: "发送专家任务" }));
    await waitFor(() => expect(client.agentNewSession).toHaveBeenCalled());
    fireEvent.click(screen.getByRole("button", { name: "打开历史 A" }));
    await waitFor(() => expect(useSessionsStore.getState().currentSessionId).toBe("history-A"));
    await act(async () => { complete("late-new-session"); });
    await waitFor(() => expect(client.agentSend).toHaveBeenCalled());
    expect(vi.mocked(client.agentSend).mock.calls.slice(-1)[0]?.[0]).toBe("late-new-session");
    expect(useSessionsStore.getState().currentSessionId).toBe("history-A");
    expect(useSessionStore.getState().sessionId).toBe("history-A");
    expect(useSessionStore.getState().transcripts["late-new-session"].messages.some(message => message.parts.some(part => part.kind === "text" && part.text === "my unsent task"))).toBe(true);
  });
  it("waits for the native binding ACK before showing the task or sending its persona", async () => {
    let resolve!: (value:boolean)=>void;
    vi.mocked(client.agentSetSessionExpert).mockImplementationOnce(()=>new Promise<boolean>(accept=>{resolve=accept;}));
    await start(); fireEvent.click(screen.getByRole("button",{name:"输入菜单选择专家"})); fireEvent.click(screen.getByRole("button",{name:"发送专家任务"}));
    await waitFor(()=>expect(client.agentSetSessionExpert).toHaveBeenCalledWith("new-session","Review","Review","local",undefined,"/fixture/agents/review.md","---\nname: Review\n---\nVerify the task"));
    expect(client.agentSend).not.toHaveBeenCalled(); expect(useSessionsStore.getState().currentSessionId).not.toBe("new-session");
    expect(usePendingExpertStore.getState().expert?.name).toBe("Review");
    await act(async()=>{resolve(true);});
    await waitFor(()=>expect(client.agentSend).toHaveBeenCalled());
    expect(vi.mocked(client.agentSend).mock.calls[0][1]).toContain("Verify the task");
    expect(usePendingExpertStore.getState().expert).toBeNull();
    expect(useSessionsStore.getState().independent.find(session=>session.sessionId==="new-session")?.expertName).toBe("Review");
  });
  it("a binding error preserves pending selection and never sends the user prompt", async () => {
    vi.mocked(client.agentSetSessionExpert).mockRejectedValueOnce(new Error("专家配置已更新，请重新选择"));
    await start(); fireEvent.click(screen.getByRole("button",{name:"输入菜单选择专家"})); fireEvent.click(screen.getByRole("button",{name:"发送专家任务"}));
    await waitFor(()=>expect(client.agentDeleteSession).toHaveBeenCalledWith("new-session",expect.stringMatching(/^\/workspace\/Tasks\//)));
    expect(client.agentSend).not.toHaveBeenCalled(); expect(usePendingExpertStore.getState().expert?.name).toBe("Review");
    expect(screen.getAllByRole("alert").some(alert => alert.textContent?.includes("专家配置已更新"))).toBe(true);
  });
});

it("默认任务使用不同的独立目录，手动选择保留用户指定的位置", async () => {
  vi.mocked(client.agentNewSession).mockResolvedValueOnce("task-1").mockResolvedValueOnce("task-2").mockResolvedValueOnce("task-3");
  await start();
  fireEvent.click(screen.getByRole("button", {name:"发送专家任务"}));
  await screen.findByText("已进入会话");
  const firstCwd = useSessionsStore.getState().independent.find(s => s.sessionId === "task-1")?.cwd;
  expect(firstCwd).toMatch(/^\/workspace\/Tasks\//);
  fireEvent.click(screen.getByRole("button", {name:"从任务新建"}));
  fireEvent.click(screen.getByRole("button", {name:"发送专家任务"}));
  await screen.findByText("已进入会话");
  const secondCwd = useSessionsStore.getState().independent.find(s => s.sessionId === "task-2")?.cwd;
  expect(secondCwd).toMatch(/^\/workspace\/Tasks\//);
  expect(secondCwd).not.toBe(firstCwd);
  fireEvent.click(screen.getByRole("button", {name:"从任务新建"}));
  fireEvent.click(screen.getByRole("button", {name:"新任务选择 C"}));
  fireEvent.click(screen.getByRole("button", {name:"发送专家任务"}));
  await screen.findByText("已进入会话");
  expect(client.agentPrepareTaskWorkspace).toHaveBeenCalledTimes(2);
  expect(useSessionsStore.getState().independent.find(s => s.sessionId === "task-3")?.cwd).toBe("/workspace/C");
  expect(useSessionsStore.getState().independent.find(s => s.sessionId === "task-1")?.cwd).toBe(firstCwd);
});

it("目录准备失败时保留首页，重试使用同一请求避免生成多个默认目录", async () => {
  vi.mocked(client.agentPrepareTaskWorkspace).mockRejectedValueOnce(new Error("目录暂不可用"));
  await start();
  fireEvent.click(screen.getByRole("button", {name:"发送专家任务"}));
  await waitFor(() => expect(screen.getAllByRole("alert").some(a => a.textContent?.includes("目录暂不可用"))).toBe(true));
  expect(client.agentNewSession).not.toHaveBeenCalled();
  const firstId = vi.mocked(client.agentPrepareTaskWorkspace).mock.calls[0][0];
  fireEvent.click(screen.getByRole("button", {name:"发送专家任务"}));
  await screen.findByText("已进入会话");
  expect(vi.mocked(client.agentPrepareTaskWorkspace).mock.calls[1][0]).toBe(firstId);
});


it("历史 A 与下一任务 B/C 分离，最终新任务真正绑定 C", async () => {
  vi.mocked(client.agentListAllSessions).mockResolvedValue([{sessionId:"history-A",title:"History",cwd:"/workspace/A",currentModelId:"model-a"}]);
  await start();
  fireEvent.click(screen.getByRole("button", {name:"新任务选择 B"}));
  expect(screen.getByTestId("new-task-cwd")).toHaveTextContent("/workspace/B");
  fireEvent.click(screen.getByRole("button", {name:"打开历史 A"}));
  expect(await screen.findByTestId("session-cwd")).toHaveTextContent("/workspace/A");
  await waitFor(() => expect(client.agentLoadSession).toHaveBeenLastCalledWith("history-A", "/workspace/A"));
  fireEvent.click(screen.getByRole("button", {name:"从任务新建"}));
  expect(await screen.findByTestId("new-task-cwd")).toHaveTextContent("/workspace/B");
  fireEvent.click(screen.getByRole("button", {name:"新任务选择 C"}));
  fireEvent.click(screen.getByRole("button", {name:"打开历史 A"}));
  expect(await screen.findByTestId("session-cwd")).toHaveTextContent("/workspace/A");
  fireEvent.click(screen.getByRole("button", {name:"从任务新建"}));
  expect(await screen.findByTestId("new-task-cwd")).toHaveTextContent("/workspace/C");
  fireEvent.click(screen.getByRole("button", {name:"发送专家任务"}));
  await waitFor(() => expect(client.agentNewSession).toHaveBeenLastCalledWith("/workspace/C", "model-a", "ask"));
  expect(useSessionsStore.getState().independent.find(item => item.sessionId === "new-session")?.cwd).toBe("/workspace/C");
});
