import { marked } from "marked";

export interface DocumentNode {
  type: string;
  text?: string;
  attrs?: Record<string, unknown>;
  marks?: { type: string; attrs?: Record<string, unknown> }[];
  content?: DocumentNode[];
}
export const EMPTY_DOCUMENT: DocumentNode = { type: "doc", content: [{ type: "paragraph" }] };
const BLOCKS = new Set(["paragraph", "heading", "bulletList", "orderedList", "blockquote", "codeBlock", "horizontalRule", "table"]);
const INLINE = new Set(["text", "hardBreak"]);
const MARKS = new Set(["bold", "italic", "underline", "strike", "code", "link"]);
const documentLink = (href: string) => /^(https?:\/\/|mailto:)/i.test(href) && href.length <= 2048;
export function normalizeDocument(value: unknown): DocumentNode {
  let count = 0, size = 0;
  function visit(raw: unknown, parent: string, depth: number): DocumentNode {
    if (!raw || typeof raw !== "object" || depth > 32 || ++count > 20000) throw new Error("文档结构无效或超出编辑范围");
    const input = raw as DocumentNode;
    const type = input.type;
    const allowed = parent === "root" ? type === "doc"
      : ["paragraph", "heading", "codeBlock"].includes(parent) ? INLINE.has(type)
        : ["bulletList", "orderedList"].includes(parent) ? type === "listItem"
          : parent === "table" ? type === "tableRow"
            : parent === "tableRow" ? type === "tableCell" || type === "tableHeader"
              : BLOCKS.has(type);
    if (!allowed) throw new Error(`不支持的文档节点：${type}`);
    const node: DocumentNode = { type };
    const attrs = input.attrs ?? {};
    if (type === "heading") node.attrs = { level: Math.min(3, Math.max(1, Number(attrs.level) || 1)) };
    if (["paragraph", "heading"].includes(type) && ["left", "center", "right", "justify"].includes(String(attrs.textAlign))) node.attrs = { ...node.attrs, textAlign: attrs.textAlign };
    if (type === "orderedList") node.attrs = { start: Math.max(1, Math.min(100000, Number(attrs.start) || 1)) };
    if (type === "tableCell" || type === "tableHeader") {
      const colspan = Number(attrs.colspan ?? 1), rowspan = Number(attrs.rowspan ?? 1);
      if (!Number.isInteger(colspan) || !Number.isInteger(rowspan) || colspan < 1 || colspan > 32 || rowspan !== 1) throw new Error("暂不支持跨行合并的表格，请保留原件或拆分后编辑");
      node.attrs = { colspan, rowspan: 1, colwidth: null };
    }
    if (type === "text") {
      if (typeof input.text !== "string" || !input.text) throw new Error("文档文字无效");
      size += input.text.length;
      if (size > 1000000) throw new Error("文档内容过长");
      node.text = input.text;
      node.marks = (input.marks ?? []).map(mark => {
        if (!MARKS.has(mark.type)) throw new Error(`不支持的文字格式：${mark.type}`);
        if (mark.type !== "link") return { type: mark.type };
        const href = String(mark.attrs?.href ?? "");
        if (!documentLink(href)) throw new Error("文档链接无效");
        return { type: "link", attrs: { href, target: "_blank", rel: "noopener noreferrer" } };
      });
      if (!node.marks.length) delete node.marks;
    } else if (!["hardBreak", "horizontalRule"].includes(type)) {
      if (input.content !== undefined && !Array.isArray(input.content)) throw new Error("文档子节点无效");
      node.content = (input.content ?? []).map(child => visit(child, type, depth + 1));
      if (["doc", "listItem", "tableCell", "tableHeader", "blockquote"].includes(type) && !node.content.length) node.content = [{ type: "paragraph" }];
      if (["table", "tableRow", "bulletList", "orderedList"].includes(type) && !node.content.length) throw new Error("表格或列表不能为空");
      if (type === "tableRow" && node.content.length > 32) throw new Error("表格最多支持 32 列");
      if (type === "table") {
        if (node.content.length > 1000) throw new Error("表格最多支持 1000 行");
        const widths = node.content.map(row => row.content!.reduce((sum, cell) => sum + Number(cell.attrs?.colspan ?? 1), 0));
        if (widths.some(width => width !== widths[0] || width > 32)) throw new Error("表格各行列数不一致");
      }
    }
    return node;
  }
  return visit(value, "root", 0);
}

