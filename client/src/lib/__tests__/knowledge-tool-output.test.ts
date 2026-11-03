import { describe, expect, it } from "vitest";
import { knowledgeToolContent, knowledgeToolSource } from "../knowledge-tool-output";

describe("组织 MCP 原生输出", () => {
  const result = (output: unknown, extra = {}) => ({ type: "MCP", server_name: "echoagent_organization_memory", tool_name: "knowledge_ask", output, ...extra });

  it("完整保留带引用的 JSON，过大结果解释限制而不是截断成无效 JSON", () => {
    const text = JSON.stringify({ answer: "答案", citations: [{ docId: "document" }] });
    expect(knowledgeToolContent(result({ OkayOutput: text }))).toEqual([{ type: "text", text }]);
    expect(knowledgeToolContent(result({ OkayOutput: "x".repeat(1_000_001) })))
      .toEqual([{ type: "text", text: expect.stringContaining("缩小查询范围") }]);
  });

  it("原生失败显示失败原因，不能把失败载荷当成有依据的答案", () => {
    expect(knowledgeToolContent(result({ Error: "登录已过期" }, { is_error: true })))
      .toEqual([{ type: "text", text: "组织知识操作未完成：登录已过期" }]);
    expect(knowledgeToolContent(result({ OkayOutput: '{"answer":"不应显示"}' }, { is_error: true })))
      .toEqual([{ type: "text", text: expect.stringContaining("检查组织连接") }]);
  });

  it("未登录组织的个人工具失败只提示本地问题", () => {
    expect(knowledgeToolContent(result({ Error: "文件已不存在" }, { tool_name: "local_knowledge_fetch" })))
      .toEqual([{ type: "text", text: "个人知识库操作未完成：文件已不存在" }]);
    const content = knowledgeToolContent(result({}, { tool_name: "local_knowledge_search", is_error: true }));
    expect(content).toEqual([{ type: "text", text: expect.stringContaining("本地知识目录") }]);
    expect(JSON.stringify(content)).not.toContain("组织");
  });

  it("使用截断前的有界界面结果保留引用，旧历史截断则说明限制", () => {
    const ui = { answer: "答案", qaEventId: "qa", citations: [{ docId: "doc" }] };
    const truncated = '{"answer":"partial\n\n[MCP output truncated: showing first 20 KB]';
    expect(knowledgeToolContent(result({ OkayOutput: truncated }, { ui_result: ui })))
      .toEqual([{ type: "text", text: JSON.stringify(ui) }]);
    expect(knowledgeToolContent(result({ OkayOutput: truncated })))
      .toEqual([{ type: "text", text: expect.stringContaining("未能完整展示") }]);
    expect(knowledgeToolContent(result({ Error: "已退出" }, { ui_result: ui })))
      .toEqual([{ type: "text", text: "组织知识操作未完成：已退出" }]);
  });

  it("只识别精确工具身份，其他服务和未知工具保留原处理路径", () => {
    expect(knowledgeToolSource("echoagent_organization_memory__local_knowledge_search")).toBe("personal");
    expect(knowledgeToolSource("knowledge_context")).toBe("organization");
    expect(knowledgeToolSource("other__knowledge_ask")).toBeNull();
    expect(knowledgeToolSource("search_knowledge_ask_docs")).toBeNull();
    for (const extra of [{ tool_name: "unknown" }, { tool_name: undefined }, { server_name: "another-server" }]) {
      expect(knowledgeToolContent(result({ OkayOutput: "text" }, { ui_result: { answer: "data" }, ...extra }))).toBeUndefined();
    }
  });

  it.each([null, [], "text", { type: "MCP", server_name: "unknown", output: { OkayOutput: "data" } }])
    ("忽略其他格式或其他服务：%j", raw => expect(knowledgeToolContent(raw)).toBeUndefined());
});
