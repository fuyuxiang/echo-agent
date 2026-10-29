import { beforeEach, describe, expect, it, vi } from "vitest";

// vi.mock is hoisted, so the mock object must be created inside vi.hoisted.
const api = vi.hoisted(() => ({
  listTasks: vi.fn(),
  createTask: vi.fn(),
  getTask: vi.fn(),
  deleteTask: vi.fn(),
  renameTask: vi.fn(),
  getChangeSet: vi.fn(),
  listVerifications: vi.fn(),
  listProblems: vi.fn(),
  executionLedger: vi.fn(),
  orchestratorState: vi.fn(),
}));

vi.mock("../lib/tauri-api", () => ({ codingApi: api }));

import { useTaskStore } from "../store/task-store";
import type { CodingTask, VerificationRecord } from "../lib/types";

function task(overrides: Partial<CodingTask> = {}): CodingTask {
  return {
    schemaVersion: 2,
    id: "t1",
    name: "重构登录",
    requirement: "改成 OIDC",
    phase: "implementing",
    acceptanceCriteria: [],
    taskNodes: [],
    planIssues: [],
    globalConstraints: [],
    createdAt: "2026-09-11T00:00:00Z",
    updatedAt: "2026-09-11T00:00:00Z",
    ...overrides,
  };
}

