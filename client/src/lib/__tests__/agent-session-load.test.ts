import { beforeEach, expect, it, vi } from "vitest";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@/lib/knowledge-traces-client", () => ({ hydrateKnowledgeTraces: vi.fn(async () => {}), persistKnowledgeTrace: vi.fn() }));
import { invoke } from "@tauri-apps/api/core";
import { agentLoadSession, agentReloadSession, agentSessionLoadInFlight } from "../agent-client";
import { hydrateKnowledgeTraces } from "../knowledge-traces-client";
import { useSessionStore } from "@/stores/session-store";
import { useSubagentStore } from "@/stores/subagent-store";

beforeEach(() => {
  vi.clearAllMocks();
  useSessionStore.setState(useSessionStore.getInitialState());
  useSubagentStore.setState({ bySession: {} });
});

it("连续打开同一会话共用一次原生回放，所有调用等待同一结果", async () => {
  let finish!: (value: string) => void;
  vi.mocked(invoke).mockImplementationOnce(() => new Promise(resolve => { finish = resolve as typeof finish; }));
  const loads = Array.from({ length: 6 }, () => agentLoadSession("shared-load", "/workspace"));
  expect(loads.every(load => load === loads[0])).toBe(true);
  expect(agentSessionLoadInFlight("shared-load")).toBe(true);
  await Promise.resolve();
  expect(invoke).toHaveBeenCalledExactlyOnceWith("agent_load_session", { sessionId: "shared-load", cwd: "/workspace" });
  finish("model-a");
  expect(await Promise.all(loads)).toEqual(Array(6).fill("model-a"));
  expect(hydrateKnowledgeTraces).toHaveBeenCalledExactlyOnceWith("shared-load");
  expect(agentSessionLoadInFlight("shared-load")).toBe(false);
  expect(useSessionStore.getState().transcripts["shared-load"]).toBeUndefined();
});

it("知识来源仍在恢复时重复打开，不清空已恢复的子任务；下一次新回放再清理", async () => {
  let finishTraces!: () => void;
  vi.mocked(invoke).mockResolvedValue("model-a");
  vi.mocked(hydrateKnowledgeTraces).mockImplementationOnce(() => new Promise(resolve => { finishTraces = resolve; }));
  useSubagentStore.getState().applyEvent({ sessionId: "traces-load", phase: "spawned", subagentId: "stale", description: "过期投影" });
  const first = agentLoadSession("traces-load", "/workspace");
  await vi.waitFor(() => expect(hydrateKnowledgeTraces).toHaveBeenCalled());
  expect(useSubagentStore.getState().getForSession("traces-load")).toEqual([]);
  useSubagentStore.getState().applyEvent({ sessionId: "traces-load", phase: "finished", subagentId: "restored", status: "completed", output: "执行证据" });
  const repeated = agentLoadSession("traces-load", "/workspace");
  expect(repeated).toBe(first);
  finishTraces();
  await repeated;
  expect(useSubagentStore.getState().getForSession("traces-load")).toMatchObject([{ id: "restored", output: "执行证据" }]);
  await agentLoadSession("traces-load", "/workspace");
  expect(useSubagentStore.getState().getForSession("traces-load")).toEqual([]);
  expect(invoke).toHaveBeenCalledTimes(2);
});

it("加载失败后释放请求，下次打开可以重试", async () => {
  vi.mocked(invoke).mockRejectedValueOnce(new Error("read failed")).mockResolvedValueOnce("model-b");
  await expect(agentLoadSession("retry-load", "/workspace")).rejects.toThrow("read failed");
  expect(agentSessionLoadInFlight("retry-load")).toBe(false);
  await expect(agentLoadSession("retry-load", "/workspace")).resolves.toBe("model-b");
  expect(invoke).toHaveBeenCalledTimes(2);
});

it.each([false, true])("回退历史等待旧回放结束后重新读取，旧回放失败=%s", async failOldLoad => {
  const store = useSessionStore.getState();
  store.setSession("reload-history");
  store.pushAssistant("已经失效的旧回复");
  let finish!: (value: string) => void;
  let reject!: (error: Error) => void;
  vi.mocked(invoke)
    .mockImplementationOnce(() => new Promise((resolve, fail) => { finish = resolve as typeof finish; reject = fail; }))
    .mockImplementationOnce(async () => {
      store.applyUpdate({ __sessionId: "reload-history", sessionUpdate: "agent_message_chunk", content: { type: "text", text: "重写后的真实历史" }, _meta: { eventId: "rewritten-event", promptId: "rewritten-prompt", isReplay: true } });
      useSubagentStore.getState().applyEvent({ sessionId: "reload-history", phase: "finished", subagentId: "rewritten-child", status: "completed", output: "最新证据" });
      return "new-model";
    });
  const old = agentLoadSession("reload-history", "/workspace").catch(() => null);
  const reloaded = agentReloadSession("reload-history", "/workspace");
  await Promise.resolve();
  expect(invoke).toHaveBeenCalledTimes(1);
  // Replacing a background projection must not steal the user's current page.
  store.setSession("other");
  if (failOldLoad) reject(new Error("旧快照读取失败")); else finish("old-model");
  await old;
  await expect(reloaded).resolves.toBe("new-model");
  expect(invoke).toHaveBeenCalledTimes(2);
  expect(useSessionStore.getState().sessionId).toBe("other");
  expect(useSessionStore.getState().transcripts["reload-history"].messages).toMatchObject([
    { role: "assistant", parts: [{ kind: "text", text: "重写后的真实历史" }] },
  ]);
  expect(useSubagentStore.getState().getForSession("reload-history")).toMatchObject([{ id: "rewritten-child", output: "最新证据" }]);
  expect(agentSessionLoadInFlight("reload-history")).toBe(false);
});

it("正在加载的会话不会被其他工作目录劫持，不同会话可以独立加载", async () => {
  let finish!: (value: string) => void;
  vi.mocked(invoke).mockImplementationOnce(() => new Promise(resolve => { finish = resolve as typeof finish; })).mockResolvedValueOnce("other-model");
  const first = agentLoadSession("owned-load", "/original");
  await expect(agentLoadSession("owned-load", "/different")).rejects.toThrow("绑定的工作目录");
  await expect(agentLoadSession("other-load", "/different")).resolves.toBe("other-model");
  expect(agentSessionLoadInFlight("owned-load")).toBe(true);
  finish("original-model");
  await first;
});
