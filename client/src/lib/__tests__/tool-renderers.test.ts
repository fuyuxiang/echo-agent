import { describe, it, expect } from "vitest";
import {
  detectToolRenderer,
  detectToolCallRenderer,
  isToolDiscovery,
  rendererLabel,
  rendererIcon,
  summarizeTool,
} from "../tool-renderers";
import type { ToolCallView } from "@/stores/session-store";

function tc(kind: string, raw?: unknown, title = "T"): ToolCallView {
  return { toolCallId: "x", title, kind, status: "completed", content: [], rawInput: raw };
}

describe("detectToolRenderer", () => {
  it("command 类", () => {
    expect(detectToolRenderer("run_terminal_command")).toBe("command");
    expect(detectToolRenderer("bash")).toBe("command");
    expect(detectToolRenderer("execute_command")).toBe("command");
  });
  it("edit / read / search", () => {
    expect(detectToolRenderer("edit")).toBe("edit");
    expect(detectToolRenderer("write_file")).toBe("edit");
    expect(detectToolRenderer("read_file")).toBe("read");
    expect(detectToolRenderer("grep")).toBe("read");
    expect(detectToolRenderer("web_search")).toBe("search");
    expect(detectToolRenderer("echoagent-live-info__search_web")).toBe("search");
    expect(detectToolRenderer("echoagent-live-info__weather_forecast")).toBe("weather");
  });
  it("专用渲染器", () => {
    expect(detectToolRenderer("defer_execute")).toBe("defer-execute");
    expect(detectToolRenderer("send_message")).toBe("send-message");
    expect(detectToolRenderer("image_gen")).toBe("image-gen");
    expect(detectToolRenderer("generate_image")).toBe("image-gen");
    expect(detectToolRenderer("visualizer")).toBe("visualizer");
    expect(detectToolRenderer("team_create")).toBe("team-create");
    expect(detectToolRenderer("team_delete")).toBe("team-delete");
    expect(detectToolRenderer("team_status")).toBe("team-status");
    expect(detectToolRenderer("agent_mail")).toBe("agent-mail");
    expect(detectToolRenderer("specialist")).toBe("specialist");
    expect(detectToolRenderer("echoagent_organization_memory__local_knowledge_search")).toBe("personal-knowledge");
    expect(detectToolRenderer("echoagent_organization_memory__knowledge_context")).toBe("knowledge");
  });
  it("EchoAgent task 工具 → task 渲染器（优先于 defer 的子串匹配）", () => {
    expect(detectToolRenderer("task")).toBe("task");
    // 注意：含 task 子串但不是纯 task 的（如 defer）不应被 task 拦截
    expect(detectToolRenderer("defer_execute")).toBe("defer-execute");
  });
  it("未知 kind → default", () => {
    expect(detectToolRenderer("something_new")).toBe("default");
  });
  it("空 kind → unknown", () => {
    expect(detectToolRenderer("")).toBe("unknown");
  });
});

describe("Runtime tool identity", () => {
  it.each([
    { title: "echoagent-live-info__weather_forecast" },
    { toolName: "echoagent-live-info__weather_forecast" },
    { toolName: "use_tool", rawInput: { variant: "UseTool", tool_name: "echoagent-live-info__weather_forecast" } },
    { rawInput: { variant: "UseTool", tool_name: "echoagent-live-info__weather_forecast" } },
  ])("从真实直调、UseTool 包装和历史标题识别天气：%j", fields => {
    expect(detectToolCallRenderer({ ...tc("other"), ...fields })).toBe("weather");
  });

  it("工具包装、卡片和活动状态共享搜索与工具发现的识别", () => {
    const wrapped = (tool_name: string) => ({ ...tc("other"), toolName: "use_tool", rawInput: { variant: "UseTool", tool_name } });
    expect(detectToolCallRenderer(wrapped("echoagent-live-info__search_web"))).toBe("search");
    expect(isToolDiscovery(wrapped("search_tools"))).toBe(true);
    expect(isToolDiscovery(wrapped("echoagent-live-info__search_web"))).toBe(false);
    expect(isToolDiscovery(tc("other", undefined, 'Search tools: "天气"'))).toBe(true);
  });

  it("支持 ACP execute/fetch，未知工具不根据描述中的业务词猜测身份", () => {
    expect(detectToolCallRenderer(tc("execute"))).toBe("command");
    expect(detectToolCallRenderer(tc("fetch"))).toBe("search");
    expect(detectToolCallRenderer(tc("other", { tool_name: "echoagent-live-info__weather_forecast" }, "查询天气信息"))).toBe("default");
    expect(detectToolCallRenderer({ ...tc("read"), toolName: "unrecognized_tool" })).toBe("read");
  });
});

