import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { ProjectDetailView } from "../ProjectDetailView";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { usePermissionModeStore } from "@/stores/permission-mode-store";
import { filesystemPickDirectory, projectAssetsUse } from "@/lib/agent-client";
import type { StartProjectConversation } from "@/lib/project-context";

vi.mock("@/lib/agent-client", () => ({
  projectAssetsUse: vi.fn().mockResolvedValue(["/private-project/A.md"]),
  filesystemAttachmentStats: vi.fn().mockResolvedValue({files:[{inputPath:"/private-project/A.md",path:"/private-project/A.md",sizeBytes:12}],rejected:[]}),
  projectsSave: vi.fn().mockResolvedValue(undefined),
  filesystemPickFiles: vi.fn().mockResolvedValue([]),
  filesystemPickDirectory: vi.fn(),
  openLocalPath: vi.fn(),
  projectAssetMakeDir: vi.fn(),
  projectAssetRemove: vi.fn(),
  projectAssetsImport: vi.fn(),
  discardAttachmentBlob: vi.fn().mockResolvedValue(undefined),
  permissionModeGet: vi.fn().mockResolvedValue({ permissionMode: "ask", configuredPermissionMode: "ask", locked: false }),
}));

const baseProject: ProjectMeta = {
  id: "p1",
  name: "发布项目",
  cwd: "/workspace",
  createdAt: new Date().toISOString(),
  connectors: [],
  experts: [],
  skills: [],
  assets: [],
  members: [],
  conversations: [],
  plans: [],
  tasks: [],
};

const models = [
  { id: "model-a", label: "模型 A" },
  { id: "model-b", label: "模型 B" },
];

const picker = {
  options: { connectors: [], experts: [], skills: [] },
  loading: false,
  error: null,
};

function resetStores(project: ProjectMeta = baseProject) {
  useProjectsStore.setState({ projects: [structuredClone(project)], draftPermissions: {} });
  useSessionsStore.setState({ independent: [], drafts: {} });
  usePermissionModeStore.setState(usePermissionModeStore.getInitialState());
}

async function send() {
  const button = screen.getByRole("button", { name: "发送" });
  await waitFor(() => expect(button).toBeEnabled());
  fireEvent.click(button);
}

