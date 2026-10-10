import {
  useId,
  useRef,
  useState,
  type CSSProperties,
  type ImgHTMLAttributes,
  type KeyboardEvent,
  type MouseEvent,
} from "react";
import { createPortal } from "react-dom";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useImagePreviewViewport, validImageSize, type ImageSize } from "./use-image-preview-viewport";

type PreviewableImageProps = Omit<ImgHTMLAttributes<HTMLImageElement>, "src" | "alt"> & {
  src: string;
  alt?: string;
  previewTitle?: string;
  previewLabel?: string;
  intrinsicSize?: ImageSize;
  onPreview?: () => void;
  previewOpen?: boolean;
  onPreviewOpenChange?: (open: boolean) => void;
};

function ImagePreviewDialog({
  src,
  title,
  intrinsicSize,
  onClose,
}: {
  src: string;
  title: string;
  intrinsicSize?: ImageSize;
  onClose: () => void;
}) {
  const dialogRef = useModalFocus<HTMLDivElement>(true, onClose);
  const stageRef = useRef<HTMLDivElement>(null);
  const [loadedSize, setLoadedSize] = useState<ImageSize | null>(null);
  const [loadError, setLoadError] = useState(false);
  const hintId = useId();
  const imageSize = loadError ? null : validImageSize(intrinsicSize) ? intrinsicSize : loadedSize;
  const viewport = useImagePreviewViewport(stageRef, imageSize);
  const { scale, zoom, ready } = viewport;
  const imageStyle: CSSProperties = imageSize && scale !== null
    ? { width: imageSize.width * scale, height: imageSize.height * scale }
    : { maxWidth: "100%", maxHeight: "100%" };

  return createPortal(
    <div
      className="md-image-preview__overlay"
      onPointerDownCapture={(event) => {
        if (event.target === event.currentTarget) viewport.resetClickSuppression();
      }}
      onClick={(event) => {
        event.stopPropagation();
        if (event.target === event.currentTarget && !viewport.suppressClick()) onClose();
      }}
    >
      <div
        ref={dialogRef}
        className="md-image-preview__dialog"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
      >
        <div className="md-image-preview__toolbar">
          <span className="md-image-preview__title" title={title}>{title}</span>
          <div className="md-image-preview__actions">
            <button type="button" onClick={() => viewport.changeZoom(0.8)} disabled={!ready || scale! <= viewport.minScale} aria-label="缩小图片">−</button>
            <span className="md-image-preview__scale">{scale === null ? "适应窗口" : scale < 0.01 ? "<1%" : `${Math.round(scale * 100)}%`}</span>
            <button type="button" onClick={() => viewport.changeZoom(1.25)} disabled={!ready || scale! >= viewport.maxScale} aria-label="放大图片">+</button>
            <button type="button" onClick={() => viewport.zoomAt(null)} disabled={!ready} aria-pressed={zoom === null}>适应窗口</button>
            <button type="button" onClick={() => viewport.zoomAt(1)} disabled={!ready} aria-pressed={zoom === 1}>原始大小</button>
            <button type="button" onClick={() => viewport.setWheelPan(!viewport.wheelPan)} disabled={!ready} aria-label={viewport.wheelPan ? "切换为滚轮缩放" : "切换为滚轮平移"} aria-pressed={viewport.wheelPan} title="切换滚轮操作；触控板可选平移，捏合始终缩放">{viewport.wheelPan ? "滚轮平移" : "滚轮缩放"}</button>
            <button type="button" className="md-image-preview__close" onClick={onClose} aria-label="关闭图片预览">✕</button>
          </div>
        </div>
        <div
          ref={stageRef}
          className="md-image-preview__stage"
          role="region"
          aria-label="图片预览画布"
          aria-describedby={hintId}
          data-modal-initial-focus
          tabIndex={0}
          data-pannable={viewport.canPan || undefined}
          data-dragging={viewport.dragging || undefined}
          onPointerDown={viewport.onPointerDown}
          onDoubleClick={viewport.onDoubleClick}
          onKeyDown={viewport.onKeyDown}
        >
          <div className="md-image-preview__canvas">
            {loadError ? (
              <div className="md-image-preview__error" role="alert">图片无法加载，资源可能已失效。</div>
            ) : (
              <img
                src={src}
                alt={title}
                style={imageStyle}
                draggable={false}
                onDragStart={(event) => event.preventDefault()}
                onError={() => setLoadError(true)}
                onLoad={(event) => {
                  if (validImageSize(intrinsicSize)) return;
                  const image = event.currentTarget;
                  if (image.naturalWidth > 0 && image.naturalHeight > 0) {
                    setLoadedSize({ width: image.naturalWidth, height: image.naturalHeight });
                  }
                }}
              />
            )}
          </div>
        </div>
        <div id={hintId} className="md-image-preview__hint">
          <span>{viewport.wheelPan ? "滚轮平移 · 捏合缩放" : "滚轮缩放 · Shift+滚轮平移"} · 拖拽移动 · 双击查看细节</span>
          <span>键盘：+/− 缩放，0 适应，1 原始大小，方向键移动</span>
        </div>
      </div>
    </div>,
    document.body,
  );
}

export function MarkdownPreviewImage({
  src,
  alt,
  previewTitle,
  previewLabel,
  intrinsicSize,
  onPreview,
  previewOpen,
  onPreviewOpenChange,
  className,
  onClick,
  onKeyDown,
  ...imgProps
}: PreviewableImageProps) {
  const [internalOpen, setInternalOpen] = useState(false);
  const open = previewOpen ?? internalOpen;
  const setOpen = (next: boolean) => {
    if (previewOpen === undefined) setInternalOpen(next);
    onPreviewOpenChange?.(next);
  };
  const title = previewTitle ?? (alt?.trim() ? `图片预览：${alt.trim()}` : "图片预览");
  const label = previewLabel ?? (alt?.trim() ? `放大预览：${alt.trim()}` : "放大预览图片");
  const openPreview = () => {
    if (onPreview) onPreview();
    else setOpen(true);
  };
  const handleClick = (event: MouseEvent<HTMLImageElement>) => {
    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.focus();
    onClick?.(event);
    openPreview();
  };
  const handleKeyDown = (event: KeyboardEvent<HTMLImageElement>) => {
    onKeyDown?.(event);
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    event.stopPropagation();
    openPreview();
  };

  return (
    <>
      <img
        {...imgProps}
        src={src}
        alt={alt}
        className={`md-previewable-image${className ? ` ${className}` : ""}`}
        role="button"
        tabIndex={0}
        aria-haspopup="dialog"
        aria-label={label}
        onClick={handleClick}
        onKeyDown={handleKeyDown}
      />
      {open && <ImagePreviewDialog key={src} src={src} title={title} intrinsicSize={intrinsicSize} onClose={() => setOpen(false)} />}
    </>
  );
}
