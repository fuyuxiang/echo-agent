import { invoke } from "@tauri-apps/api/core";
import { detectPreviewKind } from "./file-kind";

/** Preview reads retain the native file grant and the attachment's 20 MiB cap.
 * Text uses a smaller, visibly truncated prefix; full binary content is loaded
 * only after the user opens a preview and is never kept in the thumbnail cache. */
export async function loadAttachmentPreview(path: string, cwd?: string): Promise<string> {
  const kind = detectPreviewKind(path);
  const stat = await invoke<{ absolute: string; exists: boolean; kind: string }>("path_stat", { path, cwd: cwd ?? null });
  if (!stat.exists || stat.kind !== "file") throw new Error("附件文件不存在");
  if (kind === "binary") return "";
  if (["markdown", "code", "text"].includes(kind)) {
    return invoke<string>("read_text_file", { path: stat.absolute, cwd: null, maxBytes: 256 * 1024 });
  }
  const base64 = await invoke<string>("attachment_preview_data", { path: stat.absolute });
  const extension = path.toLowerCase().split(".").pop() ?? "";
  const mime: Record<string, string> = {
    png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp",
    pdf: "application/pdf", docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", xls: "application/vnd.ms-excel",
  };
  return `data:${mime[extension] ?? "application/octet-stream"};base64,${base64}`;
}

export function attachmentSizeLabel(bytes?: number): string {
  if (bytes === undefined || !Number.isFinite(bytes) || bytes < 0) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
