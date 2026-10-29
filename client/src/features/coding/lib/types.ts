/**
 * Mirrors the Rust types in `src-tauri/src/coding/`. Struct fields arrive as
 * camelCase and enum values as snake_case, matching the serde attributes there.
 */

export type TaskPhase =
  | "idle"
  | "discovering"
  | "implementing"
  | "verifying"
  | "diagnosing"
  | "repairing"
  | "paused"
  | "stopped"
  | "delivered"
  | "blocked";

export type TaskNodeStatus =
  | "pending"
  | "running"
  | "success"
  | "failed"
  | "blocked";
export type TaskNextAction = "revise_plan" | "continue_node";

export interface AcceptanceCriterion {
  id: string;
  content: string;
  satisfied: boolean;
  evidence: string[];
}

export interface TaskNode {
  id: string;
  planKey: string;
  content: string;
  dependencies: string[];
  relatedFiles: string[];
  readSet: string[];
  writeSet: string[];
  consumes: string[];
  produces: string[];
  acceptanceCriteria: string[];
  verificationCommands: string[];
  status: TaskNodeStatus;
  priority: string;
  attempt: number;
  startedAt?: string | null;
  completedAt?: string | null;
  failure?: string | null;
}

export type PlanIssueSeverity = "warning" | "error";

export interface PlanIssue {
  severity: PlanIssueSeverity;
  code: string;
  message: string;
  nodeKeys: string[];
}

export interface RuntimePlanEntry {
  key: string;
  content: string;
  dependencies: string[];
  relatedFiles: string[];
  readSet: string[];
  writeSet: string[];
  consumes: string[];
  produces: string[];
  acceptanceCriteria: string[];
  verificationCommands: string[];
  status: TaskNodeStatus;
  priority: string;
}

export interface ExecutionLedgerEvent {
  id: string;
  taskId: string;
  kind: string;
  nodeKey?: string | null;
  message: string;
  planRevision?: string | null;
  createdAt: string;
}

