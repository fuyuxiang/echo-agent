import { invoke } from "@tauri-apps/api/core";
import type { ProjectConversationOptions } from "./project-context";
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
export const canEditTeamProject = (role: unknown) => role === "admin" || role === "collaborator";
export const projectRoleUnavailable = "组织服务未返回可识别的项目角色。请刷新项目，若仍无法确认请联系组织管理员。";
export type TeamTaskStatus = "pending" | "in_progress" | "blocked" | "review" | "accepted";
export const TEAM_TASK_LABELS: Record<TeamTaskStatus, string> = { pending: "待开始", in_progress: "进行中", blocked: "阻塞", review: "待验收", accepted: "已验收" };
export interface TeamProject { syncWarning?: string; id: string; name: string; goal: string; scopeId: string; ownerId: string; revision: number; updatedAt: number; role: ProjectRole }
export interface ProjectMember { userId: string; username: string; displayName: string; role: ProjectRole; status: string }
export interface ProjectMemberCandidate { userId: string; username: string; displayName: string }
export interface ProjectCollaborationCapabilities { taskIdentity?: boolean; projectComments?: boolean; decisionRevisions?: boolean; projectDeletion?: boolean; version: number; memberSearch: boolean; memberRoles: boolean; idempotentCreate: boolean }
export const PROJECT_DELETED_MESSAGE = "共享项目已删除";
export function projectCollaborationError(error: unknown): string {
  const text = String(error).replace(/^Error:\s*/, "");
  return /接口不存在|Route .*not found|Cannot (GET|POST).*projects/i.test(text)
    ? "当前组织服务尚未提供项目协作，请联系管理员升级服务。个人项目仍可正常使用。" : text;
}
export function projectConnectionIssue(error: string) {
  if (error === PROJECT_DELETED_MESSAGE) return "deleted";
  if (/\b401\b|unauthorized|token.*expired|session expired|not signed in|登录.*(失效|过期)|请.*登录/i.test(error)) return "sign-in";
  if (/\b(403|404)\b|forbidden|permission denied|project not found|项目不存在|失去成员权限|无权访问|无访问权限/i.test(error)) return "access";
  if (/error sending request|fetch|network|connect|unreachable|offline|timed?\s*out|timeout|dns|certificate|tls|ssl|服务不可用|\bHTTP 5\d\d\b/i.test(error)) return "connection";
  return undefined;
}
export interface TeamTask { id: string; title: string; description: string; acceptance: string; assigneeId: string | null; assigneeName: string | null; status: TeamTaskStatus; revision: number; updatedAt: number }
export interface ProjectDecision { id: string; content: string; rationale: string; status: "proposed" | "active" | "superseded" | "retired"; sourceTaskId: string | null; sourceDocumentId: string | null; supersedesId: string | null; revision: number; history?: {content: string; rationale: string; updatedAt: number; revision: number; status: string}[] }
export interface SharedAsset { id: string; familyId: string; title: string; version: number; status: string; failReason?: string; ftsStatus: string; byteSize: number; currentDocumentId: string | null; updatedAt: number }
export interface ProjectRun { id: string; taskId: string; actorId: string; sessionId: string; status: "running" | "completed" | "failed" | "cancelled"; modelId: string | null; expertName: string | null; expertVersion: string | null; summary: string; updatedAt: number }
export interface TeamProjectSnapshot extends TeamProject { comments?: {id: string; content: string; createdAt: number; actorName: string}[]; runs?: ProjectRun[]; members: ProjectMember[]; tasks: TeamTask[]; decisions: ProjectDecision[]; assets: SharedAsset[]; events: { id: string; kind: string; resourceId: string; createdAt: number; actorName: string }[] }
export interface ProjectCitation { id: string; title: string; quote: string; resourceId: string; kind: string; openUrl: string }
export interface ProjectAnswer { answer: string; verification: string; insufficient: boolean; projectRevision: number; updatedAt: number; gaps: string[]; citations: ProjectCitation[] }
export type StartTeamWork = (project: TeamProjectSnapshot, task: TeamTask, modelId: string, capability?: { expert?: AgentEntry; team?: TeamTemplate; modelOverride?: string; localProjectId?: string; options?: ProjectConversationOptions }) => Promise<string>;
export type OpenTeamExecution = (project: TeamProjectSnapshot, run: ProjectRun) => Promise<void>;
export type ProjectAction = "createComment" | "capabilities" | "memberCandidates" | "updateMember" | "list" | "create" | "snapshot" | "update" | "delete" | "addMember" | "removeMember" | "createTask" | "updateTask" | "claimTask" | "createDecision" | "updateDecision" | "ask";
export function teamProjectRequest<T>(action: ProjectAction, projectId?: string, body?: unknown, resourceId?: string, operation = captureTeamProjectOperation()): Promise<T> {
  operation.assertCurrent();
  return invoke<T>("org_project_request", { action, projectId: projectId ?? null, resourceId: resourceId ?? null, body: body ?? null, expectedGeneration: operation.expectedGeneration }).catch(error => { throw new Error(projectCollaborationError(error)); });
}
/** Old services with the original project API remain usable, with fewer controls. */
export async function projectCollaborationCapabilities(operation = captureTeamProjectOperation()): Promise<ProjectCollaborationCapabilities> {
  try {
    const value = await teamProjectRequest<ProjectCollaborationCapabilities>("capabilities", undefined, undefined, undefined, operation);
    if (!value || !Number.isInteger(value.version) || value.version < 1) throw new Error("协作服务返回的功能版本不兼容，请联系管理员更新服务。");
    return { ...(value.taskIdentity === true ? { taskIdentity: true } : {}), ...(value.projectComments === true ? { projectComments: true } : {}), ...(value.decisionRevisions === true ? { decisionRevisions: true } : {}), ...(value.projectDeletion === true ? { projectDeletion: true } : {}), version: value.version, memberSearch: value.memberSearch === true, memberRoles: value.memberRoles === true, idempotentCreate: value.idempotentCreate === true };
  }
  catch (error) {
    if (!/尚未提供项目协作|404|项目不存在|失去成员权限/.test(String(error))) throw error;
    const projects = await teamProjectRequest<TeamProject[]>("list", undefined, undefined, undefined, operation);
    if (!Array.isArray(projects)) throw new Error("共享项目列表格式不兼容，请更新组织服务。个人项目仍可正常使用。");
    return { version: 1, memberSearch: false, memberRoles: false, idempotentCreate: false };
  }
}
export const teamProjectUpload = (projectId: string, filePath: string, asset?: SharedAsset, operation = captureTeamProjectOperation()) => {
  operation.assertCurrent();
  return invoke<{ docId: string; familyId?: string; status: string }>("org_project_upload", {
    projectId, filePath, familyId: asset?.familyId ?? null, expectedVersion: asset?.version ?? null, expectedGeneration: operation.expectedGeneration,
  });
};
export const bindTeamProjectSession = (sessionId: string, projectId: string, taskId?: string, operation = captureTeamProjectOperation()) => {
  operation.assertCurrent();
  return invoke<void>("org_bind_project_session", { sessionId, projectId, taskId: taskId ?? null, expectedGeneration: operation.expectedGeneration });
};
