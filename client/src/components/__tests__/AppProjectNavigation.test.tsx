import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
    onNavigate={props.onProjectNavigate} onStartProject={props.onStartProject} onStartProjectConversation={props.onStartProjectConversation}
    onOpenSession={props.onOpenSession} onOpenProjectWorkItemSession={props.onOpenProjectWorkItemSession}
    onToast={props.onToast} />}));
vi.mock("@/components/TasksPanel", () => ({ TasksPanel: () => null }));
vi.mock("@/components/SecondarySidebar", () => ({ SecondarySidebar: () => null }));
vi.mock("@/components/TopbarActions", () => ({ TopbarActions: () => null }));
vi.mock("@/components/FolderTrustDialog", () => ({ FolderTrustDialog: () => null }));
vi.mock("@/components/Sidebar", () => ({Sidebar: (props: ComponentProps<typeof import("@/components/Sidebar").Sidebar>) => <aside>
  <button onClick={() => props.onOpenProjectDraft?.("project-no-cwd")}>新建项目对话</button>
  <button onClick={() => props.onOpenProjectDraft?.("project-b")}>新建 B 对话</button>
  <button onClick={() => props.onOpenProject?.("project-b")}>打开 B 项目</button>
  <button onClick={() => props.onSelect("work-session", "/workspace")}>从侧栏打开项目会话</button>
  <button onClick={() => props.onNavigate?.("项目")}>查看项目</button>
  <button onClick={() => props.onNewSession()}>返回首页</button>
  <button onClick={() => props.onNavigate?.("代码开发")}>打开 Code</button>
</aside>}));
vi.mock("@/components/ChatView", async () => {
  const { SessionHeader } = await import("@/components/SessionHeader");
  return { ChatView: (props: ComponentProps<typeof import("@/components/ChatView").ChatView>) => <>
    <SessionHeader sessionId={useSessionsStore.getState().currentSessionId!} title={props.title ?? ""}
      projectBreadcrumb={props.projectBreadcrumb} onRename={props.onRename}
      summary={{ searchable: false, hasContent: false, changeCount: 0, artifactCount: 0, hasPlan: false,
        planCompleted: 0, planTotal: 0, subagentCount: 0, runningSubagents: 0, teamCount: 0 }}
      activeView={null} onView={() => {}} onCloseView={() => {}} onFind={() => {}} findOpen={false} />
    <div>已进入会话<span data-testid="session-cwd">{props.cwd}</span><button onClick={props.onNewTask}>从任务新建</button></div>
  </> };
});
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
import { usePermissionModeStore } from "@/stores/permission-mode-store";
import { resetOrgSessionMirror } from "@/stores/org-session-store";

import { usePendingExpertStore } from "@/stores/pending-expert-store";
const auth = { ready:true,runtimeReady:true,synchronized:true,providers:["model-a"],runtimeModels:["model-a"],defaultModelId:"model-a" };
beforeEach(() => {
  vi.clearAllMocks(); localStorage.clear();
  resetOrgSessionMirror();
  useKnowledgeStore.setState({defaultSources:[],defaultOrganizationScopeIds:[],sessionSources:{},sessionOrganizationScopeIds:{}});
  useSessionsStore.setState(useSessionsStore.getInitialState()); useSessionStore.setState(useSessionStore.getInitialState());
  usePermissionModeStore.setState(usePermissionModeStore.getInitialState());
  useProjectsStore.setState({ ...useProjectsStore.getInitialState(), projects: [] }); usePendingExpertStore.getState().clear();
  vi.mocked(client.agentInit).mockResolvedValue({ ok:true,cwd:"/workspace",auth } as never);
  vi.mocked(client.agentAuthStatus).mockResolvedValue(auth);
  vi.mocked(client.providersList).mockResolvedValue({providers:[],models:[{modelId:"model-a",name:"Model A",providerId:"p"}]} as never);
  vi.mocked(client.agentListAllSessions).mockResolvedValue([]);
  vi.mocked(client.agentNewSession).mockReset().mockResolvedValue("new-session");
  vi.mocked(client.agentPrepareTaskWorkspace).mockReset().mockImplementation(async id => `/workspace/Tasks/${id}`);
  vi.mocked(invoke).mockImplementation(async command => command === "permission_mode_get"
    ? { permissionMode: "ask", configuredPermissionMode: "ask", locked: false }
    : ["notification_take_pending_opens", "coding_task_list"].includes(String(command)) ? [] : null);
});
async function start() {
  useProjectsStore.setState({projects:[{id:"project-no-cwd",name:"Project",createdAt:"2026-10-26T00:00:00Z",connectors:[],experts:[],skills:[],assets:[],members:[],plans:[],tasks:[],conversations:[]}]});
  render(<App />); await screen.findByText("首页"); await waitFor(()=>expect(client.providersList).toHaveBeenCalled()); await act(async()=>{}); }

