import { beforeEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ProjectAutomationsDialog } from "../ProjectAutomationsDialog";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
const api = vi.hoisted(() => ({ save: vi.fn(), skills: vi.fn() }));
vi.mock("@/lib/agent-client", () => ({ projectsSave: vi.fn().mockResolvedValue(undefined), automationsSave: api.save, skillsList: api.skills }));
const project: ProjectMeta = { id: "p", name: "项目", cwd: "/work", createdAt: "2026-11-17", tasks: [], conversations: [], assets: [], plans: [], members: [], experts: [], skills: [{ id: "/work/skills/analysis", name: "数据分析" }], connectors: [] };
beforeEach(() => {
  vi.clearAllMocks(); localStorage.clear(); useProjectsStore.setState({ projects: [structuredClone(project)] });
  api.skills.mockResolvedValue([{ name: "analysis", path: "/work/skills/analysis", enabled: true, capability: { ready: true } }]);
  api.save.mockImplementation(async automation => ({ ...automation, id: "auto" }));
});
const view = () => render(<ProjectAutomationsDialog project={project} data={{ automations: [], records: [], loading: false, error: "", refresh: vi.fn().mockResolvedValue(undefined) }} models={[{ id: "m", label: "模型" }]} defaultModelId="m" onClose={vi.fn()} />);
it("preflights inherited project skills in the actual execution workspace before saving", async () => {
  view(); fireEvent.click(screen.getByRole("button", { name: "新建定时工作" })); fireEvent.click(screen.getByRole("button", { name: "保存定时工作" }));
  await waitFor(() => expect(api.save).toHaveBeenCalledOnce());
  expect(api.skills).toHaveBeenCalledWith("/work");
  expect(api.save.mock.calls[0][0]).toMatchObject({ projectId: "p", skills: ["/work/skills/analysis"] });
});
it("keeps the draft and explains an unavailable inherited skill without creating an active schedule", async () => {
  api.skills.mockResolvedValue([]); view();
  fireEvent.click(screen.getByRole("button", { name: "新建定时工作" })); fireEvent.click(screen.getByRole("button", { name: "保存定时工作" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("此工作目录不可用");
  expect(screen.getByRole("textbox", { name: "名称" })).toHaveValue("项目周报"); expect(api.save).not.toHaveBeenCalled();
});
