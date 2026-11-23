import { invoke } from "@tauri-apps/api/core";

const ARCHIVED_DRAFTS = "echo-coding-archive-v1";

export function isLegacyWorkspaceKey(key: string): boolean {
  return key === "echo-coding-workspaces"
    || key === "echo-coding-active-cwd"
    || key === ARCHIVED_DRAFTS
    || key.startsWith("echo-coding-intent-v1:")
    || key.startsWith("echo-coding-hot-exit-v1:task-draft:")
    || key.startsWith("echo-coding-hot-exit-v1:task-context:")
    || key.startsWith("echo-coding-followup-v1:")
    || key.startsWith("echo-coding-plan-v1:");
}

export function legacyWorkspaceStorageKeys(): string[] {
  return Array.from({ length: localStorage.length }, (_, index) => localStorage.key(index))
    .filter((key): key is string => !!key && isLegacyWorkspaceKey(key));
}

export function isRetiredQueueRow(owner: string, row: unknown): boolean {
  return owner.startsWith("coding:")
    || (row != null && typeof row === "object" && "coding" in row);
}

export const OUTBOX_KEY = "echoagent.outbox.v1";

function splitOutbox(raw: string): { active: string | null; archived?: string } {
  try {
    const value = JSON.parse(raw);
    if (value.version === 1 && value.data != null && typeof value.data === "object" && !Array.isArray(value.data)
      && Object.values(value.data).every(Array.isArray)) {
      const active = new Map<string, unknown[]>();
      const archived = new Map<string, unknown[]>();
      for (const [owner, rows] of Object.entries(value.data) as [string, unknown[]][]) {
        for (const row of rows) {
          const target = isRetiredQueueRow(owner, row) ? archived : active;
          const items = target.get(owner);
          if (items) items.push(row);
          else target.set(owner, [row]);
        }
      }
      if (!archived.size) return { active: raw };
      const encode = (rows: Map<string, unknown[]>) => JSON.stringify({ version: 1, data: Object.fromEntries(rows) });
      return { active: active.size ? encode(active) : null, archived: encode(archived) };
    }
  } catch {
    // Preserve the exact unreadable snapshot without trying to run any row.
  }
  return { active: null, archived: raw };
}

export function splitLegacyWorkspaceUi(records: Record<string, string>) {
  const archive = Object.fromEntries(Object.entries(records).filter(([key]) => isLegacyWorkspaceKey(key)));
  const outbox = records[OUTBOX_KEY];
  const split = outbox === undefined ? { active: null } : splitOutbox(outbox);
  if (split.archived !== undefined) archive[OUTBOX_KEY] = split.archived;
  return { archive, outbox: split.active };
}

export function localLegacyWorkspaceUi(): Record<string, string> {
  return Object.fromEntries([...legacyWorkspaceStorageKeys(), OUTBOX_KEY].flatMap(key => {
    const value = localStorage.getItem(key);
    return value === null ? [] : [[key, value]];
  }));
}

export async function exportLegacyWorkspaceArchive(): Promise<string | null> {
  const { archive } = splitLegacyWorkspaceUi(localLegacyWorkspaceUi());
  return invoke("backup_export_ui_archive", { kind: "legacyWorkspace", records: archive });
}

interface ExternalOperation {
  workspace: string;
  operationId: string;
  toolName: string;
}

export const pendingExternalOperations = () => invoke<ExternalOperation[]>("pending_external_operations");
export const legacyWorkspaceSessions = () => invoke<string[]>("legacy_workspace_sessions");
export const confirmExternalOperationStopped = (operationId: string, note: string) =>
  invoke<void>("confirm_external_operation_stopped", { operationId, note });
