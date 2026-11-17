import { beforeEach, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ProjectQuestionsDialog } from "../ProjectQuestionsDialog";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import type { ProjectWorkspace } from "@/lib/use-project-workspace";
import type { ProjectAnswer, TeamProjectSnapshot } from "@/lib/team-project-client";
const api = vi.hoisted(() => ({ request: vi.fn(), document: vi.fn() }));
vi.mock("@/lib/team-project-client", async original => ({ ...await original<object>(), teamProjectRequest: api.request }));
vi.mock("@/lib/org-client", () => ({ orgFetchDocument: api.document }));
const snapshot: TeamProjectSnapshot = { id: "shared", name: "项目", goal: "目标", scopeId: "scope", ownerId: "owner", role: "viewer", revision: 3, updatedAt: 100, tasks: [], assets: [], members: [], decisions: [], events: [] };
const answer: ProjectAnswer = { answer: "当前任务待验收 [来源1]", projectRevision: 3, updatedAt: 100, verification: "extractive_fallback", insufficient: true, gaps: ["部分资料仍在处理"], citations: [{ id: "来源1", title: "项目规范", resourceId: "doc", kind: "md", quote: "已保存摘录", openUrl: "/docs/doc" }] };
const workspace = (value: TeamProjectSnapshot | null = snapshot) => ({ snapshot: value, error: "", loading: false, refresh: vi.fn().mockResolvedValue(undefined), mutate: vi.fn() }) as unknown as ProjectWorkspace;
const view = (ws = workspace(), open = true) => <ProjectQuestionsDialog open={open} projectId="shared" draftKey="question:account:shared" workspace={ws} onClose={vi.fn()} />;
beforeEach(() => {
  vi.resetAllMocks(); localStorage.clear(); useSessionsStore.setState({ drafts: {} });
  useOrgSessionStore.setState({ boundaryRevision: 1, session: { loggedIn: true, accountId: "account", accountGeneration: 7, user: { id: "viewer", username: "viewer", displayName: "查看者", role: "member", clearance: 0 } } });
  api.request.mockImplementation(async action => action === "ask" ? structuredClone(answer) : structuredClone(snapshot));
  api.document.mockResolvedValue({ text: "重新授权读取的原文" });
});
const query = () => { fireEvent.change(screen.getByRole("textbox", { name: "项目问题" }), { target: { value: "项目当前进展？" } }); fireEvent.click(screen.getByRole("button", { name: "查询项目" })); };

it("lets a viewer query without a configured model and opens freshly authorized sources", async () => {
  const ws = workspace(); render(view(ws)); query();
  expect(await screen.findByLabelText("项目问答结果")).toHaveTextContent("当前任务待验收");
  expect(screen.getByText("当前依据仍有不足")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "来源1 · 项目规范" }));
  expect(await screen.findByLabelText("项目来源原文")).toHaveTextContent("重新授权读取的原文");
  expect(api.document).toHaveBeenCalledWith("doc", undefined, undefined, 7);
  expect(api.request.mock.calls.filter(c => c[0] === "snapshot")).toHaveLength(3);
  expect(ws.mutate).not.toHaveBeenCalled();
});
it("offers a newcomer briefing while retaining a separate typed question", async () => {
  render(view()); fireEvent.change(screen.getByRole("textbox", { name: "项目问题" }), { target: { value: "保留的问题" } });
  fireEvent.click(screen.getByRole("button", { name: "新成员快速了解项目" }));
  await screen.findByLabelText("项目问答结果");
  expect(api.request.mock.calls.find(c => c[0] === "ask")?.[2].question).toContain("我是新加入的项目成员");
  expect(screen.getByRole("textbox", { name: "项目问题" })).toHaveValue("保留的问题");
});
it("invalidates answers and opened evidence as soon as the shared revision changes", async () => {
  const rendered = render(view()); query(); await screen.findByLabelText("项目问答结果");
  fireEvent.click(screen.getByRole("button", { name: "来源1 · 项目规范" })); await screen.findByLabelText("项目来源原文");
  rendered.rerender(view(workspace({ ...snapshot, revision: 4 })));
  expect(screen.queryByLabelText("项目问答结果")).toBeNull(); expect(screen.queryByLabelText("项目来源原文")).toBeNull();
  expect(await screen.findByText(/项目内容已更新/)).toBeInTheDocument();
  expect(screen.getByRole("textbox", { name: "项目问题" })).toHaveValue("项目当前进展？");
});
it("rejects a source whose project changed before opening, including structured project citations", async () => {
  api.request.mockImplementation(async action => action === "ask" ? { ...answer, citations: [{ ...answer.citations[0], kind: "project_task" }] } : snapshot);
  const ws = workspace(); render(view(ws)); query(); await screen.findByLabelText("项目问答结果");
  api.request.mockResolvedValue({ ...snapshot, revision: 4 });
  fireEvent.click(screen.getByRole("button", { name: "来源1 · 项目规范" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("项目依据已更新");
  expect(screen.queryByText("已保存摘录")).toBeNull(); expect(api.document).not.toHaveBeenCalled(); expect(ws.refresh).toHaveBeenCalled();
});
it("discards an answer when access is revoked during the final authorization check", async () => {
  api.request.mockImplementation(async action => { if (action === "ask") return answer; throw new Error("已失去项目成员权限"); });
  render(view()); query();
  expect(await screen.findByRole("alert")).toHaveTextContent("失去项目成员权限");
  expect(screen.queryByLabelText("项目问答结果")).toBeNull(); expect(screen.getByRole("textbox", { name: "项目问题" })).toHaveValue("项目当前进展？");
});
it("never restores another account's delayed answer after logout", async () => {
  let finish!: (value: ProjectAnswer) => void; api.request.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  render(view()); query();
  act(() => useOrgSessionStore.setState({ boundaryRevision: 2, session: null }));
  await act(async () => finish(answer));
  expect(screen.queryByLabelText("项目问答结果")).toBeNull(); expect(api.request).toHaveBeenCalledTimes(1);
});
it("never displays a delayed source after the project revision changes", async () => {
  let finish!: (value: { text: string }) => void; api.document.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const rendered = render(view()); query(); await screen.findByLabelText("项目问答结果");
  fireEvent.click(screen.getByRole("button", { name: "来源1 · 项目规范" })); await waitFor(() => expect(api.document).toHaveBeenCalled());
  rendered.rerender(view(workspace({ ...snapshot, revision: 4 })));
  await act(async () => finish({ text: "不应显示的旧原文" }));
  expect(screen.queryByText("不应显示的旧原文")).toBeNull(); expect(screen.queryByLabelText("项目问答结果")).toBeNull();
});
it("retains the question and answer when closing and reopening, without another request", async () => {
  const ws = workspace(), rendered = render(view(ws)); query(); await screen.findByLabelText("项目问答结果");
  rendered.rerender(view(ws, false)); expect(screen.queryByRole("dialog")).toBeNull();
  rendered.rerender(view(ws)); expect(screen.getByLabelText("项目问答结果")).toBeInTheDocument();
  expect(screen.getByRole("textbox", { name: "项目问题" })).toHaveValue("项目当前进展？");
  expect(api.request.mock.calls.filter(c => c[0] === "ask")).toHaveLength(1);
});
