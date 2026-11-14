import { act, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ExecutionProcess } from "../ExecutionProcess";
import { ToolCallDetailBody } from "../ToolCallCard";
import { MessageItem } from "../MessageItem";
import { ThemeProvider } from "../ThemeProvider";
import type { MessagePart } from "@/stores/session-store";

describe("execution record control and result consistency", () => {
  const action = (id: string, status: "completed" | "failed" | "in_progress" = "completed", kind = "bash"): MessagePart => ({
    kind: "tool_call", toolCall: { toolCallId: id, kind, title: id, status, content: [] },
  });

  it("keeps every unfinished parallel operation visible before recent completed actions", () => {
    render(<ThemeProvider><ExecutionProcess active parts={[
      action("slow-a", "in_progress"), action("slow-b", "in_progress"),
      ...["one", "two", "three", "four"].map(id => action(id)),
    ]} /></ThemeProvider>);
    expect(screen.getByRole("button", { name: "bash slow-a 运行中" })).toBeVisible();
    expect(screen.getByRole("button", { name: "bash slow-b 运行中" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "bash one 完成" })).toBeNull();
    expect(screen.getByRole("list", { name: "最近执行进展" }).children).toHaveLength(5);
  });

  it("keeps a focused group unchanged through appended reads, completion, and release", () => {
    const view = (parts: MessagePart[], active = true) => <ThemeProvider><ExecutionProcess active={active} parts={parts} /></ThemeProvider>;
    const reads = [action("a", "completed", "read_file"), action("b", "completed", "read_file")];
    const { rerender } = render(view(reads));
    const group = screen.getByRole("button", { name: /文件读取 · 2 次操作/ });
    act(() => group.focus());
    const more = [...reads, action("c", "completed", "read_file"), ...["one", "two", "three"].map(id => action(id))];
    rerender(view(more));
    expect(group).toHaveFocus();
    expect(group).toHaveTextContent("文件读取 · 2 次操作");
    rerender(view(more, false));
    expect(group).toHaveFocus();
    act(() => group.blur());
    expect(screen.queryByRole("list", { name: "最近执行进展" })).toBeNull();
  });

  it.each([false, true])("preserves selected operation text while new records arrive (grouped: %s)", grouped => {
    const parts = grouped ? [action("a", "completed", "read_file"), action("b", "completed", "read_file")] : [action("selected")];
    const view = (updatedParts: MessagePart[], hasFinalAnswer = false) => <ThemeProvider><ExecutionProcess active parts={updatedParts} hasFinalAnswer={hasFinalAnswer} /></ThemeProvider>;
    const { rerender } = render(view(parts));
    const row = screen.getByRole("list", { name: "最近执行进展" }).firstElementChild!;
    const label = row.querySelector(grouped ? "button > span" : ".toolcall__title")!;
    const selection = document.getSelection()!;
    const range = document.createRange(); range.selectNodeContents(label); selection.addRange(range);
    fireEvent(document, new Event("selectionchange"));
    const selected = selection.toString();
    const more = [...parts, ...["one", "two", "three"].map(id => action(id))];
    rerender(view(more));
    expect(row).toBeInTheDocument();
    expect(selection.toString()).toBe(selected);
    rerender(view(more, true));
    expect(selection.toString()).toBe(selected);
    selection.removeAllRanges();
    fireEvent(document, new Event("selectionchange"));
    expect(screen.queryByRole("list", { name: "最近执行进展" })).toBeNull();
  });

  it.each(["等待你补充信息", "等待你批准操作", "等待你确认计划"])("keeps compact progress and keyboard focus while %s", waitingReason => {
    const view = (reason?: string) => <ThemeProvider><ExecutionProcess active parts={[action("focused", "in_progress")]} waitingReason={reason} /></ThemeProvider>;
    const { rerender } = render(view());
    const focused = screen.getByRole("button", { name: "bash focused 运行中" });
    act(() => focused.focus());
    rerender(view(waitingReason));
    expect(focused).toHaveFocus();
    expect(screen.getByRole("status")).toHaveTextContent(waitingReason);
    expect(screen.queryByRole("region", { name: "执行过程详情" })).toBeNull();
    rerender(view());
    expect(focused).toHaveFocus();
  });

  it("defaults to public progress and the latest three actions, with every original record one click away", () => {
    const onOpenTool = vi.fn();
    const parts: MessagePart[] = [{ kind: "text", phase: "commentary", stageTitle: "核对资料", text: "正在核对资料来源。" },
      { kind: "thought", text: "不应出现在默认进展中的分析原文" },
      ...["oldest", "second", "third", "latest"].map(id => action(id))];
    const { rerender } = render(<ThemeProvider><ExecutionProcess active parts={parts} onOpenTool={onOpenTool} /></ThemeProvider>);
    expect(screen.getByLabelText("当前阶段进展")).toHaveTextContent("正在核对资料来源。");
    expect(screen.getByRole("list", { name: "最近执行进展" }).children).toHaveLength(3);
    expect(screen.queryByRole("button", { name: "bash oldest 完成" })).toBeNull();
    expect(screen.queryByText("不应出现在默认进展中的分析原文")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "bash latest 完成" }));
    expect(onOpenTool).toHaveBeenCalledWith(expect.objectContaining({ toolCallId: "latest" }));
    fireEvent.click(screen.getByRole("button", { name: / · 展开过程$/ }));
    expect(screen.getByRole("button", { name: "bash oldest 完成" })).toBeVisible();
    fireEvent.click(screen.getByText("分析详情"));
    expect(screen.getByText("不应出现在默认进展中的分析原文")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: / · 收起过程$/ }));
    rerender(<ThemeProvider><ExecutionProcess active={false} parts={parts} stopReason="end_turn" /></ThemeProvider>);
    expect(screen.queryByRole("list", { name: "最近执行进展" })).toBeNull();
    expect(screen.queryByLabelText("当前阶段进展")).toBeNull();
  });

  it("opens grouped reads as individual records, while preserving failure visibility outside recent actions", () => {
    const reads = [action("a", "completed", "read_file"), action("b", "completed", "read_file")];
    const { rerender } = render(<ThemeProvider><ExecutionProcess active parts={reads} /></ThemeProvider>);
    fireEvent.click(screen.getByRole("button", { name: /文件读取 · 2 次操作/ }));
    expect(screen.getByRole("region", { name: "执行过程详情" }).querySelectorAll(".toolcall")).toHaveLength(2);
    fireEvent.click(screen.getByRole("button", { name: / · 收起过程$/ }));
    rerender(<ThemeProvider><ExecutionProcess active parts={[action("failed", "failed"), ...["a", "b", "c"].map(id => action(id))]} /></ThemeProvider>);
    expect(screen.queryByRole("button", { name: "bash failed 失败" })).toBeNull();
    expect(screen.getByRole("note")).toHaveTextContent("1 项操作失败，执行仍在继续");
    fireEvent.click(screen.getByRole("button", { name: "查看记录" }));
    expect(screen.getByRole("button", { name: "bash failed 失败" })).toBeVisible();
  });

  it("retains a focused live action through new records and completion, without falsely leaving it running", () => {
    const view = (parts: MessagePart[], active = true) => <ThemeProvider><ExecutionProcess active={active} parts={parts} /></ThemeProvider>;
    const { rerender } = render(view([action("focused", "in_progress", "read_file")]));
    const focused = screen.getByRole("button", { name: "read_file focused 运行中" });
    act(() => focused.focus());
    const parts = [action("focused", "completed", "read_file"), ...["a", "b", "c", "d"].map(id => action(id, "completed", "read_file"))];
    rerender(view(parts));
    expect(focused).toHaveFocus();
    expect(focused).toHaveAttribute("aria-label", "read_file focused 完成");
    rerender(view([action("focused", "in_progress", "read_file")], false));
    expect(focused).toHaveFocus();
    expect(focused).toHaveAttribute("aria-label", "read_file focused 结果待确认");
    expect(focused).not.toHaveClass("toolcall--run");
    act(() => focused.blur());
    expect(screen.queryByRole("list", { name: "最近执行进展" })).toBeNull();
  });

  it("keeps selected public progress readable when the answer starts and retires it after selection clears", () => {
    const parts: MessagePart[] = [{ kind: "text", phase: "commentary", stageTitle: "核对资料", text: "正在比较资料来源。" }];
    const view = (hasFinalAnswer: boolean, updatedParts = parts) => <ThemeProvider><ExecutionProcess active hasFinalAnswer={hasFinalAnswer} parts={updatedParts} /></ThemeProvider>;
    const { rerender } = render(view(false));
    const text = screen.getByLabelText("当前阶段进展").querySelector("p")!;
    const selection = document.getSelection()!;
    const range = document.createRange(); range.selectNodeContents(text); selection.addRange(range);
    fireEvent(document, new Event("selectionchange"));
    rerender(view(false, [{ kind: "text", phase: "commentary", stageTitle: "核对资料", text: "正在整理最新资料。" }]));
    expect(text).toHaveTextContent("正在比较资料来源。");
    expect(selection.toString()).toBe("正在比较资料来源。");
    rerender(view(true));
    expect(text).toBeVisible();
    expect(selection.toString()).toBe("正在比较资料来源。");
    selection.removeAllRanges();
    fireEvent(document, new Event("selectionchange"));
    expect(screen.queryByLabelText("当前阶段进展")).toBeNull();
  });

  it.each(["completed", "failed"] as const)("does not describe an unknown desktop outcome as confirmed failure: %s", status => {
    render(<ThemeProvider><ExecutionProcess active parts={[{ kind: "tool_call", toolCall: {
      toolCallId: "unknown-click", kind: "other", toolName: "computer_click", title: "点击按钮", status,
      content: [{ type: "text", text: JSON.stringify({ status: "result_unknown" }) }],
    } }]} /></ThemeProvider>);
    expect(screen.getByRole("note")).toHaveTextContent("1 项操作结果待确认");
    expect(screen.getByRole("note")).not.toHaveTextContent("操作失败");
    expect(screen.getByRole("button", { name: /结果待核验$/ })).toBeVisible();
    expect(screen.getByText("结果待核验")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: / · 展开过程$/ }));
    expect(screen.getByText("1 项结果待确认")).toBeVisible();
    expect(screen.queryByText(/1 项失败/)).toBeNull();
  });

  it("shows a later verified task consistently in the overview and individual historical records", () => {
    const parts: MessagePart[] = [
      { kind: "tool_call", stageTitle: "执行操作", toolCall: { toolCallId: "dispatch", kind: "other", toolName: "computer_click", title: "点击按钮", status: "completed",
        content: [{ type: "text", text: '{"status":"dispatched_unverified","receiptId":"action-1"}' }] } },
      { kind: "tool_call", stageTitle: "核验成果", toolCall: { toolCallId: "verified", kind: "other", toolName: "automation_complete", title: "完成核验", status: "completed",
        content: [{ type: "text", text: '{"task":{"phase":"completed","receipts":[{"id":"action-1"}]}}' }] } },
    ];
    render(<ThemeProvider><ExecutionProcess parts={parts} active={false} stopReason="end_turn" /></ThemeProvider>);
    expect(screen.getByText("本次处理已结束")).toBeVisible();
    expect(screen.getByText("已完成 2 项操作")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: /查看过程$/ }));
    expect(screen.getByRole("button", { name: /点击按钮 后续任务已核验$/ })).toHaveClass("toolcall--ok");
    expect(screen.queryByText("结果待确认")).toBeNull();
  });

  it("follows streamed records until the reader scrolls up, then preserves the view when the answer starts", () => {
    const onReview = vi.fn();
    const view = (length: number, hasFinalAnswer = false) => <ThemeProvider><ExecutionProcess
      active autoExpand hasFinalAnswer={hasFinalAnswer} onReview={onReview}
      parts={Array.from({ length }, (_, index) => ({ kind: "text", phase: "commentary", text: `进展 ${index}` }))} /></ThemeProvider>;
    const { rerender } = render(view(8));
    const body = screen.getByRole("region", { name: "执行过程详情" });
    let height = 400;
    Object.defineProperties(body, { clientHeight: { value: 150 }, scrollHeight: { get: () => height } });
    rerender(view(9));
    expect(body.scrollTop).toBe(250);

    fireEvent.keyDown(body, { key: "ArrowUp" });
    expect(body.scrollTop).toBeLessThan(250);
    expect(onReview).toHaveBeenCalledOnce();
    const position = body.scrollTop;
    height = 500;
    rerender(view(10, true));
    expect(body).toBeVisible();
    expect(body.scrollTop).toBe(position);

    fireEvent.click(screen.getByRole("button", { name: "回到最新进展" }));
    expect(body).toHaveFocus();
    expect(body.scrollTop).toBe(350);
    expect(screen.queryByRole("button", { name: "回到最新进展" })).toBeNull();
    height = 600;
    rerender(view(11, true));
    expect(body.scrollTop).toBe(450);
  });

  it.each(["keyboard focus", "text selection"])("preserves %s at the latest record when the answer starts", interaction => {
    const parts: MessagePart[] = [
      { kind: "text", phase: "commentary", text: "进度说明。" },
      { kind: "tool_call", toolCall: { toolCallId: "review-focus", kind: "read_file", title: "检查资料", status: "completed", content: [] } },
    ];
    const view = (hasFinalAnswer: boolean) => <ThemeProvider><ExecutionProcess
      active autoExpand hasFinalAnswer={hasFinalAnswer} parts={[...parts]} /></ThemeProvider>;
    const { rerender } = render(view(false));
    const body = screen.getByRole("region", { name: "执行过程详情" });
    const tool = screen.getByRole("button", { name: "read_file 检查资料 完成" });
    const heading = body.querySelector("h4")!;
    const selection = document.getSelection()!;
    if (interaction === "keyboard focus") {
      tool.focus();
    } else {
      const range = document.createRange();
      range.selectNode(heading.lastChild!);
      selection.addRange(range);
    }

    rerender(view(true));
    expect(body).toBeVisible();
    if (interaction === "keyboard focus") expect(tool).toHaveFocus();
    else expect(selection.toString()).toBe("进度说明。");
    selection.removeAllRanges();
  });

  it.each(["button", "ArrowDown"])("returns keyboard focus to the header if following resumes via %s", action => {
    const view = (hasFinalAnswer: boolean) => <ThemeProvider><ExecutionProcess
      active autoExpand hasFinalAnswer={hasFinalAnswer} parts={[{ kind: "thought", text: "检查资料。" }]} /></ThemeProvider>;
    const { rerender } = render(view(false));
    const body = screen.getByRole("region", { name: "执行过程详情" });
    Object.defineProperties(body, { clientHeight: { value: 150 }, scrollHeight: { value: 400 } });
    rerender(view(false));
    body.focus();
    fireEvent.keyDown(body, { key: "ArrowUp" });
    if (action === "button") fireEvent.click(screen.getByRole("button", { name: "回到最新进展" }));
    else fireEvent.keyDown(body, { key: action });
    expect(body).toHaveFocus();

    rerender(view(true));
    expect(screen.queryByRole("region", { name: "执行过程详情" })).toBeNull();
    expect(screen.getByRole("button", { name: /展开过程$/ })).toHaveFocus();
  });

  it("restores an execution's expanded analysis and scroll position after remounting", () => {
    const analysis = "已保留执行记录。".repeat(100) + "完整记录末尾";
    const view = (sessionId: string, active = true) => <ThemeProvider><ExecutionProcess
      key={sessionId} sessionId={sessionId} messageId="review-remount"
      active={active} autoExpand parts={[{ kind: "thought", text: analysis }]} /></ThemeProvider>;
    const { rerender } = render(view("review-first"));
    const body = screen.getByRole("region", { name: "执行过程详情" });
    fireEvent.click(screen.getByText("分析详情"));
    Object.defineProperties(body, { clientHeight: { value: 150 }, scrollHeight: { value: 400 } });
    body.scrollTop = 120;
    fireEvent.scroll(body);

    rerender(view("review-second"));
    expect(screen.getByRole("region", { name: "执行过程详情" }).scrollTop).toBe(0);
    rerender(view("review-first", false));
    expect(screen.getByRole("region", { name: "执行过程详情" }).scrollTop).toBe(120);
    expect(screen.getByText(/完整记录末尾/)).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "回到最新进展" }));
    rerender(view("review-second"));
    rerender(view("review-first", false));
    expect(screen.queryByRole("region", { name: "执行过程详情" })).toBeNull();
  });

  it.each(["等待你补充信息", "等待你批准操作", "等待你确认计划"])("%s 时保留过程，处理后随正文收起", waitingReason => {
    const parts = [{ kind: "thought" as const, text: "检查输入。" }];
    const view = (reason?: string) => <ThemeProvider><ExecutionProcess
      active autoExpand hasFinalAnswer parts={parts} waitingReason={reason} /></ThemeProvider>;
    const { rerender } = render(view());
    expect(screen.queryByRole("region", { name: "执行过程详情" })).toBeNull();
    rerender(view(waitingReason));
    expect(screen.getByRole("region", { name: "执行过程详情" })).toBeVisible();
    rerender(view());
    expect(screen.queryByRole("region", { name: "执行过程详情" })).toBeNull();
  });

  it.each([
    [true, undefined, "展开过程"],
    [false, "end_turn", "查看过程"],
    [false, "cancelled", "查看过程"],
    [false, "error", "查看过程"],
  ] as const)("运行中与终态共用一个记录入口，键盘收起后保留焦点：%s / %s", async (active, stopReason, label) => {
    const user = userEvent.setup();
    render(<ThemeProvider><ExecutionProcess active={active} stopReason={stopReason}
      parts={[{ kind: "thought", text: "保留完整分析过程。" }]} /></ThemeProvider>);
    const entry = screen.getByRole("button", { name: new RegExp(` · ${label}$`) });
    expect(entry).toHaveTextContent(label);
    expect(screen.queryByRole("button", { name: "收起过程" })).toBeNull();
    await user.click(entry);
    const body = screen.getByRole("region", { name: "执行过程详情" });
    expect(entry).toHaveTextContent("收起过程");
    expect(screen.getAllByRole("button", { name: /收起过程$/ })).toHaveLength(1);
    expect(entry).toHaveAttribute("aria-controls", body.id);
    expect(entry).toHaveAttribute("aria-expanded", "true");
    entry.focus();
    await user.keyboard("{Enter}");
    expect(screen.queryByRole("region", { name: "执行过程详情" })).toBeNull();
    expect(entry).toHaveFocus();
    expect(entry).toHaveTextContent(label);
  });

  it("手动展开后完成不打断阅读，收起选择在重新挂载后保留", () => {
    const parts = [{ kind: "thought" as const, text: "检查并保留过程。" }];
    const view = (active: boolean) => <ThemeProvider><ExecutionProcess active={active} parts={parts}
      sessionId="footer-collapse-session" messageId="footer-collapse-message" /></ThemeProvider>;
    const { rerender, unmount } = render(view(true));
    fireEvent.click(screen.getByRole("button", { name: / · 展开过程$/ }));
    rerender(view(false));
    expect(screen.getByRole("region", { name: "执行过程详情" })).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: / · 收起过程$/ }));
    unmount();
    render(view(false));
    expect(screen.getByRole("button", { name: / · 查看过程$/ })).toHaveAttribute("aria-expanded", "false");
  });

  it("search reveals a record temporarily, while explicit collapse and reopen remain usable", () => {
    const parts = [{ kind: "text" as const, text: "已经确认票价和开放时间。" }];
    const view = (findQuery?: string) => <ThemeProvider><ExecutionProcess parts={parts} active={false} findQuery={findQuery} /></ThemeProvider>;
    const { rerender } = render(view());
    const entry = screen.getByRole("button", { name: / · (展开|查看|收起)过程$/ });
    rerender(view("票价"));
    expect(entry).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(entry);
    expect(entry).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(entry);
    expect(entry).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(entry);
    rerender(view("开放时间"));
    expect(entry).toHaveAttribute("aria-expanded", "true");
    rerender(view());
    expect(entry).toHaveAttribute("aria-expanded", "false");
  });

  it("停止后没有回执的操作显示待确认，详情也不会继续声称运行中", () => {
    const tool = { toolCallId: "pending-result", kind: "read", title: "读取资料", status: "in_progress" as const, content: [] };
    const { container } = render(<ThemeProvider>
      <ExecutionProcess active={false} autoExpand parts={[{ kind: "tool_call", toolCall: tool }]} stopReason="cancelled" />
      <ToolCallDetailBody tc={tool} executionEnded />
    </ThemeProvider>);
    fireEvent.click(screen.getByRole("button", { name: / · (展开|查看|收起)过程$/ }));
    expect(screen.getByRole("button", { name: "read 读取资料 结果待确认" })).toBeVisible();
    expect(container.querySelector(".toolcall--run, .toolcall__status-mark--in_progress")).toBeNull();
    expect(container.querySelector(".execution-process__metrics")).toHaveTextContent("1 项结果待确认");
    expect(container.querySelector(".execution-process__metrics")).not.toHaveTextContent("进行中");
    expect(container.querySelector(".tool-detail__status")).toHaveTextContent("结果待确认");
    expect(tool.status).toBe("in_progress");
  });

  it.each([
    ["empty", "本轮没有生成回复", "attention"],
    ["quota", "模型额度已用尽", "failed"],
  ])("%s result uses the same meaning in the overview and recovery notice", (kind, title, state) => {
    const { container } = render(<ThemeProvider><MessageItem streaming={false} message={{
      id: kind, role: "assistant", complete: true, stopReason: kind === "empty" ? "end_turn" : "error",
      parts: [{ kind: "thought", text: "分析片段" }], agentResult: kind === "quota" ? "quota exhausted" : undefined,
    }} /></ThemeProvider>);
    expect(container.querySelector(".execution-process__title")).toHaveTextContent(title);
    expect(container.querySelector(`.execution-process--${state}`)).not.toBeNull();
    expect(screen.getByRole("alert")).toHaveTextContent(title);
    expect(screen.queryByText("本次处理已结束")).toBeNull();
  });
});
