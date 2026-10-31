import { describe, expect, it } from "vitest";
import { executionProgressText } from "../execution-process-preview";
import type { MessagePart, ToolCallView } from "@/stores/session-store";

const tool = (id: string, status: ToolCallView["status"] = "completed"): MessagePart => ({
  kind: "tool_call", toolCall: { toolCallId: id, kind: "read_file", title: id, status, content: [] },
});

describe("live execution preview", () => {
  it("同段进展保留结果与下一步的上下文", () => {
    expect(executionProgressText([{ kind: "text", text: "官方票价已核实。正在补齐开放时间。" }])).toBe("官方票价已核实。正在补齐开放时间。");
  });
  it("默认进展只采用公开说明，不展示内部分析、工具输出或最终答复", () => {
    expect(executionProgressText([
      { kind: "text", phase: "commentary", text: "已核实票价，正在补齐开放时间。" },
      { kind: "thought", text: "内部分析内容" }, tool("read", "failed"),
      { kind: "text", phase: "final_answer", text: "最终结果" },
    ])).toBe("已核实票价，正在补齐开放时间。");
    expect(executionProgressText([{ kind: "thought", text: "内部分析" }])).toBe("");
  });

  it("代码块不覆盖同阶段已有的可读进展", () => {
    expect(executionProgressText([
      { kind: "text", text: "正在核对资料。" },
      { kind: "text", text: "```js\nconst draft = 1;" },
    ])).toBe("正在核对资料。");
  });

  it("限制长内容并保留 Unicode 字符，空事件不会虚构进展", () => {
    expect(executionProgressText([{ kind: "text", text: "进展".repeat(10_000) + "最新片段" }]))
      .toMatch(/^….{196}最新片段$/);
    expect(executionProgressText([])).toBe("");
  });

  it.each([
    ["```ts", "const draft = 1;\n", "```"],
    ["~~~markdown", "示例代码\n", "~~~~"],
    ["````markdown", "```js\nconst draft = 1;\n```\n", "````"],
    ["  ```ts\r", "const draft = 1;\r\n", "  ```\r"],
  ])("keeps prose after a long %s fence without exposing its code", (opening, code, closing) => {
    const text = `早先的进展。\n\n${opening}\n${code.repeat(300)}${closing}\n\n当前最新决定：**改用备用方案**。`;
    expect(executionProgressText([{ kind: "text", text }])).toBe("当前最新决定：改用备用方案。");
  });

  it("keeps prior prose while a long fence is still streaming, including misleading shorter fences", () => {
    const text = "正在核对方案。\n\n````ts\n" + "const draft = 1;\n".repeat(300) + "```\n仍属于代码块";
    expect(executionProgressText([{ kind: "text", text }])).toBe("正在核对方案。");
    expect(executionProgressText([{ kind: "text", text: "```js\n" + "code\n".repeat(1000) }])).toBe("");
  });

  it("keeps the newest prose across multiple code blocks and preserves complete Unicode characters", () => {
    expect(executionProgressText([{ kind: "text", text: "第一段。\n```js\na\n```\n第二段。\n~~~\nb\n~~~\n最终段落。" }]))
      .toBe("最终段落。");
    const excerpt = executionProgressText([{ kind: "text", text: "进展".repeat(1000) + "😀".repeat(199) + "完成" }]);
    expect(Array.from(excerpt)).toHaveLength(201);
    expect(excerpt).toBe("…" + "😀".repeat(198) + "完成");
  });
});
