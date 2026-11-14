import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ExecutionProcess } from "../ExecutionProcess";
import { MessageItem } from "../MessageItem";
import { ThemeProvider } from "../ThemeProvider";
import type { AutomationStatus } from "@/lib/automation-client";
import type { MessagePart } from "@/stores/session-store";

const thought: MessagePart = { kind: "thought", text: "核对原始资料，保留结论依据。", streamId: "one" };
const tool: MessagePart = { kind: "tool_call", streamId: "one", toolCall: {
  toolCallId: "read", kind: "read_file", title: "Read /project/资料.md", status: "in_progress", content: [],
} };
const nativeTool = (mode: "browser_use" | "computer_use"): MessagePart => ({ kind: "tool_call", streamId: "one", toolCall: {
  toolCallId: "native", kind: "other", toolName: mode === "browser_use" ? "browser_snapshot" : "computer_screenshot",
  title: "读取应用界面", status: "in_progress", content: [],
} });
const status = (mode: "browser_use" | "computer_use"): AutomationStatus => ({
  sessionId: "task", mode, paused: false, allowPrivateNetwork: false,
  browser: { available: true }, computer: { available: true, platform: "macOS", screenCapture: true, inputControl: true },
  browserRunning: false, browserHasData: false,
  task: { phase: "running", reason: null, scope: "核对资料", authorized: true, generation: 1 },
});

