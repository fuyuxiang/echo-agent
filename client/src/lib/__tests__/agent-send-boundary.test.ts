import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { agentSend, agentSendNow, invalidateAgentKnowledgeSourceSync } from "../agent-client";
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

it("Code 检查点等待期间换号，尚未同步组织知识也必须停止", async () => {
  const checkpoint = deferred<void>();
  vi.mocked(invoke).mockImplementation(async command => command === "coding_checkpoint_start" ? checkpoint.promise : attached);
  const pending = agentSend("task", "实现需求", [], "实现需求", "turn", { workspace: "/repo", taskId: "coding-task" });
  const rejected = expect(pending).rejects.toThrow("组织账号或权限已改变");
  useOrgSessionStore.getState().setSession(account("bob"));
  checkpoint.resolve();
  await rejected;
  expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual(["coding_checkpoint_start"]);
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
