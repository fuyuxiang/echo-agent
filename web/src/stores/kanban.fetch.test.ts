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
  useKanbanStore.setState({
    tasks: [], loading: false, loaded: false, dirtyIds: new Set<string>(),
  });
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

  it("飞行期间 WS 更新过的同一个 id 不被较旧的快照盖回", async () => {
    // 只按 id 判断新增/删除挡不住这种情况:快照里有这条记录,但它是请求发出时的
    // 旧状态。WS 期间已推来 running,快照后到仍带 pending,直接以服务端为准就会
    // 让卡片“跳回”上一个状态。
    useKanbanStore.setState({ tasks: [task("a", { status: "pending" })] });
    let release: (value: { tasks: TaskCard[] }) => void = () => {};
    vi.spyOn(api, "apiFetch").mockImplementation(
      () => new Promise((resolve) => { release = resolve as typeof release; }),
    );

    const pending = useKanbanStore.getState().fetchTasks();
    useKanbanStore.getState().updateLocal("a", { status: "running" });
    release({ tasks: [task("a", { status: "pending" }), task("b")] });
    await pending;

    const byId = new Map(useKanbanStore.getState().tasks.map((t) => [t.id, t]));
    expect(byId.get("a")!.status).toBe("running");
    // 同一份快照里其他任务的更新照常生效,不因一个脏 id 而整份丢弃。
    expect(byId.has("b")).toBe(true);
  });

  it("脏标记只在本次请求内有效,下一次快照仍以服务端为准", async () => {
    useKanbanStore.setState({ tasks: [task("a", { status: "pending" })] });
    useKanbanStore.getState().updateLocal("a", { status: "running" });

    vi.spyOn(api, "apiFetch").mockResolvedValue({
      tasks: [task("a", { status: "success" })],
    });
    await useKanbanStore.getState().fetchTasks();

    // fetch 开始时清空脏标记,所以这份新快照对 a 是权威的。
    expect(useKanbanStore.getState().tasks[0].status).toBe("success");
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
