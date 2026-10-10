import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MessageItem } from "../MessageItem";
import { ExecutionProcess } from "../ExecutionProcess";
import { ThemeProvider } from "../ThemeProvider";
import type { ChatMessage, ToolCallView } from "@/stores/session-store";
import { useKnowledgeStore } from "@/stores/knowledge-store";

describe("assistant execution process", () => {
  const renderMessage = (onOpenTool?: (tool: ToolCallView) => void) => render(
    <ThemeProvider>
      <MessageItem
        message={message}
        streaming={false}
        onOpenTool={onOpenTool}
      />
    </ThemeProvider>,
  );

  const message: ChatMessage = {
    id: "assistant-1",
    role: "assistant",
    complete: true,
    startedAt: 1_000,
    completedAt: 9_000,
    parts: [
      { kind: "text", text: "我先检查代码。" },
      { kind: "thought", text: "需要从客户端交互状态入手。" },
      {
        kind: "tool_call",
        toolCall: {
          toolCallId: "tool-1",
          title: "Edit src/App.tsx",
          kind: "edit_file",
          status: "completed",
          content: [],
        },
      },
      { kind: "text", text: "最终修复结果" },
    ],
  };

  it("完成后默认收起过程并始终展示最终答复", () => {
    renderMessage();

    expect(screen.queryByText("我先检查代码。")).toBeNull();
    expect(screen.getByText("最终修复结果")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /已完成执行过程/ })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    expect(screen.queryByText("需要从客户端交互状态入手。")).toBeNull();
  });

  it("没有最终答复的历史工具过程也默认收起", () => {
    render(
      <ThemeProvider>
        <MessageItem
          message={{
            id: "tool-only-history",
            role: "assistant",
            complete: true,
            parts: [message.parts[2]],
          }}
          streaming={false}
        />
      </ThemeProvider>,
    );

    expect(screen.getByRole("button", { name: /已完成执行过程/ }))
      .toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("button", { name: /edit_file/i })).toBeNull();
  });

  it("只有工具调用的轮次在执行时展开，结束后自动收起", async () => {
    const liveMessage: ChatMessage = {
      id: "tool-only-live",
      role: "assistant",
      complete: false,
      parts: [message.parts[2]],
    };
    const { rerender } = render(
      <ThemeProvider><MessageItem message={liveMessage} streaming /></ThemeProvider>,
    );
    expect(screen.getByRole("button", { name: /正在分析任务/ }))
      .toHaveAttribute("aria-expanded", "true");

    rerender(
      <ThemeProvider><MessageItem message={{ ...liveMessage, complete: true }} streaming={false} /></ThemeProvider>,
    );
    await waitFor(() => expect(screen.getByRole("button", { name: /已完成执行过程/ }))
      .toHaveAttribute("aria-expanded", "false"));
  });

  it("切换任务后保留每条执行过程的手动折叠状态", () => {
    const liveMessage: ChatMessage = {
      id: "manual-process-a",
      role: "assistant",
      complete: false,
      parts: [message.parts[2]],
    };
    const otherMessage: ChatMessage = { ...liveMessage, id: "manual-process-b" };
    const view = (sessionId: string, item: ChatMessage) => (
      <ThemeProvider>
        <MessageItem key={`${sessionId}:${item.id}`} sessionId={sessionId} message={item} streaming />
      </ThemeProvider>
    );
    const { rerender } = render(view("task-a", liveMessage));
    const first = screen.getByRole("button", { name: /正在分析任务/ });
    expect(first).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(first);
    expect(first).toHaveAttribute("aria-expanded", "false");

    rerender(view("task-b", otherMessage));
    expect(screen.getByRole("button", { name: /正在分析任务/ }))
      .toHaveAttribute("aria-expanded", "true");
    rerender(view("task-a", liveMessage));
    expect(screen.getByRole("button", { name: /正在分析任务/ }))
      .toHaveAttribute("aria-expanded", "false");

    fireEvent.click(screen.getByRole("button", { name: /正在分析任务/ }));
    rerender(view("task-b", otherMessage));
    rerender(view("task-a", liveMessage));
    expect(screen.getByRole("button", { name: /正在分析任务/ }))
      .toHaveAttribute("aria-expanded", "true");
  });

  it("历史轮次被手动展开后，切换任务回来仍保持展开", () => {
    const historical: ChatMessage = {
      id: "manual-history-a",
      role: "assistant",
      complete: true,
      parts: [message.parts[2]],
    };
    const other: ChatMessage = { ...historical, id: "manual-history-b" };
    const view = (sessionId: string, item: ChatMessage) => (
      <ThemeProvider>
        <MessageItem key={`${sessionId}:${item.id}`} sessionId={sessionId} message={item} streaming={false} />
      </ThemeProvider>
    );
    const { rerender } = render(view("history-a", historical));
    const process = screen.getByRole("button", { name: /已完成执行过程/ });
    expect(process).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(process);
    rerender(view("history-b", other));
    expect(screen.getByRole("button", { name: /已完成执行过程/ }))
      .toHaveAttribute("aria-expanded", "false");
    rerender(view("history-a", historical));
    expect(screen.getByRole("button", { name: /已完成执行过程/ }))
      .toHaveAttribute("aria-expanded", "true");
  });

  it("将模型混在同一文本块的思考折叠，只直接显示答案", () => {
    render(
      <ThemeProvider>
        <MessageItem
          message={{
            id: "assistant-tagged-thinking",
            role: "assistant",
            complete: true,
            parts: [{
              kind: "text",
              text: "<analysis>The user asks about a timeout.</analysis>\n\n超时通常表示某一层的等待预算用完了。",
              streamId: "generation-1",
            }],
          }}
          streaming={false}
        />
      </ThemeProvider>,
    );

    expect(screen.getByText("超时通常表示某一层的等待预算用完了。")).toBeInTheDocument();
    expect(screen.queryByText("The user asks about a timeout.")).toBeNull();
    const process = screen.getByRole("button", { name: /已完成思考/ });
    expect(process).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(process);
    fireEvent.click(screen.getByText("深度思考"));
    expect(screen.getByText("The user asks about a timeout.")).toBeInTheDocument();
    expect(screen.queryByText(/<analysis>/)).toBeNull();
  });

  it("展开后可查看操作并打开右侧详情", () => {
    const onOpenTool = vi.fn();
    renderMessage(onOpenTool);

    fireEvent.click(screen.getByRole("button", { name: /已完成执行过程/ }));
    fireEvent.click(screen.getByRole("button", { name: /edit_file/i }));
    expect(onOpenTool).toHaveBeenCalledWith(
      expect.objectContaining({ toolCallId: "tool-1" }),
    );

    fireEvent.click(screen.getByText("深度思考"));
    expect(screen.getByText("我先检查代码。")).toBeInTheDocument();
    expect(screen.getByText("需要从客户端交互状态入手。")).toBeInTheDocument();
  });

  it("答案输出前展开思考，首个答案到达时立即自动收起", async () => {
    const liveMessage: ChatMessage = {
      id: "assistant-live",
      role: "assistant",
      complete: false,
      startedAt: 1_000,
      parts: [{ kind: "thought", text: "正在分析请求。" }],
    };
    const { rerender } = render(
      <ThemeProvider>
        <MessageItem message={liveMessage} streaming />
      </ThemeProvider>,
    );
    expect(screen.getByRole("button", { name: /正在分析任务/ }))
      .toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("深度思考").closest("details")).toHaveAttribute("open");

    rerender(
      <ThemeProvider>
        <MessageItem
          message={{
            ...liveMessage,
            parts: [
              ...liveMessage.parts,
              { kind: "text", text: "这是正式答案的第一部分。" },
            ],
          }}
          streaming
        />
      </ThemeProvider>,
    );
    expect(screen.getByText("这是正式答案的第一部分。")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: /已完成思考/ }))
      .toHaveAttribute("aria-expanded", "false"));
  });

  it("规划文本之后继续思考和并行工具时保持运行中与默认展开", () => {
    const liveMessage: ChatMessage = {
      id: "preface-before-live-tools",
      role: "assistant",
      complete: false,
      parts: [
        { kind: "text", text: "我会先安排并行检查。" },
        { kind: "thought", text: "正在继续检查界面。" },
        { kind: "tool_call", toolCall: {
          toolCallId: "live-edit", title: "Edit src/App.tsx", kind: "edit_file",
          status: "in_progress", content: [],
        } },
        { kind: "tool_call", toolCall: {
          toolCallId: "live-child", title: "检查产物交付", kind: "spawn_subagent",
          status: "in_progress", content: [],
        } },
      ],
    };
    const { container, rerender } = render(
      <ThemeProvider><MessageItem message={liveMessage} streaming /></ThemeProvider>,
    );
    expect(container.querySelector(".execution-process--running")).toBeInTheDocument();
    const header = container.querySelector(".execution-process__header");
    expect(header).toHaveAttribute("aria-expanded", "true");
    expect(header).toHaveTextContent("0/2 项");
    expect(header).not.toHaveTextContent("已完成");
    expect(screen.getByText("深度思考").closest("details")).toHaveAttribute("open");

    fireEvent.click(header!);
    rerender(<ThemeProvider><MessageItem message={{ ...liveMessage, parts: [
      ...liveMessage.parts, { kind: "thought", text: "并行检查仍在继续。" },
    ] }} streaming /></ThemeProvider>);
    expect(container.querySelector(".execution-process__header")).toHaveAttribute("aria-expanded", "false");
    expect(container.querySelector(".execution-process--running")).toBeInTheDocument();
  });

  it("所有工具完成也需等真正的尾部答复才自动折叠过程", async () => {
    const preface: ChatMessage = {
      id: "preface-before-finished-tool",
      role: "assistant", complete: false,
      parts: [
        { kind: "text", text: "我先检查项目。" },
        { kind: "thought", text: "工具完成后还需整理结论。" },
        message.parts[2],
      ],
    };
    const { container, rerender } = render(
      <ThemeProvider><MessageItem message={preface} streaming /></ThemeProvider>,
    );
    expect(screen.getByRole("button", { name: /正在分析任务/ })).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".execution-process--running")).toBeInTheDocument();
    rerender(<ThemeProvider><MessageItem message={{ ...preface, parts: [
      ...preface.parts, { kind: "text", text: "这是工具完成后的最终答复。" },
    ] }} streaming /></ThemeProvider>);
    expect(screen.getByText("这是工具完成后的最终答复。")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: /已完成执行过程/ })).toHaveAttribute("aria-expanded", "false"));
    expect(container.querySelector(".execution-process--complete")).toBeInTheDocument();
  });

  it("已有文本之后的新思考不被当作最终答复，用户选择保持不变", () => {
    const liveMessage: ChatMessage = {
      id: "preface-then-thought",
      role: "assistant", complete: false,
      parts: [
        { kind: "text", text: "我先分析请求。" },
        { kind: "thought", text: "当前仍在分析。" },
      ],
    };
    const { container, rerender } = render(<ThemeProvider><MessageItem message={liveMessage} streaming /></ThemeProvider>);
    const header = screen.getByRole("button", { name: /正在分析任务/ });
    expect(header).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".execution-process--running")).toBeInTheDocument();
    fireEvent.click(header);
    rerender(<ThemeProvider><MessageItem message={{ ...liveMessage, parts: [
      ...liveMessage.parts, { kind: "text", text: "现在开始最终答复。" },
    ] }} streaming /></ThemeProvider>);
    expect(screen.getByRole("button", { name: /已完成思考/ })).toHaveAttribute("aria-expanded", "false");
  });

  it("调用者已有答案标志也不能覆盖未完成工具，终态仍显示需要注意", () => {
    const parts: ChatMessage["parts"] = [{ kind: "tool_call", toolCall: {
      toolCallId: "unfinished-tool-defense", kind: "read_file", title: "Read report.md",
      status: "in_progress", content: [],
    } }];
    const { container, rerender } = render(<ThemeProvider><ExecutionProcess parts={parts} active hasFinalAnswer /></ThemeProvider>);
    expect(container.querySelector(".execution-process--running")).toBeInTheDocument();
    expect(container.querySelector(".execution-process__header")).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".execution-process__header")).not.toHaveTextContent("已完成");
    rerender(<ThemeProvider><ExecutionProcess parts={parts} active={false} hasFinalAnswer stopReason="end_turn" /></ThemeProvider>);
    expect(container.querySelector(".execution-process--attention")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /执行已结束，但有操作未收尾/ })).toHaveAttribute("aria-expanded", "false");
  });

  it("用户手动展开后不被后续答案输出抢夺控制权", async () => {
    const liveMessage: ChatMessage = {
      id: "assistant-manual-process",
      role: "assistant",
      complete: false,
      parts: [{ kind: "thought", text: "正在分析。" }],
    };
    const { rerender } = render(
      <ThemeProvider>
        <MessageItem message={liveMessage} streaming />
      </ThemeProvider>,
    );
    const process = screen.getByRole("button", { name: /正在分析任务/ });
    fireEvent.click(process);
    fireEvent.click(process);
    expect(process).toHaveAttribute("aria-expanded", "true");

    rerender(
      <ThemeProvider>
        <MessageItem
          message={{
            ...liveMessage,
            parts: [
              ...liveMessage.parts,
              { kind: "text", text: "开始输出正式答案。" },
            ],
          }}
          streaming
        />
      </ThemeProvider>,
    );

    await waitFor(() => expect(screen.getByRole("button", { name: /已完成思考/ }))
      .toHaveAttribute("aria-expanded", "true"));
  });

  it("没有回复文本的失败保留异常提示和可展开的终止原因，不提供空复制", () => {
    render(
      <ThemeProvider>
        <MessageItem
          message={{
            id: "failed-empty",
            role: "assistant",
            parts: [],
            complete: true,
            stopReason: "error",
            agentResult: "provider disconnected",
          }}
          streaming={false}
        />
      </ThemeProvider>,
    );

    expect(screen.getByRole("button", { name: /执行未正常完成/ })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    expect(screen.queryByRole("button", { name: "复制纯文本" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /执行未正常完成/ }));
    expect(screen.getByRole("alert")).toHaveTextContent("provider disconnected");
  });

  it("已有答案的失败过程保持紧凑，异常状态和详情仍可查看", () => {
    render(
      <ThemeProvider>
        <MessageItem
          message={{
            ...message,
            id: "failed-with-answer",
            stopReason: "error",
            agentResult: "upstream timeout",
          }}
          streaming={false}
        />
      </ThemeProvider>,
    );

    expect(screen.getByText("最终修复结果")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /执行未正常完成/ })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    fireEvent.click(screen.getByRole("button", { name: /执行未正常完成/ }));
    expect(screen.getByRole("alert")).toHaveTextContent("upstream timeout");
  });

  it("在对应回复的执行过程中展示个人知识文件、行号和片段", () => {
    useKnowledgeStore.setState({
      turnTraces: {
        "session-kb": {
          "prompt-kb": {
            selectedSources: ["personal"],
            personal: {
              state: "used",
              resultCount: 1,
              sourceCount: 1,
              titles: ["差旅制度"],
              items: [{
                title: "差旅制度",
                path: "/notes/travel.md",
                sourceLabel: "本地笔记",
                snippet: "住宿标准为每晚 500 元。",
                startLine: 8,
                endLine: 10,
              }],
            },
          },
        },
      },
    });

    render(
      <ThemeProvider>
        <MessageItem
          message={{
            id: "assistant-kb",
            role: "assistant",
            promptId: "prompt-kb",
            parts: [{ kind: "text", text: "根据制度回答。" }],
            complete: true,
          }}
          streaming={false}
          sessionId="session-kb"
        />
      </ThemeProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: /知识检索完成/ }));
    expect(screen.getByText("已向模型提供 1 个相关片段")).toBeInTheDocument();
    expect(screen.getByText("本地笔记 · 第 8–10 行")).toBeInTheDocument();
    expect(screen.getByText("住宿标准为每晚 500 元。")).toBeInTheDocument();
  });
});
