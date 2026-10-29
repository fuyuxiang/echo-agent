import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  getChangeSet: vi.fn(),
  captureBaseline: vi.fn(),
  checkpointStart: vi.fn(),
  checkpointFinish: vi.fn(),
  syncChanges: vi.fn(),
  syncPlan: vi.fn(),
  reportImplementation: vi.fn(),
  reportInterrupted: vi.fn(),
  reportStartFailed: vi.fn(),
}));

vi.mock("../lib/tauri-api", () => ({ codingApi: api }));

import { useSessionStore } from "@/stores/session-store";

const refreshTaskState = vi.fn(async () => undefined);
const taskStoreState = {
  task: null as {
    id: string;
    phase: import("../lib/types").TaskPhase;
    sessionId?: string | null;
    phaseReason?: string | null;
    blocker?: string | null;
    strictWorkflow?: boolean;
  } | null,
};

function setTaskStreaming(streaming: boolean) {
  useSessionStore.setState({
    transcripts: {
      s1: {
        messages: [],
        streamingMessageId: streaming ? "assistant-1" : null,
        pendingSendNowPromptId: null,
        usage: {},
        plan: null,
        planMode: false,
        planApprovals: [],
        suppressReplay: false,
        dismissedControlPromptIds: [],
      },
    },
  });
}

function completeTaskTurn(options: {
  stopReason: string;
  cancelTrigger?: string;
  cancellationCategory?: string;
  agentResult?: string;
}) {
  useSessionStore.setState({
    transcripts: {
      s1: {
        messages: [{
          id: "assistant-1",
          promptId: "prompt-1",
          role: "assistant",
          parts: [],
          complete: true,
          ...options,
        }],
        streamingMessageId: null,
        pendingSendNowPromptId: null,
        usage: {},
        plan: null,
        planMode: false,
        planApprovals: [],
        suppressReplay: false,
        dismissedControlPromptIds: [],
      },
    },
  });
}

vi.mock("../store/task-store", () => ({
  useTaskStore: Object.assign(
    (selector: (state: typeof taskStoreState) => unknown) => selector(taskStoreState),
    {
      getState: () => ({ refreshTaskState }),
    },
  ),
}));

import { useTaskLifecycle } from "../lib/task-lifecycle";

beforeEach(() => {
  for (const fn of Object.values(api)) {
    fn.mockReset();
    fn.mockResolvedValue(undefined);
  }
  refreshTaskState.mockClear();
  taskStoreState.task = null;
  setTaskStreaming(false);
});

