/**
 * 分享 / 导出链接模式 —— 对齐 EchoAgent `share:*`(share a conversation/artifact
 * via link or downloadable file)。可移植部分:生成可分享的载荷(markdown / 自包含
 * HTML)、构造下载用 blob: URL、构造 mailto / Web Share 意图 URL。
 *
 * EchoAgent 是本地桌面应用,没有云端 share 后端,因此只做「本地导出 + 系统分享意图」,
 * 不上传任何内容(对应 EchoAgent `share:uploadFile/createLink` 的云上传部分不移植)。
 * 纯函数 + 依赖注入(URL / navigator),便于单测。
 */
import {
  buildSessionMarkdown,
  sanitizeFilename,
  type SessionExportOptions,
} from "./export-markdown";
import type { ChatMessage } from "@/stores/session-store";

/** 分享载荷格式。 */
export type ShareFormat = "markdown" | "html" | "text";

/** 构造好的分享载荷。 */
export interface SharePayload {
  /** 文件名(已 sanitize)。 */
  filename: string;
  /** MIME 类型。 */
  mime: string;
  /** 文本内容(markdown / html / text)。 */
  content: string;
  /** 字节大小(UTF-8 近似)。 */
  bytes: number;
}

export interface ShareExportOptions extends SessionExportOptions {
  /** HTML follows the current client theme unless explicitly selected. */
  theme?: "light" | "dark";
}

/** HTML rendering is asynchronous; plain formats stay synchronous for native share activation. */
export function buildSharePayload(messages: ChatMessage[], format: "markdown" | "text", title?: string, options?: ShareExportOptions): SharePayload;
export function buildSharePayload(messages: ChatMessage[], format: "html", title?: string, options?: ShareExportOptions): Promise<SharePayload>;
export function buildSharePayload(messages: ChatMessage[], format: ShareFormat, title?: string, options?: ShareExportOptions): SharePayload | Promise<SharePayload>;
export function buildSharePayload(
  messages: ChatMessage[],
  format: ShareFormat,
  title?: string,
  options: ShareExportOptions = {},
): SharePayload | Promise<SharePayload> {
  const base = sanitizeFilename(title || "对话导出");
  if (format === "markdown") {
    const content = buildSessionMarkdown(messages, title, options);
    return {
      filename: `${base}.md`,
      mime: "text/markdown;charset=utf-8",
      content,
      bytes: byteLength(content),
    };
  }
  if (format === "html") {
    return buildShareHtml(messages, title, options).then(content => ({
      filename: `${base}.html`,
      mime: "text/html;charset=utf-8",
      content,
      bytes: byteLength(content),
    }));
  }
  // text:剥离 markdown 标记的纯文本。
  const content = buildSessionMarkdown(messages, title, options).replace(/^[#>*_`-]+/gm, "").trim();
  return {
    filename: `${base}.txt`,
    mime: "text/plain;charset=utf-8",
    content,
    bytes: byteLength(content),
  };
}

/** Produce a complete offline reading page using the client's Markdown renderer. */
export async function buildShareHtml(
  messages: ChatMessage[],
  title?: string,
  options: ShareExportOptions = {},
): Promise<string> {
  const markdown = buildSessionMarkdown(messages, title, options);
  const theme = options.theme ?? (globalThis.document?.documentElement.dataset.theme === "dark" ? "dark" : "light");
  const { renderShareHtml } = await import("./export-html");
  return renderShareHtml(markdown, title || "对话导出", theme);
}

/** 构造 mailto 分享链接(把正文塞进 body,URL 编码)。 */
export function buildMailtoUrl(
  subject: string,
  body: string,
  deps: { encode?: (s: string) => string } = {},
): string {
  const enc = deps.encode ?? encodeURIComponent;
  return `mailto:?subject=${enc(subject)}&body=${enc(body)}`;
}

/** Copy text with a DOM fallback for desktop webviews without Clipboard API. */
export async function copyShareText(
  text: string,
  deps: {
    clipboard?: Pick<Clipboard, "writeText"> | null;
    document?: Document | null;
  } = {},
): Promise<boolean> {
  const clipboard = "clipboard" in deps ? deps.clipboard : globalThis.navigator?.clipboard;
  if (clipboard?.writeText) {
    try {
      await clipboard.writeText(text);
      return true;
    } catch {
      // Some desktop webviews expose the API but deny it. Keep the same
      // fallback path as webviews without the API.
    }
  }
  const doc = "document" in deps ? deps.document : globalThis.document;
  if (!doc?.body || typeof doc.execCommand !== "function") return false;
  const textarea = doc.createElement("textarea");
  const previousFocus = doc.activeElement as HTMLElement | null;
  textarea.value = text;
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  doc.body.appendChild(textarea);
  try {
    textarea.select();
    return doc.execCommand("copy");
  } catch {
    return false;
  } finally {
    textarea.remove();
    if (previousFocus?.isConnected && typeof previousFocus.focus === "function") previousFocus.focus({ preventScroll: true });
  }
}

/** Use the operating system share sheet when the current webview exposes it. */
export async function systemShare(
  payload: SharePayload,
  title?: string,
  share?: (data: ShareData) => Promise<void>,
): Promise<boolean> {
  const shareFn = share ?? globalThis.navigator?.share?.bind(globalThis.navigator);
  if (!shareFn) return false;
  await shareFn({ title: title || "对话分享", text: payload.content });
  return true;
}

/**
 * 构造下载用 blob: URL。依赖注入 URL.createObjectURL + Blob 以保持可测。
 * 返回 { url, revoke }。
 */
export function buildDownloadUrl(
  payload: SharePayload,
  deps: {
    createObjectURL?: (b: Blob) => string;
    Blob?: typeof Blob;
  } = {},
): { url: string; revoke: () => void } {
  const createObjectURL = deps.createObjectURL ?? ((b: Blob) => URL.createObjectURL(b));
  const BlobCtor = deps.Blob ?? Blob;
  const blob = new BlobCtor([payload.content], { type: payload.mime });
  const url = createObjectURL(blob);
  return { url, revoke: () => URL.revokeObjectURL(url) };
}

/** 触发浏览器下载(创建 <a> 并 click)。依赖注入 document 以保持可测。
 *  显式传 document:null 视为「无 document」(安全返回)。 */
export function triggerDownload(
  payload: SharePayload,
  deps: {
    document?: Document | null;
    createObjectURL?: (b: Blob) => string;
  } = {},
): void {
  // deps.document 显式传入(含 null)优先;未传时回落全局 document。
  const doc =
    "document" in deps
      ? deps.document
      : typeof document !== "undefined"
        ? document
        : undefined;
  if (!doc) return;
  const { url } = buildDownloadUrl(payload, { createObjectURL: deps.createObjectURL });
  const a = doc.createElement("a");
  a.href = url;
  a.download = payload.filename;
  a.rel = "noopener";
  doc.body.appendChild(a);
  a.click();
  doc.body.removeChild(a);
  // 略微延迟 revoke,确保下载已启动。
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

// ---------- 工具 ----------

function byteLength(s: string): number {
  return new TextEncoder().encode(s).length;
}
