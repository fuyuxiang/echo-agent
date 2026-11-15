import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { ChevronLeft, ChevronRight, Copy, FileSpreadsheet, FileText, Maximize2, Minimize2, Minus, PanelLeft, Plus, Presentation, RotateCw, Search, Scan, X } from 'lucide-react';
import { copyShareText } from '@/lib/share';
import { officeKind, OFFICE_LABELS, searchExcerpt } from '@/lib/office-preview';
import type { OfficePreview } from '@/lib/office-preview';
import { loadOfficePreview } from '@/lib/office-preview-loaders';
import { useModalFocus } from '@/lib/use-modal-focus';
import type { FilePreviewCopyHandler } from '../FilePreview';
import OfficePage from './OfficePage';
import SpreadsheetCanvas from './SpreadsheetCanvas';

export interface OfficeWorkbenchProps {
  filename: string; content: string; path?: string; active?: boolean;
  fileActions?: ReactNode; controls?: ReactNode; notices?: ReactNode; children?: ReactNode;
  onCopyText?: FilePreviewCopyHandler; onReload?: () => void; sourceRevision?: string | number;
  loadContent?: () => Promise<string>; fallback?: ReactNode; onSelectionText?: (text: string) => void;
}
interface Match { page: number; excerpt: string; cell?: string; }
const SEARCH_RESULT_LIMIT = 500;

// Move one portal host instead of remounting the canvas/editor when entering
// fullscreen. Selection, scroll positions and in-progress edits stay intact.
function WorkbenchSurface({ expanded, children }: { expanded: boolean; children: ReactNode }) {
  const inline = useRef<HTMLDivElement>(null);
  const [host] = useState(() => { const node = document.createElement('div'); node.style.display = 'contents'; return node; });
  useLayoutEffect(() => {
    const positions = Array.from(host.querySelectorAll<HTMLElement>('.office-workbench__viewport,.office-sheet__viewport,.office-workbench__navigation-scroll,.office-workbench__custom'))
      .map(node => ({ node, left: node.scrollLeft, top: node.scrollTop }));
    (expanded ? document.body : inline.current)?.append(host);
    for (const { node, left, top } of positions) { node.scrollLeft = left; node.scrollTop = top; }
  }, [expanded, host]);
  useLayoutEffect(() => () => host.remove(), [host]);
  return <><div className="office-workbench-mount" ref={inline} />{createPortal(children, host)}</>;
}

function PageThumbnail({ model, index, selected, onSelect }: { model: OfficePreview; index: number; selected: boolean; onSelect: () => void }) {
  const ref = useRef<HTMLButtonElement>(null); const [visible, setVisible] = useState(false);
  useEffect(() => {
    const node = ref.current; if (!node) return;
    if (typeof IntersectionObserver === "undefined") { setVisible(true); return; }
    const observer = new IntersectionObserver(entries => { if (entries.some(e => e.isIntersecting)) { setVisible(true); observer.disconnect(); } }, { rootMargin: '180px' });
    observer.observe(node); return () => observer?.disconnect();
  }, [model, index]);
  useEffect(() => { if (selected) ref.current?.scrollIntoView({ block: 'nearest' }); }, [selected]);
  return <button type="button" ref={ref} className={`office-thumbnail${selected ? ' office-thumbnail--selected' : ''}`} aria-label={`查看第 ${index + 1} 页：${model.pages[index].label}`} aria-current={selected ? 'page' : undefined} onClick={onSelect}>
    <span className="office-thumbnail__number">{index + 1}</span>
    <span className="office-thumbnail__paper">{visible ? <OfficePage model={model} index={index} width={116} thumbnail /> : <span style={{ display: 'block', width: 116, height: 116 * model.pages[index].height / model.pages[index].width }} />}</span>
    <span className="office-thumbnail__title">{model.pages[index].label}</span>
  </button>;
}

