import { writeFile, readFile } from "node:fs/promises";
import ExcelJS from "exceljs";
import JSZip from "jszip";
import pptxgen from "pptxgenjs";

const ensure = (condition, message) => { if (!condition) throw new Error(message); };
const text = (value, limit = 20000) => {
  ensure(typeof value === "string" && value.length <= limit, "文本类型或长度无效");
  return value;
};
const numeric = value => {
  ensure(typeof value === "number" && Number.isFinite(value), "数值必须是有限数字");
  return value;
};
class FormulaValueError extends Error {}
function roundDecimal(value, places) {
  ensure(Number.isInteger(places) && Math.abs(places) <= 12, "ROUND 参数无效");
  numeric(value);
  // Work in decimal at Excel's 15 significant digit precision. Multiplying a
  // binary float by 100 turns midpoints such as 10.075 into 1007.499999... .
  const [mantissa, exponent = "0"] = Math.abs(value).toPrecision(15).split("e");
  const fraction = mantissa.split(".")[1]?.length ?? 0;
  const shift = Number(exponent) - fraction + places;
  if (shift >= 0) return numeric(Math.sign(value) * Number(`${mantissa}e${exponent}`));
  const digits = BigInt(mantissa.replace(".", ""));
  const scale = 10n ** BigInt(-shift);
  const rounded = (digits + scale / 2n) / scale;
  return rounded === 0n ? 0 : numeric(Math.sign(value) * Number(`${rounded}e${-places}`));
}
const xml = value => String(value).replace(/[<>&"']/g, c => ({
  "<": "&lt;", ">": "&gt;", "&": "&amp;", '"': "&quot;", "'": "&apos;",
})[c]);
const cellAddress = value => {
  ensure(typeof value === "string" && /^\$?[A-Z]{1,3}\$?[1-9]\d{0,6}$/i.test(value), "无效的单元格地址");
  const cell = value.replace(/\$/g, "").toUpperCase();
  const column = cell.match(/[A-Z]+/)[0].split("").reduce((n, c) => n * 26 + c.charCodeAt(0) - 64, 0);
  const row = Number(cell.match(/\d+/)[0]);
  ensure(column <= 16384 && row <= 1048576, "单元格超出 Excel 范围");
  return { address: cell, row, column };
};
function range(sheet, source) {
  const parts = text(source, 64).split(":");
  ensure(parts.length <= 2, "范围必须是 A1 或 A1:B5");
  const a = cellAddress(parts[0]), b = cellAddress(parts[1] ?? parts[0]);
  ensure(b.row >= a.row && b.column >= a.column
    && (b.row - a.row + 1) * (b.column - a.column + 1) <= 20000, "单元格范围过大或顺序无效");
  const cells = [];
  for (let r = a.row; r <= b.row; r++) for (let c = a.column; c <= b.column; c++) cells.push(sheet.getCell(r, c));
  return cells;
}

