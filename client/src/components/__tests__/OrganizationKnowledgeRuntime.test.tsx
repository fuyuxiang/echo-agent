import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useSessionStore, type ToolCallView } from "@/stores/session-store";
import { ToolCallCard, ToolCallDetailBody } from "../ToolCallCard";
import { resetOrgSessionMirror, useOrgSessionStore } from "@/stores/org-session-store";

const api = vi.hoisted(() => ({ orgFetchDocument: vi.fn(), orgQaFeedback: vi.fn() }));
vi.mock("@/lib/org-client", () => api);

const answer = {
  answer: "最新口令为红枫-928。", insufficient: false, confidence: 0.95, qaEventId: "qa-live",
  citations: [
    { id: "cit-1", docId: "doc-1", title: "验收规范", quote: "最新口令为红枫-928。" },
    { id: "cit-2", docId: "doc-2", title: "执行流程", quote: "发布前先进行验证。" },
  ],
};

function nativeCompletion(output: unknown = answer, server = "echoagent_organization_memory", tool = "knowledge_ask", native = {}): ToolCallView {
  const store = useSessionStore.getState();
  store.setSession("organization-live");
  store.applyUpdate({ sessionUpdate: "tool_call", __sessionId: "organization-live", toolCallId: "ask-live",
    title: "use_tool", kind: "other", rawInput: { variant: "UseTool",
      tool_name: `echoagent_organization_memory__${tool}`, tool_input: { question: "最新验收口令？" } },
    _meta: { "echo.agent/tool": { version: 1, name: "use_tool" } },
  } as never);
  // Completion may arrive after the user has switched to another task.
  store.setSession("another-task");
  store.applyUpdate({ sessionUpdate: "tool_call_update", __sessionId: "organization-live", toolCallId: "ask-live",
    title: `echoagent_organization_memory__${tool}`, kind: "other", status: "completed",
    rawOutput: { type: "MCP", server_name: server, tool_name: tool,
      output: { OkayOutput: JSON.stringify(output) }, ...native },
  } as never);
  store.setSession("organization-live");
  const part = useSessionStore.getState().messages.flatMap(m => m.parts).find(p => p.kind === "tool_call");
  if (part?.kind !== "tool_call") throw Error("Native tool card missing");
  return part.toolCall;
}

