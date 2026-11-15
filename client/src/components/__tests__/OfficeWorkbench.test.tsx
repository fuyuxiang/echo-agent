import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { OfficePreview, SpreadsheetSheet } from '@/lib/office-preview';
import { useModalFocus } from '@/lib/use-modal-focus';

const mocks = vi.hoisted(() => ({ load: vi.fn() }));
vi.mock('@/lib/office-preview-loaders', () => ({ loadOfficePreview: mocks.load }));
// The renderer engines are exercised in a real browser with real Office/PDF
// files. Here the boundary is async ownership, navigation and clipboard work.
vi.mock('../office/OfficePage', () => ({ default: ({ model, index, thumbnail }: { model: OfficePreview; index: number; thumbnail?: boolean }) =>
  thumbnail ? <span /> : <article>{model.pages[index].text}</article> }));
import OfficeWorkbench from '../office/OfficeWorkbench';

function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(accept => { resolve = accept; }); return { promise, resolve }; }
function documentModel(texts = ['First page', 'Second page', 'Third page']): OfficePreview {
  return { kind: 'pptx', pages: texts.map((text, i) => ({ label: `Slide ${i + 1}`, text, width: 1280, height: 720 })), notes: [],
    pageVisual: vi.fn(), pageText: vi.fn(async i => texts[i]), dispose: vi.fn() };
}
beforeAll(() => {
  HTMLElement.prototype.scrollIntoView = vi.fn(); HTMLElement.prototype.scrollTo = vi.fn();
  Object.defineProperty(HTMLElement.prototype, 'clientWidth', { configurable: true, get: () => 900 });
  Object.defineProperty(HTMLElement.prototype, 'clientHeight', { configurable: true, get: () => 600 });
});
beforeEach(() => mocks.load.mockReset());

