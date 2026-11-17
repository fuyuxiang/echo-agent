import { beforeEach, expect, it, vi } from "vitest";
const api = vi.hoisted(() => ({ invoke: vi.fn(), importFiles: vi.fn(), authorize: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: api.invoke }));
vi.mock("../agent-client", () => ({ projectsSave: vi.fn().mockResolvedValue(undefined), projectAssetsImport: api.importFiles }));
vi.mock("../artifact-access", async original => ({ ...await original<object>(), authorizeArtifactFile: api.authorize }));
import { importProjectOutput } from "../project-files";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import type { TaskArtifact } from "../artifact-catalog";
const project: ProjectMeta = { id: "p", name: "项目", createdAt: "2026-01-01", assets: [], tasks: [], conversations: [], plans: [], members: [], experts: [], skills: [], connectors: [] };
const output: TaskArtifact = { id: "out", sessionId: "s", sessionTitle: "报告", cwd: "/work", path: "report.md", kind: "edit", title: "Write report", toolCallId: "write", status: "completed", verifiedOutput: true, updatedAt: 1 };
beforeEach(() => { vi.clearAllMocks(); useProjectsStore.setState({ projects: [structuredClone(project)] }); api.invoke.mockResolvedValue({ absolute: "/work/report.md" }); api.importFiles.mockResolvedValue([{ name: "report.md", path: "/private/projects/p/report.md", kind: "file", sizeBytes: 12 }]); });

it("copies an actual output into the project, keeps provenance and reuses the same copy", async () => {
  const asset = await importProjectOutput("p", output);
  expect(api.invoke).toHaveBeenCalledWith("path_stat", { path: "report.md", cwd: "/work" });
  expect(api.importFiles).toHaveBeenCalledWith("p", ["/work/report.md"]);
  expect(asset).toMatchObject({ sourceSessionId: "s", sourcePath: "report.md", purpose: "output", path: "/private/projects/p/report.md" });
  await expect(importProjectOutput("p", output)).resolves.toEqual(asset);
  expect(api.importFiles).toHaveBeenCalledTimes(1);
  api.importFiles.mockResolvedValueOnce([{ name: "report.md", path: "/private/projects/p/report-2.md", kind: "file", sizeBytes: 16 }]);
  const newer = await importProjectOutput("p", { ...output, updatedAt: 2, toolCallId: "write-new" });
  expect(newer?.path).toBe("/private/projects/p/report-2.md");
  expect(useProjectsStore.getState().projects[0].assets).toHaveLength(2);
});

it("does not add a phantom asset after a cancelled file grant or a missing output", async () => {
  api.invoke.mockRejectedValueOnce(new Error("unauthorized path")); api.authorize.mockResolvedValueOnce("cancelled");
  await expect(importProjectOutput("p", output)).resolves.toBeUndefined();
  expect(useProjectsStore.getState().projects[0].assets).toHaveLength(0); expect(api.importFiles).not.toHaveBeenCalled();
  api.invoke.mockRejectedValueOnce(new Error("文件不存在"));
  await expect(importProjectOutput("p", output)).rejects.toThrow("文件不存在");
  expect(useProjectsStore.getState().projects[0].assets).toHaveLength(0);
});

it("restores native export authorization before accessing its file", async () => {
  await importProjectOutput("p", { ...output, kind: "document_export", toolCallId: `document-export:${"a".repeat(64)}` });
  expect(api.invoke.mock.calls[0]).toEqual(["session_export_authorize", { sessionId: "s", path: "report.md", sha256: "a".repeat(64) }]);
});
