import { Fragment, type ReactNode } from 'react';
import type { DocumentNode } from '@/lib/document-model';

export function DocumentCanvas({ document }: { document: DocumentNode }) {
  function render(node: DocumentNode, key: number): ReactNode {
    const children = node.content?.map(render);
    const alignment = node.attrs?.textAlign as 'left'|'center'|'right'|'justify'|undefined;
    if (node.type === 'text') {
      let text: ReactNode = node.text;
      for (const [index,mark] of (node.marks ?? []).entries()) {
        if (mark.type === 'bold') text=<strong key={index}>{text}</strong>;
        if (mark.type === 'italic') text=<em key={index}>{text}</em>;
        if (mark.type === 'underline') text=<u key={index}>{text}</u>;
        if (mark.type === 'strike') text=<s key={index}>{text}</s>;
        if (mark.type === 'code') text=<code key={index}>{text}</code>;
        if (mark.type === 'link') text=<a key={index} href={String(mark.attrs?.href)} target="_blank" rel="noopener noreferrer">{text}</a>;
      }
      return <Fragment key={key}>{text}</Fragment>;
    }
    if (node.type === 'hardBreak') return <br key={key}/>;
    if (node.type === 'heading') {const Tag=(`h${node.attrs?.level ?? 1}` as 'h1'|'h2'|'h3');return <Tag key={key} style={{textAlign:alignment}}>{children}</Tag>;}
    if (node.type === 'paragraph') return <p key={key} style={{textAlign:alignment}}>{children?.length?children:<br/>}</p>;
    if (node.type === 'bulletList') return <ul key={key}>{children}</ul>;
    if (node.type === 'orderedList') return <ol key={key} start={Number(node.attrs?.start)||1}>{children}</ol>;
    if (node.type === 'listItem') return <li key={key}>{children}</li>;
    if (node.type === 'blockquote') return <blockquote key={key}>{children}</blockquote>;
    if (node.type === 'codeBlock') return <pre key={key}><code>{children}</code></pre>;
    if (node.type === 'horizontalRule') return <hr key={key}/>;
    if (node.type === 'table') return <div key={key} className="document-table-scroll"><table><tbody>{children}</tbody></table></div>;
    if (node.type === 'tableRow') return <tr key={key}>{children}</tr>;
    if (node.type === 'tableCell' || node.type === 'tableHeader') { const Tag=node.type==='tableHeader'?'th':'td'; return <Tag key={key} colSpan={Number(node.attrs?.colspan)||1}>{children}</Tag>; }
    return <Fragment key={key}>{children}</Fragment>;
  }
  return <div className="document-paper" aria-label="文档内容预览">{document.content?.map(render)}</div>;
}
