/**
 * 文件变更聚合纯函数 —— 对齐 EchoAgent `echo-chat-ui/file-changes-panel`
 * (按文件聚合 tool_call 的 diff,统计增删行,带 vscode 风格文件类型图标映射)。
 *
 * 从会话消息的 tool_call content 里提取所有 diff,按 path 聚合成「每个文件的净变更」。
 * 纯函数、无副作用,便于单测。
 */
import type { ChatMessage } from "@/stores/session-store";
import type { DiffContent } from "@/lib/types";
import type { ReactNode } from "react";
import React from "react";
import { extOf } from "./drop-utils";
import { FileTypeIcon } from "@/lib/file-type-icon";

/** 单个文件的聚合变更。 */
export interface FileChange {
  /** 文件路径(tool 报告的 path)。 */
  path: string;
  /** basename,用于紧凑展示。 */
  name: string;
  /** 净增行数(old → new 的行数差;可为负)。 */
  added: number;
  /** 净删行数(取正)。 */
  removed: number;
  /** 文件类型(扩展名),用于图标映射。 */
  ext: string;
  /** 涉及该文件的变更次数(同一文件多次 edit 会累加)。 */
  edits: number;
}

/** 聚合统计。 */
export interface FileChangesSummary {
  /** 每个文件一条(按首次出现顺序)。 */
  files: FileChange[];
  /** 总文件数。 */
  totalFiles: number;
  /** 总增行。 */
  totalAdded: number;
  /** 总删行。 */
  totalRemoved: number;
}

/** 从一条 diff 内容项计算增删行数(基于 new 与 old 的行数差,带 hunks 时更精确)。 */
function diffLineStats(diff: DiffContent["diff"]): { added: number; removed: number } {
  if (diff.hunks && diff.hunks.length > 0) {
    let added = 0;
    let removed = 0;
    for (const h of diff.hunks) {
      added += h.new.lines.length;
      removed += h.old.lines.length;
    }
    return { added, removed };
  }
  // 无 hunks 时用 old/new 整体行数近似:added = new 行数,removed = old 行数。
  const oldLines = diff.old ? diff.old.split("\n").length : 0;
  const newLines = diff.new ? diff.new.split("\n").length : 0;
  return { added: newLines, removed: oldLines };
}

/** basename 提取。 */
function basename(path: string): string {
  const slash = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return slash >= 0 ? path.slice(slash + 1) : path;
}

/**
 * 从会话消息中聚合文件变更。
 * 同一 path 的多次 edit:增删行累加,edits 计数 +1。
 */
export function aggregateFileChanges(messages: ChatMessage[]): FileChangesSummary {
  const byPath = new Map<string, FileChange>();
  for (const m of messages) {
    for (const p of m.parts) {
      if (p.kind !== "tool_call") continue;
      for (const c of p.toolCall.content ?? []) {
        if (c.type !== "diff") continue;
        const path = c.diff.path || "(unknown)";
        const { added, removed } = diffLineStats(c.diff);
        const existing = byPath.get(path);
        if (existing) {
          existing.added += added;
          existing.removed += removed;
          existing.edits += 1;
        } else {
          byPath.set(path, {
            path,
            name: basename(path),
            added,
            removed,
            ext: extOf(path),
            edits: 1,
          });
        }
      }
    }
  }
  const files = [...byPath.values()];
  const totalAdded = files.reduce((s, f) => s + f.added, 0);
  const totalRemoved = files.reduce((s, f) => s + f.removed, 0);
  return { files, totalFiles: files.length, totalAdded, totalRemoved };
}

/** 文件类型 → ReactNode 图标 (SP2 起改为 lucide-react SVG)。 */
export function fileIcon(ext: string): ReactNode {
  return React.createElement(FileTypeIcon, {
    name: `x${ext}`,
    kind: "file",
    size: 14,
  });
}

/** 变更状态:净增/净删/混合,用于配色。 */
export function changeStatus(change: FileChange): "added" | "removed" | "mixed" {
  if (change.added > 0 && change.removed === 0) return "added";
  if (change.added === 0 && change.removed > 0) return "removed";
  return "mixed";
}
