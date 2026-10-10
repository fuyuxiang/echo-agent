import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent, waitFor } from "@testing-library/react";
import type { Plan } from "@/lib/types";
import { useSessionStore } from "@/stores/session-store";

// vi.mock 工厂被提升,引用的变量必须用 vi.hoisted 声明。
const { mocks } = vi.hoisted(() => ({
  mocks: {
    capturedPlan: null as Plan | null,
    setPlan: (..._a: unknown[]) => {},
    togglePlanMode: (..._a: unknown[]) => {},
    resolvePlanApproval: vi.fn(),
    planApproval: null as null | {
      requestId: string;
      sessionId: string;
      toolCallId: string;
      planContent?: string;
    },
  },
}));

vi.mock("@/lib/agent-client", () => ({
  setPlanMode: mocks.togglePlanMode,
  agentResolvePlanApproval: mocks.resolvePlanApproval,
}));

import { PlanPanel } from "../PlanPanel";

// 用真实 vi.fn 绑定到 mocks 上(在 import 之后)。
const originalSetPlan = useSessionStore.getState().setPlan;
const setPlan = vi.fn((p: Plan | null, options?: Parameters<typeof originalSetPlan>[1]) => {
  mocks.capturedPlan = p;
  originalSetPlan(p, options);
});
const togglePlanMode = vi.fn();
mocks.setPlan = setPlan as unknown as typeof mocks.setPlan;
mocks.togglePlanMode = togglePlanMode as unknown as typeof mocks.togglePlanMode;
const capturedPlan = () => mocks.capturedPlan;
const initialPlan = (): Plan => ({
  entries: [
    { content: "步骤一", priority: "high", status: "completed" },
    { content: "步骤二", priority: "medium", status: "in_progress" },
    { content: "步骤三", priority: "low", status: "pending" },
  ],
});
const focusSession = (sessionId: string) => {
  const store = useSessionStore.getState();
  store.setSession(sessionId);
  store.clearReplaySuppression(sessionId);
  if (!useSessionStore.getState().plan) originalSetPlan(initialPlan());
};

