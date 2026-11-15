import type { CSSProperties } from 'react';
import type { Cell, CellValue, Color, Font, Borders, Fill, Style } from 'exceljs';
import { columnLabel, spreadsheetNumberFormat, officeArchive, officeBytes, officeKind, throwIfAborted, xmlNodes, xmlPart } from './office-preview';
import type { OfficePreview, OfficePage, SpreadsheetSheet, SheetCell, PageVisual } from './office-preview';
import { spreadsheetPreviewBudget } from './spreadsheet-preview-budget';

export const DOCUMENT_FRAME_CSP = "default-src 'none'; img-src data: blob:; font-src data: blob:; style-src 'unsafe-inline'; connect-src 'none'; media-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'";
export function documentFrame(html: string, styles = ''): string {
  return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${DOCUMENT_FRAME_CSP}"><style>html,body{margin:0;padding:0;background:white;color:#111}body{overflow:hidden}section{margin:0!important;box-shadow:none!important}</style>${styles}</head><body>${html}</body></html>`;
}

export async function loadOfficePreview(filename: string, content: string, signal: AbortSignal): Promise<OfficePreview> {
  const kind = officeKind(filename);
  if (!kind) throw new Error('不支持的文档类型');
  const bytes = officeBytes(content, kind === 'pdf' ? 32 * 1024 * 1024 : 8 * 1024 * 1024);
  throwIfAborted(signal);
  if (kind === 'pdf') return loadPdf(bytes, signal);
  if (/\.xls$/i.test(filename)) throw new Error('旧版 .xls 文件请先另存为 .xlsx，再打开版式预览');
  const files = officeArchive(bytes);
  throwIfAborted(signal);
  if (kind === 'pptx') return loadPresentation(bytes, files, signal);
  if (kind === 'docx') return loadWord(bytes, files, signal);
  return loadSpreadsheet(bytes, files, signal);
}

async function loadPresentation(bytes: Uint8Array, files: Record<string, Uint8Array>, signal: AbortSignal): Promise<OfficePreview> {
  const [pptx, renderer] = await Promise.all([import('@office-kit/pptx'), import('@office-kit/pptx-preview')]);
  throwIfAborted(signal);
  const document = await pptx.loadPresentation(bytes);
  throwIfAborted(signal);
  const slides = pptx.getSlides(document);
  if (!slides.length || slides.length > 300) throw new Error('演示文稿为空或超过 300 页预览上限');
  const presentation = xmlPart(files, 'ppt/presentation.xml');
  const size = presentation && xmlNodes(presentation, 'sldSz')[0];
  const ratio = size ? Number(size.getAttribute('cx')) / Number(size.getAttribute('cy')) : 16 / 9;
  const width = 1280, height = width / (Number.isFinite(ratio) && ratio > 0.1 && ratio < 10 ? ratio : 16 / 9);
  const rels = xmlPart(files, 'ppt/_rels/presentation.xml.rels');
  const targets = new Map(rels ? xmlNodes(rels, 'Relationship').filter(r => r.getAttribute('TargetMode') !== 'External').map(r => [r.getAttribute('Id'), r.getAttribute('Target') ?? '']) : []);
  const order = presentation ? xmlNodes(presentation, 'sldId').map(n => n.getAttributeNS('http://schemas.openxmlformats.org/officeDocument/2006/relationships', 'id') ?? n.getAttribute('r:id')) : [];
  const pages: OfficePage[] = slides.map((_, index) => {
    const target = targets.get(order[index]) ?? `slides/slide${index + 1}.xml`;
    const name = target.startsWith('/') ? target.slice(1) : `ppt/${target.replace(/^\.\//, '')}`;
    const xml = xmlPart(files, name);
    const lines = xml ? xmlNodes(xml, 'p').map(p => xmlNodes(p, 't').map(t => t.textContent ?? '').join('')).filter(Boolean) : [];
    return { label: lines[0]?.slice(0, 60) || `幻灯片 ${index + 1}`, width, height, text: lines.join('\n') };
  });
  const cache = new Map<number, PageVisual>();
  return { kind: 'pptx', pages, notes: [], async pageVisual(index) {
    throwIfAborted(signal);
    if (!cache.has(index)) cache.set(index, { svg: renderer.renderSlideToSvg(document, slides[index]), width, height });
    return cache.get(index)!;
  }, async pageText(index) { return pages[index]?.text ?? ''; }, dispose() { cache.clear(); } };
}

