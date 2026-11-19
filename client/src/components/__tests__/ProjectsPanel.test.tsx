import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ProjectsPanel } from "../ProjectsPanel";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { projectAssetsRemoveAll, projectsSave } from "@/lib/agent-client";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { publicationKey, sharedProjectId } from "@/lib/project-sharing";
import { projectCollaborationCapabilities, teamProjectRequest, type TeamProjectSnapshot } from "@/lib/team-project-client";

vi.mock("@/lib/team-project-client", async original => ({
  ...await original<object>(), teamProjectRequest: vi.fn().mockResolvedValue([]),
  projectCollaborationCapabilities: vi.fn(),
}));

vi.mock("../project-picker", async () => {
  const actual = await vi.importActual<typeof import("../project-picker")>("../project-picker");
  return {
    ...actual,
    useProjectPickerOptions: () => ({
      options: { connectors: [], experts: [], skills: [] },
      loading: false,
      error: null,
    }),
  };
});

vi.mock("@/lib/agent-client", async () => {
  const actual = await vi.importActual<typeof import("@/lib/agent-client")>("@/lib/agent-client");
  return {
    ...actual,
    projectsSave: vi.fn().mockResolvedValue(undefined),
    projectAssetsRemoveAll: vi.fn().mockResolvedValue(undefined),
    permissionModeGet: vi.fn().mockResolvedValue({
      permissionMode: "always-approve", configuredPermissionMode: "always-approve", locked: true,
    }),
  };
});

const project: ProjectMeta = {
  id: "project-1",
  name: "测试项目",
  createdAt: new Date().toISOString(),
  connectors: [],
  experts: [],
  skills: [],
  plans: [],
  tasks: [],
  assets: [],
  members: [],
  conversations: [],
};

const shared: TeamProjectSnapshot = {
  id: "shared-1", name: "共享交付", goal: "按期交付", scopeId: "org", ownerId: "owner",
  role: "admin", revision: 1, updatedAt: 1, members: [], tasks: [], assets: [], decisions: [], events: [],
};
const linked = { ...project, sharedProjects: { org: shared.id } };
const deletionCapabilities = { version: 5, projectDeletion: true, memberSearch: true, memberRoles: true, idempotentCreate: true };
const login = () => useOrgSessionStore.setState({
  session: { loggedIn: true, accountGeneration: 1, accountId: "org" }, boundaryRevision: 1,
});

function setViewport(width: number, height: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, value: height });
}

function rect(top: number, bottom: number): DOMRect {
  return {
    x: 820,
    y: top,
    top,
    bottom,
    left: 820,
    right: 860,
    width: 40,
    height: bottom - top,
    toJSON: () => ({}),
  };
}

