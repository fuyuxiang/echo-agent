import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { TeamStatusView } from "../TeamStatusView";
import { teamSnapshot } from "@/lib/agent-client";
import type { ChatMessage } from "@/stores/session-store";

const teamMessage = (status: "in_progress" | "completed"): ChatMessage => ({
  id: "assistant-team", role: "assistant", complete: status === "completed",
  parts: [{ kind: "tool_call", toolCall: {
    toolCallId: "create-team", kind: "echoagent__create_team", title: "创建团队", status,
    rawInput: { team_id: "release-team", members: ["planner"] }, content: [],
  } }],
});

vi.mock("@/lib/agent-client", () => ({ teamSnapshot: vi.fn() }));

describe("TeamStatusView", () => {
  beforeEach(() => { vi.mocked(teamSnapshot).mockReset(); });

  it("从持久化运行时快照展示真实团队", async () => {
    vi.mocked(teamSnapshot).mockResolvedValue([{
      teamId: "release-team",
      members: ["planner", "reviewer"],
      createdAt: 10,
    }]);
    render(<TeamStatusView messages={[]} />);
    expect(await screen.findByText("release-team")).toBeInTheDocument();
    expect(screen.getByText("planner")).toBeInTheDocument();
    expect(screen.getByText("1 个 · 2 名成员")).toBeInTheDocument();
  });

  it("无团队时给出明确空状态", async () => {
    vi.mocked(teamSnapshot).mockResolvedValue([]);
    render(<TeamStatusView messages={[]} />);
    expect(await screen.findByText(/当前没有活动团队/)).toBeInTheDocument();
    expect(teamSnapshot).toHaveBeenCalled();
  });

  it("普通文本流式更新不重复读取团队，团队工具状态变更会刷新", async () => {
    vi.mocked(teamSnapshot).mockResolvedValue([]);
    const { rerender } = render(<TeamStatusView messages={[]} />);
    await waitFor(() => expect(teamSnapshot).toHaveBeenCalledTimes(1));
    for (let index = 1; index <= 5; index += 1) {
      rerender(<TeamStatusView messages={[{ id: "assistant", role: "assistant", complete: false, parts: [{ kind: "text", text: "输出".repeat(index) }] }]} />);
    }
    expect(teamSnapshot).toHaveBeenCalledTimes(1);
    rerender(<TeamStatusView messages={[teamMessage("in_progress")]} />);
    await waitFor(() => expect(teamSnapshot).toHaveBeenCalledTimes(2));
    rerender(<TeamStatusView messages={[teamMessage("completed")]} />);
    await waitFor(() => expect(teamSnapshot).toHaveBeenCalledTimes(3));
  });

  it("忽略比新快照更晚返回的旧快照", async () => {
    const finishes: Array<(snapshot: Awaited<ReturnType<typeof teamSnapshot>>) => void> = [];
    vi.mocked(teamSnapshot).mockImplementation(() => new Promise((resolve) => { finishes.push(resolve); }));
    const { rerender } = render(<TeamStatusView messages={[]} />);
    expect(screen.getByRole("status")).toHaveTextContent("正在读取团队状态");
    rerender(<TeamStatusView messages={[teamMessage("completed")]} />);
    await act(async () => finishes[1]([{ teamId: "新团队", members: ["new"], createdAt: 20 }]));
    await act(async () => finishes[0]([{ teamId: "旧团队", members: ["old"], createdAt: 10 }]));
    expect(screen.getByText("新团队")).toBeInTheDocument();
    expect(screen.queryByText("旧团队")).not.toBeInTheDocument();
  });

  it("已有快照后刷新失败保留团队并显示错误，支持再试", async () => {
    vi.mocked(teamSnapshot)
      .mockResolvedValueOnce([{ teamId: "保留团队", members: ["planner"], createdAt: 10 }])
      .mockRejectedValueOnce(new Error("暂时读取失败"))
      .mockResolvedValueOnce([]);
    render(<TeamStatusView messages={[]} />);
    expect(await screen.findByText("保留团队")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("已保留上次结果");
    expect(screen.getByText("保留团队")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));
    expect(await screen.findByText(/当前没有活动团队/)).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

});
