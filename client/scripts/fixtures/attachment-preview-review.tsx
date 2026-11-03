import "../../src/styles/index.css";
import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import JSZip from "jszip";
import { PDFDocument, StandardFonts } from "pdf-lib";
import { ThemeProvider } from "../../src/components/ThemeProvider";
import { Composer } from "../../src/components/Composer";
import { MessageItem } from "../../src/components/MessageItem";
import { saveDraftAttachments } from "../../src/lib/draft-attachments";
import { saveFontSize } from "../../src/lib/font-size";

const query = new URLSearchParams(location.search);
localStorage.setItem("echoagent.theme", query.get("theme") ?? "light");
saveFontSize(Number(query.get("font")) || 13);
const calls: Array<{ command: string; args: Record<string, unknown> }> = [];
const paths = ["/review/界面截图.png", "/review/截图对照.webp", "/review/需求说明.md", "/review/方案.docx", "/review/预算表.xlsx", "/review/参考材料.pdf"];
if (query.has("many")) for (let i = 1; i <= 14; i++) paths.push(`/review/需求附件-${i}.txt`);
saveDraftAttachments("attachment-review", paths);
saveDraftAttachments("other-review", ["/review/另一任务.txt"]);

function screenshot() {
  const canvas = document.createElement("canvas"); canvas.width = 1600; canvas.height = 1000;
  const ctx = canvas.getContext("2d")!;
  ctx.fillStyle = "#f8faf9"; ctx.fillRect(0, 0, 1600, 1000);
  ctx.fillStyle = "#e6eeeb"; ctx.fillRect(0, 0, 280, 1000);
  ctx.fillStyle = "#213c34"; ctx.font = "36px sans-serif"; ctx.fillText("EchoAgent", 40, 80);
  ctx.font = "24px sans-serif"; ctx.fillText("项目工作区", 40, 160); ctx.fillText("最近任务", 40, 220);
  ctx.font = "42px sans-serif"; ctx.fillText("季度项目进展", 350, 110);
  ctx.font = "26px sans-serif"; ctx.fillStyle = "#55675e"; ctx.fillText("原始截图 · 1600 × 1000", 350, 165);
  const values = [160, 260, 210, 340, 290];
  values.forEach((height, index) => {
    ctx.fillStyle = "#34a889"; ctx.fillRect(390 + index * 200, 750 - height, 110, height);
    ctx.fillStyle = "#213c34"; ctx.fillText(`项目 ${index + 1}`, 395 + index * 200, 800);
  });
  ctx.fillStyle = "#55675e"; ctx.font = "24px sans-serif"; ctx.fillText("检查图片细节、完整内容及多附件切换", 350, 910);
  const original = canvas.toDataURL("image/png").split(",")[1];
  const small = document.createElement("canvas"); small.width = 320; small.height = 200;
  small.getContext("2d")!.drawImage(canvas, 0, 0, 320, 200);
  return { original, thumbnail: small.toDataURL("image/jpeg").split(",")[1] };
}
const image = screenshot();
const word = new JSZip();
word.file("word/document.xml", '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>附件预览产品方案</w:t></w:r></w:p><w:p><w:r><w:t>用户在发送前确认文件内容，关闭预览后继续编辑草稿。</w:t></w:r></w:p></w:body></w:document>');
const wordBase64 = await word.generateAsync({ type: "base64" });
const sheet = new JSZip();
sheet.file("xl/workbook.xml", '<workbook><sheets><sheet name="预算" sheetId="1" r:id="rId1" /></sheets></workbook>');
sheet.file("xl/worksheets/sheet1.xml", '<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>项目</t></is></c><c r="B1" t="inlineStr"><is><t>预算</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>界面优化</t></is></c><c r="B2"><v>12000</v></c></row></sheetData></worksheet>');
const sheetBase64 = await sheet.generateAsync({ type: "base64" });
const pdf = await PDFDocument.create(); const pdfPage = pdf.addPage([595, 842]);
pdfPage.drawText("Attachment preview", { x: 60, y: 750, size: 26, font: await pdf.embedFont(StandardFonts.Helvetica) });
const pdfBase64 = await pdf.saveAsBase64();
Object.assign(window, { __attachmentCalls: calls, __TAURI_INTERNALS__: {
  invoke: async (command: string, args: Record<string, unknown> = {}) => {
    calls.push({ command, args });
    if (command === "attachment_thumbnail") return image.thumbnail;
    if (command === "path_stat") return { absolute: args.path, exists: true, kind: "file" };
    if (command === "attachment_preview_data") {
      if (query.get("error") === "read") throw new Error("路径不存在");
      const path = String(args.path);
      return path.endsWith(".docx") ? wordBase64 : path.endsWith(".xlsx") ? sheetBase64 : path.endsWith(".pdf") ? pdfBase64 : image.original;
    }
    if (command === "read_text_file") return String(args.path).endsWith(".md") ? "# 需求说明\n\n发送前可以核对附件内容。\n\n- 图片支持原图缩放\n- 文档支持只读预览\n- 草稿与附件完整保留" : "另一任务的附件正文";
    if (command === "filesystem_attachment_stats") return { files: (args.paths as string[]).map(path => ({ inputPath: path, path, sizeBytes: 4096 })), rejected: [] };
    if (command === "open_path" || command === "discard_attachment_blob") return;
    if (command.startsWith("plugin:event|")) return 0;
    if (command === "automation_capabilities") return { browserAvailable: false, computerAvailable: false };
    throw new Error(`Unexpected fixture command ${command}`);
  }, transformCallback: () => 0,
} });

function Review() {
  const [scope, setScope] = useState("attachment-review");
  const [draft, setDraft] = useState("请根据截图和文件优化方案，保留原有业务逻辑。");
  const [sent, setSent] = useState<{ text: string; paths: string[] } | null>(null);
  const [edit, setEdit] = useState<{ text: string; paths: string[]; nonce: number }>();
  return <main style={{ height: "100%", overflow: "auto", padding: "24px 16px", background: "var(--echo-bg-secondary)", color: "var(--echo-text-strong)" }}>
    <div style={{ maxWidth: 880, margin: "0 auto" }}>
      <button type="button" onClick={() => setScope(current => current === "attachment-review" ? "other-review" : "attachment-review")}>切换任务</button>
      <h1 style={{ fontSize: "1.75rem", textAlign: "center", margin: "24px 0" }}>今天想完成什么？</h1>
      {sent && <div style={{ marginBottom: 24 }}><MessageItem message={{ id: "sent", role: "user", complete: true, parts: [{ kind: "text", text: sent.text }], attachments: sent.paths }}
        streaming={false} cwd="/review" onEditResend={(text, files) => setEdit(previous => ({ text, paths: files, nonce: (previous?.nonce ?? 0) + 1 }))} /></div>}
      <Composer streaming={false} onCancel={() => {}} draftKey={scope} draft={draft} onDraftChange={setDraft}
        showMeta cwd="/review" externalText={edit?.text} externalAttachments={edit?.paths} externalTextNonce={edit?.nonce}
        onSend={(text, files) => { setSent({ text, paths: files ?? [] }); return true; }} />
    </div>
  </main>;
}
createRoot(document.getElementById("root")!).render(<ThemeProvider><Review /></ThemeProvider>);
