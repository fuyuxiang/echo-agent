import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MessageItem } from "../MessageItem";
import { ThemeProvider } from "../ThemeProvider";
import type { ChatMessage, MessagePart } from "@/stores/session-store";
import { useKnowledgeStore } from "@/stores/knowledge-store";

const waiting: ChatMessage = {
  id: "waiting-message", promptId: "waiting-prompt", role: "assistant",
  complete: false, startedAt: 1_000, parts: [],
};
const outputCases: [string, MessagePart][] = [
  ["正文", {kind:"text",text:"已经开始输出答案"}],
  ["思考", {kind:"thought",text:"正在分析实际需求"}],
  ["工具", {kind:"tool_call",toolCall:{toolCallId:"read-file",kind:"read_file",title:"读取文档",status:"in_progress",content:[]}}],
];
const row = (message = waiting, streaming = true, sessionId = "waiting-session") => (
  <ThemeProvider><MessageItem message={message} streaming={streaming} sessionId={sessionId} /></ThemeProvider>
);

describe("waiting companion lifecycle", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000);
    useKnowledgeStore.setState({turnTraces:{},retrievals:{}});
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    useKnowledgeStore.setState({turnTraces:{},retrievals:{}});
  });

  it.each(outputCases)("%s开始后立即收起提示，后续不会重新出现", (_label, part) => {
    const view = render(row());
    act(() => vi.advanceTimersByTime(3_000));
    expect(view.container.querySelector(".msg__loading-tip")).not.toBeNull();
    view.rerender(row({...waiting,parts:[part]}));
    expect(view.container.querySelector(".msg__loading")).toBeNull();
    if (part.kind === "text") expect(screen.getByText(part.text)).toBeVisible();
    else expect(view.container.querySelector(".execution-process--running")).not.toBeNull();
    act(() => vi.advanceTimersByTime(30_000));
    expect(view.container.querySelector(".msg__loading-tip")).toBeNull();
  });

  it.each(["暂停", "完成", "错误"])("%s后停止等待提示", state => {
    const view = render(row());
    act(() => vi.advanceTimersByTime(3_000));
    const message = state === "暂停" ? waiting : {...waiting,complete:true,stopReason:state === "错误" ? "error" : "end_turn"};
    view.rerender(row(message, false));
    act(() => vi.advanceTimersByTime(30_000));
    expect(view.container.querySelector(".msg__loading")).toBeNull();
    if (state === "错误") expect(screen.getByRole("button",{name:/ · (展开|查看|收起)过程$/})).toBeVisible();
  });

  it("知识检索开始后显示真实进度，停止显示模型等待和趣味提示", () => {
    const view = render(row());
    act(() => vi.advanceTimersByTime(3_000));
    act(() => useKnowledgeStore.getState().setRetrieval("waiting-session",{state:"searching"},"waiting-prompt"));
    expect(view.container.querySelector(".msg__loading")).toBeNull();
    expect(screen.getByRole("button",{name:/正在检索个人知识库/})).toBeVisible();
  });

  it.each(["message", "session"])("切换 %s 时不继承上一轮提示和等待时间", scope => {
    const view = render(row());
    act(() => vi.advanceTimersByTime(15_000));
    expect(view.container.querySelector(".msg__loading-tip")).not.toBeNull();
    const next = {...waiting,id:scope === "message" ? "next-message" : waiting.id,startedAt:Date.now()};
    view.rerender(row(next,true,scope === "session" ? "next-session" : "waiting-session"));
    expect(view.container.querySelector(".msg__loading-tip")).toBeNull();
    expect(screen.getByText("正在请求模型")).toBeVisible();
    act(() => vi.advanceTimersByTime(2_999));
    expect(view.container.querySelector(".msg__loading-tip")).toBeNull();
    act(() => vi.advanceTimersByTime(1));
    expect(view.container.querySelector(".msg__loading-tip")).not.toBeNull();
  });

  it("长时间没有首个事件时明确说明尚未收到模型输出", () => {
    render(row());
    act(() => vi.advanceTimersByTime(30_000));
    expect(screen.getByText("尚未收到模型输出，可继续等待，或停止本次执行。")).toBeVisible();
    expect(screen.getByRole("status")).toHaveTextContent("正在等待模型响应");
  });

  it("历史中的未完成空消息不伪装成正在等待的任务", () => {
    const view = render(row(waiting,false));
    act(() => vi.advanceTimersByTime(30_000));
    expect(view.container.querySelector(".msg__loading")).toBeNull();
  });

  it("等待用户决定或取消执行时停止陪伴提示，保留实际工具进度", () => {
    const view = render(<ThemeProvider><MessageItem message={waiting} streaming waitingForModel={false} /></ThemeProvider>);
    act(() => vi.advanceTimersByTime(30_000));
    expect(view.container.querySelector(".msg__loading")).toBeNull();
    view.rerender(<ThemeProvider><MessageItem message={{...waiting,parts:[outputCases[2][1]]}} streaming waitingForModel={false} /></ThemeProvider>);
    expect(view.container.querySelector(".execution-process--running")).not.toBeNull();
  });

  it("回复期间使用流动 E，等待用户、出错和完成时保持静止", () => {
    const renderMessage = (props = {}) => <ThemeProvider><MessageItem message={waiting} streaming {...props} /></ThemeProvider>;
    const view = render(renderMessage());
    expect(view.container.querySelector(".msg__avatar img")).toBeNull();
    expect(view.container.querySelector(".echo-mark--active")).not.toBeNull();
    for (const props of [
      { waitingForModel: false },
      { runtimeError: "模型连接中断" },
      { streaming: false },
      { message: { ...waiting, complete: true } },
    ]) {
      view.rerender(renderMessage(props));
      expect(view.container.querySelector(".echo-mark")).not.toBeNull();
      expect(view.container.querySelector(".echo-mark--active")).toBeNull();
    }
  });

  it("自定义作者保留首字头像", () => {
    const view = render(<ThemeProvider><MessageItem message={waiting} streaming authorName="产品专家" /></ThemeProvider>);
    expect(view.container.querySelector(".msg__avatar")).toHaveTextContent("产");
    expect(view.container.querySelector(".echo-mark")).toBeNull();
  });
});
