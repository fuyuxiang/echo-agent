/**
 * 文件类型识别 —— 对齐 EchoAgent `context-viewer-components` 的 media-preview
 * 类型分发(pdf/docx/pptx/sheet/audio/video/image/markdown/code/browser)。
 *
 * 纯函数:按扩展名/MIME 判定预览类型,供 Context Viewer 选择渲染器。可单测。
 * 重型格式(pdf/docx/pptx/sheet/audio/video)在桌面端需要专门依赖,这里先归到
 * `binary`(显示占位 + 文件名 + 大小);markdown/image/code/text 可本地直接渲染。
 */
import { basenameOf, extOf } from "./drop-utils";
import { AttachmentKind, classifyAttachment } from "./user-message";

export type PreviewKind =
  | "markdown"
  | "image"
  | "code"
  | "text"
  | "pdf"
  | "audio"
  | "video"
  | "docx"
  | "pptx"
  | "sheet"
  | "binary";

const IMAGE_EXTS = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".bmp", ".ico"];
const MARKDOWN_EXTS = [".md", ".markdown", ".mdown", ".mdx"];
const CODE_EXTS = [
  ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs",
  ".json", ".py", ".rs", ".go", ".java", ".kt", ".c", ".h", ".cpp", ".hpp",
  ".cs", ".rb", ".php", ".swift", ".sh", ".bash", ".ps1",
  ".html", ".css", ".scss", ".less", ".sql", ".proto", ".yml", ".yaml", ".toml", ".xml",
];
const TEXT_EXTS = [".txt", ".rst", ".log", ".csv", ".env", ".ini", ".conf"];
const PDF_EXTS = [".pdf"];
const AUDIO_EXTS = [".mp3", ".wav", ".ogg", ".flac", ".m4a", ".aac"];
const VIDEO_EXTS = [".mp4", ".webm", ".mov", ".mkv", ".avi"];
const DOCX_EXTS = [".docx"];
const PPTX_EXTS = [".pptx"];
const SHEET_EXTS = [".xlsx", ".xls"];

function inSet(ext: string, set: string[]): boolean {
  return set.includes(ext);
}

/** 根据文件名(或路径)判定预览类型。 */
export function detectPreviewKind(filename: string): PreviewKind {
  const ext = extOf(filename);
  const base = basenameOf(filename).toLowerCase();
  if (["readme", "license", "notice", "authors", "changelog", "dockerfile", "makefile"].includes(base)
    || (/^\.[a-z0-9_-]+$/i.test(base) && ![".png", ".jpg", ".gif"].includes(base))) {
    return "text";
  }
  if (inSet(ext, MARKDOWN_EXTS)) return "markdown";
  if (inSet(ext, IMAGE_EXTS)) return "image";
  if (inSet(ext, CODE_EXTS)) return "code";
  if (inSet(ext, TEXT_EXTS)) return "text";
  if (inSet(ext, PDF_EXTS)) return "pdf";
  if (inSet(ext, AUDIO_EXTS)) return "audio";
  if (inSet(ext, VIDEO_EXTS)) return "video";
  if (inSet(ext, DOCX_EXTS)) return "docx";
  if (inSet(ext, PPTX_EXTS)) return "pptx";
  if (inSet(ext, SHEET_EXTS)) return "sheet";
  // Composer and preview must agree on readable source/text/data attachments,
  // including Vue, TeX, TSV and config formats outside the syntax-label map.
  const attachmentKind = classifyAttachment(filename);
  if (attachmentKind === AttachmentKind.Code) return "code";
  if (attachmentKind === AttachmentKind.Text || attachmentKind === AttachmentKind.Data) return "text";
  return "binary";
}

/** 是否为「可本地直接渲染」的类型(markdown/image/code/text/audio/video/pdf/
 *  docx/pptx/sheet)。docx/pptx/sheet 需要 ZipReader 注入解压(OOXML 文本提取),
 *  无解压器时降级为占位。 */
export function isLocallyRenderable(kind: PreviewKind): boolean {
  return (
    kind === "markdown" ||
    kind === "image" ||
    kind === "code" ||
    kind === "text" ||
    kind === "audio" ||
    kind === "video" ||
    kind === "pdf" ||
    kind === "docx" ||
    kind === "pptx" ||
    kind === "sheet"
  );
}

/** 人类可读的中文类型标签。 */
export function previewKindLabel(kind: PreviewKind): string {
  switch (kind) {
    case "markdown":
      return "Markdown";
    case "image":
      return "图片";
    case "code":
      return "代码";
    case "text":
      return "文本";
    case "pdf":
      return "PDF";
    case "audio":
      return "音频";
    case "video":
      return "视频";
    case "docx":
      return "Word";
    case "pptx":
      return "PPT";
    case "sheet":
      return "表格";
    default:
      return "文件";
  }
}

/** 估算语言(用于代码高亮提示);无则返回 "text"。 */
export function codeLanguage(filename: string): string {
  const ext = extOf(filename).slice(1); // 去点
  const map: Record<string, string> = {
    ts: "typescript", tsx: "tsx", js: "javascript", jsx: "jsx", mjs: "javascript",
    json: "json", py: "python", rs: "rust", go: "go", java: "java", kt: "kotlin",
    c: "c", h: "c", cpp: "cpp", hpp: "cpp", cs: "csharp", rb: "ruby", php: "php",
    swift: "swift", sh: "bash", bash: "bash", ps1: "powershell",
    html: "html", css: "css", scss: "scss", less: "less",
    sql: "sql", proto: "protobuf", yml: "yaml", yaml: "yaml", toml: "toml", xml: "xml",
  };
  return map[ext] ?? "text";
}
