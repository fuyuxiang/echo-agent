import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { TopbarActions } from "../TopbarActions";
import { GlobalTooltip } from "../GlobalTooltip";
import { useModalFocus } from "@/lib/use-modal-focus";
import { exportTextFile } from "@/lib/agent-client";
import { useSessionStore } from "@/stores/session-store";

vi.mock("@/lib/agent-client", () => ({
  exportTextFile: vi.fn(),
}));

function rect(left: number, top: number, width = 32, height = 32): DOMRect {
  return {
    x: left,
    y: top,
    left,
    top,
    right: left + width,
    bottom: top + height,
    width,
    height,
    toJSON: () => ({}),
  };
}

function setViewport(width: number, height: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, value: height });
}

function ProgrammaticDialog() {
  const ref = useModalFocus<HTMLDivElement>(true, () => {});
  return <div ref={ref} role="dialog" aria-modal="true" aria-label="程序弹窗" tabIndex={-1} />;
}

beforeEach(() => {
  vi.clearAllMocks();
  setViewport(1000, 600);
  useSessionStore.setState(useSessionStore.getInitialState());
  useSessionStore.getState().setSession("session-1");
  useSessionStore.getState().setMessages([{id:"question",role:"user",complete:true,parts:[{kind:"text",text:"用户问题"}]}]);
  vi.mocked(exportTextFile).mockResolvedValue(null);
});

describe("TopbarActions", () => {
  it("任务操作 hover 提示通过全局 portal 渲染，不会被下层工具栏遮挡", async () => {
    render(
      <>
        <TopbarActions sessionId="session-1" title="测试会话" />
        <GlobalTooltip />
      </>,
    );
    const trigger = screen.getByRole("button", { name: "任务操作" });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue(rect(400, 12));

    fireEvent.pointerOver(trigger);

    const tooltip = await screen.findByRole("tooltip");
    expect(tooltip).toHaveTextContent("任务操作");
    expect(tooltip.parentElement).toBe(document.body);
    expect(tooltip).toHaveStyle({ position: "fixed", zIndex: "var(--echo-layer-tooltip)" });

    fireEvent.click(trigger);
    expect(screen.getByRole("menu", { name: "当前会话操作" })).toBeInTheDocument();
    expect(screen.queryByRole("tooltip")).toBeNull();
  });

  it("通过 body portal 以 fixed 浮层渲染，不受顶栏层叠上下文和裁剪影响", () => {
    render(<TopbarActions sessionId="session-1" title="测试会话" />);
    const trigger = screen.getByRole("button", { name: "任务操作" });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue(rect(400, 20));

    fireEvent.click(trigger);

    const menu = screen.getByRole("menu", { name: "当前会话操作" });
    expect(menu.parentElement).toBe(document.body);
    expect(menu.closest(".topbar-actions")).toBeNull();
    expect(menu).toHaveStyle({ position: "fixed", zIndex: "var(--echo-layer-popover)" });
    expect(menu).toHaveAttribute("data-placement", "bottom");
    expect(within(menu).getByRole("menuitem", { name: "导出整个会话…" })).toBeVisible();
  });

  it("程序化弹窗接管焦点时会关闭旧 portal 菜单", async () => {
    const view = render(<TopbarActions sessionId="session-1" title="测试会话" />);
    fireEvent.click(screen.getByRole("button", { name: "任务操作" }));
    expect(screen.getByRole("menu", { name: "当前会话操作" })).toBeInTheDocument();

    view.rerender(
      <>
        <TopbarActions sessionId="session-1" title="测试会话" />
        <ProgrammaticDialog />
      </>,
    );

    await waitFor(() => expect(screen.queryByRole("menu", { name: "当前会话操作" })).toBeNull());
  });

  it("视口底部空间不足时向上展开并保持在可视区域", () => {
    render(<TopbarActions sessionId="session-1" title="测试会话" />);
    const trigger = screen.getByRole("button", { name: "任务操作" });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue(rect(960, 560));

    fireEvent.click(trigger);

    const menu = screen.getByRole("menu");
    expect(menu).toHaveAttribute("data-placement", "top");
    expect(Number.parseFloat(menu.style.top)).toBeLessThan(560);
    expect(Number.parseFloat(menu.style.left)).toBeGreaterThanOrEqual(8);
  });

  it("从顶栏移除已有侧栏和导航入口的操作，只显示当前会话工具", () => {
    render(<TopbarActions sessionId="session-1" title="测试会话" onFind={vi.fn()} onRewind={vi.fn()} onFork={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "任务操作" }));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map(item => item.getAttribute("aria-label") ?? item.textContent)).toEqual([
      "查找当前会话", "回溯…", "分叉…", "导出整个会话…",
    ]);
  });

  it("没有内容和可用操作时不显示空菜单", () => {
    useSessionStore.getState().setMessages([]);
    render(<TopbarActions sessionId="session-1" title="空会话" />);
    expect(screen.queryByRole("button", {name:"任务操作"})).toBeNull();
  });

  it("运行中的空会话也可停止，停止请求等待期间不可重复操作", () => {
    useSessionStore.getState().setMessages([]);
    const onStop = vi.fn();
    const view = render(<TopbarActions sessionId="session-1" title="运行中" onStop={onStop} />);
    fireEvent.click(screen.getByRole("button", {name:"任务操作"}));
    fireEvent.click(screen.getByRole("menuitem", {name:"停止当前执行"}));
    expect(onStop).toHaveBeenCalledOnce();
    expect(screen.queryByRole("menu")).toBeNull();
    view.rerender(<TopbarActions sessionId="session-1" title="运行中" onStop={onStop} stopping />);
    fireEvent.click(screen.getByRole("button", {name:"任务操作"}));
    fireEvent.click(screen.getByRole("menuitem", {name:"正在停止…"}));
    expect(onStop).toHaveBeenCalledOnce();
    view.rerender(<TopbarActions sessionId="session-2" title="空闲" />);
    expect(screen.queryByRole("menu")).toBeNull();
    expect(screen.queryByRole("button", {name:"任务操作"})).toBeNull();
  });

  it("支持方向键、Home/End 和 Escape，关闭后恢复触发按钮焦点", async () => {
    render(<TopbarActions sessionId="session-1" title="测试会话" />);
    const trigger = screen.getByRole("button", { name: "任务操作" });
    trigger.focus();

    fireEvent.keyDown(trigger, { key: "ArrowUp" });
    const menu = screen.getByRole("menu");
    const items = within(menu).getAllByRole("menuitem");
    expect(items[items.length - 1]).toHaveFocus();

    fireEvent.keyDown(menu, { key: "Home" });
    expect(items[0]).toHaveFocus();
    fireEvent.keyDown(menu, { key: "ArrowUp" });
    expect(items[items.length - 1]).toHaveFocus();
    fireEvent.keyDown(menu, { key: "Escape" });

    expect(screen.queryByRole("menu")).toBeNull();
    await waitFor(() => expect(trigger).toHaveFocus());
  });
});

