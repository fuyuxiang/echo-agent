import { useCallback, useEffect, useSyncExternalStore } from "react";
import { create } from "zustand";
import type { OrgSession } from "./org-client";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { captureTeamProjectOperation, projectCollaborationError, projectConnectionIssue, teamProjectRequest, type TeamProject } from "./team-project-client";

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
  const linked = project.sharedProjects?.[account];
  const deleted = project.deletedSharedProjects?.[account];
  if (linked && linked !== deleted) return linked;
  if (deleted) return undefined;
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

interface SharedCatalogState { boundary: number; projects: TeamProject[]; deletedIds: ReadonlySet<string>; error: string; loading: boolean }
const emptyCatalog = (boundary: number, loading: boolean): SharedCatalogState => ({ boundary, projects: [], deletedIds: new Set(), error: "", loading });
const useCatalogState = create<SharedCatalogState>(() => emptyCatalog(-1, false));
let requestRevision = 0;

/** Apply a confirmed deletion only to the affected account's link. */
export function recordSharedProjectDeletion(boundary: number, account: string, id: string): boolean {
  if (useOrgSessionStore.getState().boundaryRevision === boundary) {
    useCatalogState.setState(previous => {
      const state = previous.boundary === boundary ? previous : emptyCatalog(boundary, false);
      if (state.deletedIds.has(id)) return state;
      return { ...state, projects: state.projects.filter(project => project.id !== id), deletedIds: new Set([...state.deletedIds, id]) };
    });
  }
  const store = useProjectsStore.getState();
  const localIds = new Set(store.projects.filter(project => sharedProjectId(project, account) === id).map(project => project.id));
  store.detachDeletedSharedProject(localIds, account, id);
  for (const localId of localIds) {
    try { localStorage.removeItem(publicationKey(localId, account)); } catch { /* The saved deletion also suppresses legacy recovery. */ }
  }
  return localIds.size > 0;
}

// Keep the last visible catalog across page navigation, but discard it at logout or an authorization change.
useOrgSessionStore.subscribe((state, previous) => {
  if (state.boundaryRevision === previous.boundaryRevision && state.session?.loggedIn === previous.session?.loggedIn) return;
  ++requestRevision;
  useCatalogState.setState(emptyCatalog(state.boundaryRevision, state.session?.loggedIn === true));
});

/** Shared metadata is memory-only and disappears immediately at an account boundary. */
export function useSharedProjectCatalog() {
  const session = useOrgSessionStore(state => state.session), boundary = useOrgSessionStore(state => state.boundaryRevision);
  const enabled = session?.loggedIn === true;
  const catalog = useCatalogState();
  const reload = useCallback(async () => {
    if (!enabled || useOrgSessionStore.getState().boundaryRevision !== boundary) return;
    const revision = ++requestRevision;
    const identity = captureTeamProjectOperation();
    useCatalogState.setState(previous => previous.boundary === boundary ? { loading: true } : emptyCatalog(boundary, true));
    try {
      const projects = await teamProjectRequest<TeamProject[]>("list", undefined, undefined, undefined, identity);
      identity.assertCurrent();
      if (!Array.isArray(projects)) throw new Error("共享项目列表格式不兼容，请联系管理员更新组织服务。个人项目仍可正常使用。");
      if (revision === requestRevision) useCatalogState.setState(state => ({
        projects: projects.filter(project => !state.deletedIds.has(project.id)), error: "", loading: false,
      }));
    } catch (error) {
      if (revision === requestRevision && useOrgSessionStore.getState().boundaryRevision === boundary) {
        const message = projectCollaborationError(error);
        useCatalogState.setState(previous => ({
          projects: projectConnectionIssue(message) === "connection" ? previous.projects : [],
          error: message,
          loading: false,
        }));
      }
    }
  }, [boundary, enabled]);
  useEffect(() => {
    void reload();
    const refresh = () => { if (document.visibilityState !== "hidden") void reload(); };
    window.addEventListener("focus", refresh);
    window.addEventListener("echo:project-sharing-changed", refresh);
    return () => { window.removeEventListener("focus", refresh); window.removeEventListener("echo:project-sharing-changed", refresh); };
  }, [reload]);
  return { ...(catalog.boundary === boundary && enabled ? catalog : emptyCatalog(boundary, enabled)), reload, enabled, account: projectSharingAccount(session) };
}
