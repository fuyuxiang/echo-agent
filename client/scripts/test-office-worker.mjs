import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import ExcelJS from "exceljs";
import { createRequire } from "node:module";
const JSZip = createRequire(import.meta.resolve("exceljs"))("jszip");
import { PDFDocument, PDFName, decodePDFRawStream } from "pdf-lib";

const root = resolve(import.meta.dirname, "..");
const worker = join(root, "src-tauri/resources/office/worker.mjs");
const font = join(root, "src-tauri/resources/office/fonts/NotoSansCJKsc-Regular.otf");
const stagedNode = process.platform === "win32"
  ? join(root, "src-tauri/resources/theia/node/node.exe")
  : join(root, "src-tauri/resources/theia/node/bin/node");
const executable = existsSync(stagedNode) ? stagedNode : process.execPath;
const directory = await mkdtemp(join(tmpdir(), "echo-office-test-"));
const markdown = "# 季度报告\n\n中文段落与 English。\n\n| 项目 | 金额 |\n| --- | ---: |\n| 收入 | 1200 |\n| 公式样本 | =SUM(B2) |\n";

// Read the Worker PDF's actual text operators through its embedded Unicode
// maps. A valid header/page count alone cannot detect silently missing text.
function pdfText(document) {
  const maps = new Map();
  const decode = (stream) => new TextDecoder().decode(decodePDFRawStream(stream).decode());
  const lines = [];
  for (const page of document.getPages()) {
    const fonts = page.node.Resources().lookup(PDFName.of("Font"));
    const contents = page.node.Contents();
    let font;
    for (let index = 0; index < contents.size(); index++) {
      const source = decode(document.context.lookup(contents.get(index)));
      for (const match of source.matchAll(/\/([^\s]+)\s+[\d.]+\s+Tf|<([\da-f]+)>\s*Tj/gi)) {
        if (match[1]) {
          const unicode = fonts.lookup(PDFName.of(match[1])).lookup(PDFName.of("ToUnicode"));
          if (!maps.has(unicode)) {
            const map = new Map();
            for (const section of decode(unicode).matchAll(/beginbfchar([\s\S]*?)endbfchar/g)) {
              for (const pair of section[1].matchAll(/<([\da-f]{4})>\s*<([\da-f]+)>/gi)) {
                map.set(pair[1].toUpperCase(), String.fromCharCode(
                  ...pair[2].match(/.{4}/g).map((code) => parseInt(code, 16)),
                ));
              }
            }
            maps.set(unicode, map);
          }
          font = maps.get(unicode);
        } else {
          assert.ok(font, "PDF text has no Unicode font map");
          lines.push(match[2].match(/.{4}/g).map((code) => {
            assert.ok(font.has(code.toUpperCase()), `PDF text has an unmapped glyph: ${code}`);
            return font.get(code.toUpperCase());
          }).join(""));
        }
      }
    }
  }
  return lines.join("\n");
}

const fidelityRows = [
  ["普通比较", "a < b > c"],
  ["公式源码", "$a < b > c$"],
  ["行内代码", "<tag> &amp; </tag>"],
  ["实体源码", "&lt;angle&gt; &amp;"],
  ["HTML分隔", "左右强调"],
];
const fidelityMarkdown = "# 文本保真\n\n普通比较 a < b > c\n\n公式源码 $a < b > c$\n\n"
  + "行内代码 `<tag> &amp; </tag>`\n\n实体源码 &lt;angle&gt; &amp;\n\nHTML分隔 左<wbr>右<strong>强调</strong>\n\n"
  + "| 项目 | 内容 |\n| --- | --- |\n| 普通比较 | a < b > c |\n| 公式源码 | $a < b > c$ |\n"
  + "| 行内代码 | `<tag> &amp; </tag>` |\n| 实体源码 | &lt;angle&gt; &amp; |\n| HTML分隔 | 左<wbr>右<strong>强调</strong> |\n";
const xmlText = (value) => value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const mathSource = "$$\na<b>c\n\\{x\\}\n$$";
const bracketMathSource = "\\[\na<b>c\n\\{x\\}\n\\]";
const mermaidSource = "graph TD\n  A[甲] --> B[乙]";
const mathMarkdown = "# 源码保真\n\n紧贴公式 $a<b>c$\n\n括号公式 $\\{x\\}$\n\n"
  + `${mathSource}\n\n\`\`\`mermaid\n${mermaidSource}\n\`\`\`\n\n`
  + `TeX分隔 \\(a<b>c\\)\n\n${bracketMathSource}\n\n`
  + "价格 \\$5 and \\$10\n\n"
  + "English office affinity efficient final_file\n\n代码 `final_file efficient_office ffi_helper`\n\n"
  + "转义美元 \\$literal 和行内代码 `$a<b>c$`\n\n```text\n$not-closed\n$$ not a math fence\nfi ff ffi fl final_file\n```\n";
