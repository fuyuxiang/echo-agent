import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { useKanbanStore } from "./kanban";
import * as api from "../lib/api";
import type { TaskCard } from "./kanban";

function task(id: string, overrides: Partial<TaskCard> = {}): TaskCard {
  return {
    id,
    title: id,
    description: "",
    status: "pending",
    priority: 5,
    labels: [],
    assignee: "",
    source: "human",
    session_id: "",
    blocked_reason: "",
    review_summary: "",
    result: "",
    error: "",
    retry_count: 0,
    max_retries: 3,
    created_at: "2026-01-01T00:00:00",
    updated_at: "2026-01-01T00:00:00",
    ...overrides,
  };
}

beforeEach(() => {
  useKanbanStore.setState({ tasks: [], loading: false, loaded: false });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("fetchTasks 合并服务端快照", () => {
  it("不冲掉请求飞行期间 WS 写入的新任务", async () => {
    // 请求发出后、resolve 之前,WS 推来一个快照里还不存在的任务。
    let release: (value: { tasks: TaskCard[] }) => void = () => {};
    vi.spyOn(api, "apiFetch").mockImplementation(
      () => new Promise((resolve) => { release = resolve as typeof release; }),
    );

    const pending = useKanbanStore.getState().fetchTasks();
    useKanbanStore.getState().addLocal(task("ws-new"));
    release({ tasks: [task("from-server")] });
    await pending;

    const ids = useKanbanStore.getState().tasks.map((t) => t.id);
    expect(ids).toContain("from-server");
    expect(ids).toContain("ws-new");
  });

  it("服务端记录覆盖本地同 id 的状态", async () => {
    useKanbanStore.setState({ tasks: [task("a", { status: "pending" })] });
    vi.spyOn(api, "apiFetch").mockResolvedValue({ tasks: [task("a", { status: "running" })] });

    await useKanbanStore.getState().fetchTasks();

    expect(useKanbanStore.getState().tasks).toHaveLength(1);
    expect(useKanbanStore.getState().tasks[0].status).toBe("running");
  });

  it("快照里已删除的任务被移除", async () => {
    useKanbanStore.setState({ tasks: [task("gone"), task("stays")] });
    vi.spyOn(api, "apiFetch").mockResolvedValue({ tasks: [task("stays")] });

    await useKanbanStore.getState().fetchTasks();

    expect(useKanbanStore.getState().tasks.map((t) => t.id)).toEqual(["stays"]);
  });

  it("缺少 tasks 字段的响应不抛错", async () => {
    vi.spyOn(api, "apiFetch").mockResolvedValue({} as { tasks: TaskCard[] });
    await expect(useKanbanStore.getState().fetchTasks()).resolves.toBeUndefined();
    expect(useKanbanStore.getState().loaded).toBe(true);
  });

  it("成功后置 loaded,失败时不置", async () => {
    vi.spyOn(api, "apiFetch").mockRejectedValue(new Error("boom"));
    await useKanbanStore.getState().fetchTasks();
    expect(useKanbanStore.getState().loaded).toBe(false);
    expect(useKanbanStore.getState().loading).toBe(false);
  });

  it("action 引用在 state 变化后保持稳定(可安全进 effect 依赖)", async () => {
    const before = useKanbanStore.getState().fetchTasks;
    useKanbanStore.getState().addLocal(task("x"));
    expect(useKanbanStore.getState().fetchTasks).toBe(before);
  });
});

describe("乐观更新与回滚", () => {
  it("transition 失败后调用方可回滚到原状态", async () => {
    useKanbanStore.setState({ tasks: [task("a", { status: "pending" })] });
    vi.spyOn(api, "apiFetch").mockRejectedValue(new Error("rejected"));

    const { updateLocal, transitionTask } = useKanbanStore.getState();
    updateLocal("a", { status: "queued" });
    expect(useKanbanStore.getState().tasks[0].status).toBe("queued");

    await expect(transitionTask("a", "queued")).rejects.toThrow("rejected");
    updateLocal("a", { status: "pending" });
    expect(useKanbanStore.getState().tasks[0].status).toBe("pending");
  });

  it("transition 成功后用后端回传记录覆盖本地", async () => {
    useKanbanStore.setState({ tasks: [task("a", { status: "pending" })] });
    vi.spyOn(api, "apiFetch").mockResolvedValue({
      task: task("a", { status: "queued", updated_at: "2026-02-02T00:00:00" }),
    });

    await useKanbanStore.getState().transitionTask("a", "queued");

    expect(useKanbanStore.getState().tasks[0].status).toBe("queued");
    expect(useKanbanStore.getState().tasks[0].updated_at).toBe("2026-02-02T00:00:00");
  });

  it("addLocal 幂等:同 id 重复推送不产生重复卡片", () => {
    useKanbanStore.getState().addLocal(task("dup"));
    useKanbanStore.getState().addLocal(task("dup"));
    expect(useKanbanStore.getState().tasks).toHaveLength(1);
  });
});