async function sendProjectDraft() {
  const input = await screen.findByRole("textbox", { name: "输入问题或任务" });
  fireEvent.change(input, { target: { value: "执行项目任务" } });
  await waitFor(() => expect(screen.getByRole("button", { name: "发送" })).toBeEnabled());
  fireEvent.click(screen.getByRole("button", { name: "发送" }));
}

it("侧栏加号聚焦并复用项目草稿，只在发送时绑定任务目录", async () => {
  await start();
  const project = { ...useProjectsStore.getState().projects[0], cwd: "/project" };
  act(() => {
    useProjectsStore.setState({ projects: [project] });
    useSessionsStore.getState().setDraft("project:project-no-cwd", "未发送的草稿");
    useSessionsStore.getState().setDraft("project-workspace:project-no-cwd", "/task");
    usePermissionModeStore.getState().setHomeMode("always-approve");
  });
  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  const input = await screen.findByRole("textbox", { name: "输入问题或任务" });
  expect(input).toHaveFocus();
  expect(input).toHaveValue("未发送的草稿");
  expect(client.agentNewSession).not.toHaveBeenCalled();
  fireEvent.click(await screen.findByRole("button", { name: "请求批准" }));
  fireEvent.click(screen.getByRole("menuitemradio", { name: /^自动判断/ }));
  expect(screen.getByRole("button", { name: "自动判断" })).toBeVisible();
  await waitFor(() => expect(screen.getByRole("button", { name: "发送" })).toBeEnabled());
  fireEvent.click(screen.getByRole("button", { name: "发送" }));
  await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledWith("/task", "model-a", "auto", "auto"));
  await waitFor(() => expect(screen.getByTestId("session-cwd")).toHaveTextContent("/task"));
  expect(useSessionsStore.getState().independent.find(item => item.sessionId === "new-session")?.permissionMode).toBe("auto");
  expect(usePermissionModeStore.getState().homeMode).toBe("always-approve");
  expect(useProjectsStore.getState().projects[0].cwd).toBe("/project");
});

it("项目资料准备期间切换项目，晚完成的原任务留在后台", async () => {
  await start();
  const project = useProjectsStore.getState().projects[0];
  const assetPath = "/private-project/A.md";
  act(() => {
    useProjectsStore.setState({ projects: [
      { ...project, assets: [{ id: "a", kind: "file", name: "A.md", path: assetPath }] },
      { ...project, id: "project-b", name: "项目 B" },
    ] });
    useSessionsStore.getState().setDraft(`project-assets:${project.id}`, '["a"]');
  });
  let releaseAsset!: (paths: string[]) => void;
  const prepareAsset = new Promise<string[]>(resolve => { releaseAsset = resolve; });
  const previousInvoke = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "project_assets_use") return prepareAsset;
    if (command === "filesystem_attachment_stats") {
      return { files: [{ inputPath: assetPath, path: assetPath, sizeBytes: 12 }], rejected: [] };
    }
    return previousInvoke(command, args);
  });
  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  await sendProjectDraft();
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("project_assets_use", expect.any(Object)));
  expect(client.agentNewSession).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "打开 B 项目" }));
  await act(async () => releaseAsset([assetPath]));
  await waitFor(() => expect(client.agentSend).toHaveBeenCalledOnce());
  expect(useSessionsStore.getState().currentSessionId).toBeNull();
  expect(document.querySelector(".pd-crumb__name")).toHaveTextContent("项目 B");
});

it("分配默认任务目录失败后显示真实原因和选取现有目录的入口", async () => {
  await start();
  vi.mocked(client.agentPrepareTaskWorkspace).mockRejectedValueOnce("无法分配默认任务目录：磁盘空间不足");
  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  await sendProjectDraft();
  const settings = screen.getByRole("group", { name: "任务执行设置" });
  const error = await within(settings).findByRole("alert");
  expect(error).toHaveTextContent("请选择一个现有文件夹后重试");
  expect(error).toHaveTextContent("磁盘空间不足");
  expect(screen.getByRole("textbox", { name: "输入问题或任务" })).toHaveValue("执行项目任务");
  expect(client.agentNewSession).not.toHaveBeenCalled();
});

