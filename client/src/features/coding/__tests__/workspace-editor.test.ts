import { describe, expect, it, vi } from "vitest";
import { EditorPreparationCancelled, prepareWorkspaceEditor, registerWorkspaceEditor, withWorkspaceDiskMutation, workspaceEditor } from "../lib/workspace-editor";

describe("workspace editor execution boundary", () => {
  it("queries only the requested workspace and refuses unattended dirty verification", async () => {
    const a = {getDirtyCount: vi.fn(async () => 1), saveAll: vi.fn(async () => true)};
    const b = {getDirtyCount: vi.fn(async () => 0), saveAll: vi.fn(async () => true)};
    const removeA = registerWorkspaceEditor("/a", a); const removeB = registerWorkspaceEditor("/b", b);
    try { await expect(prepareWorkspaceEditor("/a")).rejects.toThrow("未保存"); expect(a.saveAll).not.toHaveBeenCalled(); expect(b.getDirtyCount).not.toHaveBeenCalled(); }
    finally { removeA(); removeB(); }
  });
  it("cancel and failed ACK preserve the buffer and stop execution", async () => {
    const editor = {getDirtyCount: vi.fn(async () => 1), saveAll: vi.fn(async () => false)};
    const remove = registerWorkspaceEditor("/cancel", editor);
    try {
      await expect(prepareWorkspaceEditor("/cancel", async () => false)).rejects.toBeInstanceOf(EditorPreparationCancelled);
      expect(editor.saveAll).not.toHaveBeenCalled();
      await expect(prepareWorkspaceEditor("/cancel", async () => true)).rejects.toThrow("仍有文件未保存");
    } finally { remove(); }
  });
  it("does not use a replacement editor after an awaited confirmation", async () => {
    const first = {getDirtyCount: async () => 1, saveAll: vi.fn(async () => true)};
    const remove = registerWorkspaceEditor("/switch", first); let removeNext: (() => void) | undefined;
    try {
      await expect(prepareWorkspaceEditor("/switch", async () => {
        removeNext = registerWorkspaceEditor("/switch", {getDirtyCount: async () => 0, saveAll: vi.fn()}); return true;
      })).rejects.toThrow("编辑器已切换");
      expect(first.saveAll).not.toHaveBeenCalled();
    } finally { remove(); removeNext?.(); }
  });
  it("requires a fresh clean ACK after successful save", async () => {
    const getDirtyCount = vi.fn().mockResolvedValueOnce(1).mockResolvedValueOnce(1);
    const remove = registerWorkspaceEditor("/fresh", {getDirtyCount, saveAll: async () => true});
    try { await expect(prepareWorkspaceEditor("/fresh", async () => true)).rejects.toThrow("又有未保存"); }
    finally { remove(); }
  });
  it("locks input through rollback and model reload, always unlocking on failure", async () => {
    const order: string[] = [];
    const remove = registerWorkspaceEditor("/rollback", {getDirtyCount: async () => 0, saveAll: async () => true,
      lockEditing: locked => {order.push(locked ? "lock" : "unlock");},
      refreshFromDisk: async () => {order.push("reload"); throw new Error("reload timeout");}});
    try {
      await expect(withWorkspaceDiskMutation("/rollback", async () => {order.push("restore");})).rejects.toThrow("reload timeout");
      expect(order).toEqual(["lock", "restore", "reload", "unlock"]);
    } finally { remove(); }
  });
  it("does not restore disk when a new dirty buffer appears before the mutation", async () => {
    const action = vi.fn(); const unlock = vi.fn();
    const remove = registerWorkspaceEditor("/new-dirty", {getDirtyCount: async () => 1, saveAll: async () => true, lockEditing: unlock});
    try { await expect(withWorkspaceDiskMutation("/new-dirty", action)).rejects.toThrow("又有未保存"); expect(action).not.toHaveBeenCalled(); expect(unlock).toHaveBeenLastCalledWith(false); }
    finally { remove(); }
  });
  it("retains the live workspace owner when an older frame unregisters", () => {
    const previous = {getDirtyCount: vi.fn(async () => 0),saveAll:vi.fn(async () => true)};
    const current = {getDirtyCount: vi.fn(async () => 1),saveAll:vi.fn(async () => true)};
    const removePrevious = registerWorkspaceEditor("/same-project",previous);
    const removeCurrent = registerWorkspaceEditor("/same-project",current);
    try { removePrevious(); expect(workspaceEditor("/same-project")).toBe(current); }
    finally { removeCurrent(); }
    expect(workspaceEditor("/same-project")).toBeUndefined();
  });
  it("exposes context and cancellation only on the registered project without saving or locking for reads", async () => {
    const first = {getDirtyCount:vi.fn(async () => 1),saveAll:vi.fn(async () => true),lockEditing:vi.fn(),
      getContext:vi.fn(async () => ({workspace:"/context-a",document:null,openDocuments:[],diagnostics:[],diagnosticsTruncated:false,terminal:null,truncated:false})),
      cancelRun:vi.fn(async () => true)};
    const second = {getDirtyCount:vi.fn(async () => 0),saveAll:vi.fn(async () => true),cancelRun:vi.fn(async () => true)};
    const removeFirst = registerWorkspaceEditor("/context-a",first); const removeSecond = registerWorkspaceEditor("/context-b",second);
    try {
      await expect(workspaceEditor("/context-a")!.getContext!()).resolves.toEqual(expect.objectContaining({workspace:"/context-a"}));
      await expect(workspaceEditor("/context-a")!.cancelRun!("run-a")).resolves.toBe(true);
      expect(first.cancelRun).toHaveBeenCalledWith("run-a"); expect(second.cancelRun).not.toHaveBeenCalled();
      expect(first.saveAll).not.toHaveBeenCalled(); expect(first.lockEditing).not.toHaveBeenCalled(); expect(first.getDirtyCount).not.toHaveBeenCalled();
    } finally { removeFirst(); removeSecond(); }
  });
});
