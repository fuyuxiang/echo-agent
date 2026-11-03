import { useLayoutEffect, useRef, type ClipboardEventHandler } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";
import { useModalFocus } from "@/lib/use-modal-focus";

/** Edits the same durable draft; dismissing the larger editor never discards
 * changes or sends a message. Enter keeps its normal multiline semantics. */
export function ExpandedDraftEditor({ value, onChange, onPaste, onClose, selection, returnFocus, disabled }: {
  value: string; onChange: (value: string) => void; onPaste: ClipboardEventHandler<HTMLTextAreaElement>;
  onClose: (selection: [number, number]) => void; selection: [number, number];
  returnFocus: HTMLTextAreaElement | null; disabled?: boolean;
}) {
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const close = () => onClose(inputRef.current ? [inputRef.current.selectionStart, inputRef.current.selectionEnd] : selection);
  const dialogRef = useModalFocus<HTMLDivElement>(true, close, returnFocus);
  useLayoutEffect(() => { inputRef.current?.setSelectionRange(...selection); }, []);
  return createPortal(<div className="attachment-preview-overlay" onClick={event => { event.stopPropagation(); if (event.target === event.currentTarget) close(); }} onKeyDown={event => event.stopPropagation()}>
    <div ref={dialogRef} className="attachment-preview expanded-draft-editor" role="dialog" aria-modal="true" aria-label="编辑完整输入内容" tabIndex={-1}>
      <header className="attachment-preview__header"><span className="attachment-preview__title" style={{ flex: 1 }}>编辑完整输入内容</span><button type="button" aria-label="关闭展开编辑" onClick={close}><X size={20} /></button></header>
      <textarea ref={inputRef} className="expanded-draft-editor__input" aria-label="完整输入内容" data-modal-initial-focus value={value} disabled={disabled}
        onChange={event => onChange(event.currentTarget.value)} onPaste={onPaste} />
      <footer className="attachment-preview__footer"><span>修改会保留在当前草稿中</span><button type="button" onClick={close}>完成编辑</button></footer>
    </div>
  </div>, document.body);
}