describe('document workbench ownership and controls', () => {
  it('disposes late loads and keeps the currently opened document visible', async () => {
    const old = deferred<OfficePreview>(), current = documentModel(['Current document']), stale = documentModel(['Old document']);
    mocks.load.mockReturnValueOnce(old.promise).mockResolvedValueOnce(current);
    const view = render(<OfficeWorkbench filename="old.pptx" content="old bytes" />);
    await waitFor(() => expect(mocks.load).toHaveBeenCalledTimes(1));
    view.rerender(<OfficeWorkbench filename="new.pptx" content="new bytes" />);
    expect(await screen.findByRole('article')).toHaveTextContent('Current document');
    await act(async () => { old.resolve(stale); });
    expect(screen.getByRole('article')).toHaveTextContent('Current document');
    expect(stale.dispose).toHaveBeenCalledOnce();
    view.unmount(); expect(current.dispose).toHaveBeenCalledOnce();
  });
  it('never copies text from a document that has been closed during extraction', async () => {
    const extraction = deferred<string>(), old = documentModel(['Old text']), current = documentModel(['Current text']);
    old.pageText = vi.fn(() => extraction.promise);
    mocks.load.mockResolvedValueOnce(old).mockResolvedValueOnce(current);
    const copy = vi.fn(async () => true);
    const view = render(<OfficeWorkbench filename="old.pptx" content="old" onCopyText={copy} />);
    await screen.findByRole('article'); fireEvent.click(screen.getByRole('button', { name: '复制文档文本' }));
    view.rerender(<OfficeWorkbench filename="new.pptx" content="new" onCopyText={copy} />);
    await screen.findByText('Current text');
    await act(async () => { extraction.resolve('Old extracted text'); });
    expect(copy).not.toHaveBeenCalled();
  });
  it.each(['hide', 'unmount'])('cancels clipboard extraction when the view closes (%s)', async action => {
    const extraction = deferred<string>(), model = documentModel(['Original text']);
    model.pageText = vi.fn(() => extraction.promise); mocks.load.mockResolvedValue(model);
    const copy = vi.fn(async () => true), props = { filename: 'original.pptx', content: 'original', onCopyText: copy };
    const view = render(<OfficeWorkbench {...props} />);
    await screen.findByRole('article'); fireEvent.click(screen.getByRole('button', { name: '复制文档文本' }));
    if (action === 'hide') {
      view.rerender(<OfficeWorkbench {...props} active={false} />);
      view.rerender(<OfficeWorkbench {...props} active />);
      expect(screen.getByRole('button', { name: '复制文档文本' })).toBeEnabled();
    } else view.unmount();
    await act(async () => { extraction.resolve('Late original text'); }); expect(copy).not.toHaveBeenCalled();
  });
  it('supports page navigation, search, zoom and leaving fullscreen when hidden', async () => {
    mocks.load.mockResolvedValue(documentModel());
    const props = { filename: 'deck.pptx', content: 'bytes' };
    const view = render(<OfficeWorkbench {...props} />);
    await screen.findByText('First page');
    fireEvent.click(screen.getByRole('button', { name: '下一页' })); expect(screen.getByRole('article')).toHaveTextContent('Second page');
    fireEvent.change(screen.getByRole('spinbutton', { name: '跳转到页码' }), { target: { value: '3' } });
    expect(screen.getByRole('article')).toHaveTextContent('Third page');
    fireEvent.click(screen.getByRole('button', { name: '搜索文档' }));
    fireEvent.change(screen.getByRole('textbox', { name: '搜索文档内容' }), { target: { value: 'First' } });
    fireEvent.click(await screen.findByRole('button', { name: /第 1 页First page/ }));
    expect(screen.getByRole('article')).toHaveTextContent('First page');
    fireEvent.change(screen.getByRole('slider', { name: '文档缩放' }), { target: { value: '150' } }); expect(screen.getByText('150%')).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: '全屏预览' })); expect(screen.getByRole('dialog')).toBeVisible();
    view.rerender(<OfficeWorkbench {...props} active={false} />); expect(screen.queryByRole('dialog')).toBeNull();
  });
  it('retries a failed original read and re-reads published source revisions', async () => {
    const freshBytes = vi.fn().mockResolvedValueOnce('first revision').mockResolvedValueOnce('second revision').mockResolvedValueOnce('third revision');
    mocks.load.mockRejectedValueOnce(new Error('Original is unavailable')).mockResolvedValueOnce(documentModel(['Published revision two'])).mockResolvedValueOnce(documentModel(['Published revision three']));
    const props = { filename: 'report.docx', content: 'cached bytes', loadContent: freshBytes };
    const view = render(<OfficeWorkbench {...props} sourceRevision="1" />);
    expect(await screen.findByRole('alert')).toHaveTextContent('Original is unavailable');
    fireEvent.click(screen.getByRole('button', { name: '重试预览' })); expect(await screen.findByRole('article')).toHaveTextContent('Published revision two');
    expect(mocks.load.mock.calls[1][1]).toBe('second revision');
    view.rerender(<OfficeWorkbench {...props} sourceRevision="3" />);
    await screen.findByText('Published revision three'); expect(mocks.load.mock.calls[2][1]).toBe('third revision');
  });
  it('locates separate spreadsheet matches and preserves the canvas through fullscreen', async () => {
    const sheets: SpreadsheetSheet[] = ['一月', '二月'].map(name => ({ name, rows: 6000, columns: 12, cells: new Map(), columnWidths: Array(12).fill(80), rowHeights: new Map(), defaultRowHeight: 20, merges: [] }));
    for (const [index, address] of ['A5000', 'B4000'].entries()) sheets[index].cells.set(address, { address, row: index ? 4000 : 5000, column: index ? 2 : 1, text: '远处目标', value: '远处目标', style: {} });
    mocks.load.mockResolvedValue({ ...documentModel(), kind: 'sheet', sheets, pages: sheets.map(sheet => ({ label: sheet.name, text: '', width: 1000, height: 700 })) });
    render(<OfficeWorkbench filename="book.xlsx" content="bytes" />);
    await screen.findByRole('grid'); fireEvent.click(screen.getByRole('button', { name: '搜索文档' }));
    fireEvent.change(screen.getByRole('textbox', { name: '搜索文档内容' }), { target: { value: '远处目标' } });
    fireEvent.click(await screen.findByRole('button', { name: /一月 · A5000/ }));
    expect(screen.getByRole('textbox', { name: '跳转到单元格' })).toHaveValue('A5000');
    expect(screen.getByRole('gridcell', { name: 'A5000：远处目标' })).toHaveAttribute('aria-selected', 'true');
    const canvas = screen.getByRole('grid'), scrollTop = canvas.scrollTop;
    fireEvent.click(screen.getByRole('button', { name: '全屏预览' }));
    expect(screen.getByRole('grid')).toBe(canvas); expect(canvas.scrollTop).toBe(scrollTop);
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull(); expect(screen.getByRole('grid')).toBe(canvas);
    expect(screen.getByRole('textbox', { name: '跳转到单元格' })).toHaveValue('A5000');
    fireEvent.click(screen.getByRole('button', { name: /二月 · B4000/ }));
    expect(screen.getByRole('gridcell', { name: 'B4000：远处目标' })).toHaveAttribute('aria-selected', 'true');
    fireEvent.click(screen.getByRole('button', { name: /一月 · A5000/ }));
    expect(screen.getByRole('gridcell', { name: 'A5000：远处目标' })).toHaveAttribute('aria-selected', 'true');
  }, 15000);
  it('gives fullscreen ownership of keyboard input above an existing modal', async () => {
    mocks.load.mockResolvedValue(documentModel()); const close = vi.fn();
    function ParentDialog() { const ref = useModalFocus<HTMLDivElement>(true, close); return <div role="dialog" aria-label="附件" ref={ref}><button>关闭附件</button><OfficeWorkbench filename="deck.pptx" content="bytes" /></div>; }
    render(<ParentDialog />); await screen.findByText('First page');
    const expand = screen.getByRole('button', { name: '全屏预览' }); expand.focus(); fireEvent.click(expand);
    const last = screen.getByRole('button', { name: '适合窗口' }); last.focus(); fireEvent.keyDown(last, { key: 'Tab' });
    expect(document.activeElement?.closest('.office-workbench--expanded')).not.toBeNull();
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    expect(close).not.toHaveBeenCalled(); expect(screen.queryByRole('button', { name: '退出全屏预览' })).toBeNull();
    expect(expand).toHaveFocus(); fireEvent.keyDown(expand, { key: 'Escape' }); expect(close).toHaveBeenCalledOnce();
  });
});
