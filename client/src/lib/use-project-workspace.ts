import { useCallback, useEffect, useRef, useState } from "react";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { captureTeamProjectOperation, projectCollaborationError, teamProjectRequest, type ProjectAction, type TeamProjectSnapshot } from "./team-project-client";

/** Shared state is never persisted in the local project or carried across accounts. */
export function useProjectWorkspace(projectId?: string) {
  const boundary = useOrgSessionStore(s => s.boundaryRevision);
  const loggedIn = useOrgSessionStore(s => s.session?.loggedIn);
  const key = `${boundary}:${projectId ?? ""}`;
  const [state, setState] = useState<{ key: string; snapshot: TeamProjectSnapshot | null; loading: boolean; error: string }>({ key, snapshot: null, loading: false, error: "" });
  const current = useRef(key); current.current = key;
  const sequence = useRef(0);
  const refresh = useCallback(async () => {
    if (!projectId || !loggedIn || current.current !== key || useOrgSessionStore.getState().boundaryRevision !== boundary) return;
    const version = ++sequence.current;
    const identity = captureTeamProjectOperation();
    setState(s => ({ key, snapshot: s.key === key ? s.snapshot : null, loading: true, error: "" }));
    try {
      const snapshot = await teamProjectRequest<TeamProjectSnapshot>("snapshot", projectId, undefined, undefined, identity);
      identity.assertCurrent();
      if (snapshot.id !== projectId || !Array.isArray(snapshot.tasks) || !Array.isArray(snapshot.assets) || !Array.isArray(snapshot.members)) throw new Error("项目数据格式不兼容，请更新组织服务");
      if (current.current === key && version === sequence.current) setState({ key, snapshot, loading: false, error: "" });
    } catch (error) {
      // Clear revoked or stale shared state. Linked rows remain locked until reauthorized.
      if (current.current === key && version === sequence.current) setState({ key, snapshot: null, loading: false, error: projectCollaborationError(error) });
    }
  }, [key, projectId, loggedIn, boundary]);
  useEffect(() => {
    void refresh();
    const update = () => { if (document.visibilityState !== "hidden") void refresh(); };
    const timer = projectId && loggedIn ? window.setInterval(update, 15000) : undefined;
    window.addEventListener("focus", update);
    return () => { ++sequence.current; clearInterval(timer); window.removeEventListener("focus", update); };
  }, [refresh, projectId, loggedIn]);
  const mutate = async <T,>(action: ProjectAction, body?: unknown, resourceId?: string): Promise<T> => {
    if (!projectId) throw new Error("项目尚未开启协作");
    if (current.current !== key || useOrgSessionStore.getState().boundaryRevision !== boundary) throw new Error("组织身份已变化，请重新打开项目");
    const identity = captureTeamProjectOperation();
    const value = await teamProjectRequest<T>(action, projectId, body, resourceId, identity);
    identity.assertCurrent();
    if (current.current !== key) throw new Error("项目已切换，请重新打开");
    void refresh();
    return value;
  };
  return { ...(state.key === key && loggedIn ? state : { snapshot: null, loading: false, error: "" }), refresh, mutate };
}
export type ProjectWorkspace = ReturnType<typeof useProjectWorkspace>;
