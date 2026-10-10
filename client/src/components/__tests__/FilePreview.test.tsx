import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { FilePreview } from "../FilePreview";

describe("FilePreview", () => {
  it("markdown 渲染文件名 + Markdown 标签 + 正文", () => {
    render(<FilePreview filename="readme.md" content="# 标题" />);
    expect(screen.getByText("readme.md")).toBeInTheDocument();
    expect(screen.getByText("Markdown")).toBeInTheDocument();
    expect(screen.getByText("标题")).toBeInTheDocument();
  });

  it("Markdown 文件复制保留原始语法", async () => {
    const onCopyText = vi.fn(async () => true);
    render(<FilePreview filename="readme.md" content={"# 标题\n\n- 交付结果"} onCopyText={onCopyText} />);
    fireEvent.click(screen.getByRole("button", { name: "复制 Markdown 内容" }));
    expect(await screen.findByText("已复制")).toBeInTheDocument();
    expect(onCopyText).toHaveBeenCalledWith("# 标题\n\n- 交付结果");
  });

  it("image 渲染 <img>", () => {
    render(<FilePreview filename="pic.png" content="data:image/png;base64,xxx" />);
    const img = screen.getByAltText("pic.png") as HTMLImageElement;
    expect(img.src).toContain("data:image/png;base64,xxx");
  });

  it("code 渲染语言标签 + 复制按钮", () => {
    render(<FilePreview filename="app.tsx" content="const x = 1;" />);
    expect(screen.getByText("tsx")).toBeInTheDocument();
    expect(screen.getByText("const x = 1;")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "复制内容" })).toBeInTheDocument();
  });

  it("复制按钮回调 onCopyText 并切换文案", async () => {
    const onCopyText = vi.fn();
    render(
      <FilePreview filename="a.txt" content="hello" onCopyText={onCopyText} />,
    );
    const btn = screen.getByRole("button", { name: "复制内容" });
    fireEvent.click(btn);
    expect(onCopyText).toHaveBeenCalledWith("hello");
    expect(await screen.findByText("已复制")).toBeInTheDocument();
  });

  it("text 渲染 lang=text", () => {
    render(<FilePreview filename="notes.txt" content="一行文本" />);
    expect(screen.getByText("text")).toBeInTheDocument();
    expect(screen.getByText("一行文本")).toBeInTheDocument();
  });

  it("audio 渲染 HTML5 <audio>", () => {
    render(<FilePreview filename="song.mp3" content="data:audio/mp3;base64,xxx" />);
    const audio = document.querySelector("audio") as HTMLAudioElement;
    expect(audio).not.toBeNull();
    expect(audio.getAttribute("src")).toContain("data:audio/mp3");
    expect(audio.hasAttribute("controls")).toBe(true);
  });

  it("video 渲染 HTML5 <video>", () => {
    render(<FilePreview filename="clip.mp4" content="data:video/mp4;base64,xxx" />);
    const video = document.querySelector("video") as HTMLVideoElement;
    expect(video).not.toBeNull();
    expect(video.getAttribute("src")).toContain("data:video/mp4");
    expect(video.hasAttribute("controls")).toBe(true);
  });

  it("audio/video 不走 binary 占位", () => {
    render(<FilePreview filename="song.mp3" content="x" />);
    expect(screen.queryByText(/暂不支持内嵌预览/)).toBeNull();
  });

  it("pdf 渲染 <iframe>(浏览器原生 PDF 预览)", () => {
    render(<FilePreview filename="doc.pdf" content="data:application/pdf;base64,xxx" />);
    const iframe = document.querySelector("iframe") as HTMLIFrameElement;
    expect(iframe).not.toBeNull();
    expect(iframe.getAttribute("src")).toContain("data:application/pdf");
    expect(iframe.title).toBe("doc.pdf");
  });

  it("pdf 不走 binary 占位", () => {
    render(<FilePreview filename="doc.pdf" content="x" />);
    expect(screen.queryByText(/暂不支持内嵌预览/)).toBeNull();
  });

  // ---------- 文档预览(docx/pptx/sheet)----------
  it("docx 无 docExtractor 时显示降级占位", () => {
    render(<FilePreview filename="report.docx" content="binary" />);
    expect(screen.getByText("Word")).toBeInTheDocument();
    expect(screen.getByText(/请用系统应用打开原件/)).toBeInTheDocument();
  });

  it("docx 有 docExtractor 时渲染提取的段落文本", () => {
    const zip = {
      readText: () => `<w:p><w:r><w:t>第一段</w:t></w:r></w:p>`,
      listEntries: () => ["word/document.xml"],
    };
    render(
      <FilePreview
        filename="report.docx"
        content="binary"
        docExtractor={() => zip}
      />,
    );
    expect(screen.getByText("第一段")).toBeInTheDocument();
  });

  it("pptx 有 docExtractor 时渲染幻灯片文本", () => {
    const zip = {
      readText: () => `<a:p><a:t>幻灯片标题</a:t></a:p>`,
      listEntries: () => ["ppt/slides/slide1.xml"],
    };
    render(
      <FilePreview
        filename="deck.pptx"
        content="binary"
        docExtractor={() => zip}
      />,
    );
    expect(screen.getByText("幻灯片标题")).toBeInTheDocument();
  });

  it("sheet 有 docExtractor 时渲染表格(<table>)", () => {
    const zip = {
      readText: (p: string) =>
        p.includes("sharedStrings")
          ? `<sst><si><t>姓名</t></si></sst>`
          : `<worksheet><row><c t="s"><v>0</v></c></row></worksheet>`,
      listEntries: () => ["xl/sharedStrings.xml", "xl/worksheets/sheet1.xml"],
    };
    render(
      <FilePreview
        filename="data.xlsx"
        content="binary"
        docExtractor={() => zip}
      />,
    );
    expect(screen.getByText("姓名")).toBeInTheDocument();
    expect(document.querySelector("table")).not.toBeNull();
  });

  it("docx 复制文本按钮回调 onCopyText", async () => {
    const onCopyText = vi.fn();
    const zip = {
      readText: () => `<w:p><w:r><w:t>内容</w:t></w:r></w:p>`,
      listEntries: () => ["word/document.xml"],
    };
    render(
      <FilePreview
        filename="r.docx"
        content="binary"
        docExtractor={() => zip}
        onCopyText={onCopyText}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "复制文本" }));
    expect(onCopyText).toHaveBeenCalledWith("内容");
    expect(await screen.findByText("已复制")).toBeInTheDocument();
  });

  it("docx 默认解压器:从 data: URL(base64 zip)提取文本(运行时路径)", async () => {
    // 用 Node zlib 构造一个真实 DEFLATE docx zip,转 base64 data: URL。
    const { Buffer } = await import("node:buffer");
    const { deflateRawSync } = await import("node:zlib");
    const docXml = Buffer.from(
      `<w:document><w:p><w:r><w:t>真实文档</w:t></w:r></w:p></w:document>`,
      "utf-8",
    );
    const compressed = deflateRawSync(docXml);
    const name = Buffer.from("word/document.xml", "utf-8");
    const crc = 0;
    const h = Buffer.alloc(30);
    h.writeUInt32LE(0x04034b50, 0); h.writeUInt16LE(20, 4); h.writeUInt16LE(0, 6);
    h.writeUInt16LE(8, 8); h.writeUInt16LE(0, 10); h.writeUInt16LE(0, 12);
    h.writeUInt32LE(crc, 14); h.writeUInt32LE(compressed.length, 18);
    h.writeUInt32LE(docXml.length, 22); h.writeUInt16LE(name.length, 26); h.writeUInt16LE(0, 28);
    const zip = Buffer.concat([h, name, compressed]);
    const dataUrl = `data:application/vnd.openxmlformats-officedocument.wordprocessingml.document;base64,${zip.toString("base64")}`;
    render(<FilePreview filename="report.docx" content={dataUrl} />);
    expect(screen.getByText("真实文档")).toBeInTheDocument();
  });

  it("binary 显示占位文案", () => {
    render(<FilePreview filename="archive.zip" content="" />);
    expect(screen.getByText(/暂不支持内嵌预览/)).toBeInTheDocument();
  });

  it("zip 等未知类型也走 binary 占位", () => {
    render(<FilePreview filename="archive.zip" content="" />);
    expect(screen.getByText(/暂不支持内嵌预览/)).toBeInTheDocument();
    expect(screen.getByText("文件")).toBeInTheDocument();
  });

  it("复制等待回调完成并禁止重复提交", async () => {
    let finish!: (value: boolean) => void;
    const onCopyText = vi.fn(() => new Promise<boolean>((resolve) => { finish = resolve; }));
    render(<FilePreview filename="a.txt" content="hello" onCopyText={onCopyText} />);
    const button = screen.getByRole("button", { name: "复制内容" });
    fireEvent.click(button);
    expect(button).toBeDisabled();
    expect(screen.queryByText("已复制")).not.toBeInTheDocument();
    fireEvent.click(button);
    expect(onCopyText).toHaveBeenCalledTimes(1);
    finish(true);
    expect(await screen.findByText("已复制")).toBeInTheDocument();
    expect(button).toBeEnabled();
  });

  it.each(["false", "reject"])("复制回调 %s 时显示失败而不是成功", async (mode) => {
    const onCopyText = mode === "false" ? vi.fn(async () => false) : vi.fn(async () => { throw new Error("denied"); });
    render(<FilePreview filename="a.txt" content="hello" onCopyText={onCopyText} />);
    fireEvent.click(screen.getByRole("button", { name: "复制内容" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("复制失败");
    expect(screen.queryByText("已复制")).not.toBeInTheDocument();
  });

  it("无复制回调时实际复制到剪贴板", async () => {
    const original = Object.getOwnPropertyDescriptor(navigator, "clipboard");
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    try {
      render(<FilePreview filename="a.txt" content="hello" />);
      fireEvent.click(screen.getByRole("button", { name: "复制内容" }));
      await screen.findByText("已复制");
      expect(writeText).toHaveBeenCalledWith("hello");
    } finally {
      if (original) Object.defineProperty(navigator, "clipboard", original);
      else Reflect.deleteProperty(navigator, "clipboard");
    }
  });

  it("切换文件不保留旧复制结果", async () => {
    let finish!: (value: boolean) => void;
    const onCopyText = vi.fn(() => new Promise<boolean>((resolve) => { finish = resolve; }));
    const { rerender } = render(<FilePreview filename="a.txt" content="first" onCopyText={onCopyText} />);
    fireEvent.click(screen.getByRole("button", { name: "复制内容" }));
    rerender(<FilePreview filename="b.txt" content="second" onCopyText={onCopyText} />);
    finish(true);
    await waitFor(() => expect(screen.getByRole("button", { name: "复制内容" })).toBeEnabled());
    expect(screen.queryByText("已复制")).not.toBeInTheDocument();
  });

  it("图片失败有可读反馈，重新选择资源后恢复预览", () => {
    const { rerender } = render(<FilePreview filename="pic.png" content="bad" />);
    fireEvent.error(screen.getByAltText("pic.png"));
    expect(screen.getByRole("alert")).toHaveTextContent("无法加载预览");
    rerender(<FilePreview filename="pic.png" content="data:image/png;base64,new" />);
    expect(screen.getByRole("button", { name: "放大预览：pic.png" })).toBeInTheDocument();
  });

  it("音视频失败显示可读状态", () => {
    const { container } = render(<FilePreview filename="clip.mp4" content="bad" />);
    fireEvent.error(container.querySelector("video")!);
    expect(screen.getByRole("alert")).toHaveTextContent("无法加载预览");
  });

  it("空文档说明没有可读文本", () => {
    render(<FilePreview filename="empty.docx" content="binary" docExtractor={() => ({ readText: () => "<w:document />", listEntries: () => ["word/document.xml"] })} />);
    expect(screen.getByText("未提取到可阅读的文本")).toBeInTheDocument();
  });
});
