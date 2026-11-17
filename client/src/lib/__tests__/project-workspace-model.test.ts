import { expect, it } from "vitest";
import { projectTaskState, projectFeed } from "../project-workspace-model";
import { buildProjectPrompt } from "../project-context";
import type { ProjectMeta } from "@/stores/projects-store";
import type { UnifiedTask } from "../project-tasks";
import type { SessionSummary } from "../types";
import type { TeamProjectSnapshot } from "../team-project-client";

const project: ProjectMeta = { id: "p", name: "交付", createdAt: "2026-11-01", instructions: "标注资料来源", description: "形成交付报告", tasks: [], assets: [], conversations: [], plans: [], members: [], experts: [], skills: [], connectors: [] };
const row: UnifiedTask = { id: "t", title: "报告", private: true, sessionId: "s", local: { id: "t", title: "报告", status: "in_progress", source: "manual", scope: "personal", sessionId: "s" } };
const summary: SessionSummary = { sessionId: "s", title: "报告", cwd: "/work", status: "completed", updatedAt: "2026-11-17T01:00:00Z" };

it("requires explicit acceptance after a completed run and does not claim legacy histories completed", () => {
  expect(projectTaskState(row, summary)).toBe("review");
  expect(projectTaskState(row)).toBe("unknown");
  const accepted = { ...row, local: { ...row.local!, status: "completed" as const, acceptedAt: "2026-11-17T02:00:00Z" } };
  expect(projectTaskState(accepted, summary)).toBe("completed");
  expect(projectTaskState(accepted, { ...summary, status: "working" })).toBe("running");
  expect(projectTaskState(accepted, { ...summary, updatedAt: "2026-11-17T03:00:00Z" })).toBe("review");
});

it("uses the shared task as authority after handoff and locks unavailable shared rows", () => {
  const shared: UnifiedTask = { ...row, private: false, remoteId: "remote", remote: { id: "remote", title: "报告", description: "", acceptance: "", assigneeId: "other", assigneeName: "其他成员", status: "accepted", revision: 5, updatedAt: 100 } };
  expect(projectTaskState(shared, { ...summary, status: "working" })).toBe("completed");
  expect(projectTaskState({ ...shared, remote: undefined }, summary)).toBe("syncing");
  expect(projectTaskState({ ...shared, remote: { ...shared.remote!, status: "in_progress" } }, { ...summary, status: "awaiting_permission" })).toBe("attention");
});

it("injects the project goal and only active decisions into new task context", () => {
  const prompt = buildProjectPrompt({ ...project, decisions: [{ id: "d", content: "保留原始数据", rationale: "可追溯", status: "active", createdAt: "now", updatedAt: "now" }, { id: "old", content: "废弃的决定", rationale: "", status: "retired", createdAt: "now", updatedAt: "now" }] }, "生成报告");
  expect(prompt).toContain("形成交付报告"); expect(prompt).toContain("保留原始数据"); expect(prompt).not.toContain("废弃的决定"); expect(prompt.endsWith("生成报告")).toBe(true);
});

it("renders shared comments once and resolves run events to the source task", () => {
  const snapshot = { tasks: [{ id: "r", title: "原任务" }], runs: [{ id: "run", taskId: "r" }], comments: [{ id: "c", content: "请补充来源", actorName: "林晓", createdAt: 200 }], events: [{ id: "ce", kind: "comment_created", resourceId: "c", actorName: "林晓", createdAt: 200 }, { id: "re", kind: "run_updated", resourceId: "run", actorName: "林晓", createdAt: 100 }] } as unknown as TeamProjectSnapshot;
  const feed = projectFeed(project, snapshot, []);
  expect(feed).toHaveLength(2); expect(feed[0].content).toBe("请补充来源"); expect(feed[1]).toMatchObject({ taskId: "shared:r", content: "更新了执行记录「原任务」" });
});
