import type { ZipReader } from './doc-preview';
import { normalizeDocument, type DocumentNode } from './document-model';

export interface ImportedDocument { document: DocumentNode; limitations: string[] }
/** Read structure rather than flattening table cells into unrelated paragraphs. */
export function importDocxDocument(zip: ZipReader): ImportedDocument | null {
  const source = zip.readText('word/document.xml');
  if (!source) return null;
  const xml = new DOMParser().parseFromString(source, 'application/xml');
  if (xml.getElementsByTagName('parsererror').length) throw new Error('Word 文档 XML 无法读取');
  const NS = 'http://schemas.openxmlformats.org/wordprocessingml/2006/main';
  const elements = (node: Element, name?: string) => Array.from(node.children).filter(child => !name || child.localName === name);
  const descendant = (node: Element, name: string) => node.getElementsByTagNameNS(NS, name)[0] ?? Array.from(node.getElementsByTagName('*')).find(child => child.localName === name);
  const val = (node?: Element) => node?.getAttributeNS(NS, 'val') ?? node?.getAttribute('w:val') ?? '';
  const limitations = new Set<string>();
  for (const [tag, label] of [['drawing','图片'],['pict','图片'],['object','嵌入对象'],['vMerge','跨行合并单元格'],['ins','修订标记'],['del','修订标记'],['fldChar','动态域'],['footnoteReference','脚注'],['endnoteReference','尾注']] as const) if (descendant(xml.documentElement, tag)) limitations.add(label);
  if (zip.listEntries().some(path => /^word\/(header|footer)\d*\.xml$/.test(path))) limitations.add('页眉页脚');
  for (const [tag, label] of [['numPr','原生编号列表'],['hyperlink','超链接'],['fldSimple','动态域'],['altChunk','外部嵌入内容']] as const) if (descendant(xml.documentElement,tag)) limitations.add(label);
  if (Array.from(xml.getElementsByTagName('*')).some(node => ['oMath','oMathPara'].includes(node.localName))) limitations.add('公式');
  function inline(node: Element, inherited: DocumentNode['marks'] = []): DocumentNode[] {
    if (node.localName === 't') return node.textContent ? [{type:'text', text:node.textContent, ...(inherited?.length ? {marks:inherited} : {})}] : [];
    if (node.localName === 'br' || node.localName === 'cr') return [{type:'hardBreak'}];
    if (node.localName === 'tab') return [{type:'text', text:'\t'}];
    let marks = inherited;
    if (node.localName === 'r') {
      const properties = elements(node,'rPr')[0];
      const found = properties ? elements(properties).flatMap(property => {
        const type = ({b:'bold',i:'italic',u:'underline',strike:'strike'} as Record<string,string>)[property.localName];
        return type && !['0','false','none'].includes(val(property)) ? [{type}] : [];
      }) : [];
      marks = [...(inherited ?? []), ...found];
    }
    return elements(node).filter(child => !['rPr','pPr','del'].includes(child.localName)).flatMap(child => inline(child,marks));
  }
  function blocks(parent: Element): DocumentNode[] {
    return elements(parent).flatMap(node => {
      if (node.localName === 'p') {
        const style = val(descendant(elements(node,'pPr')[0] ?? node,'pStyle'));
        const heading = /(?:heading|标题)\s*([1-3])/i.exec(style);
        const align = val(descendant(elements(node,'pPr')[0] ?? node,'jc'));
        const content = inline(node);
        return [{type:heading?'heading':'paragraph', attrs:{...(heading?{level:Number(heading[1])}:{}), ...(['left','center','right','both'].includes(align)?{textAlign:align==='both'?'justify':align}:{})}, content}];
      }
      if (node.localName === 'tbl') {
        const rows = elements(node,'tr').map(row => ({type:'tableRow',content:elements(row,'tc').map(cell => ({type:'tableCell',attrs:{colspan:Number(val(descendant(cell,'gridSpan'))) || 1,rowspan:1},content:blocks(cell)}))}));
        if (rows.length) return [{type:'table',content:rows}];
      }
      if (node.localName === 'sdt') { const child=elements(node,'sdtContent')[0]; return child?blocks(child):[]; }
      return [];
    });
  }
  const body = descendant(xml.documentElement,'body');
  if (!body) return null;
  return {document:normalizeDocument({type:'doc',content:blocks(body)}),limitations:[...limitations]};
}
