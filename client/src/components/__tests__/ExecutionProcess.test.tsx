import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MessageItem } from "../MessageItem";
import { ExecutionProcess } from "../ExecutionProcess";
import { ThemeProvider } from "../ThemeProvider";
import type { ChatMessage, ToolCallView } from "@/stores/session-store";
import { useKnowledgeStore } from "@/stores/knowledge-store";

describe("assistant execution process", () => {
  it("过程外只保留匹配当前阶段的一行，更新时替换并可在过程内找回旧说明", () => {
    const initial: ChatMessage = { id: "stage-matched-progress", role: "assistant", complete: false, parts: [
      { kind: "text", text: "已经拿到官方票价。", streamId: "1" },
      { kind: "tool_call", streamId: "1", toolCall: { toolCallId: "tickets", kind: "web_fetch", title: "核对官方票价", status: "completed", content: [] } },
      { kind: "text", text: "继续核对索道信息。\n\n正在读取官方开放时间。", streamId: "2" },
      { kind: "tool_call", streamId: "2", toolCall: { toolCallId: "hours", kind: "web_fetch", title: "核对开放时间", status: "in_progress", content: [] } },
    ] };
    const view = (message: ChatMessage, waitingReason?: string) => <ThemeProvider><MessageItem message={message} streaming waitingReason={waitingReason} /></ThemeProvider>;
    const { container, rerender } = render(view(initial));
    const excerpt = container.querySelector(".execution-process__excerpt")!;
    expect(excerpt).toHaveTextContent("正在读取官方开放时间。");
    expect(excerpt.querySelectorAll("p")).toHaveLength(1);
    expect(container.querySelectorAll(".msg__body > .markdown-body")).toHaveLength(0);
    expect(screen.queryByText("已经拿到官方票价。")).toBeNull();
    fireEvent.click(container.querySelector(".execution-process__header")!);
    expect(container.querySelectorAll(".execution-process__stage")).toHaveLength(2);
    expect(container.querySelector(".execution-process__body")).toHaveTextContent("已经拿到官方票价。");
    expect(container.querySelector(".execution-process__stage--current")).toHaveAttribute("data-stage-id", excerpt.getAttribute("data-stage-id"));
    expect(container.querySelectorAll(".execution-process__excerpt")).toHaveLength(1);
    rerender(view(initial, "等待你批准操作"));
    expect(excerpt).toHaveTextContent("等待你批准操作");
    expect(container.querySelector(".execution-process__stage--current")).toHaveAttribute("data-stage-state", "等待确认");
    rerender(view({ ...initial, parts: [...initial.parts, { kind: "thought", text: "下一阶段分析", streamId: "3" }] }));
    expect(container.querySelector(".execution-process__excerpt")).toBeNull();
  });

  it("同批连续操作折叠为可展开的操作组，思考和后续说明仍在原位", () => {
    const onOpenTool = vi.fn();
    const { container } = render(<ThemeProvider><ExecutionProcess active autoExpand onOpenTool={onOpenTool} parts={[
      { kind: "text", phase: "commentary", text: "核对票价与交通", streamId: "1" },
      ...["tickets", "transport"].map(id => ({ kind: "tool_call" as const, streamId: "1", toolCall: { toolCallId: id, title: id, kind: "read", status: "completed" as const, content: [] } })),
      { kind: "thought", text: "核对两个来源", streamId: "1" },
      { kind: "text", phase: "commentary", text: "现在核对开放时间", streamId: "1" },
    ]} /></ThemeProvider>);
    expect(container.querySelectorAll(".execution-process__tool-group")).toHaveLength(1);
    expect(container.querySelector(".toolcall")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /已完成 2 项操作/ }));
    expect(container.querySelectorAll(".toolcall")).toHaveLength(2);
    fireEvent.click(screen.getByRole("button", { name: "read tickets 完成" }));
    expect(onOpenTool).toHaveBeenCalledWith(expect.objectContaining({ toolCallId: "tickets" }));
    expect(container.querySelector(".execution-process__reasoning")).not.toHaveAttribute("open");
    expect(container.querySelector(".execution-process__excerpt")).toHaveTextContent("现在核对开放时间");
  });

  it("阅读单项操作时新增同批操作不会卸载已聚焦的操作", () => {
    const part = (id: string): ChatMessage["parts"][number] => ({ kind: "tool_call", streamId: "1", toolCall: { toolCallId: id, kind: "read", title: id, status: "completed", content: [] } });
    const view = (parts: ChatMessage["parts"]) => <ThemeProvider><ExecutionProcess active autoExpand parts={parts} /></ThemeProvider>;
    const { rerender } = render(view([part("focused-call")]));
    const target = screen.getByRole("button", { name: "read focused-call 完成" });
    act(() => target.focus());
    rerender(view([part("focused-call"), part("new-call")]));
    expect(target).toHaveFocus();
    expect(screen.getByRole("button", { name: /已完成 2 项操作/ })).toHaveAttribute("aria-expanded", "true");
  });

  it("同一计划跨生成时保持失败、后续进展和重试的顺序，思考在原位折叠", () => {
    const parts: ChatMessage["parts"] = [
      { kind: "thought", text: "检查原配置", streamId: "1", stageTitle: "核验结果并修复" },
      { kind: "tool_call", streamId: "1", stageTitle: "核验结果并修复", toolCall: { toolCallId: "first", title: "首次读取失败", kind: "read", status: "failed", content: [] } },
      { kind: "text", phase: "commentary", text: "改用备用配置。", streamId: "2", stageTitle: "核验结果并修复" },
      { kind: "thought", text: "检查备用配置", streamId: "2", stageTitle: "核验结果并修复" },
      { kind: "tool_call", streamId: "2", stageTitle: "核验结果并修复", toolCall: { toolCallId: "retry", title: "读取备用配置", kind: "read", status: "in_progress", content: [] } },
    ];
    const { container } = render(<ThemeProvider><ExecutionProcess parts={parts} active autoExpand /></ThemeProvider>);
    const stage = container.querySelector(".execution-process__stage")!;
    expect(container.querySelectorAll(".execution-process__stage")).toHaveLength(1);
    const order = Array.from(stage.children).filter(element => element.matches(".execution-process__reasoning, .execution-process__progress, .execution-process__tool"))
      .map(element => element.querySelector("[data-tool-call-id]")?.getAttribute("data-tool-call-id")
        ?? (element.matches("details") ? "thought" : element.textContent));
    expect(order).toEqual(["thought", "first", "改用备用配置。", "thought", "retry"]);
    expect(stage.querySelector(".execution-process__stage-index")).toHaveTextContent("1");
    const thoughts = stage.querySelectorAll("details");
    expect(thoughts).toHaveLength(2);
    thoughts.forEach(thought => expect(thought).not.toHaveAttribute("open"));
  });

  it("没有真实计划时使用活动分组，不把模型生成批次编号成任务步骤", () => {
    const { container } = render(<ThemeProvider><ExecutionProcess active autoExpand parts={[
      { kind: "thought", text: "先分析", streamId: "1" },
      { kind: "tool_call", streamId: "2", toolCall: { toolCallId: "weather", kind: "other", title: "echoagent-live-info__weather_forecast", status: "in_progress", content: [] } },
    ]} /></ThemeProvider>);
    expect(screen.getByRole("list", { name: "执行记录" })).toBeVisible();
    expect(Array.from(container.querySelectorAll(".execution-process__stage-index"), element => element.textContent)).toEqual(["·", "·"]);
    expect(screen.getByRole("heading", { name: "查询天气" })).toBeVisible();
  });

  it("真实 UseTool 天气调用在阶段标题和工具卡片中使用一致文案", () => {
    render(<ThemeProvider><ExecutionProcess active parts={[{ kind: "tool_call", toolCall: {
      toolCallId: "weather-real", kind: "other", title: "echoagent-live-info__weather_forecast", toolName: "use_tool", status: "in_progress", content: [],
      rawInput: { variant: "UseTool", tool_name: "echoagent-live-info__weather_forecast", tool_input: { location: "南京" } },
    } }]} /></ThemeProvider>);
    expect(screen.getByRole("button", { name: "正在查询天气" })).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "1 项进行中" }));
    expect(screen.getByText("🌤️ 天气查询")).toBeVisible();
    expect(screen.getByRole("button", { name: "other 南京 运行中" })).toBeVisible();
  });

  it("真实事件未提供语义标记时也将说明归入过程，正文只显示当前阶段的一行", () => {
    const message: ChatMessage = { id: "stage-description", role: "assistant", complete: false, parts: [
      { kind: "thought", text: "让我并行发起。", streamId: "1" },
      ...["weather", "web"].map(id => ({ kind: "tool_call" as const, streamId: "1", toolCall: {
        toolCallId: id, title: 'Search tools: "天气 旅游"', kind: "other", status: "completed" as const, content: [],
      } })),
      { kind: "text", text: "我先确认实时信息来源，同时核对出行条件。", streamId: "1" },
    ] };
    const { container } = render(<ThemeProvider><MessageItem message={message} streaming /></ThemeProvider>);
    expect(screen.getByText("我先确认实时信息来源，同时核对出行条件。")).toBeVisible();
    expect(screen.getAllByText("我先确认实时信息来源，同时核对出行条件。")).toHaveLength(1);
    expect(screen.getByRole("button", { name: "正在分析任务" })).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByRole("button", { name: /已完成 2 项工具查找/ })).toHaveAttribute("aria-expanded", "false");
    expect(container.querySelector(".toolcall, .msg__writing, .msg__actions")).toBeNull();
    expect(screen.queryByText("让我并行发起。")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "正在分析任务" }));
    expect(screen.getByRole("list", { name: "执行记录" })).toBeVisible();
    expect(container.querySelector(".execution-process__progress")).toHaveTextContent("我先确认实时信息来源，同时核对出行条件。");
    expect(screen.getByText("思考摘要").closest("details")).not.toHaveAttribute("open");
  });

  it.each(["等待你补充信息", "等待你确认计划", "等待你批准操作"])("%s 使用真实等待状态且保留独立阶段成果", waitingReason => {
    const message: ChatMessage = { id: `waiting-${waitingReason}`, role: "assistant", complete: false, parts: [
      { kind: "text", phase: "commentary", text: "正在核对条件。", stageTitle: "确认出行安排" },
      { kind: "text", phase: "partial_answer", text: "目前已确认的交通方案。" },
    ] };
    const view = (reason?: string) => <ThemeProvider><MessageItem message={message} streaming waitingForModel={!reason} waitingReason={reason} /></ThemeProvider>;
    const { container, rerender } = render(view(waitingReason));
    expect(screen.getByRole("button", { name: waitingReason })).toBeVisible();
    expect(container.querySelector(".execution-process--waiting")).toBeInTheDocument();
    expect(container.querySelector(".execution-process--running, .msg__writing")).toBeNull();
    expect(screen.getByText("目前已确认的交通方案。")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: waitingReason }));
    rerender(view());
    expect(container.querySelector(".execution-process__header")).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".execution-process--running")).toBeInTheDocument();
  });

  it("默认突出进度说明并折叠工具和思考，正式答复开始后撤下预览", () => {
    const onOpenTool = vi.fn();
    const live: ChatMessage = {
      id: "live-preview", role: "assistant", complete: false,
      parts: [
        { kind: "text", text: "我先核对资料。", streamId: "progress-1" },
        { kind: "thought", text: "准备检查表格的字段与统计口径。", streamId: "progress-1" },
        { kind: "tool_call", streamId: "progress-1", toolCall: { toolCallId: "preview-read", title: "Read 报告.xlsx", kind: "read_file", status: "in_progress", content: [] } },
      ],
    };
    const view = (item: ChatMessage) => <ThemeProvider><MessageItem message={item} streaming={!item.complete} onOpenTool={onOpenTool} /></ThemeProvider>;
    const { container, rerender } = render(view(live));
    expect(screen.getByRole("button", { name: /正在读取项目文件/ })).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByLabelText("实时执行进展")).toBeVisible();
    expect(screen.getByText("我先核对资料。")).toBeVisible();
    expect(screen.queryByText("准备检查表格的字段与统计口径。")).toBeNull();
    expect(container.querySelectorAll(".toolcall")).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: /1 项进行中/ }));
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
    fireEvent.click(screen.getByRole("button", { name: /项进行中/ }));
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
    fireEvent.click(screen.getByRole("button", { name: /已完成 1 项操作/ }));
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
    expect(screen.queryByText("正在核对引用来源。")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "查看思考摘要" }));
    fireEvent.click(screen.getByText("思考摘要"));
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
    fireEvent.click(screen.getByText("思考摘要"));
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

    fireEvent.click(screen.getByText("思考摘要"));
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
    expect(screen.getByText("思考摘要").closest("details")).not.toHaveAttribute("open");

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
    expect(screen.getByText("思考摘要").closest("details")).not.toHaveAttribute("open");

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
    fireEvent.click(screen.getByRole("button", { name: /已完成 1 项操作 · 1 项失败/ }));
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
