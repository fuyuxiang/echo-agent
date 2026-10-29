/** Current-conversation tools; session management lives in the sidebar menu. */
import { useRef, useState, useLayoutEffect } from "react";
import { Search, History, GitFork, Download, Square } from "lucide-react";
import { MoreDotsIcon } from "@/foundation/components/Icon/icons";
import { useSessionStore } from "@/stores/session-store";
import { ActionMenu } from "./ActionMenu";
import { ShareMenu } from "./ShareMenu";

export interface TopbarActionsProps {
  sessionId: string;
  title: string;
  onToast?: (msg: string) => void;
  onRewind?: () => void;
  onFork?: () => void;
  onFind?: () => void;
  findOpen?: boolean;
  historyDisabled?: boolean;
  onStop?: () => void;
  stopping?: boolean;
  stopDisabled?: boolean;
}

export function TopbarActions({ sessionId, title, onToast,
  onRewind, onFork, historyDisabled, onFind, findOpen, onStop, stopping, stopDisabled,
}: TopbarActionsProps) {
  const [shareOpen, setShareOpen] = useState(false);
  const returnFocus = useRef<HTMLButtonElement | null>(null);
  useLayoutEffect(() => { setShareOpen(false); }, [sessionId]);
  const messages = useSessionStore(s => s.messages);
  const hasContent = messages.some(m => m.parts.length > 0 || m.attachments?.length);
  if (!onFind && !onRewind && !onFork && !hasContent && !onStop) return null;

  return <>
    <ActionMenu label="任务操作" menuLabel="当前会话操作" resetKey={sessionId}
      trigger={<MoreDotsIcon size="md" />}>
      {({ close, triggerRef }) => {
        const invoke = (action: () => void, restore = true) => {
          close();
          if (restore) triggerRef.current?.focus();
          action();
        };
        return <>
          {onStop && <button type="button" role="menuitem" className="topbar-actions__item"
            disabled={stopping || stopDisabled} onClick={() => invoke(onStop)}
            title="停止当前执行，保留草稿、附件和已有结果">
            <Square size={15} />{stopping ? "正在停止…" : "停止当前执行"}
          </button>}
          {onStop && (onFind || onRewind || onFork || hasContent) && <div role="separator" className="action-menu__separator" />}
          {onFind && <button type="button" role="menuitem" className="topbar-actions__item" aria-label="查找当前会话" aria-expanded={findOpen} onClick={() => invoke(onFind)}><Search size={15} />查找当前会话<span className="action-menu__meta">⌘ / Ctrl F</span></button>}
          {onRewind && <button type="button" role="menuitem" className="topbar-actions__item" disabled={historyDisabled} onClick={() => invoke(onRewind)} title="将文件或对话恢复到历史节点"><History size={15} />回溯…</button>}
          {onFork && <button type="button" role="menuitem" className="topbar-actions__item" disabled={historyDisabled} onClick={() => invoke(onFork)} title="复制当前完整会话并创建分支"><GitFork size={15} />分叉…</button>}
          {hasContent && <>
            {(onFind || onRewind || onFork) && <div role="separator" className="action-menu__separator" />}
            <button type="button" role="menuitem" className="topbar-actions__item" onClick={() => {
              returnFocus.current = triggerRef.current;
              invoke(() => setShareOpen(true), false);
            }}><Download size={15} />导出整个会话…</button>
          </>}
        </>;
      }}
    </ActionMenu>
    <ShareMenu key={sessionId} messages={messages} title={title} onDone={onToast}
      controlledOpen={shareOpen} onClose={() => setShareOpen(false)} returnFocus={returnFocus.current} saveToFile />
  </>;
}
