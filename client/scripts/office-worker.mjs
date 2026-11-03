import { readFile, writeFile } from "node:fs/promises";
import { marked } from "marked";
import {
  Document, HeadingLevel, Packer, Paragraph, Table, TableCell, TableRow, TextRun,
  WidthType, AlignmentType,
} from "docx";
import ExcelJS from "exceljs";
import pptxgen from "pptxgenjs";
import { PDFDocument, rgb } from "pdf-lib";
import fontkit from "@pdf-lib/fontkit";
import { createStructuredWorkbook, createStructuredPresentation } from "./office-structured.mjs";
import { renderDocument, documentBlocks } from "./office-document.mjs";
import { markdownToDocument, normalizeDocument } from "../src/lib/document-model.ts";

const [outputPath, fontPath] = process.argv.slice(2);
if (!outputPath || !fontPath) throw new Error("Missing output or font path");

let input = "";
for await (const chunk of process.stdin) input += chunk;
const request = JSON.parse(input);
const { title, markdown = "", format } = request;
if (typeof title !== "string" || typeof markdown !== "string") throw new Error("Invalid document request");
if (!["docx", "pdf", "xlsx", "pptx"].includes(format)) throw new Error("Unsupported document format");

// Protect mathematical source before Markdown consumes HTML-looking terms or
// backslash escapes. These tokens preserve text; Office does not render TeX.
marked.use({ extensions: [
  {
    name: "math_source", level: "block",
    start(source) { return source.search(/^ {0,3}(?:\${2,}|\\\[)[ \t]*(?:\r?\n|$)/m); },
    tokenizer(source) {
      const opening = /^ {0,3}(\${2,}|\\\[)[ \t]*\r?\n/.exec(source);
      if (!opening) return;
      const remainder = source.slice(opening[0].length);
      const delimiter = opening[1] === "\\[" ? "\\]" : opening[1];
      for (const closing of remainder.matchAll(/^ {0,3}(\${2,}|\\\])[ \t]*(?:\r?\n|$)/gm)) {
        if (closing[1] !== delimiter) continue;
        const raw = source.slice(0, opening[0].length + closing.index + closing[0].length);
        return { type: "math_source", raw, text: raw };
      }
    },
  },
  {
    name: "math_source", level: "inline",
    start(source) {
      const positions = [source.indexOf("$"), source.search(/\\(?:\(|\[)/)].filter((index) => index >= 0);
      return positions.length ? Math.min(...positions) : -1;
    },
    tokenizer(source) {
      const bracket = /^\\(?:\(|\[)/.exec(source)?.[0];
      if (bracket) {
        const closing = source.indexOf(bracket === "\\(" ? "\\)" : "\\]", bracket.length);
        if (closing < 0) return;
        const raw = source.slice(0, closing + 2);
        return { type: "math_source", raw, text: raw };
      }
      const opening = /^\$+/.exec(source)?.[0];
      if (!opening) return;
      for (let index = opening.length; index < source.length; index++) {
        if (source[index] !== "$") continue;
        const closing = /^\$+/.exec(source.slice(index))[0];
        let backslashes = 0;
        for (let previous = index - 1; previous >= 0 && source[previous] === "\\"; previous--) backslashes++;
        if (closing === opening && backslashes % 2 === 0
            && source.slice(opening.length, index).trim()) {
          const raw = source.slice(0, index + closing.length);
          return { type: "math_source", raw, text: raw };
        }
        index += closing.length - 1;
      }
    },
  },
] });
const tokens = marked.lexer(markdown, { ...marked.defaults, gfm: true });

function plain(value) {
  if (Array.isArray(value)) return value.map(plain).join("");
  if (!value || typeof value !== "object") return String(value ?? "");
  if (value.type === "math_source") return value.raw;
  if (value.type === "br") return "\n";
  if (value.type === "image") return value.text || "[图片]";
  if (value.type === "link") return plain(value.tokens ?? value.text);
  // Only HTML tokens contain markup. Comparisons and inline code can contain
  // angle brackets as ordinary text, and must survive all Office formats.
  if (value.type === "html") return String(value.text ?? "").replace(/<[^>]+>/g, "");
  return value.tokens ? plain(value.tokens) : String(value.text ?? "");
}

function blocks(items = tokens, depth = 0) {
  const out = [];
  for (const token of items) {
    if (token.type === "space") continue;
    if (token.type === "heading") out.push({ type: "heading", level: token.depth, text: plain(token.tokens) });
    else if (token.type === "math_source") out.push({ type: "paragraph", text: plain(token) });
    else if (token.type === "paragraph" || token.type === "text") out.push({ type: "paragraph", text: plain(token.tokens ?? token.text) });
    else if (token.type === "code") out.push({ type: "code", text: token.text });
    else if (token.type === "blockquote") out.push(...blocks(token.tokens, depth).map((block) => ({ ...block, quote: true })));
    else if (token.type === "list") {
      token.items.forEach((item, index) => {
        const itemTokens = item.tokens ?? [{ type: "text", text: item.text }];
        let first = true;
        // Preserve source order when a nested list sits between paragraphs.
        // List tokens have no text property and must be traversed recursively.
        for (const part of itemTokens) {
          if (part.type === "space") continue;
          if (part.type === "list") {
            out.push(...blocks([part], depth + 1));
          } else if (part.type === "text" || part.type === "paragraph") {
            out.push({
              type: "list", text: plain(part.tokens ?? part.text), depth,
              prefix: first ? (token.ordered ? `${Number(token.start || 1) + index}.` : "•") : "",
            });
            first = false;
          } else {
            out.push(...blocks([part], depth + 1));
          }
        }
      });
    } else if (token.type === "table") out.push({
      type: "table",
      rows: [token.header, ...token.rows].map((row) => row.map((cell) => plain(cell.tokens ?? cell.text))),
    });
    else if (token.type === "hr") out.push({ type: "paragraph", text: "────────────────────" });
  }
  return out;
}

const editableDocument = request.document ? normalizeDocument(request.document) : format === 'docx' ? markdownToDocument(markdown) : null;
const content = request.document ? documentBlocks(editableDocument) : blocks();

function docxTextRuns(value, options = {}) {
  return String(value).split("\n").map((line, index) => new TextRun({
    ...options, text: line, ...(index > 0 ? { break: 1 } : {}),
  }));
}

async function createDocx() {
  const children = [];
  if (!content.some((block) => block.type === "heading" && block.level === 1)) {
    children.push(new Paragraph({ text: title, heading: HeadingLevel.TITLE, spacing: { after: 300 } }));
  }
  for (const block of content) {
    if (block.type === "table") {
      children.push(new Table({
        width: { size: 100, type: WidthType.PERCENTAGE },
        rows: block.rows.map((row, index) => new TableRow({
          tableHeader: index === 0,
          children: row.map((cell) => new TableCell({
            children: [new Paragraph({
              children: docxTextRuns(cell, { bold: index === 0 }),
              spacing: { before: 80, after: 80 },
            })],
          })),
        })),
      }));
      continue;
    }
    const heading = block.type === "heading"
      ? [HeadingLevel.HEADING_1, HeadingLevel.HEADING_2, HeadingLevel.HEADING_3][Math.min(block.level, 3) - 1]
      : undefined;
    children.push(new Paragraph({
      heading,
      children: docxTextRuns(`${block.prefix ? `${block.prefix} ` : ""}${block.text}`, { italics: Boolean(block.quote) }),
      spacing: { after: block.type === "heading" ? 180 : 100, line: 330 },
      indent: block.type === "list" || block.quote ? { left: 420 * ((block.depth ?? 0) + 1) } : undefined,
      alignment: block.type === "code" ? AlignmentType.LEFT : undefined,
    }));
  }
  const document = new Document({
    creator: "EchoAgent",
    title,
    styles: { default: { document: { run: { font: "Noto Sans CJK SC", size: 22 } } } },
    sections: [{ properties: { page: { margin: { top: 1150, bottom: 1150, left: 1200, right: 1200 } } }, children }],
  });
  await writeFile(outputPath, await Packer.toBuffer(document));
}

async function createPdf() {
  const document = await PDFDocument.create();
  document.registerFontkit(fontkit);
  // fontkit's CFF subsetting can produce malformed glyph streams for this
  // OpenType CJK font. Full embedding keeps Chinese text renderable offline.
  // Full-font ToUnicode maps cover cmap glyphs, not fontkit's localized
  // alternates. Keep those direct glyphs so copying currency digits and
  // punctuation yields the original Unicode text as well as readable pixels.
  const font = await document.embedFont(await readFile(fontPath), { features: { locl: false } });
  document.setTitle(title);
  document.setCreator("EchoAgent");
  const pageWidth = 595.28;
  const pageHeight = 841.89;
  const left = 56;
  const right = pageWidth - 56;
  let page = document.addPage([pageWidth, pageHeight]);
  let y = pageHeight - 62;
  const ink = rgb(0.13, 0.17, 0.22);
  const muted = rgb(0.36, 0.42, 0.48);
  const wrap = (value, size, maxWidth) => {
    const lines = [];
    for (const source of String(value).split("\n")) {
      let line = "";
      for (const character of source) {
        if (line && font.widthOfTextAtSize(line + character, size) > maxWidth) {
          lines.push(line);
          line = character;
        } else line += character;
      }
      lines.push(line);
    }
    return lines;
  };
  const nextPage = () => {
    page = document.addPage([pageWidth, pageHeight]);
    y = pageHeight - 62;
  };
  const draw = (value, size = 10.5, indent = 0, color = ink, after = 8) => {
    const lineHeight = size * 1.62;
    for (const line of wrap(value, size, right - left - indent)) {
      if (y < 64 + lineHeight) nextPage();
      if (line) page.drawText(line, { x: left + indent, y, size, font, color });
      y -= lineHeight;
    }
    y -= after;
  };
  const drawTable = (rows) => {
    const span = cell => typeof cell === 'object' && cell !== null ? cell.colspan ?? 1 : 1;
    const columns = Math.max(1, ...rows.map(row => row.reduce((width, cell) => width + span(cell), 0)));
    const columnWidth = (right - left) / columns;
    const lineHeight = 13.5;
    for (const [rowIndex, row] of rows.entries()) {
      let column=0;
      const cells = row.map(cell => {
        const width=span(cell), start=column;column+=width;
        const text=typeof cell==='object' && cell!==null ? cell.text : cell;
        return {start,lines:wrap(text ?? '',9,columnWidth*width-16)};
      });
      const maxLines = Math.max(1, ...cells.map(cell => cell.lines.length));
      let offset = 0;
      while (offset < maxLines) {
        const available = Math.floor((y - 66 - 16) / lineHeight);
        if (available < 1) nextPage();
        const linesThisPage = Math.min(maxLines - offset, Math.max(1, Math.floor((y - 66 - 16) / lineHeight)));
        const height = linesThisPage * lineHeight + 16;
        const top = y + 5;
        if (rowIndex === 0) {
          page.drawRectangle({ x: left, y: top - height, width: right - left, height,
            color: rgb(0.90, 0.94, 0.97) });
        }
        page.drawLine({ start: { x: left, y: top }, end: { x: right, y: top },
          thickness: 0.6, color: rgb(0.75, 0.80, 0.84) });
        page.drawLine({ start: { x: left, y: top - height }, end: { x: right, y: top - height },
          thickness: 0.6, color: rgb(0.75, 0.80, 0.84) });
        for (const boundary of new Set([0,...cells.map(cell=>cell.start),column,columns])) {
          const x = left + boundary * columnWidth;
          page.drawLine({ start: { x, y: top }, end: { x, y: top - height },
            thickness: 0.6, color: rgb(0.75, 0.80, 0.84) });
        }
        cells.forEach(({lines,start}) => {
          lines.slice(offset, offset + linesThisPage).forEach((line, index) => {
            if (line) page.drawText(line, { x: left + start * columnWidth + 8,
              y: top - 14 - index * lineHeight, size: 9, font, color: ink });
          });
        });
        y = top - height - 5;
        offset += linesThisPage;
        if (offset < maxLines) nextPage();
      }
    }
    y -= 12;
  };
  if (!content.some((block) => block.type === "heading" && block.level === 1)) draw(title, 20, 0, ink, 20);
  for (const block of content) {
    if (block.type === "table") {
      drawTable(block.rows);
    } else if (block.type === "heading") {
      y -= block.level === 1 ? 9 : 5;
      draw(block.text, block.level === 1 ? 19 : block.level === 2 ? 14 : 12, 0, ink, 12);
    } else draw(`${block.prefix ? `${block.prefix} ` : ""}${block.text}`, block.type === "code" ? 9 : 10.5, block.quote || block.type === "list" ? 14 * ((block.depth ?? 0) + 1) : 0, block.quote ? muted : ink);
  }
  const pages = document.getPages();
  pages.forEach((item, index) => item.drawText(`${index + 1} / ${pages.length}`, {
    x: pageWidth - 82, y: 31, font, size: 8, color: muted,
  }));
  await writeFile(outputPath, await document.save());
}

async function createXlsx() {
  const book = new ExcelJS.Workbook();
  book.creator = "EchoAgent";
  const cellValue = (value) => {
    const text = String(value);
    const trimmed = text.trim();
    // Keep IDs with leading zeroes and long identifiers as text. Safe ordinary
    // numbers become numeric cells so users can sum/filter them in Excel.
    if (/^-?(?:0|[1-9]\d{0,14}|[1-9]\d{0,2}(?:,\d{3})+)(?:\.\d+)?$/.test(trimmed)) {
      const number = Number(trimmed.replace(/,/g, ""));
      if (Number.isFinite(number) && Math.abs(number) < 1e15) return number;
    }
    return text;
  };
  let section = title;
  const tables = [];
  for (const block of content) {
    if (block.type === "heading" && block.level <= 2) section = block.text;
    if (block.type === "table") tables.push({ rows: block.rows, section });
  }
  if (tables.length === 0) {
    const sheet = book.addWorksheet("内容");
    sheet.addRow([title]);
    for (const block of content) sheet.addRow([block.text ?? ""]);
    sheet.getColumn(1).width = 72;
  } else {
    tables.forEach((table, index) => {
      const label = (table.section || "表格").replace(/[\\/\[\]:*?]/g, "-").slice(0, 24);
      const sheet = book.addWorksheet(`${label}-${index + 1}`);
      for (const row of table.rows) sheet.addRow(row.map(cellValue));
      sheet.getRow(1).font = { bold: true, color: { argb: "FFFFFFFF" } };
      sheet.getRow(1).fill = { type: "pattern", pattern: "solid", fgColor: { argb: "FF22577A" } };
      sheet.columns.forEach((column) => { column.width = 22; });
      sheet.views = [{ state: "frozen", ySplit: 1 }];
      sheet.autoFilter = { from: { row: 1, column: 1 }, to: { row: 1, column: table.rows[0]?.length || 1 } };
    });
  }
  await book.xlsx.writeFile(outputPath);
}

async function createPptx() {
  const deck = new pptxgen();
  deck.layout = "LAYOUT_WIDE";
  deck.author = "EchoAgent";
  deck.subject = title;
  deck.title = title;
  deck.lang = "zh-CN";
  deck.theme = { headFontFace: "Microsoft YaHei", bodyFontFace: "Microsoft YaHei", lang: "zh-CN" };
  let slide = deck.addSlide();
  slide.background = { color: "F7FAFC" };
  slide.addText(title, { x: 0.8, y: 2.4, w: 11.6, h: 1.2, fontSize: 32, bold: true, color: "16324F", breakLine: false });
  let sectionTitle = "";
  let lines = [];
  const flush = () => {
    if (!lines.length) return;
    slide = deck.addSlide();
    slide.background = { color: "FFFFFF" };
    slide.addShape(deck.ShapeType.rect, { x: 0, y: 0, w: 0.18, h: 7.5, line: { color: "2F7C92" }, fill: { color: "2F7C92" } });
    slide.addText(sectionTitle || title, { x: 0.65, y: 0.45, w: 12, h: 0.7, fontSize: 25, bold: true, color: "16324F" });
    slide.addText(lines.join("\n"), { x: 0.72, y: 1.45, w: 11.9, h: 5.35, fontSize: 18, color: "263746", breakLine: false, valign: "top", margin: 0.08 });
    lines = [];
  };
  const visualLines = (source) => {
    const wrapped = [];
    let part = "";
    let units = 0;
    for (const character of source) {
      const width = /[\u2E80-\u9FFF\uAC00-\uD7AF]/u.test(character) ? 2 : 1;
      if (part && units + width > 78) {
        wrapped.push(part);
        part = "";
        units = 0;
      }
      part += character;
      units += width;
    }
    wrapped.push(part);
    return wrapped;
  };
  for (const block of content) {
    if (block.type === "heading" && block.level <= 2) {
      flush();
      sectionTitle = block.text;
      continue;
    }
    const text = block.type === "table" ? block.rows.map((row) => row.join("  |  ")).join("\n") : `${"  ".repeat(block.depth ?? 0)}${block.prefix ? `${block.prefix} ` : ""}${block.text ?? ""}`;
    for (const line of text.split("\n")) {
      for (const visualLine of visualLines(line)) {
        if (lines.length >= 8) flush();
        lines.push(visualLine);
      }
    }
  }
  flush();
  await deck.writeFile({ fileName: outputPath });
}

if (format === "xlsx" && request.workbook) await createStructuredWorkbook(request.workbook, outputPath);
else if (format === "pptx" && request.slides) await createStructuredPresentation(title, request.slides, outputPath);
else if (format === "docx") {
  if (!request.document && !editableDocument.content.some(node => node.type === "heading" && node.attrs?.level === 1)) editableDocument.content.unshift({type:"heading", attrs:{level:1}, content:[{type:"text",text:title}]});
  await renderDocument(editableDocument, title, outputPath);
}
else if (format === "pdf") await createPdf();
else if (format === "xlsx") await createXlsx();
else await createPptx();
