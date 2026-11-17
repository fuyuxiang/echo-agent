import { expect, it } from "vitest";
import { resolveAutomationSkills } from "../automation-skills";
import type { SkillInfo } from "../types";

const skill: SkillInfo = { name: "analysis", displayName: "数据分析", path: "/work/skills/analysis", enabled: true, capability: { declared: false, state: "instruction_only", ready: true, capabilities: [], checks: [] } };

it("resolves names and project paths to the same catalog-owned reference", () => {
  expect(resolveAutomationSkills([skill.name, skill.path!, skill.name], [skill])).toEqual([skill.path]);
});
it("does not substitute a same-name skill from another workspace for a missing path", () => {
  expect(() => resolveAutomationSkills(["/other/analysis"], [skill])).toThrow("此工作目录不可用");
});
it("blocks disabled, unchecked and unready skills before saving active work", () => {
  expect(() => resolveAutomationSkills([skill.name], [{ ...skill, enabled: false }])).toThrow("已停用");
  expect(() => resolveAutomationSkills([skill.name], [{ ...skill, capability: undefined }])).toThrow("运行预检");
  expect(() => resolveAutomationSkills([skill.name], [{ ...skill, capability: { ...skill.capability!, ready: false } }])).toThrow("运行预检");
});
