import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  deliveryReport: vi.fn(),
  confirmAcceptance: vi.fn(),
  confirmReview: vi.fn(),
  waiveTestFirst: vi.fn(),
}));

vi.mock("../lib/tauri-api", () => ({ codingApi: api }));

import { DeliveryReportTab } from "../main/docs/DeliveryReportTab";
import { TaskDagTab } from "../main/docs/TaskDagTab";
import type { CodingTask, DeliveryReport, QualityGate } from "../lib/types";

function gate(overrides: Partial<QualityGate> = {}): QualityGate {
  return {
    id: "test",
    title: "测试通过",
    status: "satisfied",
    summary: "1 项检查全部通过",
    evidence: ["pnpm test：通过（退出码 0）"],
    ...overrides,
  };
}

function task(overrides: Partial<CodingTask> = {}): CodingTask {
  return {
    schemaVersion: 2,
    id: "t1",
    name: "重构登录",
    requirement: "改成 OIDC",
    phase: "delivered",
    acceptanceCriteria: [],
    taskNodes: [],
    planIssues: [],
    globalConstraints: [],
    createdAt: "",
    updatedAt: "",
    ...overrides,
  };
}

function report(overrides: Partial<DeliveryReport> = {}): DeliveryReport {
  return {
    task: task(),
    gates: [gate()],
    changes: [
      {
        path: "src/auth.ts",
        kind: "modified",
        added: 24,
        removed: 6,
        baselineContent: "old",
        preExisting: false,
      },
    ],
    totalAdded: 24,
    totalRemoved: 6,
    verifications: [],
    problems: [],
    repairRounds: [],
    evidence: [],
    blockers: [],
    deliverable: true,
    ...overrides,
  };
}

