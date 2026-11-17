import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ExpertCapabilitiesEditor } from "../experts-panel/experts/ExpertCapabilitiesEditor";
import { ModelCatalogContext } from "@/lib/model-management";
import type { ExpertSettings } from "@/lib/expert-definition";
const native = vi.hoisted(() => ({ invoke: vi.fn() }));
const api = vi.hoisted(() => ({ skillsList: vi.fn(), mcpList: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => native);
vi.mock("@/lib/agent-client", () => api);
const settings: ExpertSettings = { model: "custom-model", skills: ["review"], tools: ["read_file"], disallowedTools: ["shell"], discoverSkills: true, inheritSkills: true, mcpServers: ["repo"], mcpInheritance: "all" };
const models = [{ id: "custom-model", label: "代码模型", source: "personal" as const }, { id: "org-model", label: "组织模型", source: "organization" as const }];
const editor = (raw: string | undefined, change = vi.fn(), cwd?: string) => <ModelCatalogContext.Provider value={models}><ExpertCapabilitiesEditor raw={raw} cwd={cwd} disabled={false} onChange={change} /></ModelCatalogContext.Provider>;
beforeEach(() => {
 vi.resetAllMocks(); native.invoke.mockResolvedValue(settings);
 api.skillsList.mockResolvedValue([{ name: "review", displayName: "代码审查", description: "检查代码与风险", enabled: true, scope: "user", path: "/skills/review" }, { name: "requirements", displayName: "需求分析", enabled: true, scope: "server" }, { name: "disabled", displayName: "停用技能", enabled: false }]);
 api.mcpList.mockResolvedValue([{ name: "repo", displayName: "代码仓库", enabled: true, live: true, status: "ready" }, { name: "docs", displayName: "组织文档", enabled: true, live: true }, { name: "mail", displayName: "企业邮箱", enabled: true, authRequired: true }]);
});
describe("expert capability selection", () => {
 it("selects catalog models and skills without typing routing IDs and keeps advanced rules unchanged", async () => {
  const change = vi.fn(); render(editor("definition", change, "/workspace"));
  const model = await screen.findByRole("combobox", { name: "专家默认模型" });
  expect(model).toHaveValue("custom-model");
  fireEvent.change(model, { target: { value: "org-model" } });
  fireEvent.click(screen.getByRole("button", { name: "选择技能" }));
  fireEvent.click(await screen.findByRole("checkbox", { name: /需求分析/ }));
  expect(change).toHaveBeenLastCalledWith({ ...settings, model: "org-model", skills: ["review", "requirements"] });
  expect(screen.getByRole("checkbox", { name: /停用技能/ })).toBeDisabled();
  expect(api.skillsList).toHaveBeenCalledWith("/workspace");
  expect(document.querySelector(".expert-capability-advanced")).not.toHaveAttribute("open");
 });
 it("selects authorized connectors, shows setup state and preserves inheritance until explicitly changed", async () => {
  const change = vi.fn(); render(editor("definition", change));
  fireEvent.click(await screen.findByRole("button", { name: "选择连接器" }));
  fireEvent.click(await screen.findByRole("checkbox", { name: /组织文档/ }));
  expect(screen.getByRole("checkbox", { name: /企业邮箱/ })).toBeDisabled();
  expect(change).toHaveBeenLastCalledWith(expect.objectContaining({ mcpServers: ["repo", "docs"], mcpInheritance: "all" }));
  fireEvent.change(screen.getByRole("combobox", { name: "连接器继承" }), { target: { value: "none" } });
  expect(change).toHaveBeenLastCalledWith(expect.objectContaining({ mcpServers: ["repo", "docs"], mcpInheritance: "none" }));
 });
 it("preserves inline connector configurations and custom inheritance rules", async () => {
  native.invoke.mockResolvedValue({ ...settings, mcpServers: null, mcpInheritance: { named: ["repo"] } });
  const change = vi.fn(); render(editor("inline-definition", change));
  await waitFor(() => expect(screen.getByRole("button", { name: "选择连接器" })).toBeDisabled());
  fireEvent.change(screen.getByRole("combobox", { name: "专家默认模型" }), { target: { value: "inherit" } });
  expect(change).toHaveBeenLastCalledWith(expect.objectContaining({ model: "inherit", mcpServers: null, mcpInheritance: { named: ["repo"] } }));
 });
 it("preserves missing models and path-based skill references when another setting changes", async () => {
  native.invoke.mockResolvedValue({ ...settings, model: "old-model", skills: ["/skills/review", "missing-skill"] });
  const change = vi.fn(); render(editor("legacy", change));
  expect(await screen.findByRole("combobox", { name: "专家默认模型" })).toHaveValue("old-model");
  fireEvent.click(screen.getByRole("button", { name: "选择技能" }));
  await screen.findByRole("checkbox", { name: /代码审查/ });
  fireEvent.click(screen.getByRole("checkbox", { name: /需求分析/ }));
  expect(change).toHaveBeenLastCalledWith(expect.objectContaining({ model: "old-model", skills: ["/skills/review", "missing-skill", "requirements"] }));
 });
 it("does not clear configured capabilities when a list request fails", async () => {
  api.skillsList.mockRejectedValue(new Error("offline"));
  const change = vi.fn(); render(editor("definition", change));
  fireEvent.click(await screen.findByRole("button", { name: "选择技能" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("原有选择不会被清空");
  expect(change).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "移除技能 review" })).toBeInTheDocument();
 });
 it("replaces the visible configuration when switching definitions and ignores late parsing results", async () => {
  let finish!: (value: ExpertSettings) => void;
  native.invoke.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const change = vi.fn(); const view = render(editor("first", change));
  native.invoke.mockResolvedValue({ ...settings, model: "org-model", skills: ["requirements"] });
  view.rerender(editor("second", change));
  await waitFor(() => expect(screen.getByRole("combobox", { name: "专家默认模型" })).toHaveValue("org-model"));
  await act(async () => finish(settings));
  expect(screen.getByRole("combobox", { name: "专家默认模型" })).toHaveValue("org-model");
  expect(screen.getByRole("button", { name: "移除技能 需求分析" })).toBeInTheDocument();
 });
});
