/** Markers used to keep an expert persona in the model prompt but out of UI. */
export const EXPERT_PERSONA_BEGIN = "<!--EXPERT_PERSONA_BEGIN-->";
export const EXPERT_PERSONA_END = "<!--EXPERT_PERSONA_END-->";

/** Metadata key persisted on ACP prompt text blocks for attachment replay. */
export const ATTACHMENTS_META_KEY = "echoAgentAttachments";

/** Legacy suffix written by the desktop bridge before structured metadata. */
export const LEGACY_ATTACHMENT_HEADING =
  "附件（图片已作为多模态内容附加；其他文件请使用 read_file 读取）：";

export function attachmentBasename(path: string): string {
  const normalized = path.replace(/\\/g, "/").replace(/\/+$/, "");
  return normalized.split("/").pop() || path;
}

/**
 * 分类会话内粘贴、拖拽和选择的附件。
 *
 * 设计要点:
 * - 白名单驱动。所有支持的扩展名显式列举,避免误判。
 * - 大小写不敏感。POSIX/Windows 路径都识别。
 * - 安全护栏:可执行二进制 + 平台安装包 (.exe/.dll/.dylib/.app/.pkg/.dmg/
 *   .deb/.rpm/.msi/.bat/.cmd/.com/.scr/.vbs) 全部归类为 Unsupported,
 *   Composer 必须显式拒绝并 toast 提示。
 * - 压缩包 / 音视频本期暂不直接打开(`Unsupported`),留给二期。Agent 场景
 *   优先做代码 + 文档 + 文本 + 数据 这 4 类,覆盖 90% 真实使用。
 */
export enum AttachmentKind {
  Image = "image",
  Document = "document",
  Code = "code",
  Text = "text",
  Data = "data",
  Unsupported = "unsupported",
}

/** 所有 ACP 多模态桥接消费的图像扩展名(小写,带点号)。 */
const IMAGE_EXTENSIONS = new Set([
  ".png",
  ".jpg",
  ".jpeg",
  ".gif",
  ".webp",
]);

/**
 * 文档类:仅包含 Runtime 能可靠提取内容的格式。
 *
 * 旧版二进制 Office（DOC/XLS/PPT）和 RTF 不在此列：把文件显示为“已添加”
 * 但在 read_file 时才失败，比一开始明确拒绝更糟。现代 OOXML、OpenDocument
 * 与 ePub 由 read_file 的专用提取器处理。
 */
const DOCUMENT_EXTENSIONS = new Set([
  ".pdf",
  ".docx",
  ".odt",
  ".ods",
  ".odp",
  ".xlsx",
  ".pptx",
  ".epub",
]);

/** 没有普通扩展名、但 agent 工作流中高频的精确文件名。 */
const SPECIAL_TEXT_FILENAMES = new Set([
  "dockerfile",
  "makefile",
  "rakefile",
  "cmakelists.txt",
  ".gitignore",
  ".gitattributes",
  ".env.example",
  ".editorconfig",
]);

/** 源代码:agent 工作流高频。覆盖主流语言 + 前端框架 + 脚本。 */
const CODE_EXTENSIONS = new Set([
  ".ts",
  ".tsx",
  ".js",
  ".jsx",
  ".mjs",
  ".cjs",
  ".py",
  ".pyi",
  ".pyx",
  ".rs",
  ".go",
  ".java",
  ".kt",
  ".kts",
  ".swift",
  ".c",
  ".cc",
  ".cpp",
  ".cxx",
  ".h",
  ".hh",
  ".hpp",
  ".cs",
  ".rb",
  ".php",
  ".sh",
  ".bash",
  ".zsh",
  ".fish",
  ".ps1",
  // .bat 是 Windows batch 脚本(可执行),归入 UNSUPPORTED_EXTENSIONS 走安全护栏
  ".sql",
  ".vue",
  ".svelte",
  ".astro",
  ".lua",
  ".dart",
  ".r",
  ".scala",
  ".sc",
  ".clj",
  ".cljs",
  ".cljc",
  ".hs",
  ".ex",
  ".exs",
  ".erl",
  ".ml",
  ".mli",
  ".groovy",
  ".gradle",
  ".dockerfile", // 实际无点号,容错
  ".makefile",
  ".mk",
]);

/** 纯文本/标记:可作为模型直接读取的上下文。 */
const TEXT_EXTENSIONS = new Set([
  ".txt",
  ".md",
  ".markdown",
  ".mdown",
  ".log",
  ".org",
  ".rst",
  ".adoc",
  ".tex",
  ".bib",
]);

/** 结构化数据:模型通常需要 YAML/JSON schema 来精确回答。 */
const DATA_EXTENSIONS = new Set([
  ".json",
  ".jsonc",
  ".json5",
  ".yaml",
  ".yml",
  ".toml",
  ".ini",
  ".conf",
  ".cfg",
  ".env",
  ".properties",
  ".csv",
  ".tsv",
  ".xml",
  ".xsd",
  ".xsl",
  ".proto",
  ".graphql",
  ".gql",
]);

