import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ExpertCapabilitiesEditor } from "../experts-panel/experts/ExpertCapabilitiesEditor";
const native = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => native);
const settings = { model: "custom-model", skills: ["review"], tools: [], disallowedTools: [], discoverSkills: true, inheritSkills: true, mcpServers: ["repo"], mcpInheritance: "all" };
beforeEach(() => { vi.resetAllMocks(); native.invoke.mockResolvedValue(settings); });
describe("expert capability configuration", () => {
 it("reads the native definition and keeps multiline dependency inputs usable", async () => {
  const change = vi.fn(); render(<ExpertCapabilitiesEditor raw="original-definition" disabled={false} onChange={change} />);
  const field = await screen.findByRole("textbox", { name: "专家技能" });
  fireEvent.change(field, { target: { value: "review\n" } });
  expect(field).toHaveValue("review\n");
  fireEvent.change(field, { target: { value: "review\nrequirements" } });
  expect(change).toHaveBeenLastCalledWith(expect.objectContaining({ skills: ["review", "requirements"], model: "custom-model" }));
  expect(native.invoke).toHaveBeenCalledWith("agents_definition_settings", { raw: "original-definition" });
 });
 it("preserves inline connector configurations and custom inheritance rules", async () => {
  native.invoke.mockResolvedValue({ ...settings, mcpServers: null, mcpInheritance: { named: ["repo"] } });
  const change = vi.fn(); render(<ExpertCapabilitiesEditor raw="inline-definition" disabled={false} onChange={change} />);
  await waitFor(() => expect(screen.getByRole("textbox", { name: "专家连接器" })).toBeDisabled());
  fireEvent.change(screen.getByRole("textbox", { name: "专家默认模型" }), { target: { value: "" } });
  expect(change).toHaveBeenLastCalledWith(expect.objectContaining({ model: "inherit", mcpServers: null, mcpInheritance: { named: ["repo"] } }));
 });
 it("replaces visible capabilities when hiring a different template in the same editor", async () => {
  const change = vi.fn();
  const { rerender } = render(<ExpertCapabilitiesEditor raw="first-definition" disabled={false} onChange={change} />);
  expect(await screen.findByRole("textbox", { name: "专家技能" })).toHaveValue("review");
  native.invoke.mockResolvedValue({ ...settings, model: "second-model", skills: ["handoff"], mcpServers: ["docs"] });
  rerender(<ExpertCapabilitiesEditor raw="second-definition" disabled={false} onChange={change} />);
  await waitFor(() => expect(screen.getByRole("textbox", { name: "专家技能" })).toHaveValue("handoff"));
  expect(screen.getByRole("textbox", { name: "专家连接器" })).toHaveValue("docs");
  expect(screen.getByRole("textbox", { name: "专家默认模型" })).toHaveValue("second-model");
 });
});
