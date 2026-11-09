import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { ActivityTab, AssetsTab, PlanTab, TaskTab } from "../project-tabs";
import { ProjectDetailView } from "../ProjectDetailView";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { filesystemPickFiles, projectAssetsImport } from "@/lib/agent-client";

vi.mock("@/lib/agent-client", () => ({
  projectsSave: vi.fn().mockResolvedValue(undefined),
  filesystemPickFiles: vi.fn().mockResolvedValue([]),
  openLocalPath: vi.fn(),
  projectAssetMakeDir: vi.fn(),
  projectAssetRemove: vi.fn(),
  projectAssetsImport: vi.fn(),
  permissionModeGet: vi.fn().mockResolvedValue({ permissionMode: "ask", configuredPermissionMode: "ask", locked: false }),
}));

const project: ProjectMeta = {
  id: "p1",
  name: "发布项目",
  cwd: "/workspace",
  createdAt: new Date().toISOString(),
  connectors: [], experts: [], skills: [], assets: [], members: [],
  conversations: [{ sessionId: "existing", title: "历史会话", createdAt: new Date().toISOString() }],
  plans: [{ id: "plan1", title: "完成发布检查", status: "pending" }],
  tasks: [{ id: "task1", title: "修复阻断问题", scope: "personal", source: "manual", status: "pending" }],
};
const models = [
  { id: "model-a", label: "模型 A" },
  { id: "model-b", label: "模型 B" },
];

