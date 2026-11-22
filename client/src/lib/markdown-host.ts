import { invoke } from "@tauri-apps/api/core";
import { ask, save } from "@tauri-apps/plugin-dialog";
import type { MarkdownConfig, PathType } from "@/components/markdown/types";

export type MarkdownHostOptions = {
  cwd?: string;
  sessionId?: string | null;
  onToast?: (msg: string) => void;
  onOpenWeb?: (url: string) => void;
};

type PathStat = {
  path: string;
  exists: boolean;
  kind: string;
  absolute: string;
};

function linePreview(code: string, maxLines = 8): string {
  const lines = code.split(/\r?\n/);
  const head = lines.slice(0, maxLines).join("\n");
  if (lines.length > maxLines) {
    return `${head}\n…共 ${lines.length} 行`;
  }
  return head;
}

function joinPath(cwd: string, relative: string): string {
  const sep = cwd.includes("\\") && !cwd.includes("/") ? "\\" : "/";
  const base = cwd.endsWith("/") || cwd.endsWith("\\") ? cwd : `${cwd}${sep}`;
  // If preferred path is already absolute, use as-is.
  if (/^(?:[a-zA-Z]:[\\/]|[\\/])/.test(relative)) return relative;
  return `${base}${relative.replace(/^\.[\\/]/, "")}`;
}

function defaultSnippetName(language: string): string {
  const ext =
    language === "typescript" || language === "ts"
      ? "ts"
      : language === "javascript" || language === "js"
        ? "js"
        : language === "python" || language === "py"
          ? "py"
          : language === "rust" || language === "rs"
            ? "rs"
            : language === "json"
              ? "json"
              : language === "markdown" || language === "md"
                ? "md"
                : language === "css"
                  ? "css"
                  : language === "html"
                    ? "html"
                    : language === "tsx"
                      ? "tsx"
                      : language === "jsx"
                        ? "jsx"
                        : "txt";
  return `snippet.${ext}`;
}

/**
 * Open a local path with the OS (reveal file when possible).
 * Shared by markdown inline paths and tool-call diff paths.
 */
export async function openLocalPath(
  path: string,
  opts: {
    cwd?: string;
    type?: PathType;
    onToast?: (msg: string) => void;
    /** Files are revealed by default; set false when the UI promises to open them. */
    revealFile?: boolean;
    /** Keep a reveal action from unexpectedly launching the file on failure. */
    revealOnly?: boolean;
  },
): Promise<boolean> {
  const { cwd, type, onToast: toast, revealFile = true } = opts;
  if (!cwd && !path.match(/^(?:[a-zA-Z]:[\\/]|[\\/])/)) {
    toast?.("无工作区，无法打开相对路径");
    return false;
  }
  try {
    if (type === "directory") {
      await invoke("open_path", { path, cwd: cwd ?? null });
      return true;
    }
    if (!revealFile) {
      await invoke("open_path", { path, cwd: cwd ?? null });
      return true;
    }
    try {
      await invoke("reveal_in_folder", { path, cwd: cwd ?? null });
    } catch (error) {
      if (opts.revealOnly) throw error;
      await invoke("open_path", { path, cwd: cwd ?? null });
    }
    return true;
  } catch (e) {
    toast?.(String(e).replace(/^Error:\s*/, ""));
    return false;
  }
}

/**
 * Build a MarkdownConfig wired to Tauri shell/fs commands for the active session.
 */
