import { expect, it, vi } from "vitest";
vi.mock("@/lib/agent-client", () => ({ projectsLoad: vi.fn(), projectsSave: vi.fn() }));
import { migrateProjectWorkItems } from "../projects-store";
it("migrates both legacy lists without merging colliding ids or losing session/model state", () => {
  const tasks = migrateProjectWorkItems({
    tasks: [{ id: "same", title: "task", scope: "personal", source: "manual", status: "paused" }],
    plans: [{ id: "same", title: "plan", status: "in_progress", modelId: "m", sessionId: "s" }],
  });
  expect(tasks).toMatchObject([{ id: "same", title: "task" }, { id: "legacy-plan:same", title: "plan", sessionId: "s", modelId: "m", status: "in_progress" }]);
  expect(migrateProjectWorkItems({ tasks, plans: [] })).toEqual(tasks);
});

it("clears removed model bindings when restoring projects while keeping work and session references", async () => {
  const { projectsLoad } = await import("@/lib/agent-client");
  const { hydrateProjectsFromBackend, useProjectsStore } = await import("../projects-store");
  localStorage.clear();
  useProjectsStore.setState({ projects: [] });
  vi.mocked(projectsLoad).mockResolvedValue([{
    id: "project", name: "旧项目", defaultModelId: "echoagent-ojlab/Echo Reed",
    tasks: [
      { id: "old", title: "保留任务", modelId: "echoagent-ojlab/Echo Willow", sessionId: "saved-session", status: "paused" },
      { id: "mine", title: "个人任务", modelId: "mine/Echo Reed", status: "pending" },
    ],
    conversations: [{ sessionId: "saved-session", title: "保留对话", modelId: "echoagent-ojlab/Echo Reed" }],
  }]);
  await hydrateProjectsFromBackend();
  const project = useProjectsStore.getState().projects[0];
  expect(project.defaultModelId).toBeUndefined();
  expect(project.tasks[0]).toMatchObject({ id: "old", title: "保留任务", sessionId: "saved-session", status: "paused" });
  expect(project.tasks[0].modelId).toBeUndefined();
  expect(project.tasks[1].modelId).toBe("mine/Echo Reed");
  expect(project.conversations[0]).toMatchObject({ sessionId: "saved-session", title: "保留对话" });
  expect(project.conversations[0].modelId).toBeUndefined();
});
