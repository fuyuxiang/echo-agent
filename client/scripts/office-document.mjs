import { Document, Paragraph, TextRun, Table, TableRow, TableCell, Packer, HeadingLevel, WidthType, AlignmentType, ExternalHyperlink, UnderlineType } from 'docx';
import { writeFile } from 'node:fs/promises';
import { normalizeDocument, documentText } from '../src/lib/document-model.ts';

export async function renderDocument(model, title, output) {
  const document = normalizeDocument(model);
  function runs(node, extra = {}) {
    if (node.type === 'hardBreak') return [new TextRun({ break: 1 })];
    if (node.type !== 'text') return (node.content ?? []).flatMap(child => runs(child, extra));
    const marks = node.marks ?? [];
    const options = { ...extra, bold: marks.some(m => m.type === 'bold') || extra.bold,
      italics: marks.some(m => m.type === 'italic'), strike: marks.some(m => m.type === 'strike'),
      underline: marks.some(m => m.type === 'underline') ? { type: UnderlineType.SINGLE } : undefined,
      font: marks.some(m => m.type === 'code') ? 'Consolas' : undefined };
    const children = node.text.split('\n').map((text, index) => new TextRun({ ...options, text, ...(index ? { break: 1 } : {}) }));
    const link = marks.find(m => m.type === 'link');
    return link ? [new ExternalHyperlink({ link: link.attrs.href, children })] : children;
  }
  function blocks(nodes, depth = 0, prefix = '') {
    return nodes.flatMap((node, index) => {
      if (node.type === 'table') return [new Table({ width: { size: 100, type: WidthType.PERCENTAGE },
        rows: node.content.map(row => new TableRow({ tableHeader: row.content.every(cell => cell.type === 'tableHeader'),
          children: row.content.map(cell => new TableCell({ columnSpan: cell.attrs?.colspan ?? 1,
            children: blocks(cell.content.map(child => cell.type === 'tableHeader' ? { ...child, content: (child.content ?? []).map(inline => inline.type === 'text' ? { ...inline, marks: [...(inline.marks ?? []), { type: 'bold' }] } : inline) } : child)) })) })) })];
      if (node.type === 'bulletList' || node.type === 'orderedList') return node.content.flatMap((item, itemIndex) => blocks(item.content, depth + 1, node.type === 'bulletList' ? '• ' : `${Number(node.attrs?.start ?? 1) + itemIndex}. `));
      if (node.type === 'blockquote') return blocks(node.content, depth + 1);
      if (node.type === 'horizontalRule') return [new Paragraph({ text: '────────────────────' })];
      return [new Paragraph({
        heading: node.type === 'heading' ? [HeadingLevel.HEADING_1, HeadingLevel.HEADING_2, HeadingLevel.HEADING_3][Number(node.attrs?.level ?? 1) - 1] : undefined,
        children: [...(prefix && index === 0 ? [new TextRun(prefix)] : []), ...runs(node)],
        indent: depth ? { left: depth * 420 } : undefined,
        alignment: ({ left: AlignmentType.LEFT, center: AlignmentType.CENTER, right: AlignmentType.RIGHT, justify: AlignmentType.JUSTIFIED })[node.attrs?.textAlign],
        spacing: { after: node.type === 'heading' ? 180 : 100, line: 330 },
      })];
    });
  }
  const outputDocument = new Document({ creator: 'EchoAgent', title,
    styles: { default: { document: { run: { font: 'Noto Sans CJK SC', size: 22 } } } },
    sections: [{ properties: { page: { margin: { top: 1150, bottom: 1150, left: 1200, right: 1200 } } }, children: blocks(document.content) }] });
  await writeFile(output, await Packer.toBuffer(outputDocument));
  await writeFile(output + '.document.json', JSON.stringify(document));
}

export function documentBlocks(document) {
  const output = [];
  function visit(nodes, depth = 0, prefix) {
    for (const node of nodes) {
      if (node.type === 'table') output.push({ type: 'table', rows: node.content.map(row => row.content.map(cell => ({text:documentText(cell),colspan:Number(cell.attrs?.colspan ?? 1)}))) });
      else if (node.type === 'bulletList' || node.type === 'orderedList') node.content.forEach((item, index) => visit(item.content, depth + 1, node.type === 'bulletList' ? '•' : `${Number(node.attrs?.start ?? 1) + index}.`));
      else if (node.type === 'blockquote' || node.type === 'listItem') visit(node.content, depth + 1);
      else output.push({ type: node.type === 'heading' ? 'heading' : node.type === 'codeBlock' ? 'code' : 'paragraph', text: documentText(node), level: node.attrs?.level, depth, prefix });
    }
  }
  visit(normalizeDocument(document).content);
  return output;
}
