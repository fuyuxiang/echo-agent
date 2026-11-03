import { describe, expect, it } from 'vitest';
import { documentText, documentToMarkdown, markdownToDocument, markdownDocumentLimitations, normalizeDocument } from '../document-model';
import { importDocxDocument } from '../docx-document';

const xml='<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>家庭作息表</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>时间</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>儿童安排</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>6:30</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>起床</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>';
describe('editable document model',()=>{
  it('preserves headings, formatted text and real table cells',()=>{
    const doc=markdownToDocument('# 作息表\n\n**儿童**和*家长*\n\n| 时间 | 安排 |\n| --- | --- |\n| 6:30 | 起床 |');
    expect(doc.content?.[0]).toMatchObject({type:'heading',attrs:{level:1}});
    expect(doc.content?.[1].content?.[0]).toMatchObject({text:'儿童',marks:[{type:'bold'}]});
    expect(doc.content?.[2].content?.[1].content?.[0].content?.[0].content?.[0].text).toBe('6:30');
    expect(documentText(doc)).toContain('6:30\t起床');
    expect(documentToMarkdown(doc)).toContain('| 6:30 | 起床 |');
  });
  it('keeps nested list order, code and empty paragraphs',()=>{
    const doc=markdownToDocument('- 第一条\n  - 子项\n- 第二条\n\n```text\na < b\n```');
    expect(documentText(doc)).toContain('第一条\n子项\n第二条');
    expect(documentText(doc)).toContain('a < b');
    expect(normalizeDocument({type:'doc',content:[]})).toEqual({type:'doc',content:[{type:'paragraph'}]});
  });
  it('rejects executable nodes, links and oversized or ragged tables',()=>{
    expect(()=>normalizeDocument({type:'doc',content:[{type:'script'}]})).toThrow();
    expect(()=>normalizeDocument({type:'doc',content:[{type:'paragraph',content:[{type:'text',text:'x',marks:[{type:'link',attrs:{href:'javascript:alert(1)'}}]}]}]})).toThrow('链接');
    expect(()=>normalizeDocument({type:'doc',content:[{type:'table',content:[{type:'tableRow',content:[{type:'tableCell'}]},{type:'tableRow',content:[{type:'tableCell'},{type:'tableCell'}]}]}]})).toThrow('列数');
  });
  it('reads DOCX table structure and inline formatting rather than flattening columns',()=>{
    const imported=importDocxDocument({readText:()=>xml,listEntries:()=>['word/document.xml']})!;
    expect(imported.limitations).toEqual([]);
    expect(imported.document.content?.[0].type).toBe('heading');
    const table=imported.document.content?.[1];
    expect(table?.type).toBe('table');
    expect(table?.content).toHaveLength(2);
    expect(table?.content?.[0].content?.[0].content?.[0].content?.[0]).toMatchObject({text:'时间',marks:[{type:'bold'}]});
    expect(documentText(table!)).toContain('6:30\t起床');
  });
  it('reports unsupported imported content so conversion cannot silently discard it',()=>{
    const imported=importDocxDocument({readText:()=>xml.replace('</w:body>','<w:p><w:r><w:drawing/></w:r></w:p></w:body>'),listEntries:()=>['word/document.xml','word/header1.xml']})!;
    expect(imported.limitations).toEqual(expect.arrayContaining(['图片','页眉页脚']));
  });
  it('keeps local/anchor destinations readable without making them executable hyperlinks',()=>{
    const doc=markdownToDocument('[结论](#结论) [附件](EchoAgent成果/report.docx) [网站](https://example.com) [危险](javascript:alert)');
    expect(documentText(doc)).toBe('结论（#结论） 附件（EchoAgent成果/report.docx） 网站 危险');
    const links=doc.content?.flatMap(node=>node.content ?? []).flatMap(node=>node.marks ?? []).filter(mark=>mark.type==='link');
    expect(links).toHaveLength(1);expect(links?.[0].attrs?.href).toBe('https://example.com');
  });
  it('reports lossy Markdown features while permitting ordinary content and literal code',()=>{
    expect(markdownDocumentLimitations('# 标题\n\n**正文**\n\n| 列 |\n| --- |\n| 值 |\n\n`$price$`\n\n```text\n![literal](x)\n```')).toEqual([]);
    expect(markdownDocumentLimitations('![架构](https://example.com/a.png)\n\n```mermaid\ngraph LR; A-->B\n```\n\n公式 \\(x+1\\)\n\n<div>内容</div>\n\n[结论](#结论)\n\n- [x] 已完成')).toEqual(expect.arrayContaining(['图片','Mermaid 图表','数学公式','HTML 内容','相对路径、页内或不支持的链接','任务复选框']));
  });
  it('keeps paragraph boundaries within table cells',()=>{
    expect(documentText({type:'tableCell',content:[{type:'paragraph',content:[{type:'text',text:'第一段'}]},{type:'paragraph',content:[{type:'text',text:'第二段'}]}]})).toBe('第一段\n第二段');
  });
});