function record(overrides: Partial<VerificationRecord> = {}): VerificationRecord {
  return {
    id: "v1",
    taskId: "t1",
    kind: "test",
    command: "pnpm test",
    status: "passed",
    exitCode: 0,
    stdout: "",
    stderr: "",
    durationMs: 10,
    startedAt: "2026-09-11T00:00:00Z",
    finishedAt: "2026-09-11T00:00:01Z",
    structured: true,
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}

describe("task store", () => {
  beforeEach(() => {
    localStorage.clear();
    for (const fn of Object.values(api)) fn.mockReset();
    useTaskStore.setState({ root: "" });
    useTaskStore.getState().reset();
    api.getChangeSet.mockResolvedValue({
      taskId: "t1",
      baselineFiles: [],
      changes: [],
      createdAt: "",
      reviewedFiles: [],
    });
    api.listVerifications.mockResolvedValue([]);
    api.listProblems.mockResolvedValue([]);
    api.executionLedger.mockResolvedValue([]);
    api.orchestratorState.mockResolvedValue({
      task: task(),
      problems: [],
      repairRounds: [],
      changedFileCount: 0,
      maxRepairRounds: 3,
    });
  });

  it("does nothing without a workspace", async () => {
    await useTaskStore.getState().refreshSummaries();
    expect(api.listTasks).not.toHaveBeenCalled();
  });

  it("restores each project's last task once without allowing background refresh to undo a new selection",async()=>{
    api.listTasks.mockImplementation(async(root:string)=>root === "/a" ? [{id:"a1"},{id:"a2"}] : [{id:"b1"}]);
    let current=task();
    api.getTask.mockImplementation(async(_root:string,id:string)=>{current=task({id,sessionId:`session-${id}`});return current;});
    api.orchestratorState.mockImplementation(async()=>({task:current,problems:[],repairRounds:[],changedFileCount:0,maxRepairRounds:3}));
    useTaskStore.getState().setRoot("/a");
    await useTaskStore.getState().selectTask("a1");
    useTaskStore.getState().setRoot("/b");
    await useTaskStore.getState().selectTask("b1");
    useTaskStore.getState().setRoot("/a");
    await useTaskStore.getState().refreshSummaries();
    expect(useTaskStore.getState().task?.sessionId).toBe("session-a1");
    await useTaskStore.getState().selectTask("a2");
    await useTaskStore.getState().refreshSummaries();
    expect(useTaskStore.getState().task?.id).toBe("a2");
    useTaskStore.getState().setRoot("/b");
    await useTaskStore.getState().refreshSummaries();
    expect(useTaskStore.getState().task?.sessionId).toBe("session-b1");
    expect(localStorage.getItem("echo-coding-last-task-v1:/a")).toBe("a2");
  });

  it("loads task summaries for the workspace", async () => {
    api.listTasks.mockResolvedValue([
      { id: "t1", name: "重构登录", phase: "implementing", updatedAt: "" },
    ]);
    useTaskStore.getState().setRoot("/repo");
    await useTaskStore.getState().refreshSummaries();
    expect(useTaskStore.getState().summaries).toHaveLength(1);
  });

  it("creates a task and refreshes the list", async () => {
    api.createTask.mockResolvedValue(task());
    api.listTasks.mockResolvedValue([{ id: "t1", name: "重构登录", phase: "idle", updatedAt: "" }]);
    useTaskStore.getState().setRoot("/repo");
    const created = await useTaskStore.getState().createTask("重构登录", "改成 OIDC");
    expect(created?.id).toBe("t1");
    expect(api.listTasks).toHaveBeenCalled();
  });

  it("surfaces a backend failure instead of throwing", async () => {
    api.listTasks.mockRejectedValue(new Error("读取任务列表失败"));
    useTaskStore.getState().setRoot("/repo");
    await useTaskStore.getState().refreshSummaries();
    expect(useTaskStore.getState().error).toBe("读取任务列表失败");
  });

  it("treats the orchestrator's task copy as authoritative", async () => {
    api.getTask.mockResolvedValue(task({ phase: "idle" }));
    api.orchestratorState.mockResolvedValue({
      task: task({ phase: "verifying" }),
      problems: [],
      repairRounds: [],
      changedFileCount: 2,
      maxRepairRounds: 3,
    });
    useTaskStore.getState().setRoot("/repo");
    await useTaskStore.getState().selectTask("t1");
    // The phase must come from the orchestrator, not the plain task read.
    expect(useTaskStore.getState().task?.phase).toBe("verifying");
    expect(useTaskStore.getState().orchestrator?.changedFileCount).toBe(2);
  });

  it("applies a phase event to both the task and its summary", () => {
    useTaskStore.setState({
      task: task(),
      summaries: [{ id: "t1", name: "重构登录", phase: "implementing", updatedAt: "" }],
    });
    useTaskStore.getState().applyPhase("t1", "blocked");
    expect(useTaskStore.getState().task?.phase).toBe("blocked");
    expect(useTaskStore.getState().summaries[0].phase).toBe("blocked");
  });

  it("ignores a phase event for a different task", () => {
    useTaskStore.setState({ task: task({ id: "t1", phase: "implementing" }) });
    useTaskStore.getState().applyPhase("other", "blocked");
    expect(useTaskStore.getState().task?.phase).toBe("implementing");
  });

  it("appends verification records only for the active task", () => {
    useTaskStore.setState({ task: task(), verifications: [] });
    useTaskStore.getState().applyVerification(record());
    expect(useTaskStore.getState().verifications).toHaveLength(1);
    useTaskStore.getState().applyVerification(record({ id: "v2", taskId: "other" }));
    expect(useTaskStore.getState().verifications).toHaveLength(1);
  });

  it("clears state when the workspace changes", () => {
    useTaskStore.setState({ root: "/repo", task: task(), verifications: [record()] });
    useTaskStore.getState().setRoot("/other");
    expect(useTaskStore.getState().task).toBeNull();
    expect(useTaskStore.getState().verifications).toHaveLength(0);
  });

  it("deleting the active task clears it", async () => {
    api.deleteTask.mockResolvedValue(undefined);
    api.listTasks.mockResolvedValue([]);
    useTaskStore.setState({
      root: "/repo",
      task: task(),
      changeSet: {
        taskId: "t1",
        baselineFiles: [],
        changes: [],
        createdAt: "",
        reviewedFiles: [],
      },
      verifications: [record()],
      problems: [{
        id: "p1",
        kind: "test_failure",
        severity: "error",
        message: "failed",
        fingerprint: "fp",
        sourceCommand: "pnpm test",
      }],
    });
    await useTaskStore.getState().deleteTask("t1");
    expect(useTaskStore.getState().task).toBeNull();
    expect(useTaskStore.getState().changeSet).toBeNull();
    expect(useTaskStore.getState().verifications).toEqual([]);
    expect(useTaskStore.getState().problems).toEqual([]);
  });

  it("删除被后端拒绝时保留当前任务并允许界面重试", async () => {
    api.deleteTask.mockRejectedValue(new Error("任务正在执行或验证，请先停止任务再删除"));
    useTaskStore.setState({ root: "/repo", task: task() });

    await expect(useTaskStore.getState().deleteTask("t1")).rejects.toThrow("请先停止任务再删除");

    expect(useTaskStore.getState().task?.id).toBe("t1");
    expect(useTaskStore.getState().error).toContain("请先停止任务再删除");
  });

  it("开始新任务后忽略之前尚未返回的选择请求", async () => {
    const pending = deferred<CodingTask>();
    api.getTask.mockReturnValue(pending.promise);
    useTaskStore.getState().setRoot("/repo");
    const selecting = useTaskStore.getState().selectTask("t1");
    useTaskStore.getState().reset();
    pending.resolve(task());
    await selecting;
    expect(useTaskStore.getState().task).toBeNull();
    expect(useTaskStore.getState().loading).toBe(false);
  });

  it("切换项目会取消旧项目的加载状态", async () => {
    const pending = deferred<CodingTask>();
    api.getTask.mockReturnValue(pending.promise);
    useTaskStore.getState().setRoot("/repo");
    const selecting = useTaskStore.getState().selectTask("t1");
    useTaskStore.getState().setRoot("/other");
    expect(useTaskStore.getState().loading).toBe(false);
    pending.resolve(task());
    await selecting;
    expect(useTaskStore.getState().task).toBeNull();
  });

  it("任务列表响应乱序时保留最新列表", async () => {
    const pending = deferred<Array<{ id: string; name: string; phase: "idle"; updatedAt: string }>>();
    api.listTasks.mockReturnValueOnce(pending.promise).mockResolvedValueOnce([]);
    useTaskStore.getState().setRoot("/repo");
    const beforeDelete = useTaskStore.getState().refreshSummaries();
    await useTaskStore.getState().refreshSummaries();
    pending.resolve([{ id: "deleted", name: "已删除", phase: "idle", updatedAt: "" }]);
    await beforeDelete;
    expect(useTaskStore.getState().summaries).toEqual([]);
  });

  it("往返切换项目后丢弃第一次访问的旧列表", async () => {
    const pending = deferred<Array<{ id: string; name: string; phase: "idle"; updatedAt: string }>>();
    api.listTasks.mockReturnValueOnce(pending.promise).mockResolvedValueOnce([]);
    useTaskStore.getState().setRoot("/repo");
    const beforeSwitch = useTaskStore.getState().refreshSummaries();
    useTaskStore.getState().setRoot("/other");
    useTaskStore.getState().setRoot("/repo");
    await useTaskStore.getState().refreshSummaries();
    pending.resolve([{ id: "stale", name: "旧访问", phase: "idle", updatedAt: "" }]);
    await beforeSwitch;
    expect(useTaskStore.getState().summaries).toEqual([]);
  });

  it("旧项目创建失败不会污染新项目错误状态", async () => {
    const pending = deferred<CodingTask>();
    api.createTask.mockReturnValue(pending.promise);
    useTaskStore.getState().setRoot("/repo");
    const creating = useTaskStore.getState().createTask("task", "requirement");
    useTaskStore.getState().setRoot("/other");
    pending.reject(new Error("旧项目创建失败"));
    await creating;
    expect(useTaskStore.getState().error).toBeNull();
  });

  it("旧项目重命名的迟到响应不会改写新项目同名任务", async () => {
    const pending = deferred<CodingTask>();
    api.renameTask.mockReturnValue(pending.promise);
    useTaskStore.getState().setRoot("/repo");
    const renaming = useTaskStore.getState().renameTask("t1", "旧项目新名字");
    useTaskStore.getState().setRoot("/other");
    useTaskStore.setState({ task: task({ name: "新项目任务" }) });
    pending.resolve(task({ name: "旧项目新名字" }));
    await renaming;
    expect(useTaskStore.getState().task?.name).toBe("新项目任务");
    expect(api.listTasks).not.toHaveBeenCalled();
  });

  it("删除完成后在途选择不会重新打开已删除任务", async () => {
    const pending = deferred<CodingTask>();
    api.getTask.mockReturnValue(pending.promise);
    api.deleteTask.mockResolvedValue(undefined);
    api.listTasks.mockResolvedValue([]);
    useTaskStore.getState().setRoot("/repo");
    const selecting = useTaskStore.getState().selectTask("t1");
    await useTaskStore.getState().deleteTask("t1");
    pending.resolve(task());
    await selecting;
    expect(useTaskStore.getState().task).toBeNull();
    expect(useTaskStore.getState().loading).toBe(false);
  });
});


describe("explicit task creation ownership", () => {
  it("returns created A identity for recovery without selecting it in B", async () => {
    useTaskStore.getState().setRoot("/a");
    const pending = deferred<CodingTask>();
    api.createTask.mockReturnValueOnce(pending.promise);
    const creating = useTaskStore.getState().createTask("A", "Only A", "/a");
    useTaskStore.getState().setRoot("/b");
    pending.resolve(task({id: "created-a"}));
    expect((await creating)?.id).toBe("created-a");
    expect(api.createTask).toHaveBeenCalledWith("/a", "A", "Only A");
    expect(useTaskStore.getState().root).toBe("/b");
    expect(useTaskStore.getState().task).toBeNull();
  });
  it("creating explicitly in A does not invalidate B's pending selection", async () => {
    useTaskStore.getState().setRoot("/b-select");
    const pendingB = deferred<CodingTask>();
    const selectedB = task({id: "selected-b"});
    api.getTask.mockReturnValueOnce(pendingB.promise);
    api.createTask.mockResolvedValueOnce(task({id: "created-a"}));
    api.orchestratorState.mockResolvedValueOnce({task: selectedB, problems: []});
    const selecting = useTaskStore.getState().selectTask("selected-b");
    await useTaskStore.getState().createTask("A", "Only A", "/a-other");
    pendingB.resolve(selectedB);
    await selecting;
    expect(useTaskStore.getState().root).toBe("/b-select");
    expect(useTaskStore.getState().task?.id).toBe("selected-b");
  });

  it("does not restore over a manual selection whose details are still loading", async()=>{
    vi.clearAllMocks();
    const listed=deferred<any[]>(), chosen=deferred<CodingTask>();
    localStorage.setItem("echo-coding-last-task-v1:/race","saved");
    api.listTasks.mockReturnValue(listed.promise);
    api.getTask.mockReturnValue(chosen.promise);
    api.orchestratorState.mockResolvedValue({task:task({id:"chosen"}),problems:[],repairRounds:[],changedFileCount:0,maxRepairRounds:3});
    useTaskStore.getState().setRoot("/race");
    const listing=useTaskStore.getState().refreshSummaries();
    const selecting=useTaskStore.getState().selectTask("chosen");
    listed.resolve([{id:"saved"},{id:"chosen"}]);
    await listing;
    expect(api.getTask).toHaveBeenCalledTimes(1);
    chosen.resolve(task({id:"chosen"}));await selecting;
    expect(useTaskStore.getState().task?.id).toBe("chosen");
  });

});
