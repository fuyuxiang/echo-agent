import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ThemeProvider } from "../ThemeProvider";
import { MessageItem } from "../MessageItem";
import type { ChatMessage } from "@/stores/session-store";

const { openLocalPath, attachmentThumbnail } = vi.hoisted(() => ({
  openLocalPath: vi.fn().mockResolvedValue(undefined),
  attachmentThumbnail: vi.fn().mockResolvedValue("jpeg-base64"),
}));
vi.mock("@/lib/agent-client", () => ({ openLocalPath, attachmentThumbnail }));
vi.mock("@/lib/attachment-preview", async importOriginal => ({
  ...await importOriginal<typeof import("@/lib/attachment-preview")>(),
  loadAttachmentPreview: vi.fn().mockResolvedValue("data:image/png;base64,original-image"),
}));
const originalClipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, "clipboard");

const message: ChatMessage = {
  id: "u1",
  role: "user",
  parts: [{ kind: "text", text: "请优化这份方案" }],
  attachments: ["/tmp/AI数据集平台-数据回流方案.docx"],
  complete: true,
};

function renderMessage(onEditResend = vi.fn()) {
  return {
    onEditResend,
    ...render(
      <ThemeProvider>
        <MessageItem
          message={message}
          streaming={false}
          cwd="/tmp"
          onEditResend={onEditResend}
        />
      </ThemeProvider>,
    ),
  };
}

describe("MessageItem user attachments", () => {
  afterEach(() => {
    if (originalClipboardDescriptor) {
      Object.defineProperty(navigator, "clipboard", originalClipboardDescriptor);
    } else {
      Reflect.deleteProperty(navigator, "clipboard");
    }
  });

  it("在用户气泡中展示文档名和类型", () => {
    renderMessage();
    expect(screen.getByText("AI数据集平台-数据回流方案.docx")).toBeInTheDocument();
    expect(screen.getByText("DOCX 文件")).toBeInTheDocument();
    expect(screen.getByText("请优化这份方案")).toBeInTheDocument();
  });

  it("点击文档卡片先在应用内预览，并保留系统打开入口", async () => {
    renderMessage();
    fireEvent.click(screen.getByRole("button", { name: /打开附件 AI数据集平台/ }));
    await screen.findByRole("dialog", { name: "AI数据集平台-数据回流方案.docx" });
    fireEvent.click(screen.getByRole("button", { name: "用系统应用打开" }));
    await waitFor(() => {
      expect(openLocalPath).toHaveBeenCalledWith(
        "/tmp/AI数据集平台-数据回流方案.docx",
        "/tmp",
      );
    });
  });

  it("编辑重发同时传回文本和附件", () => {
    const onEditResend = vi.fn();
    renderMessage(onEditResend);
    fireEvent.click(screen.getByRole("button", { name: "编辑并重新发送" }));
    expect(onEditResend).toHaveBeenCalledWith(
      "请优化这份方案",
      ["/tmp/AI数据集平台-数据回流方案.docx"],
    );
  });

  it("图片附件懒加载缩略图，点击后预览原图", async () => {
    const imageMessage: ChatMessage = {
      ...message,
      attachments: ["/tmp/screenshot.PNG"],
    };
    render(
      <ThemeProvider>
        <MessageItem message={imageMessage} streaming={false} cwd="/tmp" />
      </ThemeProvider>,
    );

    expect(screen.getByText("PNG 图片")).toBeInTheDocument();
    await waitFor(() => {
      expect(attachmentThumbnail).toHaveBeenCalledWith("/tmp/screenshot.PNG");
      expect(document.querySelector(".msg__attachment-thumbnail"))
        .toHaveAttribute("src", "data:image/jpeg;base64,jpeg-base64");
    });

    fireEvent.click(screen.getByRole("button", { name: "打开附件 screenshot.PNG" }));
    await screen.findByRole("dialog", { name: "screenshot.PNG" });
    await waitFor(() => {
      expect(screen.getByRole("img", { name: "screenshot.PNG" })).toHaveAttribute("src", "data:image/png;base64,original-image");
    });
  });

  it("缩略图生成失败时降级为图片图标，不影响附件卡片", async () => {
    attachmentThumbnail.mockRejectedValueOnce(new Error("missing"));
    const imageMessage: ChatMessage = {
      ...message,
      attachments: ["/tmp/missing.webp"],
    };
    render(
      <ThemeProvider>
        <MessageItem message={imageMessage} streaming={false} cwd="/tmp" />
      </ThemeProvider>,
    );

    await waitFor(() => expect(attachmentThumbnail).toHaveBeenCalledWith("/tmp/missing.webp"));
    expect(screen.getByText("WEBP 图片")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "打开附件 missing.webp" })).toBeEnabled();
    expect(document.querySelector(".msg__attachment-thumbnail")).toBeNull();
  });

  it("复制成功在原按钮反馈，不触发全局 Toast", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    const onToast = vi.fn();
    render(
      <ThemeProvider>
        <MessageItem
          message={message}
          streaming={false}
          cwd="/tmp"
          onToast={onToast}
        />
      </ThemeProvider>,
    );

    const copyButton = screen.getByRole("button", { name: "复制" });
    expect(copyButton).toHaveAttribute("data-chat-copy", "true");
    fireEvent.click(copyButton);

    await waitFor(() => {
      expect(writeText).toHaveBeenCalledWith("请优化这份方案");
      expect(screen.getByRole("button", { name: "已复制" })).toBeInTheDocument();
    });
    expect(onToast).not.toHaveBeenCalled();
  });
});