/**
 * 不可接受的扩展名 —— 安全护栏说明。
 *
 * 这些是平台可执行 / 安装包 / 脚本二进制 / 屏幕保护程序等,即使我们
 * 只是把路径发给模型、不读字节,也不应让用户把这类文件拖进对话以免
 * 触发模型幻觉式执行建议(给用户展示「运行这段代码」之类的危险输出)。
 *
 * `classifyAttachment` 通过「不在任何已知白名单就归 Unsupported」的兜底
 * 策略自动覆盖(IMAGE / DOCUMENT / CODE / TEXT / DATA 五类),因此不
 * 需要单独查询这里列举的清单。如未来要给某些拒绝给具体文案,再把
 * 这套集合具象化成函数返回 `reason: string`。
 */

/** 按扩展名分类附件。未知扩展名 / 无扩展名 / 可执行二进制均返回 Unsupported。 */
export function classifyAttachment(path: string): AttachmentKind {
  const name = attachmentBasename(path);
  if (!name) return AttachmentKind.Unsupported;
  const lower = name.toLowerCase();
  // Exact-name checks must happen before extension parsing: `.gitignore` and
  // `CMakeLists.txt` both contain a dot, so placing this inside `dot === -1`
  // makes the intended branches unreachable.
  if (SPECIAL_TEXT_FILENAMES.has(lower)) return AttachmentKind.Text;
  const dot = name.lastIndexOf(".");
  if (dot === -1) {
    return AttachmentKind.Unsupported;
  }
  const ext = name.slice(dot).toLowerCase();
  if (IMAGE_EXTENSIONS.has(ext)) return AttachmentKind.Image;
  if (DOCUMENT_EXTENSIONS.has(ext)) return AttachmentKind.Document;
  if (CODE_EXTENSIONS.has(ext)) return AttachmentKind.Code;
  if (TEXT_EXTENSIONS.has(ext)) return AttachmentKind.Text;
  if (DATA_EXTENSIONS.has(ext)) return AttachmentKind.Data;
  return AttachmentKind.Unsupported;
}

/** Image formats that the native ACP bridge sends as multimodal content and
 * can therefore share the same thumbnail behavior in chat history.
 *
 * 保留向后兼容的布尔 API;新代码请优先使用 [`classifyAttachment`]。 */
export function isImageAttachment(path: string): boolean {
  return classifyAttachment(path) === AttachmentKind.Image;
}

export function normalizeAttachmentPaths(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return [...new Set(value.filter((item): item is string =>
    typeof item === "string" && item.trim().length > 0
  ))];
}

/** Remove context that is intentionally sent to the model but hidden in chat. */
export function stripInjectedUserContext(text: string): string {
  let visible = text;
  while (true) {
    const begin = visible.indexOf(EXPERT_PERSONA_BEGIN);
    if (begin === -1) break;
    const end = visible.indexOf(EXPERT_PERSONA_END, begin);
    // An unterminated reserved block is corrupt transport context. Hiding its
    // tail is safer than exposing an internal persona in chat/title surfaces.
    if (end === -1) {
      visible = visible.slice(0, begin);
      break;
    }
    visible = visible.slice(0, begin) + visible.slice(end + EXPERT_PERSONA_END.length);
  }
  visible = visible.replace(/<system-reminder>[\s\S]*?<\/system-reminder>/g, "");
  visible = visible.replace(/<echoagent_team_context>[\s\S]*?(?:<\/echoagent_team_context>|$)/g, "");
  return visible.replace(/^\n+|\n+$/g, "").trim();
}

/**
 * Recover attachment paths from prompts persisted by older desktop versions.
 * Only strip the suffix when every trailing non-empty row is an `- @path`
 * entry, so ordinary user text mentioning the heading is left untouched.
 */
export function parseLegacyAttachmentPrompt(text: string): {
  text: string;
  attachments: string[];
} {
  const parsed = stripAttachmentTransportContext(text);
  return {
    text: stripInjectedUserContext(parsed.text),
    attachments: parsed.attachments,
  };
}

/** Remove only the desktop-generated attachment suffix while preserving
 * model-facing context such as expert/project instructions. */
export function stripAttachmentTransportContext(text: string): {
  text: string;
  attachments: string[];
} {
  const marker = `\n\n${LEGACY_ATTACHMENT_HEADING}\n`;
  const markerIndex = text.lastIndexOf(marker);
  if (markerIndex === -1) {
    return { text, attachments: [] };
  }

  const rows = text
    .slice(markerIndex + marker.length)
    .split("\n")
    .map((row) => row.trim())
    .filter(Boolean);
  if (rows.length === 0 || rows.some((row) => !row.startsWith("- @"))) {
    return { text, attachments: [] };
  }

  const attachments = [...new Set(rows.map((row) => row.slice(3).trim()).filter(Boolean))];
  return {
    text: text.slice(0, markerIndex),
    attachments,
  };
}