// Only this bounded grammar is evaluated. External workbooks, macros, dynamic
// references and arbitrary JavaScript are never executed.
function formulaTree(formula) {
  const source = text(formula, 2048).replace(/^=/, "");
  const matches = [...source.matchAll(/'(?:[^']|'')+'!\$?[A-Z]{1,3}\$?\d+|[A-Z_\u4e00-\u9fff][\w\u4e00-\u9fff]*!\$?[A-Z]{1,3}\$?\d+|\$?[A-Z]{1,3}\$?\d+|(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?|"(?:[^"]|"")*"|[A-Z_]+|<>|<=|>=|[+\-*/^(),:<>=%]/gi)];
  let end = 0;
  for (const match of matches) {
    ensure(source.slice(end, match.index).trim() === "", "公式包含不支持的字符");
    end = match.index + match[0].length;
  }
  ensure(source.slice(end).trim() === "", "公式包含不支持的字符");
  const tokens = matches.map(m => m[0]);
  ensure(tokens.length <= 512, "公式过于复杂");
  let at = 0, depth = 0;
  const peek = () => tokens[at];
  const take = expected => { const t = tokens[at++]; ensure(!expected || t === expected, "公式语法无效"); return t; };
  function primary() {
    ensure(++depth <= 64, "公式嵌套过深");
    let node, token = take();
    ensure(token !== undefined, "公式不完整");
    if (token === "(") { node = comparison(); take(")"); }
    else if (token === "+" || token === "-") node = { op: token, right: primary() };
    else if (/^(?:\d|\.)/.test(token)) node = { value: numeric(Number(token)) };
    else if (token.startsWith('"')) node = { value: token.slice(1, -1).replace(/""/g, '"') };
    else if (peek() === "(") {
      take("(");
      const args = [];
      if (peek() !== ")") do { args.push(comparison()); if (peek() !== ",") break; take(","); } while (true);
      take(")");
      const fn = token.toUpperCase();
      ensure(["SUM", "AVERAGE", "MIN", "MAX", "COUNT", "ROUND", "IF", "IFERROR"].includes(fn), "不支持的公式函数：" + fn);
      ensure(fn === "IF" ? args.length >= 2 && args.length <= 3
        : ["ROUND", "IFERROR"].includes(fn) ? args.length === 2 : args.length >= 1, fn + " 参数数量无效");
      node = { fn, args };
    } else if (/^(TRUE|FALSE)$/i.test(token)) node = { value: token.toUpperCase() === "TRUE" };
    else {
      ensure(/\$?[A-Z]{1,3}\$?\d+$/i.test(token), "公式引用无效");
      ensure(!/[\[\]\\/]/.test(token), "不支持外部工作簿引用");
      node = { ref: token };
      if (peek() === ":") { take(":"); node.end = take(); cellAddress(node.end); }
    }
    if (peek() === "%") { take("%"); node = { op: "/", left: node, right: { value: 100 } }; }
    depth--; return node;
  }
  function power() { let n = primary(); while (peek() === "^") { take("^"); n = { op: "^", left: n, right: primary() }; } return n; }
  function product() { let n = power(); while (["*", "/"].includes(peek())) n = { op: take(), left: n, right: power() }; return n; }
  function sum() { let n = product(); while (["+", "-"].includes(peek())) n = { op: take(), left: n, right: product() }; return n; }
  function comparison() { let n = sum(); while (["=", "<>", "<", ">", "<=", ">="].includes(peek())) n = { op: take(), left: n, right: sum() }; return n; }
  const tree = comparison();
  ensure(at === tokens.length, "公式存在多余内容");
  return tree;
}