export interface CodingTask {
  schemaVersion: 2;
  strictWorkflow?: boolean;
  id: string;
  name: string;
  requirement: string;
  contextPaths?: string[];
  phase: TaskPhase;
  phaseReason?: string | null;
  blocker?: string | null;
  acceptanceCriteria: AcceptanceCriterion[];
  taskNodes: TaskNode[];
  planRevision?: string | null;
  planUpdatedAt?: string | null;
  planIssues: PlanIssue[];
  globalConstraints: string[];
  nextAction?: TaskNextAction | null;
  modelId?: string | null;
  sessionId?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface TaskSummary {
  id: string;
  name: string;
  phase: TaskPhase;
  updatedAt: string;
}

export type ChangeKind = "added" | "modified" | "deleted" | "renamed";

export interface FileChange {
  path: string;
  kind: ChangeKind;
  added: number;
  removed: number;
  baselineContent?: string | null;
  preExisting: boolean;
}

export interface ChangeSet {
  contentRevision?: string;
  taskId: string;
  baselineMode?: "git" | "filesystem" | null;
  /** Legacy backend fields; current IPC intentionally omits internal baselines. */
  baselineFiles?: string[];
  baselineHead?: string | null;
  baselineEntries?: Array<{
    path: string;
    existed: boolean;
    contentBase64?: string | null;
    hash?: string | null;
  }>;
  changes: FileChange[];
  createdAt: string;
  reviewedFiles: string[];
  reviewedHashes?: Record<string, string>;
  changeHashes?: Record<string, string>;
  rollbackUnsafeFiles?: string[];
  committedHash?: string | null;
  committedPaths?: string[];
}

export interface ChangeDiff {
  original: string;
  modified: string;
  binary: boolean;
  modifiedHash: string;
  /** Both snapshots are regular text files with unchanged metadata and native editor size support. */
  canApplyText?: boolean;
}

export interface IsolatedWorkspace {
  root: string;
  sourceRoot: string;
  baseHead: string;
  integratedHash?: string | null;
}

export interface CommitHunk {
  id: string;
  preview: string;
}

export type VerificationKind =
  | "build"
  | "lint"
  | "type_check"
  | "test"
  | "custom";
export type VerificationStatus =
  | "running"
  | "passed"
  | "failed"
  | "timed_out"
  | "cancelled"
  | "environment_unavailable";

export interface TestSummary {
  total: number;
  passed: number;
  failed: number;
  skipped: number;
}

export interface DetectedCommand {
  kind: VerificationKind;
  command: string;
  label: string;
  /** Plan-authored commands require an explicit, task-scoped user grant. */
  requiresApproval?: boolean;
}

export interface VerificationRecord {
  id: string;
  taskId: string;
  kind: VerificationKind;
  command: string;
  status: VerificationStatus;
  exitCode: number | null;
  stdout: string;
  stderr: string;
  durationMs: number;
  startedAt: string;
  finishedAt: string;
  contentRevision?: string | null;
  configurationRevision?: string | null;
  workspaceTree?: string | null;
  cwd?: string | null;
  testSummary?: TestSummary | null;
  /** False when the runner output could not be parsed into a summary. */
  structured: boolean;
}

export type ProblemKind =
  | "compile"
  | "syntax"
  | "type"
  | "lint"
  | "test_failure"
  | "runtime"
  | "dependency"
  | "configuration"
  | "documentation";

export type ProblemSeverity = "error" | "warning";

export interface Problem {
  id: string;
  kind: ProblemKind;
  severity: ProblemSeverity;
  message: string;
  file?: string | null;
  line?: number | null;
  column?: number | null;
  symbol?: string | null;
  sourceCommand: string;
  /** Stable across repair rounds; used to detect loops and regressions. */
  fingerprint: string;
}

export type RepairOutcome =
  | "fixed"
  | "same_errors"
  | "new_errors"
  | "rounds_exhausted";

export interface RepairRound {
  round: number;
  problemFingerprints: string[];
  startedAt: string;
  outcome?: RepairOutcome | null;
}

export interface OrchestratorState {
  task: CodingTask;
  problems: Problem[];
  repairRounds: RepairRound[];
  changedFileCount: number;
  maxRepairRounds: number;
}

export type GateId =
  | "build"
  | "test"
  | "lint"
  | "type_check"
  | "verification_freshness"
  | "diff_review"
  | "acceptance"
  | "requirements_review"
  | "code_quality_review"
  | "test_first";

export interface TddEvidence {
  redCommand?: string | null;
  redRecordId?: string | null;
  redRevision?: string | null;
  redAt?: string | null;
  waiverReason?: string | null;
}
export type GateStatus = "satisfied" | "not_satisfied" | "not_applicable";

export interface QualityGate {
  id: GateId;
  title: string;
  status: GateStatus;
  summary: string;
  evidence: string[];
}

export type ReviewKind = "requirements" | "code_quality";

export interface ReviewRecord {
  kind: ReviewKind;
  contentRevision: string;
  confirmedAt: string;
  sessionId?: string | null;
  runId?: string | null;
  summary?: string;
  runtimeReview?: boolean;
}

export interface EvidenceEntry {
  criterionId: string;
  kind: string;
  detail: string;
  source: string;
}

export interface DeliveryReport {
  task: CodingTask;
  gates: QualityGate[];
  changes: FileChange[];
  totalAdded: number;
  totalRemoved: number;
  verifications: VerificationRecord[];
  problems: Problem[];
  repairRounds: RepairRound[];
  evidence: EvidenceEntry[];
  blockers: string[];
  /** True only when no gate is `not_satisfied`. */
  deliverable: boolean;
}

/** Payload of `coding://task-phase-changed`. */
export interface PhaseChangedEvent {
  root: string;
  taskId: string;
  phase: TaskPhase;
  reason: string;
  blocker?: string | null;
}

/** Payload of `coding://verification-output`. */
export interface VerificationOutputEvent {
  root: string;
  taskId: string;
  runId: string;
  stream: "stdout" | "stderr";
  chunk: string;
}

/** Payload of `coding://analysis-progress`. */
export interface AnalysisProgressEvent {
  root: string;
  scanned: number;
}

// ---- Phase 2: cross-file symbol index ----

export type SymbolKind =
  | "function"
  | "class"
  | "method"
  | "constant"
  | "type"
  | "interface"
  | "enum"
  | "module"
  | "variable";

export interface SymbolRecord {
  id: string;
  name: string;
  kind: SymbolKind;
  container: string | null;
  file: string;
  line: number;
  column: number;
  signature: string | null;
  exported: boolean;
}

export interface SymbolQueryHit {
  symbol: SymbolRecord;
  score: number;
}

export type IndexState =
  | "empty"
  | "building"
  | "ready"
  | "rebuilding"
  | "stale";

export interface IndexStatus {
  state: IndexState;
  filesIndexed: number;
  symbols: number;
  lastReconciledAt: string | null;
  inProgress: boolean;
}

/** Payload of `coding://index-progress`. */
export interface IndexProgressEvent {
  root: string;
  scanned: number;
  totalEstimate: number;
}

/** Payload of `coding://index-updated`. */
export interface IndexUpdatedEvent {
  root: string;
  file: string;
  added: number;
  updated: number;
  removed: number;
}

/** Payload of `coding://index-removed`. */
export interface IndexRemovedEvent {
  root: string;
  file: string;
}

/** Payload of workspace file events used to keep open editor tabs current. */
export interface WorkspaceFileEvent {
  root: string;
  file: string;
}

// ---- Phase 2: cross-file references + impact ----

export type ReferenceKind =
  | "definition"
  | "read"
  | "write"
  | "call"
  | "import"
  | "type"
  | "unknown";

export interface ReferenceRecord {
  symbol: string;
  file: string;
  line: number;
  column: number;
  kind: ReferenceKind;
  preview: string;
}

export interface ReferenceHit {
  reference: ReferenceRecord;
  enclosingSymbol: SymbolRecord | null;
}

export interface ImpactNode {
  symbol: SymbolRecord;
  references: number;
  tests: number;
  depth: number;
}

export interface ImpactEdge {
  fromFile: string;
  fromLine: number;
  to: string;
  kind: ReferenceKind;
}

export interface ImpactGraph {
  target: string;
  direct: ImpactNode[];
  transitive: ImpactNode[];
  testImpact: SymbolRecord[];
  edges: ImpactEdge[];
  depthUsed: number;
}

export interface ProjectCheck {
  id: string;
  kind: VerificationKind;
  command: string;
  label: string;
  cwd: string;
  env: Record<string, string>;
  timeoutSecs?: number | null;
  autoRun: boolean;
}
export interface VerificationConfig {
  checks: ProjectCheck[];
}
export interface RunCheckpoint {
  runId: string;
  taskId: string;
  createdAt: string;
  finishedAt?: string | null;
  restoredAt?: string | null;
  confirmedPaths: string[];
  externalPaths: string[];
}
export interface CommitEvidence {
  hash: string;
  tree: string;
  paths: string[];
  contentRevision: string;
  verified: boolean;
  verificationRecordIds: string[];
  hookChangedTree: boolean;
  createdAt: string;
}
