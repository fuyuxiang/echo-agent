import { useId, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import type { ProjectMeta } from "@/stores/projects-store";
import { useModalFocus } from "@/lib/use-modal-focus";

/** Select one existing project. The original session and its workspace remain intact. */
export function MoveToProjectDialog({
  sessionTitle,
  sessionCwd,
  projects,
  returnFocus,
  onMove,
  onCreateProject,
  onClose,
}: {
  sessionTitle: string;
  sessionCwd: string;
  projects: ProjectMeta[];
  returnFocus?: HTMLElement;
  onMove: (projectId: string) => Promise<void>;
  onCreateProject?: () => void;
  onClose: () => void;
}) {
  const titleId = useId();
  const descriptionId = useId();
  const errorId = useId();
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const selected = projects.find((project) => project.id === selectedId);
  const directoryMismatch = !!selected?.cwd && !!sessionCwd && selected.cwd !== sessionCwd;
  const visibleProjects = useMemo(() => projects.filter((project) =>
    project.name.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())), [projects, query]);
  const close = () => { if (!busy) onClose(); };
  const dialogRef = useModalFocus<HTMLDivElement>(true, close, returnFocus);

  const move = async () => {
    if (busy || !selectedId) return;
    setBusy(true);
    setError(null);
    try {
      await onMove(selectedId);
      onClose();
    } catch (cause) {
      setError(String(cause).replace(/^Error:\s*/, "") || "移入失败，请重试");
    } finally {
      setBusy(false);
    }
  };

  return createPortal(
    <div className="app-dialog-overlay" onClick={close}>
      <div
        ref={dialogRef}
        className="app-dialog project-move-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={`${descriptionId}${error ? ` ${errorId}` : ""}`}
        aria-busy={busy}
        tabIndex={-1}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="app-dialog__header">
          <h2 id={titleId} className="app-dialog__title">移入项目</h2>
        </div>
        <div className="app-dialog__body">
        <p id={descriptionId} className="app-dialog__description">
          选择“{sessionTitle}”要移入的项目。对话历史和原工作目录会保留。
        </p>
        <label className="app-dialog__field project-move-dialog__search">
          <span>搜索项目</span>
          <input
            type="search"
            value={query}
            onChange={(event) => { setQuery(event.target.value); setSelectedId(null); }}
            placeholder="输入项目名称"
            disabled={busy}
            data-modal-initial-focus
          />
        </label>
        <div className="project-move-dialog__list" role="radiogroup" aria-label="目标项目">
          {visibleProjects.map((project) => (
            <label key={project.id} className={`project-move-dialog__option${selectedId === project.id ? " project-move-dialog__option--selected" : ""}`}>
              <input
                type="radio"
                name="target-project"
                value={project.id}
                checked={selectedId === project.id}
                onChange={() => { setSelectedId(project.id); setError(null); }}
                disabled={busy}
              />
              <span>{project.name}</span>
            </label>
          ))}
          {visibleProjects.length === 0 && (
            <div className="project-move-dialog__empty">
              <p>{projects.length ? "没有匹配的项目" : "还没有项目"}</p>
              {!projects.length && onCreateProject && (
                <button type="button" className="app-dialog__button app-dialog__button--cancel" onClick={onCreateProject}>前往项目页创建</button>
              )}
            </div>
          )}
        </div>
        {directoryMismatch && (
          <p className="project-move-dialog__notice">
            此会话仍在原工作目录运行。需要处理“{selected.name}”的项目目录文件时，请从项目中新建对话。
          </p>
        )}
        {error && <div id={errorId} className="app-dialog__error" role="alert">{error}</div>}
        </div>
        <div className="app-dialog__actions">
          <button type="button" className="app-dialog__button app-dialog__button--cancel" onClick={close} disabled={busy}>取消</button>
          <button type="button" className="app-dialog__button app-dialog__button--primary" onClick={() => void move()} disabled={busy || !selectedId}>
            {busy ? "保存中…" : "移入项目"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
