import { relativeToWorkspace } from "./windows-path";
import type {
  ApplyResult,
  DocumentSnapshot,
  EditOperation,
  EditorContextSnapshot,
} from "../../../../vendor/theia-platform/examples/echo-coding-bridge/src/browser/echo-editor-protocol";
export type {
  ApplyResult,
  DocumentSnapshot,
  EditOperation,
  EditorContextSnapshot,
};
/** Workspace-keyed editor boundary. Background tasks never query the visible other project. */
export interface WorkspaceEditor {
  workspace?: string;
  getDirtyCount: () => Promise<number>;
  getDocument?: (path?: string, full?: boolean, range?: {offset:number;limit:number}) => Promise<DocumentSnapshot>;
  getContext?: () => Promise<EditorContextSnapshot>;
  cancelRun?: (runId: string) => Promise<boolean>;
  applyEdits?: (operation: EditOperation) => Promise<ApplyResult>;
  saveAll: () => Promise<boolean>;
  refreshFromDisk?: () => Promise<void>;
  lockEditing?: (locked: boolean) => void;
}
const editors = new Map<string, WorkspaceEditor>();
export const workspaceEditor = (root: string) =>
  editors.get(root) ??
  [...editors.values()].find(
    (editor) =>
      editor.workspace && relativeToWorkspace(editor.workspace, root) === "",
  );
export function registerWorkspaceEditor(
  root: string,
  editor: WorkspaceEditor,
): () => void {
  editors.set(root, editor);
  return () => {
    if (editors.get(root) === editor) editors.delete(root);
  };
}
export class EditorPreparationCancelled extends Error {}
export async function prepareWorkspaceEditor(
  root: string,
  confirmSave?: (count: number) => Promise<boolean>,
): Promise<void> {
  const editor = editors.get(root);
  // No mounted editor means there is no in-memory buffer owned by this renderer.
  if (!editor) return;
  const assertOwner = () => {
    if (editors.get(root) !== editor)
      throw new Error("项目编辑器已切换，请重试");
  };
  const count = await editor.getDirtyCount();
  assertOwner();
  if (count === 0) return;
  if (!confirmSave)
    throw new Error("IDE 有未保存文件，请保存后运行验证或继续任务");
  if (!(await confirmSave(count)))
    throw new EditorPreparationCancelled("已取消，编辑内容已保留");
  assertOwner();
  if (!(await editor.saveAll())) throw new Error("仍有文件未保存，操作未执行");
  assertOwner();
  if ((await editor.getDirtyCount()) !== 0)
    throw new Error("IDE 又有未保存内容，请保存后重试");
  assertOwner();
}

/** Lock the iframe during destructive disk changes, then ACK reloaded clean models. */
export async function withWorkspaceDiskMutation<T>(
  root: string,
  action: () => Promise<T>,
): Promise<T> {
  const editor = editors.get(root);
  if (!editor) return action();
  editor.lockEditing?.(true);
  try {
    if ((await editor.getDirtyCount()) !== 0)
      throw new Error("回滚前又有未保存编辑，请保存后重试");
    if (editors.get(root) !== editor) throw new Error("项目已切换，回滚未执行");
    const result = await action();
    await editor.refreshFromDisk?.();
    return result;
  } finally {
    editor.lockEditing?.(false);
  }
}
