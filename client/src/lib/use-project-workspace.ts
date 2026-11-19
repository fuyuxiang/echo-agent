import { useCallback, useEffect, useRef, useState } from "react";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { projectSharingAccount, recordSharedProjectDeletion } from "./project-sharing";
import { captureTeamProjectOperation, projectCollaborationError, projectConnectionIssue, teamProjectRequest, type ProjectAction, type TeamProjectSnapshot } from "./team-project-client";

export interface CheckedProjectSnapshot {
  boundary: number;
  snapshot: TeamProjectSnapshot;
}

export async function loadTeamProjectSnapshot(projectId: string, operation = captureTeamProjectOperation()): Promise<TeamProjectSnapshot> {
  const snapshot = await teamProjectRequest<TeamProjectSnapshot>("snapshot", projectId, undefined, undefined, operation);
  operation.assertCurrent();
  if (snapshot?.id !== projectId || !Array.isArray(snapshot.tasks) || !Array.isArray(snapshot.assets) || !Array.isArray(snapshot.members)) {
    throw new Error("项目数据格式不兼容，请更新组织服务");
  }
  return snapshot;
}

/** Shared state is never persisted in the local project or carried across accounts. */
export function useProjectWorkspace(projectId?: string, checked?: CheckedProjectSnapshot) {
  const boundary = useOrgSessionStore(s => s.boundaryRevision);
  const loggedIn = useOrgSessionStore(s => s.session?.loggedIn);
  const key = `${boundary}:${projectId ?? ""}`;
  const initialSnapshot = loggedIn && checked?.boundary === boundary && checked.snapshot.id === projectId ? checked.snapshot : null;
  const checkedKey = useRef(initialSnapshot ? key : null);
  const [state, setState] = useState<{ key: string; snapshot: TeamProjectSnapshot | null; loading: boolean; error: string }>({ key, snapshot: initialSnapshot, loading: false, error: "" });
  const current = useRef(key); current.current = key;
  const sequence = useRef(0);
  const refresh = useCallback(async () => {
    if (!projectId || !loggedIn || current.current !== key || useOrgSessionStore.getState().boundaryRevision !== boundary) return;
    const version = ++sequence.current;
    const identity = captureTeamProjectOperation();
    const account = projectSharingAccount(useOrgSessionStore.getState().session);
    setState(s => ({ key, snapshot: s.key === key ? s.snapshot : null, loading: true, error: "" }));
    try {
      const snapshot = await loadTeamProjectSnapshot(projectId, identity);
      if (current.current === key && version === sequence.current) setState({ key, snapshot, loading: false, error: "" });
    } catch (error) {
      if (current.current !== key || version !== sequence.current || useOrgSessionStore.getState().boundaryRevision !== boundary) return;
      const message = projectCollaborationError(error);
      if (projectConnectionIssue(message) === "deleted") recordSharedProjectDeletion(boundary, account, projectId);
      setState({ key, snapshot: null, loading: false, error: message });
    }
  }, [key, projectId, loggedIn, boundary]);
  useEffect(() => {
    if (checkedKey.current !== key) {
      checkedKey.current = null;
      void refresh();
    }
    const update = () => { if (document.visibilityState !== "hidden") void refresh(); };
    const timer = projectId && loggedIn ? window.setInterval(update, 15000) : undefined;
    window.addEventListener("focus", update);
    return () => { ++sequence.current; clearInterval(timer); window.removeEventListener("focus", update); };
  }, [key, refresh, projectId, loggedIn]);
  const mutate = async <T,>(action: ProjectAction, body?: unknown, resourceId?: string): Promise<T> => {
    if (!projectId) throw new Error("项目尚未开启协作");
    if (current.current !== key || useOrgSessionStore.getState().boundaryRevision !== boundary) throw new Error("组织身份已变化，请重新打开项目");
    const identity = captureTeamProjectOperation();
    const account = projectSharingAccount(useOrgSessionStore.getState().session);
    try {
      const value = await teamProjectRequest<T>(action, projectId, body, resourceId, identity);
      identity.assertCurrent();
      if (current.current !== key) throw new Error("项目已切换，请重新打开");
      void refresh();
      return value;
    } catch (error) {
      if (projectConnectionIssue(projectCollaborationError(error)) === "deleted") {
        identity.assertCurrent();
        recordSharedProjectDeletion(boundary, account, projectId);
      }
      throw error;
    }
  };
  return { ...(state.key === key && loggedIn ? state : { snapshot: null, loading: false, error: "" }), refresh, mutate };
}
export type ProjectWorkspace = ReturnType<typeof useProjectWorkspace>;
