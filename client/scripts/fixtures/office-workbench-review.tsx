import React, { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { FolderOpen, ExternalLink } from 'lucide-react';
import OfficeWorkbench from '../../src/components/office/OfficeWorkbench';
import { AttachmentPreview } from '../../src/components/AttachmentPreview';
import '../../src/styles/index.css';
const files = ['春天.pptx', '春日计划.docx', '经营数据.xlsx', '春天.pdf', '图文演示.pptx', '分页长文.docx', '混合纸张.pdf', '超大合并.xlsx'];
const review = { fileReads: 0 };
async function readFixture(filename: string, signal?: AbortSignal) {
  const response = await fetch(`/__office-fixture/${encodeURIComponent(filename)}`, { signal });
  if (!response.ok) throw new Error('Missing fixture');
  const bytes = new Uint8Array(await response.arrayBuffer()), chunks = [];
  for (let i = 0; i < bytes.length; i += 16384) chunks.push(String.fromCharCode(...bytes.subarray(i, i + 16384)));
  return btoa(chunks.join(''));
}
Object.assign(window, { officeReview: review, __TAURI_INTERNALS__: { transformCallback: () => 1, unregisterCallback: () => {}, invoke: async (command: string, args: { path: string }) => {
  if (command === 'path_stat') return { absolute: args.path, exists: true, kind: 'file' };
  if (command === 'attachment_preview_data') { review.fileReads++; return readFixture(args.path.split('/').pop()!); }
  if (command === 'open_path') return;
  throw new Error(`Unexpected fixture command ${command}`);
} } });
function App() {
  const query = new URLSearchParams(location.search);
  const [filename, setFilename] = useState(query.get('file') || files[0]);
  const [content, setContent] = useState(''); const [reload, setReload] = useState(0);
  const [attachmentOpen, setAttachmentOpen] = useState(true);
  const attachment = query.get('attachment') === '1';
  const theme = query.get('theme') || 'light';
  document.documentElement.dataset.theme = theme;
  useEffect(() => {
    if (attachment) return;
    const controller = new AbortController(); setContent('');
    void readFixture(filename, controller.signal).then(content => {
      if (!controller.signal.aborted) setContent(content);
    }).catch(error => { if (!controller.signal.aborted) console.error(error); });
    return () => controller.abort();
  }, [filename, reload, attachment]);
  if (attachment) return <div className="app" data-theme={theme}>{attachmentOpen ? <AttachmentPreview paths={files.map(file => `/review/${file}`)} path={`/review/${filename}`} cwd="/review" onSelect={path => setFilename(path.split('/').pop()!)} onClose={() => setAttachmentOpen(false)} /> : <button onClick={() => setAttachmentOpen(true)}>重新打开附件</button>}</div>;
  return <div className="app" data-theme={theme} style={{ height: '100%', width: '100%', display: 'flex', flexDirection: 'column', padding: 14, gap: 0, background: 'var(--echo-bg-secondary)' }}>
    <div role="tablist" aria-label="打开的文档" style={{ display: 'flex', flexShrink: 0, alignItems: 'center', gap: 4, padding: '0 8px 10px', overflow: 'auto' }}>{files.map(file => <button key={file} role="tab" aria-selected={filename === file} onClick={() => setFilename(file)} style={{ padding: '8px 13px', border: '1px solid var(--echo-border-default)', borderRadius: 7, background: filename === file ? 'var(--echo-bg-primary)' : 'transparent', color: 'var(--echo-text-strong)', whiteSpace: 'nowrap', fontSize: 12 }}>{file}</button>)}</div>
    <div style={{ flex: 1, minHeight: 0, minWidth: 0 }}>{content ? <OfficeWorkbench key={`${filename}:${reload}`} filename={filename} content={content} onReload={() => setReload(v => v + 1)}
      fileActions={[
        { label: '在文件夹中显示', icon: FolderOpen, onSelect: () => {} },
        { label: '用系统应用打开', icon: ExternalLink, onSelect: () => {} },
      ]}/>
      : <p role="status">正在读取本地文档…</p>}</div>
  </div>;
}
createRoot(document.getElementById('root')!).render(<App/>);
