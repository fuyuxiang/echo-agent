import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { ProjectDetailView } from "../ProjectDetailView";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { projectAssetsUse } from "@/lib/agent-client";

vi.mock("@/lib/agent-client", () => ({
  projectAssetsUse: vi.fn().mockResolvedValue(["/private-project/A.md"]),
  filesystemAttachmentStats: vi.fn().mockResolvedValue({files:[{inputPath:"/private-project/A.md",path:"/private-project/A.md",sizeBytes:12}],rejected:[]}),
  projectsSave: vi.fn().mockResolvedValue(undefined),
  filesystemPickFiles: vi.fn().mockResolvedValue([]),
  openLocalPath: vi.fn(),
  projectAssetMakeDir: vi.fn(),
  projectAssetRemove: vi.fn(),
  projectAssetsImport: vi.fn(),
  discardAttachmentBlob: vi.fn().mockResolvedValue(undefined),
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
  useProjectsStore.setState({ projects: [structuredClone(project)] });
  useSessionsStore.setState({ independent: [], drafts: {} });
}

describe("ProjectDetailView 项目 composer 接入首页 Composer", () => {
  it("only explicitly selected project assets are granted and forwarded, then consumed", async () => {
    const project = { ...baseProject, assets: [
      { id: "a", name: "A.md", path: "/private-project/A.md", kind: "file" as const },
      { id: "b", name: "B.md", path: "/private-project/B.md", kind: "file" as const },
    ] };
    resetStores(project);
    vi.mocked(projectAssetsUse).mockResolvedValue(["/private-project/A.md"]);
    const start = vi.fn().mockResolvedValue("s-asset");
    render(<ProjectDetailView project={project} onBack={vi.fn()} onStartConversation={start} models={models} defaultModelId="model-a" picker={picker} />);
    fireEvent.click(screen.getByRole("button", { name: "项目资料" }));
    const row = screen.getByRole("row", { name: /A.md/ });
    fireEvent.click(within(row).getByRole("button", { name: "用于本次任务" }));
    expect(screen.getByRole("button", { name: "移除本次资料 A.md" })).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText(/Shift\+Enter/), { target: { value: "use chosen evidence" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(start).toHaveBeenCalledWith("p1", "use chosen evidence", "model-a", ["/private-project/A.md"]));
    expect(projectAssetsUse).toHaveBeenLastCalledWith("p1", ["/private-project/A.md"]);
    await waitFor(() => expect(screen.queryByRole("button", { name: "移除本次资料 A.md" })).toBeNull());
  });

  it("native project-asset rejection preserves both selection and task draft", async () => {
    const project = { ...baseProject, assets: [{ id: "a", name: "A.md", path: "/private-project/A.md", kind: "file" as const }] };
    resetStores(project); useSessionsStore.getState().setDraft("project-assets:p1", '["a"]');
    vi.mocked(projectAssetsUse).mockRejectedValueOnce(new Error("所选项目资料已更改"));
    const start = vi.fn();
    render(<ProjectDetailView project={project} onBack={vi.fn()} onStartConversation={start} models={models} defaultModelId="model-a" picker={picker} />);
    const input = screen.getByPlaceholderText(/Shift\+Enter/);
    fireEvent.change(input, { target: { value: "retry me" } }); fireEvent.click(screen.getByRole("button", { name: "发送" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("所选项目资料已更改");
    expect(start).not.toHaveBeenCalled(); expect(input).toHaveValue("retry me");
    expect(screen.getByRole("button", { name: "移除本次资料 A.md" })).toBeInTheDocument();
  });

  it("header chip 显示项目名与 0 项降级文案", () => {
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
    const header = screen.getByRole("status", { name: /项目「发布项目」/ });
    expect(header.textContent).toContain("发布项目");
    expect(header.textContent).toContain("尚未配置任务参考信息");
    expect(header.querySelector(".pd-composer-header__count--active")).toBeNull();
  });

  it("header chip 在含项目指令但无 Agent/Skill/MCP 时显示「项目指令」", () => {
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
    const header = screen.getByRole("status", { name: /已配置项目指令/ });
    expect(header.textContent).toContain("项目指令");
    expect(header.textContent).not.toContain("Agent");
    expect(header.textContent).not.toContain("Skill");
    expect(header.textContent).not.toContain("MCP");
  });

  it("header chip 在含 Agent/Skill/MCP 时按计数 > 0 高亮并显示完整摘要", () => {
    resetStores({
      ...baseProject,
      experts: [{ id: "a1", name: "需求分析师" }],
      skills: [
        { id: "s1", name: "code-review" },
        { id: "s2", name: "release-notes" },
      ],
      connectors: [],
    });
    render(
      <ProjectDetailView
        project={{
          ...baseProject,
          experts: [{ id: "a1", name: "需求分析师" }],
          skills: [
            { id: "s1", name: "code-review" },
            { id: "s2", name: "release-notes" },
          ],
          connectors: [],
        }}
        onBack={vi.fn()}
        onStartConversation={vi.fn()}
        models={models}
        defaultModelId="model-a"
        picker={picker}
      />,
    );
    const header = screen.getByRole("status", { name: /已配置 1 个专家、2 个技能、0 个连接器/ });
    expect(header.textContent).toContain("1 专家");
    expect(header.textContent).toContain("2 技能");
    expect(header.textContent).toContain("0 连接器");
    // Agent/Skill > 0 高亮;MCP = 0 不高亮。
    const activeCounts = header.querySelectorAll(".pd-composer-header__count--active");
    expect(activeCounts.length).toBe(2);
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

  it("附件由 Composer 透传到 onStartConversation 第 4 参", async () => {
    resetStores();
    const onStartConversation = vi.fn().mockResolvedValue("session-1");
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
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(onStartConversation).toHaveBeenCalled());
    // 第 4 参 attachments 一定存在(可能为 []),且只接收 4 参。
    const calls = onStartConversation.mock.calls;
    expect(calls.length).toBeGreaterThan(0);
    expect(calls[0]).toHaveLength(4);
    expect(calls[0][0]).toBe("p1");
    expect(calls[0][1]).toBe("请分析附件");
    expect(calls[0][2]).toBe("model-a");
    expect(Array.isArray(calls[0][3])).toBe(true);
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
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "发送" }));
    });

    await waitFor(() => {
      const alerts = screen.getAllByRole("alert");
      expect(alerts.some((node) => node.textContent?.includes("Network down"))).toBe(true);
    });
    expect(composer).toHaveValue("测试抛错");
  });
});
