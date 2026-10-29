import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

vi.mock("@/components/ExecutionProcess", () => ({ ExecutionProcess: () => <div data-testid="execution" /> }));
vi.mock("@/components/Markdown", () => ({ Markdown: ({ children }: { children: string }) => <div>{children}</div> }));
vi.mock("@/components/ModelSelector", () => ({ ModelSelector: () => <div data-testid="model-selector" /> }));
vi.mock("@/components/PermissionPicker", () => ({ PermissionPicker: () => <div data-testid="permission-picker" /> }));
vi.mock("@/components/PermissionDialog", () => ({ PermissionInlineCard: () => <div data-testid="permission-card" /> }));
vi.mock("@/components/QuestionInlineCard", () => ({ QuestionInlineCard: () => <div data-testid="question-card" /> }));

import { AgentPane } from "../agent/AgentPane";
import { describePhase, describeTaskProgress, isBusyPhase } from "../lib/phase";
import type { CodingTask, TaskNode } from "../lib/types";

function task(overrides: Partial<CodingTask> = {}): CodingTask {
  return {
    schemaVersion: 2, id: "t1", name: "重构登录", requirement: "改成 OIDC", phase: "implementing",
    acceptanceCriteria: [], taskNodes: [], planIssues: [], globalConstraints: [], createdAt: "", updatedAt: "", ...overrides,
  };
}

function node(overrides: Partial<TaskNode> = {}): TaskNode {
  return {
    id: "node-1", planKey: "T1", content: "建立订单域模型", dependencies: [],
    relatedFiles: ["src/order.ts"], readSet: [], writeSet: ["src/order.ts"], consumes: [],
    produces: ["OrderService"], acceptanceCriteria: ["订单可创建"], verificationCommands: ["pnpm test"],
    status: "running", priority: "high", attempt: 1, ...overrides,
  };
}

function paneProps(overrides: Partial<Parameters<typeof AgentPane>[0]> = {}) {
  return {
    task: task(), changeSet: null, verifications: [], sessionId: "s1", messages: [], streaming: false,
    awaitingPermission: false, awaitingQuestion: false, models: [{ id: "m1" }], modelId: "m1", sending: false,
    onModelChange: vi.fn(), onSend: vi.fn(), onCancel: vi.fn(), onContinue: vi.fn(), onOpenChanges: vi.fn(), onOpenReport: vi.fn(), ...overrides,
  };
}

describe("phase presentation", () => {
  it("marks engine-owned phases as active and terminal phases inactive", () => {
    for (const phase of ["discovering", "implementing", "verifying", "diagnosing", "repairing"] as const) {
      expect(describePhase(phase).active).toBe(true);
    }
    expect(describePhase("delivered")).toMatchObject({ label: "执行完成", tone: "waiting", active: false });
    expect(describePhase("blocked")).toMatchObject({ tone: "bad", active: false });
    expect(describePhase("paused")).toMatchObject({ label: "已暂停", active: false });
    expect(describePhase("stopped")).toMatchObject({ label: "已停止", active: false });
    expect(isBusyPhase("blocked")).toBe(false);
  });

  it("groups detailed scheduler phases into readable user-facing progress", () => {
    expect(describeTaskProgress("discovering").label).toBe("分析中");
    expect(describeTaskProgress("implementing").label).toBe("开发中");
    for (const phase of ["verifying", "diagnosing", "repairing"] as const) {
      expect(describeTaskProgress(phase).label).toBe("验证与修复");
    }
  });
});

describe("AgentPane", () => {
  it("shows structured plan progress and its active node", () => {
    render(<AgentPane {...paneProps({ task: task({ planRevision: "rev-1", taskNodes: [node()] }) })} />);
    expect(screen.getByText("执行计划 · 0/1")).toBeInTheDocument();
    expect(screen.queryByLabelText("执行计划进度")).not.toBeInTheDocument();
  });

  it("surfaces invalid plan contracts before execution", () => {
    render(<AgentPane {...paneProps({ task: task({ planIssues: [{ severity: "error", code: "write-conflict", message: "T1 与 T2 同时写入 src/a.ts", nodeKeys: ["T1", "T2"] }] }) })} />);
    expect(screen.getByText("计划详情 · 1 项提示")).toBeInTheDocument();
    expect(screen.getByText("T1 与 T2 同时写入 src/a.ts")).toBeInTheDocument();
  });

  it("finishes a read-only answer without empty changes or delivery gates", async () => {
    const props = paneProps({ task: task({ phase: "delivered" }) });
    render(<AgentPane {...props} />);
    expect(screen.getByText("本轮完成")).toBeInTheDocument();
    expect(screen.getByText("已完成回答")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /确认验收/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "交付报告" })).not.toBeInTheDocument();
    expect(props.onOpenReport).not.toHaveBeenCalled();
  });

  it("shows a recoverable stopped state with review and continue actions", async () => {
    const user = userEvent.setup();
    const props = paneProps({
      task: task({ phase: "stopped" }),
      phaseReason: "任务已停止，已保留当前工作区状态",
      changeSet: {
        taskId: "t1",
        changes: [{ path: "src/a.ts", kind: "added", added: 1, removed: 0, preExisting: false }],
        createdAt: "",
        reviewedFiles: [],
      },
    });
    render(<AgentPane {...props} />);

    expect(screen.getByRole("status")).toHaveTextContent("任务已停止");
    expect(screen.queryByText("需要人工介入")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "查看当前变更" }));
    await user.click(screen.getByRole("button", { name: "继续执行" }));
    expect(props.onOpenChanges).toHaveBeenCalled();
    expect(props.onContinue).toHaveBeenCalled();
  });

  it("renders permission and clarification cards when paused", () => {
    render(<AgentPane {...paneProps({ awaitingPermission: true, awaitingQuestion: true })} />);
    expect(screen.getByTestId("permission-card")).toBeInTheDocument();
    expect(screen.getByTestId("question-card")).toBeInTheDocument();
  });

  it("将最终答案作为正常正文展示，不塞进执行过程", () => {
    render(<AgentPane {...paneProps({
      messages: [{
        id: "answer-1",
        role: "assistant",
        parts: [{ kind: "text", text: "这是最终答案" }],
        complete: true,
      }],
    })} />);

    expect(screen.getByText("这是最终答案")).toBeInTheDocument();
    expect(screen.queryByTestId("execution")).toBeNull();
  });

  it("长执行过程可滚动，阅读旧消息时暂停跟随并可回到最新", async () => {
    const user = userEvent.setup();
    render(<AgentPane {...paneProps({ streaming: true })} />);
    const stream = screen.getByLabelText("Agent 执行消息");
    Object.defineProperties(stream, {
      scrollHeight: { configurable: true, value: 1_000 },
      clientHeight: { configurable: true, value: 400 },
      scrollTop: { configurable: true, writable: true, value: 600 },
    });
    fireEvent.scroll(stream);
    stream.scrollTop = 240;
    fireEvent.scroll(stream);

    const jump = await screen.findByRole("button", { name: "回到最新消息并恢复自动跟随" });
    expect(stream.scrollTop).toBe(240);
    await user.click(jump);
    expect(stream.scrollTop).toBe(600);
    expect(screen.queryByRole("button", { name: "回到最新消息并恢复自动跟随" })).toBeNull();
  });

});
