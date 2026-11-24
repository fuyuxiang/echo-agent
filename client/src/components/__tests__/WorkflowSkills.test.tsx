import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Automation } from "@/lib/types";
import type { Workflow, WorkflowCapture, WorkflowVersion } from "@/lib/workflow-skills";
import { WorkflowCaptureAction, WorkflowCollection, WorkflowTaskBinding } from "../workflow-skills/WorkflowSkills";

const { invoke, subscribe } = vi.hoisted(() => ({ invoke: vi.fn(), subscribe: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: subscribe }));

const cwd = "/approved/workspace";
const capture: WorkflowCapture = { kind: "scheduled", runId: "run-1" };
const procedure = {
  title: "销售数据整理", description: "整理周期数据", instructions: "使用当期数据执行归档规则。", change: "更正归档规则",
};
const version: WorkflowVersion = {
  ...procedure, number: 1, createdAt: "2026-11-24T00:00:00Z",
  source: { sessionId: "source", promptId: "prompt", runId: "run-1" },
  cases: [{ input: "待处理输入", expected: "标准输出", kind: "exact" }],
};
const passed = {
  ...version, evaluation: {
    modelId: "model", checkedAt: "2026-11-24T00:00:00Z",
    cases: [{ baselinePassed: false, candidatePassed: true, baselineMs: 500, candidateMs: 420,
      baselineOutput: "未完成", candidateOutput: "标准输出" }],
  },
};
const workflow: Workflow = { id: "workflow", activeVersion: null, versions: [version] };

const task = {
  id: "task-1", name: "已配置任务", cwds: [cwd], modelId: "model",
  workflowSkill: { workflowId: "workflow", version: 2, title: procedure.title },
} as Automation;

