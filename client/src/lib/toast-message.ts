export interface ToastMessage {
  summary: string;
  details?: string;
}

export type ToastMessageKind = "auto" | "info" | "error";

const DIAGNOSTIC_PREFIX = /^(?:[\w.$]*(?:Error|Exception)\b|(?:ENOENT|EACCES|ECONN\w*|ENOTFOUND)\b|os error\b|HTTP(?:\/\d+(?:\.\d+)?)?\s*\d|status(?:[_ ]code)?\s*[:=]?\s*\d|failed\b|fetch failed\b|request timed out\b|timeout\b|timed out\b|no such file\b|not found\b|permission denied\b|access denied\b|unauthorized\b|forbidden\b|disk full\b|no space left\b|\{\s*"(?:error|code|message)")/i;
const FAILURE_CONTEXT = /^([^：:\n]{1,80}(?:失败|未完成|不可用))[：:]\s*/;
const HTTP_STATUS_PREFIX = /^(?:HTTP(?:\/\d+(?:\.\d+)?)?\s*|status(?:[_ ]code)?\s*[:=]?\s*)([45]\d{2})\b/i;
const HTTP_REASONS: Partial<Record<string, string>> = {
  "401": "登录或模型凭据已失效，请检查设置",
  "403": "没有访问权限，请检查授权",
  "404": "找不到所需文件或内容，请刷新后重试",
  "408": "等待响应超时，请稍后重试",
  "429": "请求过于频繁，请稍后重试",
  "502": "暂时无法连接服务，请检查网络后重试",
  "503": "暂时无法连接服务，请检查网络后重试",
  "504": "暂时无法连接服务，请检查网络后重试",
  "507": "存储空间不足，请清理后重试",
};

/** Toasts explain the outcome; diagnostics remain available on demand. */
export function toastMessage(message: string, kind: ToastMessageKind = "auto"): ToastMessage {
  const raw = message.trim();
  const unadorned = raw.replace(/^(?:⚠️?\s*)+/u, "").trim();
  const clean = kind === "info" ? raw : raw.replace(/^(?:⚠️?\s*|Error:\s*)+/u, "").trim();
  if (clean.includes("\n")) {
    // Callers can deliberately separate a product explanation from diagnostics.
    return { summary: toastMessage(clean.split("\n")[0], kind).summary, details: raw };
  }
  const contextMatch = clean.match(FAILURE_CONTEXT);
  const context = contextMatch?.[1];
  // Names and paths are user content, even when they contain HTTP 403 or
  // TypeError. Legacy callers are inferred only from an error prefix; typed
  // callers can bypass inference altogether. Classify the diagnostic suffix
  // rather than an operation's name (for example "下载 TypeError.log 失败").
  const suffix = contextMatch ? clean.slice(contextMatch[0].length) : clean;
  // A native command can add its operation before the actual HTTP error.
  // Keep the user-facing action, but classify the terminal diagnostic.
  const nested = suffix.match(FAILURE_CONTEXT);
  const underlying = nested ? suffix.slice(nested[0].length) : suffix;
  const diagnostic = DIAGNOSTIC_PREFIX.test(underlying) ? underlying : suffix;
  const startsWithError = DIAGNOSTIC_PREFIX.test(unadorned);
  const isError = kind === "error" || (kind === "auto" && !/^(?:已|成功)/u.test(clean) && (Boolean(context) || startsWithError));
  const technical = /(?:\bos error\b|ENOENT|EACCES|ECONN\w*|ENOTFOUND|Failed to fetch|fetch failed|\b\w+Error\b|status[_ ]code|status:\s*\d|\bHTTP\s*\d|stack trace|\bat \w+\s*\(|\{\s*"(?:error|code|message)")/i.test(diagnostic);
  let reason: string | undefined;
  const status = diagnostic.match(HTTP_STATUS_PREFIX)?.[1];
  if (isError && status) {
    reason = HTTP_REASONS[status] ?? "暂时无法完成，请重试或查看详情";
  } else if (isError && (DIAGNOSTIC_PREFIX.test(diagnostic) || !/[\u3400-\u9fff]/u.test(diagnostic))) {
    if (/ENOENT|os error 2\b|no such file|not found|文件不存在/i.test(diagnostic)) reason = "找不到所需文件或内容，请刷新后重试";
    else if (/EACCES|os error 13\b|permission denied|access denied|forbidden|\b403\b/i.test(diagnostic)) reason = "没有访问权限，请检查授权";
    else if (/quotaexceeded|disk full|no space left|os error 28\b/i.test(diagnostic)) reason = "存储空间不足，请清理后重试";
    else if (/\b401\b|unauthorized|invalid.*(?:key|token)/i.test(diagnostic)) reason = "登录或模型凭据已失效，请检查设置";
    else if (/\b429\b|rate.?limit/i.test(diagnostic)) reason = "请求过于频繁，请稍后重试";
    else if (/timeout|timed out/i.test(diagnostic)) reason = "等待响应超时，请稍后重试";
    else if (/ECONN|ENOTFOUND|fetch failed|failed to fetch|network|connection|\b50[234]\b/i.test(diagnostic)) reason = "暂时无法连接服务，请检查网络后重试";
    else if (technical || !/[\u3400-\u9fff]/u.test(diagnostic)) reason = "暂时无法完成，请重试或查看详情";
  }
  if (reason) return { summary: context ? `${context}：${reason}` : reason, details: raw };
  if (clean.length > 110 || /\n/.test(clean)) {
    const first = clean.split(/[。\n]/u)[0];
    return {
      summary: first.length <= 100 ? first : context ? `${context}，请查看详情` : `${clean.slice(0, 88)}…`,
      details: raw,
    };
  }
  return { summary: clean };
}
