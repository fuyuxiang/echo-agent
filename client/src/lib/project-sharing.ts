import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { OrgSession } from "./org-client";
import type { ProjectMeta } from "@/stores/projects-store";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { captureTeamProjectOperation, projectCollaborationError, teamProjectRequest, type TeamProject } from "./team-project-client";

/** Matches the legacy publication key so existing shared projects are never recreated. */
export function projectSharingAccount(session: OrgSession | null): string {
  return session?.accountId ?? JSON.stringify([session?.serverUrl, session?.user?.id ?? session?.bootstrap?.user.id]);
}
export const publicationKey = (projectId: string, account: string) => `echoagent.team-publication.v1:${account}:${projectId}`;
export function projectPublicationLinks(projectId: string, account: string, sharedId?: string): { tasks: Record<string, string>; assets: Record<string, string> } {
  try {
    const value = JSON.parse(localStorage.getItem(publicationKey(projectId, account)) ?? "null");
    if (!sharedId || value?.projectId !== sharedId) return { tasks: {}, assets: {} };
    const strings = (map: unknown): Record<string, string> => map && typeof map === "object" && !Array.isArray(map) ? Object.fromEntries(Object.entries(map).filter((entry): entry is [string, string] => typeof entry[1] === "string")) : {};
    return { tasks: strings(value.createdTasks), assets: strings(value.createdAssets) };
  } catch { return { tasks: {}, assets: {} }; }
}
export function sharedProjectId(project: ProjectMeta, account: string): string | undefined {
  if (project.sharedProjects?.[account]) return project.sharedProjects[account];
  try { const value = JSON.parse(localStorage.getItem(publicationKey(project.id, account)) ?? "null"); return typeof value?.projectId === "string" ? value.projectId : undefined; }
  catch { return undefined; }
}
export function notifyProjectSharing() { window.dispatchEvent(new Event("echo:project-sharing-changed")); }
const subscribe = (listener: () => void) => {
  window.addEventListener("echo:project-sharing-changed", listener);
  window.addEventListener("storage", listener);
  return () => { window.removeEventListener("echo:project-sharing-changed", listener); window.removeEventListener("storage", listener); };
};
export function useSharedProjectId(project: ProjectMeta): string | undefined {
  const session = useOrgSessionStore(state => state.session);
  const account = projectSharingAccount(session);
  return useSyncExternalStore(subscribe, () => session?.loggedIn ? sharedProjectId(project, account) : undefined);
}
/** Shared metadata is memory-only and disappears immediately at an account boundary. */
export function useSharedProjectCatalog() {
  const session = useOrgSessionStore(state => state.session), boundary = useOrgSessionStore(state => state.boundaryRevision);
  const [catalog, setCatalog] = useState<{ boundary: number; projects: TeamProject[]; error: string; loading: boolean }>({ boundary, projects: [], error: "", loading: false });
  const requestRevision = useRef(0);
  const reload = useCallback(async () => {
    if (!session?.loggedIn) return;
    const revision = ++requestRevision.current;
    const identity = captureTeamProjectOperation();
    setCatalog(previous => ({ ...previous, loading: true }));
    try {
      const projects = await teamProjectRequest<TeamProject[]>("list", undefined, undefined, undefined, identity);
      identity.assertCurrent();
      if (!Array.isArray(projects)) throw new Error("共享项目列表格式不兼容，请联系管理员更新组织服务。个人项目仍可正常使用。");
      if (revision === requestRevision.current) setCatalog({ boundary, projects, error: "", loading: false });
    } catch (error) {
      if (revision === requestRevision.current && useOrgSessionStore.getState().boundaryRevision === boundary) setCatalog({ boundary, projects: [], error: projectCollaborationError(error), loading: false });
    }
  }, [boundary, session?.loggedIn]);
  useEffect(() => {
    setCatalog({ boundary, projects: [], error: "", loading: false });
    void reload();
    const refresh = () => { if (document.visibilityState !== "hidden") void reload(); };
    window.addEventListener("focus", refresh);
    window.addEventListener("echo:project-sharing-changed", refresh);
    return () => { window.removeEventListener("focus", refresh); window.removeEventListener("echo:project-sharing-changed", refresh); };
  }, [boundary, reload]);
  return { ...(catalog.boundary === boundary && session?.loggedIn ? catalog : { projects: [], error: "", loading: false }), reload, account: projectSharingAccount(session) };
}