type Token = { type: string; text?: string; raw?: string; lang?: string; task?: boolean; depth?: number; start?: number | string; ordered?: boolean; href?: string; align?: (string|null)[]; tokens?: Token[]; items?: Token[]; header?: Token[]; rows?: Token[][] };
function inline(tokens: Token[], marks: DocumentNode["marks"] = []): DocumentNode[] {
  return tokens.flatMap(token => {
    if (token.type === "html") return [];
    if (token.type === "br") return [{ type: "hardBreak" }];
    if (token.type === "link" && !documentLink(token.href ?? "")) {
      const label=inline(token.tokens ?? [{type:'text',text:token.text}],marks);
      // Local destinations and anchors are useful information in an exported
      // document, but must never become browser/file-protocol hyperlinks.
      const href=token.href ?? '';
      return !/^[a-z][a-z\d+.-]*:/i.test(href) && href && href.length<=2048
        ? [...label,{type:'text',text:`（${href}）`,...(marks?.length?{marks}:{})}]
        : label;
    }
    const mark = ({ strong: "bold", em: "italic", del: "strike", codespan: "code", link: "link" } as Record<string, string>)[token.type];
    const nextMarks = mark ? [...marks!, { type: mark, ...(mark === "link" ? { attrs: { href: token.href } } : {}) }] : marks;
    if (token.tokens) return inline(token.tokens, nextMarks);
    const text = token.type === "math_source" ? token.raw : token.text;
    return text ? [{ type: "text", text, ...(nextMarks?.length ? { marks: nextMarks } : {}) }] : [];
  });
}
/** Conversion is deliberately narrower than reading. Never present a lossy
 * editable copy as a faithful conversion of the original Markdown. */
