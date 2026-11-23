/**
 * 文件类型 → SVG 图标选择（workspace-panel 标签 / 文件树共用）。
 *
 * SP2 起废除 emoji，统一用 lucide-react。
 */
import { FileTypeIcon } from "@/lib/file-type-icon";
import type { ReactNode } from "react";

/** 按文件名返回 SVG 图标节点（无扩展名走 default）。 */
export function pickFileEmoji(fileName: string): ReactNode {
  return <FileTypeIcon name={fileName} kind="file" size={12} />;
}