describe("useTaskLifecycle", () => {
  it("does nothing without a workspace", async () => {
    renderHook(() => useTaskLifecycle(""));
    await waitFor(() => expect(api.getChangeSet).not.toHaveBeenCalled());
  });

  it("does nothing without a task", async () => {
    renderHook(() => useTaskLifecycle("/repo"));
    await waitFor(() => expect(api.getChangeSet).not.toHaveBeenCalled());
  });

  it("never recaptures the task baseline after the Agent has started", async () => {
    taskStoreState.task = { id: "t1", phase: "implementing", sessionId: "s1" };
    renderHook(() => useTaskLifecycle("/repo"));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(api.getChangeSet).not.toHaveBeenCalled();
    expect(api.captureBaseline).not.toHaveBeenCalled();
  });

  it("persists runtime plan contracts while Agent is working", async () => {
    taskStoreState.task = { id: "t1", phase: "discovering", sessionId: "s1", strictWorkflow: true };
    renderHook(() => useTaskLifecycle("/repo"));
    await act(async () => {
      useSessionStore.setState({
        transcripts: {
          s1: {
            messages: [],
            streamingMessageId: "assistant-1",
            pendingSendNowPromptId: null,
            usage: {},
            plan: {
              entries: [{
                content: "[T1] 修改登录\nFiles: src/auth.ts\nAcceptance: 登录成功\nVerify: pnpm test",
                priority: "high",
                status: "in_progress",
              }],
            },
            planMode: false,
            planApprovals: [],
            suppressReplay: false,
            dismissedControlPromptIds: [],
          },
        },
      });
    });

    await waitFor(() => expect(api.syncPlan).toHaveBeenCalledWith(
      "/repo",
      "t1",
      [expect.objectContaining({
        key: "T1",
        writeSet: ["src/auth.ts"],
        acceptanceCriteria: ["登录成功"],
        verificationCommands: ["pnpm test"],
        status: "running",
      })],
    ));
  });

  it("does not capture the baseline outside Implementing", async () => {
    taskStoreState.task = { id: "t1", phase: "verifying" };
    useSessionStore.setState({ streaming: false });
    renderHook(() => useTaskLifecycle("/repo"));
    // The hook never reaches the Git call outside Implementing, so neither
    // getChangeSet nor captureBaseline should fire.
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(api.getChangeSet).not.toHaveBeenCalled();
    expect(api.captureBaseline).not.toHaveBeenCalled();
  });

  it("syncs and reports when streaming falls while Implementing", async () => {
    api.getChangeSet.mockResolvedValue({
      taskId: "t1",
      baselineFiles: ["src/a.ts"],
      changes: [],
      createdAt: "",
      reviewedFiles: [],
    });
    taskStoreState.task = { id: "t1", phase: "implementing", sessionId: "s1" };
    // Initial render with streaming=false — nothing to fire yet.
    setTaskStreaming(false);
    renderHook(() => useTaskLifecycle("/repo"));

    // Simulate a streaming turn finishing.
    await act(async () => {
      setTaskStreaming(true);
    });
    await act(async () => {
      setTaskStreaming(false);
    });

    await waitFor(() => expect(api.syncChanges).toHaveBeenCalledWith("/repo", "t1"));
    await waitFor(() =>
      expect(api.reportImplementation).toHaveBeenCalledWith("/repo", "t1", undefined),
    );
    // Order matters: sync must happen before report.
    const syncOrder = api.syncChanges.mock.invocationCallOrder[0] ?? 0;
    const reportOrder = api.reportImplementation.mock.invocationCallOrder[0] ?? 0;
    expect(syncOrder).toBeLessThan(reportOrder);
    expect(refreshTaskState).toHaveBeenCalled();
  });

  it("records a user stop as recoverable instead of reporting implementation complete", async () => {
    taskStoreState.task = { id: "t1", phase: "implementing", sessionId: "s1" };
    setTaskStreaming(true);
    renderHook(() => useTaskLifecycle("/repo"));

    await act(async () => {
      completeTaskTurn({ stopReason: "cancelled", cancelTrigger: "stop" });
    });

    await waitFor(() => expect(api.reportInterrupted).toHaveBeenCalledWith(
      "/repo",
      "t1",
      "stopped",
    ));
    expect(api.reportImplementation).not.toHaveBeenCalled();
    expect(api.reportStartFailed).not.toHaveBeenCalled();
    expect(api.checkpointStart).toHaveBeenCalledWith("/repo", "t1", "prompt-1");
    expect(api.checkpointFinish).toHaveBeenCalledWith("/repo", "t1", "prompt-1", []);
    expect(api.checkpointStart.mock.invocationCallOrder[0]).toBeLessThan(api.checkpointFinish.mock.invocationCallOrder[0]!);
    expect(api.checkpointFinish.mock.invocationCallOrder[0]).toBeLessThan(api.reportInterrupted.mock.invocationCallOrder[0]!);
  });

  it("records a pause separately and preserves the unfinished round", async () => {
    taskStoreState.task = { id: "t1", phase: "repairing", sessionId: "s1" };
    setTaskStreaming(true);
    renderHook(() => useTaskLifecycle("/repo"));

    await act(async () => {
      completeTaskTurn({ stopReason: "cancelled", cancelTrigger: "pause" });
    });

    await waitFor(() => expect(api.reportInterrupted).toHaveBeenCalledWith(
      "/repo",
      "t1",
      "paused",
    ));
    expect(api.reportImplementation).not.toHaveBeenCalled();
    expect(api.checkpointStart).toHaveBeenCalledWith("/repo", "t1", "prompt-1");
    expect(api.checkpointFinish).toHaveBeenCalledWith("/repo", "t1", "prompt-1", []);
  });

  it("routes runtime failures to an exact blocker without claiming a clean finish", async () => {
    taskStoreState.task = { id: "t1", phase: "implementing", sessionId: "s1" };
    setTaskStreaming(true);
    renderHook(() => useTaskLifecycle("/repo"));

    await act(async () => {
      completeTaskTurn({ stopReason: "error", agentResult: "provider disconnected" });
    });

    await waitFor(() => expect(api.reportStartFailed).toHaveBeenCalledWith(
      "/repo",
      "t1",
      "provider disconnected",
    ));
    expect(api.reportImplementation).not.toHaveBeenCalled();
    expect(api.checkpointStart).toHaveBeenCalledWith("/repo", "t1", "prompt-1");
    expect(api.checkpointFinish).toHaveBeenCalledWith("/repo", "t1", "prompt-1", []);
  });

  it("persists an empty checkpoint for a real read-only turn without tool edits", async () => {
    taskStoreState.task = { id: "t1", phase: "discovering", sessionId: "s1" };
    setTaskStreaming(true);
    renderHook(() => useTaskLifecycle("/repo"));

    await act(async () => completeTaskTurn({ stopReason: "end_turn" }));

    await waitFor(() => expect(api.reportImplementation).toHaveBeenCalledWith("/repo", "t1", "prompt-1"));
    expect(api.checkpointStart).toHaveBeenCalledWith("/repo", "t1", "prompt-1");
    expect(api.checkpointFinish).toHaveBeenCalledWith("/repo", "t1", "prompt-1", []);
    expect(api.checkpointFinish.mock.invocationCallOrder[0]).toBeLessThan(api.syncChanges.mock.invocationCallOrder[0]!);
    expect(api.captureBaseline).not.toHaveBeenCalled();
  });

  it("ignores the cancelled half of a send-now handoff", async () => {
    taskStoreState.task = { id: "t1", phase: "implementing", sessionId: "s1" };
    setTaskStreaming(true);
    renderHook(() => useTaskLifecycle("/repo"));

    await act(async () => {
      completeTaskTurn({ stopReason: "cancelled", cancelTrigger: "send_now" });
    });
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(api.reportInterrupted).not.toHaveBeenCalled();
    expect(api.reportImplementation).not.toHaveBeenCalled();
    expect(api.reportStartFailed).not.toHaveBeenCalled();
  });

  it("repairs the exact legacy no-change blocker after a stopped terminal turn", async () => {
    taskStoreState.task = {
      id: "t1",
      phase: "blocked",
      sessionId: "s1",
      phaseReason: "实现阶段结束但没有代码变更",
      blocker: "Agent 结束了实现但没有写入任何文件。请检查是否只在会话里返回了示例代码。",
    };
    completeTaskTurn({ stopReason: "cancelled", cancelTrigger: "stop" });
    renderHook(() => useTaskLifecycle("/repo"));

    await waitFor(() => expect(api.reportInterrupted).toHaveBeenCalledWith(
      "/repo",
      "t1",
      "stopped",
    ));
  });

  it("skips the sync when the task is not in Implementing", async () => {
    taskStoreState.task = { id: "t1", phase: "verifying" };
    useSessionStore.setState({ streaming: false });
    renderHook(() => useTaskLifecycle("/repo"));

    await act(async () => {
      useSessionStore.setState({ streaming: true });
    });
    await act(async () => {
      useSessionStore.setState({ streaming: false });
    });

    // Give the effect a chance to run.
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(api.syncChanges).not.toHaveBeenCalled();
    expect(api.reportImplementation).not.toHaveBeenCalled();
  });

  it("survives a sync failure without breaking the workbench", async () => {
    api.getChangeSet.mockResolvedValue({
      taskId: "t1",
      baselineFiles: ["src/a.ts"],
      changes: [],
      createdAt: "",
      reviewedFiles: [],
    });
    api.syncChanges.mockRejectedValue(new Error("checkpoint failure"));
    taskStoreState.task = { id: "t1", phase: "implementing", sessionId: "s1" };
    setTaskStreaming(false);
    renderHook(() => useTaskLifecycle("/repo"));

    await act(async () => {
      setTaskStreaming(true);
    });
    await act(async () => {
      setTaskStreaming(false);
    });

    await waitFor(() => expect(api.syncChanges).toHaveBeenCalled());
    expect(api.reportImplementation).not.toHaveBeenCalled();
    await waitFor(() => expect(api.reportStartFailed).toHaveBeenCalled());
  });
});
