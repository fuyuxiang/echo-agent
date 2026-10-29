import { describe, expect, it } from "vitest";

import { parseRuntimePlan, runtimePlanFingerprint } from "../lib/workflow-plan";
import {
  buildCodingWorkflowPrompt,
  buildNodeContinuationInstruction,
  buildPlanRevisionInstruction,
  mergeTaskVerificationCommands,
} from "../lib/workflow";
import type { CodingTask, TaskNode } from "../lib/types";

const node = (overrides: Partial<TaskNode> = {}): TaskNode => ({
  id: "node-1",
  planKey: "T1",
  content: "实现登录回调",
  dependencies: [],
  relatedFiles: ["src/auth.ts"],
  readSet: ["src/session.ts"],
  writeSet: ["src/auth.ts"],
  consumes: ["Session"],
  produces: ["AuthCallback"],
  acceptanceCriteria: ["回调成功"],
  verificationCommands: ["pnpm test -- auth"],
  status: "running",
  priority: "high",
  attempt: 1,
  ...overrides,
});

const task = (overrides: Partial<CodingTask> = {}): CodingTask => ({
  schemaVersion: 2,
  id: "task-1",
  name: "登录",
  requirement: "实现登录",
  phase: "implementing",
  acceptanceCriteria: [],
  taskNodes: [node()],
  planIssues: [],
  globalConstraints: [],
  createdAt: "",
  updatedAt: "",
  ...overrides,
});

describe("coding workflow contract", () => {
  it("defaults to direct Agent work without mandatory delivery ceremonies", () => {
    const prompt = buildCodingWorkflowPrompt("迁移登录", [" src/auth.ts ", "src/auth.ts"]);
    expect(prompt).toContain("[回声代码 · agent]");
    expect(prompt).toContain("在授权范围内直接完成修改");
    expect(prompt).toContain("根据变更选择相关检查");
    expect(prompt).toContain("保护用户修改");
    expect(prompt).toContain("不要求固定 DAG、分轮 RED/GREEN、双重审查或人工交付表");
    expect(prompt).not.toContain("两道门禁");
    expect(prompt.match(/src\/auth\.ts/g)).toHaveLength(1);
  });

  it("keeps follow-ups in the current session with the selected intent", () => {
    const prompt = buildCodingWorkflowPrompt("再增加审计日志", [], true);
    expect(prompt).toContain("继续当前会话");
    expect(prompt).toContain("本轮要求");
    expect(prompt).toContain("再增加审计日志");
  });

  it("carries explicit read-only intent and versioned editor context without escalating from rules", () => {
    const ask = buildCodingWorkflowPrompt("解释当前函数", [], false, {
      intent: "ask", editorContext: "file:///repo/auth.ts version=7 dirty=true\n未保存函数",
    });
    expect(ask).toContain("当前为 Ask");
    expect(ask).toContain("未保存内容不要求保存");
    expect(ask).toContain("version=7 dirty=true");
    expect(ask).toContain("不能提升用户选择的只读权限");
    const plan = buildCodingWorkflowPrompt("规划重构", [], false, { intent: "plan" });
    expect(plan).toContain("当前为 Plan");
    expect(plan).toContain("工程只读，不创建计划文件");
    expect(plan).toContain("只有用户点击执行后才进入 Agent");
  });

  it("builds bounded instructions for plan repair and node continuation", () => {
    const invalid = task({
      nextAction: "revise_plan",
      planIssues: [{
        severity: "error",
        code: "missing_verification",
        message: "T1 没有声明验证命令",
        nodeKeys: ["T1"],
      }],
    });
    expect(buildPlanRevisionInstruction(invalid)).toContain("T1 没有声明验证命令");
    const continuation = buildNodeContinuationInstruction(task(), node());
    expect(continuation).toContain("只继续下面这个由调度器选中的节点");
    expect(continuation).toContain("Files: src/auth.ts");
    expect(continuation).toContain("RED → GREEN → REFACTOR");
  });

  it("preserves the user's test-only boundary in a metadata-only plan repair turn", () => {
    const requirement = "本轮只写测试，等待工作台记录 RED 后再实现生产代码。";
    const prompt = buildPlanRevisionInstruction(task({
      requirement,
      nextAction: "revise_plan",
      planIssues: [{ severity: "error", code: "missing_verification", message: "T1 缺少验证命令", nodeKeys: ["T1"] }],
    }));
    expect(prompt).toContain(requirement);
    expect(prompt).not.toContain("修正后再继续实施");
    expect(prompt).toContain("不修改任何工程文件");
  });

  it("merges project checks with node-level verification contracts", () => {
    const commands = mergeTaskVerificationCommands(
      [{ kind: "build", command: "pnpm build", label: "构建" }],
      task(),
    );
    expect(commands).toEqual([
      { kind: "build", command: "pnpm build", label: "构建" },
      {
        kind: "test",
        command: "pnpm test -- auth",
        label: "节点验证 · T1",
        requiresApproval: true,
      },
    ]);
  });

  it("still requires consent when a plan command matches a detected project check", () => {
    const planned = task();
    planned.taskNodes[0].verificationCommands = ["pnpm build"];
    expect(mergeTaskVerificationCommands(
      [{ kind: "build", command: "pnpm build", label: "构建" }],
      planned,
    )).toEqual([
      { kind: "build", command: "pnpm build", label: "构建", requiresApproval: true },
    ]);
  });
});

describe("runtime plan parser", () => {
  it("extracts dependency, file and interface contracts from plan entries", () => {
    const entries = parseRuntimePlan({
      entries: [{
        content: "[T2] 接入回调\nDepends: T1\nFiles: src/callback.ts\nReads: src/auth.ts\nConsumes: AuthSession\nProduces: CallbackHandler\nAcceptance: 回调成功落库\nVerify: pnpm test -- callback.test.ts",
        priority: "high",
        status: "in_progress",
      }],
    });
    expect(entries[0]).toMatchObject({
      key: "T2",
      content: "接入回调",
      dependencies: ["T1"],
      relatedFiles: ["src/callback.ts", "src/auth.ts"],
      writeSet: ["src/callback.ts"],
      consumes: ["AuthSession"],
      produces: ["CallbackHandler"],
      acceptanceCriteria: ["回调成功落库"],
      verificationCommands: ["pnpm test -- callback.test.ts"],
      status: "running",
    });
  });

  it("turns unstructured plans into a conservative sequential DAG", () => {
    const entries = parseRuntimePlan({
      entries: [
        { content: "定义共享接口", priority: "high", status: "completed" },
        { content: "实现消费者", priority: "medium", status: "pending" },
      ],
    });
    expect(entries[0]).toMatchObject({ key: "T1", dependencies: [], status: "success" });
    expect(entries[1]).toMatchObject({ key: "T2", dependencies: ["T1"], status: "pending" });
    expect(runtimePlanFingerprint({ entries: [] })).toBe("[]");
  });
});
