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
  rewindPreview: vi.fn().mockResolvedValue({ revision: "revision", cleanFiles: ["ready.txt"], conflicts: [{ path: "user.txt", conflictType: "modified_externally" }] }),
  rewindUndoPreview: vi.fn(),
  rewindUndoExecute: vi.fn(),
  sessionFork: vi.fn().mockResolvedValue("forked-session-id-1234"),
}));

// Re-import the mocked module so we can assert call args on the stubs.
const { rewindExecute, rewindPoints, rewindPreview, rewindUndoExecute, rewindUndoPreview, sessionFork } = await import("@/lib/agent-client");

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
    const confirmation = await screen.findByRole("alertdialog", { name: `确认回溯（${modeLabel}）？` });
    expect(confirmation).toHaveTextContent("第 1 步执行前：first prompt");
    if (wireMode !== "conversation_only") {
      expect(rewindPreview).toHaveBeenCalledWith("s1", 0, wireMode);
      expect(confirmation).toHaveTextContent("ready.txt");
      expect(confirmation).toHaveTextContent("user.txt");
      expect(confirmation).toHaveTextContent("将保留当前状态");
    }
    fireEvent.click(screen.getByRole("button", { name: "确认回溯" }));
    await waitFor(() => expect(onRewound).toHaveBeenCalledWith("s1"));
    const preview = wireMode === "conversation_only" ? [] : ["revision"];
    expect(rewindExecute).toHaveBeenCalledWith("s1", 0, wireMode, true, ...preview);
    expect(onToast).toHaveBeenCalled();
  });

  it("从现有时间线预览并撤销上次文件恢复", async () => {
    const onRewound = vi.fn();
    vi.mocked(rewindPoints).mockResolvedValueOnce([]);
    vi.mocked(rewindUndoPreview).mockResolvedValueOnce({
      revision: "undo-revision",
      cleanFiles: ["report.txt"],
      conflicts: [{ path: "uncertain.txt", conflictType: "rewind_interrupted" }],
    });
    vi.mocked(rewindUndoExecute).mockResolvedValueOnce({ targetPromptIndex: 0, revertedFiles: ["report.txt"] });
    render(<RewindBar sessionId="s1" action="rewind" onRewound={onRewound} />);
    await screen.findByText("还没有回溯点，走几步再回头看。");
    fireEvent.click(screen.getByRole("button", { name: "撤销上次文件恢复" }));
    const confirmation = await screen.findByRole("alertdialog", { name: "撤销上次文件恢复？" });
    expect(confirmation).toHaveTextContent("report.txt");
    expect(confirmation).toHaveTextContent("上次写入中断，结果待核对");
    expect(rewindUndoExecute).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "确认撤销" }));
    await waitFor(() => expect(rewindUndoExecute).toHaveBeenCalledWith("s1", "undo-revision"));
    expect(onRewound).toHaveBeenCalledWith("s1");
    expect(rewindPoints).toHaveBeenCalledTimes(1);
  });

  it("回溯后保留并发修改的安全副本位置供用户核对", async () => {
    vi.mocked(rewindExecute).mockResolvedValueOnce({
      targetPromptIndex: 0,
      revertedFiles: [],
      conflicts: [{
        path: "ready.txt", conflictType: "modified_externally", recoveryPath: "/private/rewind-copy/ready.txt",
      }],
    });
    render(<RewindBar sessionId="s1" action="rewind" />);
    fireEvent.click(await screen.findByRole("button", { name: "仅文件" }));
    fireEvent.click(await screen.findByRole("button", { name: /回溯到此处/ }));
    fireEvent.click(await screen.findByRole("button", { name: "确认回溯" }));
    expect(await screen.findByRole("status")).toHaveTextContent("/private/rewind-copy/ready.txt");
    expect(screen.getByRole("dialog", { name: "回溯时间线" })).toBeInTheDocument();
  });

  it("仅文件模式在没有可恢复文件时只显示核对结果", async () => {
    vi.mocked(rewindPreview).mockResolvedValueOnce({ revision: "revision", cleanFiles: [], conflicts: [
      { path: "kept.txt", conflictType: "modified_externally" },
    ] });
    render(<RewindBar sessionId="s1" action="rewind" />);
    fireEvent.click(await screen.findByRole("button", { name: "仅文件" }));
    fireEvent.click(await screen.findByRole("button", { name: /回溯到此处/ }));
    const notice = await screen.findByRole("dialog", { name: "确认回溯（仅文件）？" });
    expect(notice).toHaveTextContent("kept.txt");
    fireEvent.click(screen.getByRole("button", { name: "知道了" }));
    expect(rewindExecute).not.toHaveBeenCalled();
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
    const confirmation = await screen.findByRole("button", { name: "确认回溯" });
    // The live session can change before React has committed another render.
    allowed = false;
    fireEvent.click(confirmation);
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