describe("DeliveryReportTab optional evidence export", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    Object.values(api).forEach(mock => mock.mockReset());
    api.deliveryReport.mockResolvedValue(report());
  });

  it("waits for a coding session without requesting a report", () => {
    render(<DeliveryReportTab root="/repo" taskId={null} onOpenFile={vi.fn()} />);
    expect(screen.getByText("选择会话后可导出摘要")).toBeInTheDocument();
    expect(api.deliveryReport).not.toHaveBeenCalled();
  });

  it("exports real changes without restoring manual delivery gates", async () => {
    api.deliveryReport.mockResolvedValue(report({ deliverable: false, gates: [
      gate({ id: "acceptance", status: "not_satisfied" }),
      gate({ id: "requirements_review", status: "not_satisfied" }),
      gate({ id: "test_first", status: "not_satisfied" }),
    ] }));
    render(<DeliveryReportTab root="/repo" taskId="t1" onOpenFile={vi.fn()} />);
    expect(await screen.findByText(/摘要引用会话真实变更和检查历史/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "复制 PR 摘要" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: /确认|豁免|审查/ })).not.toBeInTheDocument();
    expect(api.confirmAcceptance).not.toHaveBeenCalled();
    expect(api.confirmReview).not.toHaveBeenCalled();
    expect(api.waiveTestFirst).not.toHaveBeenCalled();
  });

  it("distinguishes no executed checks from a passing result", async () => {
    render(<DeliveryReportTab root="/repo" taskId="t1" onOpenFile={vi.fn()} />);
    const summary = await screen.findByText(/### 检查记录/);
    expect(summary).toHaveTextContent("未执行");
    expect(summary).not.toHaveTextContent("全部通过");
  });

  it("keeps historical revisions and real exit codes visible in the export", async () => {
    const base = { taskId: "t1", kind: "test" as const, command: "pnpm test", stdout: "", stderr: "", durationMs: 10, startedAt: "", finishedAt: "", structured: false };
    api.deliveryReport.mockResolvedValue(report({ verifications: [
      { ...base, id: "old-failure", status: "failed", exitCode: 1, contentRevision: "old-revision" },
      { ...base, id: "new-pass", status: "passed", exitCode: 0, contentRevision: "new-revision" },
    ] }));
    render(<DeliveryReportTab root="/repo" taskId="t1" onOpenFile={vi.fn()} />);
    const summary = await screen.findByText(/### 检查记录/);
    expect(summary).toHaveTextContent("历史结果不代表当前版本");
    expect(summary).toHaveTextContent("failed, exit=1, revision=old-revision");
    expect(summary).toHaveTextContent("passed, exit=0, revision=new-revision");
  });

  it("opens a changed file in the workspace", async () => {
    const onOpenFile = vi.fn();
    render(<DeliveryReportTab root="/repo" taskId="t1" onOpenFile={onOpenFile} />);
    await userEvent.click(await screen.findByRole("button", { name: "src/auth.ts" }));
    expect(onOpenFile).toHaveBeenCalledWith("src/auth.ts");
  });

  it("retries a failed evidence read without losing the coding session", async () => {
    api.deliveryReport.mockRejectedValueOnce(new Error("历史读取失败")).mockResolvedValue(report());
    render(<DeliveryReportTab root="/repo" taskId="t1" onOpenFile={vi.fn()} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("历史读取失败");
    await userEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByRole("button", { name: "复制 PR 摘要" })).toBeEnabled();
    expect(api.deliveryReport).toHaveBeenNthCalledWith(2, "/repo", "t1");
  });

  it("ignores an old project's late report after navigating to another project", async () => {
    let finishOld!: (value: DeliveryReport) => void;
    api.deliveryReport.mockReturnValueOnce(new Promise<DeliveryReport>(resolve => { finishOld = resolve; }))
      .mockResolvedValueOnce(report({ task: task({ name: "新项目会话" }) }));
    const view = render(<DeliveryReportTab root="/repo" taskId="t1" onOpenFile={vi.fn()} />);
    view.rerender(<DeliveryReportTab root="/other" taskId="t2" onOpenFile={vi.fn()} />);
    expect(await screen.findByText(/## 新项目会话/)).toBeInTheDocument();
    await act(async () => { finishOld(report({ task: task({ name: "旧项目会话" }) })); });
    expect(screen.queryByText(/## 旧项目会话/)).not.toBeInTheDocument();
    expect(screen.getByText(/## 新项目会话/)).toBeInTheDocument();
  });

  it("copies the same evidence text and exposes clipboard failure", async () => {
    const user = userEvent.setup();
    const write = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error("clipboard unavailable"));
    const onToast = vi.fn();
    render(<DeliveryReportTab root="/repo" taskId="t1" onOpenFile={vi.fn()} onToast={onToast} />);
    const button = await screen.findByRole("button", { name: "复制 PR 摘要" });
    await user.click(button);
    expect(write).toHaveBeenCalledWith(expect.stringContaining("src/auth.ts (+24 -6)"));
    expect(write).toHaveBeenCalledWith(expect.stringContaining("未执行"));
    expect(onToast).toHaveBeenLastCalledWith("PR 摘要已复制");
    await user.click(button);
    expect(onToast).toHaveBeenLastCalledWith("复制失败，请选择下方文本");
  });
});

describe("TaskDagTab", () => {
  const props = {
    repairRounds: [],
    maxRepairRounds: 3,
    changedFileCount: 2,
    problemCount: 0,
    onOpenFile: vi.fn(),
  };

  it("asks for a task first", () => {
    render(<TaskDagTab {...props} task={null} />);
    expect(screen.getByText(/新建开发任务后即可查看拆解与进度/)).toBeInTheDocument();
  });

  it("explains when Agent has not generated a breakdown yet", () => {
    render(<TaskDagTab {...props} task={task({ phase: "discovering" })} />);
    expect(screen.getByText("Agent 尚未生成执行计划。")).toBeInTheDocument();
  });

  it("lists nodes with dependencies and opens their files", async () => {
    const user = userEvent.setup();
    const onOpenFile = vi.fn();
    render(
      <TaskDagTab
        {...props}
        onOpenFile={onOpenFile}
        task={task({
          taskNodes: [
            {
              id: "T2",
              planKey: "T2",
              content: "接入回调",
              dependencies: ["T1"],
              relatedFiles: ["src/callback.ts"],
              readSet: ["src/auth.ts"],
              writeSet: ["src/callback.ts"],
              consumes: ["AuthSession"],
              produces: ["CallbackHandler"],
              acceptanceCriteria: ["回调可处理"],
              verificationCommands: ["pnpm test"],
              status: "running",
              priority: "high",
              attempt: 1,
            },
          ],
        })}
      />,
    );
    expect(screen.getByText("接入回调")).toBeInTheDocument();
    expect(screen.getByText("依赖 T1")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "src/callback.ts" }));
    expect(onOpenFile).toHaveBeenCalledWith("src/callback.ts");
  });

  it("counts completed nodes", () => {
    render(
      <TaskDagTab
        {...props}
        task={task({
          taskNodes: [
            { id: "T1", planKey: "T1", content: "a", dependencies: [], relatedFiles: [], readSet: [], writeSet: [], consumes: [], produces: [], acceptanceCriteria: [], verificationCommands: [], status: "success", priority: "high", attempt: 1 },
            { id: "T2", planKey: "T2", content: "b", dependencies: [], relatedFiles: [], readSet: [], writeSet: [], consumes: [], produces: [], acceptanceCriteria: [], verificationCommands: [], status: "pending", priority: "low", attempt: 0 },
          ],
        })}
      />,
    );
    expect(screen.getByText("1/2")).toBeInTheDocument();
  });

  it("shows the repair round budget when repairs ran", () => {
    render(
      <TaskDagTab
        {...props}
        task={task()}
        repairRounds={[{ round: 1, problemFingerprints: [], startedAt: "" }]}
      />,
    );
    expect(screen.getByText("1/3")).toBeInTheDocument();
  });
});
