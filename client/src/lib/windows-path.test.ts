import { describe, expect, it } from "vitest";
import { normalizeWorkspacePath, relativeToWorkspace } from "./windows-path";

describe("Windows paths at the IDE boundary", () => {
  it("preserves Chinese components while removing supported verbatim prefixes", () => {
    expect(normalizeWorkspacePath(String.raw`\\?\C:\应用\中文项目`)).toBe("C:/应用/中文项目");
    expect(normalizeWorkspacePath(String.raw`\\?\UNC\server\share\中文项目`)).toBe("//server/share/中文项目");
  });

  it("matches drive and UNC paths across spelling and case differences", () => {
    expect(relativeToWorkspace(String.raw`\\?\C:\项目\代码`, "c:/项目/代码/src/main.ts")).toBe("src/main.ts");
    expect(relativeToWorkspace(String.raw`\\?\UNC\SERVER\Share\项目`, "//server/share/项目/src/main.ts")).toBe("src/main.ts");
    expect(relativeToWorkspace("C:/", "c:/中文.ts")).toBe("中文.ts");
    expect(relativeToWorkspace("C:/", "c:/")).toBe("");
    expect(relativeToWorkspace("/", "/项目/main.ts")).toBe("项目/main.ts");
  });

  it("rejects sibling paths and traversal", () => {
    expect(relativeToWorkspace("C:/项目", "C:/项目二/a.ts")).toBeNull();
    expect(relativeToWorkspace("//server/share/项目", "//server/other/项目/a.ts")).toBeNull();
    expect(relativeToWorkspace("C:/项目", "C:/项目/../其他/a.ts")).toBeNull();
  });
});
