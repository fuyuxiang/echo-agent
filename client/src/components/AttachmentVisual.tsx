import { useEffect, useRef, useState } from "react";
import {
  FileCode,
  FileSpreadsheet,
  FileText,
  Image as ImageIcon,
} from "lucide-react";
import { attachmentThumbnail } from "@/lib/agent-client";
import { AttachmentKind, classifyAttachment } from "@/lib/user-message";

/** 按附件分类挑选最贴近语义的 lucide-react 图标。
 *
 * 设计原则:先按文件类型直观对应(Code → FileCode, Document → FileText),
 * 再用 kind 兜底。避免「任何非图片都是 FileText」的单调体验。 */
function iconForKind(kind: AttachmentKind) {
  switch (kind) {
    case AttachmentKind.Image:
      return ImageIcon;
    case AttachmentKind.Code:
      return FileCode;
    case AttachmentKind.Data:
      return FileSpreadsheet;
    case AttachmentKind.Document:
      return FileText;
    case AttachmentKind.Text:
      return FileText;
    default:
      return FileText;
  }
}

/** 给 chip / 列表头用的简短标签。 */
function labelForKind(kind: AttachmentKind): string {
  switch (kind) {
    case AttachmentKind.Image:
      return "image";
    case AttachmentKind.Document:
      return "document";
    case AttachmentKind.Code:
      return "code";
    case AttachmentKind.Text:
      return "text";
    case AttachmentKind.Data:
      return "data";
    case AttachmentKind.Unsupported:
      return "file";
  }
}

const MAX_MEMORY_THUMBNAILS = 128;
/** Shared across message remounts/session switches so history replay does not
 * repeat native decoding. Concurrent rows for one file share the same invoke. */
const cache = new Map<string, string>();
const inflight = new Map<string, Promise<string>>();

function remember(path: string, url: string): void {
  cache.delete(path);
  cache.set(path, url);
  while (cache.size > MAX_MEMORY_THUMBNAILS) {
    const oldest = cache.keys().next().value as string | undefined;
    if (!oldest) break;
    cache.delete(oldest);
  }
}

function load(path: string): Promise<string> {
  const cached = cache.get(path);
  if (cached) {
    // Refresh insertion order to approximate a small LRU.
    remember(path, cached);
    return Promise.resolve(cached);
  }
  let pending = inflight.get(path);
  if (!pending) {
    pending = attachmentThumbnail(path)
      .then((base64) => {
        const url = base64 ? `data:image/jpeg;base64,${base64}` : "";
        if (url) remember(path, url);
        return url;
      })
      .catch(() => "")
      .finally(() => inflight.delete(path));
    inflight.set(path, pending);
  }
  return pending;
}

/** Lazy thumbnail for supported chat images, with a kind-aware icon fallback
 * for missing or unreadable local files. Non-image attachments now show
 * an icon that matches their file category (Code → FileCode, Data → FileSpreadsheet,
 * Document → FileText) so users can scan a multi-file list at a glance. */
export function AttachmentVisual({ path, showStatus = false }: { path: string; showStatus?: boolean }) {
  const kind = classifyAttachment(path);
  const image = kind === AttachmentKind.Image;
  const Icon = iconForKind(kind);
  const [src, setSrc] = useState(() => image ? cache.get(path) : undefined);
  const [failed, setFailed] = useState(false);
  const wrapperRef = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    setFailed(false);
    if (!image) {
      setSrc(undefined);
      return;
    }
    const cached = cache.get(path);
    if (cached) {
      remember(path, cached);
      setSrc(cached);
      return;
    }
    setSrc(undefined);
    let active = true;
    let started = false;
    const start = () => {
      if (started) return;
      started = true;
      void load(path).then((url) => {
        if (active) { if (url) setSrc(url); else setFailed(true); }
      });
    };
    const element = wrapperRef.current;
    if (!element || typeof IntersectionObserver === "undefined") {
      start();
      return () => {
        active = false;
      };
    }
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        start();
        observer.disconnect();
      }
    }, { rootMargin: "240px 0px" });
    observer.observe(element);
    return () => {
      active = false;
      observer.disconnect();
    };
  }, [image, path]);

  if (!image) {
    return (
      <span
        className={`msg__attachment-icon msg__attachment-icon--${labelForKind(kind)}`}
        aria-hidden="true"
      >
        <Icon size={20} strokeWidth={1.8} />
      </span>
    );
  }
  return (
    <span
      ref={wrapperRef}
      className="msg__attachment-icon msg__attachment-icon--image"
      data-thumbnail-state={src ? "ready" : failed ? "error" : "loading"}
      aria-hidden="true"
    >
      {src ? (
        <img
          className="msg__attachment-thumbnail"
          src={src}
          alt=""
          draggable={false}
          onError={() => {
            cache.delete(path);
            setSrc(undefined);
            setFailed(true);
          }}
        />
      ) : (
        <><ImageIcon size={22} strokeWidth={1.7} />{showStatus && <span className="composer-attachments__thumbnail-status">{failed ? "点击查看原图" : "加载中…"}</span>}</>
      )}
    </span>
  );
}
