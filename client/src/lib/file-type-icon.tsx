/**
 * 文件类型 → lucide-react SVG 图标。SP2 把资源管理器的 emoji 图标全部换成 SVG，
 * 视觉对齐 VSCode Explorer。
 *
 * 用法：
 *   <FileTypeIcon name="a.ts" kind="file" />
 *   <FileTypeIcon name="src" kind="directory" />
 */
import type { ComponentType, ReactNode } from "react";
import {
  Folder,
  FileText,
  FileCode2,
  FileJson,
  FileType,
  Hash,
  Settings,
  Globe,
  Image as FileImage,
  Music,
  Film,
  FileArchive,
} from "lucide-react";

import { extOf } from "@/lib/drop-utils";

// Use a permissive type — lucide-react's component prop type is stricter than
// what we actually consume (we only forward `size` and `aria-hidden`).
type LucideIcon = ComponentType<Record<string, unknown>>;

const EXT_ICON: Record<string, LucideIcon> = {
  ".ts": FileCode2,
  ".tsx": FileCode2,
  ".js": FileCode2,
  ".jsx": FileCode2,
  ".mjs": FileCode2,
  ".cjs": FileCode2,
  ".json": FileJson,
  ".md": Hash,
  ".mdx": Hash,
  ".py": FileCode2,
  ".rs": FileCode2,
  ".go": FileCode2,
  ".java": FileCode2,
  ".kt": FileCode2,
  ".kts": FileCode2,
  ".c": FileCode2,
  ".h": FileCode2,
  ".cpp": FileCode2,
  ".cxx": FileCode2,
  ".hpp": FileCode2,
  ".cs": FileCode2,
  ".rb": FileCode2,
  ".php": FileCode2,
  ".swift": FileCode2,
  ".sh": FileCode2,
  ".bash": FileCode2,
  ".zsh": FileCode2,
  ".ps1": FileCode2,
  ".css": FileType,
  ".scss": FileType,
  ".less": FileType,
  ".html": Globe,
  ".htm": Globe,
  ".xml": Globe,
  ".svg": FileImage,
  ".png": FileImage,
  ".jpg": FileImage,
  ".jpeg": FileImage,
  ".gif": FileImage,
  ".webp": FileImage,
  ".bmp": FileImage,
  ".ico": FileImage,
  ".mp3": Music,
  ".wav": Music,
  ".ogg": Music,
  ".flac": Music,
  ".mp4": Film,
  ".mov": Film,
  ".mkv": Film,
  ".webm": Film,
  ".zip": FileArchive,
  ".tar": FileArchive,
  ".gz": FileArchive,
  ".tgz": FileArchive,
  ".7z": FileArchive,
  ".rar": FileArchive,
  ".yml": Settings,
  ".yaml": Settings,
  ".toml": Settings,
};

export type FileKind = "file" | "directory" | "other";

export interface FileTypeIconProps {
  name: string;
  kind: FileKind;
  size?: number;
}

/** 返回组件（便于测试 / 复用） */
export function getIconComponentForName(name: string, kind: FileKind): LucideIcon {
  if (kind === "directory") return Folder;
  const ext = extOf(name);
  return EXT_ICON[ext.toLowerCase()] ?? FileText;
}

/** 返回字符串 key（用于脱离 React 的纯断言测试） */
export function getIconKeyForName(name: string, kind: FileKind): string {
  const icon = getIconComponentForName(name, kind);
  // lucide-react 默认导出名形如 FileCode2 / FileText / Folder
  return icon.displayName ?? icon.name ?? "FileText";
}

export function FileTypeIcon({ name, kind, size = 14 }: FileTypeIconProps): ReactNode {
  const Icon = getIconComponentForName(name, kind);
  return <Icon size={size} aria-hidden />;
}
