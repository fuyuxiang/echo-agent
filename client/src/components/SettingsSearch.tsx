import { useEffect, useId, useRef, useState } from "react";
import { Search } from "lucide-react";
import { searchSettings, type SettingsSearchEntry } from "@/lib/settings-search";

export function SettingsSearch({ onSelect }: {
  onSelect: (entry: SettingsSearchEntry, onSelected: () => void) => void;
}) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [selected, setSelected] = useState(0);
  const composing = useRef(false);
  const id = useId();
  const results = searchSettings(query);
  const expanded = open && Boolean(query.trim());
  useEffect(() => {
    if (expanded) document.getElementById(`${id}-${selected}`)?.scrollIntoView?.({ block: "nearest" });
  }, [expanded, id, selected]);
  const choose = (entry: SettingsSearchEntry) => onSelect(entry, () => {
    setQuery("");
    setOpen(false);
    setSelected(0);
  });
  return <div className="settings-search" onBlur={event => {
    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setOpen(false);
  }}>
    <Search size={15} aria-hidden="true" />
    <input className="form-control" type="search" role="combobox" aria-label="搜索设置" placeholder="搜索设置"
      aria-autocomplete="list" aria-expanded={expanded} aria-controls={expanded ? id : undefined}
      aria-activedescendant={expanded && results[selected] ? `${id}-${selected}` : undefined}
      autoComplete="off" value={query} onFocus={() => setOpen(true)}
      onCompositionStart={() => { composing.current = true; }}
      onCompositionEnd={() => { composing.current = false; }}
      onChange={event => { setQuery(event.target.value); setSelected(0); setOpen(true); }}
      onKeyDown={event => {
        // Some WebViews clear isComposing before the IME's final keydown.
        if (composing.current || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return;
        if (event.key === "Escape" && expanded) { event.preventDefault(); event.stopPropagation(); setOpen(false); }
        if ((event.key === "ArrowDown" || event.key === "ArrowUp") && results.length) {
          event.preventDefault(); setOpen(true);
          setSelected(value => (value + (event.key === "ArrowDown" ? 1 : -1) + results.length) % results.length);
        }
        if (event.key === "Enter" && expanded && results[selected]) { event.preventDefault(); choose(results[selected]); }
      }} />
    {expanded && <div className="settings-search__results" id={id} role="listbox" aria-label="设置搜索结果">
      {results.length ? results.map((entry, index) => <button type="button" role="option"
        id={`${id}-${index}`} key={`${entry.section}:${entry.title}`} aria-selected={selected === index} tabIndex={-1}
        onMouseDown={event => event.preventDefault()}
        onFocus={() => setSelected(index)} onPointerMove={() => setSelected(index)} onClick={() => choose(entry)}>
        <span>{entry.title}</span><small>{entry.category}</small>
      </button>) : <p role="status">没有找到相关设置，试试“通知”“备份”或“模型”。</p>}
    </div>}
  </div>;
}
