import { beforeEach, describe, expect, it, vi } from "vitest";
import { projectsLoad, projectsSave } from "@/lib/agent-client";
import { hydrateProjectsFromBackend, useProjectsStore, type ProjectMeta } from "../projects-store";

vi.mock("@/lib/agent-client", () => ({
  projectsLoad: vi.fn().mockResolvedValue([]),
  projectsSave: vi.fn().mockResolvedValue(undefined),
}));

function project(id: string, name = id): ProjectMeta {
  return {
    id, name, createdAt: "2026-10-12T00:00:00Z", connectors: [], experts: [], skills: [],
    plans: [], tasks: [], assets: [], members: [], conversations: [],
  };
}

describe("project hydration concurrency", () => {
  beforeEach(() => {
    vi.mocked(projectsLoad).mockReset();
    vi.mocked(projectsSave).mockReset().mockResolvedValue(undefined);
    localStorage.clear();
    useProjectsStore.setState({ projects: [project("p1")], persisting: false, persistError: null });
  });

  it("启动读取期间保留新增、重命名和后端独有项目并最终落盘", async () => {
    let resolve!: (projects: ProjectMeta[]) => void;
    vi.mocked(projectsLoad).mockReturnValue(new Promise((accept) => { resolve = accept; }));
    const hydration = hydrateProjectsFromBackend();
    useProjectsStore.getState().rename("p1", "用户刚修改的名称");
    const added = useProjectsStore.getState().add({ name: "启动时新建" });
    resolve([project("p1", "磁盘旧名称"), project("backend-only")]);
    await hydration;
    expect(useProjectsStore.getState().projects).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: "p1", name: "用户刚修改的名称" }),
      expect.objectContaining({ id: added.id }),
      expect.objectContaining({ id: "backend-only" }),
    ]));
    const calls = vi.mocked(projectsSave).mock.calls;
    expect(calls[calls.length - 1]?.[0]).toEqual(useProjectsStore.getState().projects);
  });

  it("启动读取期间删除的项目不会被后端旧快照复活", async () => {
    let resolve!: (projects: ProjectMeta[]) => void;
    vi.mocked(projectsLoad).mockReturnValue(new Promise((accept) => { resolve = accept; }));
    const hydration = hydrateProjectsFromBackend();
    useProjectsStore.getState().remove("p1");
    resolve([project("p1"), project("backend-only")]);
    await hydration;
    expect(useProjectsStore.getState().projects.map((item) => item.id)).toEqual(["backend-only"]);
  });

  it("启动读取期间解除共享会保留后端独有的组织关联与资料", async () => {
    const cached = { ...project("p1"), sharedProjects: { current: "deleted-project" } };
    useProjectsStore.setState({ projects: [cached] });
    let resolve!: (projects: ProjectMeta[]) => void;
    vi.mocked(projectsLoad).mockReturnValue(new Promise((accept) => { resolve = accept; }));
    const hydration = hydrateProjectsFromBackend();
    useProjectsStore.getState().detachDeletedSharedProject(new Set([cached.id]), "current", "deleted-project");
    const file = { id: "asset", name: "后端资料", kind: "file" as const };
    resolve([{ ...cached, sharedProjects: { ...cached.sharedProjects, other: "other-project" }, assets: [file] }]);
    await hydration;
    expect(useProjectsStore.getState().projects).toMatchObject([{
      sharedProjects: { other: "other-project" },
      deletedSharedProjects: { current: "deleted-project" },
      assets: [file],
    }]);
    expect(useProjectsStore.getState().projects[0].sharedProjects).not.toHaveProperty("current");
  });

  it("待同步缓存不会绕过正常写入队列导致旧快照覆盖新项目", async () => {
    localStorage.setItem("echoagent.projects.pending-backend-sync", "1");
    let finish!: () => void;
    vi.mocked(projectsSave).mockImplementationOnce(() => new Promise((accept) => { finish = accept; }));
    const hydration = hydrateProjectsFromBackend();
    await Promise.resolve();
    await Promise.resolve();
    useProjectsStore.getState().add({ name: "同步期间新建" });
    finish();
    await hydration;
    await Promise.resolve();
    await Promise.resolve();
    const calls = vi.mocked(projectsSave).mock.calls;
    expect(calls[calls.length - 1]?.[0]).toEqual(useProjectsStore.getState().projects);
  });
});
