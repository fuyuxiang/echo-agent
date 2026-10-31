import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useSessionStore } from "../session-store";

const store = () => useSessionStore.getState();
const activity = (sid = "progress-a") => store().transcripts[sid].messages.find(message => message.id === store().transcripts[sid].streamingMessageId)?.lastActivityAt;

describe("live execution activity receipts", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000);
    localStorage.removeItem("echoagent.session-controls.v1");
    useSessionStore.setState({ sessionId: null, transcripts: {}, messages: [], streaming: false, streamingMessageId: null, sendNowPending: false });
    store().setSession("progress-a");
    store().startStreaming("progress-a", "live-a");
  });
  afterEach(() => vi.useRealTimers());

  it("records accepted text, tool results and plan events, ignoring empty chunks and usage", () => {
    vi.setSystemTime(2_000);
    store().applyUpdate({ sessionUpdate: "agent_thought_chunk", content: { type: "text", text: "检查资料" } });
    expect(activity()).toBe(2_000);
    vi.setSystemTime(3_000);
    store().applyUpdate({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "" } });
    store().applyUpdate({ sessionUpdate: "usage_update", usage: { promptTokens: 20 } });
    expect(activity()).toBe(2_000);
    store().applyUpdate({ sessionUpdate: "tool_call", toolCallId: "read", kind: "read", title: "读取资料", status: "in_progress" });
    expect(activity()).toBe(3_000);
    vi.setSystemTime(4_000);
    store().applyUpdate({ sessionUpdate: "tool_call_update", toolCallId: "read", status: "completed" });
    expect(activity()).toBe(4_000);
    vi.setSystemTime(5_000);
    store().applyUpdate({ sessionUpdate: "plan", entries: [{ content: "整理结果", status: "in_progress", priority: "high" }] });
    expect(activity()).toBe(5_000);
  });

  it("session switches preserve receipt times and background updates belong to their own task", () => {
    store().applyUpdate({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "核对资料" } });
    store().setSession("progress-b");
    vi.setSystemTime(8_000);
    store().applyUpdate({ sessionUpdate: "agent_message_chunk", __sessionId: "progress-a", content: { type: "text", text: "继续核对" } });
    expect(activity()).toBe(8_000);
    expect(store().messages).toHaveLength(0);
    vi.setSystemTime(15_000);
    store().setSession("progress-a");
    expect(activity()).toBe(8_000);
  });

  it("history replay never becomes a fresh live progress receipt", () => {
    store().applyUpdate({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "实时资料" } });
    vi.setSystemTime(9_000);
    store().applyUpdate({ sessionUpdate: "agent_thought_chunk", content: { type: "text", text: "回放记录" }, _meta: { isReplay: true } });
    expect(activity()).toBe(1_000);
  });

  it("late results for an older turn do not reset the new turn's wait", () => {
    store().applyUpdate({ sessionUpdate: "tool_call", toolCallId: "old-read", kind: "read", title: "旧操作", status: "in_progress" });
    store().markComplete({ sessionId: "progress-a", promptId: "live-a", stopReason: "end_turn" });
    store().startStreaming("progress-a", "live-b");
    vi.setSystemTime(2_000);
    store().applyUpdate({ sessionUpdate: "agent_thought_chunk", content: { type: "text", text: "新任务" }, _meta: { promptId: "live-b" } });
    vi.setSystemTime(10_000);
    store().applyUpdate({ sessionUpdate: "tool_call_update", toolCallId: "old-read", status: "completed", _meta: { promptId: "live-a" } });
    expect(activity()).toBe(2_000);
  });
});