export function markdownDocumentLimitations(markdown: string): string[] {
  const limitations=new Set<string>();
  const mathSource=(tokens:Token[]):string=>tokens.map(token=>token.type==='codespan'||token.type==='image'?'':token.tokens?mathSource(token.tokens):token.raw ?? token.text ?? '').join('');
  function visit(tokens: Token[]) {
    for(const token of tokens) {
      if(token.type==='code'){if(/^mermaid\b/i.test(token.lang ?? ''))limitations.add('Mermaid 图表');continue;}
      if(token.type==='codespan')continue;
      if(token.type==='image')limitations.add('图片');
      if(token.type==='html')limitations.add('HTML 内容');
      if(token.type==='heading' && (token.depth ?? 1)>3)limitations.add('四级及以上标题');
      if(token.task)limitations.add('任务复选框');
      if(Array.isArray(token.align) && token.align.some(Boolean))limitations.add('表格对齐');
      if(token.type==='link' && !documentLink(token.href ?? ''))limitations.add('相对路径、页内或不支持的链接');
      if(token.type==='math_source' || ['paragraph','heading','text'].includes(token.type) && /\\\([\s\S]+?\\\)|\\\[[\s\S]+?\\\]|(^|[^\\$])\${1,2}[^$]+\${1,2}/.test(token.tokens?mathSource(token.tokens):token.raw ?? token.text ?? ''))limitations.add('数学公式');
      if(token.tokens)visit(token.tokens);
      if(token.items)visit(token.items);
      if(Array.isArray(token.header))visit(token.header);
      if(token.rows)token.rows.forEach(visit);
    }
  }
  visit(marked.lexer(markdown,{...marked.defaults,gfm:true}) as unknown as Token[]);
  return [...limitations];
}
export function markdownToDocument(markdown: string): DocumentNode {
  const para = (token: Token): DocumentNode => ({ type: "paragraph", content: inline(token.tokens ?? [{ type: "text", text: token.text ?? "" }]) });
  function blocks(tokens: Token[]): DocumentNode[] {
    return tokens.flatMap(token => {
      if (token.type === "space" || token.type === "html") return [];
      if (token.type === "heading") return [{ ...para(token), type: "heading", attrs: { level: Math.min(3, token.depth ?? 1) } }];
      if (token.type === "hr") return [{ type: "horizontalRule" }];
      if (token.type === "code") return [{ type: "codeBlock", content: token.text ? [{ type: "text", text: token.text }] : [] }];
      if (token.type === "blockquote") return [{ type: "blockquote", content: blocks(token.tokens ?? []) }];
      if (token.type === "list") return [{ type: token.ordered ? "orderedList" : "bulletList", attrs: { start: Number(token.start) || 1 }, content: (token.items ?? []).map(item => ({ type: "listItem", content: blocks(item.tokens ?? []) })) }];
      if (token.type === "table") return [{ type: "table", content: [token.header ?? [], ...(token.rows ?? [])].map((row, index) => ({ type: "tableRow", content: row.map(cell => ({ type: index ? "tableCell" : "tableHeader", content: [para(cell)] })) })) }];
      if (token.type === "math_source") return [para({ type: "text", text: token.raw })];
      return [para(token)];
    });
  }
  return normalizeDocument({ type: "doc", content: blocks(marked.lexer(markdown, { ...marked.defaults, gfm: true }) as unknown as Token[]) });
}
export function documentText(node: DocumentNode): string {
  if (node.type === "text") return node.text ?? "";
  if (node.type === "hardBreak") return "\n";
  const separator = ["doc", "table", "tableCell", "tableHeader", "bulletList", "orderedList", "blockquote", "listItem"].includes(node.type) ? "\n" : node.type === "tableRow" ? "\t" : "";
  return (node.content ?? []).map(documentText).join(separator);
}
export function documentToMarkdown(doc: DocumentNode): string {
  function serialize(node: DocumentNode): string {
    if (node.type === "text") {
      let text = (node.text ?? "").replace(/([\\`*_[\]|])/g, "\\$1");
      for (const mark of node.marks ?? []) {
        if (mark.type === "bold") text = `**${text}**`;
        if (mark.type === "italic") text = `*${text}*`;
        if (mark.type === "strike") text = `~~${text}~~`;
        if (mark.type === "code") text = `\`${node.text}\``;
        if (mark.type === "link") text = `[${text}](${mark.attrs?.href})`;
      }
      return text;
    }
    if (node.type === "hardBreak") return "<br>";
    const children = node.content ?? [];
    const text = children.map(serialize).join("");
    if (node.type === "heading") return `${"#".repeat(Number(node.attrs?.level) || 1)} ${text}\n\n`;
    if (node.type === "paragraph") return text + "\n\n";
    if (node.type === "horizontalRule") return "---\n\n";
    if (node.type === "codeBlock") return `\`\`\`\n${documentText(node)}\n\`\`\`\n\n`;
    if (node.type === "blockquote") return text.trim().split("\n").map(line => `> ${line}`).join("\n") + "\n\n";
    if (["bulletList", "orderedList"].includes(node.type)) return children.map((item, index) => `${node.type === "bulletList" ? "-" : `${Number(node.attrs?.start ?? 1) + index}.`} ${serialize(item).trim().replace(/\n/g, "\n  ")}`).join("\n") + "\n\n";
    if (node.type === "table") {
      const rows = children.map(row => `| ${(row.content ?? []).map(cell => (cell.content ?? []).map(serialize).join("").trim().replace(/\n+/g, "<br>")).join(" | ")} |`);
      rows.splice(1, 0, `| ${(children[0]?.content ?? []).map(() => "---").join(" | ")} |`);
      return rows.join("\n") + "\n\n";
    }
    return text;
  }
  return serialize(doc).trim();
}
