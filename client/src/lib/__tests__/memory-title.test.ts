import { describe, expect, it } from "vitest";
import { isContentDerivedMemoryTitle, memoryTitleFromContent } from "../memory-title";

describe("memory titles", () => {
  it.each(["用户口令为 memory-old", " 用户 口令为 MEMORY—OLD！ ", "用户口令为 MEMORY-OLD。"])(
    "recognizes the same fact despite punctuation, case and whitespace: %s", key => {
      expect(isContentDerivedMemoryTitle(key, "用户口令为 MEMORY-OLD。")).toBe(true);
    },
  );
  it("retains custom topics and does not treat punctuation-only titles as facts", () => {
    expect(isContentDerivedMemoryTitle("报告语言", "报告使用中文")).toBe(false);
    expect(isContentDerivedMemoryTitle("!!!", "你好")).toBe(false);
    expect(isContentDerivedMemoryTitle("", "你好")).toBe(true);
  });
  it.each(["🧠偏好".repeat(35), "🧠".repeat(70), "周报使用中文".repeat(30)])("keeps legacy 60/80-unit titles recognizable and new titles free of broken surrogate pairs: %s", content => {
    expect(isContentDerivedMemoryTitle(content.slice(0, 60), content)).toBe(true);
    expect(isContentDerivedMemoryTitle(content.slice(0, 80), content)).toBe(true);
    const title = memoryTitleFromContent(content);
    expect([...title]).toHaveLength(60);
    expect(isContentDerivedMemoryTitle(title, content)).toBe(true);
    expect(title).not.toMatch(/[\uD800-\uDBFF]$/u);
  });
  it("keeps exact symbol titles derived without treating unrelated symbols as the same fact", () => {
    expect(isContentDerivedMemoryTitle("🧠", "🧠")).toBe(true);
    expect(isContentDerivedMemoryTitle("!!!", "!!!")).toBe(true);
    expect(isContentDerivedMemoryTitle("!!!", "🧠")).toBe(false);
  });
});
