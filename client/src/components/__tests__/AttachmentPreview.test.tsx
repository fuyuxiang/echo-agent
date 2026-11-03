import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import JSZip from "jszip";
import { Composer } from "../Composer";
import { AttachmentList } from "../AttachmentList";
import { saveDraftAttachments } from "@/lib/draft-attachments";
import { attachmentSizeLabel, loadAttachmentPreview } from "@/lib/attachment-preview";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/webview", () => ({ getCurrentWebview: () => ({ onDragDropEvent: async () => () => {} }) }));

async function native(command: string, args: Record<string, unknown> = {}): Promise<unknown> {
  if (command === "attachment_thumbnail") return "thumbnail-jpeg";
  if (command === "attachment_preview_data") return "full-original";
  if (command === "path_stat") return { exists: true, kind: "file", absolute: args.path };
  if (command === "read_text_file") return `正文：${args.path}`;
  if (command === "filesystem_attachment_stats") return { files: (args.paths as string[]).map(path => ({ path, inputPath: path, sizeBytes: 4096 })), rejected: [] };
  if (command === "open_path" || command === "discard_attachment_blob") return;
  throw new Error(`Unexpected command ${command}`);
}

beforeEach(() => {
  localStorage.clear();
  vi.mocked(invoke).mockReset().mockImplementation(native as typeof invoke);
});

