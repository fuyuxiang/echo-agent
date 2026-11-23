import { invoke } from "@tauri-apps/api/core";
import { isLegacyWorkspaceKey, legacyWorkspaceStorageKeys, localLegacyWorkspaceUi, OUTBOX_KEY, splitLegacyWorkspaceUi } from "./legacy-workspace";

const PROJECT_SYNC_KEY = "echoagent.projects.pending-backend-sync";

export const BACKUP_UI_KEYS = [
  "echoagent.projects", "echoagent.drafts.v1", "echoagent.recovery-session.v1", "echoagent.group-drafts.v1", "echoagent.group-read.v1", "echoagent.group-outbox.v1", "echoagent.outbox.v1",
  "echoagent.draft-attachments.v1", "echoagent.task-artifacts.v1", "echo.meeting-drafts.v1", "echo.meeting-drafts.v1.recovery",
  "echoagent.usage", "echoagent.quota", "echoagent.usage-snapshots.v1",
  "echoagent.theme", "echoagent.fontSize",
] as const;
export interface BackupPreview { token: string; createdAt: string; fileCount: number; totalBytes: number; uiKeys: string[] }
export function exportBackup(): Promise<string | null> {
  const uiState: Record<string, string> = {};
  for (const key of [...BACKUP_UI_KEYS, ...legacyWorkspaceStorageKeys()]) {
    const raw = localStorage.getItem(key);
    if (raw !== null) uiState[key] = raw;
  }
  return invoke("backup_export", { uiState });
}
export const inspectBackup = () => invoke<BackupPreview | null>("backup_inspect");
export const restoreBackup = (token: string) => invoke<void>("backup_restore", { token });
export const backupLastError = () => invoke<string | null>("backup_last_error");
export const hasDeferredUiArchive = () => invoke<boolean>("backup_has_deferred_ui");
export const exportDeferredUiArchive = () => invoke<string | null>("backup_export_ui_archive", { kind: "deferredRestore", records: {} });

async function archiveLegacyUi(archive: Record<string, string>) {
  if (Object.keys(archive).length) await invoke("backup_archive_legacy_ui", { records: archive });
}

async function migrateLocalLegacyUi() {
  const local = localLegacyWorkspaceUi();
  const { archive, outbox } = splitLegacyWorkspaceUi(local);
  await archiveLegacyUi(archive);
  // Only remove the browser copies after the native write succeeds.
  for (const key of Object.keys(archive)) {
    if (localStorage.getItem(key) !== local[key]) continue;
    if (key === OUTBOX_KEY && outbox !== null) localStorage.setItem(key, outbox);
    else localStorage.removeItem(key);
  }
}

async function deferUiRestore(): Promise<"deferred"> {
  // Native project data has already been restored. Never replay a previous
  // renderer cache over it when the user continues into the application.
  localStorage.removeItem(PROJECT_SYNC_KEY);
  await invoke("backup_defer_restored_ui");
  return "deferred";
}

/** Must run before importing stores: module initialization reads persisted drafts. */
export async function restoreUiBeforeBootstrap(): Promise<"deferred" | void> {
  if (!("__TAURI_INTERNALS__" in window)) return;
  const restored = await invoke<Record<string, string> | null>("backup_restored_ui");
  try {
    await migrateLocalLegacyUi();
  } catch {
    // Ordinary startup can still use the existing outbox and export the original drafts.
    if (restored === null) return;
    return deferUiRestore();
  }
  if (restored === null) return;
  const entries = Object.entries(restored);
  if (entries.some(([key, value]) => (!BACKUP_UI_KEYS.includes(key as typeof BACKUP_UI_KEYS[number]) && !isLegacyWorkspaceKey(key)) || typeof value !== "string")) {
    throw new Error("恢复的界面数据格式无效，已保留恢复文件");
  }
  const { archive, outbox } = splitLegacyWorkspaceUi(restored);
  try {
    await archiveLegacyUi(archive);
  } catch {
    return deferUiRestore();
  }
  const keys = [...BACKUP_UI_KEYS, PROJECT_SYNC_KEY];
  const previous = new Map(keys.map((key) => [key, localStorage.getItem(key)]));
  try {
    // Release the old storage first. Applying one key at a time over an old
    // snapshot can exceed quota even when the final snapshot fits on its own.
    for (const key of keys) localStorage.removeItem(key);
    for (const key of BACKUP_UI_KEYS) {
      const value = key === OUTBOX_KEY ? outbox : restored[key];
      if (value != null) localStorage.setItem(key, value);
    }
    await invoke("backup_acknowledge_ui");
  } catch (error) {
    for (const key of keys) localStorage.removeItem(key);
    for (const [key, value] of previous) {
      if (value !== null) localStorage.setItem(key, value);
    }
    if (!(error instanceof DOMException) || error.name !== "QuotaExceededError") throw error;
    return deferUiRestore();
  }
}
