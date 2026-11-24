import { describe, it, expect } from "vitest";
import React from "react";
import {
  aggregateFileChanges,
  fileIcon,
  changeStatus,
} from "../file-changes";
import type { ChatMessage } from "@/stores/session-store";

function diffMsg(path: string, old: string, ne: string): ChatMessage {
  return {
    id: path,
    role: "assistant",
    complete: true,
    parts: [
      {
        kind: "tool_call",
        toolCall: {
          toolCallId: "t",
          title: "Edit",
          kind: "edit",
          status: "completed",
          content: [{ type: "diff", diff: { path, old, new: ne } }],
        },
      },
    ],
  };
}

function hunksMsg(
  path: string,
  hunks: Array<{ old: { start: number; lines: string[] }; new: { start: number; lines: string[] } }>,
): ChatMessage {
  return {
    id: path,
    role: "assistant",
    complete: true,
    parts: [
      {
        kind: "tool_call",
        toolCall: {
          toolCallId: "t",
          title: "Edit",
          kind: "edit",
          status: "completed",
          content: [{ type: "diff", diff: { path, old: "", new: "", hunks } }],
        },
      },
    ],
  };
}

describe("aggregateFileChanges", () => {
  it("空消息返回空聚合", () => {
    const s = aggregateFileChanges([]);
    expect(s.files).toEqual([]);
    expect(s.totalFiles).toBe(0);
    expect(s.totalAdded).toBe(0);
    expect(s.totalRemoved).toBe(0);
  });

  it("单文件 diff 聚合(old/new 行数近似)", () => {
    const s = aggregateFileChanges([diffMsg("a.ts", "x\ny", "z")]);
    expect(s.totalFiles).toBe(1);
    expect(s.files[0].path).toBe("a.ts");
    expect(s.files[0].name).toBe("a.ts");
    expect(s.files[0].added).toBe(1); // new "z" → 1 行
    expect(s.files[0].removed).toBe(2); // old "x\ny" → 2 行
    expect(s.files[0].approximate).toBe(true);
    expect(s.files[0].edits).toBe(1);
    expect(s.files[0].ext).toBe(".ts");
  });

  it("带 hunks 时按 hunk 行数统计", () => {
    const s = aggregateFileChanges([
      hunksMsg("a.ts", [
        { old: { start: 1, lines: ["a", "b"] }, new: { start: 1, lines: ["x", "y", "z"] } },
      ]),
    ]);
    expect(s.files[0].added).toBe(3);
    expect(s.files[0].removed).toBe(2);
    expect(s.files[0].approximate).toBe(false);
  });

  it("只在累计变更含整文件替换估算时标记行数", () => {
    const s = aggregateFileChanges([
      diffMsg("a.ts", "", "initial"),
      diffMsg("a.ts", "initial", "updated"),
      diffMsg("a.ts", "updated", ""),
      diffMsg("b.ts", "", "added"),
    ]);
    expect(s.files.map(file => file.approximate)).toEqual([true, false]);
  });

  it("同文件多次 edit 累加 + edits 计数", () => {
    const s = aggregateFileChanges([
      diffMsg("a.ts", "x", "y"),
      diffMsg("a.ts", "y", "z\nw"),
    ]);
    expect(s.totalFiles).toBe(1);
    expect(s.files[0].edits).toBe(2);
    expect(s.files[0].added).toBe(3); // 1 + 2
    expect(s.files[0].removed).toBe(2); // 1 + 1
  });

  it("不同文件分别聚合,保持首次出现顺序", () => {
    const s = aggregateFileChanges([
      diffMsg("a.ts", "x", "y"),
      diffMsg("b.md", "p", "q\nr"),
    ]);
    expect(s.files.map((f) => f.path)).toEqual(["a.ts", "b.md"]);
    expect(s.totalFiles).toBe(2);
  });

  it("basename 正确(带目录)", () => {
    const s = aggregateFileChanges([diffMsg("src/sub/c.ts", "x", "y")]);
    expect(s.files[0].name).toBe("c.ts");
    expect(s.files[0].path).toBe("src/sub/c.ts");
  });

  it("Windows 路径 basename", () => {
    const s = aggregateFileChanges([diffMsg("C:\\proj\\d.ts", "x", "y")]);
    expect(s.files[0].name).toBe("d.ts");
  });

  it("汇总 totalAdded/totalRemoved", () => {
    const s = aggregateFileChanges([
      diffMsg("a.ts", "x\ny\nz", "q"),
      diffMsg("b.ts", "a", "b\nc\nd"),
    ]);
    expect(s.totalAdded).toBe(4);
    expect(s.totalRemoved).toBe(4);
  });

  it("忽略非 diff 的 tool_call 内容", () => {
    const m: ChatMessage = {
      id: "m",
      role: "assistant",
      complete: true,
      parts: [
        {
          kind: "tool_call",
          toolCall: {
            toolCallId: "t",
            title: "Run",
            kind: "run_terminal_command",
            status: "completed",
            content: [{ type: "command_output", command: "ls", output: "a" }],
          },
        },
      ],
    };
    expect(aggregateFileChanges([m]).totalFiles).toBe(0);
  });

  it("忽略 text/thought parts", () => {
    const m: ChatMessage = {
      id: "m",
      role: "assistant",
      complete: true,
      parts: [{ kind: "text", text: "hello" }],
    };
    expect(aggregateFileChanges([m]).totalFiles).toBe(0);
  });
});

describe("fileIcon", () => {
  it("已知扩展名返回 lucide SVG ReactNode", () => {
    expect(React.isValidElement(fileIcon(".ts"))).toBe(true);
    expect(React.isValidElement(fileIcon(".md"))).toBe(true);
    expect(React.isValidElement(fileIcon(".py"))).toBe(true);
  });
  it("未知扩展名也返回 SVG fallback", () => {
    expect(React.isValidElement(fileIcon(".unknownext"))).toBe(true);
    expect(React.isValidElement(fileIcon(""))).toBe(true);
  });
});

describe("changeStatus", () => {
  it("纯增", () => {
    expect(changeStatus({ added: 5, removed: 0 } as never)).toBe("added");
  });
  it("纯删", () => {
    expect(changeStatus({ added: 0, removed: 3 } as never)).toBe("removed");
  });
  it("混合", () => {
    expect(changeStatus({ added: 2, removed: 1 } as never)).toBe("mixed");
  });
});
