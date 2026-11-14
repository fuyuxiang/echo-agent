import { describe, expect, it } from "vitest";
import { executionActivities, executionActivityLabel } from "../execution-activity";
import type { MessagePart, ToolCallView } from "@/stores/session-store";

function tool(id: string, extra: Partial<ToolCallView> = {}): MessagePart {
  return { kind: "tool_call", toolCall: { toolCallId: id, kind: "read_file", title: `Read /project/${id}.ts`, status: "completed", content: [], ...extra } };
}

describe("live execution activity summaries", () => {
  it("counts adjacent completed calls without claiming distinct files or modifying records", () => {
    const parts = [tool("a"), tool("b"), tool("c")];
    const snapshot = structuredClone(parts);
    const groups = executionActivities(parts);
    expect(groups).toHaveLength(1);
    expect(groups[0].label).toBe("文件读取 · 3 次操作");
    expect(groups[0].tools.map(tool => tool.toolCallId)).toEqual(["a", "b", "c"]);
    expect(parts).toEqual(snapshot);
  });

  it("preserves commentary, analysis, failures, running calls and different operations as boundaries", () => {
    const parts: MessagePart[] = [tool("a"), { kind: "text", phase: "commentary", text: "继续检查。" },
      tool("b"), { kind: "thought", text: "内部分析" }, tool("c"), tool("failed", { status: "failed" }),
      tool("running", { status: "in_progress" }), tool("fetch", { kind: "web_fetch" }), tool("search", { kind: "web_search" }),
      tool("grep-a", { kind: "grep" }), tool("grep-b", { kind: "grep" })];
    expect(executionActivities(parts).map(group => group.tools.length)).toEqual([1, 1, 1, 1, 1, 1, 1, 1, 1]);
  });

  it("keeps the focused operation stable when it completes next to other reads", () => {
    const held = executionActivities([tool("focused", { status: "in_progress" })]);
    expect(executionActivities([tool("a"), tool("focused"), tool("c"), tool("d")], held)
      .map(group => group.tools.map(tool => tool.toolCallId))).toEqual([["a"], ["focused"], ["c", "d"]]);
  });

  it("keeps a selected group stable as new adjacent reads arrive, then resumes grouping after release", () => {
    const parts = [tool("a"), tool("b")];
    const held = executionActivities(parts);
    const updated = [...parts, tool("c"), tool("d")];
    expect(executionActivities(updated, held).map(group => group.tools.map(tool => tool.toolCallId)))
      .toEqual([["a", "b"], ["c", "d"]]);
    expect(executionActivities(updated)[0].tools).toHaveLength(4);
  });

  it("does not merge calls that have individual document coverage", () => {
    const documentProgress = { source: "资料.xlsx", revision: "abc", readRecords: 2, totalRecords: 10, checkpointCount: 1, complete: false };
    expect(executionActivities([tool("a", { documentProgress }), tool("b", { documentProgress })])).toHaveLength(2);
  });

  it("keeps filenames and commands readable, including wrapped MCP input", () => {
    const value = (part: MessagePart) => part.kind === "tool_call" ? executionActivityLabel(part.toolCall) : "";
    expect(value(tool("a"))).toBe("a.ts");
    expect(value(tool("windows", { rawInput: { path: "C:\\project\\文档.md" } }))).toBe("文档.md");
    expect(value(tool("search", { kind: "grep", title: "Search /src/components for execution", rawInput: { path: "/src/components", pattern: "execution" } }))).toBe("Search /src/components for execution");
    expect(value(tool("command", { kind: "bash", title: "Run test", rawInput: { command: "pnpm test" } }))).toBe("pnpm test");
    expect(value(tool("wrapped", { kind: "other", rawInput: { variant: "UseTool", tool_name: "web_search", tool_input: { query: "官方文档" } } }))).toBe("官方文档");
  });

  it("full records retain distinct workspace-relative paths and preserve external path context", () => {
    const label = (path: string, cwd = "/work") => executionActivityLabel({ toolCallId: path, kind: "read_file", title: `Read ${path}`,
      status: "completed", content: [], rawInput: { path } }, { fullPath: true, cwd });
    expect(label("/work/frontend/config.ts")).toBe("frontend/config.ts");
    expect(label("/work/backend/config.ts")).toBe("backend/config.ts");
    expect(label("/work-other/config.ts")).toBe("/work-other/config.ts");
    expect(label("/Work/config.ts")).toBe("/Work/config.ts");
    expect(label("C:\\Work\\src\\config.ts", "c:\\work\\")).toBe("src/config.ts");
    expect(label("/source.md", "/")).toBe("source.md");
    expect(label("/outside/source.md", "")).toBe("/outside/source.md");
  });
});