describe("session menu migration", () => {
  it("统一导出入口仍保存整个会话，默认排除过程；显式选择后包含过程", async () => {
    const { useSessionStore } = await import("@/stores/session-store");
    useSessionStore.getState().setSession("export-session");
    useSessionStore.getState().setMessages([
      { id: "u-export", role: "user", complete: true, parts: [{ kind: "text", text: "第一轮用户问题" }] },
      { id: "a-export", role: "assistant", complete: true, parts: [{ kind: "thought", text: "内部执行过程" }, { kind: "text", text: "第一轮正式回答" }] },
      { id: "u-export-2", role: "user", complete: true, parts: [{ kind: "text", text: "第二轮问题" }] },
    ]);
    vi.mocked(exportTextFile).mockResolvedValue(null);
    render(<TopbarActions sessionId="export-session" title="完整会话" />);
    const openShare = () => {
      fireEvent.click(screen.getByRole("button", { name: "任务操作" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "导出整个会话…" }));
    };
    openShare();
    const dialog = screen.getByRole("dialog", { name: "分享选项" });
    expect(within(dialog).getByText("本地分享，不会自动上传会话")).toBeVisible();
    fireEvent.click(within(dialog).getByRole("button", { name: "导出 Markdown" }));
    await waitFor(() => expect(exportTextFile).toHaveBeenLastCalledWith("完整会话.md", expect.stringContaining("第二轮问题"), "md"));
    expect(vi.mocked(exportTextFile).mock.lastCall?.[1]).toContain("第一轮正式回答");
    expect(vi.mocked(exportTextFile).mock.lastCall?.[1]).not.toContain("内部执行过程");
    fireEvent.click(within(dialog).getByRole("checkbox", { name: /包含执行过程/ }));
    fireEvent.click(within(dialog).getByRole("button", { name: "导出 Markdown" }));
    await waitFor(() => expect(vi.mocked(exportTextFile).mock.lastCall?.[1]).toContain("内部执行过程"));
  });

  it("查找和历史操作调用当前会话的回调，运行期间不可回溯或分叉", () => {
    const onFind = vi.fn(), onRewind = vi.fn(), onFork = vi.fn();
    const view = render(<TopbarActions sessionId="scope-a" title="A" onFind={onFind} onRewind={onRewind} onFork={onFork} />);
    const select = (name: string) => { fireEvent.click(screen.getByRole("button", { name: "任务操作" })); fireEvent.click(screen.getByRole("menuitem", { name })); };
    select("查找当前会话"); expect(onFind).toHaveBeenCalledOnce();
    select("回溯…"); expect(onRewind).toHaveBeenCalledOnce();
    select("分叉…"); expect(onFork).toHaveBeenCalledOnce();
    view.rerender(<TopbarActions sessionId="scope-a" title="A" onRewind={onRewind} onFork={onFork} historyDisabled />);
    fireEvent.click(screen.getByRole("button", { name: "任务操作" }));
    expect(screen.getByRole("menuitem", { name: "回溯…" })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "分叉…" })).toBeDisabled();
    view.rerender(<TopbarActions sessionId="scope-b" title="B" />);
    expect(screen.queryByRole("menu")).toBeNull();
  });
});
