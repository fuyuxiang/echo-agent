import { beforeEach, describe, expect, it } from "vitest";
import { useSessionStore } from "../session-store";
import { executionSource, readTurnIdentity, resolveExecutionTurn } from "@/lib/execution-turn";
import { continuationAnchor, continuationTargets, latestContinuationTarget } from "@/lib/session-continuation";
import { isExecutionSettled } from "@/lib/execution-process";
import type { SessionUpdate } from "@/lib/types";

const store = () => useSessionStore.getState();
const transcript = () => store().transcripts.A;
const emit = (sessionUpdate: string, fields: Record<string, unknown> = {}, meta: Record<string, unknown> = {}) => store().applyUpdate({
  sessionUpdate, ...fields, _meta: meta, __sessionId: "A",
} as SessionUpdate);
const user = (promptId: string, index: number, replay = false) => emit("user_message_chunk", { content: { type: "text", text: "排查字体" } },
  { promptId, promptIndex: index, isReplay: replay, turnOrigin: { origin: "user" } });
const answer = (promptId: string, text: string, replay = false) => emit("agent_message_chunk", { content: { type: "text", text } }, { promptId, isReplay: replay });
const complete = (promptId: string, replay = false) => emit("turn_completed", { prompt_id: promptId, stop_reason: "end_turn" }, { isReplay: replay });
const background = (id: string, replay = false) => emit("task_backgrounded", { task_id: id, tool_call_id: "lookup", description: "查找微信数据目录" },
  { eventId: `bg-${id}`, agentTimestampMs: 100, isReplay: replay });
const wake = (id: string, index: number, replay = false) => emit("user_message_chunk", { content: { type: "text", text: "PRIVATE INTERNAL REMINDER" } },
  { promptId: `task-completed-${id}`, promptIndex: index, hideFromScrollback: true, isReplay: replay,
    turnOrigin: { origin: "background_task", sourceId: id } });
const seed = (replay = false) => {
  user("human", 0, replay);
  emit("tool_call", { toolCallId: "lookup", title: "读取目录", kind: "read", status: "completed" }, { promptId: "human", isReplay: replay });
  background("one", replay);
  answer("human", "已取得初步结论。", replay);
  complete("human", replay);
};
beforeEach(() => {
  localStorage.clear();
  useSessionStore.setState({ sessionId: null, transcripts: {}, messages: [], streaming: false, streamingMessageId: null });
  store().setSession("A");
});
const latest = () => transcript().messages[transcript().messages.length - 1];

