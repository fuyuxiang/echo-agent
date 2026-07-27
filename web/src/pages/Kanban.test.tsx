import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { Kanban } from "./Kanban";
import { ConfirmProvider } from "../components/ConfirmDialog";
import * as api from "../lib/api";
import { useKanbanStore, type TaskCard } from "../stores/kanban";
import { useAuthStore } from "../stores/auth";
import { useToastStore } from "../stores/toast";

function task(overrides: Partial<TaskCard> = {}): TaskCard {
  return {
    id: "t1",
    title: "写周报",
    description: "",
    status: "pending",
    priority: 3,
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
    created_at: "2026-07-27T09:00:00Z",
    updated_at: "2026-07-27T09:00:00Z",
    ...overrides,
  };
}

function renderKanban() {
  render(
    <MemoryRouter>
      <ConfirmProvider>
        <Kanban />
      </ConfirmProvider>
    </MemoryRouter>,
  );
}

/** 让 fetchTasks 直接返回指定任务集,并把 store 置为已加载。 */
function mockTasks(tasks: TaskCard[]) {
  return vi.spyOn(api, "apiFetch").mockImplementation(async (path: string, init?: RequestInit) => {
    if (path.startsWith("/tasks?")) return { tasks, total: tasks.length } as never;
    if (init?.method === "POST" && path === "/tasks") {
      return { task: task({ id: "new", title: JSON.parse(String(init.body)).title }) } as never;
    }
    if (path.endsWith("/transition")) {
      return { task: { ...tasks[0], status: JSON.parse(String(init?.body ?? "{}")).to } } as never;
    }
    if (path.endsWith("/retry")) return { task: { ...tasks[0], status: "queued" } } as never;
    return { task: tasks[0] } as never;
  });
}

beforeEach(() => {
  useAuthStore.setState({ token: "" });
  useKanbanStore.setState({ tasks: [], loading: false, loaded: false });
  useToastStore.setState({ toasts: [] });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Kanban 看板", () => {
  it("按状态分列渲染,失败列常驻", async () => {
    mockTasks([task(), task({ id: "t2", title: "跑批", status: "failed", error: "超时了" })]);
    renderKanban();

    expect(await screen.findByText("写周报")).toBeInTheDocument();
    // 失败任务不再从看板消失,且卡片直接显示错误摘要。
    expect(screen.getByText("跑批")).toBeInTheDocument();
    expect(screen.getByText("超时了")).toBeInTheDocument();
    expect(screen.getByText("失败")).toBeInTheDocument();
  });

  it("默认隐藏归档列,勾选后出现", async () => {
    mockTasks([task({ id: "t3", status: "cancelled" })]);
    renderKanban();

    await waitFor(() => expect(screen.queryByText("已取消")).not.toBeInTheDocument());
    fireEvent.click(screen.getByRole("checkbox"));

    expect(screen.getByText("已取消")).toBeInTheDocument();
  });

  it("创建成功后清空输入框", async () => {
    mockTasks([]);
    renderKanban();

    const input = await screen.findByPlaceholderText("新建任务…");
    fireEvent.change(input, { target: { value: "新任务" } });
    fireEvent.click(screen.getByRole("button", { name: /创建/ }));

    await waitFor(() => expect(input).toHaveValue(""));
  });

  it("创建失败时保留已输入的标题", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path.startsWith("/tasks?")) return { tasks: [], total: 0 } as never;
      throw new Error("boom");
    });
    renderKanban();

    const input = await screen.findByPlaceholderText("新建任务…");
    fireEvent.change(input, { target: { value: "别弄丢" } });
    fireEvent.click(screen.getByRole("button", { name: /创建/ }));

    await waitFor(() => expect(useToastStore.getState().toasts.length).toBe(1));
    expect(input).toHaveValue("别弄丢");
  });

  it("收件箱卡片的“开始执行”走 transition 到 queued", async () => {
    const spy = mockTasks([task()]);
    renderKanban();

    fireEvent.click(await screen.findByRole("button", { name: "开始执行(交给 Agent)" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/tasks/t1/transition", {
        method: "POST",
        body: JSON.stringify({ to: "queued" }),
      }),
    );
  });

  it("重试走专用 retry 端点,不走通用 transition —— 否则会绕过 max_retries", async () => {
    const spy = mockTasks([task({ status: "failed", retry_count: 1 })]);
    renderKanban();

    fireEvent.click(await screen.findByRole("button", { name: "重试" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/tasks/t1/retry", { method: "POST" }));
    expect(spy).not.toHaveBeenCalledWith("/tasks/t1/transition", expect.anything());
  });

  it("待审任务给出通过/打回两个入口", async () => {
    const spy = mockTasks([task({ status: "review", review_summary: "请确认口径" })]);
    renderKanban();

    // 两个入口同时存在:通过直接结单,打回则重新排队。
    expect(await screen.findByRole("button", { name: "打回重排" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "通过审核" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/tasks/t1/transition", {
        method: "POST",
        body: JSON.stringify({ to: "success" }),
      }),
    );
  });

  it("打回重排把待审任务送回排队中", async () => {
    const spy = mockTasks([task({ status: "review", review_summary: "请确认口径" })]);
    renderKanban();

    fireEvent.click(await screen.findByRole("button", { name: "打回重排" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/tasks/t1/transition", {
        method: "POST",
        body: JSON.stringify({ to: "queued" }),
      }),
    );
  });

  it("流转失败时回滚本地状态", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path.startsWith("/tasks?")) return { tasks: [task()], total: 1 } as never;
      throw new Error("被状态机拒绝");
    });
    renderKanban();

    fireEvent.click(await screen.findByRole("button", { name: "开始执行(交给 Agent)" }));

    await waitFor(() => expect(useKanbanStore.getState().tasks[0].status).toBe("pending"));
  });

  it("点卡片打开详情抽屉", async () => {
    mockTasks([task({ description: "汇总本周进展" })]);
    renderKanban();

    fireEvent.click(await screen.findByRole("button", { name: "查看任务详情：写周报" }));

    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(screen.getByText("任务描述")).toBeInTheDocument();
  });

  it("列内按优先级降序排列", async () => {
    mockTasks([
      task({ id: "low", title: "低优", priority: 1 }),
      task({ id: "high", title: "高优", priority: 8 }),
    ]);
    renderKanban();

    await screen.findByText("高优");
    const titles = screen.getAllByRole("button", { name: /查看任务详情/ })
      .map((el) => el.textContent);
    expect(titles[0]).toContain("高优");
    expect(titles[1]).toContain("低优");
  });
});
