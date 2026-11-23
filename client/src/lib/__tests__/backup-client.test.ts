import { beforeEach, afterEach, expect, it, vi } from "vitest";
const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { exportBackup, restoreUiBeforeBootstrap, BACKUP_UI_KEYS } from "../backup-client";
beforeEach(() => { localStorage.clear(); invoke.mockReset(); Object.defineProperty(window, "__TAURI_INTERNALS__", { value: {}, configurable: true }); });
afterEach(() => { Reflect.deleteProperty(window, "__TAURI_INTERNALS__"); vi.restoreAllMocks(); });
it("exports only the reviewed UI allowlist", async () => {
  localStorage.setItem("echoagent.drafts.v1", "draft");
  localStorage.setItem("organization-token", "secret");
  await exportBackup();
  expect(invoke).toHaveBeenCalledWith("backup_export", { uiState: { "echoagent.drafts.v1": "draft" } });
});
it("restores before stores hydrate and acknowledges only after success", async () => {
  invoke.mockResolvedValueOnce({ "echoagent.drafts.v1": "new" });
  await restoreUiBeforeBootstrap();
  expect(localStorage.getItem("echoagent.drafts.v1")).toBe("new");
  expect(invoke).toHaveBeenLastCalledWith("backup_acknowledge_ui");
});
it("defers the native recovery snapshot when storage is full and keeps the existing drafts", async () => {
  localStorage.setItem("echoagent.drafts.v1", "old");
  localStorage.setItem("echoagent.projects.pending-backend-sync", "1");
  invoke.mockResolvedValueOnce({ "echoagent.drafts.v1": "new" });
  const persist = Storage.prototype.setItem;
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (this: Storage, key, value) {
    if (value === "new") throw new DOMException("full", "QuotaExceededError");
    persist.call(this, key, value);
  });
  await expect(restoreUiBeforeBootstrap()).resolves.toBe("deferred");
  expect(localStorage.getItem("echoagent.drafts.v1")).toBe("old");
  expect(localStorage.getItem("echoagent.projects.pending-backend-sync")).toBeNull();
  expect(invoke).not.toHaveBeenCalledWith("backup_acknowledge_ui");
  expect(invoke).toHaveBeenLastCalledWith("backup_defer_restored_ui");
});
it("Rust and renderer serialize the same backup UI allowlist", async () => {
  const { readFileSync } = await import("node:fs");
  const source = readFileSync("src-tauri/src/backup.rs", "utf8");
  const section = source.slice(source.indexOf("pub const UI_KEYS"), source.indexOf("#[derive", source.indexOf("pub const UI_KEYS")));
  expect([...section.matchAll(/"([^"]+)"/g)].map((match) => match[1])).toEqual([...BACKUP_UI_KEYS]);
});

const queueSnapshot = (owners: Record<string, unknown[]>) => JSON.stringify({ version: 1, data: owners });

it("replaces the full restored snapshot and clears drafts absent from the backup", async () => {
  localStorage.setItem("echoagent.outbox.v1", queueSnapshot({ local: [{ id: "pending", text: "stale queued message" }] }));
  localStorage.setItem("echoagent.drafts.v1", "stale draft");
  localStorage.setItem("organization-token", "retain");
  invoke.mockResolvedValueOnce({ "echoagent.theme": "dark" });
  await restoreUiBeforeBootstrap();
  expect(localStorage.getItem("echoagent.outbox.v1")).toBeNull();
  expect(localStorage.getItem("echoagent.drafts.v1")).toBeNull();
  expect(localStorage.getItem("organization-token")).toBe("retain");
});
it("rolls back keys removed from the snapshot when acknowledgement fails", async () => {
  localStorage.setItem("echoagent.outbox.v1", queueSnapshot({ local: [{ id: "pending", text: "queued message" }] }));
  localStorage.setItem("echoagent.projects.pending-backend-sync", "pending");
  invoke.mockResolvedValueOnce({ "echoagent.theme": "dark" }).mockRejectedValueOnce(new Error("ack failed"));
  await expect(restoreUiBeforeBootstrap()).rejects.toThrow("ack failed");
  expect(localStorage.getItem("echoagent.outbox.v1")).toBe(queueSnapshot({ local: [{ id: "pending", text: "queued message" }] }));
  expect(localStorage.getItem("echoagent.projects.pending-backend-sync")).toBe("pending");
  expect(localStorage.getItem("echoagent.theme")).toBeNull();
});


it("archives both versions of a legacy plan and keeps retired rows out of the restored queue", async () => {
  const outbox = "echoagent.outbox.v1";
  const localArchive = { id: "retired-local", text: "本机旧消息", coding: { mode: "plan" } };
  const backupArchive = { id: "retired-backup", text: "备份旧消息", attachments: ["/archive/附件.docx"] };
  const draft = "echo-coding-plan-v1:old";
  localStorage.setItem(outbox, queueSnapshot({ session: [localArchive, { id: "stale" }] }));
  localStorage.setItem(draft, "本机旧草稿");
  const restored = {
    [outbox]: queueSnapshot({ "coding:old-owner": [backupArchive], session: [{ id: "active-backup" }] }),
    [draft]: "备份旧草稿",
  };
  invoke.mockResolvedValueOnce(restored);
  await restoreUiBeforeBootstrap();
  expect(invoke.mock.calls.filter(([command]) => command === "backup_archive_legacy_ui")).toEqual([
    ["backup_archive_legacy_ui", { records: { [draft]: "本机旧草稿", [outbox]: queueSnapshot({ session: [localArchive] }) } }],
    ["backup_archive_legacy_ui", { records: { [draft]: "备份旧草稿", [outbox]: queueSnapshot({ "coding:old-owner": [backupArchive] }) } }],
  ]);
  expect(JSON.parse(localStorage.getItem(outbox)!).data).toEqual({ session: [{ id: "active-backup" }] });
  expect(localStorage.getItem(draft)).toBeNull();
});

it("keeps the original draft and outbox if archiving during ordinary startup fails", async () => {
  const draft = "echo-coding-plan-v1:old";
  const outbox = "echoagent.outbox.v1";
  const snapshot = queueSnapshot({ "coding:old": [{ id: "retired" }] });
  localStorage.setItem(draft, "本机旧草稿");
  localStorage.setItem(outbox, snapshot);
  invoke.mockResolvedValueOnce(null).mockRejectedValueOnce(new Error("write failed"));
  await expect(restoreUiBeforeBootstrap()).resolves.toBeUndefined();
  expect(localStorage.getItem(draft)).toBe("本机旧草稿");
  expect(localStorage.getItem(outbox)).toBe(snapshot);
});

it("defers a pending restore when the legacy archive write fails", async () => {
  const draft = "echo-coding-plan-v1:old";
  localStorage.setItem(draft, "本机旧草稿");
  localStorage.setItem("echoagent.drafts.v1", "本机会话草稿");
  invoke.mockResolvedValueOnce({ [draft]: "远端旧草稿" })
    .mockRejectedValueOnce(new Error("archive unavailable"));
  await expect(restoreUiBeforeBootstrap()).resolves.toBe("deferred");
  expect(localStorage.getItem(draft)).toBe("本机旧草稿");
  expect(localStorage.getItem("echoagent.drafts.v1")).toBe("本机会话草稿");
  expect(invoke).toHaveBeenLastCalledWith("backup_defer_restored_ui");
});

it("archives a plan on upgrade and releases its storage before restoring active drafts", async () => {
  const draft = "echo-coding-plan-v1:old";
  const localDraft = "a".repeat(2000);
  const previous = "echoagent.group-drafts.v1";
  const active = "b".repeat(2000);
  localStorage.setItem(draft, localDraft);
  localStorage.setItem(previous, active);
  invoke.mockResolvedValueOnce({ "echoagent.drafts.v1": active });
  const persist = Storage.prototype.setItem;
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (this: Storage, key, value) {
    if (this.getItem(draft) || this.getItem(previous)) throw new DOMException("full", "QuotaExceededError");
    persist.call(this, key, value);
  });
  await expect(restoreUiBeforeBootstrap()).resolves.toBeUndefined();
  expect(invoke).toHaveBeenCalledWith("backup_archive_legacy_ui", { records: { [draft]: localDraft } });
  expect(localStorage.getItem("echoagent.drafts.v1")).toBe(active);
  expect(invoke).not.toHaveBeenCalledWith("backup_defer_restored_ui");
});
