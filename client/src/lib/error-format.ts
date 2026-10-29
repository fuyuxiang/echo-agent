/**
 * Parse EchoAgent agent errors into user-friendly Chinese messages.
 *
 * EchoAgent returns errors as Rust `Error` structs serialized to JSON, which
 * produce ugly `Error { code: ..., message: ..., data: ... }` strings when
 * passed through `String(e)`. This utility extracts the human-readable
 * message and, for common cases like 429 rate-limiting, formats a friendly
 * summary with usage stats.
 */

interface PromptUsage {
  inputTokens?: number;
  outputTokens?: number;
  totalTokens?: number;
  cachedReadTokens?: number;
  reasoningTokens?: number;
  modelCalls?: number;
  apiDurationMs?: number;
  numTurns?: number;
  modelUsage?: Record<string, unknown>;
}

/** Exhausted allowance is different from a per-minute rate limit. Neither
 * implies a reset time unless the service explicitly supplies one. */
export function isModelQuotaExhausted(message: string): boolean {
  return /insufficient_quota|quota[^\n]{0,80}(?:exhausted|depleted)|额度[^。\n]{0,40}(?:耗尽|用尽|不足)/i.test(message);
}

interface AgentErrorData {
  message?: string;
  error_kind?: string;
  errorKind?: string;
  promptUsage?: PromptUsage;
  [key: string]: unknown;
}

const INCOMPATIBLE_MODEL_RESPONSE_MESSAGE =
  "⚠️ 模型服务返回了与当前连接协议不兼容的响应，本轮已安全停止。请在「设置 → 模型与连接」中确认 API 协议与服务商一致（OpenAI Chat Completions / Responses 或 Anthropic Messages），然后重试。";

const AMBIGUOUS_PARALLEL_TOOL_MESSAGE =
  "⚠️ 模型服务返回的工具调用无法安全区分。系统已尝试兼容处理并停止本轮，不会执行可能错误的操作。请重试；若持续出现，请切换模型或检查服务商的工具调用兼容性。";

function isIncompatibleModelResponse(message: string): boolean {
  const lower = message.toLowerCase();
  const unknownFinishReason = lower.includes("unknown variant")
    && (lower.includes("finish_reason")
      || (lower.includes("tool_calls") && lower.includes("content_filter")));
  return lower.includes("failed to parse api response")
    || lower.includes("unexpected response format")
    || lower.includes("model provider returned an incompatible response")
    || unknownFinishReason;
}

/** Try to parse a EchoAgent error string into structured data. */
/**
 * Convert Rust Debug-style value wrappers to JSON.
 * Handles: String("...") → "...", Number(123) → 123,
 * Object {...} → {...}, Bool(true) → true, etc.
 */
function rustDebugToJson(s: string): string {
  return s
    // String("value") → "value" (handle escaped quotes inside)
    .replace(/String\("((?:[^"\\]|\\.)*)"\)/g, '"$1"')
    // Number(123) or Number(123.45) → 123 or 123.45
    .replace(/Number\(([^)]+)\)/g, "$1")
    // Bool(true/false) → true/false
    .replace(/Bool\((true|false)\)/g, "$1")
    // Object { → { (the inner content is already JSON-like after above transforms)
    .replace(/Object\s*\{/g, "{")
    // None → null
    .replace(/\bNone\b/g, "null")
    // Some(...) → just the inner value (already unwrapped by caller)
    ;
}

function dataFromDebugValue(content: string): AgentErrorData | undefined {
  const converted = rustDebugToJson(content);
  try {
    const value: unknown = JSON.parse(converted);
    if (typeof value === "string") return { message: value };
    if (value && typeof value === "object") return value as AgentErrorData;
  } catch {
    // Some debug payloads have an object surrounded by another wrapper.
  }

  const braceStart = converted.indexOf("{");
  const braceEnd = converted.lastIndexOf("}");
  if (braceStart >= 0 && braceEnd > braceStart) {
    try {
      return JSON.parse(converted.slice(braceStart, braceEnd + 1));
    } catch {
      // Unrecognized debug payload.
    }
  }
  return undefined;
}

