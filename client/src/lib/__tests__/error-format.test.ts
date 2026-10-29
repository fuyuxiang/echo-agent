import { describe, it, expect } from "vitest";
import { formatAgentError, friendlyError } from "../error-format";

describe("formatAgentError", () => {
  it("识别服务商窗口超限并保留当前任务的继续处理入口", () => {
    const error = "API error (status 400 Bad Request): bad_request_error: invalid params, context window exceeds limit (2013)";
    expect(formatAgentError(error)).toContain("当前任务");
    expect(formatAgentError(error)).not.toContain("新建会话");
    expect(formatAgentError(JSON.stringify({ data: { message: error } }))).toContain("分批处理");
  });
  it("parses 429 TPM rate limit from Rust Debug string", () => {
    const raw = `Error { code: -32003: Unknown error, message: "Rate limited", data: Some(Object {"message": String("API error (status 429 Too Many Requests): runtime_error: tpm rate limit exceeded"), "promptUsage": Object {"inputTokens": Number(219848), "outputTokens": Number(10094), "totalTokens": Number(229942), "cachedReadTokens": Number(0), "reasoningTokens": Number(2163), "modelCalls": Number(15), "apiDurationMs": Number(211574), "modelUsage": Object {"glm-5": Object {"inputTokens": Number(219848)}}, "numTurns": Number(4)}}) }`;
    const result = formatAgentError(raw);
    expect(result).not.toBeNull();
    expect(result).toContain("TPM");
    expect(result).toContain("219.8k");
    expect(result).toContain("15 次");
    expect(result).toContain("4 轮");
    expect(result).toContain("glm-5");
    expect(result).toContain("恢复时间");
    expect(result).not.toContain("1-2 分钟");
  });

  it("parses 429 RPM rate limit", () => {
    const raw = JSON.stringify({
      code: -32003,
      message: "Rate limited",
      data: {
        message: "API error (status 429): rpm rate limit exceeded",
        promptUsage: { inputTokens: 5000, modelCalls: 3 },
      },
    });
    const result = formatAgentError(raw);
    expect(result).not.toBeNull();
    expect(result).toContain("RPM");
  });

  it("handles auth error", () => {
    const raw = JSON.stringify({
      code: -32003,
      data: { message: "401 Unauthorized: invalid API key" },
    });
    const result = formatAgentError(raw);
    expect(result).not.toBeNull();
    expect(result).toContain("认证失败");
    expect(result).toContain("组织托管模型");
  });

  it("将 Runtime 凭证同步问题与 API Key 被拒绝区分展示", () => {
    const raw = `Error { code: -32000: Authentication required, message: "Authentication required", data: Some(String("no auth method id provided")) }`;
    const result = formatAgentError(raw);
    expect(result).toContain("凭证同步尚未完成");
    expect(result).not.toContain("API Key");
    expect(result).not.toContain("认证失败");
  });

  it("parses ACP internal errors whose Rust data is a String", () => {
    const raw = `Error { code: -32603: Internal error, message: "Internal error", data: Some(String("API request failed with status 401 Unauthorized: missing credentials for runtime-default")) }`;
    const result = formatAgentError(raw);
    expect(result).toContain("认证失败");
    expect(result).not.toContain("Internal error");
  });

  it("handles connection error", () => {
    const raw = JSON.stringify({
      data: { message: "connection refused: ECONNREFUSED" },
    });
    const result = formatAgentError(raw);
    expect(result).not.toBeNull();
    expect(result).toContain("网络连接失败");
  });

  it("将旧版 finish_reason 反序列化错误转为可操作提示", () => {
    const raw = "serialization error: unknown variant ``, expected one of `stop`, `length`, `tool_calls`, `content_filter`, `function_call` at line 1 column 71";
    const result = formatAgentError(raw);

    expect(result).toContain("模型服务返回了");
    expect(result).toContain("设置 → 模型与连接");
    expect(result).toContain("Chat Completions");
    expect(result).not.toContain("unknown variant");
  });

  it("识别 Runtime 的结构化响应协议错误", () => {
    const raw = JSON.stringify({
      code: -32603,
      message: "Internal error",
      data: {
        message: "Model provider returned an incompatible response.",
        error_kind: "serialization",
      },
    });
    const result = formatAgentError(raw);

    expect(result).toContain("本轮已安全停止");
    expect(result).not.toContain("Internal error");
  });

  it("将无法安全区分的并行工具流转为可操作提示", () => {
    const raw = "reqwest error stream: 模型服务返回的并行工具调用缺少稳定标识：索引 0 对应 2 个调用，但当前片段没有调用 ID。";
    const result = formatAgentError(raw);

    expect(result).toContain("无法安全区分");
    expect(result).toContain("不会执行可能错误的操作");
    expect(result).toContain("切换模型");
    expect(result).not.toContain("reqwest error stream");
  });

  it("不把请求序列化失败误报为模型响应协议不兼容", () => {
    const raw = "serialization error: failed to serialize Responses request";
    expect(formatAgentError(raw)).toBeNull();
  });

  it("将回溯协议错配转为安全且可操作的提示", () => {
    const raw = "ext echo.agent/rewind/execute: Error { code: -32602: Invalid params, message: \"Invalid params\", data: Some(String(\"invalid params: unknown variant `conversation`, expected one of `all`, `conversation_only`, `code_only`, `files_only`\")) }";

    const result = formatAgentError(raw);

    expect(result).toContain("回溯服务");
    expect(result).toContain("重启应用或更新");
    expect(result).toContain("未修改对话或文件");
    expect(result).not.toContain("unknown variant");
  });

  it("returns null for unparseable string", () => {
    expect(formatAgentError("some random error")).toBeNull();
  });
});

describe("friendlyError", () => {
  it("formats parseable errors", () => {
    const raw = JSON.stringify({
      code: -32003,
      data: { message: "401 Unauthorized" },
    });
    const result = friendlyError(raw);
    expect(result).toContain("认证失败");
  });

  it("falls back to raw string for unparseable errors", () => {
    const raw = "something went wrong";
    expect(friendlyError(raw)).toBe(raw);
  });

  it("展示普通 Error 时去掉技术性前缀", () => {
    expect(friendlyError(new Error("知识来源尚未就绪"))).toBe("知识来源尚未就绪");
  });

  it("handles Error objects", () => {
    const err = new Error("connection timeout");
    const result = friendlyError(err);
    expect(result).toContain("网络连接失败");
  });
});

describe("quota and rate limit guidance", () => {
  it.each([
    "HTTP 429 quota_error: five-hour quota exhausted",
    JSON.stringify({ code: -32003, message: "Rate limited", data: { message: "429 insufficient_quota" } }),
  ])("额度耗尽保留真实原因，不猜测恢复时间", raw => {
    const message = formatAgentError(raw);
    expect(message).toContain("额度已用尽");
    expect(message).not.toMatch(/1-2|新建会话/);
  });
  it("普通限流不推断为额度耗尽，也不指定未经确认的等待时长", () => {
    const message = formatAgentError("HTTP 429 rate limit exceeded");
    expect(message).toContain("请求受限");
    expect(message).not.toMatch(/额度已用尽|1-2/);
  });
});
