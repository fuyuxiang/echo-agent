import { invoke } from "@tauri-apps/api/core";
import type { AgentEntry } from "./types";
import type { TeamTemplate } from "@/features/groups/group-client";
import { useOrgSessionStore } from "@/stores/org-session-store";

export interface TeamProjectOperation {
  expectedGeneration: number | undefined;
  assertCurrent: () => void;
}
export function captureTeamProjectOperation(): TeamProjectOperation {
  const initial = useOrgSessionStore.getState();
  const boundary = initial.boundaryRevision;
  return {
    expectedGeneration: initial.session?.accountGeneration,
    assertCurrent: () => {
      const current = useOrgSessionStore.getState();
      if (!current.session?.loggedIn || current.boundaryRevision !== boundary) {
        throw new Error("组织身份或权限已变化，请重新打开项目后操作");
      }
    },
  };
}

export type ProjectRole = "admin" | "collaborator" | "viewer";
export type TeamTaskStatus = "pending" | "in_progress" | "blocked" | "review" | "accepted";
export const TEAM_TASK_LABELS: Record<TeamTaskStatus, string> = { pending: "待开始", in_progress: "进行中", blocked: "阻塞", review: "待验收", accepted: "已验收" };
export interface TeamProject { syncWarning?: string; id: string; name: string; goal: string; scopeId: string; ownerId: string; revision: number; updatedAt: number; role: ProjectRole }
export interface ProjectMember { userId: string; username: string; displayName: string; role: ProjectRole; status: string }
export interface TeamTask { id: string; title: string; description: string; acceptance: string; assigneeId: string | null; assigneeName: string | null; status: TeamTaskStatus; revision: number; updatedAt: number }
export interface ProjectDecision { id: string; content: string; rationale: string; status: "proposed" | "active" | "superseded" | "retired"; sourceTaskId: string | null; sourceDocumentId: string | null; supersedesId: string | null; revision: number }
export interface SharedAsset { id: string; familyId: string; title: string; version: number; status: string; failReason?: string; ftsStatus: string; byteSize: number; currentDocumentId: string | null; updatedAt: number }
export interface ProjectRun { id: string; taskId: string; actorId: string; sessionId: string; status: "running" | "completed" | "failed" | "cancelled"; modelId: string | null; expertName: string | null; expertVersion: string | null; summary: string; updatedAt: number }
export interface TeamProjectSnapshot extends TeamProject { runs?: ProjectRun[]; members: ProjectMember[]; tasks: TeamTask[]; decisions: ProjectDecision[]; assets: SharedAsset[]; events: { id: string; kind: string; resourceId: string; createdAt: number; actorName: string }[] }
export interface ProjectCitation { id: string; title: string; quote: string; resourceId: string; kind: string; openUrl: string }
export interface ProjectAnswer { answer: string; verification: string; insufficient: boolean; projectRevision: number; updatedAt: number; gaps: string[]; citations: ProjectCitation[] }
export type StartTeamWork = (project: TeamProjectSnapshot, task: TeamTask, modelId: string, capability?: { expert?: AgentEntry; team?: TeamTemplate; modelOverride?: string }) => Promise<string>;
export type OpenTeamExecution = (project: TeamProjectSnapshot, run: ProjectRun) => Promise<void>;
export type ProjectAction = "list" | "create" | "snapshot" | "update" | "addMember" | "removeMember" | "createTask" | "updateTask" | "claimTask" | "createDecision" | "updateDecision" | "ask";
export function teamProjectRequest<T>(action: ProjectAction, projectId?: string, body?: unknown, resourceId?: string, operation = captureTeamProjectOperation()): Promise<T> {
  operation.assertCurrent();
  return invoke<T>("org_project_request", { action, projectId: projectId ?? null, resourceId: resourceId ?? null, body: body ?? null, expectedGeneration: operation.expectedGeneration });
}
export const teamProjectUpload = (projectId: string, filePath: string, asset?: SharedAsset, operation = captureTeamProjectOperation()) => {
  operation.assertCurrent();
  return invoke<{ docId: string; status: string }>("org_project_upload", {
    projectId, filePath, familyId: asset?.familyId ?? null, expectedVersion: asset?.version ?? null, expectedGeneration: operation.expectedGeneration,
  });
};
export const bindTeamProjectSession = (sessionId: string, projectId: string, taskId?: string, operation = captureTeamProjectOperation()) => {
  operation.assertCurrent();
  return invoke<void>("org_bind_project_session", { sessionId, projectId, taskId: taskId ?? null, expectedGeneration: operation.expectedGeneration });
};