async function withoutExternalRelationships(bytes: Uint8Array, files: Record<string, Uint8Array>): Promise<Uint8Array> {
  const replacements: Array<[string, string]> = [];
  for (const name of Object.keys(files).filter(name => name.endsWith('.rels'))) {
    const xml = xmlPart(files, name);
    if (!xml) continue;
    const external = xmlNodes(xml, 'Relationship').filter(r => r.getAttribute('TargetMode') === 'External');
    if (!external.length) continue;
    external.forEach(node => node.remove());
    replacements.push([name, new XMLSerializer().serializeToString(xml)]);
  }
  if (!replacements.length) return bytes;
  const { default: JSZip } = await import('jszip');
  const zip = await JSZip.loadAsync(bytes);
  replacements.forEach(([name, xml]) => zip.file(name, xml));
  return zip.generateAsync({ type: 'uint8array' });
}

async function loadWord(bytes: Uint8Array, files: Record<string, Uint8Array>, signal: AbortSignal): Promise<OfficePreview> {
  const [{ renderAsync }, safeBytes] = await Promise.all([import('@apollo-design/docx-preview'), withoutExternalRelationships(bytes, files)]);
  throwIfAborted(signal);
  const frame = document.createElement('iframe');
  frame.setAttribute('sandbox', 'allow-same-origin');
  frame.setAttribute('aria-hidden', 'true');
  frame.style.cssText = 'position:fixed;left:-20000px;top:0;width:1400px;height:1200px;visibility:hidden;pointer-events:none;border:0';
  let cancelReady: () => void = () => {};
  const ready = new Promise<void>((resolve, reject) => { frame.onload = () => resolve(); cancelReady = () => reject(new DOMException('预览已取消', 'AbortError')); signal.addEventListener('abort', cancelReady, { once: true }); });
  frame.srcdoc = documentFrame('');
  document.body.append(frame);
  try {
    await ready; throwIfAborted(signal);
    const doc = frame.contentDocument;
    if (!doc) throw new Error('无法创建文档版式预览');
    const styles = doc.createElement('div'); doc.head.append(styles);
    await renderAsync(safeBytes, doc.body, styles, { className: 'office-word-page', inWrapper: false, breakPages: true, paginate: true,
      ignoreLastRenderedPageBreak: false, useBase64URL: true, renderAltChunks: false,
      renderHeaders: true, renderFooters: true, renderFootnotes: true, renderEndnotes: true });
    throwIfAborted(signal);
    await Promise.all(Array.from(doc.images).map(image => image.decode?.().catch(() => undefined)));
    throwIfAborted(signal);
    doc.querySelectorAll('a').forEach(link => { link.removeAttribute('href'); link.removeAttribute('target'); });
    const sections = Array.from(doc.body.querySelectorAll<HTMLElement>('section.office-word-page'));
    if (!sections.length || sections.length > 300) throw new Error('Word 文档为空或超过 300 页预览上限');
    const pageStyles = styles.innerHTML + Array.from(doc.body.querySelectorAll('style')).map(style => style.outerHTML).join('');
    const visuals: PageVisual[] = sections.map(section => ({ html: documentFrame(section.outerHTML, pageStyles),
      width: Math.max(100, section.getBoundingClientRect().width || 794), height: Math.max(100, section.getBoundingClientRect().height || 1123) }));
    const pages = sections.map((section, i) => ({ label: `第 ${i + 1} 页`, width: visuals[i].width, height: visuals[i].height, text: section.textContent ?? '' }));
    return { kind: 'docx', pages, notes: [], async pageVisual(index) { return visuals[index]; }, async pageText(index) { return pages[index]?.text ?? ''; }, dispose() {} };
  } finally { signal.removeEventListener('abort', cancelReady); frame.remove(); }
}