export default function OfficeWorkbench({ filename, content, path, active = true, fileActions, controls, notices, children, onCopyText, onReload, sourceRevision, loadContent, fallback, onSelectionText }: OfficeWorkbenchProps) {
  const kind = officeKind(filename) ?? 'docx';
  const [refresh, setRefresh] = useState(0);
  const customView = children != null;
  const identity = useMemo(() => ({}), [filename, content, sourceRevision, refresh, customView]);
  const copyIdentity = useMemo(() => ({}), [identity, active]);
  const currentCopyIdentity = useRef(copyIdentity); currentCopyIdentity.current = copyIdentity;
  const loader = useRef(loadContent); loader.current = loadContent;
  const [state, setState] = useState<{ identity: object; model?: OfficePreview; error?: string }>({ identity });
  const [page, setPage] = useState(0), [manualZoom, setManualZoom] = useState<number | null>(null);
  const [navigation, setNavigation] = useState(true), [expanded, setExpanded] = useState(false), [searchOpen, setSearchOpen] = useState(false);
  const [query, setQuery] = useState(''), [matches, setMatches] = useState<Match[]>([]), [searching, setSearching] = useState(false), [searchError, setSearchError] = useState('');
  const [searchTruncated, setSearchTruncated] = useState(false);
  const [sheetTarget, setSheetTarget] = useState<{ page: number; address: string; request: number }>();
  const [copyStatus, setCopyStatus] = useState<'idle' | 'copying' | 'copied' | 'failed'>('idle');
  const [dimensions, setDimensions] = useState({ width: 900, height: 620, shell: 900 });
  const expandButton = useRef<HTMLButtonElement>(null);
  const root = useModalFocus<HTMLDivElement>(expanded && active, () => setExpanded(false), undefined, () => expandButton.current);
  const viewport = useRef<HTMLDivElement>(null), searchInput = useRef<HTMLInputElement>(null);
  const ownerActive = useRef(active); ownerActive.current = active;
  useEffect(() => { ownerActive.current = active; return () => { ownerActive.current = false; }; }, [active]);
  useEffect(() => { setCopyStatus('idle'); }, [copyIdentity]);
  const model = state.identity === identity ? state.model : undefined;
  const error = state.identity === identity ? state.error : undefined;
  useEffect(() => {
    if (customView) return;
    const controller = new AbortController(); let document: OfficePreview | undefined;
    setState({ identity }); setCopyStatus('idle');
    void (async () => {
      const bytes = await loader.current?.() ?? content;
      const loaded = await loadOfficePreview(filename, bytes, controller.signal);
      if (controller.signal.aborted) { loaded.dispose(); return; }
      document = loaded; setState({ identity, model: loaded }); setPage(p => Math.min(p, loaded.pages.length - 1));
    })().catch(error => { if (!controller.signal.aborted) setState({ identity, error: String(error).replace(/^Error:\s*/, '') }); });
    return () => { controller.abort(); document?.dispose(); };
  }, [identity]);
  useEffect(() => { setPage(0); setManualZoom(null); setQuery(''); setMatches([]); setSheetTarget(undefined); }, [filename, content]);
  useEffect(() => { setSheetTarget(undefined); }, [model]);
  useLayoutEffect(() => {
    const node = viewport.current, shell = root.current;
    if (!node || !shell) return;
    const measure = () => setDimensions({ width: node.clientWidth, height: node.clientHeight, shell: shell.clientWidth });
    measure(); const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure); observer?.observe(node); observer?.observe(shell);
    return () => observer?.disconnect();
  }, [expanded, customView, Boolean(model)]);
  useEffect(() => { if (dimensions.shell < 600) setNavigation(false); }, [dimensions.shell < 600]);
  useEffect(() => { if (!active) setExpanded(false); }, [active]);
  useEffect(() => { if (searchOpen) searchInput.current?.focus(); }, [searchOpen, expanded]);
  useEffect(() => {
    const controller = new AbortController(); const text = query.trim();
    setMatches([]); setSearchError(''); setSearchTruncated(false);
    if (!model || !text) { setSearching(false); return () => controller.abort(); }
    setSearching(true);
    const timer = setTimeout(() => { void (async () => {
      const found: Match[] = []; let truncated = false, scanned = 0;
      pages: for (let index = 0; index < model.pages.length; index++) {
        if (controller.signal.aborted) return;
        if (model.kind === 'sheet' && model.sheets) {
          const sheet = model.sheets[index];
          for (const cell of sheet.cells.values()) {
            if (controller.signal.aborted) return;
            const excerpt = searchExcerpt(`${cell.address} ${cell.text}${cell.formula ? ` =${cell.formula}` : ''}`, text);
            if (excerpt) found.push({ page: index, cell: cell.address, excerpt });
            if (found.length > SEARCH_RESULT_LIMIT) { found.pop(); truncated = true; break pages; }
            if (++scanned % 1000 === 0) await new Promise(resolve => setTimeout(resolve, 0));
          }
        } else {
          const excerpt = searchExcerpt(await model.pageText(index), text);
          if (excerpt) found.push({ page: index, excerpt });
        }
      }
      if (!controller.signal.aborted) { setMatches(found); setSearchTruncated(truncated); setSearching(false); }
    })().catch(() => { if (!controller.signal.aborted) { setSearchError('暂时无法搜索此文档'); setSearching(false); } }); }, 180);
    return () => { controller.abort(); clearTimeout(timer); };
  }, [model, query]);
  useEffect(() => { if (copyStatus !== 'copied' && copyStatus !== 'failed') return; const timer = setTimeout(() => setCopyStatus('idle'), 2400); return () => clearTimeout(timer); }, [copyStatus]);
  const metadata = model?.pages[page];
  const fit = kind === 'sheet' ? 1 : metadata ? Math.min(1, Math.max(0.1, (dimensions.width - 48) / metadata.width), kind === 'pptx' ? Math.max(0.1, (dimensions.height - 48) / metadata.height) : 10) : 1;
  const zoom = manualZoom ?? fit;
  const total = model?.pages.length ?? 0;
  const changePage = (value: number) => { if (total) setPage(Math.max(0, Math.min(total - 1, value))); };
  const changeZoom = (value: number) => setManualZoom(Math.min(2.5, Math.max(0.1, value)));
  const selectMatch = (match: Match) => {
    changePage(match.page);
    if (match.cell) setSheetTarget(previous => ({ page: match.page, address: match.cell!, request: (previous?.request ?? 0) + 1 }));
    else viewport.current?.scrollTo(0, 0);
    if (dimensions.shell < 600) setNavigation(false);
  };
  const copy = async () => {
    if (!model || !active || copyStatus === 'copying') return;
    const owner = copyIdentity; setCopyStatus('copying');
    try {
      const texts = [];
      for (let index = 0; index < model.pages.length; index++) { texts.push(await model.pageText(index)); if (currentCopyIdentity.current !== owner || !ownerActive.current) return; }
      const result = await (onCopyText ?? copyShareText)(texts.join('\n\n'));
      if (currentCopyIdentity.current === owner && ownerActive.current) setCopyStatus(result === false ? 'failed' : 'copied');
    } catch { if (currentCopyIdentity.current === owner && ownerActive.current) setCopyStatus('failed'); }
  };
  const Icon = kind === 'pptx' ? Presentation : kind === 'sheet' ? FileSpreadsheet : FileText;
  const title = filename.split(/[\\/]/).pop() || filename;
  const showNavigation = navigation && !customView && (model || searchOpen);
  const workbench = <div ref={root} tabIndex={-1} className={`office-workbench office-workbench--${kind}${expanded ? ' office-workbench--expanded' : ''}`} aria-label={`${title} 文档工作台`} role={expanded ? 'dialog' : 'region'} aria-modal={expanded ? true : undefined}
    onKeyDown={event => {
      if (event.target instanceof HTMLElement && event.target.closest('input,textarea,select,[contenteditable="true"]')) return;
      if (event.key === 'PageDown' || event.key === 'PageUp') { event.preventDefault(); changePage(page + (event.key === 'PageDown' ? 1 : -1)); }
    }}>
    <header className="office-workbench__header">
      <span className="office-workbench__file-icon"><Icon size={19} aria-hidden /></span>
      <div className="office-workbench__file"><strong title={path || filename}>{title}</strong><span>{OFFICE_LABELS[kind]} · 本地文件</span></div>
      <div className="office-workbench__file-actions">{fileActions}
        <button ref={expandButton} type="button" aria-label={expanded ? '退出全屏预览' : '全屏预览'} title={expanded ? '退出全屏预览' : '全屏预览'} onClick={() => setExpanded(value => !value)}>{expanded ? <Minimize2 size={16} /> : <Maximize2 size={16} />}</button>
      </div>
    </header>
    <div className={`office-workbench__toolbar${controls && !customView ? ' office-workbench__toolbar--document' : ''}`} role="toolbar" aria-label="文档查看工具">
      {!customView && <><button type="button" aria-label={kind === 'sheet' ? '切换工作表导航' : '切换页面缩略图'} title={kind === 'sheet' ? '工作表导航' : '页面缩略图'} aria-pressed={navigation} onClick={() => setNavigation(v => !v)}><PanelLeft size={17} /></button><span className="office-workbench__separator" />
        <button type="button" aria-label={kind === 'sheet' ? '上一个工作表' : '上一页'} disabled={!total || page === 0} onClick={() => changePage(page - 1)}><ChevronLeft size={17} /></button>
        <span className="office-workbench__page-count" aria-live="polite">{total ? `${page + 1} / ${total}` : '—'}</span>
        <button type="button" aria-label={kind === 'sheet' ? '下一个工作表' : '下一页'} disabled={!total || page + 1 >= total} onClick={() => changePage(page + 1)}><ChevronRight size={17} /></button>
        <span className="office-workbench__separator" /><button type="button" aria-label="搜索文档" title="搜索文档" aria-pressed={searchOpen} onClick={() => { setSearchOpen(value => !value); setNavigation(true); }}><Search size={16} /></button>
        {!controls && <button type="button" aria-label="复制文档文本" title="复制文本" disabled={!model || copyStatus === 'copying'} onClick={() => void copy()}><Copy size={16} /></button>}</>}
      {controls && <div className="office-workbench__controls">{controls}</div>}
      <span className="office-workbench__toolbar-spacer" />
      <span className="office-workbench__copy-status" role="status">{copyStatus === 'copying' ? '正在复制…' : copyStatus === 'copied' ? '已复制文本' : copyStatus === 'failed' ? '复制失败，请重试' : ''}</span>
      <button type="button" aria-label="重新读取文档" title="重新读取文档" onClick={() => onReload ? onReload() : setRefresh(value => value + 1)}><RotateCw size={15} /></button>
    </div>
    {notices && <div className="office-workbench__notices">{notices}</div>}
    <div className="office-workbench__body">
      {showNavigation && <aside className="office-workbench__navigation" aria-label={kind === 'sheet' ? '工作表导航' : '文档页面导航'}>
        <div className="office-workbench__navigation-head"><strong>{query.trim() ? '搜索结果' : kind === 'sheet' ? '工作表' : kind === 'pptx' ? '幻灯片' : '页面'}</strong><span>{query.trim() ? `${matches.length}${searchTruncated ? '+' : ''}` : total}</span></div>
        {searchOpen && <div className="office-workbench__search"><Search size={14} /><input ref={searchInput} aria-label="搜索文档内容" placeholder="搜索文档内容" value={query} onChange={event => setQuery(event.target.value)} /><button type="button" aria-label="关闭文档搜索" onClick={() => { setSearchOpen(false); setQuery(''); }}><X size={13} /></button></div>}
        <div className="office-workbench__navigation-scroll">
          {query.trim() ? <>{searching ? <p role="status">正在搜索…</p> : searchError ? <p role="status">{searchError}</p> : matches.length ? <>{matches.map(match => <button className="office-search-result" type="button" key={`${match.page}:${match.cell ?? ''}`} aria-current={page === match.page && (!match.cell || sheetTarget?.address === match.cell && sheetTarget.page === match.page) ? 'page' : undefined} onClick={() => selectMatch(match)}><strong>{kind === 'sheet' ? `${model?.pages[match.page].label} · ${match.cell}` : `第 ${match.page + 1} 页`}</strong><span>{match.excerpt}</span></button>)}{searchTruncated && <p role="status">仅显示前 {SEARCH_RESULT_LIMIT} 条结果，请输入更具体的关键词。</p>}</> : <p role="status">没有找到匹配内容</p>}</>
            : model?.pages.map((item, index) => kind === 'sheet' ? <button type="button" key={index} className="office-sheet-tab-nav" aria-current={page === index ? 'page' : undefined} onClick={() => changePage(index)}><FileSpreadsheet size={15} /><span>{item.label}</span></button>
              : <PageThumbnail key={index} model={model} index={index} selected={page === index} onSelect={() => { changePage(index); if (dimensions.shell < 600) setNavigation(false); }} />)}
        </div>
      </aside>}
      <main className="office-workbench__viewport" ref={viewport} aria-label="文档画布">
        {customView ? <div className="office-workbench__custom">{children}</div> : error ? <div className="office-workbench__empty" role="alert"><FileText size={30} /><strong>暂时无法显示版式预览</strong><p>{error}</p><button type="button" onClick={() => onReload ? onReload() : setRefresh(v => v + 1)}>重试预览</button>{fallback && <details><summary>查看可读取的内容</summary>{fallback}</details>}</div>
          : !model ? <div className="office-workbench__empty" role="status"><span className="office-workbench__spinner" /><strong>正在打开{OFFICE_LABELS[kind]}…</strong><p>正在读取页面与原始格式</p></div>
          : model.kind === 'sheet' && model.sheets ? <SpreadsheetCanvas key={page} sheet={model.sheets[page]} zoom={zoom} target={sheetTarget?.page === page ? sheetTarget : undefined} />
          : metadata ? <div className="office-workbench__stage" style={{ width: Math.max(dimensions.width, metadata.width * zoom + 48), minHeight: Math.max(dimensions.height, metadata.height * zoom + 48) }}><OfficePage model={model} index={page} width={metadata.width * zoom} onEscape={() => setExpanded(false)} onSelectionText={onSelectionText} /></div> : null}
      </main>
    </div>
    {model?.kind === 'sheet' && <div className="office-workbench__sheet-tabs" role="tablist" aria-label="工作表">{model.pages.map((sheet, i) => <button type="button" key={i} role="tab" aria-selected={page === i} onClick={() => changePage(i)}>{sheet.label}</button>)}</div>}
    <footer className="office-workbench__footer">
      <span className="office-workbench__document-status" title={model?.notes.join(' ')}>{model?.notes.length ? model.notes[0] : customView ? '编辑文稿' : total ? `${total} ${kind === 'sheet' ? '个工作表' : '页'}${kind === 'pptx' ? ` · ${Math.abs((metadata?.width ?? 1280) / (metadata?.height ?? 720) - 16/9) < .02 ? '16:9' : Math.abs((metadata?.width ?? 1280) / (metadata?.height ?? 720) - 4/3) < .02 ? '4:3' : '自定义画布'}` : ''}` : OFFICE_LABELS[kind]}</span>
      {!customView && total > 0 && <label className="office-workbench__page-jump"><span>{kind === 'sheet' ? '工作表' : '页码'}</span><input type="number" aria-label={kind === 'sheet' ? '跳转到工作表' : '跳转到页码'} min={1} max={total} value={page + 1} onChange={e => { const n = Number(e.target.value); if (Number.isInteger(n) && n > 0) changePage(n - 1); }} /><span>/ {total}</span></label>}
      <div className="office-workbench__zoom"><button type="button" aria-label="缩小文档" disabled={customView || zoom <= 0.1} onClick={() => changeZoom(zoom - 0.1)}><Minus size={14} /></button><input type="range" aria-label="文档缩放" min={10} max={250} step={5} disabled={customView} value={Math.round(zoom * 100)} onChange={e => changeZoom(Number(e.target.value) / 100)} /><button type="button" aria-label="放大文档" disabled={customView || zoom >= 2.5} onClick={() => changeZoom(zoom + 0.1)}><Plus size={14} /></button><span>{Math.round(zoom * 100)}%</span><button type="button" aria-label={kind === 'pptx' ? '适合窗口' : kind === 'sheet' ? '实际大小' : '适合宽度'} title={kind === 'pptx' ? '适合窗口' : kind === 'sheet' ? '实际大小' : '适合宽度'} disabled={customView} onClick={() => setManualZoom(null)}><Scan size={15} /></button></div>
    </footer>
  </div>;
  return <WorkbenchSurface expanded={expanded}>{workbench}</WorkbenchSurface>;
}
