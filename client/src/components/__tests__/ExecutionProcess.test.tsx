import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MessageItem } from "../MessageItem";
import { ExecutionProcess } from "../ExecutionProcess";
import { ThemeProvider } from "../ThemeProvider";
import type { ChatMessage, ToolCallView } from "@/stores/session-store";
import { useKnowledgeStore } from "@/stores/knowledge-store";

describe("assistant execution process", () => {
  it("默认收起时直接显示思考片段和真实操作，正式答复开始后撤下预览", () => {
    const onOpenTool = vi.fn();
    const live: ChatMessage = {
      id: "live-preview", role: "assistant", complete: false,
      parts: [
        { kind: "text", text: "我先核对资料。" },
        { kind: "thought", text: "准备检查表格的字段与统计口径。" },
        { kind: "tool_call", toolCall: { toolCallId: "preview-read", title: "Read 报告.xlsx", kind: "read_file", status: "in_progress", content: [] } },
      ],
    };
    const view = (item: ChatMessage) => <ThemeProvider><MessageItem message={item} streaming={!item.complete} onOpenTool={onOpenTool} /></ThemeProvider>;
    const { container, rerender } = render(view(live));
    expect(screen.getByRole("button", { name: /正在读取项目文件/ })).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByLabelText("实时执行进展")).toBeVisible();
    expect(screen.getByText("准备检查表格的字段与统计口径。")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: /read_file.*报告.xlsx.*运行中/ }));
    expect(onOpenTool).toHaveBeenCalledWith(expect.objectContaining({ toolCallId: "preview-read" }));
    expect(container.querySelector(".msg__caret")).toBeNull();
    expect(container.querySelector(".msg__writing")).toBeNull();

    const finishedTool = { ...live.parts[2], toolCall: { ...(live.parts[2] as Extract<ChatMessage["parts"][number], { kind: "tool_call" }>).toolCall, status: "completed" as const } };
    const answering = { ...live, parts: [...live.parts.slice(0, 2), finishedTool, { kind: "text" as const, text: "核对结果如下。" }] };
    rerender(view(answering));
    expect(screen.queryByLabelText("实时执行进展")).toBeNull();
    expect(screen.getByRole("button", { name: /正在生成回复/ })).toBeVisible();
    expect(container.querySelector(".msg__writing")).toBeVisible();
    expect(screen.getAllByRole("status")).toHaveLength(1);
    expect(screen.getByText("核对结果如下。")).toBeVisible();
    rerender(view({ ...answering, complete: true }));
    expect(screen.getByRole("button", { name: /查看过程/ })).toHaveAttribute("aria-expanded", "false");
    expect(container.querySelector(".msg__writing")).toBeNull();
  });

  it("正文末尾状态与并行工具、等待决定、完成状态互斥", () => {
    const parts: ChatMessage["parts"] = [
      { kind: "thought", text: "正在核对资料。" },
      { kind: "tool_call", toolCall: { toolCallId: "parallel", kind: "read_file", title: "核对资料", status: "in_progress", content: [] } },
      { kind: "text", text: "这里先保留部分说明。" },
    ];
    const message: ChatMessage = { id: "parallel-answer-status", role: "assistant", complete: false, parts };
    const view = (item: ChatMessage, waitingForModel = true) => <ThemeProvider><MessageItem message={item} streaming waitingForModel={waitingForModel} /></ThemeProvider>;
    const { container, rerender } = render(view(message));
    expect(container.querySelector(".msg__writing")).toBeNull();
    expect(screen.getByLabelText("实时执行进展")).toBeVisible();
    const answering = { ...message, parts: parts.map(part => part.kind === "tool_call" ? { ...part, toolCall: { ...part.toolCall, status: "completed" as const } } : part) };
    rerender(view(answering));
    expect(container.querySelector(".msg__writing")).toBeVisible();
    rerender(view(answering, false));
    expect(container.querySelector(".msg__writing")).toBeNull();
    rerender(view({ ...answering, complete: true }));
    expect(container.querySelector(".msg__writing")).toBeNull();
  });

  it("工具完成和新增并行工具不移除正在聚焦的预览项，焦点离开后恢复最近三项", () => {
    const onOpenTool = vi.fn();
    const makeTool = (id: string, status: ToolCallView["status"]): ChatMessage["parts"][number] => ({
      kind: "tool_call", toolCall: { toolCallId: id, kind: "read_file", title: id, status, content: [] },
    });
    const parts = [makeTool("focused", "in_progress"), ...[1, 2, 3].map(id => makeTool(`other-${id}`, "completed"))];
    const view = (items: ChatMessage["parts"]) => <ThemeProvider><ExecutionProcess parts={items} active onOpenTool={onOpenTool} /></ThemeProvider>;
    const { rerender, container } = render(view(parts));
    const target = screen.getByRole("button", { name: "read_file focused 运行中" });
    act(() => target.focus());
    const next = [makeTool("focused", "completed"), ...parts.slice(1), makeTool("new-running", "in_progress")];
    rerender(view(next));
    expect(target).toHaveFocus();
    expect(target).toHaveAccessibleName("read_file focused 完成");
    expect(container.querySelectorAll(".execution-process__preview .toolcall")).toHaveLength(3);
    fireEvent.click(target);
    expect(onOpenTool).toHaveBeenLastCalledWith(expect.objectContaining({ toolCallId: "focused", status: "completed" }));
    const header = container.querySelector<HTMLButtonElement>(".execution-process__header")!;
    act(() => header.focus());
    expect(header).toHaveFocus();
    expect(target).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "read_file new-running 运行中" })).toBeVisible();
  });

  it.each(["answer", "complete"])("预览因 %s 自动移除时把键盘焦点移回过程入口", transition => {
    const parts: ChatMessage["parts"] = [{ kind: "tool_call", toolCall: { toolCallId: "transition", kind: "read_file", title: "transition", status: "completed", content: [] } }];
    const view = (active: boolean, hasFinalAnswer = false) => <ThemeProvider><ExecutionProcess parts={parts} active={active} hasFinalAnswer={hasFinalAnswer} /></ThemeProvider>;
    const { container, rerender } = render(view(true));
    act(() => screen.getByRole("button", { name: "read_file transition 完成" }).focus());
    rerender(view(transition !== "complete", transition === "answer"));
    expect(screen.queryByLabelText("实时执行进展")).toBeNull();
    expect(container.querySelector(".execution-process__header")).toHaveFocus();
  });

  it("自动撤下预览时保留详情面板内的焦点", () => {
    const parts: ChatMessage["parts"] = [{ kind: "thought", text: "正在核对。" }];
    const view = (active: boolean) => <ThemeProvider><ExecutionProcess parts={parts} active={active} /><button>详情面板操作</button></ThemeProvider>;
    const { rerender } = render(view(true));
    const outside = screen.getByRole("button", { name: "详情面板操作" });
    act(() => outside.focus());
    rerender(view(false));
    expect(outside).toHaveFocus();
  });

  it("仅正文流式输出时使用可读状态，完成或等待决定时移除", () => {
    const live: ChatMessage = { id: "text-writing", role: "assistant", complete: false, parts: [{ kind: "text", text: "这是回复的开头。" }] };
    const { container, rerender } = render(<ThemeProvider><MessageItem message={live} streaming /></ThemeProvider>);
    expect(screen.getByRole("status", { name: "" })).toHaveTextContent("正在生成回复");
    expect(container.querySelector(".msg__caret")).toBeNull();
    rerender(<ThemeProvider><MessageItem message={live} streaming waitingForModel={false} /></ThemeProvider>);
    expect(container.querySelector(".msg__writing")).toBeNull();
    rerender(<ThemeProvider><MessageItem message={{ ...live, complete: true }} streaming={false} /></ThemeProvider>);
    expect(container.querySelector(".msg__writing")).toBeNull();
  });

  it("上一段说明之后的流式推理标签仍显示思考进展，不误判为正式答复", () => {
    render(<ThemeProvider><MessageItem streaming message={{
      id: "tagged-live-preview", role: "assistant", complete: false,
      parts: [{ kind: "text", text: "我先整理资料。" }, { kind: "text", text: "<think>正在核对引用来源。" }],
    }} /></ThemeProvider>);
    expect(screen.getByLabelText("实时执行进展")).toBeVisible();
    expect(screen.getByText("正在核对引用来源。")).toBeVisible();
    expect(screen.queryByText("正在生成回复")).toBeNull();
  });

  it("执行过程收起后仍显示最新资料的读取覆盖", () => {
    render(<ThemeProvider><ExecutionProcess autoExpand active={false} parts={[{ kind: "tool_call", toolCall: {
      toolCallId: "document", kind: "read_file", title: "Read 比赛.xlsx", status: "completed", content: [],
      documentProgress: { source: "比赛.xlsx", revision: "a".repeat(64), readRecords: 12, totalRecords: 100, checkpointCount: 2, complete: false },
    } }]} /></ThemeProvider>);
    const header = screen.getByRole("button", { name: /查看过程/ });
    expect(header).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(header);
    expect(screen.getByText(/比赛.xlsx · 已读取 12\/100 段内容/)).toBeVisible();
  });
  const renderMessage = (onOpenTool?: (tool: ToolCallView) => void) => render(
    <ThemeProvider>
      <MessageItem autoExpandProcess
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
    expect(screen.getByRole("button", { name: /查看过程/ })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    expect(screen.queryByText("需要从客户端交互状态入手。")).toBeNull();
  });

  it("没有最终答复的历史工具过程也默认收起", () => {
    render(
      <ThemeProvider>
        <MessageItem autoExpandProcess
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

    expect(screen.getByRole("button", { name: /查看过程/ }))
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
      <ThemeProvider><MessageItem autoExpandProcess message={liveMessage} streaming /></ThemeProvider>,
    );
    expect(screen.getByRole("button", { name: /正在分析任务/ }))
      .toHaveAttribute("aria-expanded", "true");

    rerender(
      <ThemeProvider><MessageItem autoExpandProcess message={{ ...liveMessage, complete: true }} streaming={false} /></ThemeProvider>,
    );
    await waitFor(() => expect(screen.getByRole("button", { name: /查看过程/ }))
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
        <MessageItem autoExpandProcess key={`${sessionId}:${item.id}`} sessionId={sessionId} message={item} streaming />
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
        <MessageItem autoExpandProcess key={`${sessionId}:${item.id}`} sessionId={sessionId} message={item} streaming={false} />
      </ThemeProvider>
    );
    const { rerender } = render(view("history-a", historical));
    const process = screen.getByRole("button", { name: /查看过程/ });
    expect(process).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(process);
    rerender(view("history-b", other));
    expect(screen.getByRole("button", { name: /查看过程/ }))
      .toHaveAttribute("aria-expanded", "false");
    rerender(view("history-a", historical));
    expect(screen.getByRole("button", { name: /查看过程/ }))
      .toHaveAttribute("aria-expanded", "true");
  });

  it("将模型混在同一文本块的思考折叠，只直接显示答案", () => {
    render(
      <ThemeProvider>
        <MessageItem autoExpandProcess
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
    const process = screen.getByRole("button", { name: /查看过程/ });
    expect(process).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(process);
    fireEvent.click(screen.getByText("深度思考"));
    expect(screen.getByText("The user asks about a timeout.")).toBeInTheDocument();
    expect(screen.queryByText(/<analysis>/)).toBeNull();
  });

  it("展开后可查看操作并打开右侧详情", () => {
    const onOpenTool = vi.fn();
    renderMessage(onOpenTool);

    fireEvent.click(screen.getByRole("button", { name: /查看过程/ }));
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
        <MessageItem autoExpandProcess message={liveMessage} streaming />
      </ThemeProvider>,
    );
    expect(screen.getByRole("button", { name: /正在分析任务/ }))
      .toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("深度思考").closest("details")).toHaveAttribute("open");

    rerender(
      <ThemeProvider>
        <MessageItem autoExpandProcess
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
    await waitFor(() => expect(screen.getByRole("button", { name: /正在生成回复/ }))
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
      <ThemeProvider><MessageItem autoExpandProcess message={liveMessage} streaming /></ThemeProvider>,
    );
    expect(container.querySelector(".execution-process--running")).toBeInTheDocument();
    const header = container.querySelector(".execution-process__header");
    expect(header).toHaveAttribute("aria-expanded", "true");
    expect(header).not.toHaveTextContent("0/2 项");
    expect(screen.getByText("0/2 项")).toBeVisible();
    expect(header).not.toHaveTextContent("已完成");
    expect(screen.getByText("深度思考").closest("details")).toHaveAttribute("open");

    fireEvent.click(header!);
    rerender(<ThemeProvider><MessageItem autoExpandProcess message={{ ...liveMessage, parts: [
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
      <ThemeProvider><MessageItem autoExpandProcess message={preface} streaming /></ThemeProvider>,
    );
    expect(screen.getByRole("button", { name: /正在分析任务/ })).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".execution-process--running")).toBeInTheDocument();
    rerender(<ThemeProvider><MessageItem autoExpandProcess message={{ ...preface, parts: [
      ...preface.parts, { kind: "text", text: "这是工具完成后的最终答复。" },
    ] }} streaming /></ThemeProvider>);
    expect(screen.getByText("这是工具完成后的最终答复。")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: /正在生成回复/ })).toHaveAttribute("aria-expanded", "false"));
    expect(container.querySelector(".execution-process--running")).toBeInTheDocument();
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
    const { container, rerender } = render(<ThemeProvider><MessageItem autoExpandProcess message={liveMessage} streaming /></ThemeProvider>);
    const header = screen.getByRole("button", { name: /正在分析任务/ });
    expect(header).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".execution-process--running")).toBeInTheDocument();
    fireEvent.click(header);
    rerender(<ThemeProvider><MessageItem autoExpandProcess message={{ ...liveMessage, parts: [
      ...liveMessage.parts, { kind: "text", text: "现在开始最终答复。" },
    ] }} streaming /></ThemeProvider>);
    expect(screen.getByRole("button", { name: /正在生成回复/ })).toHaveAttribute("aria-expanded", "false");
  });

  it("调用者已有答案标志也不能覆盖未完成工具，终态仍显示需要注意", () => {
    const parts: ChatMessage["parts"] = [{ kind: "tool_call", toolCall: {
      toolCallId: "unfinished-tool-defense", kind: "read_file", title: "Read report.md",
      status: "in_progress", content: [],
    } }];
    const { container, rerender } = render(<ThemeProvider><ExecutionProcess autoExpand parts={parts} active hasFinalAnswer /></ThemeProvider>);
    expect(container.querySelector(".execution-process--running")).toBeInTheDocument();
    expect(container.querySelector(".execution-process__header")).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".execution-process__header")).not.toHaveTextContent("已完成");
    rerender(<ThemeProvider><ExecutionProcess autoExpand parts={parts} active={false} hasFinalAnswer stopReason="end_turn" /></ThemeProvider>);
    expect(container.querySelector(".execution-process--attention")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /查看过程/ })).toHaveAttribute("aria-expanded", "false");
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
        <MessageItem autoExpandProcess message={liveMessage} streaming />
      </ThemeProvider>,
    );
    const process = screen.getByRole("button", { name: /正在分析任务/ });
    fireEvent.click(process);
    fireEvent.click(process);
    expect(process).toHaveAttribute("aria-expanded", "true");

    rerender(
      <ThemeProvider>
        <MessageItem autoExpandProcess
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

    await waitFor(() => expect(screen.getByRole("button", { name: /正在生成回复/ }))
      .toHaveAttribute("aria-expanded", "true"));
  });

  it("没有回复文本的失败保留异常提示和可展开的终止原因，不提供空复制", () => {
    render(
      <ThemeProvider>
        <MessageItem autoExpandProcess
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

    expect(screen.getByRole("button", { name: /查看过程/ })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    expect(screen.queryByRole("button", { name: "复制纯文本" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /查看过程/ }));
    expect(screen.getByText("provider disconnected")).toBeVisible();
  });

  it("已有答案的失败过程保持紧凑，异常状态和详情仍可查看", () => {
    render(
      <ThemeProvider>
        <MessageItem autoExpandProcess
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
    expect(screen.getByRole("button", { name: /查看过程/ })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    fireEvent.click(screen.getByRole("button", { name: /查看过程/ }));
    expect(screen.getByText("upstream timeout")).toBeVisible();
  });

  it("已恢复的中间失败不显示顶层告警，展开仍可查看原始失败和成功重试", () => {
    const onOpenTool = vi.fn();
    const parts: ChatMessage["parts"] = [
      {kind:"tool_call",toolCall:{toolCallId:"weather-failed",toolName:"weather",kind:"web_fetch",title:"查询天气",rawInput:{city:"北京"},status:"failed",content:[{type:"text",text:"首次连接超时"}],finishedAt:1_000}},
      {kind:"tool_call",toolCall:{toolCallId:"weather-retry",toolName:"weather",kind:"web_fetch",title:"查询天气",rawInput:{city:"北京"},status:"completed",content:[{type:"text",text:"天气查询结果"}],startedAt:2_000}},
      {kind:"text",text:"这是天气结果"},
    ];
    const { container } = render(<ThemeProvider><MessageItem autoExpandProcess message={{id:"recovered-weather",role:"assistant",complete:true,stopReason:"end_turn",parts}} streaming={false} onOpenTool={onOpenTool} /></ThemeProvider>);
    expect(container.querySelector(".execution-process--recovered")).toBeInTheDocument();
    expect(container.querySelector(".execution-process__outcome-detail")).toBeNull();
    const header = screen.getByRole("button",{name:/查看过程/});
    expect(header).toHaveAttribute("aria-expanded","false");
    fireEvent.click(header);
    expect(screen.getByText(/1 次中间失败已由后续同一操作的重试恢复/)).toBeVisible();
    fireEvent.click(screen.getByRole("button",{name:/web_fetch .*失败/}));
    expect(onOpenTool).toHaveBeenLastCalledWith(expect.objectContaining({toolCallId:"weather-failed",status:"failed",content:[{type:"text",text:"首次连接超时"}]}));
    expect(screen.getByRole("button",{name:/web_fetch .*完成/})).toBeVisible();
  });

  it("未恢复的文件失败在回复附近提示影响，计数只在详情展示", () => {
    const { container } = render(<ThemeProvider><MessageItem autoExpandProcess message={{...message,id:"partial-result",stopReason:"end_turn",parts:[
      ...message.parts,{kind:"tool_call",toolCall:{toolCallId:"export-failed",kind:"write_file",title:"生成报告",status:"failed",content:[]}},
      {kind:"text",text:"所有工作都已完成"},
    ]}} streaming={false} /></ThemeProvider>);
    expect(container.querySelector(".execution-process--partial")).toBeInTheDocument();
    expect(container.querySelector(".execution-process--failed")).toBeNull();
    expect(screen.getByRole("button",{name:/查看过程/})).toHaveAttribute("aria-expanded","false");
    expect(screen.queryByText(/1 项失败操作尚未确认恢复/)).toBeNull();
    expect(screen.getByRole("alert")).toHaveTextContent("文件操作未成功");
    fireEvent.click(screen.getByRole("button",{name:/查看过程/}));
    expect(screen.getByText(/1 项失败操作尚未确认恢复/)).toBeVisible();
    expect(screen.getByText("所有工作都已完成")).toBeVisible();
  });

  it("历史回放仅关闭气泡而没有完成事件时，不因正文和成功工具记录显示完成", () => {
    const { container } = render(<ThemeProvider><MessageItem autoExpandProcess message={{...message,id:"unconfirmed-replay",replayed:true}} streaming={false} /></ThemeProvider>);
    expect(container.querySelector(".execution-process--attention")).toBeInTheDocument();
    expect(screen.getByRole("button",{name:/查看过程/})).toBeVisible();
    expect(screen.getByText("最终修复结果")).toBeVisible();
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
        <MessageItem autoExpandProcess
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

    fireEvent.click(screen.getByRole("button", { name: /查看过程/ }));
    expect(screen.getByText("已向模型提供 1 个相关片段")).toBeInTheDocument();
    expect(screen.getByText("本地笔记 · 第 8–10 行")).toBeInTheDocument();
    expect(screen.getByText("住宿标准为每晚 500 元。")).toBeInTheDocument();
  });
});
