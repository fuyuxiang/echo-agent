import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, readFile, rm, access } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import ExcelJS from "exceljs";
import JSZip from "jszip";
import { createStructuredWorkbook, createStructuredPresentation } from "./office-structured.mjs";

const directory = await mkdtemp(join(tmpdir(), "echo-structured-office-test-"));
const root = resolve(import.meta.dirname, "..");
const stagedNode = join(root, "src-tauri/resources/theia/node", process.platform === "win32" ? "node.exe" : "bin/node");
function createViaWorker(request, output) {
  const result = spawnSync(existsSync(stagedNode) ? stagedNode : process.execPath,
    [join(root, "src-tauri/resources/office/worker.mjs"), output,
      join(root, "src-tauri/resources/office/fonts/NotoSansCJKsc-Regular.otf")],
    { input: JSON.stringify(request), encoding: "utf8", timeout: 30000 });
  assert.equal(result.status, 0, result.stderr || String(result.error));
}
try {
  const xlsx = join(directory, "monthly.xlsx");
  const income = [100, 120, 140, 160, 180, 102], cost = [60, 72, 84, 96, 108, 60];
  const rows = [
    ["月份", "收入", "成本", "利润", "利润率", "编号"],
    ...income.map((value, i) => [String(i + 1) + "月", value, cost[i],
      { formula: "B" + (i + 2) + "-C" + (i + 2), format: "#,##0.00" },
      { formula: "IF(B" + (i + 2) + "=0,0,D" + (i + 2) + "/B" + (i + 2) + ")", format: "0.00%" },
      { type: "text", value: "001234" }]),
    ["总计", { formula: "SUM(B2:B7)" }, { formula: "SUM(C2:C7)" }, { formula: "SUM(D2:D7)" }, { formula: "D8/B8" }],
  ];
  createViaWorker({ title: "月度经营分析", format: "xlsx", workbook: { sheets: [
    { name: "月度指标", rows, formats: ["General", "0.00", "0.00", "0.000", "0.000"], charts: [{ type: "bar", title: "收入与成本", categories: "A2:A7",
      series: [{ name: "收入", values: "B2:B7" }, { name: "成本", values: "C2:C7" }] }] },
    { name: "边界 校验", rows: [["项目", "结果"], ["零收入", { formula: "IFERROR(0/0,0)" }],
      ["跨表", { formula: "'月度指标'!D8" }], ["空格表名", { formula: "'边界 校验'!B2+1" }],
      ["文本公式", "=SUM(B2)"], ["日期", { type: "date", value: "2026-10-23" }],
      ["空白计数", { formula: "COUNT(C10:C12)" }], ["混合参数", { formula: 'SUM(TRUE,"3",C10:C12)' }],
      ["空白均值", { formula: "IFERROR(AVERAGE(C10:C12),0)" }],
      ["文本计数", { formula: 'COUNT(TRUE,"3","文字",A1:A2)' }],
      ["指数结合顺序", { formula: "2^3^2" }], ["日期序号", { formula: "SUM(B6)" }],
      ["早期日期", { type: "date", value: "1900-01-01" }], ["早期日期序号", { formula: "SUM(B13)" }]] },
  ] } }, xlsx);
  const book = new ExcelJS.Workbook(); await book.xlsx.readFile(xlsx);
  assert.equal(book.getWorksheet(1).getCell("B8").result, 802);
  assert.equal(book.getWorksheet(1).getCell("C8").result, 480);
  assert.equal(book.getWorksheet(1).getCell("D8").result, 322);
  assert.equal(book.getWorksheet(1).getCell("F2").value, "001234");
  assert.equal(book.getWorksheet(1).getCell("E2").numFmt, "0.00%");
  assert.equal(book.getWorksheet(1).getCell("D2").numFmt, "#,##0.00");
  assert.equal(book.getWorksheet(1).getCell("D8").numFmt, "0.000");
  assert.equal(book.getWorksheet(2).getCell("B2").result, 0);
  assert.equal(book.getWorksheet(2).getCell("B3").result, 322);
  assert.equal(book.getWorksheet(2).getCell("B4").result, 1);
  assert.equal(book.getWorksheet(2).getCell("B5").value, "=SUM(B2)");
  assert.equal(book.getWorksheet(2).getCell("B7").result, 0);
  assert.equal(book.getWorksheet(2).getCell("B8").result, 4);
  assert.equal(book.getWorksheet(2).getCell("B9").result, 0);
  assert.equal(book.getWorksheet(2).getCell("B10").result, 2);
  assert.equal(book.getWorksheet(2).getCell("B11").result, 64);
  assert.equal(book.getWorksheet(2).getCell("B12").result, 46318);
  assert.equal(book.getWorksheet(2).getCell("B14").result, 1);
  const archive = await JSZip.loadAsync(await readFile(xlsx));
  const chart = await archive.file("xl/charts/chart1.xml").async("string");
  assert.match(chart, /barChart/); assert.match(chart, /收入/); assert.match(chart, /月度指标.*B2:B7/);
  assert.match(await archive.file("xl/worksheets/sheet1.xml").async("string"), /drawing r:id="echoDrawing"/);
  assert.match(await archive.file("xl/drawings/_rels/drawingEcho1.xml.rels").async("string"), /charts\/chart1.xml/);
  assert.match(await archive.file("[Content_Types].xml").async("string"), /drawingml.chart\+xml/);

  const chartOutput = join(directory, "chart-text.xlsx");
  const chartWorkbook = {
    sheets: [{ name: "图表数据", rows: [["类别", "数量"], ["第一季 & \"重点\"", 123]],
      charts: [{ type: "bar", title: "增长 <年度>", categories: "A2:A2", series: [{ name: "销售额 🚀", values: "B2:B2" }] }] }],
  };
  for (const [field, update] of [
    ["分类单元格 A2", sheet => { sheet.rows[1][0] = "Q1\u0000data"; }],
    ["第 1 个系列名称", sheet => { sheet.charts[0].series[0].name = "收入\u000b"; }],
    ["图表标题", sheet => { sheet.charts[0].title = "标题\ud800"; }],
  ]) {
    const invalid = structuredClone(chartWorkbook);
    update(invalid.sheets[0]);
    await assert.rejects(createStructuredWorkbook(invalid, chartOutput), new RegExp(field));
    assert.equal(existsSync(chartOutput), false, "invalid chart must not leave an output file");
  }
  await createStructuredWorkbook(chartWorkbook, chartOutput);
  const validChart = await JSZip.loadAsync(await readFile(chartOutput));
  const validChartXml = await validChart.file("xl/charts/chart1.xml").async("string");
  assert.match(validChartXml, /第一季 &amp; &quot;重点&quot;/);
  assert.match(validChartXml, /增长 &lt;年度&gt;/);
  assert.match(validChartXml, /销售额 🚀/);

  const roundedPath = join(directory, "rounding.xlsx");
  const roundingCases = [
    ["ROUND(10.075,2)", 10.08], ["ROUND(-10.075,2)", -10.08],
    ["ROUND(1.005,2)", 1.01], ["ROUND(-1.005,2)", -1.01],
    ["ROUND(1.275,2)", 1.28], ["ROUND(2.5,0)", 3], ["ROUND(-2.5,0)", -3],
    ["ROUND(1250,-2)", 1300], ["ROUND(-1250,-2)", -1300],
    ["ROUND(1.5E-12,12)", 2e-12], ["ROUND(1E-20,12)", 0],
    ["ROUND(1E20,-12)", 1e20], ["ROUND(10.07,2)", 10.07],
    ["ROUND(123456789012345.5,0)", 123456789012346],
    ["ROUND(1.005-0.0000000000000001,2)", 1.01],
  ];
  createViaWorker({ title: "舍入边界", format: "xlsx", workbook: { sheets: [{
    name: "舍入", rows: [["公式", "结果", "依赖结果"], ...roundingCases.map(([formula], i) =>
      [formula, { formula }, { formula: "B" + (i + 2) + "*100" }])],
    charts: [{ type: "bar", categories: "A2:A3", series: [{ name: "舍入结果", values: "B2:B3" }] }],
  }] } }, roundedPath);
  const rounded = new ExcelJS.Workbook(); await rounded.xlsx.readFile(roundedPath);
  roundingCases.forEach(([formula, expected], i) => {
    assert.equal(rounded.getWorksheet(1).getCell(i + 2, 2).result, expected, formula);
    assert.equal(rounded.getWorksheet(1).getCell(i + 2, 3).result, expected * 100, formula + " dependency");
  });
  const roundingArchive = await JSZip.loadAsync(await readFile(roundedPath));
  const roundingChart = await roundingArchive.file("xl/charts/chart1.xml").async("string");
  assert.match(roundingChart, /<c:v>10\.08<\/c:v>/);
  assert.match(roundingChart, /<c:v>-10\.08<\/c:v>/);

  const pptx = join(directory, "briefing.pptx");
  createViaWorker({ title: "六页汇报", format: "pptx", slides: [
    { title: "季度经营汇报", body: "收入 802，成本 480，利润 322。" },
    { title: "关键结论", body: "所有指标来自同一份已核对的数据。" },
    { title: "收入趋势", chart: { type: "line", categories: ["1月", "2月"], series: [{ name: "收入", values: [100, 120] }] } },
    { title: "收入与成本", chart: { type: "bar", categories: ["1月", "2月"], series: [{ name: "收入", values: [100, 120] }, { name: "成本", values: [60, 72] }] } },
    { title: "行动计划", table: [["行动", "负责人"], ["复核异常订单", "待确认"]] },
    { title: "下一步", body: "按已确认的口径开展后续复核。" },
  ] }, pptx);
  const deck = await JSZip.loadAsync(await readFile(pptx));
  const slideNames = Object.keys(deck.files).filter(p => /^ppt\/slides\/slide\d+\.xml$/.test(p));
  assert.equal(slideNames.length, 6);
  assert.equal(Object.keys(deck.files).filter(p => /^ppt\/charts\/chart\d+\.xml$/.test(p)).length, 2);
  assert.match(await deck.file("ppt/slides/slide5.xml").async("string"), /<a:tbl>/);

  for (const [name, spec, expected] of [
    ["cycle", { sheets: [{ name: "表", rows: [[{ formula: "A1" }]] }] }, /循环引用/],
    ["external", { sheets: [{ name: "表", rows: [[{ formula: "'[secret.xlsx]表'!A1" }]] }] }, /外部/],
    ["unused-external", { sheets: [{ name: "表", rows: [[{ formula: "IF(TRUE,1,'[secret.xlsx]表'!A1)" }]] }] }, /外部/],
    ["unsupported", { sheets: [{ name: "表", rows: [[{ formula: 'IFERROR(HYPERLINK("https://example.invalid"),0)' }]] }] }, /不支持/],
    ["unused-unsupported", { sheets: [{ name: "表", rows: [[{ formula: 'IF(TRUE,1,HYPERLINK("https://example.invalid"))' }]] }] }, /不支持/],
    ["division", { sheets: [{ name: "表", rows: [[{ formula: "1/0" }]] }] }, /除数为零/],
    ["date", { sheets: [{ name: "表", rows: [[{ type: "date", value: "2026-02-30" }]] }] }, /日期无效/],
    ["duplicate", { sheets: [{ name: "Sheet", rows: [[1]] }, { name: "sheet", rows: [[2]] }] }, /名称/],
    ["chart-length", { sheets: [{ name: "表", rows: [["分类", "数值"], ["甲", 1], ["乙", 2]], charts: [
      { type: "line", categories: "A2:A3", series: [{ name: "数值", values: "B2" }] },
    ] }] }, /长度不一致/],
  ]) {
    const output = join(directory, name + ".xlsx");
    await assert.rejects(createStructuredWorkbook(spec, output), expected);
    await assert.rejects(access(output), undefined, "invalid content must not publish a file");
  }
  await assert.rejects(createStructuredPresentation("标题", [{ title: "过密", body: "字".repeat(701) }], join(directory, "overflow.pptx")), /过多/);
  await assert.rejects(createStructuredPresentation("标题", [{ title: "过密表格", table: Array.from({ length: 12 }, () => Array(8).fill("很长的内容".repeat(15))) }], join(directory, "overflow-table.pptx")), /过多/);
  console.log("Structured Office passed: real formulas/caches, zero-income/cross-sheet cases, native chart relationships, exact 6 slides, 2 charts, table, invalid-input rejection.");
} finally {
  await rm(directory, { recursive: true, force: true });
}
