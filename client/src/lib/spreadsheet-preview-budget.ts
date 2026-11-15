import { cellAddress, columnLabel, xmlNodes, xmlPart, type SheetMerge } from './office-preview';

export const MAX_SPREADSHEET_CELLS = 100_000;
export interface SparseWorksheet {
  rows: number[];
  cells: string[];
  merges: SheetMerge[];
  maxRow: number;
  maxColumn: number;
}

function integer(value: string, maximum: number): number {
  const number = Number(value);
  if (!/^\d+$/.test(value) || !Number.isSafeInteger(number) || number < 1 || number > maximum) {
    throw new Error('工作表包含无效的行列坐标，请用系统应用检查原件');
  }
  return number;
}

/** Admit sparse coordinates before ExcelJS can allocate rows, columns or merges. */
export function spreadsheetPreviewBudget(files: Record<string, Uint8Array>): Map<string, SparseWorksheet> {
  const parts = new Map<string, SparseWorksheet>();
  let cells = 0, rows = 0, columns = 0, mergedCells = 0;
  for (const name of Object.keys(files).filter(name => /^xl\/worksheets\/[^/]+\.xml$/.test(name))) {
    if (parts.size >= 300) throw new Error('工作簿超过 300 个工作表预览上限');
    const xml = xmlPart(files, name)!;
    const part: SparseWorksheet = { rows: [], cells: [], merges: [], maxRow: 0, maxColumn: 0 };
    const addresses = new Set<string>(), rowNumbers = new Set<number>(), merged = new Set<string>();
    for (const row of xmlNodes(xml, 'row')) {
      if (++rows > MAX_SPREADSHEET_CELLS) throw new Error('工作簿超过 100,000 条行记录预览上限');
      const number = integer(row.getAttribute('r') ?? String(part.maxRow + 1), 1_048_576);
      if (rowNumbers.has(number)) throw new Error('工作表包含重复行，无法准确预览');
      rowNumbers.add(number); part.rows.push(number); part.maxRow = Math.max(part.maxRow, number);
      let previousColumn = 0;
      for (const cell of xmlNodes(row, 'c')) {
        if (++cells > MAX_SPREADSHEET_CELLS) throw new Error('工作簿超过 100,000 个单元格预览上限');
        const address = (cell.getAttribute('r') ?? `${columnLabel(previousColumn + 1)}${number}`).replace(/\$/g, '').toUpperCase();
        const position = cellAddress(address);
        if (!position || position.row !== number || addresses.has(address)) throw new Error('工作表包含无效或重复的单元格坐标');
        addresses.add(address); part.cells.push(address); previousColumn = position.column;
        part.maxColumn = Math.max(part.maxColumn, position.column);
      }
    }
    for (const column of xmlNodes(xml, 'col')) {
      const first = integer(column.getAttribute('min') ?? '', 16_384), last = integer(column.getAttribute('max') ?? '', 16_384);
      if (last < first) throw new Error('工作表列范围无效');
      columns += last - first + 1;
      if (columns > MAX_SPREADSHEET_CELLS) throw new Error('工作簿列格式超过预览预算');
      part.maxColumn = Math.max(part.maxColumn, last);
    }
    for (const node of xmlNodes(xml, 'mergeCell')) {
      const range = (node.getAttribute('ref') ?? '').split(':');
      const start = cellAddress(range[0]), end = cellAddress(range[1] ?? range[0]);
      if (range.length > 2 || !start || !end || start.row > end.row || start.column > end.column) throw new Error('工作表合并区域无效');
      mergedCells += (end.row - start.row + 1) * (end.column - start.column + 1);
      if (mergedCells > MAX_SPREADSHEET_CELLS) throw new Error('合并区域合计超过 100,000 个单元格预览上限，请用系统应用打开原件');
      // A bounded occupancy set also rejects overlapping ranges without an
      // unbounded pairwise comparison between thousands of small merges.
      for (let row = start.row; row <= end.row; row++) for (let column = start.column; column <= end.column; column++) {
        const key = `${row}:${column}`;
        if (merged.has(key)) throw new Error('工作表包含重叠的合并区域');
        merged.add(key);
      }
      part.merges.push({ top: start.row, left: start.column, bottom: end.row, right: end.column });
      part.maxRow = Math.max(part.maxRow, end.row); part.maxColumn = Math.max(part.maxColumn, end.column);
    }
    parts.set(name, part);
  }
  const workbook = xmlPart(files, 'xl/workbook.xml'), relationships = xmlPart(files, 'xl/_rels/workbook.xml.rels');
  if (!workbook || !relationships) throw new Error('工作簿结构缺失，无法预览');
  const targets = new Map(xmlNodes(relationships, 'Relationship').filter(node => node.getAttribute('TargetMode') !== 'External')
    .map(node => [node.getAttribute('Id'), node.getAttribute('Target') ?? '']));
  const sheets = new Map<string, SparseWorksheet>();
  for (const sheet of xmlNodes(workbook, 'sheet')) {
    const id = sheet.getAttributeNS('http://schemas.openxmlformats.org/officeDocument/2006/relationships', 'id') ?? sheet.getAttribute('r:id');
    const target = targets.get(id);
    if (!target) throw new Error('工作表关联缺失，无法准确预览');
    const path = new URL(target, 'https://office.invalid/xl/').pathname.slice(1);
    const part = parts.get(path);
    if (!part) throw new Error('工作簿包含暂不支持的工作表类型，请用系统应用打开原件');
    sheets.set(sheet.getAttribute('name') ?? '', part);
  }
  return sheets;
}
