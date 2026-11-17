import type { ProjectConversation, TaskItem, ProjectMeta, AssetItem } from "@/stores/projects-store";
import type { TeamProjectSnapshot, TeamTask, SharedAsset } from "./team-project-client";

/** Session records remain untouched; only their project entry becomes a task. */
export function unifyProjectTasks(tasks: readonly TaskItem[], conversations: readonly ProjectConversation[]): TaskItem[] {
  const result = tasks.map(task => ({ ...task }));
  const represented = new Set(result.map(task => task.sessionId).filter(Boolean));
  for (const conversation of conversations) {
    if (represented.has(conversation.sessionId)) continue;
    let id = `session:${conversation.sessionId}`;
    while (result.some(task => task.id === id)) id = `session:${id}`;
    result.push({ id, title: conversation.title, sessionId: conversation.sessionId, modelId: conversation.modelId, sessionArchived: conversation.archived, createdAt: conversation.createdAt, source: "conversation", scope: "personal", status: "pending" });
    represented.add(conversation.sessionId);
  }
  return result;
}
export interface SharedTaskLink { projectId: string; taskId?: string; requestId?: string }
export interface SharedAssetLink { projectId: string; documentId: string; familyId?: string }
export interface UnifiedTask { id: string; title: string; local?: TaskItem; remote?: TeamTask; remoteId?: string; sessionId?: string; private: boolean }
export function projectTaskRows(project: ProjectMeta, snapshot: TeamProjectSnapshot | null, account: string, legacyLinks: Record<string, string> = {}, actorId?: string, sharedId = snapshot?.id): UnifiedTask[] {
  const locals = unifyProjectTasks(project.tasks, project.conversations);
  const represented = new Set<string>();
  const rows: UnifiedTask[] = locals.map(local => {
    const link = local.sharedTasks?.[account];
    const remoteId = link && link.projectId === sharedId ? link.taskId : legacyLinks[local.id];
    const remote = snapshot?.tasks.find(task => task.id === remoteId);
    if (remote) represented.add(remote.id);
    const ownRun = remote ? snapshot?.runs?.find(run => run.taskId === remote.id && run.actorId === actorId) : undefined;
    return { id: local.id, title: remote?.title ?? local.title, local, remote, remoteId, sessionId: local.sessionId ?? ownRun?.sessionId, private: !remoteId && !(link && link.projectId === sharedId && link.requestId) };
  });
  for (const remote of snapshot?.tasks ?? []) {
    if (represented.has(remote.id)) continue;
    const ownRun = snapshot?.runs?.find(run => run.taskId === remote.id && run.actorId === actorId);
    const existing = ownRun && rows.find(row => row.sessionId === ownRun.sessionId);
    if (existing) { existing.remote = remote; existing.remoteId = remote.id; existing.private = false; existing.title = remote.title; }
    else rows.push({ id: `shared:${remote.id}`, title: remote.title, remote, remoteId: remote.id, sessionId: ownRun?.sessionId, private: false });
  }
  return rows;
}
export interface UnifiedAsset { id: string; name: string; local?: AssetItem; remote?: SharedAsset; previousLocals?: AssetItem[] }
export function projectAssetRows(project: ProjectMeta, snapshot: TeamProjectSnapshot | null, account: string, legacyLinks: Record<string, string> = {}): UnifiedAsset[] {
  const represented = new Set<string>();
  const rows: UnifiedAsset[] = [];
  for (const local of project.assets) {
    const link = local.sharedAssets?.[account];
    const remote = snapshot?.assets.find(asset => (link?.projectId === snapshot.id && ((link.familyId && asset.familyId === link.familyId) || asset.id === link.documentId)) || asset.id === legacyLinks[local.id]);
    if (remote) {
      const existing = rows.find(row => row.remote?.id === remote.id);
      if (existing) { existing.previousLocals = [...(existing.previousLocals ?? []), existing.local!]; existing.local = local; continue; }
      represented.add(remote.id);
    }
    rows.push({ id: local.id, name: remote?.title ?? local.name, local, remote });
  }
  for (const remote of snapshot?.assets ?? []) if (!represented.has(remote.id)) rows.push({ id: `shared:${remote.id}`, name: remote.title, remote });
  return rows;
}
