import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import type { ComponentProps, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";

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
  <ProjectsPanel cwd="/workspace" models={[{ id: "model-a", label: "Model A" }]} defaultModelId="model-a"
    onNavigate={props.onProjectNavigate} onStartProject={props.onStartProject} onStartProjectConversation={props.onStartProjectConversation} />}));
vi.mock("@/components/Composer", () => ({ Composer: () => null }));
vi.mock("@/components/TasksPanel", () => ({ TasksPanel: () => null }));
vi.mock("@/components/SecondarySidebar", () => ({ SecondarySidebar: () => null }));
vi.mock("@/components/TopbarActions", () => ({ TopbarActions: () => null }));
vi.mock("@/components/FolderTrustDialog", () => ({ FolderTrustDialog: () => null }));
vi.mock("@/components/Sidebar", () => ({Sidebar: (props: ComponentProps<typeof import("@/components/Sidebar").Sidebar>) => <aside>
  <button onClick={() => props.onStartProjectConversation?.("project-no-cwd")}>新建项目对话</button>
  <button onClick={() => props.onStartProjectConversation?.("project-b")}>新建 B 对话</button>
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
  agentLoadSession: vi.fn(async () => "model-a"), agentSend: vi.fn(async () => {}), projectsSave: vi.fn(async () => {}),
  internalReload: vi.fn(async () => {}), subscribeAgentEvents: vi.fn(async () => () => {}),
}));

import App from "@/App";
import { ProjectsPanel } from "@/components/ProjectsPanel";
import * as client from "@/lib/agent-client";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";
import { useProjectsStore } from "@/stores/projects-store";
import { useKnowledgeStore } from "@/stores/knowledge-store";
import { resetOrgSessionMirror } from "@/stores/org-session-store";

import { usePendingExpertStore } from "@/stores/pending-expert-store";
const auth = { ready:true,runtimeReady:true,synchronized:true,providers:["model-a"],runtimeModels:["model-a"],defaultModelId:"model-a" };
beforeEach(() => {
  vi.clearAllMocks(); localStorage.clear();
  resetOrgSessionMirror();
  useKnowledgeStore.setState({defaultSources:[],defaultOrganizationScopeIds:[],sessionSources:{},sessionOrganizationScopeIds:{}});
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


it("创建对话期间通过项目面包屑返回列表，迟到结果只注册原项目", async () => {
  await start();
  fireEvent.click(screen.getByRole("button", { name: "查看项目" }));
  fireEvent.click(document.querySelector(".project-card2__main")!);
  await screen.findByRole("button", { name: "项目" });
  let finish!: (id: string) => void;
  vi.mocked(client.agentNewSession).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledOnce());
  fireEvent.click(screen.getByRole("button", { name: "项目" }));
  expect(document.querySelector(".project-card2__main")).not.toBeNull();
  await act(async () => { finish("breadcrumb-late-task"); });
  expect(useSessionsStore.getState().currentSessionId).toBeNull();
  expect(document.querySelector(".project-card2__main")).not.toBeNull();
  expect(useProjectsStore.getState().projects[0].conversations.some(item => item.sessionId === "breadcrumb-late-task")).toBe(true);
});

it("等待原项目创建时进入另一项目，仍保留当前项目页面", async () => {
  await start();
  const original = useProjectsStore.getState().projects[0];
  act(() => useProjectsStore.setState({ projects: [original, { ...original, id: "project-b", name: "项目 B", conversations: [] }] }));
  fireEvent.click(screen.getByRole("button", { name: "查看项目" }));
  let finish!: (id: string) => void;
  vi.mocked(client.agentNewSession).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledOnce());
  fireEvent.click(document.querySelectorAll(".project-card2__main")[1]);
  await act(async () => { finish("project-a-background"); });
  expect(useSessionsStore.getState().currentSessionId).toBeNull();
  expect(document.querySelector(".pd-crumb__name")).toHaveTextContent("项目 B");
  expect(useProjectsStore.getState().projects[0].conversations.some(item => item.sessionId === "project-a-background")).toBe(true);
});

it("两个项目连续创建时，仅最后发起的任务可以打开前台", async () => {
  await start();
  const original = useProjectsStore.getState().projects[0];
  act(() => useProjectsStore.setState({ projects: [original, { ...original, id: "project-b", name: "项目 B", conversations: [] }] }));
  let finishA!: (id: string) => void;
  let finishB!: (id: string) => void;
  vi.mocked(client.agentNewSession)
    .mockImplementationOnce(() => new Promise(resolve => { finishA = resolve; }))
    .mockImplementationOnce(() => new Promise(resolve => { finishB = resolve; }));
  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledTimes(1));
  fireEvent.click(screen.getByRole("button", { name: "新建 B 对话" }));
  await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledTimes(2));
  await act(async () => { finishB("project-b-foreground"); });
  expect(useSessionsStore.getState().currentSessionId).toBe("project-b-foreground");
  await act(async () => { finishA("project-a-late"); });
  expect(useSessionsStore.getState().currentSessionId).toBe("project-b-foreground");
  expect(useSessionStore.getState().sessionId).toBe("project-b-foreground");
  expect(useProjectsStore.getState().projects.map(project => project.conversations.length)).toEqual([1, 1]);
});