describe("项目计划/任务与 Agent 会话闭环", () => {
  beforeEach(() => {
    useProjectsStore.setState({ projects: [structuredClone(project)] });
    useSessionsStore.setState({
      independent: [{ sessionId: "existing", title: "历史会话", cwd: "/workspace" }],
    });
  });

  it("看板工作项交给 Agent 后进入进行中并关联会话", async () => {
    useProjectsStore.setState({ projects: [{ ...structuredClone(project), tasks: [{ ...project.tasks[0], title: "完成发布检查" }] }] });
    const onRun = vi.fn().mockResolvedValue("session-plan");
    render(<PlanTab projectId="p1" models={models} defaultModelId="model-a" onRun={onRun} />);
    fireEvent.click(screen.getByRole("button", { name: "交给 Agent" }));
    await waitFor(() => expect(onRun).toHaveBeenCalledWith(
      expect.stringContaining("完成发布检查"),
      "model-a",
    ));
    await waitFor(() => expect(useProjectsStore.getState().projects[0].tasks[0]).toMatchObject({
      status: "in_progress", sessionId: "session-plan", modelId: "model-a",
    }));
  });

  it("任务支持 Agent 执行和人工状态流转", async () => {
    const onRun = vi.fn().mockResolvedValue("session-task");
    render(<TaskTab projectId="p1" models={models} defaultModelId="model-a" onRun={onRun} />);
    fireEvent.change(screen.getByRole("combobox", { name: /选择任务模型/ }), {
      target: { value: "model-b" },
    });
    fireEvent.click(screen.getByRole("button", { name: "交给 Agent" }));
    await waitFor(() => expect(onRun).toHaveBeenCalledWith(
      expect.stringContaining("修复阻断问题"),
      "model-b",
    ));
    await waitFor(() => expect(useProjectsStore.getState().projects[0].tasks[0]).toMatchObject({
      sessionId: "session-task",
      modelId: "model-b",
    }));
    expect(screen.queryByRole("button", { name: "交给 Agent" })).toBeNull();
    expect(screen.getByText(/模型 B/)).toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox", { name: /调整任务状态/ }), { target: { value: "completed" } });
    expect(useProjectsStore.getState().projects[0].tasks[0].status).toBe("in_progress");
    fireEvent.click(screen.getByRole("button", { name: "确认验收" }));
    await waitFor(() => expect(useProjectsStore.getState().projects[0].tasks[0].status).toBe("completed"));
  });

  it("项目任务的关联会话归档后可就地恢复并打开", async () => {
    useProjectsStore.setState({
      projects: [{
        ...structuredClone(project),
        tasks: [{
          ...project.tasks[0],
          sessionId: "archived-task-session",
          sessionArchived: true,
        }],
      }],
    });
    const onRestoreSession = vi.fn().mockResolvedValue(undefined);
    const onOpenSession = vi.fn();
    render(
      <TaskTab
        projectId="p1"
        onRestoreSession={onRestoreSession}
        onOpenSession={onOpenSession}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "恢复并打开" }));
    await waitFor(() => expect(onRestoreSession).toHaveBeenCalledWith("archived-task-session"));
    expect(onOpenSession).toHaveBeenCalledWith("archived-task-session");
  });

  it("没有可用模型时不允许任务提前进入进行中", () => {
    const onRun = vi.fn();
    render(<TaskTab projectId="p1" onRun={onRun} />);

    expect(screen.getByRole("combobox", { name: /选择任务模型/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "交给 Agent" })).toBeDisabled();
    expect(onRun).not.toHaveBeenCalled();
    expect(useProjectsStore.getState().projects[0].tasks[0].status).toBe("pending");
  });

  it("启动任务期间锁定模型与操作，避免重复创建会话", async () => {
    let finish!: (sessionId: string) => void;
    const onRun = vi.fn().mockImplementation(() => new Promise<string>((resolve) => {
      finish = resolve;
    }));
    render(<TaskTab projectId="p1" models={models} defaultModelId="model-a" onRun={onRun} />);

    fireEvent.click(screen.getByRole("button", { name: "交给 Agent" }));
    const running = screen.getByRole("button", { name: "启动中…" });
    expect(running).toBeDisabled();
    expect(screen.getByRole("combobox", { name: /选择任务模型/ })).toBeDisabled();
    fireEvent.click(running);
    expect(onRun).toHaveBeenCalledTimes(1);

    await act(async () => finish("session-task"));
    await waitFor(() => expect(useProjectsStore.getState().projects[0].tasks[0].sessionId).toBe("session-task"));
  });

  it("新建待办使用应用内输入对话框", async () => {
    render(<PlanTab projectId="p1" defaultModelId="model-a" />);
    fireEvent.click(screen.getByRole("button", { name: "+ 新建任务" }));
    const dialog = screen.getByRole("dialog", { name: "新建任务" });
    fireEvent.change(screen.getByRole("textbox", { name: "任务标题" }), { target: { value: "  发布验收  " } });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "新建任务" })).toBeNull());
    expect(dialog).not.toBeInTheDocument();
    expect(useProjectsStore.getState().projects[0].tasks).toEqual(expect.arrayContaining([
      expect.objectContaining({ title: "发布验收", status: "pending", modelId: undefined }),
    ]));
  });

  it("未显式选模型的新计划会在执行时继承最新项目默认模型", async () => {
    useProjectsStore.setState({
      projects: [{ ...structuredClone(project), plans: [], tasks: [] }],
    });
    const onRun = vi.fn().mockResolvedValue("inherited-model-session");
    const { rerender } = render(
      <PlanTab projectId="p1" models={models} defaultModelId="model-a" onRun={onRun} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "+ 新建任务" }));
    fireEvent.change(screen.getByRole("textbox", { name: "任务标题" }), {
      target: { value: "继承模型" },
    });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));
    await waitFor(() => expect(useProjectsStore.getState().projects[0].tasks[0].modelId).toBeUndefined());

    rerender(<PlanTab projectId="p1" models={models} defaultModelId="model-b" onRun={onRun} />);
    fireEvent.click(screen.getByRole("button", { name: "交给 Agent" }));
    await waitFor(() => expect(onRun).toHaveBeenCalledWith(
      expect.stringContaining("继承模型"),
      "model-b",
    ));
  });

  it("未显式选模型的新任务不会固化创建时的默认模型", async () => {
    useProjectsStore.setState({
      projects: [{ ...structuredClone(project), tasks: [] }],
    });
    const onRun = vi.fn().mockResolvedValue("inherited-task-session");
    const { rerender } = render(
      <TaskTab projectId="p1" models={models} defaultModelId="model-a" onRun={onRun} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "+ 新建任务" }));
    fireEvent.change(screen.getByRole("textbox", { name: "任务标题" }), {
      target: { value: "继承任务模型" },
    });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));
    await waitFor(() => expect(useProjectsStore.getState().projects[0].tasks[0].modelId).toBeUndefined());

    rerender(<TaskTab projectId="p1" models={models} defaultModelId="model-b" onRun={onRun} />);
    fireEvent.click(screen.getByRole("button", { name: "交给 Agent" }));
    await waitFor(() => expect(onRun).toHaveBeenCalledWith(
      expect.stringContaining("继承任务模型"),
      "model-b",
    ));
  });

  it("删除任务需确认，取消不变更项目", () => {
    render(<TaskTab projectId="p1" />);
    const remove = screen.getByRole("button", { name: "删除任务 修复阻断问题" });
    fireEvent.click(remove);
    const dialog = screen.getByRole("alertdialog", { name: "删除任务“修复阻断问题”？" });
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(dialog).not.toBeInTheDocument();
    expect(useProjectsStore.getState().projects[0].tasks).toHaveLength(1);
  });

  it("项目动态可直接打开真实历史会话", () => {
    const onOpenSession = vi.fn();
    render(<ActivityTab projectId="p1" onOpenSession={onOpenSession} />);
    fireEvent.click(screen.getByText("历史会话"));
    expect(onOpenSession).toHaveBeenCalledWith("existing", "/workspace");
  });

  it("会话摘要的归档状态优先于项目镜像字段", () => {
    useSessionsStore.setState({
      independent: [{
        sessionId: "existing",
        title: "历史会话",
        cwd: "/workspace",
        archived: true,
      }],
    });
    render(<ActivityTab projectId="p1" onOpenSession={vi.fn()} />);

    expect(screen.queryByText("历史会话")).toBeNull();
    expect(screen.getByRole("button", { name: "1 个已归档" })).toBeInTheDocument();
  });

  it("旧运行时未返回归档字段时保留项目镜像状态", () => {
    useProjectsStore.setState({
      projects: [{
        ...structuredClone(project),
        conversations: [{ ...project.conversations[0], archived: true }],
      }],
    });
    useSessionsStore.setState({
      independent: [{ sessionId: "existing", title: "历史会话", cwd: "/workspace" }],
    });

    render(<ActivityTab projectId="p1" onOpenSession={vi.fn()} />);
    expect(screen.queryByText("历史会话")).toBeNull();
    expect(screen.getByRole("button", { name: "1 个已归档" })).toBeInTheDocument();
  });

  it("项目动态不把隐藏子代理记录当作顶层项目对话", () => {
    useProjectsStore.setState({
      projects: [{
        ...structuredClone(project),
        conversations: [
          ...project.conversations,
          { sessionId: "child", title: "内部子代理", createdAt: new Date().toISOString() },
        ],
      }],
    });
    useSessionsStore.setState({
      independent: [
        { sessionId: "existing", title: "历史会话", cwd: "/workspace" },
        { sessionId: "child", title: "内部子代理", cwd: "/workspace", hidden: true },
      ],
    });

    render(<ActivityTab projectId="p1" onOpenSession={vi.fn()} />);
    expect(screen.getByText("历史会话")).toBeInTheDocument();
    expect(screen.queryByText("内部子代理")).toBeNull();
    expect(screen.getByRole("button", { name: "1 个对话" })).toBeInTheDocument();
  });

  it("计划和任务分开展示项目进度与 Agent 执行状态", () => {
    useProjectsStore.setState({
      projects: [{
        ...structuredClone(project),
        plans: [{ ...project.plans[0], sessionId: "plan-session", modelId: "model-a" }],
        tasks: [{ ...project.tasks[0], sessionId: "task-session", modelId: "model-a" }],
      }],
    });
    useSessionsStore.setState({
      independent: [
        { sessionId: "plan-session", title: "计划会话", cwd: "/workspace", status: "failed" },
        { sessionId: "task-session", title: "任务会话", cwd: "/workspace", status: "awaiting_answer" },
      ],
    });

    useProjectsStore.setState({ projects: [{ ...useProjectsStore.getState().projects[0], tasks: [{ ...project.tasks[0], sessionId: "plan-session" }] }] });
    const { unmount } = render(<PlanTab projectId="p1" models={models} />);
    expect(screen.getByLabelText(/Agent 执行状态：执行失败/)).toBeInTheDocument();
    expect(useProjectsStore.getState().projects[0].tasks[0].status).toBe("pending");
    unmount();
    useProjectsStore.setState({ projects: [{ ...useProjectsStore.getState().projects[0], tasks: [{ ...project.tasks[0], sessionId: "task-session" }] }] });
    render(<TaskTab projectId="p1" models={models} />);
    expect(screen.getByLabelText(/Agent 执行状态：等待回答/)).toBeInTheDocument();
    expect(screen.getByText(/待开始 · 已关联 Agent/)).toBeInTheDocument();
    expect(useProjectsStore.getState().projects[0].tasks[0].status).toBe("pending");
  });

  it("项目动态展示可信执行状态并支持待处理筛选与优先排序", () => {
    useProjectsStore.setState({
      projects: [{
        ...structuredClone(project),
        conversations: [
          { sessionId: "done", title: "已结束对话", createdAt: "2026-09-14T10:00:00Z" },
          { sessionId: "failed", title: "失败对话", createdAt: "2026-09-14T09:00:00Z" },
          { sessionId: "answer", title: "待回答对话", createdAt: "2026-09-14T08:00:00Z" },
          { sessionId: "running", title: "执行中对话", createdAt: "2026-09-14T07:00:00Z" },
        ],
      }],
    });
    useSessionsStore.setState({
      independent: [
        { sessionId: "done", title: "已结束对话", cwd: "/workspace", status: "completed", updatedAt: "2026-09-14T10:00:00Z" },
        { sessionId: "failed", title: "失败对话", cwd: "/workspace", status: "failed", updatedAt: "2026-09-14T09:00:00Z" },
        { sessionId: "answer", title: "待回答对话", cwd: "/workspace", status: "awaiting_answer", updatedAt: "2026-09-14T08:00:00Z" },
        { sessionId: "running", title: "执行中对话", cwd: "/workspace", status: "working", updatedAt: "2026-09-14T07:00:00Z" },
      ],
    });

    render(<ActivityTab projectId="p1" onOpenSession={vi.fn()} />);

    expect(screen.getByLabelText(/执行状态：等待回答/)).toBeInTheDocument();
    expect(screen.getByLabelText(/执行状态：执行失败/)).toBeInTheDocument();
    expect(screen.getByLabelText(/执行状态：执行中/)).toBeInTheDocument();
    expect(screen.getByLabelText(/执行状态：本轮已结束/)).toHaveAttribute(
      "title",
      expect.stringContaining("不代表项目任务已完成"),
    );
    const list = screen.getByRole("list", { name: "最近项目对话" });
    expect(within(list).getAllByRole("listitem")[0]).toHaveTextContent("待回答对话");

    fireEvent.click(screen.getByRole("button", { name: /需处理，2 个对话/ }));
    expect(screen.getByText("待回答对话")).toBeInTheDocument();
    expect(screen.getByText("失败对话")).toBeInTheDocument();
    expect(screen.queryByText("已结束对话")).toBeNull();
    expect(screen.queryByText("执行中对话")).toBeNull();
  });

  it("项目动态不会把缺少持久化状态的旧记录伪装成已完成", () => {
    render(<ActivityTab projectId="p1" onOpenSession={vi.fn()} />);
    expect(screen.getByLabelText(/执行状态：历史状态未知/)).toBeInTheDocument();
    expect(screen.getByText(/不等同于项目任务进度/)).toBeInTheDocument();
    expect(screen.queryByLabelText(/执行状态：本轮已结束/)).toBeNull();
  });

  it("归档会话不会在项目动态中伪装成可打开项", () => {
    useProjectsStore.setState({
      projects: [{
        ...structuredClone(project),
        conversations: [
          ...project.conversations,
          { sessionId: "archived", title: "已归档", createdAt: new Date().toISOString(), archived: true },
        ],
      }],
    });
    render(<ActivityTab projectId="p1" onOpenSession={vi.fn()} />);
    expect(screen.queryByText("已归档")).toBeNull();
    expect(screen.getByText(/1 个已归档/)).toBeInTheDocument();
  });

  it("项目动态可查看已归档对话并直接恢复", async () => {
    useProjectsStore.setState({
      projects: [{
        ...structuredClone(project),
        conversations: [{
          sessionId: "archived",
          title: "已归档对话",
          createdAt: new Date().toISOString(),
          archived: true,
        }],
      }],
    });
    const onArchiveSession = vi.fn().mockResolvedValue(undefined);
    render(
      <ActivityTab
        projectId="p1"
        onOpenSession={vi.fn()}
        onArchiveSession={onArchiveSession}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "1 个已归档" }));
    expect(screen.getByText("已归档对话")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "已归档对话的会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "恢复会话" }));

    await waitFor(() => expect(onArchiveSession).toHaveBeenCalledWith("archived", false, "/workspace"));
  });

  it("项目对话永久删除必须二次确认，取消不触发后端", async () => {
    const onDeleteSession = vi.fn().mockResolvedValue(undefined);
    const { rerender } = render(
      <ActivityTab projectId="p1" onOpenSession={vi.fn()} onDeleteSession={onDeleteSession} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "历史会话的会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "永久删除" }));

    const dialog = screen.getByRole("alertdialog", { name: "永久删除对话“历史会话”？" });
    expect(dialog).toHaveTextContent("无法恢复");
    expect(dialog).toHaveTextContent("项目资产和工作区原始文件不会被删除");
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(onDeleteSession).not.toHaveBeenCalled();

    rerender(<ActivityTab projectId="p1" onOpenSession={vi.fn()} onDeleteSession={onDeleteSession} />);
    fireEvent.click(screen.getByRole("button", { name: "历史会话的会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "永久删除" }));
    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));
    await waitFor(() => expect(onDeleteSession).toHaveBeenCalledWith("existing", "/workspace"));
  });

  it("移出关联工作项的会话前说明影响，确认后保留对话历史", async () => {
    useProjectsStore.setState({
      projects: [{
        ...structuredClone(project),
        plans: [{ ...project.plans[0], sessionId: "existing" }],
        tasks: [{ ...project.tasks[0], sessionId: "existing" }],
      }],
    });
    render(<ActivityTab projectId="p1" onOpenSession={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "历史会话的会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "移出项目" }));

    const confirmation = screen.getByRole("dialog", { name: "将“历史会话”移出项目？" });
    expect(confirmation).toHaveTextContent("完成发布检查");
    expect(confirmation).toHaveTextContent("修复阻断问题");
    expect(useProjectsStore.getState().projects[0].conversations).toHaveLength(1);
    fireEvent.click(within(confirmation).getByRole("button", { name: "取消" }));
    expect(useProjectsStore.getState().projects[0].tasks[0].sessionId).toBe("existing");

    fireEvent.click(screen.getByRole("button", { name: "历史会话的会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "移出项目" }));
    fireEvent.click(within(screen.getByRole("dialog", { name: "将“历史会话”移出项目？" })).getByRole("button", { name: "移出项目" }));

    await waitFor(() => expect(useProjectsStore.getState().projects[0].conversations).toHaveLength(0));
    const updated = useProjectsStore.getState().projects[0];
    expect(updated.conversations).toHaveLength(0);
    expect(updated.plans[0].sessionId).toBeUndefined();
    expect(updated.tasks[0].sessionId).toBeUndefined();
    expect(useSessionsStore.getState().independent).toEqual(expect.arrayContaining([
      expect.objectContaining({ sessionId: "existing" }),
    ]));
  });

  it("归档和删除会话同步所有项目引用", () => {
    useProjectsStore.setState({
      projects: [{
        ...structuredClone(project),
        plans: [{ ...project.plans[0], sessionId: "existing" }],
        tasks: [{ ...project.tasks[0], sessionId: "existing" }],
      }],
    });
    useProjectsStore.getState().setSessionArchived("existing", true);
    let updated = useProjectsStore.getState().projects[0];
    expect(updated.conversations[0].archived).toBe(true);
    expect(updated.plans[0].sessionArchived).toBe(true);
    expect(updated.tasks[0].sessionArchived).toBe(true);

    useProjectsStore.getState().reconcileSessionArchiveStates([{
      sessionId: "existing",
      archived: false,
    }]);
    updated = useProjectsStore.getState().projects[0];
    expect(updated.conversations[0].archived).toBe(false);
    expect(updated.plans[0].sessionArchived).toBe(false);
    expect(updated.tasks[0].sessionArchived).toBe(false);

    useProjectsStore.getState().removeSessionReferences("existing");
    updated = useProjectsStore.getState().projects[0];
    expect(updated.conversations).toHaveLength(0);
    expect(updated.plans[0].sessionId).toBeUndefined();
    expect(updated.tasks[0].sessionId).toBeUndefined();
  });

  it("批量导入资产时防止重复提交并一次更新项目", async () => {
    vi.mocked(filesystemPickFiles).mockResolvedValueOnce(["/tmp/report.pdf"]);
    let finishImport!: (assets: Awaited<ReturnType<typeof projectAssetsImport>>) => void;
    vi.mocked(projectAssetsImport).mockReturnValueOnce(new Promise((resolve) => {
      finishImport = resolve;
    }));

    render(<AssetsTab projectId="p1" />);
    fireEvent.click(screen.getByRole("button", { name: "导入文件副本" }));

    await waitFor(() => expect(projectAssetsImport).toHaveBeenCalledWith("p1", ["/tmp/report.pdf"]));
    expect(screen.getByRole("button", { name: "导入中…" })).toBeDisabled();

    await act(async () => finishImport([{
      name: "report.pdf",
      path: "/private/project/report.pdf",
      kind: "file",
      ext: "PDF",
      sizeBytes: 12,
      updatedAt: new Date().toISOString(),
    }]));
    await waitFor(() => expect(screen.getByText("report.pdf")).toBeInTheDocument());
    expect(useProjectsStore.getState().projects[0].assets).toHaveLength(1);
    expect(screen.getByRole("button", { name: "删除资产 report.pdf" })).toBeInTheDocument();
  });

  it("项目消息未启动时保留草稿并给出可操作提示", async () => {
    const onStartConversation = vi.fn().mockResolvedValue(undefined);
    render(
      <ProjectDetailView
        project={project}
        onBack={vi.fn()}
        onStartConversation={onStartConversation}
        models={models}
        defaultModelId="model-a"
        picker={{ options: { connectors: [], experts: [], skills: [] }, loading: false, error: null }}
      />,
    );
    const composer = screen.getByPlaceholderText(/Shift\+Enter/);
    fireEvent.change(composer, { target: { value: "请生成发布清单" } });
    await waitFor(() => expect(screen.getByRole("button", { name: "发送" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => {
      const alerts = screen.getAllByRole("alert");
      expect(alerts.some((node) => node.textContent?.includes("消息尚未发送"))).toBe(true);
    });
    expect(onStartConversation).toHaveBeenCalledWith("p1", "请生成发布清单", "model-a", expect.objectContaining({ cwd: "/workspace" }));
    expect(composer).toHaveValue("请生成发布清单");
  });

  it("项目对话允许显式切换模型并持久化为项目默认值", async () => {
    const onStartConversation = vi.fn().mockResolvedValue("session-chat");
    render(
      <ProjectDetailView
        project={project}
        onBack={vi.fn()}
        onStartConversation={onStartConversation}
        models={models}
        defaultModelId="model-a"
        picker={{ options: { connectors: [], experts: [], skills: [] }, loading: false, error: null }}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /模型 A/ }));
    fireEvent.click(screen.getByRole("option", { name: /模型 B/ }));
    fireEvent.change(screen.getByPlaceholderText(/Shift\+Enter/), { target: { value: "执行发布" } });
    await waitFor(() => expect(screen.getByRole("button", { name: "发送" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(onStartConversation).toHaveBeenCalledWith(
      "p1",
      "执行发布",
      "model-b",
      expect.objectContaining({ cwd: "/workspace", expectedPermissionMode: "ask" }),
    ));
    expect(useProjectsStore.getState().projects[0].defaultModelId).toBe("model-b");
  });

  it("项目默认模型被删除后不静默回退到其他模型", () => {
    useProjectsStore.setState({
      projects: [{ ...structuredClone(project), defaultModelId: "removed-model" }],
    });
    render(
      <ProjectDetailView
        project={project}
        onBack={vi.fn()}
        onStartConversation={vi.fn()}
        models={models}
        defaultModelId="model-a"
        picker={{ options: { connectors: [], experts: [], skills: [] }, loading: false, error: null }}
      />,
    );

    expect(
      screen
        .getAllByRole("alert")
        .some((node) => node.textContent?.includes("原项目模型")),
    ).toBe(true);
    expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
  });

  it("项目指令在关闭配置时再持久化，避免每次按键写盘", () => {
    render(
      <ProjectDetailView
        project={project}
        onBack={vi.fn()}
        picker={{ options: { connectors: [], experts: [], skills: [] }, loading: false, error: null }}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /指令/ }));
    const input = screen.getByPlaceholderText("填写项目目标、背景、工作规范和交付要求…");
    fireEvent.change(input, { target: { value: "只在完成时保存" } });
    expect(useProjectsStore.getState().projects[0].instructions).toBeUndefined();

    fireEvent.click(screen.getByRole("button", { name: "完成" }));
    expect(useProjectsStore.getState().projects[0].instructions).toBe("只在完成时保存");
  });
});