describe("工作区可复用流程", () => {
  beforeEach(() => {
    invoke.mockReset();
    subscribe.mockReset().mockResolvedValue(() => {});
  });

  it("从运行结果提取并保存草稿，验证通过后才允许绑定", async () => {
    invoke.mockImplementation(async (command: string) => {
      switch (command) {
        case "workflow_skills_find_source": return null;
        case "workflow_skills_capture_context": return { cwd, modelId: "model", runWorkflow: null };
        case "workflow_skills_list": return { workflows: [], automaticDrafts: false };
        case "workflow_skills_suggest": return procedure;
        case "workflow_skills_save": return workflow;
        case "workflow_skills_evaluate": return { ...workflow, versions: [passed] };
        case "automations_set_workflow_skill": return { ...task, workflowSkill: { ...task.workflowSkill, version: 1 } };
        default: throw new Error(command);
      }
    });
    const onBindingChanged = vi.fn();
    render(<WorkflowCaptureAction cwd={cwd} capture={capture} task={task} modelId="model" onBindingChanged={onBindingChanged} />);
    fireEvent.click(screen.getByRole("button", { name: "保存为可复用流程" }));
    fireEvent.click(await screen.findByRole("button", { name: "提取流程草稿" }));
    expect(await screen.findByDisplayValue(procedure.title)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "添加用例" }));
    fireEvent.change(screen.getByLabelText("任务输入"), { target: { value: version.cases[0].input } });
    fireEvent.change(screen.getByLabelText("判断方式"), { target: { value: "exact" } });
    fireEvent.change(screen.getByLabelText("预期输出"), { target: { value: version.cases[0].expected } });
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    expect(await screen.findByRole("button", { name: "用于这条定时任务" })).toBeDisabled();
    expect(invoke).toHaveBeenCalledWith("workflow_skills_save", {
      capture, input: expect.objectContaining({ cases: version.cases }),
    });
    fireEvent.click(screen.getByRole("button", { name: "运行验证" }));
    expect(await screen.findByText("版本 1 1/1 通过")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "用于这条定时任务" }));
    await waitFor(() => expect(onBindingChanged).toHaveBeenCalledTimes(1));
    expect(invoke).toHaveBeenCalledWith("automations_set_workflow_skill", {
      id: task.id, expected: task.workflowSkill, workflowId: workflow.id, version: 1,
    });
  });

  it("可从任务详情回退到较早的已验证版本", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "workflow_skills_list") return { workflows: [{ ...workflow, versions: [passed] }], automaticDrafts: false };
      if (command === "automations_set_workflow_skill") return task;
      throw new Error(command);
    });
    const changed = vi.fn();
    render(<WorkflowTaskBinding cwd={cwd} task={task} onBindingChanged={changed} />);
    fireEvent.click(screen.getByRole("button", { name: "选择流程版本" }));
    const dialog = within(await screen.findByRole("dialog"));
    fireEvent.change(await dialog.findByLabelText("流程版本"), { target: { value: "workflow:1" } });
    fireEvent.click(dialog.getByRole("button", { name: "确认切换" }));
    await waitFor(() => expect(changed).toHaveBeenCalledTimes(1));
    expect(invoke).toHaveBeenCalledWith("automations_set_workflow_skill", {
      id: task.id, expected: task.workflowSkill, workflowId: workflow.id, version: 1,
    });
  });

  it("打开历史任务时沿用当次流程和提取模型，并显示实际保存位置", async () => {
    const shared = "/approved/shared";
    const current = { ...passed, number: 2, instructions: "新策略",
      source: { sessionId: "newer", promptId: "newer", runId: "run-2" } };
    const previous = { ...passed, source: { sessionId: "older", promptId: "older" } };
    invoke.mockImplementation(async (command: string) => {
      if (command === "workflow_skills_find_source") return null;
      if (command === "workflow_skills_capture_context") return {
        cwd: shared, modelId: "source-model", runWorkflow: { ...task.workflowSkill, version: 1 },
      };
      if (command === "workflow_skills_list") return {
        workflows: [{ ...workflow, activeVersion: 2, versions: [previous, current] }], automaticDrafts: false,
      };
      if (command === "workflow_skills_suggest") return procedure;
      throw new Error(command);
    });
    render(<WorkflowCaptureAction cwd={cwd} capture={capture} task={task} modelId="model" />);
    fireEvent.click(screen.getByRole("button", { name: "保存为可复用流程" }));
    const extract = await screen.findByRole("button", { name: "提取流程草稿" });
    expect(screen.getByText(shared)).toBeInTheDocument();
    expect(screen.getByLabelText("提取模型")).toHaveValue("source-model");
    expect(screen.getByDisplayValue(procedure.instructions)).toBeInTheDocument();
    fireEvent.click(extract);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("workflow_skills_suggest", expect.objectContaining({
      capture, modelId: "source-model", workflowId: workflow.id, baseVersion: 1,
    })));
  });

  it("任务模型与验收模型不匹配时提供验证入口", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "workflow_skills_list") return {
        workflows: [{ ...workflow, versions: [passed] }], automaticDrafts: false,
      };
      throw new Error(command);
    });
    render(<WorkflowTaskBinding cwd={cwd} task={{ ...task,
      workflowSkill: { workflowId: workflow.id, version: 1, title: procedure.title }, modelId: "other-model",
    }} />);
    fireEvent.click(screen.getByRole("button", { name: "选择流程版本" }));
    fireEvent.change(await screen.findByLabelText("流程版本"), { target: { value: "workflow:1" } });
    expect(screen.getByRole("button", { name: "确认切换" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "查看并验证" }));
    expect(screen.getByRole("button", { name: "编辑为新版本" })).toBeEnabled();
    expect(screen.getByText(/当前版本使用 model 验证/)).toBeInTheDocument();
  });

  it("停用工作区技能后保留已验证版本", async () => {
    const published = { ...workflow, activeVersion: 1, versions: [passed] };
    invoke.mockImplementation(async (command: string) => {
      if (command === "workflow_skills_list") return { workflows: [published], automaticDrafts: false };
      if (command === "workflow_skills_activate") return { ...published, activeVersion: null };
      throw new Error(command);
    });
    render(<WorkflowCollection cwd={cwd} />);
    fireEvent.click(await screen.findByRole("button", { name: "查看流程" }));
    fireEvent.click(screen.getByRole("button", { name: "从技能库停用" }));
    expect(await screen.findByRole("button", { name: "在技能库启用" })).toBeEnabled();
    expect(invoke).toHaveBeenCalledWith("workflow_skills_activate", {
      cwd, id: workflow.id, version: null, expectedActive: 1,
    });
  });

  it("根据失败报告生成改进，保留验收用例并另存版本", async () => {
    const failed = { ...version, evaluation: { ...passed.evaluation,
      cases: [{ ...passed.evaluation.cases[0], candidatePassed: false, candidateOutput: "格式错误" }] } };
    const original = { ...workflow, versions: [failed] };
    const revision = { ...procedure, instructions: "按当前输入校验归档数据与输出格式。" };
    invoke.mockImplementation(async (command: string) => {
      if (command === "workflow_skills_list") return { workflows: [original], automaticDrafts: false };
      if (command === "workflow_skills_suggest_revision") return revision;
      if (command === "workflow_skills_revise") return { ...workflow, versions: [failed, { ...version, ...revision, number: 2 }] };
      throw new Error(command);
    });
    render(<WorkflowCollection cwd={cwd} />);
    fireEvent.click(await screen.findByRole("button", { name: "查看流程" }));
    fireEvent.click(screen.getByRole("button", { name: "根据验证改进" }));
    expect(await screen.findByDisplayValue(revision.instructions)).toBeInTheDocument();
    expect(screen.getByDisplayValue(version.cases[0].input)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    expect(await screen.findByText("流程草稿已保存")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("workflow_skills_suggest_revision", { cwd, id: workflow.id, version: 1 });
    expect(invoke).toHaveBeenCalledWith("workflow_skills_revise", { cwd, sourceVersion: 1,
      input: expect.objectContaining({ expectedLatest: 1, draft: revision, cases: version.cases }),
    });
  });

  it("后台沉淀完成后在来源任务展示可查看的流程", async () => {
    let completed = false;
    invoke.mockImplementation(async (command: string) => {
      if (command === "workflow_skills_find_source") {
        return completed ? { title: procedure.title, version: 1, status: "draft" } : null;
      }
      if (command === "workflow_skills_auto_draft") return workflow;
      throw new Error(command);
    });
    render(<WorkflowCaptureAction cwd={cwd} capture={{ kind: "chat", sessionId: "s", promptId: "p", request: "请求", result: "结果" }}
      accepted={Date.now()} correction="用户确认的修订" />);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("workflow_skills_auto_draft",
      { correction: "用户确认的修订", cwd, capture: { kind: "chat", sessionId: "s", promptId: "p", request: "请求", result: "结果" } }));
    completed = true;
    await act(async () => subscribe.mock.calls[0]?.[1]());
    expect(await screen.findByRole("button", { name: "查看流程" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("有流程草稿「销售数据整理」");
  });
});
