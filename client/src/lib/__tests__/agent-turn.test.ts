import { beforeEach, describe, expect, it, vi } from "vitest";
import { waitFor } from "@testing-library/react";
import { beginAgentTurn, type AgentTurnSender } from "../agent-turn";
import { useSessionStore } from "@/stores/session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { useMessageQueueStore } from "@/stores/message-queue-store";

function resetStores() {
  useMessageQueueStore.setState({queues:{}});
  useSessionStore.setState({
    sessionId: null,
    transcripts: {},
    messages: [],
    streaming: false,
    sendNowPending: false,
    streamingMessageId: null,
    usage: {},
    plan: null,
    control: undefined,
    error: null,
    planMode: false,
  });
  useSessionsStore.setState({
    independent: [],
    homeCwd: "/tmp",
    currentSessionId: null,
  });
}

describe("beginAgentTurn", () => {
  beforeEach(resetStores);

  it("records each admitted coding intent for the actual run and later correction while preserving other sessions",()=>{
    const store=useSessionStore.getState();
    store.setSession("s1");store.setAgentMode("default","s1");store.setAgentMode("plan","s2");
    const send=vi.fn(()=>new Promise<void>(()=>{})) as AgentTurnSender;
    expect(beginAgentTurn({sessionId:"s1",promptText:"read only",displayText:"read only",promptId:"ask-turn",coding:{mode:"ask",documentToolBridge:true}},send)).toBe(true);
    expect(useSessionStore.getState().transcripts.s1.agentMode).toBe("ask");
    expect(useSessionStore.getState().transcripts.s2.agentMode).toBe("plan");
    store.markComplete({sessionId:"s1",promptId:"ask-turn",stopReason:"end_turn"});
    expect(beginAgentTurn({sessionId:"s1",promptText:"implement",displayText:"implement",promptId:"agent-turn",coding:{mode:"agent",documentToolBridge:true}},send)).toBe(true);
    expect(useSessionStore.getState().agentMode).toBe("default");
    expect(send).toHaveBeenLastCalledWith("s1","implement",[],"implement","agent-turn",{mode:"agent",documentToolBridge:true});
  });

  it("立即接纳用户消息和附件，不等待整轮 ACP 请求完成", () => {
    useSessionStore.getState().setSession("s1");
    let resolveSend!: () => void;
    const send = vi.fn(() => new Promise<void>((resolve) => {
      resolveSend = resolve;
    })) as AgentTurnSender;

    const accepted = beginAgentTurn({
      sessionId: "s1",
      promptText: "模型完整提示",
      displayText: "请优化文档",
      attachments: ["/tmp/数据回流方案.docx"],
    }, send);

    const state = useSessionStore.getState();
    expect(accepted).toBe(true);
    expect(state.streaming).toBe(true);
    expect(state.messages[0]).toMatchObject({
      role: "user",
      attachments: ["/tmp/数据回流方案.docx"],
      agentText: "模型完整提示",
      complete: true,
    });
    expect(state.messages[0].parts).toEqual([
      { kind: "text", text: "请优化文档" },
    ]);
    expect(send).toHaveBeenCalledWith(
      "s1",
      "模型完整提示",
      ["/tmp/数据回流方案.docx"],
      "请优化文档",
      expect.any(String),
    );

    // The unresolved model turn is deliberately left pending until after all
    // immediate assertions: beginAgentTurn itself must already have returned.
    resolveSend();
  });

  it("延迟失败保留已提交的用户附件，并结束等待占位", async () => {
    // Production creates/hydrates the authoritative summary before admitting
    // a turn. A status-only event deliberately cannot invent a cwd and route
    // an unknown id into the inbox.
    useSessionsStore.getState().upsert({ sessionId: "s1", cwd: "/tmp", title: "test" });
    useSessionStore.getState().setSession("s1");
    const send = vi.fn(() => Promise.reject(new Error("附件读取失败"))) as AgentTurnSender;

    beginAgentTurn({
      sessionId: "s1",
      promptText: "请优化",
      displayText: "请优化",
      attachments: ["/tmp/方案.docx"],
    }, send);

    useMessageQueueStore.getState().enqueue("s1", "保留的下一步", ["/tmp/补充.docx"]);
    await waitFor(() => expect(useSessionStore.getState().streaming).toBe(false));
    const state = useSessionStore.getState();
    expect(state.messages).toHaveLength(2);
    expect(state.messages[0].attachments).toEqual(["/tmp/方案.docx"]);
    expect(state.messages[1]).toMatchObject({
      role: "assistant",
      complete: true,
      stopReason: "error",
      agentResult: expect.stringContaining("附件读取失败"),
    });
    expect(state.error).toContain("附件读取失败");
    expect(useSessionsStore.getState().independent[0].status).toBe("failed");
    expect(useMessageQueueStore.getState().getQueue("s1")).toMatchObject([
      {text:"保留的下一步",attachments:["/tmp/补充.docx"],status:"paused"},
    ]);
  });

  it.each(["browser_use", "computer_use", "plan"] as const)("%s 新会话模式先返回时仍显示首轮工具和真实回复", (mode) => {
    const store = useSessionStore.getState();
    // Browser/Computer mode is acknowledged before handleSendNew focuses the
    // new conversation. That creates metadata, but no cached history.
    store.setAgentMode(mode, "browser-session");
    store.setSession("browser-session");
    const send = vi.fn(() => new Promise<void>(() => {})) as AgentTurnSender;
    expect(beginAgentTurn({ sessionId: "browser-session", promptText: "读取网页", displayText: "读取网页", promptId: "first-browser" }, send)).toBe(true);
    store.applyUpdate({ __sessionId: "browser-session", sessionUpdate: "tool_call", toolCallId: "browser-read", title: "browser_snapshot", kind: "read", status: "in_progress" });
    store.applyUpdate({ __sessionId: "browser-session", sessionUpdate: "agent_message_chunk", content: { type: "text", text: "网页读取完成" } });
    store.markComplete({ sessionId: "browser-session", promptId: "first-browser", stopReason: "end_turn" });
    const result = useSessionStore.getState();
    expect(result.agentMode).toBe(mode);
    expect(result.streaming).toBe(false);
    const assistant = result.messages[result.messages.length - 1];
    expect(assistant).toMatchObject({ role: "assistant", complete: true });
    expect(assistant?.parts).toEqual(expect.arrayContaining([
      expect.objectContaining({ kind: "tool_call", toolCall: expect.objectContaining({ toolCallId: "browser-read" }) }),
      expect.objectContaining({ kind: "text", text: "网页读取完成" }),
    ]));
  });

  it("暂停会话的新轮次被原生层拒绝时恢复控制状态", async () => {
    useSessionsStore.getState().upsert({ sessionId: "s1", cwd: "/tmp", title: "test" });
    const store = useSessionStore.getState();
    store.setSession("s1");
    store.startStreaming("s1", "paused-prompt");
    store.requestControl("s1", "pause", "paused-prompt");
    store.confirmControl("s1", "pause");
    const pausedControl = useSessionStore.getState().control;
    expect(pausedControl).toMatchObject({ action: "pause", phase: "paused" });

    const send = vi.fn(() => Promise.reject(new Error("原生会话不可用"))) as AgentTurnSender;
    const accepted = beginAgentTurn({
      sessionId: "s1",
      promptText: "请继续。",
      displayText: "请继续。",
      promptId: "resume-prompt",
    }, send);

    expect(accepted).toBe(true);
    expect(useSessionStore.getState().control).toBeUndefined();
    await waitFor(() => expect(useSessionStore.getState().control).toEqual(pausedControl));
    expect(useSessionStore.getState().streaming).toBe(false);
    expect(useSessionStore.getState().error).toContain("原生会话不可用");
    expect(useSessionsStore.getState().independent[0].status).toBe("paused");
  });

  it("会话已切换时拒绝旧界面的迟到提交", () => {
    useSessionStore.getState().setSession("new-session");
    const send = vi.fn(() => Promise.resolve()) as AgentTurnSender;

    const accepted = beginAgentTurn({
      sessionId: "stale-session",
      promptText: "stale",
      displayText: "stale",
      attachments: ["/tmp/stale.docx"],
    }, send);

    expect(accepted).toBe(false);
    expect(send).not.toHaveBeenCalled();
    expect(useSessionStore.getState().messages).toEqual([]);
  });

  it("回复替换在用户切换会话后仍精确发送到原会话", () => {
    const store = useSessionStore.getState();
    store.setSession("original");
    store.setSession("current");
    const send = vi.fn(() => Promise.resolve()) as AgentTurnSender;

    const accepted = beginAgentTurn({
      sessionId: "original",
      promptText: "original model prompt",
      displayText: "original prompt",
      allowBackgroundSession: true,
    }, send);

    expect(accepted).toBe(true);
    expect(send).toHaveBeenCalledWith(
      "original",
      "original model prompt",
      [],
      "original prompt",
      expect.any(String),
    );
    expect(useSessionStore.getState().sessionId).toBe("current");
    expect(useSessionStore.getState().messages).toEqual([]);
    expect(useSessionStore.getState().transcripts.original.messages).toHaveLength(2);
  });

  it("队列轮次复用认领时的 promptId，并在原生拒绝时精确通知调用方", async () => {
    useSessionsStore.getState().upsert({ sessionId: "s1", cwd: "/tmp", title: "test" });
    useSessionStore.getState().setSession("s1");
    const send = vi.fn(() => Promise.reject(new Error("native rejected"))) as AgentTurnSender;
    const onRejected = vi.fn();

    beginAgentTurn({
      sessionId: "s1",
      promptText: "queued",
      displayText: "queued",
      promptId: "queue-prompt-1",
      onRejected,
    }, send);

    await waitFor(() => expect(onRejected).toHaveBeenCalledWith(
      expect.any(Error),
      "queue-prompt-1",
    ));
    expect(send).toHaveBeenCalledWith(
      "s1",
      "queued",
      [],
      "queued",
      "queue-prompt-1",
    );
  });

  it("prompt_complete 已落地后忽略请求通道的迟到失败", async () => {
    useSessionsStore.getState().upsert({ sessionId: "s1", cwd: "/tmp", title: "test" });
    useSessionStore.getState().setSession("s1");
    let rejectSend!: (error: Error) => void;
    const send = vi.fn(() => new Promise<void>((_resolve, reject) => {
      rejectSend = reject;
    })) as AgentTurnSender;
    const onRejected = vi.fn();

    beginAgentTurn({
      sessionId: "s1",
      promptText: "hello",
      displayText: "hello",
      promptId: "completed-prompt",
      onRejected,
    }, send);
    useSessionStore.getState().markComplete({
      sessionId: "s1",
      promptId: "completed-prompt",
      stopReason: "end_turn",
    });
    rejectSend(new Error("late transport failure"));

    await waitFor(() => expect(send).toHaveBeenCalled());
    await Promise.resolve();
    expect(onRejected).not.toHaveBeenCalled();
    expect(useSessionStore.getState().error).toBeNull();
    const messages = useSessionStore.getState().messages;
    expect(messages[messages.length - 1]).toMatchObject({
      role: "user",
      parts: [{ kind: "text", text: "hello" }],
    });
  });
});
