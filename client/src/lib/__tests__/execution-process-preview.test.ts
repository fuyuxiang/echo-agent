import { describe, expect, it } from "vitest";
import { executionProcessPreview } from "../execution-process-preview";
import type { MessagePart, ToolCallView } from "@/stores/session-store";

const tool = (id: string, status: ToolCallView["status"] = "completed"): MessagePart => ({
  kind: "tool_call", toolCall: { toolCallId: id, kind: "read_file", title: id, status, content: [] },
});

describe("live execution preview", () => {
  it("recent completed calls cannot push older running work out of view", () => {
    const preview = executionProcessPreview([tool("long-running", "in_progress"), ...[1, 2, 3, 4].map(id => tool(String(id)))]);
    expect(preview.tools.map(item => item.toolCallId)).toEqual(["long-running", "3", "4"]);
    expect(preview.hiddenToolCount).toBe(2);
    expect(preview.hiddenRunningCount).toBe(0);
  });

  it("bounds parallel activity and reports omitted running calls", () => {
    const preview = executionProcessPreview([1, 2, 3, 4, 5].map(id => tool(String(id), "in_progress")));
    expect(preview.tools).toHaveLength(3);
    expect(preview.hiddenRunningCount).toBe(2);
  });

  it("keeps a focused call visible after completion and while newer parallel work arrives", () => {
    const parts = [tool("focused"), ...[1, 2, 3, 4].map(id => tool(String(id), "in_progress"))];
    const pinned = executionProcessPreview(parts, "focused");
    expect(pinned.tools.map(item => item.toolCallId)).toEqual(["focused", "3", "4"]);
    expect(pinned.hiddenToolCount).toBe(2);
    expect(pinned.hiddenRunningCount).toBe(2);
    expect(executionProcessPreview(parts).tools.map(item => item.toolCallId)).toEqual(["2", "3", "4"]);
    expect(executionProcessPreview(parts, "removed").tools).toEqual(executionProcessPreview(parts).tools);
  });

  it("keeps actual failure state and uses the newest readable paragraph", () => {
    const preview = executionProcessPreview([
      { kind: "thought", text: "较早的思考。" }, tool("read", "failed"),
      { kind: "thought", text: "早先段落。\n\n**接下来**检查[文件](https://example.invalid)。" },
    ]);
    expect(preview.tools[0].status).toBe("failed");
    expect(preview.excerpt).toBe("接下来检查文件。");
    expect(preview.excerptKind).toBe("thought");
  });

  it("bounds long streaming thoughts and does not invent activity for empty events", () => {
    expect(executionProcessPreview([{ kind: "thought", text: "思考".repeat(10_000) + "最新片段" }]).excerpt)
      .toMatch(/^….{196}最新片段$/);
    expect(executionProcessPreview([])).toMatchObject({ tools: [], excerpt: "", hiddenToolCount: 0 });
    expect(executionProcessPreview([{ kind: "thought", text: "```js\nconst draft = 1;" }]).excerpt).toBe("");
  });

  it.each([
    ["```ts", "const draft = 1;\n", "```"],
    ["~~~markdown", "示例代码\n", "~~~~"],
    ["````markdown", "```js\nconst draft = 1;\n```\n", "````"],
    ["  ```ts\r", "const draft = 1;\r\n", "  ```\r"],
  ])("keeps prose after a long %s fence without exposing its code", (opening, code, closing) => {
    const text = `早先的进展。\n\n${opening}\n${code.repeat(300)}${closing}\n\n当前最新决定：**改用备用方案**。`;
    expect(executionProcessPreview([{ kind: "thought", text }]).excerpt).toBe("当前最新决定：改用备用方案。");
  });

  it("keeps prior prose while a long fence is still streaming, including misleading shorter fences", () => {
    const text = "正在核对方案。\n\n````ts\n" + "const draft = 1;\n".repeat(300) + "```\n仍属于代码块";
    expect(executionProcessPreview([{ kind: "thought", text }]).excerpt).toBe("正在核对方案。");
    expect(executionProcessPreview([{ kind: "thought", text: "```js\n" + "code\n".repeat(1000) }]).excerpt).toBe("");
  });

  it("keeps the newest prose across multiple code blocks and preserves complete Unicode characters", () => {
    expect(executionProcessPreview([{ kind: "thought", text: "第一段。\n```js\na\n```\n第二段。\n~~~\nb\n~~~\n最终段落。" }]).excerpt)
      .toBe("最终段落。");
    const excerpt = executionProcessPreview([{ kind: "thought", text: "进展".repeat(1000) + "😀".repeat(199) + "完成" }]).excerpt;
    expect(Array.from(excerpt)).toHaveLength(201);
    expect(excerpt).toBe("…" + "😀".repeat(198) + "完成");
  });
});
