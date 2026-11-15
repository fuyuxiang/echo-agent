import type { CSSProperties } from 'react';
import { detectPreviewKind } from './file-kind';
import { extractEntry, listZipEntries, MAX_ZIP_INPUT_BYTES } from './zip-reader';

export type OfficeKind = 'pptx' | 'docx' | 'sheet' | 'pdf';
export function officeKind(filename: string): OfficeKind | null {
  const kind = detectPreviewKind(filename);
  return kind === 'pptx' || kind === 'docx' || kind === 'sheet' || kind === 'pdf' ? kind : null;
}
export const OFFICE_LABELS: Record<OfficeKind, string> = { pptx: '演示文稿', docx: 'Word 文档', sheet: 'Excel 工作簿', pdf: 'PDF 文档' };
export interface OfficePage { label: string; width: number; height: number; text: string; }
export interface PageVisual { svg?: string; html?: string; width: number; height: number; }
export interface SheetCell { address: string; row: number; column: number; text: string; value: string; formula?: string; style: CSSProperties; }
export interface SheetMerge { top: number; left: number; bottom: number; right: number; }
export interface SpreadsheetSheet {
  name: string; rows: number; columns: number; cells: Map<string, SheetCell>;
  columnWidths: number[]; rowHeights: Map<number, number>; defaultRowHeight: number; merges: SheetMerge[];
  rowStyles?: Map<number, CSSProperties>; columnStyles?: Map<number, CSSProperties>;
}
export interface OfficePreview {
  kind: OfficeKind; pages: OfficePage[]; sheets?: SpreadsheetSheet[]; notes: string[];
  pageVisual(index: number): Promise<PageVisual>;
  renderPdf?(index: number, canvas: HTMLCanvasElement, width: number, signal: AbortSignal): Promise<void>;
  pageText(index: number): Promise<string>;
  dispose(): void;
}

export function officeBytes(content: string, maxBytes = 32 * 1024 * 1024): Uint8Array<ArrayBuffer> {
  if (!content) throw new Error('文件内容为空');
  const base64 = content.startsWith('data:') ? content.slice(content.indexOf(',') + 1) : content;
  if (base64.length > Math.ceil(maxBytes * 4 / 3) + 16) throw new Error('文件超过预览大小限制，请用系统应用打开原件');
  const binary = atob(base64.replace(/\s/g, ''));
  if (binary.length > maxBytes) throw new Error('文件超过预览大小限制');
  return Uint8Array.from(binary, char => char.charCodeAt(0));
}

/** Validate every entry before passing the original archive to a renderer. */
export function officeArchive(bytes: Uint8Array): Record<string, Uint8Array> {
  if (bytes.byteLength > MAX_ZIP_INPUT_BYTES) throw new Error('Office 文件超过 8 MiB 预览上限，请用系统应用打开原件');
  const entries = listZipEntries(bytes);
  if (!entries.length) throw new Error('文件不是可读取的 Office 文档，可能已损坏或加密');
  const files = Object.create(null) as Record<string, Uint8Array>;
  let total = 0;
  for (const entry of entries) {
    const data = extractEntry(bytes, entry, Math.min(4 * 1024 * 1024, 16 * 1024 * 1024 - total));
    total += data.byteLength;
    if (total > 16 * 1024 * 1024 || Object.prototype.hasOwnProperty.call(files, entry.name)) throw new Error('文档内容超过预览预算或包含重复条目');
    files[entry.name] = data;
  }
  return files;
}
export function xmlPart(files: Record<string, Uint8Array>, name: string): Document | null {
  const data = files[name];
  if (!data) return null;
  const xml = new DOMParser().parseFromString(new TextDecoder().decode(data), 'application/xml');
  if (xml.getElementsByTagName('parsererror').length) throw new Error('文档结构损坏，无法准确预览');
  return xml;
}
export function xmlNodes(parent: Document | Element, localName: string): Element[] {
  return Array.from(parent.getElementsByTagNameNS('*', localName));
}
export function throwIfAborted(signal: AbortSignal): void {
  if (signal.aborted) throw new DOMException('预览已取消', 'AbortError');
}
export function columnLabel(column: number): string {
  let label = '';
  for (let n = column; n > 0; n = Math.floor((n - 1) / 26)) label = String.fromCharCode(65 + (n - 1) % 26) + label;
  return label;
}
export function cellAddress(value: string): { row: number; column: number } | null {
  const match = /^\$?([A-Z]{1,3})\$?([1-9]\d{0,6})$/i.exec(value.trim());
  if (!match) return null;
  let column = 0;
  for (const char of match[1].toUpperCase()) column = column * 26 + char.charCodeAt(0) - 64;
  const row = Number(match[2]);
  return column <= 16384 && row <= 1048576 ? { row, column } : null;
}
export function searchExcerpt(text: string, query: string): string {
  const index = text.toLocaleLowerCase().indexOf(query.toLocaleLowerCase());
  return index < 0 ? '' : `${index > 24 ? '…' : ''}${text.slice(Math.max(0, index - 24), index + query.length + 72).replace(/\s+/g, ' ')}${text.length > index + query.length + 72 ? '…' : ''}`;
}

/** Excel permits unquoted Unicode currency symbols that SSF requires escaped. */
export function spreadsheetNumberFormat(format: string): string {
  let quoted = false, escaped = false, bracket = false, output = '';
  for (const char of format) {
    if (escaped) { output += char; escaped = false; continue; }
    if (char === '\\') { output += char; escaped = true; continue; }
    if (char === '"') quoted = !quoted;
    if (!quoted && char === '[') bracket = true;
    if (!quoted && char === ']') bracket = false;
    output += !quoted && !bracket && /[¥￥€£₹₩]/u.test(char) ? `\\${char}` : char;
  }
  return output;
}