async function loadPdf(bytes: Uint8Array, signal: AbortSignal): Promise<OfficePreview> {
  const pdfjs = await import('pdfjs-dist/legacy/build/pdf.mjs');
  const { default: worker } = await import('pdfjs-dist/legacy/build/pdf.worker.min.mjs?url');
  throwIfAborted(signal);
  pdfjs.GlobalWorkerOptions.workerSrc = worker;
  const assets = new URL(`${import.meta.env.BASE_URL}document-runtime/pdfjs/${pdfjs.version}/`, window.location.href).href;
  const task = pdfjs.getDocument({ data: bytes.slice(), cMapUrl: `${assets}cmaps/`, cMapPacked: true,
    standardFontDataUrl: `${assets}standard_fonts/`, wasmUrl: `${assets}wasm/`, iccUrl: `${assets}iccs/`,
    enableXfa: false });
  const abort = () => { void task.destroy(); };
  signal.addEventListener('abort', abort, { once: true });
  try {
    const pdf = await task.promise; throwIfAborted(signal);
    if (pdf.numPages > 300) { await task.destroy(); throw new Error('PDF 超过 300 页预览上限，请用系统应用打开原件'); }
    const pages: OfficePage[] = [];
    for (let start = 0; start < pdf.numPages; start += 6) {
      throwIfAborted(signal);
      const group = await Promise.all(Array.from({ length: Math.min(6, pdf.numPages - start) }, async (_, offset) => {
        const index = start + offset; const page = await pdf.getPage(index + 1); const view = page.getViewport({ scale: 1 });
        if (!Number.isFinite(view.width) || !Number.isFinite(view.height) || view.width <= 0 || view.height <= 0) throw new Error('PDF 页面尺寸无效');
        return { label: `第 ${index + 1} 页`, width: view.width, height: view.height, text: '' };
      }));
      pages.push(...group);
    }
    const texts = new Map<number, string>();
    return { kind: 'pdf', pages, notes: [], async pageVisual(index) {
      const page = await pdf.getPage(index + 1); const view = page.getViewport({ scale: 1 });
      return { width: view.width, height: view.height };
    }, async renderPdf(index, canvas, width, renderSignal) {
      const page = await pdf.getPage(index + 1); throwIfAborted(renderSignal);
      const view = page.getViewport({ scale: Math.min(3, Math.max(0.1, width / page.getViewport({ scale: 1 }).width)) });
      if (view.width > 16384 || view.height > 16384 || view.width * view.height > 16 * 1024 * 1024) throw new Error('页面尺寸过大，请缩小后预览或用系统应用打开');
      canvas.width = Math.ceil(view.width); canvas.height = Math.ceil(view.height);
      const context = canvas.getContext('2d');
      if (!context) throw new Error('无法创建 PDF 画布');
      const rendering = page.render({ canvas, canvasContext: context, viewport: view });
      const cancel = () => rendering.cancel(); renderSignal.addEventListener('abort', cancel, { once: true });
      try { await rendering.promise; } finally { renderSignal.removeEventListener('abort', cancel); }
    }, async pageText(index) {
      if (!texts.has(index)) { const page = await pdf.getPage(index + 1); const content = await page.getTextContent();
        texts.set(index, content.items.map(item => 'str' in item ? item.str : '').join(' ')); }
      return texts.get(index)!;
    }, dispose() { signal.removeEventListener('abort', abort); void task.destroy(); } };
  } catch (error) { signal.removeEventListener('abort', abort); void task.destroy(); throw error; }
}

