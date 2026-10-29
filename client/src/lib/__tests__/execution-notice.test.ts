import { describe, expect, it } from "vitest";
import type { ChatMessage, MessagePart, ToolCallView } from "@/stores/session-store";
import { executionContinuationPrompt, getExecutionNotice } from "../execution-notice";

const tool = (kind: string, status: ToolCallView["status"], extra: Partial<ToolCallView> = {}): MessagePart => ({
  kind: "tool_call", toolCall: { toolCallId: "operation", kind, title: "操作", status, content: [], ...extra },
});
const message = (parts: MessagePart[], extra: Partial<ChatMessage> = {}): ChatMessage => ({
  id: "turn", role: "assistant", complete: true, stopReason: "end_turn",
  parts: [...parts, { kind: "text", text: "已整理查询结果。" }], ...extra,
});

describe("execution notices are about observable impact", () => {
  it.each(["failed", "in_progress"] as const)("真实浏览器上传 %s 的结果提醒不会被正常轮次结束或模型正文覆盖", status => {
    const notice = getExecutionNotice(message([tool("other", status, { toolName: "echoagent-browser__browser_upload" })]));
    expect(notice).toMatchObject({kind:"delivery", tone:"warning"});
    expect(notice?.description).toContain("文件上传");
    expect(notice?.title).toBe(status === "failed" ? "有操作未成功" : "有操作的结果尚未确认");
  });
  it.each(["web_fetch", "web_search", "read_file", "bash", "other"])("%s 的失败和缺失记录保留在详情，不按计数影响最终答复", (kind) => {
    expect(getExecutionNotice(message([tool(kind, "failed"), tool(kind, "in_progress")]))).toBeNull();
  });
  it.each(["write_file", "edit", "send_message", "send_email", "upload_file", "publish_site"])("明确的 %s 失败会说明动作影响并指向原记录", (kind) => {
    const notice = getExecutionNotice(message([tool(kind, "failed")]));
    expect(notice).toMatchObject({ kind: "delivery", tone: "warning", title: "有操作未成功" });
    expect(notice?.tools[0].toolCallId).toBe("operation");
    expect(notice?.description).not.toMatch(/项失败|餐厅|营业/);
  });
  it("发送结果缺失时提示尚未确认，不能断言失败或成功", () => {
    const notice = getExecutionNotice(message([tool("send_message", "in_progress")]));
    expect(notice?.description).toContain("消息发送的结果尚未确认");
    expect(notice?.description).toContain("避免重复操作");
  });
  it("同一动作顺序重试成功后不重复提示过去的失败", () => {
    const parts = [tool("send_message", "failed", { rawInput: { to: "team", text: "结果" }, finishedAt: 100 }),
      tool("send_message", "completed", { toolCallId: "retry", rawInput: { text: "结果", to: "team" }, startedAt: 200 })];
    expect(getExecutionNotice(message(parts))).toBeNull();
  });
  it("识别明确的工具包装信息，不从命令内容或模型成功声明推断交付", () => {
    expect(getExecutionNotice(message([tool("other", "failed", { rawInput: { tool_name: "echoagent-office__office_create" } })]))?.kind).toBe("delivery");
    expect(getExecutionNotice(message([tool("bash", "failed", { title: "send_email failed", rawInput: { command: "upload_file report.pdf" } })]))).toBeNull();
    expect(getExecutionNotice(message([tool("write_file", "failed")], { parts: [tool("write_file", "failed"), { kind: "text", text: "所有工作都已完成" }] }))?.kind).toBe("delivery");
  });
  it("历史记录缺失不会制造失败；涉及交付时明确保留不确定性", () => {
    const legacy = { stopReason: "cancelled", cancellationCategory: "session_replay_incomplete", replayed: true };
    expect(getExecutionNotice(message([tool("read", "failed")], legacy))).toBeNull();
    expect(getExecutionNotice(message([tool("upload_file", "failed")], legacy))?.description).toContain("结果尚未确认");
  });
  it.each(["error", "rate_limit", "max_tokens", "max_turns", "content_filter"])("整轮 %s 即使有正文也清楚提示", (reason) => {
    expect(getExecutionNotice(message([tool("read", "completed")], { stopReason: reason }))?.kind).toBe("runtime");
  });
  it("额度错误只在已确认的失败轮次中识别，不猜测恢复时间", () => {
    expect(getExecutionNotice(message([], { stopReason: "error", agentResult: "five-hour quota exhausted" }))).toMatchObject({ title: "模型额度已用尽", modelSettings: true });
    expect(getExecutionNotice(message([], { agentResult: "quota exhausted" }))).toBeNull();
  });
  it("运行中、立即发送替换旧轮次时不展示终态提示", () => {
    expect(getExecutionNotice(message([tool("write_file", "failed")]), true)).toBeNull();
    expect(getExecutionNotice(message([tool("write_file", "failed")], { stopReason: "cancelled", cancelTrigger: "send_now" }))).toBeNull();
  });
  it("检查并继续保留原轮次，使用易懂的动作名称，不转发内部标识、敏感参数或输出", () => {
    const notice = getExecutionNotice(message([tool("send_email", "failed", { rawInput: { token: "private-token" }, content: [{ type: "text", text: "private-error" }] })]))!;
    const prompt = executionContinuationPrompt(notice);
    expect(prompt).toContain("重点核对消息发送");
    expect(prompt).toContain("不要重复执行已完成的操作");
    expect(prompt).toContain("不要直接重新提交");
    expect(prompt).not.toMatch(/private-token|private-error|operation/);
  });
  it("实际电脑操作缺权限时给出设置入口，不用模型正文或普通查询错误推断", () => {
    const failed = tool("other", "failed", { rawInput: { tool_name: "echoagent-browser__computer_screenshot" }, content: [{ type: "text", text: "尚未授予屏幕录制权限，请在自动化面板中完成授权" }] });
    expect(getExecutionNotice(message([failed]))).toMatchObject({ kind: "environment", title: "电脑操作需要系统权限", securitySettings: true });
    expect(getExecutionNotice(message([tool("web_fetch", "failed", { content: [{ type: "text", text: "需要授予辅助功能权限" }] })]))).toBeNull();
    expect(getExecutionNotice(message([], { parts: [{ kind: "text", text: "需要授予辅助功能权限" }] }))).toBeNull();
  });
  it("桌面不可用的真实失败说明恢复条件", () => {
    expect(getExecutionNotice(message([], { stopReason: "error", agentResult: "桌面暂不可用，任务已暂停；解锁电脑后发送继续可从现场续做" })))
      .toMatchObject({ kind: "environment", title: "桌面暂不可用", description: expect.stringContaining("请解锁电脑") });
  });

});
