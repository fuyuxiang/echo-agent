import { useCallback, useEffect, useMemo, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  automationPendingApprovals, automationStatus,
  onAutomationApproval, onAutomationApprovalClosed, onAutomationStatus,
  type AutomationApproval, type AutomationStatus,
} from "./automation-client";
import { friendlyError } from "./error-format";

export interface AutomationSessionState {
  status: AutomationStatus | null;
  approvals: AutomationApproval[];
  error: string | null;
  isActive: () => boolean;
  refresh: () => Promise<void>;
  reconnect: () => void;
  setStatus: (status: AutomationStatus) => void;
  removeApproval: (requestId: string) => void;
}

/** Session-owned data, without a mode-specific toolbar. Each asynchronous
 * operation retains its owner so old events cannot affect a newly opened chat. */
export function useSessionAutomation(sessionId: string | null): AutomationSessionState {
  const [connection, setConnection] = useState(0);
  const reconnect = useCallback(() => setConnection(value => value + 1), []);
  const owner = useMemo(() => ({ sessionId, active: false, statusRevision: 0,
    approvalRevision: 0, refreshRevision: 0, epoch: 0, closed: new Set<string>() }), [sessionId, connection]);
  const [snapshot, setSnapshot] = useState<{ owner: typeof owner; status: AutomationStatus | null;
    approvals: AutomationApproval[]; error: string | null }>({ owner, status: null, approvals: [], error: null });
  const update = useCallback((change: (current: typeof snapshot) => typeof snapshot) => {
    if (!owner.active) return;
    setSnapshot(current => change(current.owner === owner ? current : { owner, status: null, approvals: [], error: null }));
  }, [owner]);
  const isActive = useCallback(() => owner.active, [owner]);
  const applyStatus = useCallback((status: AutomationStatus) => {
    if (!owner.active || status.sessionId !== sessionId) return;
    owner.statusRevision++;
    update(current => ({ ...current, status }));
  }, [owner, sessionId, update]);
  const removeApproval = useCallback((requestId: string) => {
    owner.approvalRevision++;
    owner.closed.add(requestId);
    if (owner.closed.size > 256) owner.closed.delete(owner.closed.values().next().value!);
    update(current => ({ ...current, approvals: current.approvals.filter(item => item.requestId !== requestId) }));
  }, [owner, update]);
  const refresh = useCallback(async () => {
    if (!sessionId || !("__TAURI_INTERNALS__" in window)) return;
    const epoch = owner.epoch;
    const revision = ++owner.refreshRevision;
    const statusRevision = owner.statusRevision;
    const approvalRevision = owner.approvalRevision;
    const isCurrent = () => owner.active && epoch === owner.epoch && revision === owner.refreshRevision;
    // A busy browser may delay its status snapshot. Approval requests must
    // still appear immediately so the operation can receive a decision.
    const [statusResult, approvalsResult] = await Promise.allSettled([
      automationStatus(sessionId).then(status => {
        if (isCurrent() && owner.statusRevision === statusRevision && status.sessionId === sessionId) {
          update(current => ({ ...current, status }));
        }
      }),
      automationPendingApprovals(sessionId).then(approvals => {
        if (!isCurrent()) return;
        const pending = approvals.filter(item => item.sessionId === sessionId && !owner.closed.has(item.requestId));
        update(current => ({ ...current, approvals: owner.approvalRevision === approvalRevision ? pending : [...pending,
          ...current.approvals.filter(item => !pending.some(entry => entry.requestId === item.requestId) && !owner.closed.has(item.requestId))] }));
      }),
    ]);
    if (!isCurrent()) return;
    const failure = [statusResult, approvalsResult].find(result => result.status === "rejected");
    update(current => ({ ...current, error: failure?.status === "rejected" ? friendlyError(failure.reason) : null }));
  }, [owner, sessionId, update]);

  useEffect(() => {
    owner.active = true;
    owner.epoch++;
    let disposed = false;
    const unlisteners: UnlistenFn[] = [];
    if (!sessionId || !("__TAURI_INTERNALS__" in window)) return () => { owner.active = false; };
    const fail = (error: unknown) => { if (!disposed) update(current => ({ ...current, error: friendlyError(error) })); };
    void (async () => {
      const registered = await Promise.allSettled([
        onAutomationStatus(status => { if (!disposed) applyStatus(status); }),
        onAutomationApproval(request => {
          if (disposed || !owner.active || request.sessionId !== sessionId || owner.closed.has(request.requestId)) return;
          owner.approvalRevision++;
          update(current => ({ ...current, approvals: current.approvals.some(item => item.requestId === request.requestId)
            ? current.approvals : [...current.approvals, request] }));
        }),
        onAutomationApprovalClosed(event => {
          if (!disposed && owner.active && event.sessionId === sessionId) removeApproval(event.requestId);
        }),
      ]);
      const listeners = registered.flatMap(result => result.status === "fulfilled" ? [result.value] : []);
      const failed = registered.find(result => result.status === "rejected");
      if (disposed || !owner.active || failed) {
        listeners.forEach(unlisten => unlisten());
        if (failed?.status === "rejected") fail(failed.reason);
        return;
      }
      unlisteners.push(...listeners);
      await refresh();
    })().catch(fail);
    const refreshIfVisible = () => { if (document.visibilityState === "visible") void refresh().catch(fail); };
    window.addEventListener("focus", refreshIfVisible);
    document.addEventListener("visibilitychange", refreshIfVisible);
    return () => {
      disposed = true;
      owner.epoch++;
      owner.active = false;
      unlisteners.forEach(unlisten => unlisten());
      window.removeEventListener("focus", refreshIfVisible);
      document.removeEventListener("visibilitychange", refreshIfVisible);
    };
  }, [owner, sessionId, applyStatus, removeApproval, refresh, update]);

  const current = snapshot.owner === owner ? snapshot : { status: null, approvals: [], error: null };
  return { ...current, isActive, refresh, reconnect, setStatus: applyStatus, removeApproval };
}
