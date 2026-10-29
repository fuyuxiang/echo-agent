/**
 * Thin wrappers over the coding workbench Tauri commands. Kept free of React so
 * both components and plain logic can call them, and so the backend contract
 * lives in exactly one place.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  codingCopyEntries as _codingCopyEntries,
  codingDeleteEntries as _codingDeleteEntries,
  codingMoveEntries as _codingMoveEntries,
  codingRenameEntry as _codingRenameEntry,
  codingRestoreFromTrash as _codingRestoreFromTrash,
  type CodingBatchOpResult,
  type CodingRenameResult,
  type CodingRestoreResult,
} from "@/lib/agent-client";

import type {
  AnalysisProgressEvent,
  VerificationConfig,
  RunCheckpoint,
  CommitEvidence,
  ChangeSet,
  ChangeDiff,
  CodingTask,
  DeliveryReport,
  DetectedCommand,
  FileChange,
  ImpactGraph,
  IndexProgressEvent,
  IndexRemovedEvent,
  IndexStatus,
  IndexUpdatedEvent,
  OrchestratorState,
  PhaseChangedEvent,
  ExecutionLedgerEvent,
  IsolatedWorkspace,
  CommitHunk,
  ReviewKind,
  ReviewRecord,
  TddEvidence,
  Problem,
  ReferenceHit,
  RuntimePlanEntry,
  SymbolQueryHit,
  SymbolRecord,
  TaskSummary,
  VerificationKind,
  VerificationOutputEvent,
  VerificationRecord,
  WorkspaceFileEvent,
} from "./types";

export const codingApi = {
  externalToolsPending: (root: string) => invoke<Array<{operationId:string;sessionId:string;runId:string;toolName:string;localInflight:boolean}>>("coding_external_tools_pending",{root}),
  recoverExternalTool: (root:string, operationId:string, note:string) => invoke<void>("coding_external_tool_recover",{root,operationId,note,confirmedStopped:true}),
  verificationFreshness: (root: string, taskId: string) =>
    invoke<{
      contentRevision: string;
      configurationRevision: string;
      workspaceTree?: string;
    }>("coding_verification_freshness", { root, taskId }),
  reviews: (root: string, taskId: string) =>
    invoke<ReviewRecord[]>("coding_review_list", { root, taskId }),
  taskForSession: (root: string, sessionId: string) =>
    invoke<CodingTask | null>("coding_task_for_session", { root, sessionId }),
  preserveBuffer: (
    root: string,
    taskId: string,
    runId: string,
    path: string,
    content: string,
  ) =>
    invoke<RunCheckpoint>("coding_checkpoint_preserve_buffer", {
      root,
      taskId,
      runId,
      path,
      content,
    }),
  savePlan: (root: string, path: string, content: string) =>
    invoke<string>("coding_plan_save", { root, path, content }),
  verificationConfig: (root: string) =>
    invoke<VerificationConfig>("coding_verification_config_get", { root }),
  saveVerificationConfig: (root: string, config: VerificationConfig) =>
    invoke<VerificationConfig>("coding_verification_config_save", {
      root,
      config,
    }),
  checkpointStart: (root: string, taskId: string, runId: string) =>
    invoke<RunCheckpoint>("coding_checkpoint_start", { root, taskId, runId }),
  checkpointFinish: (
    root: string,
    taskId: string,
    runId: string,
    confirmedPaths: string[],
  ) =>
    invoke<RunCheckpoint>("coding_checkpoint_finish", {
      root,
      taskId,
      runId,
      confirmedPaths,
    }),
  checkpoints: (root: string, taskId: string) =>
    invoke<RunCheckpoint[]>("coding_checkpoint_list", { root, taskId }),
  restoreCheckpoint: (root: string, taskId: string, runId: string) =>
    invoke<{ restoredPaths: string[]; conflicts: string[] }>(
      "coding_checkpoint_restore",
      { root, taskId, runId },
    ),
  commitHistory: (root: string, taskId: string) =>
    invoke<CommitEvidence[]>("coding_git_commit_history", { root, taskId }),
  isolationList: (root: string) =>
    invoke<
      Array<{
        root: string;
        sourceRoot: string;
        status: string;
        detail?: string;
        taskIds: string[];
        sessionIds: string[];
      }>
    >("coding_isolation_list", { root }),
  isolationCleanup: (root: string) =>
    invoke<void>("coding_isolation_cleanup", { root }),
  createIsolatedWorkspace: (root: string) =>
    invoke<IsolatedWorkspace>("coding_isolation_create", { root }),
  isolatedWorkspaceInfo: (root: string) =>
    invoke<IsolatedWorkspace | null>("coding_isolation_info", { root }),
  integrateIsolatedTask: (root: string, taskId: string) =>
    invoke<IsolatedWorkspace>("coding_isolation_integrate", { root, taskId }),
  confirmReview: (root: string, taskId: string, kind: ReviewKind) =>
    invoke<ReviewRecord>("coding_review_confirm", { root, taskId, kind }),
  tddStatus: (root: string, taskId: string) =>
    invoke<TddEvidence>("coding_tdd_status", { root, taskId }),
  recordRedTest: (root: string, taskId: string, recordId: string) =>
    invoke<TddEvidence>("coding_tdd_record_red", { root, taskId, recordId }),
  waiveTestFirst: (root: string, taskId: string, reason: string) =>
    invoke<TddEvidence>("coding_tdd_waive", { root, taskId, reason }),
  listTasks: (root: string) =>
    invoke<TaskSummary[]>("coding_task_list", { root }),
  createTask: (root: string, name: string, requirement: string) =>
    invoke<CodingTask>("coding_task_create", { root, name, requirement }),
  getTask: (root: string, taskId: string) =>
    invoke<CodingTask | null>("coding_task_get", { root, taskId }),
  deleteTask: (root: string, taskId: string) =>
    invoke<void>("coding_task_delete", { root, taskId }),
  renameTask: (root: string, taskId: string, name: string) =>
    invoke<CodingTask>("coding_task_rename", { root, taskId, name }),
  bindTaskRuntime: (
    root: string,
    taskId: string,
    sessionId: string,
    modelId: string,
  ) =>
    invoke<CodingTask>("coding_task_bind_runtime", {
      root,
      taskId,
      sessionId,
      modelId,
    }),
  setTaskContext: (root: string, taskId: string, paths: string[]) =>
    invoke<CodingTask>("coding_task_set_context", { root, taskId, paths }),
  confirmAcceptance: (root: string, taskId: string, criterionId: string) =>
    invoke<CodingTask>("coding_task_confirm_acceptance", {
      root,
      taskId,
      criterionId,
    }),

  submitRequirement: (root: string, taskId: string) =>
    invoke<CodingTask>("coding_task_submit_requirement", { root, taskId }),
  syncPlan: (root: string, taskId: string, entries: RuntimePlanEntry[]) =>
    invoke<CodingTask>("coding_task_sync_plan", { root, taskId, entries }),
  executionLedger: (root: string, taskId: string) =>
    invoke<ExecutionLedgerEvent[]>("coding_task_execution_ledger", {
      root,
      taskId,
    }),
  reportStartFailed: (root: string, taskId: string, reason: string) =>
    invoke<CodingTask>("coding_task_report_start_failed", {
      root,
      taskId,
      reason,
    }),
  beginFollowup: (root: string, taskId: string, requirement: string) =>
    invoke<CodingTask>("coding_task_begin_followup", {
      root,
      taskId,
      requirement,
    }),
  reportInterrupted: (
    root: string,
    taskId: string,
    interruption: "paused" | "stopped",
    expectedUpdatedAt?: string,
  ) =>
    invoke<CodingTask>("coding_task_report_interrupted", {
      root,
      taskId,
      interruption,
      ...(expectedUpdatedAt ? { expectedUpdatedAt } : {}),
    }),
  resumeTask: (root: string, taskId: string) =>
    invoke<CodingTask>("coding_task_resume", { root, taskId }),
  beginVerification: (
    root: string,
    taskId: string,
    expectedUpdatedAt?: string,
  ) =>
    invoke<CodingTask>("coding_task_begin_verification", {
      root,
      taskId,
      expectedUpdatedAt,
    }),
  rollbackTask: (root: string, taskId: string) =>
    invoke<string[]>("coding_task_rollback", { root, taskId }),

  getChangeSet: (root: string, taskId: string) =>
    invoke<ChangeSet>("coding_changeset_get", { root, taskId }),
  captureBaseline: (root: string, taskId: string, dirtyFiles: string[]) =>
    invoke<ChangeSet>("coding_changeset_capture_baseline", {
      root,
      taskId,
      dirtyFiles,
    }),
  changeDiff: (root: string, taskId: string, path: string) =>
    invoke<ChangeDiff>("coding_changeset_diff", { root, taskId, path }),
  markReviewed: (
    root: string,
    taskId: string,
    path: string,
    expectedHash: string,
  ) =>
    invoke<ChangeSet>("coding_changeset_mark_reviewed", {
      root,
      taskId,
      path,
      expectedHash,
    }),
  recordChange: (root: string, taskId: string, change: FileChange) =>
    invoke<ChangeSet>("coding_changeset_record_change", {
      root,
      taskId,
      change,
    }),
  discardFile: (root: string, taskId: string, path: string) =>
    invoke<ChangeSet>("coding_changeset_discard_file", { root, taskId, path }),
  revertHunkPreview: (
    root: string,
    taskId: string,
    path: string,
    hunkId: string,
  ) =>
    invoke<{ path: string; original: string; modified: string }>(
      "coding_changeset_revert_hunk_preview",
      { root, taskId, path, hunkId },
    ),
  syncChanges: (root: string, taskId: string) =>
    invoke<ChangeSet>("coding_changeset_sync", { root, taskId }),

  detectCommands: (root: string) =>
    invoke<DetectedCommand[]>("coding_verification_detect", { root }),
  listVerifications: (root: string, taskId: string) =>
    invoke<VerificationRecord[]>("coding_verification_list", { root, taskId }),
  approvePlanCommand: (root: string, taskId: string, command: string) =>
    invoke<string>("coding_verification_approve_plan_command", {
      root,
      taskId,
      command,
    }),
  runVerification: (
    root: string,
    taskId: string,
    kind: VerificationKind,
    command: string,
    timeoutSecs?: number,
    requestedRunId?: string,
    approvalToken?: string,
  ) =>
    invoke<VerificationRecord>("coding_verification_run", {
      root,
      taskId,
      kind,
      command,
      timeoutSecs: timeoutSecs ?? null,
      requestedRunId: requestedRunId ?? null,
      approvalToken: approvalToken ?? null,
    }),
  cancelVerification: (runId: string) =>
    invoke<void>("coding_verification_cancel", { runId }),

  listProblems: (root: string, taskId: string) =>
    invoke<Problem[]>("coding_diagnostics_list", { root, taskId }),

  reportImplementation: (root: string, taskId: string, runId?: string) =>
    invoke<CodingTask>("coding_orchestrator_report_implementation", {
      root,
      taskId,
      ...(runId ? {runId} : {}),
    }),
  reportVerification: (root: string, taskId: string) =>
    invoke<CodingTask>("coding_orchestrator_report_verification", {
      root,
      taskId,
    }),
  orchestratorState: (root: string, taskId: string) =>
    invoke<OrchestratorState>("coding_orchestrator_state", { root, taskId }),

  deliveryReport: (root: string, taskId: string) =>
    invoke<DeliveryReport>("coding_delivery_report", { root, taskId }),
  commitInput: (root: string, taskId: string) =>
    invoke<string>("coding_delivery_commit_input", { root, taskId }),
  commitHunks: (root: string, taskId: string, path: string) =>
    invoke<CommitHunk[]>("coding_delivery_commit_hunks", {
      root,
      taskId,
      path,
    }),
  prInput: (root: string, taskId: string) =>
    invoke<DeliveryReport>("coding_delivery_pr_input", { root, taskId }),
  commit: (
    root: string,
    taskId: string,
    message: string,
    selectedPaths: string[],
    selectedHunks: Record<string, string[]> = {},
  ) =>
    invoke<string>("coding_git_commit", {
      root,
      taskId,
      message,
      selectedPaths,
      selectedHunks,
    }),

  // Phase 2: cross-file symbol index + reference search + impact analysis.
  indexStatus: (root: string) =>
    invoke<IndexStatus>("coding_index_status", { root }),
  indexBootstrap: (root: string) =>
    invoke<IndexStatus>("coding_index_bootstrap", { root }),
  indexRelease: (root: string) =>
    invoke<void>("coding_index_release", { root }),
  indexRebuild: (root: string) =>
    invoke<IndexStatus>("coding_index_rebuild", { root }),
  symbolQuery: (
    root: string,
    needle: string,
    kind?: string,
    limit?: number,
    offset?: number,
  ) =>
    invoke<SymbolQueryHit[]>("coding_symbol_query", {
      root,
      needle,
      kind: kind ?? null,
      limit: limit ?? null,
      offset: offset ?? null,
    }),
  symbolAt: (root: string, file: string, line: number) =>
    invoke<SymbolRecord | null>("coding_symbol_at", { root, file, line }),

  refsFind: (root: string, symbol: string, includeDeclarations?: boolean) =>
    invoke<ReferenceHit[]>("coding_refs_find", {
      root,
      symbol,
      includeDeclarations: includeDeclarations ?? null,
    }),
  refsDefinition: (root: string, symbol: string) =>
    invoke<ReferenceHit[]>("coding_refs_definition", { root, symbol }),
  impactAnalyze: (
    root: string,
    target: string,
    depth?: number,
    includeTests?: boolean,
  ) =>
    invoke<ImpactGraph>("coding_impact_analyze", {
      root,
      target,
      depth: depth ?? null,
      includeTests: includeTests ?? null,
    }),
  // Theia owns visible file, search, terminal and Git interactions. These
  // native file commands remain registered for non-Theia callers and future
  // integrations; do not add a competing workbench file explorer here.
  createEntry: (
    root: string,
    parent: string | undefined,
    name: string,
    directory: boolean,
  ) =>
    invoke<string>("coding_create_entry", {
      request: { root, parent: parent ?? null, name, directory },
    }),
  deleteEntries: (root: string, paths: string[]) =>
    _codingDeleteEntries(root, paths),
  renameEntry: (root: string, path: string, newName: string) =>
    _codingRenameEntry(root, path, newName),
  copyEntries: (root: string, sources: string[], destination: string) =>
    _codingCopyEntries(root, sources, destination),
  moveEntries: (root: string, sources: string[], destination: string) =>
    _codingMoveEntries(root, sources, destination),
  restoreFromTrash: (
    root: string,
    originalPaths: string[],
    restoreTokens: string[],
  ) => _codingRestoreFromTrash(root, originalPaths, restoreTokens),
};

export type { CodingBatchOpResult, CodingRenameResult, CodingRestoreResult };

export function onPhaseChanged(
  callback: (event: PhaseChangedEvent) => void,
): Promise<UnlistenFn> {
  return listen<PhaseChangedEvent>("coding://task-phase-changed", (event) =>
    callback(event.payload),
  );
}

export function onVerificationUpdated(
  callback: (record: VerificationRecord) => void,
): Promise<UnlistenFn> {
  return listen<VerificationRecord>("coding://verification-updated", (event) =>
    callback(event.payload),
  );
}

export function onVerificationOutput(
  callback: (event: VerificationOutputEvent) => void,
): Promise<UnlistenFn> {
  return listen<VerificationOutputEvent>(
    "coding://verification-output",
    (event) => callback(event.payload),
  );
}

export function onAnalysisProgress(
  callback: (event: AnalysisProgressEvent) => void,
): Promise<UnlistenFn> {
  return listen<AnalysisProgressEvent>("coding://analysis-progress", (event) =>
    callback(event.payload),
  );
}

export function onIndexProgress(
  callback: (event: IndexProgressEvent) => void,
): Promise<UnlistenFn> {
  return listen<IndexProgressEvent>("coding://index-progress", (event) =>
    callback(event.payload),
  );
}

export function onIndexUpdated(
  callback: (event: IndexUpdatedEvent) => void,
): Promise<UnlistenFn> {
  return listen<IndexUpdatedEvent>("coding://index-updated", (event) =>
    callback(event.payload),
  );
}

export function onIndexRemoved(
  callback: (event: IndexRemovedEvent) => void,
): Promise<UnlistenFn> {
  return listen<IndexRemovedEvent>("coding://index-removed", (event) =>
    callback(event.payload),
  );
}

export function onWorkspaceFileUpdated(
  callback: (event: WorkspaceFileEvent) => void,
): Promise<UnlistenFn> {
  return listen<WorkspaceFileEvent>("coding://file-updated", (event) =>
    callback(event.payload),
  );
}

export function onWorkspaceFileRemoved(
  callback: (event: WorkspaceFileEvent) => void,
): Promise<UnlistenFn> {
  return listen<WorkspaceFileEvent>("coding://file-removed", (event) =>
    callback(event.payload),
  );
}