describe("PlanPanel 编辑器(对齐 EchoAgent plan-editor)", () => {
  beforeEach(() => {
    setPlan.mockClear();
    mocks.capturedPlan = null;
    togglePlanMode.mockClear();
    mocks.resolvePlanApproval.mockReset();
    mocks.planApproval = null;
    useSessionStore.getState().reset();
    useSessionStore.setState({ transcripts: {}, setPlan });
    focusSession("s1");
  });

  it("渲染进度与列表", () => {
    render(<PlanPanel sessionId="s1" />);
    expect(screen.getByText("1/3")).toBeInTheDocument();
    expect(screen.getByText("步骤一")).toBeInTheDocument();
    expect(screen.getByText("步骤三")).toBeInTheDocument();
  });

  it("上移/下移按钮调用 setPlan(reorder)", () => {
    render(<PlanPanel sessionId="s1" />);
    const ups = screen.getAllByRole("button", { name: "上移" });
    const downs = screen.getAllByRole("button", { name: "下移" });
    // 第二条上移 → [二, 一, 三]
    fireEvent.click(ups[1]);
    expect(setPlan).toHaveBeenCalled();
    expect(capturedPlan()!.entries.map((e) => e.content)).toEqual([
      "步骤二",
      "步骤一",
      "步骤三",
    ]);
    // 第一条不能上移(disabled)。
    expect(ups[0]).toBeDisabled();
    // 最后一条不能下移(disabled)。
    expect(downs[2]).toBeDisabled();
  });

  it("点击状态标签循环状态", () => {
    render(<PlanPanel sessionId="s1" />);
    // 第三条状态「待处理」→ 点击 → in_progress
    const statuses = screen.getAllByText("待处理");
    fireEvent.click(statuses[0]);
    expect(capturedPlan()!.entries[2].status).toBe("in_progress");
  });

  it("新增步骤:输入 + Enter 追加 pending 步骤", () => {
    render(<PlanPanel sessionId="s1" />);
    const input = screen.getByPlaceholderText("新增一个步骤…");
    fireEvent.change(input, { target: { value: "步骤四" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(capturedPlan()!.entries).toHaveLength(4);
    expect(capturedPlan()!.entries[3]).toEqual({
      content: "步骤四",
      priority: "medium",
      status: "pending",
    });
  });

  it("新增步骤按钮 disabled 当输入为空", () => {
    render(<PlanPanel sessionId="s1" />);
    expect(screen.getByText("添加").closest("button")).toBeDisabled();
  });

  it("删除按钮调用 setPlan(remove)", () => {
    render(<PlanPanel sessionId="s1" />);
    const dels = screen.getAllByRole("button", { name: "删除此任务" });
    fireEvent.click(dels[0]);
    expect(capturedPlan()!.entries.map((e) => e.content)).toEqual([
      "步骤二",
      "步骤三",
    ]);
  });

  it("运行时已批准但工作流保存失败时立即触发止损", async () => {
    mocks.planApproval = {
      requestId: "approval-1",
      sessionId: "s1",
      toolCallId: "tool-1",
      planContent: "# Plan",
    };
    useSessionStore.getState().requestPlanApproval(mocks.planApproval);
    mocks.resolvePlanApproval.mockResolvedValue(true);
    const onApprovalResolved = vi.fn(async () => {
      throw new Error("保存失败");
    });
    const onApprovalSyncFailed = vi.fn();
    render(
      <PlanPanel
        sessionId="s1"
        onApprovalResolved={onApprovalResolved}
        onApprovalSyncFailed={onApprovalSyncFailed}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "批准执行" }));
    await waitFor(() => expect(onApprovalSyncFailed).toHaveBeenCalledWith("保存失败"));
  });

  it("计划新增及编辑不会被中文输入法候选确认提前提交", () => {
    render(<PlanPanel sessionId="s1" />);
    const add = screen.getByRole("textbox", { name: "新增步骤" });
    fireEvent.change(add, { target: { value: "中文步骤" } });
    fireEvent.keyDown(add, { key: "Enter", isComposing: true });
    expect(setPlan).not.toHaveBeenCalled();
    fireEvent.doubleClick(screen.getByText("步骤一"));
    const edit = screen.getByDisplayValue("步骤一");
    fireEvent.change(edit, { target: { value: "中文修订" } });
    fireEvent.keyDown(edit, { key: "Enter", isComposing: true });
    fireEvent.keyDown(edit, { key: "Escape", isComposing: true });
    expect(setPlan).not.toHaveBeenCalled();
    expect(screen.getByDisplayValue("中文修订")).toBeInTheDocument();
    fireEvent.keyDown(edit, { key: "Enter" });
    expect(capturedPlan()!.entries[0].content).toBe("中文修订");
  });

  it("会话切换清除输入草稿、隔离并保留各自的未同步标记", () => {
    const { rerender } = render(<PlanPanel sessionId="s1" />);
    fireEvent.click(screen.getByRole("button", { name: "待处理" }));
    expect(screen.getByRole("button", { name: "同步修订" })).toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox", { name: "新增步骤" }), { target: { value: "A 的草稿" } });
    fireEvent.doubleClick(screen.getByText("步骤一"));
    fireEvent.change(screen.getByDisplayValue("步骤一"), { target: { value: "A 的未保存编辑" } });
    act(() => focusSession("s2"));
    rerender(<PlanPanel sessionId="s2" />);
    expect(screen.queryByDisplayValue("A 的未保存编辑")).not.toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "新增步骤" })).toHaveValue("");
    expect(screen.queryByRole("button", { name: "同步修订" })).not.toBeInTheDocument();
    act(() => focusSession("s1"));
    rerender(<PlanPanel sessionId="s1" />);
    expect(screen.getByRole("button", { name: "同步修订" })).toBeInTheDocument();
  });

  it("已编辑计划收起后重开仍可同步，成功提交后清除标记", async () => {
    const onSend = vi.fn().mockResolvedValue(true);
    const first = render(<PlanPanel sessionId="s1" onSend={onSend} />);
    fireEvent.doubleClick(screen.getByText("步骤一"));
    const edit = screen.getByDisplayValue("步骤一");
    fireEvent.change(edit, { target: { value: "已保存的本地修订" } });
    fireEvent.keyDown(edit, { key: "Enter" });
    first.unmount();

    render(<PlanPanel sessionId="s1" onSend={onSend} />);
    expect(screen.getByText("已保存的本地修订")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "同步修订" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "同步修订" })).not.toBeInTheDocument());
    expect(onSend).toHaveBeenCalledWith(expect.stringContaining("已保存的本地修订"));
    expect(useSessionStore.getState().transcripts.s1.planRevisionDirty).toBe(false);
  });

  it("收起后的未同步修订不会出现在另一会话，返回原会话仍可提交", () => {
    const first = render(<PlanPanel sessionId="s1" />);
    fireEvent.click(screen.getByRole("button", { name: "待处理" }));
    first.unmount();
    focusSession("s2");
    const second = render(<PlanPanel sessionId="s2" />);
    expect(screen.queryByRole("button", { name: "同步修订" })).not.toBeInTheDocument();
    second.unmount();
    focusSession("s1");
    render(<PlanPanel sessionId="s1" />);
    expect(screen.getByRole("button", { name: "同步修订" })).toBeInTheDocument();
  });

  it("同步确认晚到不会清除等待期间的新编辑，重开仍保留提交入口", async () => {
    let finishSync!: (accepted: boolean) => void;
    const onSend = vi.fn(() => new Promise<boolean>((resolve) => { finishSync = resolve; }));
    const first = render(<PlanPanel sessionId="s1" onSend={onSend} />);
    fireEvent.click(screen.getByRole("button", { name: "待处理" }));
    fireEvent.click(screen.getByRole("button", { name: "同步修订" }));
    fireEvent.change(screen.getByRole("textbox", { name: "新增步骤" }), { target: { value: "同步期间的新步骤" } });
    fireEvent.click(screen.getByRole("button", { name: "添加" }));
    await act(async () => finishSync(true));
    expect(screen.getByRole("button", { name: "同步修订" })).toBeInTheDocument();
    first.unmount();
    render(<PlanPanel sessionId="s1" onSend={onSend} />);
    expect(screen.getByText("同步期间的新步骤")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "同步修订" })).toBeInTheDocument();
  });

  it("Runtime 新计划与新审批初始化不保留旧修订标记", () => {
    render(<PlanPanel sessionId="s1" />);
    expect(screen.queryByRole("button", { name: "同步修订" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "待处理" }));
    expect(screen.getByRole("button", { name: "同步修订" })).toBeInTheDocument();
    act(() => useSessionStore.getState().applyUpdate({
      sessionUpdate: "plan",
      plan: initialPlan(),
      __sessionId: "s1",
    } as never));
    expect(screen.queryByRole("button", { name: "同步修订" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "待处理" }));
    act(() => useSessionStore.getState().requestPlanApproval({
      requestId: "fresh-approval",
      sessionId: "s1",
      toolCallId: "fresh-tool",
    }));
    expect(screen.getByRole("button", { name: "批准执行" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "同步修订" })).not.toBeInTheDocument();
  });

  it("旧会话审批晚到失败不会给新会话错误或解除新审批的等待", async () => {
    mocks.planApproval = { requestId: "a1", sessionId: "s1", toolCallId: "t1" };
    useSessionStore.getState().requestPlanApproval(mocks.planApproval);
    let rejectOld!: (error: Error) => void;
    let finishNew!: (result: boolean) => void;
    mocks.resolvePlanApproval
      .mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectOld = reject; }))
      .mockImplementationOnce(() => new Promise((resolve) => { finishNew = resolve; }));
    const { rerender } = render(<PlanPanel sessionId="s1" />);
    fireEvent.click(screen.getByRole("button", { name: "批准执行" }));
    mocks.planApproval = { requestId: "a2", sessionId: "s2", toolCallId: "t2" };
    act(() => {
      focusSession("s2");
      useSessionStore.getState().requestPlanApproval(mocks.planApproval!);
    });
    rerender(<PlanPanel sessionId="s2" />);
    fireEvent.click(screen.getByRole("button", { name: "批准执行" }));
    await act(async () => rejectOld(new Error("A 审批失败")));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "批准执行" })).toBeDisabled();
    await act(async () => finishNew(true));
    expect(screen.queryByRole("button", { name: "批准执行" })).not.toBeInTheDocument();
  });

  it("相同步数的会话切换后运行耗时从新会话重新开始", () => {
    vi.useFakeTimers();
    try {
      const { rerender } = render(<PlanPanel sessionId="s1" />);
      act(() => vi.advanceTimersByTime(3_000));
      expect(screen.getByText("⏱ 2s")).toBeInTheDocument();
      act(() => focusSession("s2"));
      rerender(<PlanPanel sessionId="s2" />);
      expect(screen.queryByText("⏱ 2s")).not.toBeInTheDocument();
      act(() => vi.advanceTimersByTime(1_000));
      expect(screen.getByText("⏱ 0s")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });
});