export function createMarkdownHostConfig(
  opts: MarkdownHostOptions,
): MarkdownConfig {
  const { cwd, sessionId, onToast } = opts;
  const toast = (msg: string) => onToast?.(msg);

  const pathClickHandler: MarkdownConfig["pathClickHandler"] = {
    onPathClick: (path, type) => {
      void (async () => {
        await openLocalPath(path, { cwd, type, onToast: toast });
      })();
    },
  };

  const resolveCode: MarkdownConfig["resolveCode"] = async (
    _requestId,
    code,
  ) => {
    const pure = code.replace(/#L\d+(?:-L\d+)?$/, "");
    // Heuristic symbol-only tokens: leave as symbol without FS check.
    if (/^[a-zA-Z_][a-zA-Z0-9_]*$/.test(pure)) {
      return "symbol";
    }
    try {
      const stat = await invoke<PathStat>("path_stat", {
        path: pure,
        cwd: cwd ?? null,
      });
      if (!stat.exists) return "unknown";
      if (stat.kind === "directory") return "directory";
      if (stat.kind === "file") return "file";
      return "unknown";
    } catch {
      return "unknown";
    }
  };

  const openCodeLink: MarkdownConfig["openCodeLink"] = (
    _requestId,
    code,
    type,
  ) => {
    pathClickHandler?.onPathClick(code.replace(/#L\d+(?:-L\d+)?$/, ""), type);
  };

  const onLinkClick: MarkdownConfig["onLinkClick"] = ({ href, event }) => {
    if (!href) return;
    if (opts.onOpenWeb && /^https?:\/\//i.test(href) && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey) {
      event.preventDefault();
      opts.onOpenWeb(href);
      return false;
    }
    if (/^https?:\/\//i.test(href) || /^mailto:/i.test(href)) {
      event.preventDefault();
      void invoke("open_url", { url: href }).catch((e) =>
        toast(String(e).replace(/^Error:\s*/, "")),
      );
      return false;
    }
    return;
  };

  const onApplyCode: MarkdownConfig["onApplyCode"] = (
    code,
    language,
    preferredPath,
  ) => {
    void (async () => {
      if (!cwd) {
        toast("无工作区，无法应用代码到文件");
        return;
      }

      let target: string | null = null;

      // If fence meta already named a file, confirm write directly to that path
      // (still restricted by write_text_file workspace check).
      if (preferredPath && preferredPath.trim()) {
        const resolved = joinPath(cwd, preferredPath.trim());
        const preview = linePreview(code);
        let ok = false;
        try {
          ok = await ask(
            `将把代码写入：\n${resolved}\n\n${preview}\n\n确认覆盖/创建该文件？`,
            {
              title: "应用代码到文件",
              kind: "warning",
              okLabel: "写入",
              cancelLabel: "取消",
            },
          );
        } catch {
          toast("无法显示确认对话框");
          return;
        }
        if (!ok) {

          return;
        }
        try {
          const written = await invoke<string>("write_text_file", {
            path: preferredPath.trim(),
            content: code,
            workspaceRoot: cwd,
          });
          toast(`已更新“${written.split(/[\\/]/).pop()}”`);
        } catch (e) {
          toast(String(e).replace(/^Error:\s*/, ""));
        }
        return;
      }

      // No preferred path — let user pick via save dialog.
      try {
        const picked = await save({
          defaultPath: joinPath(cwd, defaultSnippetName(language)),
          filters: [
            {
              name: "Code",
              extensions: [defaultSnippetName(language).split(".").pop() || "txt", "txt", "*"],
            },
          ],
        });
        if (!picked) {

          return;
        }
        target = picked;
      } catch (e) {
        toast(`选择保存路径失败：${String(e).replace(/^Error:\s*/, "")}`);
        return;
      }

      const preview = linePreview(code);
      let ok = false;
      try {
        ok = await ask(
          `将把以下代码写入：\n${target}\n\n${preview}\n\n确认覆盖/创建该文件？`,
          {
            title: "应用代码到文件",
            kind: "warning",
            okLabel: "写入",
            cancelLabel: "取消",
          },
        );
      } catch {
        toast("无法显示确认对话框");
        return;
      }
      if (!ok) {

        return;
      }

      try {
        const written = await invoke<string>("write_text_file", {
          path: target,
          content: code,
          workspaceRoot: cwd,
        });
        toast(`已更新“${written.split(/[\\/]/).pop()}”`);
      } catch (e) {
        toast(String(e).replace(/^Error:\s*/, ""));
      }
    })();
  };

  return {
    cwd,
    requestId: sessionId ?? undefined,
    pathClickHandler,
    resolveCode,
    openCodeLink,
    onLinkClick,
    onApplyCode,
  };
}

/** Map path_stat kind → Markdown PathType. */
export function pathStatToType(kind: string): PathType {
  if (kind === "file") return "file";
  if (kind === "directory") return "directory";
  return "unknown";
}
