import { useCallback, useEffect, useRef, useState } from "react";
import { EditToolIcon } from "@/foundation/components/Icon/icons";

/**
 * Editable conversation title for the main topbar — mirrors EchoAgent's
 * `echo-agent-topbar` title interaction:
 *   - default: plain title text; a pencil button fades in on hover;
 *   - click pencil → the title swaps to an <input> with the text selected;
 *   - Enter / blur commits (empty or unchanged = no-op), Esc cancels.
 *
 * `onRename` should persist the title (EchoAgent's echo.agent/session/rename) and update
 * the sessions store; on rejection the draft reverts to the current title.
 */
export function TopbarTitle({
  title,
  onRename,
}: {
  title: string;
  onRename: (newTitle: string) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState(title);
  const [saving, setSaving] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const editingRef = useRef(false);
  const submittingRef = useRef(false);

  // Track external title updates (e.g. EchoAgent's LLM-generated summary arriving
  // via agent://summary) while we're not editing.
  useEffect(() => {
    if (!editing) setValue(title);
  }, [title, editing]);

  // Focus + select-all on entering edit mode.
  useEffect(() => {
    if (editing && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
    }
  }, [editing]);

  const startEdit = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!title || submittingRef.current) return;
      setValue(title);
      editingRef.current = true;
      setEditing(true);
    },
    [title],
  );

  const commit = useCallback(async () => {
    if (!editingRef.current || submittingRef.current) return;
    editingRef.current = false;
    setEditing(false);
    const trimmed = value.trim();
    if (trimmed && trimmed !== title) {
      submittingRef.current = true;
      setSaving(true);
      try {
        await onRename(trimmed);
      } catch {
        setValue(title); // revert the draft; the store keeps the old title
      } finally {
        submittingRef.current = false;
        setSaving(false);
      }
    }
  }, [value, title, onRename]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return;
      if (e.key === "Enter") {
        e.preventDefault();
        void commit();
      } else if (e.key === "Escape") {
        editingRef.current = false;
        setEditing(false);
        setValue(title);
      }
    },
    [commit, title],
  );

  if (editing) {
    return (
      <input
        ref={inputRef}
        type="text"
        className="main-topbar__title-input"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onBlur={() => void commit()}
        onKeyDown={handleKeyDown}
        onClick={(e) => e.stopPropagation()}
      />
    );
  }

  return (
    <span className="main-topbar__title-area">
      <span className="main-topbar__title" title={title || "未命名会话"}>
        {title || "未命名会话"}
      </span>
      {title && (
        <button
          className="main-topbar__title-edit"
          type="button"
          aria-label="编辑标题"
          data-tip="编辑标题"
          disabled={saving}
          onClick={startEdit}
        >
          <EditToolIcon size="sm" />
        </button>
      )}
    </span>
  );
}