describe("unified compact process presentation", () => {
  it("preserves visible reasoning and text selection when tools arrive after the reader opened it", () => {
    const view = (parts: MessagePart[]) => <ThemeProvider><ExecutionProcess active parts={parts} /></ThemeProvider>;
    const { rerender } = render(view([thought]));
    fireEvent.click(screen.getByRole("button", { name: /展开过程$/ }));
    const text = screen.getByText(thought.text);
    const selection = document.getSelection()!;
    const range = document.createRange(); range.selectNodeContents(text); selection.addRange(range);
    rerender(view([thought, tool]));
    expect(screen.getByText(thought.text)).toBe(text);
    expect(selection.toString()).toBe(thought.text);
    expect(screen.getByText("分析详情").closest("details")).toHaveAttribute("open");
    expect(screen.getByRole("button", { name: "read_file /project/资料.md 运行中" })).toBeVisible();
    selection.removeAllRanges();
  });

  it("omits a single repeated plan heading while retaining multiple real plan stages", () => {
    const view = (parts: MessagePart[]) => <ThemeProvider><ExecutionProcess active parts={parts} /></ThemeProvider>;
    const { container, rerender } = render(view([{ ...thought, stageTitle: "核对资料" }, { ...tool, stageTitle: "核对资料" }]));
    fireEvent.click(screen.getByRole("button", { name: /展开过程$/ }));
    expect(screen.getAllByText("核对资料")).toHaveLength(1);
    expect(container.querySelector(".execution-process__stage-heading")).toBeNull();
    rerender(view([{ ...thought, stageTitle: "核对资料" }, { ...tool, stageTitle: "核验结果" }]));
    expect(container.querySelectorAll(".execution-process__stage-heading")).toHaveLength(2);
  });

  it("keeps the unique plan name when completion or native context replaces the overview title", () => {
    const parts: MessagePart[] = [{ ...thought, stageTitle: "核对资料" }];
    const view = (active: boolean, automationStatus?: AutomationStatus) => <ThemeProvider><ExecutionProcess
      active={active} parts={parts} automationStatus={automationStatus} stopReason={active ? undefined : "end_turn"} /></ThemeProvider>;
    const { container, rerender } = render(view(true));
    fireEvent.click(screen.getByRole("button", { name: /展开过程$/ }));
    expect(screen.getAllByText("核对资料")).toHaveLength(1);
    rerender(view(false));
    expect(screen.getByRole("heading", { name: "核对资料" })).toBeVisible();
    expect(container.querySelector(".execution-process__title")).toHaveTextContent("本次处理已结束");
    rerender(view(true, status("computer_use")));
    fireEvent.click(screen.getByRole("button", { name: /展开过程$/ }));
    expect(screen.getByRole("heading", { name: "核对资料" })).toBeVisible();
    expect(container.querySelector(".execution-process__title")).toHaveTextContent("正在处理应用中的任务");
  });

  it("full records distinguish same-name files while compact progress stays concise", () => {
    const parts: MessagePart[] = ["frontend", "backend"].map(directory => ({ kind: "tool_call", toolCall: {
      toolCallId: directory, kind: "read_file", title: `Read /project/${directory}/config.ts`, status: "completed", content: [],
      rawInput: { path: `/project/${directory}/config.ts` },
    } }));
    render(<ThemeProvider><ExecutionProcess active={false} parts={parts} cwd="/project" /></ThemeProvider>);
    fireEvent.click(screen.getByRole("button", { name: /查看过程$/ }));
    for (const directory of ["frontend", "backend"]) {
      const row = screen.getByRole("button", { name: `read_file ${directory}/config.ts 完成` });
      expect(row).toBeVisible();
      expect(row).toHaveAttribute("title", expect.stringContaining(`${directory}/config.ts`));
    }
  });

  it.each(["browser_use", "computer_use"] as const)("keeps %s running between model turns and preserves explicit waiting", mode => {
    const automation = status(mode);
    const view = (waitingReason?: string) => <ThemeProvider><MessageItem
      message={{ id: "native-between-turns", role: "assistant", complete: true, stopReason: "end_turn", parts: [nativeTool(mode)] }}
      streaming={false} automationStatus={automation} waitingReason={waitingReason} /></ThemeProvider>;
    const { container, rerender } = render(view());
    expect(container.querySelector(".execution-process--running")).not.toBeNull();
    expect(screen.queryByText("本轮没有生成回复")).toBeNull();
    expect(screen.getByRole("button", { name: "other 读取应用界面 运行中" })).toBeVisible();
    rerender(view("等待你批准操作"));
    expect(screen.getByRole("status")).toHaveTextContent("等待你批准操作");
    expect(container.querySelector(".execution-process--waiting")).not.toBeNull();
    expect(screen.queryByText("正在处理应用中的任务")).toBeNull();
  });

  it.each(["browser_use", "computer_use"] as const)("shows pending verification consistently for %s without claiming completion", mode => {
    const automation = status(mode);
    automation.task = { ...automation.task!, phase: "result_unknown", authorized: false };
    const { container } = render(<ThemeProvider><ExecutionProcess parts={[nativeTool(mode)]} active={false} automationStatus={automation} /></ThemeProvider>);
    expect(container.querySelector(".execution-process--attention")).not.toBeNull();
    expect(screen.getByRole("status")).toHaveTextContent("操作结果尚未确认");
    fireEvent.click(screen.getByRole("button", { name: /查看过程$/ }));
    expect(screen.getByRole("button", { name: "other 读取应用界面 结果待确认" })).toBeVisible();
    expect(container.querySelector(".toolcall--ok, .toolcall--run")).toBeNull();
  });

  it("uses the same line icon and purpose in compact and expanded task records", () => {
    const task: MessagePart = { kind: "tool_call", toolCall: { toolCallId: "child", kind: "spawn_subagent",
      title: "派发子代理 (general-purpose)", status: "in_progress", content: [], rawInput: { description: "核对来源" } } };
    const { container } = render(<ThemeProvider><ExecutionProcess active parts={[task]} /></ThemeProvider>);
    const compactIcon = container.querySelector(".toolcall__activity-icon svg")?.getAttribute("class");
    expect(screen.getByText("核对来源")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: /展开过程$/ }));
    expect(container.querySelector(".toolcall__activity-icon svg")?.getAttribute("class")).toBe(compactIcon);
    expect(screen.getByText("核对来源")).toBeVisible();
    expect(screen.queryByText(/general-purpose/)).toBeNull();
    expect(container.querySelector(".toolcall__kind")).toBeNull();
  });

  it.each(["paused", "stopped", "failed"] as const)("native %s retires only native spinners while other work continues", phase => {
    const automation = status("computer_use");
    automation.task = { ...automation.task!, phase, authorized: false };
    const { container, rerender } = render(<ThemeProvider><ExecutionProcess parts={[nativeTool("computer_use"), tool]} active automationStatus={automation} /></ThemeProvider>);
    expect(container.querySelector(".execution-process--running")).not.toBeNull();
    expect(screen.getByText(/本轮其他处理仍在继续/)).toBeVisible();
    expect(screen.getByRole("button", { name: "read_file 资料.md 运行中" })).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: /展开过程$/ }));
    expect(screen.getByRole("button", { name: "other 读取应用界面 结果待确认" })).toBeVisible();
    expect(screen.getByRole("button", { name: "read_file /project/资料.md 运行中" })).toBeVisible();
    expect(container.querySelector(".execution-process__metrics")).toHaveTextContent("1 项进行中 · 1 项结果待确认");
    rerender(<ThemeProvider><ExecutionProcess parts={[nativeTool("computer_use"), tool]} active={false} automationStatus={automation} /></ThemeProvider>);
    expect(container.querySelector(`.execution-process--${phase === "failed" ? "failed" : "stopped"}`)).not.toBeNull();
    expect(container.querySelector(".toolcall--run")).toBeNull();
    expect(screen.queryByText(/本轮其他处理仍在继续/)).toBeNull();
  });

  it.each([undefined, "final_answer"] as const)("native interruption does not hide the trailing %s answer or its generation feedback", phase => {
    const automation = status("computer_use");
    automation.task = { ...automation.task!, phase: "paused", authorized: false };
    const { container } = render(<ThemeProvider><MessageItem streaming automationStatus={automation} message={{
      id: "continuing-answer", role: "assistant", complete: false, parts: [nativeTool("computer_use"),
        { kind: "text", text: "应用操作已暂停，以下是已整理的资料。", streamId: "reply", phase }],
    }} /></ThemeProvider>);
    expect(screen.getByText("应用操作已暂停，以下是已整理的资料。")).toBeVisible();
    expect(screen.getByText("正在生成回复")).toBeVisible();
    expect(container.querySelector(".execution-process--running")).not.toBeNull();
    expect(container.querySelector(".execution-process__title")).toHaveTextContent("处理过程");
    expect(container.querySelector(".execution-process__live")).toBeNull();
  });

  it("keeps native user waiting distinct from an idle operation timeout", () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000);
    try {
      const automation = status("browser_use");
      automation.task = { ...automation.task!, phase: "waiting_user" };
      const { container, unmount } = render(<ThemeProvider><ExecutionProcess parts={[tool]} active
        automationStatus={automation} startedAt={1_000} lastActivityAt={1_000} /></ThemeProvider>);
      act(() => vi.advanceTimersByTime(45_000));
      expect(screen.getByRole("status")).toHaveTextContent("等待你确认操作");
      expect(container.querySelector(".execution-process__waiting-note")).toBeNull();
      unmount();
    } finally { vi.useRealTimers(); }
  });
});
