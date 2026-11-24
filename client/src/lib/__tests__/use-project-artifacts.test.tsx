import { act, renderHook } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import type { ProjectMeta } from "@/stores/projects-store";
import { ARTIFACT_CATALOG_EVENT, loadDocumentArtifactCatalog, type TaskArtifact } from "../artifact-catalog";
import { useProjectArtifacts } from "../use-project-artifacts";

vi.mock("../artifact-catalog", async (importOriginal) => ({
  ...await importOriginal<typeof import("../artifact-catalog")>(),
  loadDocumentArtifactCatalog: vi.fn(),
}));

const project: ProjectMeta = {
  id: "project-1", name: "项目", createdAt: "2026-11-24",
  connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [],
  conversations: [{ sessionId: "project-chat", title: "项目对话", createdAt: "2026-11-24" }],
};
const document: TaskArtifact = {
  id: "export-1", path: "/synthetic/report.docx", kind: "document_export", title: "报告",
  toolCallId: "document-export:receipt", status: "completed", verifiedOutput: true,
  sessionId: "project-chat", sessionTitle: "项目对话", cwd: "/synthetic", updatedAt: 1,
};

beforeEach(() => {
  localStorage.clear();
  vi.mocked(loadDocumentArtifactCatalog).mockReset();
});

it.each(["success", "failure"])("ignores a late %s after a newer project artifact read succeeds", async (outcome) => {
  let resolveOld!: (items: TaskArtifact[]) => void;
  let rejectOld!: (error: Error) => void;
  vi.mocked(loadDocumentArtifactCatalog)
    .mockImplementationOnce(() => new Promise((resolve, reject) => { resolveOld = resolve; rejectOld = reject; }))
    .mockResolvedValueOnce([document]);
  const { result } = renderHook(() => useProjectArtifacts(project));
  await act(async () => { window.dispatchEvent(new Event(ARTIFACT_CATALOG_EVENT)); });
  expect(result.current.artifacts).toEqual([document]);

  await act(async () => {
    if (outcome === "success") resolveOld([]);
    else rejectOld(new Error("旧读取失败"));
  });
  expect(result.current.artifacts).toEqual([document]);
  expect(result.current.error).toBe("");
});
