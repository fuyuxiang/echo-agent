import { beforeEach, expect, it, vi } from "vitest";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("../tauri-kb-reader", async original => ({
  ...await original<object>(), isTauriAvailable: () => true,
}));
import { invoke } from "@tauri-apps/api/core";
import { agentDeleteSession, agentLoadSession, agentRecoverSession, agentReloadSession, agentSessionLoadInFlight } from "../agent-client";
import { restoreKnowledgeTraces } from "../knowledge-traces-client";
import { useKnowledgeStore } from "@/stores/knowledge-store";
import { useSessionStore } from "@/stores/session-store";
import { useSubagentStore } from "@/stores/subagent-store";

beforeEach(() => {
  vi.mocked(invoke).mockReset().mockResolvedValue(null);
  useSessionStore.setState(useSessionStore.getInitialState());
  useSubagentStore.setState({ bySession: {} });
  useKnowledgeStore.setState({ turnTraces: {}, traceLoadErrors: {} });
});

const attachment = (modelId: string) => ({ modelId, activePromptId: null });
const nativeLoads = () => vi.mocked(invoke).mock.calls.filter(([command]) => command === "agent_recover_session");

it("连续打开同一会话共用一次原生回放，所有调用等待同一结果", async () => {
  let finish!: (value: ReturnType<typeof attachment>) => void;
  vi.mocked(invoke).mockImplementationOnce(() => new Promise(resolve => { finish = resolve as typeof finish; }));
  const loads = Array.from({ length: 6 }, () => agentLoadSession("shared-load", "/workspace"));
  expect(loads.every(load => load === loads[0])).toBe(true);
  expect(agentSessionLoadInFlight("shared-load")).toBe(true);
  await Promise.resolve();
  expect(invoke).toHaveBeenCalledExactlyOnceWith("agent_recover_session", { sessionId: "shared-load", cwd: "/workspace" });
  finish(attachment("model-a"));
  expect(await Promise.all(loads)).toEqual(Array(6).fill("model-a"));
  expect(invoke).toHaveBeenCalledWith("knowledge_traces_load", { sessionId: "shared-load", cursor: null });
  expect(agentSessionLoadInFlight("shared-load")).toBe(false);
  expect(useSessionStore.getState().transcripts["shared-load"]).toBeUndefined();
});

it("加载失败后释放请求，下次打开可以重试", async () => {
  vi.mocked(invoke).mockRejectedValueOnce(new Error("read failed")).mockResolvedValueOnce(attachment("model-b"));
  await expect(agentLoadSession("retry-load", "/workspace")).rejects.toThrow("read failed");
  expect(agentSessionLoadInFlight("retry-load")).toBe(false);
  await expect(agentLoadSession("retry-load", "/workspace")).resolves.toBe("model-b");
  expect(nativeLoads()).toHaveLength(2);
});

it("恢复加入正在进行的普通加载时仍能接收本轮活动身份", async () => {
  const sessionId = "recover-during-load";
  vi.mocked(invoke).mockImplementation(async command => {
    if (command === "agent_recover_session") return { modelId: "model-a", activePromptId: "running-turn" };
    return null;
  });
  const load = agentLoadSession(sessionId, "/workspace");
  await Promise.resolve();
  const recovery = agentRecoverSession(sessionId, "/workspace");
  await expect(load).resolves.toBe("model-a");
  await expect(recovery).resolves.toMatchObject({ modelId: "model-a", activePromptId: "running-turn" });
  expect(nativeLoads()).toHaveLength(1);
});

it("历史引用读取期间重开会话会读取当前模型，引用继续使用同一请求", async () => {
  let finishTraces!: (value: null) => void;
  const id = "reopen-with-pending-citations";
  let model = "model-a";
  vi.mocked(invoke).mockImplementation(async command => command === "knowledge_traces_load"
    ? new Promise(resolve => { finishTraces = resolve; }) : attachment(model));
  await expect(agentLoadSession(id, "/workspace")).resolves.toBe("model-a");
  expect(agentSessionLoadInFlight(id)).toBe(false);
  const traces = restoreKnowledgeTraces(id);
  model = "model-b";
  await expect(agentLoadSession(id, "/workspace")).resolves.toBe("model-b");
  expect(nativeLoads()).toHaveLength(2);
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "knowledge_traces_load")).toHaveLength(1);
  finishTraces(null);
  await traces;
});

it("恢复中断后保留已显示历史，重试时读取完整历史", async () => {
  const id = "partial-replay";
  const store = useSessionStore.getState();
  store.setSession(id);
  const replay = (text: string, eventId: string) => store.applyUpdate({ __sessionId: id,
    sessionUpdate: "agent_message_chunk", content: { type: "text", text },
    _meta: { eventId, promptId: "turn", isReplay: true } });
  vi.mocked(invoke).mockImplementationOnce(async () => {
    replay("已显示", "first");
    throw new Error("历史回放中断");
  }).mockImplementationOnce(async () => {
    replay("已显示", "first");
    replay("余下历史", "second");
    return attachment("model-a");
  });
  await expect(agentLoadSession(id, "/workspace")).rejects.toThrow("历史回放中断");
  expect(useSessionStore.getState().messages).toMatchObject([{ parts: [{ text: "已显示" }] }]);
  store.setSession(id);
  await agentLoadSession(id, "/workspace");
  expect(useSessionStore.getState().messages).toMatchObject([{ parts: [{ text: "已显示余下历史" }] }]);
});