describe("native execution facts without a background lifecycle shadow", () => {
  it.each([false, true])("live/replay=%s retains hidden boundaries, source and independent checkpoints", replay => {
    seed(replay);
    expect(store().streaming).toBe(false);
    wake("one", 1, replay);
    answer("task-completed-one", "后台检查已返回，结论不变。", replay);
    const main = transcript().messages.find(message => message.promptId === "human" && message.role === "assistant")!;
    const followup = latest();
    expect(transcript().messages.filter(message => message.role === "user")).toHaveLength(1);
    expect(JSON.stringify(transcript().messages)).not.toContain("PRIVATE INTERNAL REMINDER");
    expect(followup.promptIndex).toBe(1);
    expect(resolveExecutionTurn(followup)).toEqual({ origin: "background_task", sourceId: "one" });
    expect(executionSource(followup, transcript().messages)).toEqual({ rootPromptId: "human", description: "查找微信数据目录" });
    expect(continuationTargets(transcript().messages).get(main.id)).toEqual({ promptIndex: 0, promptId: "human" });
    complete("task-completed-one", replay);
    const targets = continuationTargets(transcript().messages);
    expect(targets.get(followup.id)).toEqual({ promptIndex: 1, promptId: "task-completed-one" });
    expect(continuationAnchor(transcript().messages, targets.get(main.id)!)).toBe(main.id);
    expect(latestContinuationTarget(transcript().messages)).toEqual(targets.get(followup.id));
  });

  it("task observations do not admit model work, reopen replies or alter control", () => {
    seed();
    const before = transcript();
    background("one");
    expect(transcript()).toBe(before);
    expect(store().streaming).toBe(false);
    store().requestControl("A", "pause", "human");
    const control = transcript().control;
    emit("task_backgrounded", { task_id: "two", tool_call_id: "lookup" });
    expect(transcript().control).toBe(control);
    expect(store().streaming).toBe(false);
  });

  it("a later full tool update preserves the native source link", () => {
    user("human", 0);
    emit("tool_call", { toolCallId: "lookup", title: "读取目录", status: "in_progress" }, { promptId: "human" });
    background("one");
    emit("tool_call", { toolCallId: "lookup", title: "目录检查已交给后台", status: "completed" }, { promptId: "human" });
    answer("human", "已取得结论"); complete("human");
    wake("one", 1); answer("task-completed-one", "补充");
    expect(executionSource(latest(), transcript().messages)).toEqual({ rootPromptId: "human", description: "查找微信数据目录" });
  });

  it.each([false, true])("source notifications ahead of tool registration are joined once, replay=%s", replay => {
    user("human", 0, replay);
    background("one", replay);
    const pending = transcript();
    background("one", replay);
    expect(transcript()).toBe(pending);
    expect(pending.pendingBackgroundTasks).toHaveLength(1);
    expect(store().streaming).toBe(false);
    store().setSession("B");
    emit("tool_call", { toolCallId: "lookup", title: "读取目录", status: "in_progress" }, { promptId: "human", isReplay: replay });
    emit("tool_call_update", { toolCallId: "lookup", status: "completed" }, { promptId: "human", isReplay: replay });
    answer("human", "主结论", replay); complete("human", replay);
    user("new-human", 1, replay); answer("new-human", "新问题结论", replay); complete("new-human", replay);
    wake("one", 2, replay); answer("task-completed-one", "旧问题补充", replay);
    expect(store().messages).toEqual([]);
    store().setSession("A");
    expect(transcript().pendingBackgroundTasks).toEqual([]);
    expect(executionSource(latest(), transcript().messages)).toEqual({ rootPromptId: "human", description: "查找微信数据目录" });
  });

  it("unmatched source links are session scoped, bounded and immutable", () => {
    for (let index = 0; index < 257; index++) emit("task_backgrounded", { task_id: `task-${index}`, tool_call_id: `tool-${index}` });
    expect(transcript().pendingBackgroundTasks).toHaveLength(256);
    emit("task_backgrounded", { task_id: "conflict", tool_call_id: "tool-256" });
    emit("tool_call", { toolCallId: "tool-256", title: "检查目录", status: "completed" }, { promptId: "human" });
    expect(latest().parts[0]).toMatchObject({ kind: "tool_call", toolCall: { backgroundTask: { id: "task-256" } } });
    expect(transcript().pendingBackgroundTasks).toHaveLength(255);
    store().setSession("B");
    store().applyUpdate({ sessionUpdate: "tool_call", __sessionId: "B", toolCallId: "tool-255", title: "独立工具", status: "completed" } as SessionUpdate);
    expect(store().messages[0].parts[0]).not.toHaveProperty("toolCall.backgroundTask");
  });

  it("a late result remains associated with the old request after a new human request", () => {
    seed(); user("next-human", 1); answer("next-human", "新的结论"); complete("next-human");
    wake("one", 2); answer("task-completed-one", "旧请求的补充");
    expect(executionSource(latest(), transcript().messages).rootPromptId).toBe("human");
    complete("task-completed-one");
    expect([...continuationTargets(transcript().messages).values()].map(target => target.promptIndex)).toEqual([0, 1, 2]);
  });

  it.each([false, true])("admission overtaking the old ACK does not announce old success, replay=%s", replay => {
    user("human", 0, replay); answer("human", "部分结果", replay);
    wake("one", 1, replay);
    const old = transcript().messages.find(message => message.role === "assistant" && message.promptId === "human")!;
    expect(old.complete).toBe(false);
    expect(isExecutionSettled(old)).toBe(false);
    answer("task-completed-one", "继续核对", replay);
    complete("human", replay);
    expect(store().streaming).toBe(true);
    expect(latest().promptId).toBe("task-completed-one");
    expect(isExecutionSettled(transcript().messages.find(message => message.id === old.id)!)).toBe(true);
  });

  it("a newer visible human admission also waits for the preceding terminal ACK", () => {
    user("human", 0); answer("human", "部分结果");
    user("next", 1); answer("next", "新回复");
    expect(transcript().messages.find(m => m.role === "assistant" && m.promptId === "human")?.complete).toBe(false);
    complete("human");
    expect(store().streaming).toBe(true);
    expect(latest().promptId).toBe("next");
  });

  it("subagent source uses the existing runtime record without copying its status", () => {
    seed();
    emit("user_message_chunk", { content: { type: "text", text: "hidden" } }, { promptId: "subagent-completed-child", promptIndex: 1, hideFromScrollback: true });
    answer("subagent-completed-child", "已核对"); complete("subagent-completed-child");
    expect(executionSource(latest(), transcript().messages, [{ id: "child", parentPromptId: "human", description: "验证字体配置", status: "failed" }]))
      .toEqual({ description: "验证字体配置", rootPromptId: "human" });
    expect(latest().execution).toEqual({ origin: "subagent", sourceId: "child" });
  });

  it("duplicate admission and late chunks cannot create a second reply or overwrite a newer turn", () => {
    seed(); wake("one", 1); wake("one", 1); answer("task-completed-one", "补充"); complete("task-completed-one");
    const count = transcript().messages.length;
    wake("one", 1); answer("task-completed-one", "重复");
    expect(transcript().messages).toHaveLength(count);
    expect(store().streaming).toBe(false);
    user("next", 2); answer("next", "新回复");
    wake("older", 1);
    expect(latest().promptId).toBe("next");
  });

  it("background sessions stay isolated while retaining source on return", () => {
    seed(); store().setSession("B");
    wake("one", 1); answer("task-completed-one", "后台补充");
    expect(store().messages).toEqual([]);
    complete("task-completed-one"); store().setSession("A");
    expect(latest().complete).toBe(true);
    expect(executionSource(latest(), transcript().messages).rootPromptId).toBe("human");
  });

  it.each(["notification", "goal", "scheduled", "plan_resume", "runtime"] as const)("structured %s admission keeps its own checkpoint and fresh plan", origin => {
    seed();
    emit("plan", { entries: [{ content: "新阶段", status: "in_progress", priority: "high" }] }, { promptId: "native-next", promptIndex: 1, turnOrigin: { origin } });
    answer("native-next", "进展");
    expect(latest().execution?.origin).toBe(origin);
    expect(latest().promptIndex).toBe(1);
    expect(latest().parts[0].stageTitle).toBe("新阶段");
  });

  it("a native generation without user echo resets the previous stage", () => {
    user("human", 0);
    emit("plan", { entries: [{ content: "上一轮阶段", status: "in_progress", priority: "high" }] }, { promptId: "human" });
    answer("human", "初步结论"); complete("human");
    emit("agent_thought_chunk", { content: { type: "text", text: "分析" } }, { promptId: "notifications-next", promptIndex: 1, turnOrigin: { origin: "notification" } });
    expect(latest().parts[0].stageTitle).toBeUndefined();
    expect(latest().promptIndex).toBe(1);
  });

  it("late structured provenance is retained when subsequent chunks omit it", () => {
    seed(); answer("legacy-wake-id", "补充");
    emit("agent_message_chunk", { content: { type: "text", text: "结果" } }, { promptId: "legacy-wake-id", promptIndex: 1, turnOrigin: { origin: "subagent", sourceId: "child" } });
    answer("legacy-wake-id", "结束");
    expect(latest().execution).toEqual({ origin: "subagent", sourceId: "child" });
  });

  it("invalid checkpoints and unknown sources do not invent human ownership", () => {
    seed();
    expect(readTurnIdentity({ promptIndex: -1, turnOrigin: { origin: "unknown" } }, "notifications-x")).toEqual({ origin: "notification", sourceId: undefined });
    wake("unknown", 1); answer("task-completed-unknown", "补充");
    expect(executionSource(latest(), transcript().messages)).toEqual({ description: undefined });
  });
});
