import { useEffect, useMemo, useRef, useState } from "react";
import { Check, ChevronDown, FolderGit2, FolderOpen, GitBranch, X } from "lucide-react";
import { idePath } from "../lib/windows-path";

export interface ProjectSwitcherItem {
  cwd: string;
  label?: string;
}

interface ProjectSwitcherProps {
  projects: ProjectSwitcherItem[];
  activeCwd: string;
  dirtyCount?: number;
  onSelect: (cwd: string) => void;
  onRemove: (cwd: string) => void;
  onOpenFolder: () => void;
  onManageWorktrees?: () => void;
}

function projectName(cwd: string): string {
  const normalized = idePath(cwd).replace(/\/+$/, "");
  return normalized.split("/").filter(Boolean).pop() ?? cwd ?? "选择项目";
}

/**
 * One window has one active project context. Other entries are recent projects,
 * not peer tabs and not roots inside a fake multi-root workspace.
 */
export function ProjectSwitcher({
  projects,
  activeCwd,
  dirtyCount = 0,
  onSelect,
  onRemove,
  onOpenFolder,
  onManageWorktrees,
}: ProjectSwitcherProps) {
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const toggleRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const entries = useMemo(() => {
    const paths = [{ cwd: activeCwd }, ...projects].filter((project) => Boolean(project.cwd));
    return [...new Map(paths.map((project) => [project.cwd, project])).values()];
  }, [activeCwd, projects]);
  const activeLabel = projects.find((project) => project.cwd === activeCwd)?.label ?? projectName(activeCwd);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: MouseEvent) => {
      if (!containerRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setOpen(false);
      toggleRef.current?.focus();
    };
    window.addEventListener("mousedown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  const focusMenuItem = (position: "first" | "last" | "next" | "previous") => {
    const items = [...(menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? [])];
    if (items.length === 0) return;
    const currentIndex = items.indexOf(document.activeElement as HTMLButtonElement);
    const nextIndex = position === "first"
      ? 0
      : position === "last"
        ? items.length - 1
        : position === "next"
          ? (currentIndex + 1 + items.length) % items.length
          : currentIndex < 0
            ? items.length - 1
            : (currentIndex - 1 + items.length) % items.length;
    items[nextIndex]?.focus();
  };

  return (
    <div className="coding-project-switcher" ref={containerRef}>
      <button
        ref={toggleRef}
        type="button"
        className="coding-project-switcher__toggle"
        aria-label="切换项目"
        aria-expanded={open}
        aria-haspopup="menu"
        title={`当前项目：${activeLabel}\n${idePath(activeCwd)}`}
        onClick={() => setOpen((value) => !value)}
        onKeyDown={(event) => {
          if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
          event.preventDefault();
          setOpen(true);
          window.requestAnimationFrame(() => focusMenuItem(event.key === "ArrowDown" ? "first" : "last"));
        }}
      >
        <FolderGit2 size={13} aria-hidden />
        <small className="coding-project-switcher__label">项目</small>
        <span>{activeLabel}</span>
        {dirtyCount > 0 && (
          <b title={`${dirtyCount} 个未保存文件`} aria-label={`${dirtyCount} 个未保存文件`}>
            {dirtyCount}
          </b>
        )}
        <ChevronDown size={12} aria-hidden />
      </button>

      {open && (
        <div
          ref={menuRef}
          className="coding-project-switcher__menu"
          role="menu"
          aria-label="项目列表"
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") {
              event.preventDefault();
              focusMenuItem("next");
            } else if (event.key === "ArrowUp") {
              event.preventDefault();
              focusMenuItem("previous");
            } else if (event.key === "Home") {
              event.preventDefault();
              focusMenuItem("first");
            } else if (event.key === "End") {
              event.preventDefault();
              focusMenuItem("last");
            }
          }}
        >
          <div className="coding-project-switcher__caption">当前项目与最近项目</div>
          {entries.map((project) => {
            const active = project.cwd === activeCwd;
            const name = project.label ?? projectName(project.cwd);
            return (
              <div
                key={project.cwd}
                className={`coding-project-switcher__row${active ? " is-active" : ""}`}
                role="none"
              >
                <button
                  type="button"
                  role="menuitem"
                  className="coding-project-switcher__select"
                  aria-current={active ? "true" : undefined}
                  aria-label={active ? `当前项目 ${name}` : `切换到项目 ${name}`}
                  onClick={() => {
                    setOpen(false);
                    if (!active) onSelect(project.cwd);
                  }}
                >
                  <span className="coding-project-switcher__check" aria-hidden>
                    {active ? <Check size={12} /> : null}
                  </span>
                  <span>
                    <strong>{name}</strong>
                    <small>{idePath(project.cwd)}</small>
                  </span>
                </button>
                {!active && (
                  <button
                    type="button"
                    role="menuitem"
                    className="coding-project-switcher__remove"
                    aria-label={`从最近项目移除 ${name}`}
                    title="从最近项目移除（不会删除磁盘文件）"
                    onClick={() => {
                      setOpen(false);
                      onRemove(project.cwd);
                    }}
                  >
                    <X size={12} />
                  </button>
                )}
              </div>
            );
          })}
          <div className="coding-project-switcher__separator" />
          {onManageWorktrees && (
            <button
              type="button"
              role="menuitem"
              className="coding-project-switcher__open"
              onClick={() => {
                setOpen(false);
                onManageWorktrees();
              }}
            >
              <GitBranch size={13} />
              <span>管理工作树…</span>
            </button>
          )}
          <button
            type="button"
            role="menuitem"
            className="coding-project-switcher__open"
            onClick={() => {
              setOpen(false);
              onOpenFolder();
            }}
          >
            <FolderOpen size={13} />
            <span>打开其他文件夹…</span>
          </button>
        </div>
      )}
    </div>
  );
}
