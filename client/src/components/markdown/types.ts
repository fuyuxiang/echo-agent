import type { ReactNode, MouseEvent } from "react";

export type MarkdownTheme = "legacy" | "loose" | "reasoning";

export type PathType = "file" | "directory" | "symbol" | "unknown";

export type CodeBlockAction = {
  id: string;
  label: string;
  description?: string;
  icon?: ReactNode;
  onClick: (code: string, language: string) => void;
  condition?: (code: string, language: string) => boolean;
};

export type PathClickHandler = {
  onPathClick: (
    path: string,
    type: PathType,
    range?: { start: number; end?: number },
  ) => void;
};

export type MarkdownConfig = {
  /** Render portable reading content without app-only controls or effects. */
  staticRendering?: boolean;
  /** Workspace cwd for relative path resolution (Phase 3). */
  cwd?: string;
  requestId?: string;
  pathClickHandler?: PathClickHandler;
  resolveCode?: (
    requestId: string,
    code: string,
    paths?: string[],
  ) => Promise<PathType>;
  openCodeLink?: (requestId: string, code: string, type: PathType) => void;
  renderInlineCodePathIcon?: (info: {
    code: string;
    purePath: string;
    type: PathType;
  }) => ReactNode;
  /**
   * Apply code to a file. `preferredPath` comes from fence meta when present
   * (e.g. `ts:1:20:src/foo.ts` → `src/foo.ts`).
   */
  onApplyCode?: (
    code: string,
    language: string,
    preferredPath?: string,
  ) => void;
  onCodeBlockAction?: (
    action: string,
    code: string,
    language: string,
    requestId?: string,
  ) => void;
  codeBlockActions?: CodeBlockAction[];
  imageUrlResolver?: (src: string) => string;
  imageUrlResolverAsync?: (src: string) => Promise<string | null | undefined>;
  onImageResolveError?: (error: unknown, info: { src: string }) => void;
  customUrlSchemes?: Array<{
    scheme: string;
    linkifyValidate?: (text: string, pos: number) => number;
  }>;
  onLinkClick?: (info: {
    href: string;
    title?: string | null;
    children: ReactNode;
    event: MouseEvent<HTMLAnchorElement>;
  }) => boolean | void;
  onDownloadMermaid?: (svg: string, code: string) => void;
  onPreviewMermaid?: (svg: string, code: string) => void;
  expandThreshold?: number;
};

export type MarkdownProps = {
  children: string;
  /** When false, streaming is still in progress (gate mermaid etc.). */
  complete?: boolean;
  markdownTheme?: MarkdownTheme;
  config?: MarkdownConfig;
  /** Host UI theme for mermaid / diagrams (Phase 2). */
  theme?: "light" | "dark";
  /** 会话内查找的当前关键词(非空时启用 inline 高亮)。 */
  findQuery?: string;
};
