import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { agentCancel, agentSend, agentSendNow, invalidateAgentKnowledgeSourceSync } from "../agent-client";
import { useKnowledgeStore } from "@/stores/knowledge-store";
import { resetOrgSessionMirror, useOrgSessionStore } from "@/stores/org-session-store";
import type { OrgSession } from "../org-client";

const preparation = vi.hoisted(() => ({
  prompt: vi.fn(async (_session: string, text: string) => ({ promptText: text, resultCount: 0, sourceCount: 0 })),
  trace: vi.fn(async () => {}),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../knowledge-context", () => ({ preparePromptWithPersonalKnowledge: preparation.prompt }));
vi.mock("../knowledge-traces-client", () => ({ persistKnowledgeTrace: preparation.trace, hydrateKnowledgeTraces: vi.fn() }));

function account(id = "alice"): OrgSession {
  const user = { id, username: id, displayName: id, role: "member" as const, clearance: 1 };
  return {
    loggedIn: true, organizationMemoryEnabled: true, serverUrl: "https://memory.example.com", user,
    bootstrap: { apiVersion: 1, user, scopes: [{ id: "team", kind: "team", name: "团队" }], policy: {}, serverTime: 1 },
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(accept => { resolve = accept; });
  return { promise, resolve };
}

const attached = { personalSelected: false, organizationSelected: true, personalAttached: false, organizationAttached: true };
beforeEach(() => {
  vi.mocked(invoke).mockReset().mockImplementation(async command => command === "agent_set_knowledge_sources" ? attached : undefined);
  preparation.prompt.mockReset().mockImplementation(async (_session, text) => ({ promptText: text, resultCount: 0, sourceCount: 0 }));
  preparation.trace.mockReset().mockResolvedValue(undefined);
  invalidateAgentKnowledgeSourceSync();
  resetOrgSessionMirror();
  useOrgSessionStore.getState().setSession(account());
  useKnowledgeStore.setState({ defaultSources: [], sessionSources: { task: ["organization"] }, sessionOrganizationScopeIds: {}, turnTraces: {} });
});

it("isolates a native-confirmed memory lookup from the session's organization knowledge connector", async () => {
  const text = "查记忆 global「工作约定」";
  vi.mocked(invoke).mockImplementation(async command => {
    if (command === "agent_is_private_memory_query") return true;
    if (command === "agent_set_knowledge_sources") throw new Error("组织离线");
  });
  await agentSend("task", text, [], text, "lookup");
  expect(invoke).toHaveBeenCalledWith("agent_send", expect.objectContaining({ text, promptId: "lookup" }));
  expect(invoke).not.toHaveBeenCalledWith("agent_set_knowledge_sources", expect.anything());
  expect(preparation.prompt).not.toHaveBeenCalled();
});

it.each(["pause", "stop"] as const)("收回准备中的消息后，知识检索结束也不会提交", async action => {
  const retrieval = deferred<Awaited<ReturnType<typeof preparation.prompt>>>();
  preparation.prompt.mockReturnValueOnce(retrieval.promise);
  const sending = agentSend("task", "待发送", [], "待发送", "preparing");
  await vi.waitFor(() => expect(preparation.prompt).toHaveBeenCalled());
  await agentCancel("task", action, "preparing");
  retrieval.resolve({ promptText: "待发送", resultCount: 0, sourceCount: 0 });
  await sending;
  expect(invoke).not.toHaveBeenCalledWith("agent_send", expect.anything());
});

it("取消尚未提交的立即发送时，只向原生停止被替换的回合", async () => {
  const trace = deferred<void>();
  preparation.trace.mockReturnValueOnce(trace.promise);
  const sending = agentSendNow("task", "替换", [], "替换", "replacement");
  await vi.waitFor(() => expect(preparation.trace).toHaveBeenCalled());
  await agentCancel("task", "stop", "replacement", "previous");
  trace.resolve(undefined);
  await sending;
  expect(invoke).toHaveBeenCalledWith("agent_cancel", {
    sessionId: "task", cancelAction: "stop", promptId: "previous", acceptEndedPrompt: true,
  });
  expect(invoke).not.toHaveBeenCalledWith("agent_send", expect.anything());
});

it("IPC 已发出时，把同一回合和替换来源交给原生仲裁", async () => {
  const native = deferred<void>();
  vi.mocked(invoke).mockImplementation(async command => {
    if (command === "agent_set_knowledge_sources") return attached;
    if (command === "agent_send") return native.promise;
  });
  const sending = agentSendNow("task", "替换", [], "替换", "replacement");
  await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith("agent_send", expect.anything()));
  await agentCancel("task", "pause", "replacement", "previous");
  expect(invoke).toHaveBeenCalledWith("agent_cancel", {
    sessionId: "task", cancelAction: "pause", promptId: "replacement",
    preparingPrompt: true, previousPromptId: "previous",
  });
  native.resolve(undefined);
  await sending;
});

it("暂停立即发送也撤回仍在前端预处理的上一条消息", async () => {
  const trace = deferred<void>();
  preparation.trace.mockReturnValue(trace.promise);
  const first = agentSend("task", "上一条", [], "上一条", "previous");
  const replacement = agentSendNow("task", "替换", [], "替换", "replacement");
  await vi.waitFor(() => expect(preparation.trace).toHaveBeenCalledTimes(2));
  await agentCancel("task", "pause", "replacement", "previous");
  trace.resolve(undefined);
  await Promise.all([first, replacement]);
  expect(invoke).not.toHaveBeenCalledWith("agent_send", expect.anything());
});

describe.each([["普通发送", agentSend], ["立即发送", agentSendNow]] as const)("%s 的组织身份边界", (_label, send) => {
  it.each(["sources", "retrieval", "trace"])("%s 等待期间换号，不提交原生消息", async stage => {
    const gate = deferred<void>();
    let entered = false;
    if (stage === "sources") vi.mocked(invoke).mockImplementation(async command => {
      if (command !== "agent_set_knowledge_sources") return;
      entered = true;
      await gate.promise;
      return attached;
    });
    else if (stage === "retrieval") preparation.prompt.mockImplementationOnce(async () => {
      entered = true;
      await gate.promise;
      return { promptText: "原账号准备的提示", resultCount: 1, sourceCount: 1 };
    });
    else preparation.trace.mockImplementationOnce(async () => { entered = true; await gate.promise; });

    const pending = send("task", "原账号的任务", [], "原账号的任务", "turn");
    const rejected = expect(pending).rejects.toThrow(/组织账号/);
    await vi.waitFor(() => expect(entered).toBe(true));
    useOrgSessionStore.getState().setSession(account("bob"));
    gate.resolve();
    await rejected;
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "agent_send")).toBe(false);
    expect(useKnowledgeStore.getState().turnTraces.task.turn.organization).toMatchObject({ state: "unavailable" });
  });

  it.each(["logout", "permissions", "switch-back"])("准备期间 %s 后不能沿用原授权", async change => {
    const gate = deferred<void>();
    preparation.trace.mockReturnValueOnce(gate.promise);
    const pending = send("task", "检查组织知识", [], "检查组织知识", "turn");
    const rejected = expect(pending).rejects.toThrow("组织账号或权限已改变");
    await vi.waitFor(() => expect(preparation.trace).toHaveBeenCalled());
    if (change === "logout") useOrgSessionStore.getState().clearSession();
    else if (change === "permissions") {
      const current = account();
      useOrgSessionStore.getState().setSession({ ...current, bootstrap: { ...current.bootstrap!, scopes: [] } });
    } else {
      useOrgSessionStore.getState().setSession(account("bob"));
      useOrgSessionStore.getState().setSession(account());
    }
    gate.resolve();
    await rejected;
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "agent_send")).toBe(false);
  });

  it("同一账号正常续期仍能完成发送", async () => {
    const gate = deferred<void>();
    preparation.trace.mockReturnValueOnce(gate.promise);
    const pending = send("task", "检查组织知识", [], "检查组织知识", "turn");
    await vi.waitFor(() => expect(preparation.trace).toHaveBeenCalled());
    const current = account();
    useOrgSessionStore.getState().setSession({ ...current, bootstrap: { ...current.bootstrap!, policy: { issuedAt: 10, expiresAt: 20 } } });
    gate.resolve();
    await pending;
    expect(invoke).toHaveBeenCalledWith("agent_send", expect.objectContaining({ sessionId: "task", promptId: "turn" }));
  });
});


it("普通本地任务在准备期间退出组织也能发送", async () => {
  useKnowledgeStore.getState().setSessionSources("task", []);
  vi.mocked(invoke).mockResolvedValue({ personalSelected: false, organizationSelected: false, personalAttached: false, organizationAttached: false });
  const gate = deferred<void>();
  preparation.trace.mockReturnValueOnce(gate.promise);
  const pending = agentSend("task", "本地任务", [], "本地任务", "turn");
  await vi.waitFor(() => expect(preparation.trace).toHaveBeenCalled());
  useOrgSessionStore.getState().clearSession();
  gate.resolve();
  await pending;
  expect(invoke).toHaveBeenCalledWith("agent_send", expect.objectContaining({ sessionId: "task" }));
});