describe("ProjectDetailView 项目 composer 接入首页 Composer", () => {
  it("only explicitly selected project assets are granted and forwarded, then consumed", async () => {
    const project = { ...baseProject, assets: [
      { id: "a", name: "A.md", path: "/private-project/A.md", kind: "file" as const },
      { id: "b", name: "B.md", path: "/private-project/B.md", kind: "file" as const },
    ] };
    resetStores(project);
    vi.mocked(projectAssetsUse).mockResolvedValue(["/private-project/A.md"]);
    const start = vi.fn<StartProjectConversation>().mockImplementation(async (_id, _message, _modelId, options) => {
      expect(await options?.prepareAttachments?.()).toEqual(["/private-project/A.md"]);
      return "s-asset";
    });
    render(<ProjectDetailView project={project} onBack={vi.fn()} onStartConversation={start} models={models} defaultModelId="model-a" picker={picker} />);
    fireEvent.click(screen.getByRole("button", { name: "项目资料" }));
    const row = screen.getByRole("row", { name: /A.md/ });
    fireEvent.click(within(row).getByRole("button", { name: "用于本次任务" }));
    fireEvent.click(screen.getByRole("button", { name: "动态" }));
    const composer = screen.getByPlaceholderText(/Shift\+Enter/).closest(".echo-composer") as HTMLElement;
    expect(within(composer).getByRole("button", { name: "移除本次资料 A.md" })).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText(/Shift\+Enter/), { target: { value: "use chosen evidence" } });
    await send();
    await waitFor(() => expect(start).toHaveBeenCalledWith("p1", "use chosen evidence", "model-a", expect.objectContaining({ cwd: "/workspace" })));
    await waitFor(() => expect(screen.queryByRole("button", { name: "移除本次资料 A.md" })).toBeNull());
    expect(projectAssetsUse).toHaveBeenLastCalledWith("p1", ["/private-project/A.md"]);
  });

  it("allows stale and valid project selections to be removed with no model configured", () => {
    const project = { ...baseProject, assets: [{ id: "a", name: "A.md", path: "/private-project/A.md", kind: "file" as const }] };
    resetStores(project);
    useSessionsStore.getState().setDraft("project-assets:p1", '["a","deleted"]');
    const openModelSettings = vi.fn();
    render(<ProjectDetailView project={project} onBack={vi.fn()} onOpenModelSettings={openModelSettings} picker={picker} />);
    const controls = screen.getByRole("group", { name: "本次项目资料" });
    expect(within(controls).getByRole("alert")).toHaveTextContent("部分项目资料已失效");
    fireEvent.click(within(controls).getByRole("button", { name: "移除失效选择" }));
    expect(useSessionsStore.getState().drafts["project-assets:p1"]).toBe('["a"]');
    fireEvent.click(within(controls).getByRole("button", { name: "移除本次资料 A.md" }));
    expect(screen.queryByRole("group", { name: "本次项目资料" })).toBeNull();
    expect(useSessionsStore.getState().drafts["project-assets:p1"]).toBeUndefined();
    expect(openModelSettings).not.toHaveBeenCalled();
  });

  it("native project-asset rejection preserves both selection and task draft", async () => {
    const project = { ...baseProject, assets: [{ id: "a", name: "A.md", path: "/private-project/A.md", kind: "file" as const }] };
    resetStores(project); useSessionsStore.getState().setDraft("project-assets:p1", '["a"]');
    vi.mocked(projectAssetsUse).mockRejectedValueOnce(new Error("所选项目资料已更改"));
    const createSession = vi.fn();
    const start: StartProjectConversation = async (_id, _message, _modelId, options) => {
      await options?.prepareAttachments?.();
      return createSession();
    };
    render(<ProjectDetailView project={project} onBack={vi.fn()} onStartConversation={start} models={models} defaultModelId="model-a" picker={picker} />);
    const input = screen.getByPlaceholderText(/Shift\+Enter/);
    fireEvent.change(input, { target: { value: "retry me" } });
    await send();
    expect(await screen.findByRole("alert")).toHaveTextContent("所选项目资料已更改");
    expect(createSession).not.toHaveBeenCalled(); expect(input).toHaveValue("retry me");
    expect(screen.getByRole("button", { name: "移除本次资料 A.md" })).toBeInTheDocument();
  });

  it("目录不可用时保留草稿和资料，重选后仅对本次任务生效", async () => {
    const project = { ...baseProject, assets: [{ id: "a", name: "A.md", path: "/private-project/A.md", kind: "file" as const }] };
    resetStores(project);
    useSessionsStore.getState().setDraft("project-assets:p1", '["a"]');
    vi.mocked(filesystemPickDirectory).mockResolvedValueOnce("/missing").mockResolvedValueOnce("/available");
    const start = vi.fn()
      .mockRejectedValueOnce(new Error("无法解析会话工作区：/missing"))
      .mockResolvedValueOnce("session-1");
    render(<ProjectDetailView project={project} onBack={vi.fn()} onStartConversation={start} models={models} defaultModelId="model-a" picker={picker} />);
    const chooseFolder = (current: string) => {
      fireEvent.click(screen.getByRole("button", { name: new RegExp(`工作目录：${current}`) }));
      fireEvent.click(screen.getByRole("menuitem", { name: "选择文件夹…" }));
    };
    const input = screen.getByRole("textbox");
    fireEvent.change(input, { target: { value: "保留这条任务草稿" } });
    chooseFolder("/workspace");
    await screen.findByRole("button", { name: /工作目录：\/missing/ });
    await send();
    expect(await screen.findByRole("alert")).toHaveTextContent("工作目录不可用");
    expect(input).toHaveValue("保留这条任务草稿");
    expect(screen.getByRole("button", { name: "移除本次资料 A.md" })).toBeInTheDocument();

    chooseFolder("/missing");
    await screen.findByRole("button", { name: /工作目录：\/available/ });
    await send();
    await waitFor(() => expect(start).toHaveBeenLastCalledWith("p1", "保留这条任务草稿", "model-a", expect.objectContaining({ cwd: "/available", expectedPermissionMode: "ask" })));
    await screen.findByRole("button", { name: /工作目录：\/workspace/ });
    expect(useProjectsStore.getState().projects[0].cwd).toBe("/workspace");
  });

  it("切换目录前确认恢复草稿中的相对引用，并保留原文", async () => {
    resetStores();
    useSessionsStore.getState().setDraft("project:p1", "请检查 @src/index.ts#start");
    vi.mocked(filesystemPickDirectory).mockResolvedValue("/other");
    render(<ProjectDetailView project={baseProject} onBack={vi.fn()} models={models} defaultModelId="model-a" picker={picker} />);
    const choose = () => {
      fireEvent.click(screen.getByRole("button", { name: /工作目录：\/workspace/ }));
      fireEvent.click(screen.getByRole("menuitem", { name: "选择文件夹…" }));
    };
    choose();
    const confirmation = await screen.findByRole("dialog", { name: "更改草稿的工作目录？" });
    expect(confirmation).toHaveTextContent("/workspace");
    expect(confirmation).toHaveTextContent("/other");
    expect(confirmation).toHaveTextContent("相对 @ 引用");
    fireEvent.click(within(confirmation).getByRole("button", { name: "取消" }));
    expect(screen.getByRole("button", { name: /工作目录：\/workspace/ })).toBeInTheDocument();
    choose();
    fireEvent.click(await screen.findByRole("button", { name: "切换目录" }));
    await screen.findByRole("button", { name: /工作目录：\/other/ });
    expect(screen.getByRole("textbox")).toHaveValue("请检查 @src/index.ts#start");
  });

  it.each([true, false])("工作项使用当前所选目录和资料，独立草稿存在时保留设置 %s", async hasSeparateDraft => {
    const project: ProjectMeta = {
      ...baseProject,
      defaultModelId: "model-a",
      tasks: [{ id: "task", title: "核对发布", status: "pending", scope: "personal", source: "manual", modelId: "model-b" }],
      assets: [{ id: "a", name: "A.md", path: "/private-project/A.md", kind: "file" }],
    };
    resetStores(project);
    const drafts = useSessionsStore.getState();
    const text = hasSeparateDraft ? "待发送 @src/app.ts" : "";
    drafts.setDraft("project:p1", text);
    drafts.setDraft("project-workspace:p1", "/task");
    drafts.setDraft("project-assets:p1", '["a"]');
    const start = vi.fn<StartProjectConversation>().mockImplementation(async (_id, _message, _modelId, options) => {
      expect(await options?.prepareAttachments?.()).toEqual(["/private-project/A.md"]);
      return "work-session";
    });
    const detail = <ProjectDetailView project={project} onBack={vi.fn()} onStartConversation={start} models={models} picker={picker} />;
    const page = render(detail);
    fireEvent.click(await screen.findByRole("button", { name: "请求批准" }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /^本任务免审批/ }));
    fireEvent.click(screen.getByRole("button", { name: "确认开启" }));
    fireEvent.click(screen.getByRole("button", { name: "工作项" }));
    const run = screen.getByRole("button", { name: "交给 Agent" });
    await waitFor(() => expect(run).toBeEnabled());
    expect(screen.getByText(/交给 Agent 时使用任务默认权限/)).toHaveTextContent("请求批准");
    fireEvent.click(run);
    await waitFor(() => expect(useProjectsStore.getState().projects[0].tasks[0].sessionId).toBe("work-session"));
    expect(start).toHaveBeenCalledWith("p1", expect.stringContaining("核对发布"), "model-b", expect.objectContaining({
      cwd: "/task", rememberModel: false, requestedPermissionMode: undefined, expectedPermissionMode: "ask",
    }));
    expect(screen.getByRole("button", { name: "本任务免审批" })).toBeInTheDocument();
    expect(useProjectsStore.getState().projects[0].defaultModelId).toBe("model-a");
    expect(useSessionsStore.getState().drafts["project-assets:p1"]).toBe(hasSeparateDraft ? '["a"]' : undefined);
    expect(useSessionsStore.getState().drafts["project-workspace:p1"]).toBe(hasSeparateDraft ? "/task" : undefined);
    expect(screen.getByRole("textbox", { name: "输入问题或任务" })).toHaveValue(text);
    if (hasSeparateDraft) {
      page.unmount();
      start.mockResolvedValueOnce("draft-session");
      render(detail);
      expect(await screen.findByRole("button", { name: "本任务免审批" })).toBeVisible();
      await send();
      await waitFor(() => expect(start).toHaveBeenLastCalledWith("p1", text, "model-a", expect.objectContaining({
        requestedPermissionMode: "always-approve", expectedPermissionMode: "always-approve",
      })));
      expect(await screen.findByRole("button", { name: "请求批准" })).toBeVisible();
    }
  });

  it.each([
    baseProject,
    { ...baseProject, instructions: "只在完成时保存", experts: [{ id: "a1", name: "需求分析师" }] },
  ])("omits the composer summaries and keeps project preferences in the sidebar", project => {
    resetStores(project);
    render(
      <ProjectDetailView
        project={project}
        onBack={vi.fn()}
        onStartConversation={vi.fn()}
        models={models}
        defaultModelId="model-a"
        picker={picker}
      />,
    );
    expect(screen.queryByText("本次任务上下文")).toBeNull();
    expect(screen.queryByText("项目默认偏好：")).toBeNull();
    expect(screen.queryByRole("group", { name: "本次项目资料" })).toBeNull();
    const preferences = within(screen.getByRole("complementary")).getByText("项目默认偏好");
    expect(preferences.closest("details")).toBeInTheDocument();
  });

  it("在项目输入框下方显示任务执行设置", async () => {
    resetStores({ ...baseProject, instructions: "只在完成时保存" });
    render(
      <ProjectDetailView
        project={{ ...baseProject, instructions: "只在完成时保存" }}
        onBack={vi.fn()}
        onStartConversation={vi.fn()}
        models={models}
        defaultModelId="model-a"
        picker={picker}
      />,
    );
    const settings = screen.getByRole("group", { name: "任务执行设置" });
    expect(settings).toHaveTextContent("workspace");
    const permissions = await within(settings).findByRole("button", { name: "请求批准" });
    expect(within(settings).getByRole("button", { name: /工作目录：\/workspace/ })).toBeInTheDocument();
    expect(permissions).toHaveAttribute("aria-haspopup", "menu");
    const card = screen.getByRole("textbox").closest(".echo-composer");
    expect(card).not.toHaveClass("echo-composer--home");
    expect(card?.nextElementSibling).toBe(settings);
  });

  it("模型列表为空 + defaultModelId 不可用时显示外置 alert + 前往设置模型按钮", () => {
    useProjectsStore.setState({
      projects: [{ ...structuredClone(baseProject), defaultModelId: "organization/removed-model" }],
    });
    const onOpenModelSettings = vi.fn();
    render(
      <ProjectDetailView
        project={baseProject}
        onBack={vi.fn()}
        onStartConversation={vi.fn()}
        models={[]}
        picker={picker}
        onOpenModelSettings={onOpenModelSettings}
      />,
    );
    const alerts = screen.getAllByRole("alert");
    expect(alerts.map((node) => node.textContent).join(" ")).not.toContain("organization/");
    expect(
      alerts.some((node) => node.textContent?.includes("原项目模型")),
    ).toBe(true);
    expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "前往设置模型" }));
    expect(onOpenModelSettings).toHaveBeenCalledTimes(1);
  });

  it("Composer 模型列表为空时显示 setupHint 引导去设置模型", () => {
    resetStores();
    render(
      <ProjectDetailView
        project={baseProject}
        onBack={vi.fn()}
        onStartConversation={vi.fn()}
        models={[]}
        picker={picker}
        onOpenModelSettings={vi.fn()}
      />,
    );
    expect(screen.getByText("请先在「设置 → 模型」配置模型")).toBeInTheDocument();
  });

  it("切换 tab 时 Composer 实例不卸载,草稿保留", async () => {
    resetStores();
    render(
      <ProjectDetailView
        project={baseProject}
        onBack={vi.fn()}
        onStartConversation={vi.fn()}
        models={models}
        defaultModelId="model-a"
        picker={picker}
      />,
    );
    const composer = screen.getByPlaceholderText(/Shift\+Enter/);
    fireEvent.change(composer, { target: { value: "草稿 A" } });
    expect(composer).toHaveValue("草稿 A");

    // 切到计划 tab 再切回活动 tab。
    fireEvent.click(screen.getByRole("button", { name: "工作项" }));
    expect(composer).toHaveValue("草稿 A");
    fireEvent.click(screen.getByRole("button", { name: "动态" }));
    expect(composer).toHaveValue("草稿 A");
  });

  it("onStartConversation 接收发送时的目录、权限与附件准备", async () => {
    resetStores();
    const onStartConversation = vi.fn<StartProjectConversation>().mockResolvedValue("session-1");
    render(
      <ProjectDetailView
        project={baseProject}
        onBack={vi.fn()}
        onStartConversation={onStartConversation}
        models={models}
        defaultModelId="model-a"
        picker={picker}
      />,
    );
    const composer = screen.getByPlaceholderText(/Shift\+Enter/);
    fireEvent.change(composer, { target: { value: "请分析附件" } });
    await send();

    await waitFor(() => expect(onStartConversation).toHaveBeenCalled());
    const [id, message, model, options] = onStartConversation.mock.calls[0];
    expect([id, message, model]).toEqual(["p1", "请分析附件", "model-a"]);
    expect(options).toMatchObject({ cwd: "/workspace", expectedPermissionMode: "ask", rememberModel: true });
    expect(await options?.prepareAttachments?.()).toEqual([]);
  });

  it("onStartConversation 抛错时显示发送失败 alert 并保留输入", async () => {
    resetStores();
    const onStartConversation = vi.fn().mockRejectedValue(new Error("Network down"));
    render(
      <ProjectDetailView
        project={baseProject}
        onBack={vi.fn()}
        onStartConversation={onStartConversation}
        models={models}
        defaultModelId="model-a"
        picker={picker}
      />,
    );
    const composer = screen.getByPlaceholderText(/Shift\+Enter/);
    fireEvent.change(composer, { target: { value: "测试抛错" } });
    await send();

    await waitFor(() => {
      const alerts = screen.getAllByRole("alert");
      expect(alerts.some((node) => node.textContent?.includes("Network down"))).toBe(true);
    });
    expect(composer).toHaveValue("测试抛错");
  });
});
