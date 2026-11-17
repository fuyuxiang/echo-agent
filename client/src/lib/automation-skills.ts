import type { SkillInfo } from "./types";

/** Resolve against the selected workspace, retaining a path when the catalog owns one. */
export function resolveAutomationSkills(selected: string[], catalog: SkillInfo[]): string[] {
  return [...new Set(selected.map(reference => {
    const skill = catalog.find(item => item.path === reference || item.name === reference);
    if (!skill) throw new Error(`所选技能在此工作目录不可用：${reference}。请调整项目技能或工作目录后重试。`);
    const label = skill.displayName ?? skill.name;
    if (!skill.enabled) throw new Error(`所选技能已停用：${label}。请启用后重试。`);
    if (!skill.capability?.ready) throw new Error(`所选技能尚未通过运行预检：${label}。请检查技能依赖与配置后重试。`);
    return skill.path ?? skill.name;
  }))];
}