it("离开项目后收到目录失败时标记来源并保留原草稿", async () => {
  await start();
  const original = useProjectsStore.getState().projects[0];
  act(() => useProjectsStore.setState({ projects: [original, { ...original, id: "project-b", name: "项目 B" }] }));
  let failWorkspace!: (error: Error) => void;
  vi.mocked(client.agentPrepareTaskWorkspace).mockImplementationOnce(() => new Promise((_, reject) => { failWorkspace = reject; }));

  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  await sendProjectDraft();
  await waitFor(() => expect(client.agentPrepareTaskWorkspace).toHaveBeenCalledOnce());
  fireEvent.click(screen.getByRole("button", { name: "打开 B 项目" }));
  await act(async () => failWorkspace(new Error("无法分配默认任务目录：磁盘空间不足")));

  expect(await screen.findByText(/项目「Project」发送失败.*磁盘空间不足/)).toBeInTheDocument();
  expect(document.querySelector(".pd-crumb__name")).toHaveTextContent("项目 B");
  expect(useSessionsStore.getState().drafts[`project:${original.id}`]).toBe("执行项目任务");
  expect(client.agentNewSession).not.toHaveBeenCalled();
});

it("临时目录不支持项目专家时保留草稿并说明如何恢复", async () => {
  await start();
  const project = useProjectsStore.getState().projects[0];
  const experts = [{ id: "/project/.echo-agent/agents/reviewer.md", name: "reviewer" }];
  act(() => {
    useProjectsStore.setState({ projects: [{ ...project, cwd: "/project", experts }] });
    useSessionsStore.getState().setDraft(`project-workspace:${project.id}`, "/other");
  });
  vi.mocked(client.agentsValidateReferences).mockRejectedValueOnce("专家 reviewer 在当前工作区由另一份定义生效");
  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  await sendProjectDraft();
  const error = await screen.findByText(/无法验证项目专家/);
  expect(error.closest('[role="alert"]')).toHaveTextContent("请换回项目默认目录");
  expect(error).toHaveTextContent("专家 reviewer 在当前工作区由另一份定义生效");
  expect(screen.getByRole("textbox", { name: "输入问题或任务" })).toHaveValue("执行项目任务");
  expect(client.agentsValidateReferences).toHaveBeenCalledWith("/other", experts);
  expect(client.agentNewSession).not.toHaveBeenCalled();
});

it("权限变化时刷新预览并保留草稿，用户确认后才按新权限执行", async () => {
  await start();
  const previousInvoke = vi.mocked(invoke).getMockImplementation()!;
  let policyChanged = false;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "permission_mode_get" && policyChanged) {
      return { permissionMode: "always-approve", configuredPermissionMode: "always-approve", locked: true };
    }
    return previousInvoke(command, args);
  });
  vi.mocked(client.agentNewSession).mockImplementationOnce(async () => {
    policyChanged = true;
    throw new Error("任务默认权限已变化，请确认当前权限后重新发送");
  });

  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  await sendProjectDraft();
  await screen.findByText(/任务默认权限已变化，请确认当前权限后重新发送/);
  const settings = screen.getByRole("group", { name: "任务执行设置" });
  await waitFor(() => expect(settings).toHaveTextContent("免审批"));
  expect(screen.getByRole("textbox", { name: "输入问题或任务" })).toHaveValue("执行项目任务");
  expect(client.agentNewSession).toHaveBeenCalledOnce();
  expect(client.agentNewSession).toHaveBeenLastCalledWith(expect.any(String), "model-a", undefined, "ask");
  expect(client.agentSend).not.toHaveBeenCalled();

  fireEvent.click(screen.getByRole("button", { name: "发送" }));
  await waitFor(() => expect(client.agentSend).toHaveBeenCalledOnce());
  expect(client.agentNewSession).toHaveBeenLastCalledWith(expect.any(String), "model-a", undefined, "always-approve");
});

it("创建对话期间通过项目面包屑返回列表，迟到结果只注册原项目", async () => {
  await start();
  fireEvent.click(screen.getByRole("button", { name: "查看项目" }));
  fireEvent.click(document.querySelector(".project-card2__main")!);
  await screen.findByRole("button", { name: "项目" });
  let finish!: (id: string) => void;
  vi.mocked(client.agentNewSession).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  await sendProjectDraft();
  await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledOnce());
  fireEvent.click(screen.getByRole("button", { name: "项目" }));
  expect(document.querySelector(".project-card2__main")).not.toBeNull();
  await act(async () => { finish("breadcrumb-late-task"); });
  expect(useSessionsStore.getState().currentSessionId).toBeNull();
  expect(document.querySelector(".project-card2__main")).not.toBeNull();
  expect(useProjectsStore.getState().projects[0].conversations.some(item => item.sessionId === "breadcrumb-late-task")).toBe(true);
});