describe("ProjectsPanel 项目操作菜单", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    vi.mocked(teamProjectRequest).mockResolvedValue([]);
    vi.mocked(projectCollaborationCapabilities).mockResolvedValue(deletionCapabilities);
    useOrgSessionStore.setState({ session: null, hydrated: true, boundaryRevision: 0 });
    setViewport(1000, 600);
    useProjectsStore.setState({
      projects: [structuredClone(project)],
      activeProjectId: null,
      persisting: false,
      persistError: null,
    });
  });

  afterEach(() => vi.restoreAllMocks());

  it("通过 body portal 渲染，不受项目卡片堆叠上下文限制", () => {
    render(<ProjectsPanel />);
    const trigger = screen.getByRole("button", { name: "测试项目 更多操作" });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue(rect(200, 228));

    fireEvent.click(trigger);

    const menu = screen.getByRole("menu", { name: "测试项目 操作菜单" });
    expect(menu.parentElement).toBe(document.body);
    expect(menu.closest(".project-card2")).toBeNull();
    expect(menu).toHaveStyle({ position: "fixed", zIndex: "var(--echo-layer-popover)" });
    expect(menu).toHaveAttribute("data-placement", "bottom");
    expect(screen.getByRole("menuitem", { name: "删除" })).toBeVisible();
  });

  it("首次加载或掉线时优先显示本机空状态，明确查看已共享时可重试目录", async () => {
    const user = userEvent.setup();
    let rejectRequest!: (reason: Error) => void;
    const reason = "refresh organization session: error sending request for url (https://org.example/api/v1/auth/refresh)";
    vi.mocked(teamProjectRequest).mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectRequest = reject; }));
    useProjectsStore.setState({ projects: [] });
    login();
    render(<ProjectsPanel />);
    expect(screen.getByText("还没有本机项目")).toBeVisible();
    expect(screen.queryByText("正在读取共享项目")).not.toBeInTheDocument();
    await act(async () => rejectRequest(new Error(reason)));
    expect(screen.queryByText("暂时无法连接组织服务")).not.toBeInTheDocument();

    const scope = screen.getByRole("combobox", { name: "项目范围" });
    await user.click(scope);
    await user.click(within(screen.getByRole("listbox")).getByRole("option", { name: "已共享" }));
    expect(screen.getByText("暂时无法连接组织服务")).toBeVisible();
    expect(screen.getByText(reason).closest("details")).not.toHaveAttribute("open");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText("还没有共享项目")).toBeVisible();
  });

  it("中途断网仍列出已知共享项目，点击后才提示，重试成功后才关联本机记录", async () => {
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([shared]);
    login();
    const first = render(<ProjectsPanel />);
    expect(await screen.findByRole("button", { name: "打开项目 共享交付" })).toBeVisible();
    first.unmount();

    vi.mocked(teamProjectRequest).mockRejectedValue(new Error("network disconnected"));
    render(<ProjectsPanel />);
    expect(screen.getByRole("button", { name: "打开项目 测试项目" })).toBeVisible();
    const enter = screen.getByRole("button", { name: "打开项目 共享交付" });
    await waitFor(() => expect(vi.mocked(teamProjectRequest).mock.calls.filter(([action]) => action === "list")).toHaveLength(2));
    expect(screen.queryByText("暂时无法连接组织服务")).not.toBeInTheDocument();
    expect(useProjectsStore.getState().projects).toEqual([project]);

    fireEvent.click(enter);
    expect(await screen.findByText("暂时无法连接组织服务")).toBeVisible();
    expect(useProjectsStore.getState().projects).toEqual([project]);
    vi.mocked(teamProjectRequest).mockResolvedValueOnce(shared);
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByRole("heading", { name: shared.name })).toBeVisible();
    expect(useProjectsStore.getState().projects.find(item => item.name === shared.name)?.sharedProjects).toEqual({ org: shared.id });
    expect(vi.mocked(teamProjectRequest).mock.calls.filter(([action]) => action === "snapshot")).toHaveLength(2);
  });

  it("已关联项目可读取本机资料，权限不足时共享操作保持锁定", async () => {
    const asset = { id: "private-file", name: "本机资料.md", kind: "file" as const, path: "/project/local.md" };
    login();
    useProjectsStore.setState({ projects: [{ ...linked, assets: [asset] }] });
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([shared]).mockRejectedValueOnce(new Error("HTTP 403 Forbidden"));
    render(<ProjectsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "打开项目 共享交付" }));
    expect(await screen.findByText("当前无法访问此共享项目")).toBeVisible();
    expect(screen.getByText(/此设备上的任务和资料仍可查看/)).toBeVisible();
    expect(screen.queryByText(/位参与者/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "问项目" })).toBeDisabled();
    fireEvent.click(screen.getByRole("tab", { name: "资料与成果" }));
    expect(within(screen.getByRole("tabpanel")).getByRole("button", { name: asset.name })).toBeVisible();
    expect(screen.getByRole("button", { name: "添加资料" })).toBeDisabled();
  });

  it("共享列表加载失败时保留关联卡片并提供目录重试", async () => {
    const remote = { ...shared, id: "another-shared", name: "新邀请的项目" };
    vi.mocked(teamProjectRequest).mockRejectedValueOnce(new Error("network disconnected")).mockResolvedValueOnce([shared, remote]);
    login();
    useProjectsStore.setState({ projects: [linked] });
    render(<ProjectsPanel />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("combobox", { name: "项目范围" }));
    await user.click(within(screen.getByRole("listbox")).getByRole("option", { name: "已共享" }));
    expect(screen.getByRole("button", { name: "打开项目 测试项目" })).toBeVisible();
    expect(screen.getByText(/当前共享列表可能不完整/)).toBeVisible();
    await user.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByRole("button", { name: "打开项目 新邀请的项目" })).toBeVisible();
    expect(screen.queryByText(/当前共享列表可能不完整/)).not.toBeInTheDocument();
  });

  it("成员收到已删除状态后可继续使用本机项目，并从共享列表移除旧卡片", async () => {
    const asset = { id: "private-file", name: "本机资料.md", kind: "file" as const, path: "/project/local.md" };
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([{ ...shared, role: "viewer" }]).mockRejectedValueOnce(new Error("共享项目已删除"));
    login();
    useProjectsStore.setState({ projects: [{ ...linked, assets: [asset] }] });
    render(<ProjectsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "打开项目 共享交付" }));
    expect(await screen.findByText("共享项目已删除")).toBeVisible();
    expect(screen.queryByRole("button", { name: "问项目" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "资料与成果" }));
    expect(within(screen.getByRole("tabpanel")).getByRole("button", { name: asset.name })).toBeVisible();
    expect(screen.getByRole("button", { name: "添加资料" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "项目" }));
    expect(screen.getByRole("button", { name: "打开项目 测试项目" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "打开项目 共享交付" })).not.toBeInTheDocument();
    expect(projectAssetsRemoveAll).not.toHaveBeenCalled();
  });

  it("首次进入前就能打开共享项目菜单，共享对话先进入草稿", async () => {
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([shared]).mockResolvedValueOnce(shared);
    login();
    const startTeamWork = vi.fn();
    render(<ProjectsPanel onStartTeamWork={startTeamWork} models={[{ id: "model-a", label: "模型" }]} defaultModelId="model-a" />);
    fireEvent.click(await screen.findByRole("button", { name: "共享交付 更多操作" }));
    const menu = screen.getByRole("menu", { name: "共享交付 操作菜单" });
    expect(within(menu).getByRole("menuitem", { name: "重命名" })).toBeVisible();
    expect(within(menu).getByRole("menuitem", { name: "删除共享项目" })).toBeVisible();
    expect(within(menu).queryByRole("menuitem", { name: "移除本机记录" })).not.toBeInTheDocument();
    expect(vi.mocked(teamProjectRequest).mock.calls.map(([action]) => action)).toEqual(["list"]);
    fireEvent.click(within(menu).getByRole("menuitem", { name: "开始项目对话" }));
    const input = await screen.findByPlaceholderText(/执行报告对项目成员可见/);
    await waitFor(() => expect(input).toHaveFocus());
    expect(startTeamWork).not.toHaveBeenCalled();
  });

  it("共享项目的查看者不会获得重命名、执行或删除入口", async () => {
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([{ ...shared, role: "viewer" }]);
    login();
    render(<ProjectsPanel onStartTeamWork={vi.fn()} />);
    expect(await screen.findByRole("button", { name: "打开项目 共享交付" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "共享交付 更多操作" })).not.toBeInTheDocument();
  });

  it("只有组织服务确认后才移除共享项目，并忽略之前在途的列表响应", async () => {
    let resolveOldList!: (projects: TeamProjectSnapshot[]) => void;
    let resolveDelete!: (result: { id: string }) => void;
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([shared]);
    login();
    render(<ProjectsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "共享交付 更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "删除共享项目" }));
    const dialog = screen.getByRole("alertdialog", { name: "删除共享项目「共享交付」？" });
    expect(vi.mocked(teamProjectRequest).mock.calls.map(([action]) => action)).toEqual(["list"]);
    vi.mocked(teamProjectRequest).mockImplementationOnce(() => new Promise(resolve => { resolveOldList = resolve; }));
    fireEvent.focus(window);
    vi.mocked(teamProjectRequest).mockImplementationOnce(() => new Promise(resolve => { resolveDelete = resolve; }));
    fireEvent.click(within(dialog).getByRole("button", { name: "删除共享项目" }));
    await waitFor(() => expect(teamProjectRequest).toHaveBeenCalledWith("delete", shared.id, { expectedRevision: shared.revision }, undefined,
      expect.objectContaining({ expectedGeneration: 1 })));
    expect(screen.getByRole("button", { name: "打开项目 共享交付" })).toBeInTheDocument();
    await act(async () => resolveDelete({ id: shared.id }));
    await act(async () => resolveOldList([shared]));
    expect(screen.queryByRole("button", { name: "打开项目 共享交付" })).not.toBeInTheDocument();
    expect(useProjectsStore.getState().projects).toEqual([project]);
    expect(projectAssetsRemoveAll).not.toHaveBeenCalled();
  });

  it("旧服务明确提示无法删除共享项目，本机记录和共享卡片都保留", async () => {
    vi.mocked(projectCollaborationCapabilities).mockResolvedValueOnce({ ...deletionCapabilities, projectDeletion: false });
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([shared]);
    login();
    useProjectsStore.setState({ projects: [linked] });
    render(<ProjectsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "共享交付 更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "删除共享项目" }));
    const dialog = screen.getByRole("alertdialog", { name: "删除共享项目「共享交付」？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "删除共享项目" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("当前组织服务版本尚不支持删除共享项目");
    expect(vi.mocked(teamProjectRequest).mock.calls.map(([action]) => action)).toEqual(["list"]);
    expect(useProjectsStore.getState().projects).toEqual([linked]);
    expect(projectAssetsRemoveAll).not.toHaveBeenCalled();
  });

  it("删除组织端只解除当前关联，保留本机数据和其他组织的旧版关联", async () => {
    const asset = { id: "private-file", name: "本机资料.md", kind: "file" as const, path: "/project/local.md", sharedAssets: { org: { projectId: shared.id, documentId: "doc" } } };
    const task = { id: "task", title: "本机任务", source: "manual", scope: "personal" as const, status: "pending" as const };
    const kept = { ...linked, assets: [asset], tasks: [task] };
    const legacy = { ...project, id: "legacy-local", name: "旧版关联项目" };
    localStorage.setItem(publicationKey(kept.id, "other"), JSON.stringify({ projectId: "other-shared" }));
    localStorage.setItem(publicationKey(legacy.id, "org"), JSON.stringify({ projectId: shared.id }));
    // A stale browser record can survive a failed cache eviction.
    vi.spyOn(Storage.prototype, "removeItem").mockImplementation(() => {});
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([shared]).mockResolvedValueOnce({ id: shared.id });
    login();
    useProjectsStore.setState({ projects: [kept, legacy] });
    render(<ProjectsPanel />);
    const [trigger] = await screen.findAllByRole("button", { name: "共享交付 更多操作" });
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("menuitem", { name: "删除共享项目" }));
    const dialog = screen.getByRole("alertdialog", { name: "删除共享项目「共享交付」？" });
    expect(dialog).toHaveTextContent("已有的项目记录和资料副本会保留");
    fireEvent.click(within(dialog).getByRole("button", { name: "删除共享项目" }));
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
    const [saved, savedLegacy] = useProjectsStore.getState().projects;
    expect(saved).toMatchObject({ id: kept.id, tasks: [task], assets: [asset], deletedSharedProjects: { org: shared.id } });
    expect(sharedProjectId(saved, "org")).toBeUndefined();
    expect(sharedProjectId(saved, "other")).toBe("other-shared");
    expect(sharedProjectId(savedLegacy, "org")).toBeUndefined();
    expect(await screen.findByRole("button", { name: "打开项目 旧版关联项目" })).toBeVisible();
    expect(projectAssetsRemoveAll).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "打开项目 共享交付" })).not.toBeInTheDocument();
  });

  it("解除共享的本机保存失败后可单独重试", async () => {
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([shared]).mockResolvedValueOnce({ id: shared.id });
    vi.mocked(projectsSave).mockRejectedValueOnce(new Error("磁盘空间不足"));
    login();
    useProjectsStore.setState({ projects: [linked] });
    const toast = vi.fn();
    render(<ProjectsPanel onToast={toast} />);
    fireEvent.click(await screen.findByRole("button", { name: "共享交付 更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "删除共享项目" }));
    fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "删除共享项目" }));
    const retry = await screen.findByRole("button", { name: "重试保存" });
    expect(screen.getByRole("button", { name: "打开项目 测试项目" })).toBeVisible();
    fireEvent.click(retry);
    await waitFor(() => expect(retry).not.toBeInTheDocument());
    expect(projectsSave).toHaveBeenLastCalledWith([expect.objectContaining({ deletedSharedProjects: { org: shared.id } })]);
    expect(vi.mocked(teamProjectRequest).mock.calls.filter(([action]) => action === "delete")).toHaveLength(1);
    expect(projectAssetsRemoveAll).not.toHaveBeenCalled();
  });

  it("移除本机记录后解释仍可看到共享卡片", async () => {
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([shared]);
    login();
    useProjectsStore.setState({ projects: [linked] });
    const onToast = vi.fn();
    render(<ProjectsPanel onToast={onToast} />);
    fireEvent.click(await screen.findByRole("button", { name: "共享交付 更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "移除本机记录" }));
    const dialog = screen.getByRole("alertdialog", { name: "移除「共享交付」的本机记录？" });
    expect(dialog).toHaveTextContent("组织端的共享项目仍可访问");
    fireEvent.click(within(dialog).getByRole("button", { name: "移除本机记录" }));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith("已移除本机记录。组织端的共享项目仍可访问。"));
    expect(screen.getByRole("button", { name: "打开项目 共享交付" })).toBeVisible();
    expect(useProjectsStore.getState().projects).toEqual([]);
  });

  it("管理员可直接重命名组织端项目，组织更新使用当前版本且不创建本机记录", async () => {
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([shared]).mockResolvedValueOnce(shared);
    login();
    render(<ProjectsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "共享交付 更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "重命名" }));
    const dialog = screen.getByRole("dialog", { name: "重命名项目“共享交付”" });
    fireEvent.change(within(dialog).getByRole("textbox", { name: "项目名称" }), { target: { value: "新版交付" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    await waitFor(() => expect(teamProjectRequest).toHaveBeenCalledWith("update", shared.id,
      { name: "新版交付", goal: shared.goal, expectedRevision: shared.revision }, undefined,
      expect.objectContaining({ expectedGeneration: 1 })));
    expect(useProjectsStore.getState().projects).toEqual([project]);
  });

  it("发现其他管理员已更名时保留输入并要求重新核对", async () => {
    const renamed = { ...shared, name: "其他管理员的名称", revision: shared.revision + 1 };
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([shared]).mockResolvedValueOnce(renamed).mockResolvedValueOnce([renamed])
      .mockResolvedValueOnce(renamed);
    login();
    render(<ProjectsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "共享交付 更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "重命名" }));
    const dialog = screen.getByRole("dialog", { name: "重命名项目“共享交付”" });
    const input = within(dialog).getByRole("textbox", { name: "项目名称" });
    fireEvent.change(input, { target: { value: "我填写的名称" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(renamed.name);
    expect(input).toHaveValue("我填写的名称");
    expect(vi.mocked(teamProjectRequest).mock.calls.some(([action]) => action === "update")).toBe(false);
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    await waitFor(() => expect(teamProjectRequest).toHaveBeenCalledWith("update", shared.id,
      { name: "我填写的名称", goal: renamed.goal, expectedRevision: renamed.revision }, undefined, expect.anything()));
  });

  it("切换组织身份后，迟到的共享项目快照无法创建本机记录", async () => {
    let resolveSnapshot!: (value: TeamProjectSnapshot) => void;
    vi.mocked(teamProjectRequest).mockResolvedValueOnce([shared]).mockImplementationOnce(() =>
      new Promise(resolve => { resolveSnapshot = resolve; }));
    login();
    render(<ProjectsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "打开项目 共享交付" }));
    expect(screen.getByRole("button", { name: "正在连接 共享交付" })).toBeDisabled();
    act(() => useOrgSessionStore.getState().clearSession());
    await act(async () => resolveSnapshot(shared));
    expect(screen.queryByText(shared.name)).not.toBeInTheDocument();
    expect(useProjectsStore.getState().projects).toEqual([project]);
  });

  it("快速启动确认显示工作目录和锁定的实际权限，再执行开场消息", async () => {
    const item = { ...project, cwd: "/project" };
    useProjectsStore.setState({ projects: [item] });
    const onStartProject = vi.fn();
    render(<ProjectsPanel onStartProject={onStartProject} />);
    fireEvent.click(screen.getByRole("button", { name: "测试项目 更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "开始项目对话" }));
    const confirmation = screen.getByRole("dialog", { name: "开始「测试项目」的项目对话？" });
    expect(confirmation).toHaveTextContent("/project");
    await waitFor(() => expect(confirmation).toHaveTextContent("免审批"));
    expect(onStartProject).not.toHaveBeenCalled();
    fireEvent.click(within(confirmation).getByRole("button", { name: "开始项目对话" }));
    await waitFor(() => expect(onStartProject).toHaveBeenCalledWith(item, "always-approve"));
  });

  it("底部空间不足时向上展开，避免与“从模版创建”卡片重叠", () => {
    render(<ProjectsPanel />);
    const trigger = screen.getByRole("button", { name: "测试项目 更多操作" });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue(rect(540, 568));

    fireEvent.click(trigger);

    const menu = screen.getByRole("menu");
    expect(menu).toHaveAttribute("data-placement", "top");
    expect(Number.parseFloat(menu.style.top)).toBeLessThan(540);
  });

  it("页面滚动后跟随触发按钮重新定位", () => {
    render(<ProjectsPanel />);
    const trigger = screen.getByRole("button", { name: "测试项目 更多操作" });
    let triggerRect = rect(200, 228);
    vi.spyOn(trigger, "getBoundingClientRect").mockImplementation(() => triggerRect);

    fireEvent.click(trigger);
    expect(screen.getByRole("menu")).toHaveAttribute("data-placement", "bottom");

    triggerRect = rect(540, 568);
    fireEvent.scroll(window);
    expect(screen.getByRole("menu")).toHaveAttribute("data-placement", "top");
  });

  it("Portal 中的删除项可点击并执行原有删除流程", async () => {
    render(<ProjectsPanel />);
    const trigger = screen.getByRole("button", { name: "测试项目 更多操作" });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue(rect(200, 228));

    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("menuitem", { name: "删除" }));
    const dialog = screen.getByRole("alertdialog", { name: "确定删除项目「测试项目」？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "删除项目" }));

    await waitFor(() => expect(projectAssetsRemoveAll).toHaveBeenCalledWith("project-1"));
    await waitFor(() => expect(useProjectsStore.getState().projects).toEqual([]));
    await waitFor(() => expect(screen.getByRole("button", { name: "新建项目" })).toHaveFocus());
  });

  it("删除确认默认聚焦取消，圈定 Tab 并在 Escape 后恢复菜单触发器", () => {
    render(<ProjectsPanel />);
    const trigger = screen.getByRole("button", { name: "测试项目 更多操作" });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue(rect(200, 228));
    trigger.focus();

    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("menuitem", { name: "删除" }));
    const dialog = screen.getByRole("alertdialog", { name: "确定删除项目「测试项目」？" });
    const cancel = within(dialog).getByRole("button", { name: "取消" });
    const remove = within(dialog).getByRole("button", { name: "删除项目" });
    expect(cancel).toHaveFocus();

    remove.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(within(dialog).getByRole("button", { name: "关闭对话框" })).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("alertdialog", { name: /确定删除项目/ })).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("点击外部或按 Escape 都会关闭菜单", () => {
    render(<ProjectsPanel />);
    const trigger = screen.getByRole("button", { name: "测试项目 更多操作" });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue(rect(200, 228));

    fireEvent.click(trigger);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();

    fireEvent.click(trigger);
    fireEvent.mouseDown(screen.getByRole("heading", { name: "项目" }));
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("模板按需展开，替换自定义指令前确认，关闭草稿可以继续编辑", async () => {
    render(<ProjectsPanel />);
    fireEvent.click(screen.getByRole("button", { name: "新建项目" }));
    const dialog = screen.getByRole("dialog", { name: "新建项目" });
    const input = within(dialog).getByPlaceholderText("请输入项目名称");
    fireEvent.change(input, { target: { value: "项目草稿" } });
    fireEvent.click(within(dialog).getByText("项目指令与模板"));
    const instructions = within(dialog).getByPlaceholderText("长期适用的背景、工作规范和输出要求");
    fireEvent.change(instructions, { target: { value: "保留自定义规范" } });
    const templates = within(dialog).getByRole("combobox", { name: "选择项目指令模板" }) as HTMLSelectElement;
    fireEvent.change(templates, { target: { value: templates.options[1].value } });
    const replace = screen.getByRole("dialog", { name: "替换现有项目配置？" });
    fireEvent.click(within(replace).getByRole("button", { name: "取消" }));
    expect(instructions).toHaveValue("保留自定义规范");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.getByRole("alertdialog", { name: "舍弃未保存的修改？" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
    expect(input).toHaveValue("项目草稿");
    expect(dialog).toBeInTheDocument();
  });

  it("保存失败后保留同一项目身份，重试成功后才进入项目", async () => {
    vi.mocked(projectsSave).mockRejectedValueOnce(new Error("磁盘暂不可用"));
    render(<ProjectsPanel />);
    fireEvent.click(screen.getByRole("button", { name: "新建项目" }));
    const dialog = screen.getByRole("dialog", { name: "新建项目" });
    fireEvent.change(within(dialog).getByPlaceholderText("请输入项目名称"), { target: { value: "重试项目" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "创建项目" }));
    await screen.findByText(/保存项目失败/);
    const id = useProjectsStore.getState().projects.find(p => p.name === "重试项目")!.id;
    expect(dialog).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "重试保存" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "新建项目" })).toBeNull());
    expect(useProjectsStore.getState().projects.filter(p => p.name === "重试项目")).toMatchObject([{ id }]);
  });

  it("新建项目与内层资源拾取器按层级圈定焦点并逐层 Escape 恢复", async () => {
    render(<ProjectsPanel />);
    const create = screen.getByRole("button", { name: "新建项目" });
    create.focus();
    fireEvent.click(create);

    const projectDialog = screen.getByRole("dialog", { name: "新建项目" });
    expect(within(projectDialog).getByPlaceholderText("请输入项目名称")).toHaveFocus();
    const addConnector = within(projectDialog).getAllByRole("button", { name: "+ 添加" })[0];
    addConnector.focus();
    fireEvent.click(addConnector);

    const pickerDialog = screen.getByRole("dialog", { name: "添加连接器" });
    expect(within(pickerDialog).getByRole("button", { name: "关闭" })).toHaveFocus();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "添加连接器" })).toBeNull();
    expect(screen.getByRole("dialog", { name: "新建项目" })).toBeInTheDocument();
    await waitFor(() => expect(addConnector).toHaveFocus());

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "新建项目" })).toBeNull();
    await waitFor(() => expect(create).toHaveFocus());
  });

  it("新建项目时可选择默认模型并保存到项目", async () => {
    render(
      <ProjectsPanel
        models={[
          { id: "model-a", label: "模型 A" },
          { id: "model-b", label: "模型 B" },
        ]}
        defaultModelId="model-a"
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "新建项目" }));
    const dialog = screen.getByRole("dialog", { name: "新建项目" });
    fireEvent.change(within(dialog).getByPlaceholderText("请输入项目名称"), {
      target: { value: "模型定制项目" },
    });
    fireEvent.change(within(dialog).getByRole("combobox", { name: "默认执行模型" }), {
      target: { value: "model-b" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "创建项目" }));

    await waitFor(() => expect(useProjectsStore.getState().projects[0]).toMatchObject({
      name: "模型定制项目",
      defaultModelId: "model-b",
    }));
  });
});