function tryParseAgentError(raw: string): {
  code?: number;
  message?: string;
  data?: AgentErrorData;
} | null {
  // Pattern 1: Rust-style "Error { code: -32003: ..., message: "...", data: Some({...}) }"
  // Pattern 2: JSON stringified Error object
  // Pattern 3: plain error string
  try {
    // Try JSON parse first
    const obj = JSON.parse(raw);
    if (obj && typeof obj === "object") return obj;
  } catch {
    // Not JSON — try regex extraction
  }

  // Rust Debug-style: Error { code: NNN, message: "...", data: Some({...}) }
  const codeMatch = raw.match(/code:\s*(-?\d+)/);
  const msgMatch = raw.match(/message:\s*"([^"]*)"/);

  if (!codeMatch && !msgMatch) return null;

  // Extract data object (the Some({...}) part)
  let data: AgentErrorData | undefined;
  const dataStart = raw.indexOf("data: Some(");
  if (dataStart >= 0) {
    // Find the matching closing paren — count parens
    let depth = 0;
    let start = raw.indexOf("(", dataStart + 10)!;
    for (let i = start; i < raw.length; i++) {
      if (raw[i] === "(") depth++;
      else if (raw[i] === ")") {
        depth--;
        if (depth === 0) {
          const content = raw.slice(start + 1, i);
          try {
            const value: unknown = JSON.parse(content);
            data = typeof value === "string"
              ? { message: value }
              : value as AgentErrorData;
          } catch {
            // Rust Debug format: String("...") or Object {...} wrappers.
            data = dataFromDebugValue(content);
          }
          break;
        }
      }
    }
  }

  return {
    code: codeMatch ? parseInt(codeMatch[1], 10) : undefined,
    message: msgMatch ? msgMatch[1] : undefined,
    data,
  };
}

