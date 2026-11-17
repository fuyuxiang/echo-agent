import { invoke } from "@tauri-apps/api/core";
import { flushProjectPersistence, useProjectsStore, type AssetItem } from "@/stores/projects-store";
import { authorizeDocumentArtifact, type TaskArtifact } from "./artifact-catalog";
import { authorizeArtifactFile, isUnauthorizedPathError } from "./artifact-access";
import { projectAssetsImport } from "./agent-client";

export async function importProjectOutput(projectId: string, output: TaskArtifact): Promise<AssetItem | undefined> {
  const project = useProjectsStore.getState().projects.find(p => p.id === projectId);
  if (!project) throw new Error("项目已删除，成果尚未加入项目");
  const version = output.producedAt ?? output.updatedAt;
  const existing = project.assets.find(a => a.sourceSessionId === output.sessionId && a.sourcePath === output.path && a.sourceUpdatedAt === version && a.sourceToolCallId === output.toolCallId);
  if (existing) return existing;
  await authorizeDocumentArtifact(output.sessionId, output);
  const copy = async () => {
    const stat = await invoke<{ absolute: string }>("path_stat", { path: output.canonicalPath ?? output.path, cwd: output.cwd || null });
    return projectAssetsImport(projectId, [stat.absolute]);
  };
  let imported;
  try { imported = await copy(); }
  catch (e) {
    if (!isUnauthorizedPathError(e)) throw e;
    const granted = await authorizeArtifactFile(output.canonicalPath ?? output.path, output.cwd);
    if (granted === "cancelled") return;
    if (granted !== "authorized") throw new Error("所选文件与任务成果不一致，请重新选择");
    imported = await copy();
  }
  if (!useProjectsStore.getState().projects.some(p => p.id === projectId)) throw new Error("项目已删除，成果尚未加入项目");
  useProjectsStore.getState().addAssets(projectId, imported.map(a => ({ ...a, purpose: "output", sourceSessionId: output.sessionId, sourcePath: output.path, sourceUpdatedAt: version, sourceToolCallId: output.toolCallId })));
  await flushProjectPersistence();
  return useProjectsStore.getState().projects.find(p => p.id === projectId)?.assets.find(a => a.path === imported[0]?.path);
}