const sourceExpected = [
  "紧贴公式 $a<b>c$", "括号公式 $\\{x\\}$", mathSource,
  "TeX分隔 \\(a<b>c\\)", bracketMathSource,
  "价格 $5 and $10",
  "English office affinity efficient final_file", "代码 final_file efficient_office ffi_helper",
  mermaidSource, "转义美元 $literal 和行内代码 $a<b>c$",
  "$not-closed\n$$ not a math fence\nfi ff ffi fl final_file",
];

function officeXmlText(xml, prefix) {
  const decode = (value) => value.replace(/&lt;/g, "<").replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"').replace(/&apos;/g, "'").replace(/&amp;/g, "&");
  const text = new RegExp(`<${prefix}:t(?:\\s[^>]*)?>([\\s\\S]*?)<\\/${prefix}:t>|<${prefix}:br\\s*\\/?>|<\\/${prefix}:p>`, "g");
  return [...xml.matchAll(text)].map((match) => match[1] === undefined ? "\n" : decode(match[1]))
    .join("").replace(/\r\n?/g, "\n");
}

async function run(format, documentMarkdown = markdown, name = `report.${format}`) {
  const path = join(directory, name);
  const child = spawn(executable, ["--max-old-space-size=512", worker, path, font], { stdio: ["pipe", "ignore", "pipe"] });
  let errorText = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => { errorText += chunk; });
  child.stdin.end(JSON.stringify({ title: "季度报告", markdown: documentMarkdown, format }));
  const code = await new Promise((fulfill, reject) => {
    child.on("error", reject);
    child.on("close", fulfill);
  });
  assert.equal(code, 0, `${format}: ${errorText}`);
  assert.ok((await stat(path)).size > 1000, `${format} output is unexpectedly small`);
  return path;
}

