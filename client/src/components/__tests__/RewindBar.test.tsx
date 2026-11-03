import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent, waitFor } from "@testing-library/react";
import { RewindBar } from "../RewindBar";

// The toolbar talks to EchoAgent over Tauri `invoke`, which doesn't exist under
// vitest — stub the three client calls it uses.
vi.mock("@/lib/agent-client", () => ({
  rewindPoints: vi.fn().mockResolvedValue([
    { promptIndex: 0, promptPreview: "first prompt", timestamp: "2026-01-01T00:00:00Z" },
  ]),
  rewindExecute: vi.fn().mockResolvedValue({ targetPromptIndex: 0 }),
  sessionFork: vi.fn().mockResolvedValue("forked-session-id-1234"),
}));

// Re-import the mocked module so we can assert call args on the stubs.
const { rewindExecute, rewindPoints, sessionFork } = await import("@/lib/agent-client");

describe("RewindBar wiring", () => {
  it("接续会话只能回溯自己的新内容，继承历史保持只读", async () => {
    render(<RewindBar sessionId="child" minPromptIndex={1} />);
    fireEvent.click(screen.getByRole("button",{name:/回溯/}));
    expect(await screen.findByRole("button",{name:"继承的历史（只读）"})).toBeDisabled();
    expect(rewindExecute).not.toHaveBeenCalled();
  });
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(rewindPoints).mockReset().mockResolvedValue([
      { promptIndex: 0, promptPreview: "first prompt", timestamp: "2026-01-01T00:00:00Z" },
    ]);
  });

  it.each([
    ["全量", "all"],
    ["仅对话", "conversation_only"],
    ["仅文件", "files_only"],
  ] as const)("使用%s模式回溯时发送 Runtime 规范值", async (modeLabel, wireMode) => {
    const onRewound = vi.fn();
    const onToast = vi.fn();
    render(
      <RewindBar sessionId="s1" onRewound={onRewound} onToast={onToast} />,
    );
    // 打开下拉触发加载回溯点。
    fireEvent.click(screen.getByRole("button", { name: /回溯/ }));
    // 模式按钮与时间线动作按钮共名片段，先精确选中模式。
    const modeBtn = await screen.findByRole("button", { name: modeLabel });
    fireEvent.click(modeBtn);
    // 时间线动作按钮的可访问名包含模式标签。
    const actionBtn = await screen.findByRole("button", {
      name: new RegExp(`回溯到此处.*${modeLabel}`),
    });
    fireEvent.click(actionBtn);
    expect(rewindExecute).not.toHaveBeenCalled();
    const confirmation = screen.getByRole("alertdialog", { name: `确认回溯（${modeLabel}）？` });
    expect(confirmation).toHaveTextContent("第 1 步执行前：first prompt");
    expect(confirmation).toHaveTextContent("撤销此步骤及后续");
    fireEvent.click(screen.getByRole("button", { name: "确认回溯" }));
    await waitFor(() => expect(onRewound).toHaveBeenCalledWith("s1"));
    expect(rewindExecute).toHaveBeenCalledWith("s1", 0, wireMode, true);
    expect(onToast).toHaveBeenCalled();
  });

  it("切换会话后不接收上一会话的迟到回溯点", async () => {
    let resolveFirst!: (value: Array<{ promptIndex: number; promptPreview: string }>) => void;
    vi.mocked(rewindPoints)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveFirst = resolve; }))
      .mockResolvedValueOnce([{ promptIndex: 1, promptPreview: "session B" }]);

    const { rerender } = render(<RewindBar sessionId="A" />);
    fireEvent.click(screen.getByRole("button", { name: /\u56de\u6eaf/ }));
    await waitFor(() => expect(rewindPoints).toHaveBeenCalledWith("A"));

    rerender(<RewindBar sessionId="B" />);
    await waitFor(() => expect(screen.getByRole("button", { name: /\u56de\u6eaf/ })).toHaveAttribute("aria-expanded", "false"));
    fireEvent.click(screen.getByRole("button", { name: /\u56de\u6eaf/ }));
    expect(await screen.findByText("session B")).toBeInTheDocument();

    await act(async () => resolveFirst([{ promptIndex: 0, promptPreview: "session A" }]));
    expect(screen.queryByText("session A")).toBeNull();
    expect(screen.getByText("session B")).toBeInTheDocument();
  });

  it.each(["rewind"] as const)("%s 确认前实时检查执行条件，不使用打开时的旧状态", async action => {
    let allowed = true;
    const onActionDone = vi.fn();
    render(<RewindBar sessionId="s1" action={action} canExecute={() => allowed} onActionDone={onActionDone} />);
    if (action === "rewind") fireEvent.click(await screen.findByRole("button", { name: /回溯到此处/ }));
    // The live session can change before React has committed another render.
    allowed = false;
    fireEvent.click(screen.getByRole("button", { name: "确认回溯" }));
    await waitFor(() => expect(onActionDone).toHaveBeenCalled());
    expect(rewindExecute).not.toHaveBeenCalled();
    expect(sessionFork).not.toHaveBeenCalled();
  });

  it("回溯点加载失败显示可重试错误", async () => {
    vi.mocked(rewindPoints).mockRejectedValueOnce(new Error("offline"));
    render(<RewindBar sessionId="s1" />);
    fireEvent.click(screen.getByRole("button", { name: /\u56de\u6eaf/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("offline");
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
  });

  it("时间线弹窗具有初始焦点、Tab 圈定、Escape 与触发器恢复", async () => {
    render(<RewindBar sessionId="s1" />);
    const trigger = screen.getByRole("button", { name: /回溯/ });
    trigger.focus();
    fireEvent.click(trigger);

    const selectedMode = screen.getByRole("button", { name: "全量" });
    expect(selectedMode).toHaveFocus();
    const action = await screen.findByRole("button", { name: /回溯到此处/ });
    action.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(screen.getByRole("button", { name: "关闭回溯时间线" })).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "回溯时间线" })).toBeNull();
    expect(trigger).toHaveFocus();
  });
});