describe("真实组织 MCP 完成事件到用户交互", () => {
  beforeEach(() => {
    resetOrgSessionMirror();
    useSessionStore.setState({ sessionId: null, transcripts: {}, messages: [], streaming: false,
      streamingMessageId: null, usage: {}, error: null });
    api.orgFetchDocument.mockReset().mockResolvedValue({ text: "规范完整原文" });
    api.orgQaFeedback.mockReset().mockResolvedValue({ recorded: true });
  });

  it("未登录组织也可使用本地知识，打开源文件不调用组织接口", () => {
    useOrgSessionStore.getState().setSession({ loggedIn: false });
    const onOpenPath = vi.fn();
    const tc = nativeCompletion({ items: [{ path: "/notes/验收.md", title: "本地验收说明", snippet: "个人资料" }] },
      "echoagent_organization_memory", "local_knowledge_search");
    render(<ToolCallDetailBody tc={tc} onOpenPath={onOpenPath} />);
    fireEvent.click(screen.getByRole("button", { name: /本地验收说明/ }));
    expect(onOpenPath).toHaveBeenCalledWith("/notes/验收.md");
    expect(api.orgFetchDocument).not.toHaveBeenCalled();
    expect(api.orgQaFeedback).not.toHaveBeenCalled();
    expect(screen.queryByText("组织知识")).toBeNull();
  });

  it("组织不可用时普通工具仍保留完成状态和输出", () => {
    useOrgSessionStore.getState().setSession({ loggedIn: false, organizationMemoryEnabled: false });
    const store = useSessionStore.getState();
    store.setSession("ordinary");
    store.applyUpdate({ sessionUpdate: "tool_call", __sessionId: "ordinary", toolCallId: "shell",
      title: "echo hello", kind: "execute", status: "completed",
      content: [{ type: "content", content: { type: "text", text: "hello from local tool" } }],
      rawOutput: { type: "MCP", server_name: "another-server", tool_name: "knowledge_ask", output: { OkayOutput: "other" } },
    } as never);
    const part = useSessionStore.getState().messages.flatMap(message => message.parts).find(part => part.kind === "tool_call");
    if (part?.kind !== "tool_call") throw Error("missing ordinary tool");
    render(<ToolCallDetailBody tc={part.toolCall} />);
    expect(screen.getByText("hello from local tool")).toBeInTheDocument();
    expect(screen.getByText("已完成")).toBeInTheDocument();
    expect(api.orgFetchDocument).not.toHaveBeenCalled();
  });

  it("模型输出截断后，独立界面结果仍可读取引用并提交反馈", async () => {
    const tc = nativeCompletion(answer, "echoagent_organization_memory", "knowledge_ask", {
      output: { OkayOutput: '{"answer":"partial\n\n[MCP output truncated: showing first 20 KB]' },
      ui_result: { ...answer, _displayTruncated: true },
    });
    render(<ToolCallDetailBody tc={tc} />);
    expect(screen.getByRole("note")).toHaveTextContent("当前展示部分内容");
    fireEvent.click(screen.getAllByRole("button", { name: "查看原文" })[0]);
    expect(await screen.findByText("规范完整原文")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "有帮助" }));
    expect(await screen.findByText("感谢反馈，已记录")).toBeInTheDocument();
  });

  it("本地长原文保留打开完整源文件的入口", () => {
    const onOpenPath = vi.fn();
    const tc = nativeCompletion({}, "echoagent_organization_memory", "local_knowledge_fetch", {
      ui_result: { path: "/notes/长文.md", text: "原文摘要…", _displayTruncated: true },
    });
    render(<ToolCallDetailBody tc={tc} onOpenPath={onOpenPath} />);
    expect(screen.getByRole("note")).toHaveTextContent("当前展示部分内容");
    fireEvent.click(screen.getByRole("button", { name: /打开源文件.*\/notes\/长文.md/ }));
    expect(onOpenPath).toHaveBeenCalledWith("/notes/长文.md");
  });

  it("后台完成和切换任务后仍呈现答案、引用与反馈，并保留已提交反馈", async () => {
    const tc = nativeCompletion();
    const { rerender } = render(<><ToolCallCard tc={tc} /><ToolCallDetailBody tc={tc} /></>);
    expect(screen.getByText("最新验收口令？")).toBeInTheDocument();
    expect(screen.getByText("最新口令为红枫-928。", { selector: ".org-tool-result__answer" })).toBeInTheDocument();
    fireEvent.click(screen.getAllByRole("button", { name: "查看原文" })[0]);
    expect(await screen.findByText("规范完整原文")).toBeInTheDocument();
    expect(api.orgFetchDocument).toHaveBeenCalledWith("doc-1", undefined);
    fireEvent.click(screen.getByRole("button", { name: "有帮助" }));
    expect(await screen.findByText("感谢反馈，已记录")).toBeInTheDocument();
    expect(api.orgQaFeedback).toHaveBeenCalledWith("qa-live", "helpful");
    rerender(<><ToolCallCard tc={{ ...tc }} /><ToolCallDetailBody tc={{ ...tc }} /></>);
    expect(screen.getByText("感谢反馈，已记录")).toBeInTheDocument();
  });

  it("重复点击只提交一次反馈，读取引用不会丢失正在提交的反馈", async () => {
    let finish!: (value: unknown) => void;
    api.orgQaFeedback.mockImplementation(() => new Promise(resolve => { finish = resolve; }));
    render(<ToolCallDetailBody tc={nativeCompletion()} />);
    const button = screen.getByRole("button", { name: "有帮助" });
    fireEvent.click(button); fireEvent.click(button);
    expect(button).toBeDisabled();
    fireEvent.click(screen.getAllByRole("button", { name: "查看原文" })[0]);
    await screen.findByText("规范完整原文");
    await act(async () => finish({ recorded: true }));
    expect(await screen.findByText("感谢反馈，已记录")).toBeInTheDocument();
    expect(api.orgQaFeedback).toHaveBeenCalledTimes(1);
  });

  it("后选引用优先，迟到响应不会把用户切回上一个来源", async () => {
    let first!: (value: unknown) => void;
    api.orgFetchDocument.mockImplementation((id: string) => id === "doc-1"
      ? new Promise(resolve => { first = resolve; }) : Promise.resolve({ text: "第二来源原文" }));
    render(<ToolCallDetailBody tc={nativeCompletion()} />);
    const buttons = screen.getAllByRole("button", { name: "查看原文" });
    fireEvent.click(buttons[0]); fireEvent.click(buttons[1]);
    await screen.findByText("第二来源原文");
    await act(async () => first({ text: "迟到的第一来源" }));
    expect(screen.queryByText("迟到的第一来源")).toBeNull();
    expect(screen.getByText("第二来源原文")).toBeInTheDocument();
  });

  it("失败反馈保留重试入口，后续成功才显示已记录", async () => {
    api.orgQaFeedback.mockRejectedValueOnce(new Error("connection lost"));
    render(<ToolCallDetailBody tc={nativeCompletion()} />);
    fireEvent.click(screen.getByRole("button", { name: "有帮助" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("反馈提交失败");
    expect(screen.queryByText("感谢反馈，已记录")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "有帮助" }));
    await waitFor(() => expect(screen.getByText("感谢反馈，已记录")).toBeInTheDocument());
  });

  it("未知 MCP 服务不能伪装成组织结果，原生输出只从组织服务解包", () => {
    const tc = nativeCompletion(answer, "another-server");
    expect(tc.content).toEqual([]);
  });
});
