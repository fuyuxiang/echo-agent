import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

vi.mock("@/lib/agent-client", () => ({
  tasksList: vi.fn(),
  taskKill: vi.fn().mockResolvedValue(undefined),
}));

import { taskKill, tasksList } from "@/lib/agent-client";
import { TasksPanel } from "../TasksPanel";

beforeEach(() => {
  vi.mocked(tasksList).mockReset().mockResolvedValue([]);
  vi.mocked(taskKill).mockReset().mockResolvedValue(undefined);
});

describe("TasksPanel", () => {
  it("没有活动会话时不查询也不显示浮层", () => {
    render(<TasksPanel />);

    expect(tasksList).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("运行中任务")).not.toBeInTheDocument();
  });

  it("零任务查询失败只给非阻塞提示，不留下运行中任务 (0)", async () => {
    const onToast = vi.fn();
    vi.mocked(tasksList).mockRejectedValue(new Error("任务通道不可用"));
    render(<TasksPanel sessionId="session-1" onToast={onToast} />);

    await waitFor(() => expect(tasksList).toHaveBeenCalledWith("session-1"));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith("加载运行中任务失败：任务通道不可用"));
    expect(screen.queryByText(/运行中任务 \(0\)/)).not.toBeInTheDocument();
  });

  it("瞬时刷新失败不移除已知任务的终止入口", async () => {
    vi.mocked(tasksList)
      .mockResolvedValueOnce([{
        id: "task-123456",
        source: "task",
        description: "后台分析",
        status: "running",
        sessionId: "session-1",
      }])
      .mockRejectedValueOnce(new Error("暂时断开"));
    const { rerender } = render(<TasksPanel sessionId="session-1" refreshSignal={0} />);
    expect(await screen.findByText("后台分析")).toBeInTheDocument();

    rerender(<TasksPanel sessionId="session-1" refreshSignal={1} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("暂时断开");
    expect(screen.getByText("后台分析")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "终止任务：后台分析" })).toBeEnabled();
  });

  it("可以收起、展开和关闭，并在任务事件后重新显示", async () => {
    vi.mocked(tasksList).mockResolvedValue([{
      id: "task-1",
      source: "task",
      description: "生成报告",
      status: "running",
      sessionId: "session-1",
    }]);
    const { rerender } = render(
      <TasksPanel sessionId="session-1" refreshSignal={0} />,
    );
    expect(await screen.findByText("生成报告")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "收起运行中任务" }));
    expect(screen.queryByText("生成报告")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "展开运行中任务" }));
    expect(screen.getByText("生成报告")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "关闭运行中任务" }));
    expect(screen.queryByLabelText("运行中任务")).not.toBeInTheDocument();
    rerender(<TasksPanel sessionId="session-1" refreshSignal={1} />);
    expect(await screen.findByText("生成报告")).toBeInTheDocument();
  });

  it("按任务来源和当前会话调用正确的终止契约", async () => {
    vi.mocked(tasksList).mockResolvedValue([{
      id: "subagent-1",
      source: "subagent",
      description: "检索代码",
      status: "running",
      sessionId: "session-1",
    }]);
    render(<TasksPanel sessionId="session-1" />);

    fireEvent.click(await screen.findByRole("button", { name: "终止任务：检索代码" }));
    await waitFor(() => {
      expect(taskKill).toHaveBeenCalledWith("session-1", "subagent-1", "subagent");
    });
  });

  it("切换会话时忽略上一个会话晚到的结果", async () => {
    let resolveFirst!: (tasks: Awaited<ReturnType<typeof tasksList>>) => void;
    let resolveSecond!: (tasks: Awaited<ReturnType<typeof tasksList>>) => void;
    vi.mocked(tasksList)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveFirst = resolve; }))
      .mockImplementationOnce(() => new Promise((resolve) => { resolveSecond = resolve; }));

    const { rerender } = render(<TasksPanel sessionId="session-1" />);
    await waitFor(() => expect(tasksList).toHaveBeenCalledWith("session-1"));
    rerender(<TasksPanel sessionId="session-2" />);
    await waitFor(() => expect(tasksList).toHaveBeenCalledWith("session-2"));

    resolveFirst([{
      id: "old-task",
      source: "task",
      description: "旧会话任务",
      sessionId: "session-1",
    }]);
    resolveSecond([{
      id: "new-task",
      source: "task",
      description: "新会话任务",
      sessionId: "session-2",
    }]);

    expect(await screen.findByText("新会话任务")).toBeInTheDocument();
    expect(screen.queryByText("旧会话任务")).not.toBeInTheDocument();
  });

  it("切换会话后旧终止请求的 ACK 不重读或覆盖新会话任务", async () => {
    vi.mocked(tasksList).mockImplementation(async (sessionId) => [{
      id: `task-${sessionId}`, source: "task", description: `${sessionId} 的任务`, sessionId,
    }]);
    let finishKill!: () => void;
    vi.mocked(taskKill).mockImplementation(() => new Promise<void>((resolve) => { finishKill = resolve; }));
    const { rerender } = render(<TasksPanel sessionId="session-1" />);
    fireEvent.click(await screen.findByRole("button", { name: "终止任务：session-1 的任务" }));
    rerender(<TasksPanel sessionId="session-2" />);
    expect(await screen.findByText("session-2 的任务")).toBeInTheDocument();
    await act(async () => finishKill());
    expect(screen.getByText("session-2 的任务")).toBeInTheDocument();
    expect(tasksList).toHaveBeenCalledTimes(2);
  });

  it("两个任务并行终止时各自保留等待状态", async () => {
    const secondTask = { id: "second", source: "task" as const, description: "第二任务", sessionId: "s1" };
    vi.mocked(tasksList)
      .mockResolvedValueOnce([{ id: "first", source: "task", description: "第一任务", sessionId: "s1" }, secondTask])
      .mockResolvedValue([secondTask]);
    const finishes: Array<() => void> = [];
    vi.mocked(taskKill).mockImplementation(() => new Promise<void>((resolve) => { finishes.push(resolve); }));
    render(<TasksPanel sessionId="s1" />);
    fireEvent.click(await screen.findByRole("button", { name: "终止任务：第一任务" }));
    fireEvent.click(screen.getByRole("button", { name: "终止任务：第二任务" }));
    expect(screen.getByRole("button", { name: "终止任务：第一任务" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "终止任务：第二任务" })).toBeDisabled();
    await act(async () => finishes[0]());
    expect(screen.getByRole("button", { name: "终止任务：第二任务" })).toBeDisabled();
    await act(async () => finishes[1]());
  });
});
