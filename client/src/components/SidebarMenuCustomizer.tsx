import { useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { GripVertical, X } from "lucide-react";
import { EchoNewTaskIcon } from "@/foundation/components/Icon/icons";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useSidebarMenuDrag } from "@/lib/use-sidebar-menu-drag";
import {
  SIDEBAR_MENU_ITEMS, moveSidebarMenuItem, normalizeSidebarMenuLayout,
  type SidebarMenuGroup, type SidebarMenuId, type SidebarMenuLayout,
} from "@/lib/sidebar-menu";
import { Button, IconButton } from "./Button";

const GROUP_LABELS = { primary: "一级菜单", more: "更多" };

export function SidebarMenuCustomizer({ layout, returnFocus, onSave, onClose }: {
  layout: SidebarMenuLayout;
  returnFocus: HTMLElement | null;
  onSave: (layout: SidebarMenuLayout) => void;
  onClose: () => void;
}) {
  const titleId = useId();
  const descriptionId = useId();
  const keyboardHelpId = useId();
  const [draft, setDraft] = useState(() => normalizeSidebarMenuLayout(layout));
  const [announcement, setAnnouncement] = useState("");
  const [error, setError] = useState<string | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const focusMovedItem = useRef<SidebarMenuId | null>(null);
  const dialogRef = useModalFocus<HTMLDivElement>(true, onClose, returnFocus);

  useLayoutEffect(() => {
    if (!focusMovedItem.current) return;
    dialogRef.current?.querySelector<HTMLButtonElement>(`[data-menu-id="${focusMovedItem.current}"] [data-menu-handle]`)?.focus();
    focusMovedItem.current = null;
  }, [draft, dialogRef]);

  const move = (id: SidebarMenuId, group: SidebarMenuGroup, index?: number, restoreFocus = false) => {
    const next = moveSidebarMenuItem(draft, id, group, index);
    if (restoreFocus) focusMovedItem.current = id;
    setDraft(next);
    setError(null);
    setAnnouncement(`已将${SIDEBAR_MENU_ITEMS[id].label}移至${GROUP_LABELS[group]}第 ${next[group].indexOf(id) + 1} 位`);
  };

  const { preview, dropTarget, begin, lostCapture } = useSidebarMenuDrag(bodyRef,
    (id, group, index) => move(id, group, index, true));
  const PreviewIcon = preview ? SIDEBAR_MENU_ITEMS[preview.id].icon : null;

  const save = () => {
    try {
      onSave(draft);
      onClose();
    } catch {
      setError("菜单设置保存失败，请重试。当前调整已保留在此窗口中。");
    }
  };

  return createPortal(
    <div className="app-dialog-overlay" onClick={onClose}>
      <div ref={dialogRef} className="app-dialog sidebar-menu-customizer" role="dialog" aria-modal="true"
        aria-labelledby={titleId} aria-describedby={descriptionId} tabIndex={-1}
        onClick={event => event.stopPropagation()}>
        <header className="app-dialog__header sidebar-menu-customizer__header">
          <h2 id={titleId} className="app-dialog__title">自定义菜单栏</h2>
          <IconButton label="关闭自定义菜单栏" className="app-dialog__close" onClick={onClose}><X size={18} /></IconButton>
        </header>
        <div ref={bodyRef} className="app-dialog__body sidebar-menu-customizer__body">
          <p id={descriptionId} className="app-dialog__description">拖动右侧手柄，调整顺序或在一级菜单与“更多”之间移动入口。</p>
          <span id={keyboardHelpId} className="sidebar-menu-customizer__status">上下方向键调整顺序，向左移至一级菜单，向右移至更多。</span>
          {(["primary", "more"] as const).map(group => (
            <section key={group} data-menu-group={group} className="sidebar-menu-customizer__section" aria-label={GROUP_LABELS[group]}>
              <h3>{GROUP_LABELS[group]}</h3>
              {group === "primary" && <div className="sidebar-menu-customizer__row sidebar-menu-customizer__row--fixed">
                <EchoNewTaskIcon size="md" /><span className="sidebar-menu-customizer__label">新建任务</span>
                <span className="sidebar-menu-customizer__fixed-label">固定</span>
              </div>}
              <ul className="sidebar-menu-customizer__list" aria-label={`${GROUP_LABELS[group]}入口`}>
                {draft[group].map((id, index) => {
                  const { label, icon: Icon } = SIDEBAR_MENU_ITEMS[id];
                  return <li key={id} data-menu-id={id}
                    className={`sidebar-menu-customizer__row${preview?.id === id ? " sidebar-menu-customizer__row--dragging" : ""}${dropTarget?.group === group && dropTarget.index === index ? " sidebar-menu-customizer__row--drop-before" : ""}`}>
                    <Icon size="md" />
                    <span className="sidebar-menu-customizer__label">{label}</span>
                    <IconButton label={`拖动${label}`} title="拖动调整位置（也可使用方向键）"
                      className="sidebar-menu-customizer__grip" data-menu-handle draggable={false} aria-describedby={keyboardHelpId}
                      onDragStart={event => event.preventDefault()}
                      onPointerDown={event => begin(event, id)}
                      onLostPointerCapture={event => lostCapture(event.pointerId)}
                      onKeyDown={event => {
                        if (event.altKey || event.ctrlKey || event.metaKey || event.nativeEvent.isComposing) return;
                        if (!["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(event.key)) return;
                        event.preventDefault();
                        if (event.key === "ArrowUp" && index > 0) move(id, group, index - 1, true);
                        if (event.key === "ArrowDown" && index < draft[group].length - 1) move(id, group, index + 2, true);
                        if (event.key === "ArrowLeft" && group === "more") move(id, "primary", undefined, true);
                        if (event.key === "ArrowRight" && group === "primary") move(id, "more", undefined, true);
                      }}><GripVertical size={18} /></IconButton>
                  </li>;
                })}
                {draft[group].length === 0 && <li className="sidebar-menu-customizer__empty">将入口拖到这里</li>}
                {dropTarget?.group === group && dropTarget.index === draft[group].length && <li className="sidebar-menu-customizer__drop-end" aria-hidden="true" />}
              </ul>
            </section>
          ))}
          <span className="sidebar-menu-customizer__status" role="status">{announcement}</span>
          {error && <div className="app-dialog__error" role="alert">{error}</div>}
        </div>
        <footer className="app-dialog__actions sidebar-menu-customizer__footer">
          <Button variant="ghost" onClick={() => { setDraft(normalizeSidebarMenuLayout(null)); setError(null); setAnnouncement("已恢复默认布局，保存后生效"); }}>恢复默认</Button>
          <Button onClick={onClose}>取消</Button>
          <Button variant="primary" onClick={save}>保存</Button>
        </footer>
      </div>
      {preview && PreviewIcon && <div className="sidebar-menu-customizer__row sidebar-menu-customizer__preview" aria-hidden="true"
        style={{ left: preview.left, top: preview.top, width: preview.width, height: preview.height }}>
        <PreviewIcon size="md" /><span className="sidebar-menu-customizer__label">{SIDEBAR_MENU_ITEMS[preview.id].label}</span><GripVertical size={18} />
      </div>}
    </div>, document.body,
  );
}