describe("rendererLabel / rendererIcon", () => {
  it("各渲染器有标签与图标", () => {
    expect(rendererLabel("command")).toBe("终端命令");
    expect(rendererLabel("image-gen")).toBe("图像生成");
    expect(rendererLabel("team-create")).toBe("创建团队");
    expect(rendererLabel("personal-knowledge")).toBe("个人知识");
    expect(rendererLabel("weather")).toBe("天气查询");
    expect(rendererLabel("default")).toBe("工具");
    expect(rendererLabel("unknown")).toBe("未知工具");
    expect(typeof rendererIcon("image-gen")).toBe("string");
    expect(rendererIcon("default")).toBe("🔧");
  });
});

describe("summarizeTool", () => {
  it("send-message 提取 message", () => {
    const s = summarizeTool(tc("send_message", { message: "你好,这是通知" }), "send-message");
    expect(s).toContain("你好");
  });
  it("内置天气与搜索展示用户输入", () => {
    expect(summarizeTool(tc("echoagent-live-info__weather_forecast", { location: "北京" }), "weather")).toBe("北京");
    expect(summarizeTool(tc("echoagent-live-info__search_web", { query: "最新新闻" }), "search")).toBe("最新新闻");
    expect(summarizeTool(tc("use_tool", { tool_input: { query: "最新新闻" } }), "search")).toBe("最新新闻");
  });
  it("image-gen 提取 prompt", () => {
    const s = summarizeTool(tc("image_gen", { prompt: "一只猫" }), "image-gen");
    expect(s).toContain("一只猫");
    expect(s.startsWith("生成图像:")).toBe(true);
  });
  it("team-create 提取成员数", () => {
    const s = summarizeTool(tc("team_create", { members: [1, 2, 3] }), "team-create");
    expect(s).toContain("3 名成员");
  });
  it("defer-execute 提取命令数", () => {
    const s = summarizeTool(tc("defer_execute", { commands: ["a", "b"] }), "defer-execute");
    expect(s).toContain("2 条命令");
  });
  it("task 渲染器提取 subagent_type + 描述", () => {
    const s = summarizeTool(
      tc("task", { subagent_type: "general-purpose", prompt: "审查代码" }, "Task: review"),
      "task",
    );
    expect(s).toContain("general-purpose");
    expect(s).toContain("审查代码");
  });
  it("task 渲染器长 prompt 截断", () => {
    const long = "x".repeat(80);
    const s = summarizeTool(
      tc("task", { subagent_type: "explore", description: long }, "Task"),
      "task",
    );
    expect(s).toContain("explore");
    // 描述被截断到 50 + …
    expect(s).toContain("…");
  });
  it("task 渲染器无 subagent_type 回退到描述", () => {
    const s = summarizeTool(tc("task", { prompt: "做某事" }, "Task: do"), "task");
    expect(s).toContain("做某事");
  });
  it("default 回退到 title", () => {
    expect(summarizeTool(tc("read_file", undefined, "读取文件"), "read")).toBe("读取文件");
  });
  it("send-message 无 message 字段回退 title", () => {
    expect(summarizeTool(tc("send_message", {}, "原标题"), "send-message")).toBe("原标题");
  });
  it("image-gen prompt 截断 60 字", () => {
    const long = "x".repeat(80);
    const s = summarizeTool(tc("image_gen", { prompt: long }), "image-gen");
    // 「生成图像:」+ 60 字 = 66
    expect(s.length).toBeLessThanOrEqual(70);
  });
});
