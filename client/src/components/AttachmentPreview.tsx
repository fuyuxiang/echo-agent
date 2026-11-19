import { lazy, Suspense, useEffect, useId, useMemo, useRef, useState, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import { ChevronLeft, ChevronRight, Copy, ExternalLink, RotateCw, X } from "lucide-react";
import { useModalFocus } from "@/lib/use-modal-focus";
import { loadAttachmentPreview } from "@/lib/attachment-preview";
import { friendlyAttachmentError } from "@/lib/attachment-errors";
import { openLocalPath } from "@/lib/agent-client";
import { copyShareText } from "@/lib/share";
import { FilePreviewMenu, type FilePreviewAction } from "./FilePreviewMenu";
import { attachmentBasename } from "@/lib/user-message";
import { detectPreviewKind, previewKindLabel } from "@/lib/file-kind";
import { useImagePreviewViewport, type ImageSize } from "./markdown/use-image-preview-viewport";

const FilePreview = lazy(() => import("./FilePreview").then(module => ({ default: module.FilePreview })));

export function AttachmentPreview({ paths, path, cwd, onSelect, onClose, returnFocus }: {
  paths: string[]; path: string; cwd?: string; onSelect: (path: string) => void; onClose: () => void;
  returnFocus?: HTMLElement | null;
}) {
  const dialogRef = useModalFocus<HTMLDivElement>(true, onClose, returnFocus);
  const titleId = useId();
  const index = paths.indexOf(path);
  const kind = detectPreviewKind(path);
  const image = kind === "image";
  const file = useMemo(() => ({ path, cwd }), [path, cwd]);
  const currentFile = useRef(file);
  currentFile.current = file;
  const [attempt, setAttempt] = useState(0);
  const request = useMemo(() => ({ file, attempt }), [file, attempt]);
  const [loaded, setLoaded] = useState<{ request: typeof request; content?: string; error?: string }>();
  const [notice, setNotice] = useState<{ file: typeof file; text: string; error?: boolean }>();
  const [opening, setOpening] = useState(false);
  const openGuard = useRef(false);
  const state = loaded?.request === request ? loaded : undefined;

  useEffect(() => {
    let active = true;
    void loadAttachmentPreview(file.path, file.cwd).then(content => {
      if (active) setLoaded({ request, content });
    }).catch(error => {
      if (active) setLoaded({ request, error: friendlyAttachmentError(error, file.path) });
    });
    return () => { active = false; };
  }, [file, request]);

  const retry = () => setAttempt(value => value + 1);
  const open = async () => {
    if (openGuard.current) return;
    openGuard.current = true;
    setOpening(true);
    setNotice(undefined);
    try { await openLocalPath(path, cwd); }
    catch (error) {
      if (currentFile.current === file) setNotice({ file, text: friendlyAttachmentError(error, path), error: true });
    }
    finally { openGuard.current = false; setOpening(false); }
  };
  const originalActions: FilePreviewAction[] = [
    { label: "复制文件路径", icon: Copy, onSelect: () => {
      void copyShareText(path).then(ok => {
        if (currentFile.current === file) setNotice({ file, text: ok ? "已复制文件路径" : "无法复制文件路径", error: !ok });
      });
    } },
    { label: "用系统应用打开", icon: ExternalLink, disabled: opening, onSelect: () => void open() },
  ];
  const fileActions = [...originalActions, { label: "重新加载", icon: RotateCw, onSelect: retry }];
  return createPortal(
    <div className={`attachment-preview-overlay${image ? "" : " attachment-preview-overlay--file"}`}
      onClick={event => { event.stopPropagation(); if (event.target === event.currentTarget) onClose(); }}
      onKeyDown={event => {
        event.stopPropagation();
        if (!event.altKey || event.nativeEvent.isComposing) return;
        const next = event.key === "ArrowLeft" ? index - 1 : event.key === "ArrowRight" ? index + 1 : -1;
        if (next >= 0 && next < paths.length) { event.preventDefault(); onSelect(paths[next]); }
      }}>
      <div ref={dialogRef} className={`attachment-preview${image ? " attachment-preview--image" : ""}`}
        role="dialog" aria-modal="true" aria-labelledby={titleId} tabIndex={-1}>
        <header className="attachment-preview__header">
          <div className="attachment-preview__identity">
            <span id={titleId} className="attachment-preview__title" title={attachmentBasename(path)}>{attachmentBasename(path)}</span>
            <span className="attachment-preview__kind">{previewKindLabel(detectPreviewKind(path))} · 只读预览</span>
          </div>
          {paths.length > 1 && <nav className="attachment-preview__navigation" aria-label="切换附件">
            <button type="button" aria-label="上一个附件" title="上一个附件（Alt+←）" disabled={index <= 0} onClick={() => onSelect(paths[index - 1])}><ChevronLeft size={18} /></button>
            <span aria-live="polite">{index + 1} / {paths.length}</span>
            <button type="button" aria-label="下一个附件" title="下一个附件（Alt+→）" disabled={index >= paths.length - 1} onClick={() => onSelect(paths[index + 1])}><ChevronRight size={18} /></button>
          </nav>}
          <FilePreviewMenu fileKey={`${cwd ?? ""}\0${path}`} actions={fileActions} layer="calc(var(--echo-layer-dialog-local) + 1)" />
          <button type="button" className="attachment-preview__close" aria-label="关闭附件预览" data-modal-initial-focus onClick={onClose}><X size={20} /></button>
        </header>
        <div className="attachment-preview__body" aria-busy={!state}>
          {state?.error ? <div className="attachment-preview__empty" role="alert">
            <span>{state.error}</span><button type="button" onClick={retry}><RotateCw size={15} />重新加载</button>
          </div> : state?.content === undefined ? <div className="attachment-preview__empty" role="status">正在加载附件…</div>
            : image ? <AttachmentImage key={`${path}:${attempt}`} src={state.content} name={attachmentBasename(path)}
              onError={() => setLoaded({ request, error: "图片无法显示，文件可能已损坏。可重新加载或用系统应用打开原件。" })} />
              : <Suspense fallback={<div className="attachment-preview__empty" role="status">正在准备文件预览…</div>}>
                <FilePreview key={path} filename={attachmentBasename(path)} path={path} content={state.content}
                  fileActions={originalActions} onReload={retry} embedded />
              </Suspense>}
        </div>
        {notice?.file === file && <p className="attachment-preview__notice" role={notice.error ? "alert" : "status"}>{notice.text}</p>}
      </div>
    </div>, document.body,
  );
}

function AttachmentImage({ src, name, onError }: { src: string; name: string; onError: () => void }) {
  const stageRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState<ImageSize | null>(null);
  const viewport = useImagePreviewViewport(stageRef, size);
  const style: CSSProperties = size && viewport.scale !== null
    ? { width: size.width * viewport.scale, height: size.height * viewport.scale }
    : { maxWidth: "100%", maxHeight: "100%" };
  return <div className="attachment-preview__image">
    <div className="attachment-preview__zoom" role="group" aria-label="图片缩放">
      <button type="button" aria-label="缩小图片" disabled={!viewport.ready || viewport.scale! <= viewport.minScale} onClick={() => viewport.changeZoom(0.8)}>−</button>
      <span>{viewport.scale === null ? "加载中…" : `${Math.max(1, Math.round(viewport.scale * 100))}%`}</span>
      <button type="button" aria-label="放大图片" disabled={!viewport.ready || viewport.scale! >= viewport.maxScale} onClick={() => viewport.changeZoom(1.25)}>+</button>
      <button type="button" disabled={!viewport.ready} aria-pressed={viewport.zoom === null} onClick={() => viewport.zoomAt(null)}>适应窗口</button>
      <button type="button" disabled={!viewport.ready} aria-pressed={viewport.zoom === 1} onClick={() => viewport.zoomAt(1)}>原始大小</button>
    </div>
    <div ref={stageRef} className="md-image-preview__stage attachment-preview__stage" role="region" aria-label="图片预览画布" tabIndex={0}
      data-pannable={viewport.canPan || undefined} data-dragging={viewport.dragging || undefined}
      onPointerDown={viewport.onPointerDown} onDoubleClick={viewport.onDoubleClick} onKeyDown={viewport.onKeyDown}>
      <div className="md-image-preview__canvas"><img src={src} alt={name} style={style} draggable={false} onError={onError}
        onLoad={event => { const image = event.currentTarget; if (image.naturalWidth && image.naturalHeight) setSize({ width: image.naturalWidth, height: image.naturalHeight }); }} /></div>
    </div>
    <div className="attachment-preview__hint">滚轮或捏合缩放 · 拖拽移动 · 双击查看细节</div>
  </div>;
}