const DEFAULT_THEME = ['FFFFFF','000000','EEECE1','1F497D','4F81BD','C0504D','9BBB59','8064A2','4BACC6','F79646','0000FF','800080'];
function sheetTheme(files: Record<string, Uint8Array>): string[] {
  const xml = xmlPart(files, 'xl/theme/theme1.xml');
  const scheme = xml && xmlNodes(xml, 'clrScheme')[0];
  if (!scheme) return DEFAULT_THEME;
  // Spreadsheet theme indices order light/dark, unlike the XML child order.
  const names = ['lt1','dk1','lt2','dk2','accent1','accent2','accent3','accent4','accent5','accent6','hlink','folHlink'];
  return names.map((name, i) => { const node = xmlNodes(scheme, name)[0]?.firstElementChild;
    return node?.getAttribute('val')?.match(/^[\da-f]{6}$/i)?.[0] ?? node?.getAttribute('lastClr') ?? DEFAULT_THEME[i]; });
}
function color(value: Partial<Color> | undefined, theme: string[]): string | undefined {
  const hex = value?.argb?.slice(-6) ?? (value?.theme !== undefined ? theme[value.theme] : undefined);
  if (!hex || !/^[\da-f]{6}$/i.test(hex)) return undefined;
  const tint = Math.max(-1, Math.min(1, (value as (Partial<Color> & { tint?: number }) | undefined)?.tint ?? 0));
  return `#${[0, 2, 4].map(offset => { const channel = parseInt(hex.slice(offset, offset + 2), 16);
    return Math.round(tint < 0 ? channel * (1 + tint) : channel + (255 - channel) * tint).toString(16).padStart(2, '0'); }).join('')}`;
}
function cellStyle(cell: Partial<Style> & Partial<Pick<Cell, 'value' | 'result'>>, theme: string[]): CSSProperties {
  const font = cell.font as Partial<Font> | undefined;
  const style: CSSProperties = { color: color(font?.color, theme) ?? '#202124', fontFamily: font?.name || 'Calibri, Arial, sans-serif',
    fontSize: (font?.size || 11) * 4 / 3, fontWeight: font?.bold ? 700 : 400, fontStyle: font?.italic ? 'italic' : 'normal',
    textDecoration: [font?.underline ? 'underline' : '', font?.strike ? 'line-through' : ''].filter(Boolean).join(' ') || undefined,
    textAlign: cell.alignment?.horizontal === 'centerContinuous' ? 'center' : ['left','center','right','justify'].includes(cell.alignment?.horizontal ?? '') ? cell.alignment!.horizontal as CSSProperties['textAlign'] : typeof cell.value === 'number' || typeof cell.result === 'number' || cell.value instanceof Date ? 'right' : 'left',
    alignItems: cell.alignment?.vertical === 'top' ? 'flex-start' : cell.alignment?.vertical === 'middle' ? 'center' : 'flex-end',
    whiteSpace: cell.alignment?.wrapText ? 'pre-wrap' : 'pre', paddingLeft: 5 + (cell.alignment?.indent || 0) * 12, paddingRight: 5 };
  const fill = cell.fill as Fill | undefined;
  if (fill?.type === 'pattern' && fill.pattern === 'solid') style.backgroundColor = color(fill.fgColor, theme);
  for (const [side, property] of [['top','borderTop'],['right','borderRight'],['bottom','borderBottom'],['left','borderLeft']] as const) {
    const border = (cell.border as Partial<Borders> | undefined)?.[side];
    if (border?.style) style[property] = `${/medium|thick/.test(border.style) ? 2 : 1}px ${/dash|dot/i.test(border.style) ? 'dashed' : 'solid'} ${color(border.color, theme) ?? '#444444'}`;
  }
  return style;
}
function cellRaw(value: CellValue): string | number | boolean | Date | null {
  if (value == null) return null;
  if (typeof value !== 'object' || value instanceof Date) return value;
  if ('formula' in value || 'sharedFormula' in value) return cellRaw(value.result ?? null);
  if ('richText' in value) return value.richText.map(run => run.text).join('');
  if ('text' in value) return String(value.text);
  if ('error' in value) return value.error;
  return '';
}
async function loadSpreadsheet(bytes: Uint8Array, files: Record<string, Uint8Array>, signal: AbortSignal): Promise<OfficePreview> {
  const sparseSheets = spreadsheetPreviewBudget(files);
  const [{ default: ExcelJS }, { default: ssf }] = await Promise.all([import('exceljs'), import('ssf')]);
  throwIfAborted(signal);
  const workbook = new ExcelJS.Workbook();
  // Keep merges sparse and skip editing-only validation ranges: ExcelJS
  // otherwise materializes every address in both kinds of range.
  await workbook.xlsx.load(bytes.slice().buffer as ArrayBuffer, { ignoreNodes: ['mergeCells', 'dataValidations'] }); throwIfAborted(signal);
  const theme = sheetTheme(files); const sheets: SpreadsheetSheet[] = [];
  for (const sheet of workbook.worksheets.filter(sheet => sheet.state === 'visible')) {
    const sparse = sparseSheets.get(sheet.name);
    if (!sparse) throw new Error('工作表结构不一致，无法准确预览');
    const cells = new Map<string, SheetCell>(); const rowHeights = new Map<number, number>();
    const rowStyles = new Map<number, CSSProperties>(), columnStyles = new Map<number, CSSProperties>();
    for (const number of sparse.rows) {
      const row = sheet.getRow(number);
      if (row.hidden || row.height !== undefined) rowHeights.set(row.number, row.hidden ? 0 : row.height * 4 / 3);
      if (row.font || row.fill || row.border || row.alignment || row.numFmt) rowStyles.set(number, cellStyle(row, theme));
    }
    for (const address of sparse.cells) {
      const cell = sheet.getCell(address);
      const raw = cellRaw(cell.value); let text = raw == null ? '' : raw instanceof Date ? raw.toLocaleDateString() : String(raw);
      if (cell.numFmt && raw != null) { try { text = ssf.format(spreadsheetNumberFormat(cell.numFmt), raw, { date1904: workbook.properties.date1904 }); } catch { /* Preserve readable cached values for unknown formats. */ } }
      const value = cell.value;
      const formula = value && typeof value === 'object' && ('formula' in value || 'sharedFormula' in value) ? cell.formula : undefined;
      cells.set(cell.address, { address: cell.address, row: Number(cell.row), column: Number(cell.col), text,
        value: raw == null ? '' : raw instanceof Date ? raw.toISOString() : String(raw), formula, style: cellStyle(cell, theme) });
    }
    const columns = Math.max(12, sparse.maxColumn), rows = Math.max(30, sparse.maxRow);
    const columnWidths = Array.from({ length: columns }, (_, i) => { const col = sheet.getColumn(i + 1); return col.hidden ? 0 : Math.max(8, Math.min(1800, (col.width || sheet.properties.defaultColWidth || 8.43) * 7 + 5)); });
    for (let number = 1; number <= columns; number++) { const style = sheet.getColumn(number).style; if (Object.keys(style).length) columnStyles.set(number, cellStyle(style, theme)); }
    sheets.push({ name: sheet.name, rows, columns, cells, columnWidths, rowHeights, rowStyles, columnStyles,
      defaultRowHeight: (sheet.properties.defaultRowHeight || 15) * 4 / 3, merges: sparse.merges });
  }
  if (!sheets.length) throw new Error('工作簿中没有可见工作表');
  const pages = sheets.map(sheet => ({ label: sheet.name, width: 1000, height: 700,
    text: [...sheet.cells.values()].map(cell => `${cell.address} ${cell.text}`).join('\n') }));
  const notes = Object.keys(files).some(name => /^xl\/(?:charts\/chart\d+|drawings\/drawing\d+)\.xml$/.test(name)) ? ['此工作簿含图表或图片，当前显示单元格与格式，嵌入对象请用系统应用查看。'] : [];
  return { kind: 'sheet', pages, sheets, notes, async pageVisual() { return { width: 1000, height: 700 }; },
    async pageText(index) { return pages[index]?.text ?? ''; }, dispose() { cellsClear(sheets); } };
}
function cellsClear(sheets: SpreadsheetSheet[]): void { sheets.forEach(sheet => sheet.cells.clear()); }
export { columnLabel };
