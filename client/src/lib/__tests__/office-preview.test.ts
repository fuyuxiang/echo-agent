import { describe, expect, it } from 'vitest';
import ssf from 'ssf';
import JSZip from 'jszip';
import { cellAddress, columnLabel, officeArchive, officeBytes, officeKind, searchExcerpt, spreadsheetNumberFormat } from '../office-preview';

describe('office preview input and original number formats', () => {
  it('renders currency, percentage, date and literal values using Excel formats', () => {
    for (const [format, value, expected] of [
      ['¥#,##0.00', 120500.5, '¥120,500.50'],
      ['"¥"#,##0.00', 120500.5, '¥120,500.50'],
      ['[$¥-804]#,##0.00', 120500.5, '¥120,500.50'],
      ['\\¥#,##0.00', 120500.5, '¥120,500.50'],
      ['0.0%', 0.402, '40.2%'],
      ['yyyy-mm-dd', 46082, '2026-03-01'],
    ] as const) expect(ssf.format(spreadsheetNumberFormat(format), value)).toBe(expected);
  });
  it('navigates actual spreadsheet coordinates including sparse far rows', () => {
    expect(cellAddress('XFD1048576')).toEqual({ row: 1048576, column: 16384 });
    expect(columnLabel(16384)).toBe('XFD');
    for (const invalid of ['A0', 'XFE1', 'A1048577', '1A', '../A1']) expect(cellAddress(invalid)).toBeNull();
    expect(cellAddress('$ab$5000')).toEqual({ row: 5000, column: 28 });
  });
  it('reads bounded binary payloads and rejects oversized input before decoding', () => {
    expect([...officeBytes('data:application/pdf;base64,JVBERg==')]).toEqual([37, 80, 68, 70]);
    expect(() => officeBytes(btoa('a'.repeat(200)), 5)).toThrow(/限制/);
    expect(() => officeBytes('')).toThrow(/为空/);
  });
  it('passes only fully admitted Office archives to rendering libraries', async () => {
    const zip = new JSZip(); zip.file('word/document.xml', '<document>safe content</document>');
    expect(new TextDecoder().decode(officeArchive(await zip.generateAsync({ type: 'uint8array' }))['word/document.xml'])).toContain('safe content');
    zip.file('word/oversized.xml', 'a'.repeat(4 * 1024 * 1024 + 1));
    expect(() => officeArchive(new Uint8Array(0))).toThrow();
    const oversized = await zip.generateAsync({ type: 'uint8array', compression: 'DEFLATE' });
    expect(() => officeArchive(oversized)).toThrow(/limit/);
  });
  it('identifies the four workbench formats and searches without interpreting markup', () => {
    expect(['deck.pptx','report.docx','data.xlsx','paper.pdf'].map(officeKind)).toEqual(['pptx','docx','sheet','pdf']);
    expect(officeKind('notes.txt')).toBeNull();
    expect(searchExcerpt('Spring <script> text', 'spring')).toContain('<script>');
    expect(searchExcerpt('春天的记录', '秋天')).toBe('');
  });
});
