import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Automation } from "./types";

export interface WorkflowBinding {
  workflowId: string;
  version: number;
  title: string;
  revisionDigest?: string;
}

export type WorkflowCapture =
  | { kind: "chat"; sessionId: string; promptId: string; request: string; result: string }
  | { kind: "scheduled"; runId: string };

export interface WorkflowSource {
  sessionId: string;
  promptId: string;
  cwd?: string;
  runId?: string;
  automationId?: string;
}

export interface WorkflowDraft {
  title: string;
  description: string;
  instructions: string;
  change: string;
}

export interface WorkflowCase {
  input: string;
  expected: string;
  kind: "contains" | "exact" | "json";
}

export interface WorkflowCheck {
  baselinePassed: boolean;
  candidatePassed: boolean;
  baselineMs: number;
  candidateMs: number;
  baselineOutput: string;
  candidateOutput: string;
}

export interface WorkflowVersion extends WorkflowDraft {
  number: number;
  source: WorkflowSource;
  createdAt: string;
  baseVersion?: number | null;
  cases: WorkflowCase[];
  evaluation?: { modelId: string; checkedAt: string; cases: WorkflowCheck[] } | null;
  dismissed?: boolean;
}

export interface Workflow {
  id: string;
  versions: WorkflowVersion[];
  activeVersion: number | null;
}

export interface WorkflowLibrarySnapshot {
  workflows: Workflow[];
  automaticDrafts: boolean;
}

export interface WorkflowSave {
  workflowId?: string;
  expectedLatest?: number;
  baseVersion?: number | null;
  draft: WorkflowDraft;
  cases: WorkflowCase[];
}

export function workflowVersions(workflow: Workflow) {
  return workflow.versions.filter((version) => !version.dismissed).slice().reverse();
}

export function workflowPassed(version: WorkflowVersion) {
  const checks = version.evaluation?.cases;
  return Boolean(checks?.length && checks.every((check) => check.candidatePassed));
}

export function fromWorkflowSource(source: WorkflowSource, capture: WorkflowCapture) {
  return capture.kind === "scheduled" ? source.runId === capture.runId
    : source.sessionId === capture.sessionId && source.promptId === capture.promptId;
}

export interface WorkflowCaptureContext {
  cwd: string;
  modelId: string | null;
  runWorkflow: WorkflowBinding | null;
}

export const workflowCaptureContext = (capture: WorkflowCapture) =>
  invoke<WorkflowCaptureContext>("workflow_skills_capture_context", { capture });
export const workflowWorkspaces = () => invoke<string[]>("workflow_skills_workspaces");

export const workflowLibrary = (cwd: string) =>
  invoke<WorkflowLibrarySnapshot>("workflow_skills_list", { cwd });
export interface WorkflowSourceMatch {
  version: number;
  title: string;
  status: "draft" | "verified" | "active";
}
export const workflowFindSource = (cwd: string, runId?: string, sessionId?: string, promptId?: string) =>
  invoke<WorkflowSourceMatch | null>("workflow_skills_find_source", {
    cwd, runId: runId ?? null, sessionId: sessionId ?? null, promptId: promptId ?? null,
  });
export const listenWorkflowLibrary = (callback: () => void) =>
  listen("workflow-skills://changed", callback);
export const workflowSetAutomatic = (cwd: string, enabled: boolean) =>
  invoke<void>("workflow_skills_set_automatic", { cwd, enabled });
export const workflowSuggest = (capture: WorkflowCapture, correction: string, modelId?: string,
  workflowId?: string, baseVersion?: number | null) =>
  invoke<WorkflowDraft | null>("workflow_skills_suggest", {
    capture, correction, modelId: modelId ?? null, workflowId: workflowId ?? null, baseVersion: baseVersion ?? null,
  });
export const workflowSuggestRevision = (cwd: string, id: string, version: number) =>
  invoke<WorkflowDraft | null>("workflow_skills_suggest_revision", { cwd, id, version });
export const workflowAutoDraft = (capture: WorkflowCapture, cwd: string, correction: string) =>
  invoke<Workflow | null>("workflow_skills_auto_draft", { capture, cwd, correction });
export const workflowSave = (capture: WorkflowCapture, input: WorkflowSave) =>
  invoke<Workflow>("workflow_skills_save", { capture, input });
export const workflowRevise = (cwd: string, sourceVersion: number, input: WorkflowSave) =>
  invoke<Workflow>("workflow_skills_revise", { cwd, sourceVersion, input });
export const workflowEvaluate = (cwd: string, id: string, version: number, modelId?: string) =>
  invoke<Workflow>("workflow_skills_evaluate", { cwd, id, version, modelId: modelId ?? null });
export const workflowActivate = (cwd: string, id: string, version: number | null, expectedActive: number | null) =>
  invoke<Workflow>("workflow_skills_activate", { cwd, id, version, expectedActive });
export const workflowDismiss = (cwd: string, id: string, version: number) =>
  invoke<Workflow>("workflow_skills_dismiss", { cwd, id, version });
export const workflowBind = (task: Automation, target?: { id: string; version: number }) =>
  invoke<Automation>("automations_set_workflow_skill", {
    id: task.id,
    expected: task.workflowSkill ?? null,
    workflowId: target?.id ?? null,
    version: target?.version ?? null,
  });
