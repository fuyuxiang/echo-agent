import { beforeEach, describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ToolCallCard, ToolCallDetailBody } from "../ToolCallCard";
import type { ToolCallView } from "@/stores/session-store";

const orgApi = vi.hoisted(() => ({
  orgFetchDocument: vi.fn(),
  orgQaFeedback: vi.fn(),
}));

vi.mock("@/lib/org-client", () => orgApi);

const base: ToolCallView = {
  toolCallId: "tc1",
  title: "Write C:\\Users\\example\\hello.txt",
  kind: "edit",
  status: "completed",
  content: [],
};

describe("ToolCallCard", () => {
  it("活动摘要、无障碍名称与悬浮提示使用相同命令，且保留完整内容", () => {
    const command = "pnpm test -- --project ui";
    render(<ToolCallCard tc={{ ...base, kind: "bash", title: "Run test", rawInput: { command } }}
      appearance="activity" activityLabel={command} />);
    const button = screen.getByRole("button", { name: `bash ${command} 完成` });
    expect(button).toHaveTextContent(command);
    expect(button).toHaveAttribute("title", expect.stringContaining(command));
  });

  it("已返回的电脑失败回执不能显示完成标记", () => {
    render(<ToolCallCard tc={{ ...base, toolName: "computer_click", content: [{ type: "text", text: '{"status":"failed"}' }] }} />);
    expect(screen.getByRole("button", { name: /失败$/ })).toHaveClass("toolcall--err");
    expect(screen.getByText("!")).toBeVisible();
  });

  it("原生调用传输失败时保留实体结果未知，不显示为已确认失败", () => {
    const tc: ToolCallView = { ...base, title: "computer_invoke", kind: "other", status: "failed",
      rawInput: {tool_name:"echoagent-automation__computer_invoke"},
      content: [{type:"text",text:"native_timeout: isolated helper terminated; action result unknown"}],
    };
    render(<><ToolCallCard tc={tc} /><ToolCallDetailBody tc={tc} /></>);
    expect(screen.getByRole("button", {name:/结果待核验/})).toHaveClass("toolcall--pending");
    expect(screen.getByText("结果待核验", {selector:".tool-detail__status"})).toBeVisible();
    expect(screen.queryByText("失败", {exact:true})).toBeNull();
  });
  it("展示真实读取覆盖和阶段记录，不把读取完成当作分析完成", () => {
    const tc: ToolCallView = { ...base, kind: "read_file", title: "Read 比赛.xlsx", documentProgress: {
      source: "比赛.xlsx", revision: "a".repeat(64), readRecords: 12, totalRecords: 100, checkpointCount: 2, complete: false,
    } };
    const { rerender } = render(<><ToolCallCard tc={tc} /><ToolCallDetailBody tc={tc} /></>);
    expect(screen.getByRole("status", { name: "资料读取进度" })).toHaveTextContent("已保存 2 份阶段记录");
    expect(screen.getByRole("progressbar")).toHaveAttribute("value", "12");
    expect(screen.queryByText("已分析全部内容")).not.toBeInTheDocument();
    rerender(<ToolCallCard tc={tc} appearance="activity" activityLabel="比赛.xlsx" />);
    expect(screen.getByRole("button", { name: "read_file 比赛.xlsx · 已读取 12 / 100 段内容 完成" })).toBeVisible();
  });
  it("PDF 使用页数、显示当前范围并能打开保存的阶段记录", () => {
    const onOpenPath = vi.fn();
    render(<ToolCallDetailBody onOpenPath={onOpenPath} tc={{ ...base, documentProgress: {
      source: "扫描.pdf", revision: "a".repeat(64), readRecords: 10, totalRecords: 50,
      readPages: 1, totalPages: 5, scope: "第 1 页", checkpointCount: 1, complete: false,
      checkpointPath: "/tmp/session/findings.md",
    } }} />);
    expect(screen.getByRole("status", { name: "资料读取进度" })).toHaveTextContent("已读取 1 / 5 页");
    expect(screen.getByText("本次范围：第 1 页")).toBeInTheDocument();
    expect(screen.getByRole("progressbar")).toHaveAttribute("value", "1");
    fireEvent.click(screen.getByRole("button", { name: "查看阶段记录" }));
    expect(onOpenPath).toHaveBeenCalledWith("/tmp/session/findings.md");
  });
  it.each(["use_tool", "echoagent-office__office_create"])("间接 Office 调用 %s 展示实际文档标题", (title) => {
    const tc: ToolCallView = {
      ...base,
      title,
      kind: "use_tool",
      status: "in_progress",
      rawInput: {
        variant: "UseTool",
        tool_name: "echoagent-office__office_create",
        tool_input: { title: "原生Agent办公验收", format: "docx", markdown: "中文正文" },
      },
    };
    const { rerender } = render(<ToolCallCard tc={tc} />);
    expect(screen.getByText("文档")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "办公文档 生成 原生Agent办公验收 运行中" })).toBeInTheDocument();
    rerender(<ToolCallCard tc={tc} appearance="activity" activityLabel="use_tool" />);
    expect(screen.getByText("生成 原生Agent办公验收")).toBeVisible();
    expect(screen.queryByText("use_tool")).toBeNull();
  });

  it("间接非 Office 工具不因嵌套标题而显示文档", () => {
    render(<ToolCallCard tc={{
      ...base,
      title: "use_tool",
      kind: "use_tool",
      rawInput: { tool_name: "other__create", tool_input: { title: "季度报告", format: "docx" } },
    }} />);
    expect(screen.queryByText("文档")).not.toBeInTheDocument();
    expect(screen.queryByText("生成 季度报告")).not.toBeInTheDocument();
  });

  it("以文档成果展示内置 Office 调用", () => {
    const onOpenPath = vi.fn();
    const officeCall: ToolCallView = {
      toolCallId: "office-1",
      title: "echoagent-office__office_create",
      kind: "mcp_tool",
      status: "completed",
      rawInput: { title: "季度报告", format: "docx" },
      officeReceipt: {
        path: "/work/EchoAgent成果/季度报告.docx",
        format: "docx",
        byteSize: 1024,
        sha256: "a".repeat(64),
      },
      content: [{ type: "text", text: JSON.stringify({
        path: "/work/EchoAgent成果/季度报告.docx",
        format: "docx",
        byteSize: 1024,
        sha256: "a".repeat(64),
      }) }],
    };
    render(<>
      <ToolCallCard tc={officeCall} />
      <ToolCallDetailBody tc={officeCall} onOpenPath={onOpenPath} />
    </>);
    expect(screen.getAllByText("文档")).toHaveLength(2);
    expect(screen.getByText("生成 季度报告")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "/work/EchoAgent成果/季度报告.docx" }));
    expect(onOpenPath).toHaveBeenCalledWith("/work/EchoAgent成果/季度报告.docx");
    expect(screen.queryByText(/sha256/)).not.toBeInTheDocument();
  });

  beforeEach(() => {
    orgApi.orgFetchDocument.mockReset();
    orgApi.orgFetchDocument.mockResolvedValue({ text: "完整原文内容" });
    orgApi.orgQaFeedback.mockReset();
    orgApi.orgQaFeedback.mockResolvedValue(undefined);
  });

  it("renders compact row and opens detail on click", () => {
    const onOpen = vi.fn();
    render(<ToolCallCard tc={base} onOpen={onOpen} />);
    // edit 属专用渲染器,kind 标签显示为「✏️ 文件编辑」。
    expect(screen.getByText("✏️ 文件编辑")).toBeInTheDocument();
    expect(screen.getByText(/hello\.txt/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button"));
    expect(onOpen).toHaveBeenCalledWith(base);
  });

  it("专用渲染器(send-message)显示图标 + 标签 + 摘要", () => {
    render(
      <ToolCallCard
        tc={{
          ...base,
          kind: "send_message",
          title: "通知",
          rawInput: { message: "你好,这是一条通知" },
        }}
        onOpen={() => {}}
      />,
    );
    expect(screen.getByText("💬 发送消息")).toBeInTheDocument();
    expect(screen.getByText("你好,这是一条通知")).toBeInTheDocument();
  });

  it("内置天气和间接搜索调用显示地点与搜索词", () => {
    render(<>
      <ToolCallCard tc={{ ...base, toolCallId: "weather-1", kind: "mcp_tool", title: "echoagent-live-info__weather_forecast", rawInput: { location: "北京" } }} />
      <ToolCallCard tc={{ ...base, toolCallId: "search-1", kind: "use_tool", title: "echoagent-live-info__search_web", rawInput: { tool_input: { query: "最新新闻" } } }} />
    </>);
    expect(screen.getByText("🌤️ 天气查询")).toBeInTheDocument();
    expect(screen.getByText("北京")).toBeInTheDocument();
    expect(screen.getByText("🔍 网络搜索")).toBeInTheDocument();
    expect(screen.getByText("最新新闻")).toBeInTheDocument();
  });

  it("shows running status mark while in progress", () => {
    render(
      <ToolCallCard
        tc={{ ...base, status: "in_progress", title: "Execute notepad" }}
        onOpen={() => {}}
      />,
    );
    expect(screen.getByText("…")).toBeInTheDocument();
  });

  it("工具图片只渲染有界的内联栅格数据", () => {
    render(
      <ToolCallDetailBody
        tc={{
          ...base,
          content: [{ type: "image", mimeType: "image/png", data: "AAAA", uri: "https://tracker.invalid/pixel" }],
        }}
      />,
    );
    expect(screen.getByRole("img")).toHaveAttribute("src", "data:image/png;base64,AAAA");
    expect(screen.getByRole("img").getAttribute("src")).not.toContain("tracker.invalid");
  });

  it("拦截远程、SVG 或无效工具图片", () => {
    render(
      <ToolCallDetailBody
        tc={{
          ...base,
          content: [{ type: "image", mimeType: "image/svg+xml", data: "PHN2Zz4=", uri: "http://127.0.0.1/private" }],
        }}
      />,
    );
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.getByRole("note")).toHaveTextContent("已拦截不安全");
  });

  it("组织上下文按证据与经验卡片展示而不是原始 JSON", () => {
    render(
      <ToolCallDetailBody
        tc={{
          ...base,
          kind: "echoagent_organization_memory__knowledge_context",
          title: "读取组织上下文",
          content: [{ type: "text", text: JSON.stringify({
            sufficient: false,
            confidence: 0.62,
            missingFacts: ["确认当前生效版本"],
            memories: [{ id: "m1", kind: "pitfall", content: "不要跳过灰度验证", stale: false }],
            evidence: [{ chunkId: "c1", docTitle: "发布规范", text: "所有发布必须先灰度。", citation: { heading: "上线流程", page: 3 } }],
          }) }],
        }}
      />,
    );
    expect(screen.getByText("证据有缺口")).toBeInTheDocument();
    expect(screen.getByText("不要跳过灰度验证")).toBeInTheDocument();
    expect(screen.getByText("发布规范")).toBeInTheDocument();
    expect(screen.queryByText(/\"sufficient\"/)).not.toBeInTheDocument();
  });

  it("组织能力降级时明确显示本轮未使用的来源", () => {
    const { container } = render(
      <ToolCallCard
        tc={{
          ...base,
          kind: "echoagent_organization_memory__knowledge_context",
          title: "读取组织上下文",
          content: [{ type: "text", text: JSON.stringify({ available: false, skipped: true }) }],
        }}
      />,
    );
    expect(container).toHaveTextContent("本轮未使用组织知识");
  });

  it("组织知识回答可查看引用原文并提交反馈", async () => {
    render(
      <ToolCallDetailBody
        tc={{
          ...base,
          kind: "echoagent_organization_memory__knowledge_ask",
          title: "查询组织知识",
          content: [{ type: "text", text: JSON.stringify({
            answer: "必须先进行灰度验证。",
            qa_event_id: "qa-1",
            citations: [{
              doc_id: "doc-1",
              docTitle: "发布规范",
              quote: "所有发布必须先灰度。",
              citation: { heading: "上线流程", page: 3 },
            }],
          }) }],
        }}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "查看原文" }));
    await waitFor(() => expect(orgApi.orgFetchDocument).toHaveBeenCalledWith("doc-1", 3));
    expect(await screen.findByText("完整原文内容")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "有帮助" }));
    await waitFor(() => expect(orgApi.orgQaFeedback).toHaveBeenCalledWith("qa-1", "helpful"));
    expect(screen.getByText("感谢反馈，已记录")).toBeInTheDocument();
  });
});