function calculate(book) {
  const visited = new Set(), active = new Set();
  const scalar = v => { ensure(!Array.isArray(v), "此处需要单个单元格"); return v ?? 0; };
  function value(sheet, cell) {
    const raw = cell.value;
    if (raw instanceof Date) {
      const serial = raw.getTime() / 86400000 + 25569;
      return serial < 61 ? serial - 1 : serial;
    }
    if (!raw || typeof raw !== "object" || !raw.formula) return raw;
    const key = sheet.name + "!" + cell.address;
    ensure(!active.has(key) && active.size < 100, "公式循环引用或依赖过深：" + key);
    if (visited.has(key)) return raw.result;
    active.add(key);
    const evaluate = n => {
      if ("value" in n) return n.value;
      if (n.ref) {
        const split = n.ref.lastIndexOf("!");
        const name = split < 0 ? sheet.name : n.ref.slice(0, split).replace(/^'|'$/g, "").replace(/''/g, "'");
        const target = book.worksheets.find(s => s.name.toLowerCase() === name.toLowerCase());
        ensure(target, "公式引用的工作表不存在：" + name);
        const start = split < 0 ? n.ref : n.ref.slice(split + 1);
        return n.end ? range(target, start + ":" + n.end).map(c => value(target, c))
          : value(target, target.getCell(cellAddress(start).address));
      }
      if (n.fn) {
        if (n.fn === "IF") { const condition = scalar(evaluate(n.args[0])); ensure(["boolean", "number"].includes(typeof condition), "IF 条件必须是布尔值或数值"); return condition ? evaluate(n.args[1]) : n.args[2] ? evaluate(n.args[2]) : false; }
        if (n.fn === "IFERROR") { ensure(n.args.length === 2, "IFERROR 需要 2 个参数"); try { return evaluate(n.args[0]); } catch (e) { if (!(e instanceof FormulaValueError)) throw e; return evaluate(n.args[1]); } }
        const args = n.args.map(evaluate);
        if (n.fn === "ROUND") return roundDecimal(numeric(scalar(args[0])), args[1]);
        const numbers = [];
        args.forEach((arg, i) => {
          const reference = n.args[i].ref || Array.isArray(arg);
          for (const value of Array.isArray(arg) ? arg : [arg]) {
            if (typeof value === "number") numbers.push(numeric(value));
            else if (!reference && typeof value === "boolean") numbers.push(Number(value));
            else if (!reference && typeof value === "string") {
              if (/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i.test(value.trim())) numbers.push(numeric(Number(value)));
              else if (n.fn !== "COUNT") throw new FormulaValueError("汇总函数的文本参数不是数字");
            }
          }
          ensure(numbers.length <= 20000, "单个公式汇总的数值超过 20000 个");
        });
        if (n.fn === "SUM") return numbers.reduce((a, b) => a + b, 0);
        if (n.fn === "COUNT") return numbers.length;
        if (n.fn === "AVERAGE") { if (!numbers.length) throw new FormulaValueError("AVERAGE 的范围没有数字"); return numbers.reduce((a, b) => a + b, 0) / numbers.length; }
        if (n.fn === "MIN" || n.fn === "MAX") return numbers.length ? numbers.reduce((a, b) => Math[n.fn.toLowerCase()](a, b)) : 0;
        throw new Error("不支持的公式函数：" + n.fn);
      }
      const r = scalar(evaluate(n.right)), l = n.left ? scalar(evaluate(n.left)) : 0;
      if (["=", "<>", "<", ">", "<=", ">="].includes(n.op)) {
        ensure(typeof l === typeof r, "比较值类型必须一致");
        const a = typeof l === "string" ? l.toLowerCase() : l, b = typeof r === "string" ? r.toLowerCase() : r;
        if (n.op === "=") return a === b;
        if (n.op === "<>") return a !== b;
        if (n.op === "<") return a < b;
        if (n.op === ">") return a > b;
        if (n.op === "<=") return a <= b;
        return a >= b;
      }
      const a = typeof l === "boolean" ? Number(l) : numeric(l), b = typeof r === "boolean" ? Number(r) : numeric(r);
      if (n.op === "+") return a + b;
      if (n.op === "-") return a - b;
      if (n.op === "*") return a * b;
      if (n.op === "/") { if (b === 0) throw new FormulaValueError("公式除数为零；请使用 IF 或 IFERROR"); return a / b; }
      return a ** b;
    };
    const result = evaluate(formulaTree(raw.formula)) ?? 0;
    if (typeof result === "number") numeric(result);
    ensure(["number", "boolean", "string"].includes(typeof result), "公式结果无效");
    cell.value = { formula: raw.formula, result };
    active.delete(key); visited.add(key); return result;
  }
  for (const sheet of book.worksheets) sheet.eachRow(row => row.eachCell(cell => value(sheet, cell)));
}

function setCell(cell, input) {
  if (input === null || ["string", "number", "boolean"].includes(typeof input)) {
    cell.value = typeof input === "number" ? numeric(input) : typeof input === "string" ? text(input) : input;
    return;
  }
  ensure(input && typeof input === "object" && !Array.isArray(input), "单元格类型无效");
  if (input.formula !== undefined) cell.value = { formula: text(input.formula, 2048).replace(/^=/, "") };
  else if (input.type === "date") {
    ensure(/^\d{4}-\d{2}-\d{2}$/.test(input.value), "日期必须使用 YYYY-MM-DD");
    const date = new Date(input.value + "T00:00:00Z");
    ensure(Number.isFinite(date.getTime()) && date.toISOString().slice(0, 10) === input.value && input.value >= "1900-01-01", "日期无效，支持 1900–9999 年的真实日期");
    // Excel's 1900 calendar has a fictitious leap day. ExcelJS's Date writer
    // applies the later epoch to January/February too; use the exact serial for
    // those early dates so the saved value and our formula cache stay equal.
    const serial = date.getTime() / 86400000 + 25569;
    cell.value = serial < 61 ? serial - 1 : date; cell.numFmt = "yyyy-mm-dd";
  } else { ensure(input.type === "text", "使用 type=text/date 或 formula 声明单元格类型"); cell.value = text(input.value); }
  if (input.format !== undefined) cell.numFmt = text(input.format, 128);
}