describe("attachment previews", () => {
  it("loads a thumbnail without reading or opening the original until clicked", async () => {
    render(<AttachmentList paths={["/draft/lazy.png"]} sizes={new Map([["/draft/lazy.png", 2048]])} />);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("attachment_thumbnail", { path: "/draft/lazy.png" }));
    expect(invoke).not.toHaveBeenCalledWith("attachment_preview_data", expect.anything());
    expect(screen.getByText("PNG 图片 · 2 KB")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "预览附件 lazy.png" }));
    const dialog = await screen.findByRole("dialog", { name: "lazy.png" });
    expect(await within(dialog).findByRole("img", { name: "lazy.png" })).toHaveAttribute("src", "data:image/png;base64,full-original");
    expect(invoke).not.toHaveBeenCalledWith("open_path", expect.anything());
  });

  it("preserves the draft and selection, traps focus, and restores the preview trigger on Escape", async () => {
    const onSend = vi.fn();
    render(<Composer streaming={false} onSend={onSend} onCancel={vi.fn()} externalText="核对这张截图" externalAttachments={["/draft/selection.png"]} externalTextNonce={1} />);
    const input = screen.getByRole("textbox") as HTMLTextAreaElement;
    input.focus(); input.setSelectionRange(2, 5);
    const trigger = screen.getByRole("button", { name: "预览附件 selection.png" });
    trigger.focus(); fireEvent.click(trigger);
    await screen.findByRole("dialog");
    await waitFor(() => expect(screen.getByRole("button", { name: "关闭附件预览" })).toHaveFocus());
    const last = screen.getByRole("button", { name: "用系统应用打开" });
    last.focus(); fireEvent.keyDown(last, { key: "Tab" });
    expect(screen.getByRole("button", { name: "关闭附件预览" })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    expect(input.value).toBe("核对这张截图");
    expect([input.selectionStart, input.selectionEnd]).toEqual([2, 5]);
    expect(onSend).not.toHaveBeenCalled();
  });

  it("removal does not open a preview and returns focus to the empty composer", () => {
    render(<Composer streaming={false} onSend={vi.fn()} onCancel={vi.fn()} externalText="保留要求" externalAttachments={["/draft/remove.txt"]} externalTextNonce={1} />);
    fireEvent.click(screen.getByRole("button", { name: "移除附件" }));
    expect(screen.queryByRole("list", { name: "待发送附件" })).not.toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByRole("textbox")).toHaveValue("保留要求");
    expect(screen.getByRole("textbox")).toHaveFocus();
  });

  it("expands long text for editing without submitting and keeps changes and cursor on close", async () => {
    const onSend = vi.fn();
    const draft = "完整核对输入内容。".repeat(50);
    render(<Composer streaming={false} onSend={onSend} onCancel={vi.fn()} externalText={draft} externalTextNonce={1} />);
    const original = screen.getByRole("textbox") as HTMLTextAreaElement;
    original.setSelectionRange(3, 8);
    fireEvent.click(screen.getByRole("button", { name: "展开编辑" }));
    const editor = screen.getByRole("textbox", { name: "完整输入内容" }) as HTMLTextAreaElement;
    expect(editor.selectionStart).toBe(3);
    fireEvent.change(editor, { target: { value: `${draft}\n补充说明` } });
    editor.setSelectionRange(10, 10);
    fireEvent.keyDown(editor, { key: "Enter" });
    expect(onSend).not.toHaveBeenCalled();
    fireEvent.keyDown(editor, { key: "Escape" });
    expect(original).toHaveValue(`${draft}\n补充说明`);
    expect(original).toHaveFocus();
    expect(original.selectionStart).toBe(10);
  });

  it("changes attachments inside one dialog and ignores the prior file's late response", async () => {
    let finish!: (content: string) => void;
    vi.mocked(invoke).mockImplementation(((command: string, args: Record<string, unknown>) => command === "read_text_file" && args.path === "/draft/slow.txt"
      ? new Promise<string>(resolve => { finish = resolve; }) : native(command, args)) as typeof invoke);
    render(<AttachmentList paths={["/draft/slow.txt", "/draft/next.txt"]} />);
    fireEvent.click(screen.getByRole("button", { name: "预览附件 slow.txt" }));
    await waitFor(() => expect(finish).toBeTypeOf("function"));
    fireEvent.click(screen.getByRole("button", { name: "下一个附件" }));
    const dialog = await screen.findByRole("dialog", { name: "next.txt" });
    expect(await within(dialog).findByText("正文：/draft/next.txt", {}, { timeout: 10000 })).toBeInTheDocument();
    await act(async () => finish("旧内容不应出现"));
    expect(screen.queryByText("旧内容不应出现")).not.toBeInTheDocument();
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
    expect(screen.getByRole("button", { name: "下一个附件" })).toBeDisabled();
    fireEvent.keyDown(dialog, { key: "ArrowLeft", altKey: true });
    await screen.findByRole("dialog", { name: "slow.txt" });
  });

  it("closes a preview on scope change without clearing either scope's draft attachments", async () => {
    saveDraftAttachments("old-preview", ["/draft/old.txt"]);
    saveDraftAttachments("new-preview", ["/draft/new.txt"]);
    const props = { streaming: false, onSend: vi.fn(), onCancel: vi.fn() };
    const view = render(<Composer {...props} draftKey="old-preview" draft="旧要求" />);
    fireEvent.click(screen.getByRole("button", { name: "预览附件 old.txt" }));
    await screen.findByRole("dialog", { name: "old.txt" });
    view.rerender(<Composer {...props} draftKey="new-preview" draft="新要求" />);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByRole("textbox")).toHaveValue("新要求");
    expect(screen.getByRole("button", { name: "预览附件 new.txt" })).toBeInTheDocument();
    view.rerender(<Composer {...props} draftKey="old-preview" draft="旧要求" />);
    expect(screen.getByRole("button", { name: "预览附件 old.txt" })).toBeInTheDocument();
  });

  it("removing or replacing the selected attachment closes its preview permanently", async () => {
    const view = render(<AttachmentList paths={["/draft/a.txt"]} />);
    fireEvent.click(screen.getByRole("button", { name: "预览附件 a.txt" }));
    await screen.findByRole("dialog");
    view.rerender(<AttachmentList paths={[]} />);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    view.rerender(<AttachmentList paths={["/draft/a.txt"]} />);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("recovers from native read errors and image decode errors, with an explicit system-open fallback", async () => {
    let failed = false;
    vi.mocked(invoke).mockImplementation(((command: string, args: Record<string, unknown>) => {
      if (command === "attachment_preview_data" && !failed) { failed = true; return Promise.reject(new Error("路径不存在")); }
      return native(command, args);
    }) as typeof invoke);
    render(<AttachmentList paths={["/draft/retry.png"]} />);
    fireEvent.click(screen.getByRole("button", { name: "预览附件 retry.png" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("已不存在");
    fireEvent.click(screen.getByRole("button", { name: "重新加载" }));
    const image = await screen.findByRole("img", { name: "retry.png" });
    fireEvent.error(image);
    expect(await screen.findByRole("alert")).toHaveTextContent("图片无法显示");
    fireEvent.click(screen.getByRole("button", { name: "用系统应用打开" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("open_path", { path: "/draft/retry.png", cwd: null }));
  });

  it("renders a PDF inside the preview instead of using the image thumbnail", async () => {
    render(<AttachmentList paths={["/draft/document.pdf"]} />);
    fireEvent.click(screen.getByRole("button", { name: "预览附件 document.pdf" }));
    const dialog = await screen.findByRole("dialog");
    await waitFor(() => expect(dialog.querySelector("iframe")).toHaveAttribute("src", "data:application/pdf;base64,full-original"));
  });

  it("renders readable Word content with the existing read-only document preview", async () => {
    const zip = new JSZip();
    zip.file("word/document.xml", '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>发送前核对文档内容</w:t></w:r></w:p></w:body></w:document>');
    const binary = await zip.generateAsync({ type: "base64" });
    vi.mocked(invoke).mockImplementation(((command: string, args: Record<string, unknown>) => command === "attachment_preview_data" ? Promise.resolve(binary) : native(command, args)) as typeof invoke);
    render(<AttachmentList paths={["/draft/check.docx"]} />);
    fireEvent.click(screen.getByRole("button", { name: "预览附件 check.docx" }));
    const dialog = await screen.findByRole("dialog");
    expect(await within(dialog).findByText("发送前核对文档内容")).toBeInTheDocument();
    expect(dialog.querySelector('[contenteditable="true"]')).toBeNull();
  });

  it("reports system-open failures in the dialog and retains the preview", async () => {
    vi.mocked(invoke).mockImplementation(((command: string, args: Record<string, unknown>) => command === "open_path" ? Promise.reject(new Error("permission denied")) : native(command, args)) as typeof invoke);
    render(<AttachmentList paths={["/draft/open.txt"]} />);
    fireEvent.click(screen.getByRole("button", { name: "预览附件 open.txt" }));
    await screen.findByRole("dialog");
    fireEvent.click(screen.getByRole("button", { name: "用系统应用打开" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("预览受限");
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });
});

describe("bounded preview transport", () => {
  it("reads a native-authorized text prefix and uses the canonical path", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({ absolute: "/workspace/canonical.ts", exists: true, kind: "file" }).mockResolvedValueOnce("const a = 1;");
    expect(await loadAttachmentPreview("relative.ts", "/workspace")).toBe("const a = 1;");
    expect(invoke).toHaveBeenLastCalledWith("read_text_file", { path: "/workspace/canonical.ts", cwd: null, maxBytes: 262144 });
  });
  it("never reads nonexistent files or directories as attachment data", async () => {
    for (const stat of [{ exists: false, kind: "missing" }, { exists: true, kind: "directory" }]) {
      vi.mocked(invoke).mockResolvedValueOnce(stat);
      await expect(loadAttachmentPreview("/draft/missing.png")).rejects.toThrow("不存在");
    }
    expect(invoke).not.toHaveBeenCalledWith("attachment_preview_data", expect.anything());
  });
  it("formats known sizes without invented metadata", () => {
    expect([undefined, -1, Number.NaN, 0, 1024, 1536 * 1024].map(attachmentSizeLabel)).toEqual(["", "", "", "0 B", "1 KB", "1.5 MB"]);
  });
});
