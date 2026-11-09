import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

vi.mock("@/lib/agent-client", () => ({
  filesystemPickDirectory: vi.fn().mockResolvedValue(null),
}));

import { isAutomaticTaskWorkspace, WorkspacePicker } from "../WorkspacePicker";
import { filesystemPickDirectory } from "@/lib/agent-client";
import { GlobalTooltip } from "../GlobalTooltip";

describe("WorkspacePicker", () => {
  it.each([
    { cwd: "/work/current", defaultWorkspaceCwd: "/default", automaticWorkspace: false, expected: "/work/current" },
    { cwd: undefined, defaultWorkspaceCwd: "/default", automaticWorkspace: true, expected: "/default" },
    { cwd: undefined, defaultWorkspaceCwd: undefined, automaticWorkspace: false, expected: undefined },
  ])("系统目录选择器从有效工作目录打开：$expected", async ({ expected, ...props }) => {
    vi.mocked(filesystemPickDirectory).mockClear();
    const select = vi.fn();
    render(<WorkspacePicker {...props} workspaces={[]} onSelectWorkspace={select} />);
    fireEvent.click(screen.getByRole("button", { name: /工作目录/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "选择文件夹…" }));
    expect(filesystemPickDirectory).toHaveBeenCalledTimes(1);
    expect(vi.mocked(filesystemPickDirectory).mock.calls[0][0]).toBe(expected);
    await waitFor(() => expect(screen.getByRole("menuitem", { name: "选择文件夹…" })).toBeEnabled());
    expect(select).not.toHaveBeenCalled();
  });

  it("自动目录与手动目录可在创建前选择，自动分配的历史任务不污染候选列表", () => {
    const select = vi.fn();
    const id = "019a3c14-99e0-7654-bff0-baa788d4bbdd";
    render(<WorkspacePicker automaticWorkspace defaultWorkspaceCwd="/default" workspaces={[
      {cwd:`/default/Tasks/${id}`, sessionCount:1}, {cwd:"/repo", sessionCount:2},
    ]} onSelectWorkspace={select} />);
    fireEvent.click(screen.getByRole("button", {name:/默认任务目录/}));
    expect(screen.getByRole("menuitemradio", {name:"默认任务目录"})).toHaveAttribute("aria-checked", "true");
    expect(screen.queryByTitle(`/default/Tasks/${id}`)).toBeNull();
    fireEvent.click(screen.getByRole("menuitemradio", {name:/repo/}));
    expect(select).toHaveBeenCalledWith("/repo");
    fireEvent.click(screen.getByRole("button", {name:/默认任务目录/}));
    fireEvent.click(screen.getByRole("menuitemradio", {name:"默认任务目录"}));
    expect(select).toHaveBeenLastCalledWith("");
    expect(isAutomaticTaskWorkspace("C:\\Default", `C:\\Default\\Tasks\\${id}`)).toBe(true);
    expect(isAutomaticTaskWorkspace("/default", "/default/Tasks/my-project")).toBe(false);
  });

  it("项目目录菜单显示并聚焦没有历史会话的临时目录", async () => {
    const select = vi.fn();
    const projectDefault = { cwd: "/project" };
    const props = { projectDefault, workspaces: [], onSelectWorkspace: select };
    vi.mocked(filesystemPickDirectory).mockResolvedValueOnce("/outside/new");
    const view = render(<WorkspacePicker {...props} cwd="/project" />);
    fireEvent.click(screen.getByRole("button", { name: /工作目录：\/project/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "选择文件夹…" }));
    await waitFor(() => expect(select).toHaveBeenCalledWith("/outside/new", expect.any(HTMLButtonElement)));
    view.rerender(<WorkspacePicker {...props} cwd="/outside/new" />);
    const trigger = screen.getByRole("button", { name: /工作目录：\/outside\/new/ });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    const current = screen.getByRole("menuitemradio", { name: "本次任务目录", checked: true });
    expect(current).toHaveAccessibleDescription("/outside/new");
    await waitFor(() => expect(current).toHaveFocus());
    fireEvent.click(screen.getByRole("menuitemradio", { name: "项目默认目录", checked: false }));
    expect(select).toHaveBeenLastCalledWith("", trigger);
    expect(trigger).toHaveFocus();
  });

  it("创建期间关闭目录菜单，阻止切换；系统选择框期间其他候选也保持锁定", async () => {
    let resolve!: (cwd: string | null) => void;
    vi.mocked(filesystemPickDirectory).mockImplementationOnce(() => new Promise(accept => { resolve = accept; }));
    const busy = vi.fn(), select = vi.fn();
    const props = {cwd:"/repo",workspaces:[{cwd:"/other",sessionCount:1}],onSelectWorkspace:select,onBusyChange:busy};
    const view = render(<WorkspacePicker {...props} />);
    fireEvent.click(screen.getByRole("button", {name:/工作目录/}));
    fireEvent.click(screen.getByRole("menuitem", {name:"选择文件夹…"}));
    expect(busy).toHaveBeenLastCalledWith(true);
    expect(screen.getByRole("menuitemradio", {name:/other/})).toBeDisabled();
    resolve(null);
    await waitFor(() => expect(busy).toHaveBeenLastCalledWith(false));
    expect(select).not.toHaveBeenCalled();
    view.rerender(<WorkspacePicker {...props} disabled />);
    expect(screen.queryByRole("menu")).toBeNull();
    expect(screen.getByRole("button", {name:/工作目录/})).toBeDisabled();
  });

  it("打开目录菜单后清除触发器提示，自动聚焦的选项不覆盖菜单", async () => {
    render(
      <>
        <WorkspacePicker
          cwd="/work/one"
          workspaces={[{ cwd: "/work/one", sessionCount: 1 }]}
          onSelectWorkspace={vi.fn()}
          menuPlacement="bottom"
        />
        <GlobalTooltip />
      </>,
    );
    const trigger = screen.getByRole("button", { name: /工作目录：\/work\/one/ });

    fireEvent.pointerOver(trigger);
    expect(await screen.findByRole("tooltip")).toHaveTextContent("当前工作目录：/work/one");

    fireEvent.click(trigger);
    expect(screen.getByRole("menu", { name: "选择工作目录" })).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("tooltip")).toBeNull());

    const current = screen.getByRole("menuitemradio", { name: /work\/one/ });
    fireEvent.pointerOver(current);
    expect(await screen.findByRole("tooltip")).toHaveTextContent("/work/one");
  });

  it("菜单通过 body portal 渲染，不受顶栏 overflow 裁剪", () => {
    render(
      <div data-testid="clipping-bar" style={{ overflow: "hidden" }}>
        <WorkspacePicker
          cwd="/work/one"
          workspaces={[
            { cwd: "/work/one", sessionCount: 1 },
            { cwd: "/work/two", sessionCount: 2 },
          ]}
          onSelectWorkspace={vi.fn()}
          menuPlacement="bottom"
        />
      </div>,
    );
    const trigger = screen.getByRole("button", { name: /工作目录：\/work\/one/ });
    trigger.getBoundingClientRect = () => ({
      x: 16,
      y: 58,
      top: 58,
      right: 196,
      bottom: 84,
      left: 16,
      width: 180,
      height: 26,
      toJSON: () => ({}),
    });

    fireEvent.click(trigger);

    const menu = screen.getByRole("menu", { name: "选择工作目录" });
    expect(menu.parentElement).toBe(document.body);
    expect(menu).toHaveStyle({ position: "fixed", zIndex: "var(--echo-layer-popover)" });
    expect(menu).toHaveAttribute("data-placement", "bottom");
    expect(trigger).toHaveAttribute("aria-controls", menu.id);
    expect(trigger).toHaveAttribute("data-tip", "当前工作目录：/work/one");
  });

  it("使用有语义菜单支持方向键、Escape 和焦点恢复", async () => {
    render(
      <WorkspacePicker
        cwd="/work/one"
        workspaces={[
          { cwd: "/work/one", sessionCount: 1 },
          { cwd: "/work/two", sessionCount: 2 },
        ]}
        onSelectWorkspace={vi.fn()}
      />,
    );

    const trigger = screen.getByRole("button", { name: /work\/one/ });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("menu", { name: "选择工作目录" })).toBeInTheDocument();
    const current = screen.getByRole("menuitemradio", { name: /work\/one/ });
    const next = screen.getByRole("menuitemradio", { name: /work\/two/ });
    await waitFor(() => expect(current).toHaveFocus());
    fireEvent.keyDown(current, { key: "ArrowDown" });
    expect(next).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(trigger).toHaveFocus();
  });

  it("从 portal 菜单切换目录后关闭菜单并恢复焦点", () => {
    const onSelectWorkspace = vi.fn();
    render(
      <WorkspacePicker
        cwd="/work/one"
        workspaces={[
          { cwd: "/work/one", sessionCount: 1 },
          { cwd: "/work/two", sessionCount: 2 },
        ]}
        onSelectWorkspace={onSelectWorkspace}
      />,
    );
    const trigger = screen.getByRole("button", { name: /工作目录：\/work\/one/ });

    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("menuitemradio", { name: /work\/two/ }));

    expect(onSelectWorkspace).toHaveBeenCalledWith("/work/two");
    expect(screen.queryByRole("menu")).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("同一页多个工作目录选择器使用独立菜单 ID", () => {
    render(
      <>
        <WorkspacePicker cwd="/work/one" workspaces={[]} onSelectWorkspace={vi.fn()} />
        <WorkspacePicker cwd="/work/two" workspaces={[]} onSelectWorkspace={vi.fn()} />
      </>,
    );
    const first = screen.getByRole("button", { name: /工作目录：\/work\/one/ });
    const second = screen.getByRole("button", { name: /工作目录：\/work\/two/ });

    fireEvent.click(first);
    const firstId = first.getAttribute("aria-controls");
    fireEvent.click(second);
    const secondId = second.getAttribute("aria-controls");

    expect(firstId).toBeTruthy();
    expect(secondId).toBeTruthy();
    expect(firstId).not.toBe(secondId);
  });
});


describe("目录选择的失败与取消", () => {
  it("取消不改变有效目录；失败可见，重试保留原始路径", async () => {
    const select = vi.fn();
    vi.mocked(filesystemPickDirectory).mockResolvedValueOnce(null).mockRejectedValueOnce(new Error("权限不足")).mockResolvedValueOnce("C:\\work\\new");
    render(<WorkspacePicker cwd="C:\\work\\old" workspaces={[]} onSelectWorkspace={select} />);
    fireEvent.click(screen.getByRole("button", {name:/工作目录/}));
    fireEvent.click(screen.getByRole("menuitem", {name:"选择文件夹…"}));
    await waitFor(() => expect(screen.getByRole("menuitem", {name:"选择文件夹…"})).toBeEnabled());
    expect(select).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("menuitem", {name:"选择文件夹…"}));
    expect(await screen.findByRole("alert")).toHaveTextContent("权限不足");
    expect(screen.getByRole("button", {name:/工作目录/})).toHaveTextContent("work/old");
    fireEvent.click(screen.getByRole("menuitem", {name:"重试选择文件夹…"}));
    await waitFor(() => expect(select).toHaveBeenCalledWith("C:\\work\\new"));
  });
});
