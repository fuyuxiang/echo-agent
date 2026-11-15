import { useEffect, useRef, useState } from 'react';
import type { OfficePreview, PageVisual } from '@/lib/office-preview';

export default function OfficePage({ model, index, width, thumbnail = false, onEscape, onSelectionText }: {
  model: OfficePreview; index: number; width: number; thumbnail?: boolean; onEscape?: () => void; onSelectionText?: (text: string) => void;
}) {
  const [state, setState] = useState<{ model: OfficePreview; index: number; visual?: PageVisual; source?: string; error?: string } | null>(null);
  const [ready, setReady] = useState(false);
  const selectionCallback = useRef(onSelectionText); selectionCallback.current = onSelectionText;
  const canvas = useRef<HTMLCanvasElement>(null);
  const owner = useRef({ model, index }); owner.current = { model, index };
  useEffect(() => {
    const controller = new AbortController(); let url: string | undefined;
    setReady(false);
    void model.pageVisual(index).then(async visual => {
      if (controller.signal.aborted) return;
      if (visual.svg) url = URL.createObjectURL(new Blob([visual.svg], { type: 'image/svg+xml' }));
      setState({ model, index, visual, source: url });
      if (model.renderPdf && canvas.current) {
        await model.renderPdf(index, canvas.current, width * Math.min(2, window.devicePixelRatio || 1), controller.signal);
        if (!controller.signal.aborted) setReady(true);
      }
    }).catch(error => { if (!controller.signal.aborted) setState({ model, index, error: String(error).replace(/^Error:\s*/, '') }); });
    return () => { controller.abort(); if (url) URL.revokeObjectURL(url); };
  }, [model, index, model.kind === 'pdf' ? width : undefined]);
  const current = state?.model === model && state.index === index ? state : null;
  const metadata = current?.visual ?? model.pages[index];
  const ratio = metadata ? metadata.height / metadata.width : 9 / 16;
  return <div className={`office-page office-page--${model.kind}${thumbnail ? ' office-page--thumbnail' : ''}`} style={{ width, height: width * ratio }} data-page-index={index} data-page-rendered={Boolean(ready && current?.visual)}>
    {current?.error ? <span className="office-page__error" role={thumbnail ? undefined : 'alert'}>{thumbnail ? '无法预览' : current.error}</span> : <>
      {model.kind === 'pdf' ? <canvas ref={canvas} role={thumbnail ? undefined : 'img'} aria-label={thumbnail ? undefined : `第 ${index + 1} 页 PDF`} />
        : current?.source ? <img src={current.source} alt={thumbnail ? '' : `幻灯片 ${index + 1}：${model.pages[index].label}`} draggable={false}
          onLoad={() => { if (owner.current.model === model && owner.current.index === index) setReady(true); }} onError={() => { if (owner.current.model === model && owner.current.index === index) setState({ model, index, error: '页面图像无法显示，请重新读取文件' }); }} />
        : current?.visual?.html ? <iframe srcDoc={current.visual.html} title={thumbnail ? `第 ${index + 1} 页缩略图` : `第 ${index + 1} 页 Word`} sandbox="allow-same-origin" tabIndex={-1}
          style={{ width: current.visual.width, height: current.visual.height, transform: `scale(${width / current.visual.width})` }}
          onLoad={e => { const frame = e.currentTarget; const doc = frame.contentDocument; if (!doc?.querySelector('section')) return;
            doc.addEventListener('mouseup', () => selectionCallback.current?.(doc.getSelection()?.toString() ?? ''));
            doc.addEventListener('keydown', event => { if (event.key === 'Escape') onEscape?.(); });
            void doc.fonts?.ready.then(() => { if (owner.current.model === model && owner.current.index === index) setReady(true); }); }} /> : null}
      {!ready && !thumbnail && <span className="office-page__loading" role="status">正在绘制页面…</span>}
    </>}
  </div>;
}