it("项目会话显示可返回的路径，并按打开入口回到工作项或动态", async () => {
  await start();
  const project = useProjectsStore.getState().projects[0];
  act(() => {
    useProjectsStore.setState({ projects: [{
      ...project,
      conversations: [{ sessionId: "work-session", title: "已保存的会话标题", createdAt: project.createdAt }],
      tasks: [{ id: "work", title: "验收工作项", scope: "personal", source: "manual",
        status: "in_progress", sessionId: "work-session" }],
    }] });
    useSessionsStore.getState().upsert({ sessionId: "work-session", cwd: "/workspace", currentModelId: "model-a" });
    useSessionsStore.getState().setDraft("work-session", "未发送的会话草稿");
  });
  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  fireEvent.click(await screen.findByRole("button", { name: "工作项" }));
  fireEvent.click(screen.getByRole("button", { name: "打开会话" }));
  const path = await screen.findByRole("navigation", { name: "项目会话路径" });
  expect(within(path).getByText("已保存的会话标题")).toBeVisible();
  expect(within(path).getByRole("button", { name: "编辑标题" })).toBeInTheDocument();

  fireEvent.click(within(path).getByRole("button", { name: "Project" }));
  expect(await screen.findByText("验收工作项")).toBeVisible();
  expect(useSessionsStore.getState().drafts["work-session"]).toBe("未发送的会话草稿");

  fireEvent.click(screen.getByRole("button", { name: "从侧栏打开项目会话" }));
  const reopenedPath = await screen.findByRole("navigation", { name: "项目会话路径" });
  fireEvent.click(within(reopenedPath).getByRole("button", { name: "Project" }));
  const activity = await screen.findByRole("list", { name: "最近项目对话" });
  fireEvent.click(within(activity).getByText("已保存的会话标题", { exact: true }));
  const activityPath = await screen.findByRole("navigation", { name: "项目会话路径" });
  fireEvent.click(within(activityPath).getByRole("button", { name: "项目" }));
  expect(await screen.findByRole("button", { name: "打开项目 Project" })).toBeVisible();
});

it("从工作项启动的新会话也可以回到工作项", async () => {
  await start();
  const project = useProjectsStore.getState().projects[0];
  act(() => useProjectsStore.setState({ projects: [{
    ...project,
    tasks: [{ id: "work", title: "生成报告", scope: "personal", source: "manual", status: "pending" }],
  }] }));
  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  fireEvent.click(await screen.findByRole("button", { name: "工作项" }));
  const run = screen.getByRole("button", { name: "交给 Agent" });
  await waitFor(() => expect(run).toBeEnabled());
  fireEvent.click(run);
  const path = await screen.findByRole("navigation", { name: "项目会话路径" });
  fireEvent.click(within(path).getByRole("button", { name: "Project" }));
  expect(await screen.findByText("生成报告")).toBeVisible();
  expect(await screen.findByRole("button", { name: "打开会话" })).toBeVisible();
});

it("等待原项目创建时进入另一项目，仍保留当前项目页面", async () => {
  await start();
  const original = useProjectsStore.getState().projects[0];
  act(() => useProjectsStore.setState({ projects: [original, { ...original, id: "project-b", name: "项目 B", conversations: [] }] }));
  fireEvent.click(screen.getByRole("button", { name: "查看项目" }));
  let finish!: (id: string) => void;
  vi.mocked(client.agentNewSession).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  fireEvent.click(screen.getByRole("button", { name: "新建项目对话" }));
  await sendProjectDraft();
  await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledOnce());
  fireEvent.click(screen.getByRole("button", { name: "打开 B 项目" }));
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
  await sendProjectDraft();
  await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledTimes(1));
  fireEvent.click(screen.getByRole("button", { name: "新建 B 对话" }));
  await sendProjectDraft();
  await waitFor(() => expect(client.agentNewSession).toHaveBeenCalledTimes(2));
  await act(async () => { finishB("project-b-foreground"); });
  expect(useSessionsStore.getState().currentSessionId).toBe("project-b-foreground");
  await act(async () => { finishA("project-a-late"); });
  expect(useSessionsStore.getState().currentSessionId).toBe("project-b-foreground");
  expect(useSessionStore.getState().sessionId).toBe("project-b-foreground");
  expect(useProjectsStore.getState().projects.map(project => project.conversations.length)).toEqual([1, 1]);
});
