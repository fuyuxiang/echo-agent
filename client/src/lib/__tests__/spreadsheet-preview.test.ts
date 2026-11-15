import { afterEach, describe, expect, it, vi } from 'vitest';
import ExcelJS from 'exceljs';
import JSZip from 'jszip';
import { loadOfficePreview } from '../office-preview-loaders';

async function workbookContent(extra = '', modify?: (book: ExcelJS.Workbook) => void) {
  const book = new ExcelJS.Workbook(); book.addWorksheet('数据').getCell('A1').value = '合并标题'; modify?.(book);
  const zip = await JSZip.loadAsync(await book.xlsx.writeBuffer());
  const name = 'xl/worksheets/sheet1.xml', xml = await zip.file(name)!.async('string');
  zip.file(name, xml.replace('</worksheet>', `${extra}</worksheet>`));
  return zip.generateAsync({ type: 'base64', compression: 'DEFLATE' });
}
const load = (content: string) => loadOfficePreview('test.xlsx', content, new AbortController().signal);
afterEach(() => vi.restoreAllMocks());

describe('sparse spreadsheet preview and admission', () => {
  it.each(['A1:XFD1048576', 'A1:A100001', 'A1:XFE1', 'A1:A1048577'])('rejects unsafe merge %s before ExcelJS loading', async range => {
    const content = await workbookContent(`<mergeCells count="1"><mergeCell ref="${range}"/></mergeCells>`);
    // Never let a regression in admission exhaust the test runner's memory.
    const parser = vi.spyOn(Object.getPrototypeOf(new ExcelJS.Workbook().xlsx), 'load').mockRejectedValue(new Error('Unexpected unsafe parser call'));
    await expect(load(content)).rejects.toThrow(/合并区域/);
    expect(parser).not.toHaveBeenCalled();
  });
  it('keeps a large admitted merge sparse instead of materializing its addresses', async () => {
    const model = await load(await workbookContent('<mergeCells count="1"><mergeCell ref="A1:B50000"/></mergeCells>'));
    expect(model.sheets![0].cells.size).toBe(1);
    expect(model.sheets![0].merges).toEqual([{ top: 1, left: 1, bottom: 50000, right: 2 }]);
    expect(model.sheets![0].rows).toBe(50000); model.dispose();
  });
  it('retains blank cell formats, empty row heights and sparse high coordinates', async () => {
    const model = await load(await workbookContent('', book => {
      const sheet = book.worksheets[0]; sheet.getRow(2).height = 60;
      sheet.getCell('B2').fill = { type: 'pattern', pattern: 'solid', fgColor: { argb: 'FFFFCC00' } };
      sheet.getCell('B2').border = { bottom: { style: 'thin', color: { argb: 'FF112233' } } };
      sheet.getRow(3).height = 45; sheet.getRow(3).hidden = true;
      sheet.getColumn(4).width = 30; sheet.getColumn(4).fill = { type: 'pattern', pattern: 'solid', fgColor: { argb: 'FFEEFFEE' } };
      sheet.getCell('A1048576').value = '最后一行';
    }));
    const sheet = model.sheets![0];
    expect(sheet.rowHeights.get(2)).toBe(80); expect(sheet.rowHeights.get(3)).toBe(0);
    expect(sheet.cells.get('B2')?.style).toMatchObject({ backgroundColor: '#ffcc00', borderBottom: '1px solid #112233' });
    expect(sheet.columnStyles?.get(4)).toMatchObject({ backgroundColor: '#eeffee' });
    expect(sheet.cells.get('A1048576')?.text).toBe('最后一行'); expect(sheet.cells.size).toBeLessThan(10);
    model.dispose();
  });
  it('skips editing-only validation ranges that ExcelJS would otherwise expand', async () => {
    const content = await workbookContent('<dataValidations count="1"><dataValidation type="whole" sqref="A1:XFD1048576"><formula1>1</formula1></dataValidation></dataValidations>');
    const proto = Object.getPrototypeOf(new ExcelJS.Workbook().xlsx), original = proto.load;
    vi.spyOn(proto, 'load').mockImplementation(function(this: ExcelJS.Xlsx, bytes, options) {
      if (!(options as Partial<ExcelJS.XlsxReadOptions> | undefined)?.ignoreNodes?.includes('dataValidations')) throw new Error('Unsafe validation expansion');
      return original.call(this, bytes, options);
    });
    const model = await load(content); expect(model.sheets![0].cells.size).toBe(1); model.dispose();
  });
  it('rejects overlapping merges rather than silently hiding content', async () => {
    await expect(load(await workbookContent('<mergeCells count="2"><mergeCell ref="A1:B2"/><mergeCell ref="B2:C3"/></mergeCells>'))).rejects.toThrow(/重叠/);
  });
});
