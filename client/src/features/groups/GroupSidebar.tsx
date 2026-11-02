import { useState } from "react";
import { Pin } from "lucide-react";
import { ChevronDownIcon } from "@/foundation/components/Icon/icons";
import { useGroupStore } from "./group-store";
import { groupPublicSeq } from "./group-presentation";
import { groupStatus } from "./group-client";
import { GroupActionButton, openGroupMenu } from "./GroupActions";

export function GroupSidebar({ onOpen, active = false }: { onOpen: () => void; active?: boolean }) {
  const groups = useGroupStore(state => state.groups);
  const currentId = useGroupStore(state => state.currentId);
  const read = useGroupStore(state => state.readRevisions);
  const [collapsed, setCollapsed] = useState(false);
  const visible = [...groups].filter(group => !group.archived).sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.updatedAt - a.updatedAt);
  if (!visible.length) return null;
  return <section className={`sidebar__list-section sidebar__list-section--chats group-sidebar${!collapsed && visible.length > 3 ? " sidebar__list-section--long" : ""}`} aria-label="专家团">
    <div className="sidebar__section-head">
      <button type="button" className="sidebar__section-label" aria-expanded={!collapsed} aria-controls="sidebar-group-list" onClick={() => setCollapsed(value => !value)}>
        <span>专家团 ({visible.length})</span>
        <ChevronDownIcon size="sm" className={`sidebar__chevron${collapsed ? " sidebar__chevron--collapsed" : ""}`} />
      </button>
      <button type="button" className="sidebar__section-action group-sidebar__all" aria-label="查看全部专家团" title="查看全部专家团" onClick={() => { void useGroupStore.getState().select(null); onOpen(); }}>全部</button>
    </div>
    {!collapsed && <div id="sidebar-group-list" className="sidebar__group group-sidebar__list" role="region" aria-label="最近专家团" tabIndex={0}>
    {visible.slice(0, 12).map(group => <div key={group.id} className={`group-sidebar__row${active && currentId === group.id ? " is-selected" : ""}`} onContextMenu={event => openGroupMenu(group.id, event)}>
      <button type="button" className="group-sidebar__item" title={group.name} onClick={() => { void useGroupStore.getState().select(group.id); onOpen(); }}>
        <span>{group.pinned && <Pin size={11} />}{group.name}{groupPublicSeq(group) > (read[group.id] ?? 0) && <i className="group-unread" aria-label="有新动态" />}</span><small>{groupStatus(group)}</small>
      </button>
      <GroupActionButton group={group} />
    </div>)}
    </div>}
  </section>;
}
