import { beforeEach, describe, expect, it, vi } from "vitest";
import { projectsSave } from "@/lib/agent-client";
import { useProjectsStore, type ProjectMeta } from "../projects-store";
import type { SessionSummary } from "@/lib/types";

vi.mock("@/lib/agent-client", () => ({
  projectsLoad: vi.fn().mockResolvedValue([]),
  projectsSave: vi.fn().mockResolvedValue(undefined),
}));

const project: ProjectMeta = {
  id: "p1", name: "交付", cwd: "/project", createdAt: "2026-09-01T00:00:00Z",
  connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [], conversations: [],
};
const session: SessionSummary = {
  sessionId: "s1", title: "探索方案", cwd: "/original", currentModelId: "m1", archived: false,
};

describe("project session movement", () => {
  beforeEach(() => {
    vi.mocked(projectsSave).mockReset().mockResolvedValue(undefined);
    useProjectsStore.setState({ projects: [structuredClone(project)], persisting: false, persistError: null });
  });

  it("移动只改变项目引用，重复移动受保护，移出不删除主会话", async () => {
    await useProjectsStore.getState().moveSessionToProject("p1", session);
    expect(useProjectsStore.getState().projects[0].conversations).toMatchObject([
      { sessionId: "s1", title: "探索方案", modelId: "m1", pendingProjectContext: true },
    ]);
    await expect(useProjectsStore.getState().moveSessionToProject("p1", session)).rejects.toThrow("已归属项目");
    useProjectsStore.getState().setProjectContextPending("p1", "s1", false);
    expect(useProjectsStore.getState().projects[0].conversations[0].pendingProjectContext).toBe(false);
    await useProjectsStore.getState().detachSessionFromProject("p1", "s1");
    expect(useProjectsStore.getState().projects[0].conversations).toEqual([]);
    expect(projectsSave).toHaveBeenLastCalledWith(useProjectsStore.getState().projects, "s1");
    expect(session).toMatchObject({ sessionId: "s1", cwd: "/original" });
  });

  it("落盘失败时回滚移入，保留原归属", async () => {
    vi.mocked(projectsSave).mockRejectedValueOnce(new Error("disk full"));
    await expect(useProjectsStore.getState().moveSessionToProject("p1", session)).rejects.toThrow("disk full");
    expect(useProjectsStore.getState().projects[0].conversations).toEqual([]);
  });

  it("共享关联保存失败后重新落盘同一个关联，才向发布流程确认成功", async () => {
    vi.mocked(projectsSave).mockRejectedValueOnce(new Error("disk full"));
    const { linkSharedProject } = useProjectsStore.getState();
    await expect(linkSharedProject("p1", "org-account", "shared-1")).rejects.toThrow("disk full");
    await expect(linkSharedProject("p1", "org-account", "shared-1")).resolves.toBeUndefined();
    expect(projectsSave).toHaveBeenCalledTimes(2);
    expect(projectsSave).toHaveBeenLastCalledWith([
      expect.objectContaining({ sharedProjects: { "org-account": "shared-1" } }),
    ]);
    expect(useProjectsStore.getState().persistError).toBeNull();
  });

  it("普通独立会话分叉不创建项目或发起无关保存", async () => {
    await expect(useProjectsStore.getState().associateForkWithSourceProject("s1", { ...session, sessionId: "fork" })).resolves.toBeNull();
    expect(projectsSave).not.toHaveBeenCalled();
    expect(useProjectsStore.getState().projects).toEqual([project]);
  });

  it("仅工作项引用的旧会话分叉建立独立对话和一次项目契约，不复制旧任务", async () => {
    const owned = { ...project, tasks: [{ id: "t1", title: "原任务", scope: "personal" as const, source: "user", status: "completed" as const, sessionId: "s1" }] };
    useProjectsStore.setState({ projects: [owned] });
    await expect(useProjectsStore.getState().associateForkWithSourceProject("s1", { ...session, sessionId: "fork" })).resolves.toBe("p1");
    expect(useProjectsStore.getState().projects[0].tasks).toMatchObject([...owned.tasks, { sessionId: "fork", source: "conversation" }]);
    expect(useProjectsStore.getState().projects[0].conversations).toMatchObject([{ sessionId: "fork", pendingProjectContext: true }]);
    expect(projectsSave).toHaveBeenCalledWith(useProjectsStore.getState().projects);
  });

  it("冲突或已有归属不能把分叉关联到错误项目", async () => {
    const conversation = { sessionId: "s1", title: "原会话", createdAt: project.createdAt };
    useProjectsStore.setState({ projects: [{ ...project, conversations: [conversation] }, { ...project, id: "p2", conversations: [conversation] }] });
    await expect(useProjectsStore.getState().associateForkWithSourceProject("s1", { ...session, sessionId: "fork" })).rejects.toThrow("归属冲突");
    useProjectsStore.setState({ projects: [{ ...project, conversations: [conversation, { ...conversation, sessionId: "fork" }] }] });
    await expect(useProjectsStore.getState().associateForkWithSourceProject("s1", { ...session, sessionId: "fork" })).rejects.toThrow("已归属项目");
    expect(projectsSave).not.toHaveBeenCalled();
  });
});
