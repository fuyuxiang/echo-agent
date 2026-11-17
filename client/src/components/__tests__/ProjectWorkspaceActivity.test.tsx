import { beforeEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ProjectActivity } from "../ProjectWorkspaceSections";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { useSessionsStore } from "@/stores/sessions-store";
import type { ProjectWorkspace } from "@/lib/use-project-workspace";
vi.mock("@/lib/agent-client", () => ({ projectsSave: vi.fn().mockResolvedValue(undefined) }));
const project: ProjectMeta = { id: "p", name: "项目", createdAt: "2026-11-17", tasks: [], conversations: [], assets: [], plans: [], members: [], experts: [], skills: [], connectors: [] };
beforeEach(() => {
  localStorage.clear();
  useProjectsStore.setState({ projects: [structuredClone(project)] });
  useSessionsStore.setState({ independent: [], drafts: {} });
});

it("preserves a shared note and idempotency key across a failed response and tab remount", async () => {
  const mutate = vi.fn().mockRejectedValueOnce(new Error("响应丢失")).mockResolvedValue({ id: "comment" });
  const workspace = { snapshot: { role: "collaborator", tasks: [], runs: [], events: [], comments: [] }, mutate } as unknown as ProjectWorkspace;
  const view = () => <ProjectActivity project={project} sharedId="shared" draftScope="account:p" workspace={workspace} onOpenTask={vi.fn()} />;
  const first = render(view());
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "请核对来源" } });
  fireEvent.click(screen.getByRole("button", { name: "发布留言" }));
  await screen.findByRole("alert"); first.unmount();
  render(view()); expect(screen.getByRole("textbox")).toHaveValue("请核对来源");
  fireEvent.click(screen.getByRole("button", { name: "发布留言" }));
  await waitFor(() => expect(screen.getByRole("textbox")).toHaveValue(""));
  expect(mutate.mock.calls[0]).toEqual(mutate.mock.calls[1]);
  expect(mutate.mock.calls[0][1]).toMatchObject({ content: "请核对来源", clientRequestId: expect.any(String) });
  expect(useProjectsStore.getState().projects[0].activity).toBeUndefined();
});

it("locks shared comments when access or server capability is missing", () => {
  const mutate = vi.fn();
  const { rerender } = render(<ProjectActivity project={project} sharedId="s" workspace={{ snapshot: null, mutate } as unknown as ProjectWorkspace} onOpenTask={vi.fn()} />);
  expect(screen.queryByRole("textbox")).toBeNull();
  rerender(<ProjectActivity project={project} sharedId="s" workspace={{ snapshot: { role: "viewer", tasks: [], runs: [], events: [], comments: [] }, mutate } as unknown as ProjectWorkspace} onOpenTask={vi.fn()} />);
  expect(screen.queryByRole("textbox")).toBeNull(); expect(mutate).not.toHaveBeenCalled();
});

it("keeps local decision revisions and retires them without deleting the record", () => {
  const store = useProjectsStore.getState();
  store.saveDecision("p", { content: "原规范", rationale: "原依据" });
  const id = useProjectsStore.getState().projects[0].decisions![0].id;
  store.saveDecision("p", { content: "新规范", rationale: "新依据" }, id);
  store.setDecisionRetired("p", id, true);
  const decision = useProjectsStore.getState().projects[0].decisions![0];
  expect(decision).toMatchObject({ id, content: "新规范", status: "retired", history: [{ content: "原规范", rationale: "原依据" }] });
  store.setDecisionRetired("p", id, false);
  expect(useProjectsStore.getState().projects[0].decisions![0].status).toBe("active");
});
