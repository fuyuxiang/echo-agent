import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { ProjectsPanel } from "../ProjectsPanel";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { projectAssetsRemoveAll } from "@/lib/agent-client";

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
    const trigger = screen.getByRole("button", { name: "更多操作" });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue(rect(200, 228));

    fireEvent.click(trigger);

    const menu = screen.getByRole("menu", { name: "测试项目 操作菜单" });
    expect(menu.parentElement).toBe(document.body);
    expect(menu.closest(".project-card2")).toBeNull();
    expect(menu).toHaveStyle({ position: "fixed", zIndex: "var(--echo-layer-popover)" });
    expect(menu).toHaveAttribute("data-placement", "bottom");
    expect(screen.getByRole("menuitem", { name: "删除" })).toBeVisible();
  });

  it("底部空间不足时向上展开，避免与“从模版创建”卡片重叠", () => {
    render(<ProjectsPanel />);
    const trigger = screen.getByRole("button", { name: "更多操作" });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue(rect(540, 568));

    fireEvent.click(trigger);

    const menu = screen.getByRole("menu");
    expect(menu).toHaveAttribute("data-placement", "top");
    expect(Number.parseFloat(menu.style.top)).toBeLessThan(540);
  });

  it("页面滚动后跟随触发按钮重新定位", () => {
    render(<ProjectsPanel />);
    const trigger = screen.getByRole("button", { name: "更多操作" });
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
    const trigger = screen.getByRole("button", { name: "更多操作" });
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
    const trigger = screen.getByRole("button", { name: "更多操作" });
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
    expect(cancel).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("alertdialog", { name: /确定删除项目/ })).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("点击外部或按 Escape 都会关闭菜单", () => {
    render(<ProjectsPanel />);
    const trigger = screen.getByRole("button", { name: "更多操作" });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue(rect(200, 228));

    fireEvent.click(trigger);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();

    fireEvent.click(trigger);
    fireEvent.mouseDown(screen.getByRole("heading", { name: "项目" }));
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("项目模板通过 portal 展开，Escape 只关闭菜单，草稿关闭可继续编辑", async () => {
    render(<ProjectsPanel />);
    fireEvent.click(screen.getByRole("button", { name: "新建项目" }));
    const dialog = screen.getByRole("dialog", { name: "新建项目" });
    const input = within(dialog).getByPlaceholderText("请输入项目名称");
    fireEvent.change(input, { target: { value: "项目草稿" } });
    const trigger = within(dialog).getByRole("button", { name: "选择模板" });
    fireEvent.click(trigger);
    const menu = screen.getByRole("menu", { name: "项目模板" });
    expect(dialog.contains(menu)).toBe(false);
    expect(within(menu).getByRole("menuitemradio", { name: "自定义" })).toHaveFocus();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "项目模板" })).toBeNull();
    expect(trigger).toHaveFocus();
    expect(dialog).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.getByRole("alertdialog", { name: "舍弃未保存的修改？" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
    expect(input).toHaveValue("项目草稿");
    expect(dialog).toBeInTheDocument();
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
    fireEvent.click(within(dialog).getByRole("button", { name: "确定" }));

    await waitFor(() => expect(useProjectsStore.getState().projects[0]).toMatchObject({
      name: "模型定制项目",
      defaultModelId: "model-b",
    }));
  });
});