it("失效的增量游标会用完整回放重建无活动的会话缓存", async () => {
  const id = "expired-cursor";
  const store = useSessionStore.getState();
  store.setSession(id);
  const reply = (text: string, promptId: string, isReplay = false) => store.applyUpdate({
    __sessionId: id, sessionUpdate: "agent_message_chunk", content: { type: "text", text },
    _meta: { promptId, isReplay },
  });
  reply("旧分支", "old");
  store.applyUpdate({
    __sessionId: id, sessionUpdate: "turn_completed", prompt_id: "old", stop_reason: "end_turn",
    _meta: { eventId: "expired-cursor-29" },
  });
  let attempts = 0;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command !== "agent_recover_session") return null;
    reply("新分支", "new", true);
    return {
      modelId: "model-a",
      activePromptId: null,
      ...(attempts++ === 0 ? { cursorRejected: true } : {}),
    };
  });
  await expect(agentRecoverSession(id, "/workspace")).resolves.toMatchObject({ modelId: "model-a", activePromptId: null });
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "agent_recover_session")).toEqual([
    ["agent_recover_session", { sessionId: id, cwd: "/workspace", cursor: "expired-cursor-29" }],
    ["agent_recover_session", { sessionId: id, cwd: "/workspace" }],
  ]);
  expect(useSessionStore.getState().messages).toMatchObject([{ parts: [{ text: "新分支" }] }]);
});

it.each([false, true])("回退历史等待旧回放结束后重新读取，旧回放失败=%s", async failOldLoad => {
  const store = useSessionStore.getState();
  store.setSession("reload-history");
  store.pushAssistant("已经失效的旧回复");
  let finish!: (value: ReturnType<typeof attachment>) => void;
  let reject!: (error: Error) => void;
  let firstLoad = true;
  vi.mocked(invoke).mockImplementation(async command => {
    if (command !== "agent_recover_session") return null;
    if (firstLoad) {
      firstLoad = false;
      return new Promise((resolve, fail) => { finish = resolve as typeof finish; reject = fail; });
    }
    store.applyUpdate({ __sessionId: "reload-history", sessionUpdate: "agent_message_chunk", content: { type: "text", text: "重写后的真实历史" }, _meta: { eventId: "rewritten-event", promptId: "rewritten-prompt", isReplay: true } });
    useSubagentStore.getState().applyEvent({ sessionId: "reload-history", phase: "finished", subagentId: "rewritten-child", status: "completed", output: "最新证据" });
    return attachment("new-model");
  });
  const old = agentLoadSession("reload-history", "/workspace").catch(() => null);
  const reloaded = agentReloadSession("reload-history", "/workspace");
  await Promise.resolve();
  expect(nativeLoads()).toHaveLength(1);
  // Replacing a background projection must not steal the user's current page.
  store.setSession("other");
  if (failOldLoad) reject(new Error("旧快照读取失败")); else finish(attachment("old-model"));
  await old;
  await expect(reloaded).resolves.toBe("new-model");
  expect(nativeLoads()).toHaveLength(2);
  expect(useSessionStore.getState().sessionId).toBe("other");
  expect(useSessionStore.getState().transcripts["reload-history"].messages).toMatchObject([
    { role: "assistant", parts: [{ kind: "text", text: "重写后的真实历史" }] },
  ]);
  expect(useSubagentStore.getState().getForSession("reload-history")).toMatchObject([{ id: "rewritten-child", output: "最新证据" }]);
  expect(agentSessionLoadInFlight("reload-history")).toBe(false);
});

it.each(["回退", "删除"])("%s后丢弃迟到的历史来源结果", async (operation) => {
  let finishOldPage!: (value: unknown) => void;
  let firstPage = true;
  const trace = { selectedSources: ["personal"] };
  vi.mocked(invoke).mockImplementation(async command => {
    if (command === "agent_recover_session") return attachment("model-a");
    if (command === "agent_delete_session") return { memorySummariesDeleted: 0 };
    if (firstPage) {
      firstPage = false;
      return new Promise(resolve => { finishOldPage = resolve; });
    }
    return { traces: { current: trace }, nextCursor: null };
  });
  const id = `trace-invalidation-${operation}`;
  await agentLoadSession(id, "/workspace");
  const oldTraces = restoreKnowledgeTraces(id);
  await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith("knowledge_traces_load", { sessionId: id, cursor: null }));
  if (operation === "回退") {
    await agentReloadSession(id, "/workspace");
    await restoreKnowledgeTraces(id);
  } else {
    await agentDeleteSession(id, "/workspace");
  }
  finishOldPage({ traces: { stale: trace }, nextCursor: null });
  await oldTraces;
  expect(useKnowledgeStore.getState().turnTraces[id]).toEqual(operation === "回退" ? { current: trace } : undefined);
});

it("正在加载的会话不会被其他工作目录劫持，不同会话可以独立加载", async () => {
  let finish!: (value: ReturnType<typeof attachment>) => void;
  vi.mocked(invoke).mockImplementationOnce(() => new Promise(resolve => { finish = resolve as typeof finish; })).mockResolvedValueOnce(attachment("other-model"));
  const first = agentLoadSession("owned-load", "/original");
  await expect(agentLoadSession("owned-load", "/different")).rejects.toThrow("绑定的工作目录");
  await expect(agentLoadSession("other-load", "/different")).resolves.toBe("other-model");
  expect(agentSessionLoadInFlight("owned-load")).toBe(true);
  finish(attachment("original-model"));
  await first;
});
