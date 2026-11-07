import { describe, it, expect } from "vitest";
import { parseMemoryImport } from "../memory-import";

describe("记忆导入预览", () => {
  it("支持其他 AI 导出的逐行文本，去重并只形成待审核草稿", () => {
    const items = parseMemoryImport("## 我的偏好\n- 回答先给结论\n- 回答先给结论\n2. 周报使用中文");
    expect(items.map((v) => v.content)).toEqual(["回答先给结论", "周报使用中文"]);
    expect(items.every((v) => v.scope === "global" && !v.id && !v.expectedRevision)).toBe(true);
  });
  it("导入结构化备份时不恢复已忘记的内容、不信任旧 ID", () => {
    const items = parseMemoryImport(JSON.stringify([{ id: "old", status: "forgotten", content: "不要重新记住" }, { id: "new", status: "active", scope: "workspace", kind: "task", key: "初稿", content: "完成初稿" }]));
    expect(items).toHaveLength(1); expect(items[0]).toMatchObject({ scope: "workspace", kind: "task", key: "初稿" }); expect(items[0].id).toBeUndefined();
  });
  it("错误和超长导入不会静默部分保存", () => {
    expect(() => parseMemoryImport('[{"content":')).toThrow(/JSON/);
    expect(() => parseMemoryImport("a".repeat(4001))).toThrow(/4000/);
    expect(() => parseMemoryImport(Array.from({ length: 101 }, (_, i) => `记忆 ${i}`).join("\n"))).toThrow(/100/);
  });
  it("派生标题和自定义主题不会截断 Unicode 字符", () => {
    const content = `${"文".repeat(59)}🧠记忆`;
    const [derived] = parseMemoryImport(content);
    expect(derived.key).toBe(`${"文".repeat(59)}🧠`);
    const [custom] = parseMemoryImport(JSON.stringify([{ content, key: `${"文".repeat(159)}🧠继续` }]));
    expect([...custom.key]).toHaveLength(160);
    expect(custom.key.endsWith("🧠")).toBe(true);
  });
});
