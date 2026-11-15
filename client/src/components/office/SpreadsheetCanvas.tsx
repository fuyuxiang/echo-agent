import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { cellAddress, columnLabel } from '@/lib/office-preview';
import type { SheetMerge, SpreadsheetSheet } from '@/lib/office-preview';

const ROW_HEADER = 46, COLUMN_HEADER = 26;
function upperBound(values: number[], target: number): number {
  let lo = 0, hi = values.length;
  while (lo < hi) { const mid = (lo + hi) >>> 1; if (values[mid] <= target) lo = mid + 1; else hi = mid; }
  return lo;
}
export default function SpreadsheetCanvas({ sheet, zoom, target }: { sheet: SpreadsheetSheet; zoom: number; target?: { address: string; request: number } }) {
  const viewport = useRef<HTMLDivElement>(null);
  const [scroll, setScroll] = useState({ left: 0, top: 0, width: 900, height: 600 });
  const [selected, setSelected] = useState({ row: 1, column: 1 });
  const [addressInput, setAddressInput] = useState('A1');
  const [invalidAddress, setInvalidAddress] = useState(false);
  const geometry = useMemo(() => {
    const columns = [0]; sheet.columnWidths.forEach(width => columns.push(columns[columns.length - 1] + width));
    const overrides = [...sheet.rowHeights.entries()].sort((a, b) => a[0] - b[0]);
    const indices = overrides.map(([row]) => row), extras = [0];
    overrides.forEach(([, height]) => extras.push(extras[extras.length - 1] + (height > 0 ? Math.max(6, height) : 0) - sheet.defaultRowHeight));
    const rowTop = (row: number) => (row - 1) * sheet.defaultRowHeight + extras[upperBound(indices, row - 1)];
    const rowHeight = (row: number) => { const h = sheet.rowHeights.get(row) ?? sheet.defaultRowHeight; return h > 0 ? Math.max(6, h) : 0; };
    const findRow = (top: number) => { let lo = 1, hi = sheet.rows; while (lo < hi) { const mid = Math.ceil((lo + hi) / 2); if (rowTop(mid) <= top) lo = mid; else hi = mid - 1; } return lo; };
    return { columns, rowTop, rowHeight, findRow, height: rowTop(sheet.rows + 1) };
  }, [sheet]);
  useEffect(() => {
    const node = viewport.current;
    if (!node) return;
    const measure = () => setScroll({ left: node.scrollLeft, top: node.scrollTop, width: node.clientWidth, height: node.clientHeight });
    measure(); const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure); observer?.observe(node);
    return () => observer?.disconnect();
  }, []);
  useLayoutEffect(() => { setSelected({ row: 1, column: 1 }); setAddressInput('A1'); setInvalidAddress(false); viewport.current?.scrollTo(0, 0); }, [sheet]);
  const mergeFor = (row: number, column: number) => sheet.merges.find(m => m.top <= row && row <= m.bottom && m.left <= column && column <= m.right);
  const choose = (row: number, column: number, reveal = false) => {
    row = Math.max(1, Math.min(sheet.rows, row)); column = Math.max(1, Math.min(sheet.columns, column));
    const merged = mergeFor(row, column); if (merged) { row = merged.top; column = merged.left; }
    setSelected({ row, column }); setAddressInput(`${columnLabel(column)}${row}`); setInvalidAddress(false);
    const node = viewport.current;
    if (node && reveal) {
      const left = (geometry.columns[column - 1] + ROW_HEADER) * zoom, right = (geometry.columns[merged?.right ?? column] + ROW_HEADER) * zoom;
      const top = (geometry.rowTop(row) + COLUMN_HEADER) * zoom, bottom = (geometry.rowTop((merged?.bottom ?? row) + 1) + COLUMN_HEADER) * zoom;
      if (left < node.scrollLeft + ROW_HEADER * zoom) node.scrollLeft = Math.max(0, left - ROW_HEADER * zoom);
      else if (right > node.scrollLeft + node.clientWidth) node.scrollLeft = right - node.clientWidth;
      if (top < node.scrollTop + COLUMN_HEADER * zoom) node.scrollTop = Math.max(0, top - COLUMN_HEADER * zoom);
      else if (bottom > node.scrollTop + node.clientHeight) node.scrollTop = bottom - node.clientHeight;
      setScroll(s => ({ ...s, left: node.scrollLeft, top: node.scrollTop }));
    }
  };
  useLayoutEffect(() => {
    const address = target && cellAddress(target.address);
    if (!address) return;
    choose(address.row, address.column, true);
    viewport.current?.focus({ preventScroll: true });
  }, [sheet, target]);
  const left = scroll.left / zoom, top = scroll.top / zoom;
  const firstColumn = Math.max(1, upperBound(geometry.columns, Math.max(0, left - ROW_HEADER)) - 1);
  const lastColumn = Math.min(sheet.columns, upperBound(geometry.columns, left + scroll.width / zoom) + 1);
  const firstRow = Math.max(1, geometry.findRow(Math.max(0, top - COLUMN_HEADER)) - 2);
  const lastRow = Math.min(sheet.rows, geometry.findRow(top + scroll.height / zoom) + 3);
  const visibleRows = Array.from({ length: Math.max(0, lastRow - firstRow + 1) }, (_, i) => firstRow + i).filter(row => geometry.rowHeight(row) > 0);
  const visibleColumns = Array.from({ length: Math.max(0, lastColumn - firstColumn + 1) }, (_, i) => firstColumn + i).filter(column => sheet.columnWidths[column - 1] > 0);
  const merges = sheet.merges.filter(m => m.bottom >= firstRow && m.top <= lastRow && m.right >= firstColumn && m.left <= lastColumn);
  const cellAt = (row: number, column: number, merge?: SheetMerge) => {
    const address = `${columnLabel(column)}${row}`;
    const cell = sheet.cells.get(address);
    const isSelected = selected.row === row && selected.column === column;
    return <div key={address} role="gridcell" aria-rowindex={row + 1} aria-colindex={column + 1} aria-selected={isSelected}
      aria-label={`${address}${cell?.text ? `：${cell.text}` : ''}`} data-cell-address={address}
      className={`office-sheet__cell${isSelected ? ' office-sheet__cell--selected' : ''}`}
      style={{ ...sheet.columnStyles?.get(column), ...sheet.rowStyles?.get(row), ...cell?.style, left: ROW_HEADER + geometry.columns[column - 1], top: COLUMN_HEADER + geometry.rowTop(row),
        width: geometry.columns[merge?.right ?? column] - geometry.columns[column - 1],
        height: geometry.rowTop((merge?.bottom ?? row) + 1) - geometry.rowTop(row), zIndex: merge ? 2 : 1 }}
      onClick={() => { choose(row, column); viewport.current?.focus({ preventScroll: true }); }}
      title={cell?.formula ? `=${cell.formula}\n${cell.text}` : cell?.text}>
      <span>{cell?.text ?? ''}</span>
    </div>;
  };
  const current = sheet.cells.get(`${columnLabel(selected.column)}${selected.row}`);
  return <div className="office-sheet" aria-label={`工作表 ${sheet.name}`}>
    <div className="office-sheet__formula">
      <input aria-label="跳转到单元格" value={addressInput} aria-invalid={invalidAddress} spellCheck={false}
        onChange={e => { setAddressInput(e.target.value.toUpperCase()); setInvalidAddress(false); }}
        onKeyDown={e => { if (e.key !== 'Enter') return; const address = cellAddress(addressInput); if (!address || address.row > sheet.rows || address.column > sheet.columns) setInvalidAddress(true); else { choose(address.row, address.column, true); viewport.current?.focus(); } }} />
      <span className="office-sheet__fx" aria-hidden>ƒx</span>
      <input aria-label="单元格内容" readOnly value={current?.formula ? `=${current.formula}` : current?.value ?? ''} />
    </div>
    <div className="office-sheet__viewport" ref={viewport} role="grid" tabIndex={0} aria-label={`${sheet.name} 单元格网格`} aria-rowcount={sheet.rows + 1} aria-colcount={sheet.columns + 1}
      onScroll={e => { const node = e.currentTarget; setScroll(s => ({ ...s, left: node.scrollLeft, top: node.scrollTop })); }}
      onKeyDown={e => { if (!['ArrowUp','ArrowDown','ArrowLeft','ArrowRight'].includes(e.key)) return; e.preventDefault(); const merge = mergeFor(selected.row, selected.column);
        choose(e.key === 'ArrowUp' ? (merge?.top ?? selected.row) - 1 : e.key === 'ArrowDown' ? (merge?.bottom ?? selected.row) + 1 : selected.row,
          e.key === 'ArrowLeft' ? (merge?.left ?? selected.column) - 1 : e.key === 'ArrowRight' ? (merge?.right ?? selected.column) + 1 : selected.column, true); }}>
      <div style={{ position: 'relative', width: (ROW_HEADER + geometry.columns[sheet.columns]) * zoom, height: (COLUMN_HEADER + geometry.height) * zoom }}>
        <div className="office-sheet__plane" style={{ width: ROW_HEADER + geometry.columns[sheet.columns], height: COLUMN_HEADER + geometry.height, transform: `scale(${zoom})` }}>
          {visibleRows.flatMap(row => visibleColumns.map(column => merges.some(m => m.top <= row && row <= m.bottom && m.left <= column && column <= m.right) ? null : cellAt(row, column)))}
          {merges.map(merge => cellAt(merge.top, merge.left, merge))}
          {visibleColumns.map(column => <div key={column} role="columnheader" className="office-sheet__column-header" style={{ left: ROW_HEADER + geometry.columns[column - 1], top, width: sheet.columnWidths[column - 1], height: COLUMN_HEADER }}>{columnLabel(column)}</div>)}
          {visibleRows.map(row => <div key={row} role="rowheader" className="office-sheet__row-header" style={{ left, top: COLUMN_HEADER + geometry.rowTop(row), width: ROW_HEADER, height: geometry.rowHeight(row) }}>{row}</div>)}
          <div className="office-sheet__corner" style={{ left, top, width: ROW_HEADER, height: COLUMN_HEADER }} />
        </div>
      </div>
    </div>
    <span className="office-sheet__selection" role="status">{columnLabel(selected.column)}{selected.row}{current?.formula ? ` · 公式：=${current.formula}` : ''}</span>
  </div>;
}
