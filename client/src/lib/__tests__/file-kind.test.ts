import { describe, it, expect } from "vitest";
import {
  detectPreviewKind,
  isLocallyRenderable,
  previewKindLabel,
  codeLanguage,
} from "../file-kind";

describe("detectPreviewKind", () => {
  it("markdown", () => {
    expect(detectPreviewKind("readme.md")).toBe("markdown");
    expect(detectPreviewKind("a.mdx")).toBe("markdown");
  });
  it("image", () => {
    expect(detectPreviewKind("a.png")).toBe("image");
    expect(detectPreviewKind("b.JPG")).toBe("image");
    expect(detectPreviewKind("c.svg")).toBe("image");
  });
  it("code", () => {
    expect(detectPreviewKind("app.tsx")).toBe("code");
    expect(detectPreviewKind("main.rs")).toBe("code");
    expect(detectPreviewKind("style.scss")).toBe("code");
  });
  it("text", () => {
    expect(detectPreviewKind("notes.txt")).toBe("text");
    expect(detectPreviewKind("data.csv")).toBe("text");
  });
  it("previews accepted source, text and data formats consistently with the composer", () => {
    expect(detectPreviewKind("component.vue")).toBe("code");
    for (const path of ["data.tsv", "references.bib", "document.tex", "settings.cfg", "schema.graphql"]) {
      expect(detectPreviewKind(path)).toBe("text");
    }
    expect(detectPreviewKind("notes.mdown")).toBe("markdown");
  });
  it("pdf/audio/video", () => {
    expect(detectPreviewKind("doc.pdf")).toBe("pdf");
    expect(detectPreviewKind("song.mp3")).toBe("audio");
    expect(detectPreviewKind("clip.mp4")).toBe("video");
  });
  it("docx/pptx/sheet", () => {
    expect(detectPreviewKind("report.docx")).toBe("docx");
    expect(detectPreviewKind("deck.pptx")).toBe("pptx");
    expect(detectPreviewKind("data.xlsx")).toBe("sheet");
    expect(detectPreviewKind("old.xls")).toBe("sheet");
  });
  it("binary(未知/二进制)", () => {
    expect(detectPreviewKind("archive.zip")).toBe("binary");
    expect(detectPreviewKind("noext")).toBe("binary");
    expect(detectPreviewKind("file.bin")).toBe("binary");
  });
  it("带路径也能识别", () => {
    expect(detectPreviewKind("/a/b/c.ts")).toBe("code");
    expect(detectPreviewKind("C:\\x\\y.md")).toBe("markdown");
  });
});

describe("isLocallyRenderable", () => {
  it("markdown/image/code/text/audio/video/pdf/docx/pptx/sheet 为 true", () => {
    expect(isLocallyRenderable("markdown")).toBe(true);
    expect(isLocallyRenderable("image")).toBe(true);
    expect(isLocallyRenderable("code")).toBe(true);
    expect(isLocallyRenderable("text")).toBe(true);
    expect(isLocallyRenderable("audio")).toBe(true);
    expect(isLocallyRenderable("video")).toBe(true);
    expect(isLocallyRenderable("pdf")).toBe(true);
    expect(isLocallyRenderable("docx")).toBe(true);
    expect(isLocallyRenderable("pptx")).toBe(true);
    expect(isLocallyRenderable("sheet")).toBe(true);
  });
  it("binary 为 false", () => {
    expect(isLocallyRenderable("binary")).toBe(false);
  });
});

describe("previewKindLabel", () => {
  it("返回中文标签", () => {
    expect(previewKindLabel("markdown")).toBe("Markdown");
    expect(previewKindLabel("image")).toBe("图片");
    expect(previewKindLabel("binary")).toBe("文件");
    expect(previewKindLabel("docx")).toBe("Word");
    expect(previewKindLabel("pptx")).toBe("PPT");
    expect(previewKindLabel("sheet")).toBe("表格");
  });
});

describe("codeLanguage", () => {
  it("已知扩展名映射", () => {
    expect(codeLanguage("a.ts")).toBe("typescript");
    expect(codeLanguage("b.py")).toBe("python");
    expect(codeLanguage("c.rs")).toBe("rust");
    expect(codeLanguage("d.scss")).toBe("scss");
  });
  it("未知扩展名回退 text", () => {
    expect(codeLanguage("a.unknownext")).toBe("text");
    expect(codeLanguage("noext")).toBe("text");
  });
});
