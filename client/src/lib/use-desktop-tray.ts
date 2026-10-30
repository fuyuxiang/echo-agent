import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useSessionsStore } from "@/stores/sessions-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";
import { useGroupStore } from "@/features/groups/group-store";
import { readDurable, writeDurable } from "./durable-ui-state";
import { buildTraySnapshot, recordTrayVisit, type TrayAction } from "./desktop-tray-model";
import { connectDesktopTray } from "./desktop-tray-connection";
import { automationPendingApprovals, onAutomationApproval, onAutomationApprovalClosed } from "./automation-client";

const VISITS_KEY = "echoagent.tray-visits.v1";
function isVisits(value: unknown): value is Record<string, number> {
  return !!value && typeof value === "object" && !Array.isArray(value)
    && Object.entries(value).length <= 100
    && Object.entries(value).every(([key, time]) => key.length <= 300 && /^(session|group):./.test(key)
      && typeof time === "number" && Number.isFinite(time) && time >= 0);
}

export function useDesktopTray(ready: boolean, onAction: (action: TrayAction) => Promise<void>, onError: (error: unknown, retry: () => void) => void) {
  const callbacks = useRef({ onAction, onError });
  callbacks.current = { onAction, onError };
  useEffect(() => {
    if (!ready || !("__TAURI_INTERNALS__" in window)) return;
    let visits = readDurable<Record<string, number>>(VISITS_KEY, {}, isVisits);
    let disposed = false, approvalRevision = 0, approvalRefreshRevision = 0;
    const automation = new Map<string, { sessionId: string; revision: number }>();
    const closed = new Set<string>();
    const snapshot = () => {
      const pending = new Set<string>();
      for (const approval of automation.values()) pending.add(approval.sessionId);
      for (const queues of [usePermissionStore.getState().queues, useQuestionStore.getState().queues]) {
        for (const [id, requests] of Object.entries(queues)) if (requests.length) pending.add(id);
      }
      const groups = useGroupStore.getState();
      const catalog = groups.current ? groups.groups.map(group => group.id === groups.current!.id && groups.current!.revision >= group.revision ? groups.current! : group) : groups.groups;
      return buildTraySnapshot(useSessionsStore.getState().independent, catalog, pending, visits);
    };
    const connection = connectDesktopTray({ invoke, listen, snapshot,
      onAction: action => callbacks.current.onAction(action),
      onError: (error, retry) => callbacks.current.onError(error, retry),
    });
    const visit = (kind: "session" | "group", id: string | null) => {
      if (!id) return;
      visits = recordTrayVisit(visits, { kind, id });
      writeDurable(VISITS_KEY, visits);
    };
    const stops = [
      useSessionsStore.subscribe((state, previous) => {
        if (state.currentSessionId !== previous.currentSessionId) visit("session", state.currentSessionId);
        if (state.independent !== previous.independent || state.currentSessionId !== previous.currentSessionId) connection.refresh();
      }),
      usePermissionStore.subscribe((state, previous) => { if (state.queues !== previous.queues) connection.refresh(); }),
      useQuestionStore.subscribe((state, previous) => { if (state.queues !== previous.queues) connection.refresh(); }),
      useGroupStore.subscribe((state, previous) => {
        if (state.currentId !== previous.currentId) visit("group", state.currentId);
        if (state.groups !== previous.groups || state.current !== previous.current || state.currentId !== previous.currentId) connection.refresh();
      }),
    ];
    const refreshApprovals = async () => {
      const refreshRevision = ++approvalRefreshRevision;
      const revision = approvalRevision;
      // Only events received after this read started can supplement its result.
      // Older entries absent from the authoritative snapshot must disappear.
      try {
        const pending = await automationPendingApprovals();
        if (disposed || refreshRevision !== approvalRefreshRevision) return;
        const arrived = [...automation].filter(([, item]) => item.revision > revision);
        automation.clear();
        for (const item of pending) if (!closed.has(item.requestId)) automation.set(item.requestId, { sessionId: item.sessionId, revision });
        for (const [id, item] of arrived) if (!closed.has(id)) automation.set(id, item);
        connection.refresh();
      } catch { /* Optional automation service may be unavailable; its main UI exposes that capability state. */ }
    };
    // Global approvals include background browser/computer tasks, not just the
    // currently mounted ChatView. Subscribe before taking the recovery snapshot.
    void Promise.allSettled([
      onAutomationApproval(request => {
        if (disposed || closed.has(request.requestId)) return;
        automation.set(request.requestId, { sessionId: request.sessionId, revision: ++approvalRevision }); connection.refresh();
      }),
      onAutomationApprovalClosed(event => {
        if (disposed) return;
        automation.delete(event.requestId); closed.add(event.requestId);
        if (closed.size > 256) closed.delete(closed.values().next().value!);
        connection.refresh();
      }),
    ]).then(results => {
      for (const result of results) if (result.status === "fulfilled") { if (disposed) result.value(); else stops.push(result.value); }
      if (!disposed) void refreshApprovals();
    });
    const focus = () => { connection.refresh(); connection.wake(); void refreshApprovals(); };
    window.addEventListener("focus", focus);
    return () => { disposed = true; stops.forEach(stop => stop()); window.removeEventListener("focus", focus); connection.dispose(); };
  }, [ready]);
}
