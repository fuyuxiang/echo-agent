import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ExecutionProcess } from "../ExecutionProcess";
import { ToolCallDetailBody } from "../ToolCallCard";
import { MessageItem } from "../MessageItem";
import { ThemeProvider } from "../ThemeProvider";

describe("execution record control and result consistency", () => {
  it("search reveals a record temporarily, while explicit collapse and reopen remain usable", () => {
    const parts = [{ kind: "text" as const, text: "已经确认票价和开放时间。" }];
    const view = (findQuery?: string) => <ThemeProvider><ExecutionProcess parts={parts} active={false} findQuery={findQuery} /></ThemeProvider>;
    const { rerender } = render(view());
    const entry = screen.getByRole("button", { name: /执行记录/ });
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
    fireEvent.click(screen.getByRole("button", { name: /执行记录/ }));
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
    expect(screen.queryByText("本轮已完成")).toBeNull();
  });
});
