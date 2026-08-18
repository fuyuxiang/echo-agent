import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { TaskDetailDrawer } from "./TaskDetailDrawer";
import { ConfirmProvider } from "./ConfirmDialog";
import * as api from "../lib/api";
import { useKanbanStore, type TaskCard } from "../stores/kanban";

function task(overrides: Partial<TaskCard> = {}): TaskCard {
  return {
    id: "t1",
    title: "写周报",
    description: "汇总本周进展",
    status: "pending",
    priority: 3,
    labels: ["report"],
    assignee: "alice",
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

function mockApi(t: TaskCard) {
  return vi.spyOn(api, "apiFetch").mockImplementation(async (_path: string, init?: RequestInit) => {
    if (init?.method === "PUT" || init?.method === "POST") {
      return { task: { ...t, ...JSON.parse(String(init.body ?? "{}")) } } as never;
    }
    return {} as never;
  });
}

function renderDrawer(t: TaskCard, onClose = vi.fn()) {
  render(
    <ConfirmProvider>
      <TaskDetailDrawer task={t} onClose={onClose} />
    </ConfirmProvider>,
  );
  return onClose;
}

beforeEach(() => {
  useKanbanStore.setState({ tasks: [task()] });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("TaskDetailDrawer 详情", () => {
  it("展示后端返回的 result —— 之前它在界面上没有任何落点", async () => {
    const t = task({ status: "success", result: "已发送到群里" });
    mockApi(t);
    renderDrawer(t);

    expect(screen.getByText("已发送到群里")).toBeInTheDocument();
    expect(screen.getByText("执行结果")).toBeInTheDocument();
  });

  it("Escape 关闭抽屉", async () => {
    mockApi(task());
    const onClose = renderDrawer(task());

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });
});

describe("TaskDetailDrawer 编辑", () => {
  it("编辑表单用 store 记录回填", async () => {
    const t = task({ title: "写周报（后端最新）" });
    mockApi(t);
    renderDrawer(t);

    fireEvent.click(screen.getByRole("button", { name: "编辑任务：写周报（后端最新）" }));

    expect(screen.getByLabelText("标题")).toHaveValue("写周报（后端最新）");
    expect(screen.getByLabelText(/优先级/)).toHaveValue(3);
    expect(screen.getByLabelText("负责人")).toHaveValue("alice");
    expect(screen.getByLabelText(/标签/)).toHaveValue("report");
  });

  it("保存发 PUT,标签按逗号切分", async () => {
    const spy = mockApi(task());
    renderDrawer(task());

    fireEvent.click(await screen.findByRole("button", { name: /编辑任务/ }));
    fireEvent.change(screen.getByLabelText(/优先级/), { target: { value: "7" } });
    fireEvent.change(screen.getByLabelText(/标签/), { target: { value: " a , b ,, c " } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => {
      const put = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === "PUT");
      expect(put?.[0]).toBe("/tasks/t1");
      expect(JSON.parse(String((put?.[1] as RequestInit).body))).toEqual({
        title: "写周报",
        description: "汇总本周进展",
        priority: 7,
        assignee: "alice",
        // 空段与首尾空格都要去掉,否则会把 "" 当成一个标签存进后端。
        labels: ["a", "b", "c"],
      });
    });
  });

  it("优先级留空时沿用原值,不会把任务降成 P0", async () => {
    const spy = mockApi(task());
    renderDrawer(task());

    fireEvent.click(await screen.findByRole("button", { name: /编辑任务/ }));
    fireEvent.change(screen.getByLabelText(/优先级/), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => {
      const put = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === "PUT");
      expect(JSON.parse(String((put?.[1] as RequestInit).body)).priority).toBe(3);
    });
  });

  it("标题为空时保存按钮不可点", async () => {
    mockApi(task());
    renderDrawer(task());

    fireEvent.click(await screen.findByRole("button", { name: /编辑任务/ }));
    fireEvent.change(screen.getByLabelText("标题"), { target: { value: "   " } });

    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
  });

  it("保存成功后退出编辑态", async () => {
    mockApi(task());
    renderDrawer(task());

    fireEvent.click(await screen.findByRole("button", { name: /编辑任务/ }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(screen.queryByLabelText("标题")).not.toBeInTheDocument());
  });

  it("保存失败时保留表单内容,不清空用户输入", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async (_path: string, init?: RequestInit) => {
      if (init?.method === "PUT") throw new Error("409");
      return { task: task() } as never;
    });
    renderDrawer(task());

    fireEvent.click(await screen.findByRole("button", { name: /编辑任务/ }));
    fireEvent.change(screen.getByLabelText("标题"), { target: { value: "改了一半" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(screen.getByLabelText("标题")).toHaveValue("改了一半"));
  });
});

describe("TaskDetailDrawer 取消", () => {
  it("终态任务不给取消入口(状态机不允许)", async () => {
    mockApi(task({ status: "cancelled" }));
    renderDrawer(task({ status: "cancelled" }));

    await waitFor(() => expect(screen.getByText("已取消")).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: "取消任务" })).not.toBeInTheDocument();
  });

  it("确认后走 transition 而非 DELETE,并关闭抽屉", async () => {
    const spy = mockApi(task());
    const onClose = renderDrawer(task());

    fireEvent.click(await screen.findByRole("button", { name: "取消任务" }));
    // 文案要说明正在执行的一轮会被中断,而不只是“确定吗”。
    expect(await screen.findByText(/当前这一轮会被中断/)).toBeInTheDocument();
    // 抽屉里的入口按钮和确认框的确认按钮同名,后出现的那个是确认框里的。
    fireEvent.click(screen.getAllByRole("button", { name: "取消任务" })[1]);

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/tasks/t1/transition", {
        method: "POST",
        body: JSON.stringify({ to: "cancelled" }),
      }),
    );
    expect(onClose).toHaveBeenCalled();
  });

  it("确认框点“取消”不会真的取消任务", async () => {
    const spy = mockApi(task());
    renderDrawer(task());

    fireEvent.click(await screen.findByRole("button", { name: "取消任务" }));
    fireEvent.click(await screen.findByRole("button", { name: "取消" }));

    await waitFor(() =>
      expect(spy).not.toHaveBeenCalledWith("/tasks/t1/transition", expect.anything()),
    );
  });

  it("流转失败时回滚本地状态", async () => {
    vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path.endsWith("/transition")) throw new Error("非法流转");
      return { task: task() } as never;
    });
    const onClose = renderDrawer(task());

    fireEvent.click(await screen.findByRole("button", { name: "取消任务" }));
    fireEvent.click(screen.getAllByRole("button", { name: "取消任务" })[1]);

    await waitFor(() =>
      expect(useKanbanStore.getState().tasks[0].status).toBe("pending"),
    );
    expect(onClose).not.toHaveBeenCalled();
  });
});
