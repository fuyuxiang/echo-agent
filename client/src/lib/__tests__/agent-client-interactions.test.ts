import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import {
  agentListPendingInteractions,
  agentListAllSessions,
  agentListSessions,
  agentNewSession,
  agentPrepareSend,
  invalidateAgentKnowledgeSourceSync,
  agentResolvePlanApproval,
  agentResolveQuestion,
  automationsRun,
  folderTrustRespond,
  permissionModeGet,
  permissionModeSet,
  rewindExecute,
  setPlanMode,
  taskKill,
  tasksList,
} from "../agent-client";
import { useKnowledgeStore } from "@/stores/knowledge-store";

const invokeMock = vi.mocked(invoke);

describe("agent interaction command contracts", () => {
  beforeEach(() => invokeMock.mockReset());

  it.each([
    {status:"missing",loaded:true,message:"原目录已丢失"},
    {status:"unavailable",loaded:true,message:"原目录暂不可访问"},
    {status:"ready",loaded:false},
    null,
  ])("回复替换预检拒绝不可执行的工作目录：%j", async workspace => {
    invalidateAgentKnowledgeSourceSync();
    useKnowledgeStore.setState({defaultSources:[],sessionSources:{},sessionOrganizationScopeIds:{}});
    invokeMock.mockImplementation(async command => command === "agent_session_workspace_status" ? workspace : {
      personalSelected:false,organizationSelected:false,personalAttached:false,organizationAttached:false,
    });
    await expect(agentPrepareSend("retry-session", "/original")).rejects.toThrow();
    expect(invokeMock).toHaveBeenLastCalledWith("agent_session_workspace_status", {sessionId:"retry-session",cwd:"/original"});
    expect(invokeMock.mock.calls.some(([command]) => command === "rewind_execute" || command === "agent_send")).toBe(false);
  });

  it("工作目录和 Runtime 均已确认才允许替换回复", async () => {
    invalidateAgentKnowledgeSourceSync();
    useKnowledgeStore.setState({defaultSources:[],sessionSources:{},sessionOrganizationScopeIds:{}});
    invokeMock.mockImplementation(async command => command === "agent_session_workspace_status" ? {status:"ready",loaded:true,cwd:"/original"} : {
      personalSelected:false,organizationSelected:false,personalAttached:false,organizationAttached:false,
    });
    await expect(agentPrepareSend("retry-session", "/original")).resolves.toBeUndefined();
  });

  it("passes exact typed question and plan outcomes", async () => {
    invokeMock.mockResolvedValue(true);
    await expect(
      agentResolveQuestion("question-1", {
        outcome: "skip_interview",
        partialAnswers: { Question: "Choice" },
      }),
    ).resolves.toBe(true);
    await agentResolvePlanApproval("plan-1", "cancelled", "revise this");

    expect(invokeMock).toHaveBeenNthCalledWith(1, "agent_resolve_question", {
      requestId: "question-1",
      answers: null,
      annotations: null,
      partialAnswers: { Question: "Choice" },
      outcome: "skip_interview",
      cancelled: false,
    });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "agent_resolve_plan_approval", {
      requestId: "plan-1",
      outcome: "cancelled",
      feedback: "revise this",
    });
  });

  it("uses request identities for trust and pending replay", async () => {
    invokeMock.mockResolvedValueOnce(true).mockResolvedValueOnce({
      permissions: [],
      questions: [],
      planApprovals: [],
      folderTrustRequests: [],
    });
    await folderTrustRespond("trust-1", false);
    await agentListPendingInteractions("session-1");

    expect(invokeMock).toHaveBeenNthCalledWith(1, "folder_trust_respond", {
      requestId: "trust-1",
      trusted: false,
    });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "agent_list_pending_interactions", {
      sessionId: "session-1",
    });
  });

  it("keeps mode setting idempotent and archived listing opt-in", async () => {
    invokeMock.mockResolvedValue(undefined);
    await setPlanMode("session-1", true);
    await agentListAllSessions(true);
    await agentListSessions("/repo");
    await agentListSessions("/repo", true);

    expect(invokeMock).toHaveBeenNthCalledWith(1, "set_plan_mode", {
      sessionId: "session-1",
      enabled: true,
    });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "agent_list_all_sessions", {
      includeArchived: true,
    });
    expect(invokeMock).toHaveBeenNthCalledWith(3, "agent_list_sessions", {
      cwd: "/repo",
      includeArchived: false,
    });
    expect(invokeMock).toHaveBeenNthCalledWith(4, "agent_list_sessions", {
      cwd: "/repo",
      includeArchived: true,
    });
  });

  it("returns the permission-mode synchronization result", async () => {
    invokeMock.mockResolvedValue({
      sessionId: "session-1",
      permissionMode: "always-approve",
      agentRunning: true,
      runtimeSynced: true,
      resolvedPending: 2,
      remainingPending: 0,
      resolvedPermissions: [
        { requestId: "permission-1", sessionId: "session-1" },
        { requestId: "permission-2", sessionId: "session-1" },
      ],
    });

    await expect(permissionModeSet("session-1", "always-approve")).resolves.toEqual({
      sessionId: "session-1",
      permissionMode: "always-approve",
      agentRunning: true,
      runtimeSynced: true,
      resolvedPending: 2,
      remainingPending: 0,
      resolvedPermissions: [
        { requestId: "permission-1", sessionId: "session-1" },
        { requestId: "permission-2", sessionId: "session-1" },
      ],
    });
    expect(invokeMock).toHaveBeenCalledWith("permission_mode_set", {
      sessionId: "session-1",
      mode: "always-approve",
    });
  });

  it("returns effective permission mode capability instead of only persisted text", async () => {
    invokeMock.mockResolvedValue({
      permissionMode: "ask",
      configuredPermissionMode: "auto",
      autoModeAvailable: false,
      autoModeUnavailableReason: "disabled by policy",
    });

    await expect(permissionModeGet("session-1")).resolves.toEqual({
      permissionMode: "ask",
      configuredPermissionMode: "auto",
      autoModeAvailable: false,
      autoModeUnavailableReason: "disabled by policy",
    });
    expect(invokeMock).toHaveBeenCalledWith("permission_mode_get", {
      sessionId: "session-1",
    });
  });

  it("passes the new task's explicit permission draft at creation", async () => {
    invokeMock.mockResolvedValue("session-1");

    await expect(agentNewSession("/repo", "model-1", "auto"))
      .resolves.toBe("session-1");
    expect(invokeMock).toHaveBeenCalledWith("agent_new_session", {
      cwd: "/repo",
      modelId: "model-1",
      permissionMode: "auto",
    });
  });

  it("passes the project permission preview separately from a requested mode", async () => {
    invokeMock.mockResolvedValue("session-1");

    await expect(agentNewSession("/repo", "model-1", undefined, "auto")).resolves.toBe("session-1");
    expect(invokeMock).toHaveBeenCalledWith("agent_new_session", {
      cwd: "/repo",
      modelId: "model-1",
      permissionMode: null,
      expectedPermissionMode: "auto",
    });
  });

  it("returns the durable automation run record id", async () => {
    invokeMock.mockResolvedValue("run-123");
    await expect(automationsRun("automation-1")).resolves.toBe("run-123");
  });

  it("发送 Runtime 规范的回溯模式值", async () => {
    invokeMock.mockResolvedValue({ targetPromptIndex: 4, promptText: "执行任务" });

    await rewindExecute("session-1", 4, "conversation_only", true);

    expect(invokeMock).toHaveBeenCalledWith("rewind_execute", {
      sessionId: "session-1",
      targetPromptIndex: 4,
      mode: "conversation_only",
      force: true,
    });
  });

  it("scopes running-task queries and kills to the selected session", async () => {
    invokeMock.mockResolvedValue([]);

    await tasksList("session-1");
    await taskKill("session-1", "task-7", "task");
    await taskKill("session-1", "subagent-8", "subagent");

    expect(invokeMock).toHaveBeenNthCalledWith(1, "tasks_list", {
      sessionId: "session-1",
    });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "task_kill", {
      sessionId: "session-1",
      taskId: "task-7",
      source: "task",
    });
    expect(invokeMock).toHaveBeenNthCalledWith(3, "task_kill", {
      sessionId: "session-1",
      taskId: "subagent-8",
      source: "subagent",
    });
  });
});