/** Format token count for display: 219848 → "219.8k" */
function fmtTokens(n?: number): string {
  if (!n) return "0";
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/** Format duration: 211574 → "3分32秒" */
function fmtDuration(ms?: number): string {
  if (!ms) return "";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}秒`;
  const m = Math.floor(s / 60);
  const rs = s % 60;
  return rs > 0 ? `${m}分${rs}秒` : `${m}分钟`;
}

/**
 * Format a EchoAgent error into a user-friendly Chinese string.
 * Returns null if the error can't be parsed (caller should fall back to raw).
 */
export function formatAgentError(raw: string): string | null {
  const lowerRaw = raw.toLowerCase();
  if (isModelQuotaExhausted(raw)) {
    return "模型可用额度已用尽，本轮暂时无法继续。已有内容已保留，请等待服务恢复或选择其他可用模型。";
  }
  if (/context window exceeds limit|context_length_exceeded|context_window_exceeded|maximum context length|input token limit exceeded|prompt is too long|input is too long/.test(lowerRaw)) {
    return "⚠️ 本次内容超过模型的处理容量。请在当前任务中继续分批处理；已保存的资料和阶段记录可继续使用。";
  }
  if (
    lowerRaw.includes("rewind/execute")
    && (lowerRaw.includes("invalid params") || lowerRaw.includes("unknown variant"))
  ) {
    return "⚠️ 回溯服务与当前应用版本不兼容。请重启应用或更新到最新版本后重试；本次操作未修改对话或文件。";
  }
  if (raw.toLowerCase().includes("no auth method id provided")) {
    return "⚠️ 模型凭证同步尚未完成。请稍候后重试；若持续出现，请刷新模型配置。";
  }
  if (raw.includes("并行工具调用缺少稳定标识")) {
    return AMBIGUOUS_PARALLEL_TOOL_MESSAGE;
  }
  // Cover both the current structured Runtime error and legacy builds that
  // leaked serde's raw `unknown variant` diagnostic into the banner.
  if (isIncompatibleModelResponse(raw)) {
    return INCOMPATIBLE_MODEL_RESPONSE_MESSAGE;
  }
  const parsed = tryParseAgentError(raw);

  // If structured parsing failed, check raw string for known patterns.
  if (!parsed) {
    const lower = raw.toLowerCase();
    if (lower.includes("401") || lower.includes("unauthorized") || lower.includes("invalid api key")) {
      return "⚠️ API 认证失败。组织托管模型请重新同步组织配置；个人模型请检查「设置 → 模型与连接」中的 API Key。";
    }
    if (lower.includes("connection") || lower.includes("timeout") || lower.includes("econnrefused")) {
      return "⚠️ 网络连接失败。请检查网络或代理设置，确认模型服务地址可访问。";
    }
    if (lower.includes("429") || lower.includes("rate limit")) {
      return "模型请求受限，请检查服务的调用频率或可用额度后再试。";
    }
    return null;
  }

  const { code, message, data } = parsed;
  const innerMsg = data?.message ?? message ?? "";

  if (
    data?.error_kind === "serialization"
    || data?.errorKind === "serialization"
    || isIncompatibleModelResponse(innerMsg)
  ) {
    return INCOMPATIBLE_MODEL_RESPONSE_MESSAGE;
  }

  // 429 Rate limit
  if (code === -32003 && innerMsg.includes("429")) {
    const usage = data?.promptUsage;
    const isTpm = innerMsg.includes("tpm");
    const isRpm = innerMsg.includes("rpm");
    const limitType = isTpm ? "TPM（每分钟 token 数）" : isRpm ? "RPM（每分钟请求数）" : "API 速率";

    const lines = [`⚠️ ${limitType}限制已触发`];

    if (usage) {
      const model = usage.modelUsage ? Object.keys(usage.modelUsage)[0] : "unknown";
      lines.push(`模型: ${model}`);
      lines.push(
        `本次消耗: 输入 ${fmtTokens(usage.inputTokens)} + 输出 ${fmtTokens(usage.outputTokens)} = ${fmtTokens(usage.totalTokens)} tokens`,
      );
      if (usage.modelCalls) lines.push(`模型调用: ${usage.modelCalls} 次`);
      if (usage.numTurns) lines.push(`对话轮数: ${usage.numTurns} 轮`);
      if (usage.apiDurationMs) lines.push(`耗时: ${fmtDuration(usage.apiDurationMs)}`);
      if (usage.cachedReadTokens === 0) {
        lines.push(`缓存命中: 0（未启用 prompt caching，每次发送完整上下文）`);
      }
    }

    lines.push("");
    lines.push("建议: 根据模型服务提供的限制和恢复时间重试；已有内容已保留。");
    if (usage && usage.inputTokens && usage.inputTokens > 100_000) {
      lines.push("提示: 当前上下文已达 " + fmtTokens(usage.inputTokens) + " tokens，考虑新建会话以减少上下文大小。");
    }

    return lines.join("\n");
  }

  // Auth errors
  if (innerMsg.includes("401") || innerMsg.includes("Unauthorized") || innerMsg.includes("auth")) {
    return "⚠️ API 认证失败。组织托管模型请重新同步组织配置；个人模型请检查「设置 → 模型与连接」中的 API Key。";
  }

  // Connection errors
  if (innerMsg.includes("connection") || innerMsg.includes("timeout") || innerMsg.includes("ECONNREFUSED")) {
    return "⚠️ 网络连接失败。请检查网络或代理设置，确认模型服务地址可访问。";
  }

  // Model not found
  if (innerMsg.includes("model") && (innerMsg.includes("not found") || innerMsg.includes("invalid"))) {
    return `⚠️ 模型不可用：${innerMsg}。请在「设置 → 模型」检查连接和 Model ID。`;
  }

  // Context too long
  if (innerMsg.includes("context") && innerMsg.includes("length")) {
    const usage = data?.promptUsage;
    const tokens = usage?.inputTokens ? fmtTokens(usage.inputTokens) : "过大";
    return `⚠️ 上下文超出模型限制（当前 ${tokens} tokens）。请在当前任务中继续分批处理，或减少本轮附带的历史内容。`;
  }

  // Generic: extract inner message if available
  if (innerMsg) {
    return `⚠️ ${innerMsg}`;
  }

  return null;
}

/**
 * Wrap an error value (string or Error) with formatAgentError.
 * Falls back to String(e) if the error can't be parsed.
 */
export function friendlyError(e: unknown): string {
  const raw = String(e);
  return formatAgentError(raw) ?? raw.replace(/^Error:\s*/, "");
}
