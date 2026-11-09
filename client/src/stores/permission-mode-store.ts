import { create } from "zustand";
import { permissionModeGet, type PermissionMode, type PermissionModeStatus } from "@/lib/agent-client";

type ProjectPermissionRead = { phase: "loading" | "ready" } | { phase: "failed"; message: string };

interface PermissionModeState {
  /** Selection for the one unsent task draft on Home; reset after creation. */
  homeMode: PermissionMode;
  /** Runtime/capability state keyed by existing task session id. */
  statuses: Record<string, PermissionModeStatus>;
  /** Organization/device capability envelope used by a new-task draft. */
  capabilityStatus: PermissionModeStatus | null;
  projectPermissionRead: ProjectPermissionRead | null;
  refreshProjectPermission: () => Promise<void>;
  setHomeMode: (mode: PermissionMode) => void;
  resetHomeMode: () => void;
  setStatus: (status: PermissionModeStatus) => void;
  clearSession: (sessionId: string) => void;
}

export const usePermissionModeStore = create<PermissionModeState>((set, get) => ({
  homeMode: "ask",
  statuses: {},
  capabilityStatus: null,
  projectPermissionRead: null,
  refreshProjectPermission: async () => {
    const reading: ProjectPermissionRead = { phase: "loading" };
    const previous = get().capabilityStatus;
    set({ projectPermissionRead: reading });
    try {
      const result = permissionModeStatusFromEvent(await permissionModeGet());
      if (!result || result.sessionId) throw new Error("任务默认权限状态无效");
      if (get().projectPermissionRead !== reading) return;
      const current = get().capabilityStatus;
      // A policy update published during the read owns the latest preview.
      set({ projectPermissionRead: { phase: "ready" }, capabilityStatus: current !== previous && current ? current : result });
    } catch (error) {
      if (get().projectPermissionRead !== reading) return;
      set({ projectPermissionRead: { phase: "failed", message: String(error).replace(/^Error:\s*/, "") } });
    }
  },
  setHomeMode: (homeMode) => set({ homeMode }),
  resetHomeMode: () => set({ homeMode: "ask" }),
  setStatus: (status) => set((state) => status.sessionId
    ? { statuses: { ...state.statuses, [status.sessionId]: status } }
    : { capabilityStatus: status }),
  clearSession: (sessionId) => set((state) => {
    if (!(sessionId in state.statuses)) return {};
    const statuses = { ...state.statuses };
    delete statuses[sessionId];
    return { statuses };
  }),
}));

export function projectPermissionUnavailableReason(status: PermissionModeStatus): string | undefined {
  if (!status.locked || status.configuredPermissionMode === status.permissionMode) return undefined;
  return (status.configuredPermissionMode === "auto" ? status.autoModeUnavailableReason : status.alwaysApproveUnavailableReason)
    || "组织锁定的任务默认权限当前不可用";
}

export function selectProjectPermissionMode(state: PermissionModeState): PermissionMode | undefined {
  const status = state.capabilityStatus;
  if (state.projectPermissionRead?.phase !== "ready" || !status || projectPermissionUnavailableReason(status)) return undefined;
  return status.permissionMode;
}

function asPermissionMode(value: unknown): PermissionMode | null {
  return value === "ask" || value === "auto" || value === "always-approve" ? value : null;
}

export function permissionModeFromEvent(payload: unknown): PermissionMode | null {
  if (!payload || typeof payload !== "object") return null;
  const value = payload as Record<string, unknown>;
  const mode = value.permission_mode ?? value.permissionMode ??
    (value.yolo_mode === true ? "always-approve" : value.auto_mode === true ? "auto" : undefined);
  return asPermissionMode(mode);
}

export function permissionModeStatusFromEvent(payload: unknown): PermissionModeStatus | null {
  if (!payload || typeof payload !== "object") return null;
  const value = payload as Record<string, unknown>;
  const hasStatusEnvelope = [
    "configuredPermissionMode",
    "configured_permission_mode",
    "autoModeAvailable",
    "auto_mode_available",
    "runtimeSyncState",
    "runtime_sync_state",
  ].some((key) => key in value);
  if (!hasStatusEnvelope) return null;
  const permissionMode = permissionModeFromEvent(payload);
  if (!permissionMode) return null;
  const configuredPermissionMode =
    asPermissionMode(value.configuredPermissionMode ?? value.configured_permission_mode) ??
    permissionMode;
  const runtimeAppliedMode = asPermissionMode(
    value.runtimeAppliedMode ?? value.runtime_applied_mode,
  ) ?? undefined;
  const rawSyncState = value.runtimeSyncState ?? value.runtime_sync_state;
  const runtimeSyncState =
    rawSyncState === "offline" || rawSyncState === "syncing" ||
    rawSyncState === "synced" || rawSyncState === "failed"
      ? rawSyncState
      : "offline";
  const rawSessionId = value.sessionId ?? value.session_id;
  const sessionId = typeof rawSessionId === "string" &&
      rawSessionId.trim() !== "" &&
      rawSessionId.length <= 256 &&
      !Array.from(rawSessionId).some((char) => /[\u0000-\u001f\u007f]/.test(char))
    ? rawSessionId
    : undefined;
  if (rawSessionId != null && !sessionId) return null;
  return {
    sessionId,
    permissionMode,
    configuredPermissionMode,
    autoModeAvailable: (value.autoModeAvailable ?? value.auto_mode_available) !== false,
    autoModeUnavailableReason:
      (value.autoModeUnavailableReason ?? value.auto_mode_unavailable_reason) as string | undefined,
    alwaysApproveAvailable:
      (value.alwaysApproveAvailable ?? value.always_approve_available) !== false,
    alwaysApproveUnavailableReason:
      (value.alwaysApproveUnavailableReason ??
        value.always_approve_unavailable_reason) as string | undefined,
    locked: (value.locked as boolean | undefined) ?? false,
    lockedReason: (value.lockedReason ?? value.locked_reason) as string | undefined,
    runtimeSyncState,
    runtimeAppliedMode,
    runtimeSyncError:
      (value.runtimeSyncError ?? value.runtime_sync_error) as string | undefined,
  };
}