try {
  const docx = await readFile(await run("docx"));
  assert.equal(docx.toString("utf8", 0, 2), "PK");
  const docxZip = await JSZip.loadAsync(docx);
  const documentXml = await docxZip.file("word/document.xml").async("string");
  assert.ok(documentXml.includes("中文段落与 English。") && documentXml.includes("收入"), "DOCX must contain real Chinese text and table data");

  const nestedMarkdown = "# 层级任务\n\n- 父任务\n  - 子任务甲\n    1. 深层任务乙\n- 最后任务\n\n```text\n代码第一行\n代码第二行\n```\n";
  const nestedDocx = await JSZip.loadAsync(await readFile(await run("docx", nestedMarkdown, "nested.docx")));
  const nestedXml = await nestedDocx.file("word/document.xml").async("string");
  for (const text of ["父任务", "子任务甲", "深层任务乙", "最后任务"]) {
    assert.ok(nestedXml.includes(text), `DOCX dropped nested list item: ${text}`);
  }
  assert.ok(nestedXml.includes("<w:br/>") && nestedXml.includes("代码第一行") && nestedXml.includes("代码第二行"), "DOCX code lines must have a real Word line break");
  assert.ok(nestedXml.includes('w:left="840"') && nestedXml.includes('w:left="1260"'), "DOCX must preserve nested list indentation");

  const pdf = await PDFDocument.load(await readFile(await run("pdf")));
  assert.ok(pdf.getPageCount() >= 1);
  assert.ok(pdfText(pdf).includes("中文段落与 English。"), "PDF must contain actual Chinese paragraph text");
  const longTable = `# 多页表格\n\n| 项目 | 金额 |\n| --- | ---: |\n${
    Array.from({ length: 80 }, (_, index) => `| 项目 ${index + 1} | ${index * 10} |`).join("\n")
  }`;
  const pagedPdf = await PDFDocument.load(await readFile(await run("pdf", longTable, "paged.pdf")));
  assert.ok(pagedPdf.getPageCount() >= 2, "long PDF table should span pages");

  const book = new ExcelJS.Workbook();
  await book.xlsx.readFile(await run("xlsx"));
  assert.equal(book.worksheets[0].name, "季度报告-1");
  assert.equal(book.worksheets[0].getRow(2).getCell(1).value, "收入");
  assert.equal(book.worksheets[0].getRow(2).getCell(2).value, 1200);
  assert.equal(book.worksheets[0].getRow(3).getCell(2).value, "=SUM(B2)");

  const pptx = await readFile(await run("pptx"));
  assert.equal(pptx.toString("utf8", 0, 2), "PK");
  const orderedContent = "# 列表顺序\n\n- 父段开始\n\n  - 中间子项\n\n  父段结束\n";
  const orderedDocx = await JSZip.loadAsync(await readFile(await run("docx", orderedContent, "list-order.docx")));
  const orderedXml = await orderedDocx.file("word/document.xml").async("string");
  assert.ok(orderedXml.indexOf("父段开始") < orderedXml.indexOf("中间子项")
    && orderedXml.indexOf("中间子项") < orderedXml.indexOf("父段结束"), "nested lists must retain the order of surrounding paragraphs");

  const nestedDeck = await JSZip.loadAsync(await readFile(await run("pptx", nestedMarkdown, "nested.pptx")));
  const slideXml = (await Promise.all(Object.entries(nestedDeck.files)
    .filter(([name]) => /^ppt\/slides\/slide\d+\.xml$/.test(name))
    .map(([, file]) => file.async("string")))).join("\n");
  assert.ok(slideXml.includes("子任务甲") && slideXml.includes("深层任务乙"), "PPTX dropped nested tasks");

  const fidelityDocx = await JSZip.loadAsync(await readFile(await run("docx", fidelityMarkdown, "text-fidelity.docx")));
  const fidelityDocxXml = await fidelityDocx.file("word/document.xml").async("string");
  const fidelityPdf = await PDFDocument.load(await readFile(await run("pdf", fidelityMarkdown, "text-fidelity.pdf")));
  const fidelityPdfText = pdfText(fidelityPdf);
  const fidelityDeck = await JSZip.loadAsync(await readFile(await run("pptx", fidelityMarkdown, "text-fidelity.pptx")));
  const fidelitySlides = (await Promise.all(Object.entries(fidelityDeck.files)
    .filter(([name]) => /^ppt\/slides\/slide\d+\.xml$/.test(name))
    .map(([, file]) => file.async("string")))).join("\n");
  for (const [label, expected] of fidelityRows) {
    assert.ok(officeXmlText(fidelityDocxXml, "w").includes(expected), `DOCX changed ${label}`);
    assert.ok(fidelityPdfText.includes(expected), `PDF changed ${label}`);
    assert.ok(fidelitySlides.includes(xmlText(expected)), `PPTX changed ${label}`);
  }
  const fidelityBook = new ExcelJS.Workbook();
  await fidelityBook.xlsx.readFile(await run("xlsx", fidelityMarkdown, "text-fidelity.xlsx"));
  for (const [index, [label, expected]] of fidelityRows.entries()) {
    assert.equal(fidelityBook.worksheets[0].getRow(index + 2).getCell(1).value, label);
    assert.equal(fidelityBook.worksheets[0].getRow(index + 2).getCell(2).value, expected, `XLSX changed ${label}`);
  }
  console.log("Office text fidelity passed: comparisons, TeX source, inline code, entities, HTML/wbr across four formats");

  const mathDocx = await JSZip.loadAsync(await readFile(await run("docx", mathMarkdown, "math-source.docx")));
  const mathDocxText = officeXmlText(await mathDocx.file("word/document.xml").async("string"), "w");
  const mathPdfText = pdfText(await PDFDocument.load(await readFile(await run("pdf", mathMarkdown, "math-source.pdf"))));
  const mathDeck = await JSZip.loadAsync(await readFile(await run("pptx", mathMarkdown, "math-source.pptx")));
  const mathSlidesText = (await Promise.all(Object.entries(mathDeck.files)
    .filter(([name]) => /^ppt\/slides\/slide\d+\.xml$/.test(name))
    .map(async ([, file]) => officeXmlText(await file.async("string"), "a")))).join("\n");
  const mathBook = new ExcelJS.Workbook();
  await mathBook.xlsx.readFile(await run("xlsx", mathMarkdown, "math-source.xlsx"));
  const mathSheetText = mathBook.worksheets[0].getColumn(1).values.filter((value) => value !== undefined).join("\n");
  for (const expected of sourceExpected) {
    assert.ok(mathDocxText.includes(expected), `DOCX changed mathematical/code source: ${expected}`);
    assert.ok(mathPdfText.includes(expected), `PDF changed mathematical/code source: ${expected}`);
    assert.ok(mathSlidesText.includes(expected), `PPTX changed mathematical/code source: ${expected}`);
    assert.ok(mathSheetText.includes(expected), `XLSX changed mathematical/code source: ${expected}`);
  }
  console.log("Office math/code source passed: compact TeX, escaped braces, multiline math, Mermaid, escaped dollars and code fences");
  console.log("Offline Office export smoke passed: docx, pdf, xlsx, pptx");
} finally {
  await rm(directory, { recursive: true, force: true });
}