export async function createStructuredWorkbook(spec, output) {
  ensure(spec && Array.isArray(spec.sheets) && spec.sheets.length > 0 && spec.sheets.length <= 30, "需要 1–30 个工作表");
  const book = new ExcelJS.Workbook();
  book.creator = "EchoAgent"; book.calcProperties.fullCalcOnLoad = true;
  let count = 0;
  for (const item of spec.sheets) {
    ensure(item && typeof item.name === "string" && item.name.length > 0 && item.name.length <= 31
      && !/[\u0000-\u001f\\/*?:[\]]/.test(item.name) && !book.worksheets.some(s => s.name.toLowerCase() === item.name.toLowerCase()), "工作表名称无效或重复");
    ensure(Array.isArray(item.rows) && item.rows.length > 0 && item.rows.length <= 20000, "工作表需要 1–20000 行");
    for (const field of ["widths", "formats"]) ensure(item[field] === undefined || Array.isArray(item[field]) && item[field].length <= 256, field + " 必须是最多 256 项的数组");
    const sheet = book.addWorksheet(item.name);
    sheet.pageSetup = { orientation: "landscape", paperSize: 9, fitToPage: true, fitToWidth: 1, fitToHeight: 0 };
    sheet.pageSetup.printTitlesRow = "1:1";
    for (const [r, row] of item.rows.entries()) {
      ensure(Array.isArray(row) && row.length <= 256, "每行最多 256 列");
      count += row.length; ensure(count <= 100000, "单元格总量超过 100000");
      row.forEach((v, c) => setCell(sheet.getCell(r + 1, c + 1), v));
    }
    sheet.getRow(1).font = { bold: true, color: { argb: "FFFFFFFF" } };
    sheet.getRow(1).fill = { type: "pattern", pattern: "solid", fgColor: { argb: "FF22577A" } };
    sheet.views = [{ state: "frozen", ySplit: 1 }];
    sheet.columns.forEach((column, i) => {
      const samples = item.rows.slice(0, 100).map(row => {
        const input = row[i], value = input && typeof input === "object" ? input.value ?? "" : input ?? "";
        return String(value).split("\n").reduce((longest, line) => Math.max(longest,
          [...line].reduce((n, ch) => n + (/[^\u0000-\u007f]/.test(ch) ? 2 : 1), 0)), 0);
      });
      const width = item.widths?.[i] ?? Math.max(12, Math.min(32, Math.max(...samples) + 2));
      ensure(typeof width === "number" && width >= 4 && width <= 100, "列宽应为 4–100");
      column.width = width;
      if (item.formats?.[i]) column.numFmt = text(item.formats[i], 128);
    });
    // Column defaults must not replace an explicit cell format (for example,
    // a percentage in a column otherwise formatted as decimal numbers).
    if (item.formats) item.rows.forEach((row, r) => row.forEach((input, c) => {
      if (input && typeof input === "object" && input.format !== undefined) sheet.getCell(r + 1, c + 1).numFmt = text(input.format, 128);
    }));
    sheet.autoFilter = { from: { row: 1, column: 1 }, to: { row: 1, column: sheet.columnCount || 1 } };
  }
  calculate(book);
  const zip = await JSZip.loadAsync(await book.xlsx.writeBuffer());
  let chartId = 0;
  for (const [index, item] of spec.sheets.entries()) {
    ensure(item.charts === undefined || Array.isArray(item.charts) && item.charts.length <= 8, "每表最多 8 个图表");
    if (!item.charts?.length) continue;
    const sheet = book.worksheets[index], anchors = [];
    for (const [position, chart] of item.charts.entries()) {
      chartId++;
      const source = workbookChart(sheet, chart);
      const anchor = cellAddress(chart.position ?? "A" + (item.rows.length + 3 + position * 17));
      ensure(anchor.column + 9 <= 16384 && anchor.row + 15 <= 1048576, "图表位置超出工作表范围");
      anchors.push(drawingAnchor(chartId, anchor.column - 1, anchor.row - 1));
      zip.file("xl/charts/chart" + chartId + ".xml", source);
      addContentType(zip, "/xl/charts/chart" + chartId + ".xml", "application/vnd.openxmlformats-officedocument.drawingml.chart+xml");
    }
    const drawing = "drawingEcho" + (index + 1) + ".xml";
    zip.file("xl/drawings/" + drawing, '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">' + anchors.join("") + "</xdr:wsDr>");
    zip.file("xl/drawings/_rels/" + drawing + ".rels", rels(item.charts.map((_, i) => ({ id: "chart" + (chartId - item.charts.length + i + 1), type: "chart", target: "../charts/chart" + (chartId - item.charts.length + i + 1) + ".xml" }))));
    addContentType(zip, "/xl/drawings/" + drawing, "application/vnd.openxmlformats-officedocument.drawing+xml");
    const path = "xl/worksheets/sheet" + (index + 1) + ".xml";
    let source = await zip.file(path).async("string");
    if (!source.includes("xmlns:r=")) source = source.replace("<worksheet ", '<worksheet xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" ');
    zip.file(path, source.replace("</worksheet>", '<drawing r:id="echoDrawing"/></worksheet>'));
    const relation = "xl/worksheets/_rels/sheet" + (index + 1) + ".xml.rels";
    const existing = zip.file(relation) ? await zip.file(relation).async("string") : rels([]);
    zip.file(relation, existing.replace("</Relationships>", relationship({ id: "echoDrawing", type: "drawing", target: "../drawings/" + drawing }) + "</Relationships>"));
  }
  // Content type edits are queued because JSZip reads are asynchronous.
  const contentTypes = await zip.file("[Content_Types].xml").async("string");
  zip.file("[Content_Types].xml", contentTypes.replace("</Types>", (zip.echoTypes ?? []).join("") + "</Types>"));
  await writeFile(output, await zip.generateAsync({ type: "nodebuffer", compression: "DEFLATE" }));
  const reopened = new ExcelJS.Workbook(); await reopened.xlsx.readFile(output);
  ensure(reopened.worksheets.length === spec.sheets.length, "生成的工作表数量不符");
}

function addContentType(zip, path, type) {
  (zip.echoTypes ??= []).push('<Override PartName="' + path + '" ContentType="' + type + '"/>');
}
function relationship(r) { return '<Relationship Id="' + r.id + '" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/' + r.type + '" Target="' + r.target + '"/>'; }
function rels(items) { return '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">' + items.map(relationship).join("") + "</Relationships>"; }
function drawingAnchor(id, col, row) {
  return '<xdr:twoCellAnchor><xdr:from><xdr:col>' + col + '</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>' + row + '</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>' + (col + 9) + '</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>' + (row + 15) + '</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:graphicFrame macro=""><xdr:nvGraphicFramePr><xdr:cNvPr id="' + id + '" name="图表 ' + id + '"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr><xdr:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/></xdr:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="chart' + id + '"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor>';
}
function workbookChart(sheet, chart) {
  ensure(["bar", "line"].includes(chart.type) && Array.isArray(chart.series) && chart.series.length > 0 && chart.series.length <= 8, "图表支持 bar/line 和 1–8 个系列");
  const categories = range(sheet, chart.categories);
  ensure(new Set(categories.map(c => c.row)).size === 1 || new Set(categories.map(c => c.col)).size === 1, "图表分类必须为单行或单列");
  ensure(categories.length <= 500 && categories.length > 0, "图表最多 500 个数据点");
  const chartText = (value, field) => {
    const content = String(value);
    // XML 1.0 permits tab/newline/CR and Unicode scalar values in these ranges.
    ensure(!/[^\u0009\u000A\u000D\u0020-\uD7FF\uE000-\uFFFD\u{10000}-\u{10FFFF}]/u.test(content),
      field + " 含有无效的 XML 字符");
    return xml(content);
  };
  const actual = c => typeof c.value === "object" && c.value?.formula ? c.value.result : c.value ?? "";
  const ref = address => "'" + sheet.name.replace(/'/g, "''") + "'!" + address;
  const cache = (cells, number) => '<c:ptCount val="' + cells.length + '"/>' + cells.map((c, i) => '<c:pt idx="' + i + '"><c:v>' + chartText(number ? numeric(actual(c)) : actual(c), "图表分类单元格 " + c.address) + "</c:v></c:pt>").join("");
  const series = chart.series.map((s, i) => {
    const cells = range(sheet, s.values);
    ensure(new Set(cells.map(c => c.row)).size === 1 || new Set(cells.map(c => c.col)).size === 1, "图表数值必须为单行或单列");
    ensure(cells.length === categories.length, "图表分类与数值长度不一致");
    return '<c:ser><c:idx val="' + i + '"/><c:order val="' + i + '"/><c:tx><c:v>' + chartText(text(s.name, 160), "第 " + (i + 1) + " 个系列名称") + '</c:v></c:tx><c:cat><c:strRef><c:f>' + chartText(ref(chart.categories), "图表分类引用") + '</c:f><c:strCache>' + cache(categories, false) + '</c:strCache></c:strRef></c:cat><c:val><c:numRef><c:f>' + chartText(ref(s.values), "图表数值引用") + '</c:f><c:numCache><c:formatCode>General</c:formatCode>' + cache(cells, true) + '</c:numCache></c:numRef></c:val></c:ser>';
  }).join("");
  const tag = chart.type === "bar" ? "barChart" : "lineChart";
  const options = chart.type === "bar" ? '<c:barDir val="col"/><c:grouping val="clustered"/>' : '<c:grouping val="standard"/>';
  const title = chart.title ? '<c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>' + chartText(text(chart.title, 160), "图表标题") + '</a:t></a:r></a:p></c:rich></c:tx><c:overlay val="0"/></c:title>' : "";
  return '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><c:lang val="zh-CN"/><c:chart>' + title + '<c:plotArea><c:layout/><c:' + tag + '>' + options + series + '<c:axId val="100"/><c:axId val="200"/></c:' + tag + '><c:catAx><c:axId val="100"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:axPos val="b"/><c:tickLblPos val="nextTo"/><c:crossAx val="200"/><c:crosses val="autoZero"/><c:auto val="1"/><c:lblAlgn val="ctr"/><c:lblOffset val="100"/></c:catAx><c:valAx><c:axId val="200"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:axPos val="l"/><c:majorGridlines/><c:numFmt formatCode="General" sourceLinked="1"/><c:tickLblPos val="nextTo"/><c:crossAx val="100"/><c:crosses val="autoZero"/><c:crossBetween val="between"/></c:valAx></c:plotArea><c:legend><c:legendPos val="b"/><c:layout/><c:overlay val="0"/></c:legend><c:plotVisOnly val="1"/></c:chart></c:chartSpace>';
}

export async function createStructuredPresentation(title, slides, output) {
  ensure(Array.isArray(slides) && slides.length > 0 && slides.length <= 100, "幻灯片需要 1–100 页，封面也应包含在列表中");
  const deck = new pptxgen(); deck.layout = "LAYOUT_WIDE"; deck.author = "EchoAgent"; deck.title = title; deck.lang = "zh-CN";
  deck.theme = { headFontFace: "Microsoft YaHei", bodyFontFace: "Microsoft YaHei", lang: "zh-CN" };
  for (const [index, item] of slides.entries()) {
    const slide = deck.addSlide();
    slide.background = { color: "FFFFFF" };
    const heading = text(item.title, 100);
    ensure(heading.length <= 50, "标题过长，请缩短至 50 字以内");
    slide.addText(heading, { x: 0.65, y: 0.4, w: 12, h: 0.75, fontSize: 26, bold: true, color: "16324F" });
    const body = item.body === undefined ? "" : text(item.body, 1400);
    ensure(!(item.table && item.chart), "每页支持一个表格或图表，可在 body 中添加简短说明");
    if (body) {
      const limit = item.table || item.chart ? 180 : 700;
      ensure(body.length <= limit && body.split("\n").length <= (item.table || item.chart ? 3 : 12), "本页文字过多，请精简或增加幻灯片");
      slide.addText(body, { x: 0.7, y: 1.45, w: 11.9, h: item.table || item.chart ? 0.9 : 5.2, fontSize: item.table || item.chart ? 16 : 20, valign: "top", breakLine: false, margin: 0.05, fit: "shrink" });
    }
    const y = body ? 2.6 : 1.55, height = 6.9 - y;
    if (item.table) {
      const rows = item.table;
      ensure(Array.isArray(rows) && rows.length > 0 && rows.length <= 12 && Array.isArray(rows[0]) && rows[0].length > 0 && rows[0].length <= 8, "每页表格最多 12 行、8 列");
      ensure(rows.every(row => Array.isArray(row) && row.length === rows[0].length && row.every(v => typeof v === "string" && v.length <= 90)), "表格必须为等宽文本行，每格最多 90 字");
      const capacity = Math.max(1, Math.floor((11.9 / rows[0].length - 0.14) * 72 / 8));
      const rowHeights = rows.map(row => Math.max(0.38, ...row.map(cell => cell.split("\n").reduce((lines, line) => {
        const units = [...line].reduce((n, ch) => n + (/[^\u0000-\u007f]/.test(ch) ? 2 : 1), 0);
        return lines + Math.max(1, Math.ceil(units / capacity));
      }, 0) * 0.25 + 0.14)));
      ensure(rowHeights.reduce((a, b) => a + b, 0) <= height, "本页表格内容过多，请缩短单元格或拆成多页");
      slide.addTable(rows.map((row, r) => row.map(v => ({ text: v, options: r === 0 ? { bold: true, color: "FFFFFF", fill: "22577A" } : {} }))), {
        x: 0.7, y, w: 11.9, h: height, fontSize: 14, color: "263746", border: { type: "solid", pt: 0.5, color: "D9E2EC" },
        margin: 0.07, autoPage: false, rowH: rowHeights,
      });
    }
    if (item.chart) {
      const c = item.chart;
      ensure(["bar", "line"].includes(c.type) && Array.isArray(c.categories) && c.categories.length > 0 && c.categories.length <= 40
        && c.categories.every(v => typeof v === "string" && v.length <= 50), "图表支持 bar/line，最多 40 个分类");
      ensure(Array.isArray(c.series) && c.series.length > 0 && c.series.length <= 8, "图表需要 1–8 个系列");
      const data = c.series.map(s => {
        ensure(Array.isArray(s.values) && s.values.length === c.categories.length, "图表分类与数值长度不一致");
        return { name: text(s.name, 100), labels: c.categories, values: s.values.map(numeric) };
      });
      slide.addChart(c.type === "bar" ? deck.ChartType.bar : deck.ChartType.line, data, {
        x: 0.7, y, w: 11.9, h: height, showLegend: true, showTitle: Boolean(c.title), title: c.title ? text(c.title, 100) : "",
        catAxisLabelFontSize: 12, valAxisLabelFontSize: 12, legendFontSize: 12, showValue: false, showCatName: false,
      });
    }
    slide.addText(String(index + 1) + " / " + slides.length, { x: 11.7, y: 7.08, w: 0.9, h: 0.2, fontSize: 9, color: "64748B", align: "right" });
  }
  await deck.writeFile({ fileName: output });
  const zip = await JSZip.loadAsync(await readFile(output));
  ensure(Object.keys(zip.files).filter(p => /^ppt\/slides\/slide\d+\.xml$/.test(p)).length === slides.length, "生成的幻灯片数量不符");
}
