import { lazy, Suspense, useRef, useState } from "react";
import { X } from "lucide-react";
import { attachmentBasename, isImageAttachment } from "@/lib/user-message";
import { attachmentSizeLabel } from "@/lib/attachment-preview";
import { AttachmentVisual } from "./AttachmentVisual";

const AttachmentPreview = lazy(() => import("./AttachmentPreview").then(module => ({ default: module.AttachmentPreview })));

/** One attachment interaction for drafts, history, and edit/resend. Full file
 * reads happen only inside the preview; adding a file never steals focus. */
export function AttachmentList({ paths, cwd, variant = "composer", sizes, onRemove }: {
  paths: string[]; cwd?: string; variant?: "composer" | "message";
  sizes?: ReadonlyMap<string, number>; onRemove?: (path: string) => void;
}) {
  const [selected, setSelected] = useState<string | null>(null);
  const trigger = useRef<HTMLElement | null>(null);
  const draft = variant === "composer";
  // A removed/replaced file must not revive its preview if it is added later.
  if (selected && !paths.includes(selected)) setSelected(null);
  if (!paths.length) return null;
  return <>
    {draft && <div className="composer-attachments__hint">{paths.length} 个附件 · 点击预览{paths.length > 4 ? " · 横向滚动查看全部" : ""}</div>}
    <div className={draft ? "composer-attachments" : "msg__attachments"} role="list" aria-label={draft ? "待发送附件" : "附件"}>
      {paths.map(path => {
        const name = attachmentBasename(path);
        const image = isImageAttachment(path);
        const extension = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1).toUpperCase() : "FILE";
        const size = attachmentSizeLabel(sizes?.get(path));
        return <div key={path} role="listitem" className={draft ? `composer-attachments__item${image ? " composer-attachments__item--image" : ""}` : "msg__attachment-item"}>
          <button type="button" className={draft ? `composer-attachments__preview${image ? " composer-attachments__preview--image" : ""}` : `msg__attachment${image ? " msg__attachment--image" : ""}`}
            title={`预览 ${name}`} aria-label={`${draft ? "预览" : "打开"}附件 ${name}`} aria-haspopup="dialog"
            onClick={event => { event.stopPropagation(); trigger.current = event.currentTarget; setSelected(path); }}>
            <AttachmentVisual path={path} showStatus={draft} />
            <span className={draft ? "composer-attachments__copy" : "msg__attachment-copy"}>
              <span className={draft ? "composer-attachments__name" : "msg__attachment-name"}>{name}</span>
              <span className={draft ? "composer-attachments__type" : "msg__attachment-type"}>{extension} {image ? "图片" : "文件"}{size ? ` · ${size}` : ""}</span>
            </span>
          </button>
          {onRemove && <button type="button" className="composer-attachments__remove" aria-label="移除附件" aria-description={name} title={`移除 ${name}`}
            onClick={event => {
              event.stopPropagation();
              const item = event.currentTarget.parentElement;
              const neighbor = item?.nextElementSibling ?? item?.previousElementSibling;
              neighbor?.querySelector<HTMLButtonElement>("button")?.focus();
              onRemove(path);
            }}><X size={13} /></button>}
        </div>;
      })}
    </div>
    {selected && paths.includes(selected) && <Suspense fallback={null}>
      <AttachmentPreview paths={paths} path={selected} cwd={cwd} onSelect={setSelected} onClose={() => setSelected(null)} returnFocus={trigger.current} />
    </Suspense>}
  </>;
}
