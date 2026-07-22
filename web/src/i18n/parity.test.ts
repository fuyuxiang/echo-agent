import { describe, it, expect } from "vitest";
import { resources } from "./index";

// 递归收集一个 namespace 对象的所有叶子 key(点分路径)。
function leafKeys(obj: unknown, prefix = ""): string[] {
  if (obj === null || typeof obj !== "object") return [prefix];
  return Object.entries(obj as Record<string, unknown>).flatMap(([k, v]) =>
    leafKeys(v, prefix ? `${prefix}.${k}` : k),
  );
}

describe("i18n 词条 zh/en key 一致性", () => {
  const langs = Object.keys(resources);
  it("恰好含 zh 与 en 两种语言", () => {
    expect(langs.sort()).toEqual(["en", "zh"]);
  });

  const namespaces = new Set(
    langs.flatMap((l) => Object.keys(resources[l] as Record<string, unknown>)),
  );
  for (const ns of namespaces) {
    it(`namespace "${ns}" 的 zh/en key 集合一致`, () => {
      const zh = leafKeys((resources.zh as Record<string, unknown>)[ns]).sort();
      const en = leafKeys((resources.en as Record<string, unknown>)[ns]).sort();
      expect(en).toEqual(zh);
    });
  }
});
