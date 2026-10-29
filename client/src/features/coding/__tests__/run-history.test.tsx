import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { RunHistory } from "../panels/RunHistory";
import { codingApi } from "../lib/tauri-api";

vi.mock("../lib/tauri-api", () => ({
  codingApi: { checkpoints: vi.fn(), commitHistory: vi.fn(), reviews: vi.fn(), restoreCheckpoint: vi.fn() },
}));
vi.mock("@/components/Markdown", () => ({
  Markdown: ({ children }: { children: string }) => <p>{children}</p>,
}));

describe("RunHistory review evidence", () => {
  const review = {
    kind: "code_quality" as const,
    contentRevision: "rev-1",
    confirmedAt: "2026-10-29",
    runtimeReview: true,
    runId: "review-run",
    summary: "src/math.ts:3 handles zero incorrectly",
  };
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(codingApi.checkpoints).mockResolvedValue([]);
    vi.mocked(codingApi.commitHistory).mockResolvedValue([]);
    vi.mocked(codingApi.reviews).mockResolvedValue([review]);
  });
  it("refreshes restored history and change evidence, preserving overlapping later edits", async () => {
    const run = {runId:"run",createdAt:"today",finishedAt:"today",confirmedPaths:["agent.ts"],externalPaths:[],restoredAt:null};
    vi.mocked(codingApi.checkpoints).mockResolvedValue([run] as never);
    vi.mocked(codingApi.restoreCheckpoint).mockImplementation(async () => {
      vi.mocked(codingApi.checkpoints).mockResolvedValue([{...run,restoredAt:"now"}] as never);
      return {restoredPaths:[],conflicts:["agent.ts"]};
    });
    const onChanged=vi.fn().mockResolvedValue(undefined);
    render(<RunHistory root="/repo" taskId="task" revision="1" busy={false} onChanged={onChanged}/>);
    await userEvent.click(screen.getByText("恢复点、提交与可选审查"));
    await userEvent.click(await screen.findByRole("button",{name:"恢复本轮"}));
    await waitFor(()=>expect(onChanged).toHaveBeenCalledOnce());
    expect(codingApi.restoreCheckpoint).toHaveBeenCalledWith("/repo","task","run");
    expect(await screen.findByRole("button",{name:"已恢复"})).toBeDisabled();
    expect(screen.getByRole("status")).toHaveTextContent("agent.ts");
  });
  it("keeps real review history but marks changed or unsaved content stale and forwards its evidence to Agent", async () => {
    const onFixReview = vi.fn().mockResolvedValue(undefined);
    const props = {
      root: "/repo",
      taskId: "task",
      revision: "1",
      busy: false,
      onChanged: vi.fn(),
      onFixReview,
    };
    const view = render(<RunHistory {...props} currentRevision="rev-1" />);
    expect(
      await screen.findByText(/Agent 代码审查.*当前版本/),
    ).toBeInTheDocument();
    view.rerender(<RunHistory {...props} currentRevision="rev-2" />);
    expect(screen.getByText(/Agent 代码审查.*已过期/)).toBeInTheDocument();
    await userEvent.click(screen.getByText(/恢复点、提交与可选审查/));
    await userEvent.click(screen.getByText(/Agent 代码审查.*已过期/));
    await userEvent.click(
      screen.getByRole("button", { name: "交给 Agent 核对并修复" }),
    );
    expect(onFixReview).toHaveBeenCalledWith(review);
    view.rerender(<RunHistory {...props} currentRevision="rev-1" dirty />);
    expect(screen.getByText(/Agent 代码审查.*已过期/)).toBeInTheDocument();
  });
  it("never represents a legacy human checkbox as a real model review", async () => {
    vi.mocked(codingApi.reviews).mockResolvedValue([
      { ...review, runtimeReview: false, summary: undefined },
    ]);
    render(
      <RunHistory
        root="/repo"
        taskId="task"
        revision="1"
        currentRevision="rev-1"
        busy={false}
        onChanged={vi.fn()}
      />,
    );
    expect(
      await screen.findByText(/历史人工确认.*不作为模型审查证据/),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "交给 Agent 核对并修复" }),
    ).not.toBeInTheDocument();
  });
});
