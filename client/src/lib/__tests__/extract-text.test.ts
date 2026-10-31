import { describe, it, expect } from "vitest";
import {
  extractPlainText,
  highlightSegments,
  matchesQuery,
} from "../extract-text";
import type { ChatMessage } from "@/stores/session-store";

function msg(parts: ChatMessage["parts"]): ChatMessage {
  return { id: "m1", role: "assistant", parts, complete: true };
}

describe("extractPlainText", () => {
  it("text part 取原文", () => {
    const m = msg([{ kind: "text", text: "hello world" }]);
    expect(extractPlainText(m)).toBe("hello world");
  });

  it("多 part 用换行拼接", () => {
    const m = msg([
      { kind: "text", text: "first" },
      { kind: "text", text: "second" },
    ]);
    expect(extractPlainText(m)).toBe("first\nsecond");
  });

  it("默认不索引折叠的 thought part", () => {
    const m = msg([
      { kind: "text", text: "answer" },
      { kind: "thought", text: "thinking" },
    ]);
    expect(extractPlainText(m)).toBe("answer");
    expect(extractPlainText(m, { includeThoughts: true })).toBe("answer\nthinking");
  });

  it("明确标记的进展不会进入正文搜索", () => {
    const m = msg([
      { kind: "text", phase: "commentary", text: "internal English analysis" },
      {
        kind: "tool_call",
        toolCall: {
          toolCallId: "read-1",
          title: "Read file",
          kind: "read_file",
          status: "completed",
          content: [],
        },
      },
      { kind: "text", text: "最终答案" },
    ]);

    expect(extractPlainText(m)).toBe("最终答案");
    expect(extractPlainText(m, { includeProcess: true }))
      .toContain("internal English analysis");
  });

  it("过滤模型在普通文本中返回的 think 标签内容", () => {
    const m = msg([{
      kind: "text",
      text: "<think>private reasoning</think>\n\n用户可见答案",
      streamId: "generation-1",
    }]);

    expect(extractPlainText(m)).toBe("用户可见答案");
    expect(extractPlainText(m, { includeProcess: true }))
      .toBe("private reasoning\n用户可见答案");
  });

  it("默认不索引折叠的 tool_call，显式开启时提取详情", () => {
    const m = msg([
      {
        kind: "tool_call",
        toolCall: {
          toolCallId: "t1",
          title: "Run ls",
          kind: "run_terminal_command",
          status: "completed",
          content: [
            { type: "command_output", command: "ls -la", output: "file1\nfile2" },
          ],
        },
      },
    ]);
    expect(extractPlainText(m)).toBe("");
    expect(extractPlainText(m, { includeProcess: true }))
      .toBe("Run ls\nls -la\nfile1\nfile2");
  });

  it("tool_call 提取 diff path", () => {
    const m = msg([
      {
        kind: "tool_call",
        toolCall: {
          toolCallId: "t2",
          title: "Edit src/app.ts",
          kind: "edit",
          status: "completed",
          content: [
            {
              type: "diff",
              diff: { path: "src/app.ts", old: "a", new: "b" },
            },
          ],
        },
      },
    ]);
    expect(extractPlainText(m)).toBe("");
    expect(extractPlainText(m, { includeProcess: true }))
      .toBe("Edit src/app.ts\nsrc/app.ts");
  });

  it("空消息返回空串", () => {
    expect(extractPlainText(msg([]))).toBe("");
  });
});

describe("highlightSegments", () => {
  it("空 query 返回整段非命中", () => {
    expect(highlightSegments("hello", "")).toEqual([{ text: "hello", hit: false }]);
  });

  it("命中处标记 hit", () => {
    const segs = highlightSegments("Hello world hello", "hello");
    expect(segs.filter((s) => s.hit).map((s) => s.text)).toEqual(["Hello", "hello"]);
    // 拼接还原原文
    expect(segs.map((s) => s.text).join("")).toBe("Hello world hello");
  });

  it("大小写不敏感", () => {
    const segs = highlightSegments("Find me HERE", "here");
    expect(segs.some((s) => s.hit && s.text === "HERE")).toBe(true);
  });

  it("无命中返回整段", () => {
    expect(highlightSegments("abc", "xyz")).toEqual([{ text: "abc", hit: false }]);
  });

  it("正则元字符被转义(不抛错)", () => {
    const segs = highlightSegments("a(b).c", "(b)");
    expect(segs.filter((s) => s.hit).map((s) => s.text)).toEqual(["(b)"]);
  });

  it("开头命中", () => {
    const segs = highlightSegments("foo bar", "foo");
    expect(segs[0]).toEqual({ text: "foo", hit: true });
  });

  it("结尾命中保留剩余", () => {
    const segs = highlightSegments("x foo", "foo");
    expect(segs).toEqual([
      { text: "x ", hit: false },
      { text: "foo", hit: true },
    ]);
  });
});

describe("matchesQuery", () => {
  it("大小写不敏感命中", () => {
    expect(matchesQuery("Hello World", "world")).toBe(true);
    expect(matchesQuery("Hello World", "WORLD")).toBe(true);
  });

  it("未命中", () => {
    expect(matchesQuery("abc", "xyz")).toBe(false);
  });

  it("空 query 视为未命中", () => {
    expect(matchesQuery("abc", "")).toBe(false);
  });
});
