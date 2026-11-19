import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Composer } from "../Composer";
import { invoke } from "@tauri-apps/api/core";

// Tauri shell 接口被 Composer 调用,这里集中拦截以驱动 paste / drop 流程。
// Tauri 2 真实的 `onDragDropEvent` 回调收到的是 `event.payload = { type, paths?, position? }`,
// 所以测试也按真实结构喂事件。
type DragDropPayload =
  | { type: "enter"; paths: string[]; position: { x: number; y: number } }
  | { type: "over"; position: { x: number; y: number } }
  | { type: "drop"; paths: string[]; position: { x: number; y: number } }
  | { type: "leave" };

let dragDropCallback: ((event: { payload: DragDropPayload }) => void) | null = null;
const unlistenMock = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

async function defaultInvoke(
  cmd: string,
  args: { suggestedName?: string; mime?: string; paths?: string[] },
) {
    if (cmd === "save_attachment_blob") {
      // 保留 suggestedName 原值,这样 chip 显示的 basename 跟测试期望一致。
      return `/fake/appdata/clipboard-images/${args.suggestedName ?? "image.png"}`;
    }
    if (cmd === "filesystem_attachment_stats") {
      return {
        files: (args.paths ?? []).map((path) => ({
          inputPath: path,
          path,
          sizeBytes: 4,
        })),
        rejected: [],
      };
    }
    return undefined;
}

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: (cb: (event: { payload: DragDropPayload }) => void) => {
      dragDropCallback = cb;
      return Promise.resolve(() => unlistenMock());
    },
  }),
}));

const base = { streaming: false, onSend: vi.fn(), onCancel: vi.fn() };

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockImplementation(defaultInvoke as typeof invoke);
});

function makeImageFile(name: string): File {
  return new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], name, { type: "image/png" });
}

function makeSizedImage(name: string, size: number, arrayBuffer = vi.fn()) {
  return {
    name,
    size,
    type: "image/png",
    arrayBuffer,
  } as unknown as Blob;
}

/**
 * jsdom 不支持 ClipboardEvent 构造器。testing-library/dom v10 已经知道这点
 * (events.js:76-97),会把 init.clipboardData 通过 defineProperty 注入到事件
 * 实例上,所以这里直接传 { clipboardData } 即可。
 */
function firePasteWith(
  target: HTMLElement,
  items: Array<{ kind: string; type: string; getAsFile(): File | Blob | null }>,
) {
  fireEvent.paste(target, {
    clipboardData: { items: items as unknown as DataTransferItemList },
  });
}

describe("Composer clipboard paste of images", () => {
  beforeEach(() => {
    dragDropCallback = null;
    base.onSend.mockReset();
  });

  it("pasting an image calls save_attachment_blob and adds it to attachments", async () => {
    render(<Composer {...base} />);
    const textarea = screen.getByRole("textbox");
    firePasteWith(textarea, [
      { kind: "file", type: "image/png", getAsFile: () => makeImageFile("shot.png") },
    ]);
    expect(await screen.findByText("shot.png")).toBeInTheDocument();
  });

  it("pasting multiple images at once adds all of them", async () => {
    render(<Composer {...base} />);
    const textarea = screen.getByRole("textbox");
    firePasteWith(textarea, [
      { kind: "file", type: "image/png", getAsFile: () => makeImageFile("a.png") },
      { kind: "file", type: "image/jpeg", getAsFile: () => makeImageFile("b.jpg") },
    ]);
    expect(await screen.findByText("a.png")).toBeInTheDocument();
    expect(await screen.findByText("b.jpg")).toBeInTheDocument();
  });

  it("pasting a PDF file is accepted as a document attachment", async () => {
    // Bug contract: 之前 Composer 把 PDF / 代码文件 / 文本当作非图片
    // 直接 `filter(isImageAttachment)` 静默丢弃,用户毫无反馈。新契约:
    // 文档类(以及代码 / 文本 / 数据)走和图片相同的「save → 显示 chip」
    // 路径。
    vi.mocked(invoke).mockImplementationOnce(async (cmd, args?: unknown) => {
      if (cmd === "save_attachment_blob") {
        const suggestedName =
          (args as { suggestedName?: string } | undefined)?.suggestedName ?? "file";
        return `/fake/appdata/clipboard-images/${suggestedName}`;
      }
      return undefined;
    });
    render(<Composer {...base} />);
    const textarea = screen.getByRole("textbox");
    firePasteWith(textarea, [
      {
        kind: "file",
        type: "application/pdf",
        getAsFile: () => new File(["pdf"], "x.pdf", { type: "application/pdf" }),
      },
    ]);
    expect(await screen.findByText("x.pdf")).toBeInTheDocument();
  });

  it("pasting a text-only file (.txt / .md / .ts) is accepted", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd, args?: unknown) => {
      if (cmd === "save_attachment_blob") {
        const suggestedName =
          (args as { suggestedName?: string } | undefined)?.suggestedName ?? "file";
        return `/fake/appdata/clipboard-images/${suggestedName}`;
      }
      return undefined;
    });
    render(<Composer {...base} />);
    const textarea = screen.getByRole("textbox");
    firePasteWith(textarea, [
      {
        kind: "file",
        type: "text/plain",
        getAsFile: () => new File(["hello"], "notes.txt", { type: "text/plain" }),
      },
      {
        kind: "file",
        type: "text/markdown",
        getAsFile: () => new File(["# md"], "doc.md", { type: "text/markdown" }),
      },
      {
        kind: "file",
        type: "text/x-typescript",
        getAsFile: () => new File(["const x = 1"], "code.ts", { type: "text/x-typescript" }),
      },
    ]);
    expect(await screen.findByText("notes.txt")).toBeInTheDocument();
    expect(await screen.findByText("doc.md")).toBeInTheDocument();
    expect(await screen.findByText("code.ts")).toBeInTheDocument();
  });

  it("pasting an executable file (.exe) is rejected with a toast", () => {
    const onToast = vi.fn();
    render(<Composer {...base} onToast={onToast} />);
    firePasteWith(screen.getByRole("textbox"), [
      {
        kind: "file",
        type: "application/octet-stream",
        getAsFile: () => new File(["MZ"], "evil.exe", { type: "application/octet-stream" }),
      },
    ]);
    expect(screen.queryByText("evil.exe")).toBeNull();
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("不支持"));
  });

  it("pasting mixed text + image keeps the textarea content unchanged and adds the image", async () => {
    render(<Composer {...base} />);
    const textarea = screen.getByRole("textbox") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "看一下这张图" } });
    firePasteWith(textarea, [
      { kind: "file", type: "image/png", getAsFile: () => makeImageFile("ctx.png") },
    ]);
    expect(await screen.findByText("ctx.png")).toBeInTheDocument();
    // jsdom 不会模拟默认粘贴到 textarea 的行为,所以这里只断言处理器不破坏现有内容。
    expect(textarea.value).toBe("看一下这张图");
  });

  it("save_attachment_blob failure surfaces a toast and adds nothing", async () => {
    const onToast = vi.fn();
    vi.mocked(invoke).mockImplementationOnce(async () => {
      throw new Error("disk full");
    });
    render(<Composer {...base} onToast={onToast} />);
    const textarea = screen.getByRole("textbox");
    firePasteWith(textarea, [
      { kind: "file", type: "image/png", getAsFile: () => makeImageFile("fail.png") },
    ]);
    await waitFor(() => expect(onToast).toHaveBeenCalled());
    expect(screen.queryByText("fail.png")).toBeNull();
  });

  it("单张图片超过 20MB 时在读取二进制前拒绝", async () => {
    const onToast = vi.fn();
    const arrayBuffer = vi.fn();
    render(<Composer {...base} onToast={onToast} />);

    firePasteWith(screen.getByRole("textbox"), [
      {
        kind: "file",
        type: "image/png",
        getAsFile: () => makeSizedImage("large.png", 20 * 1024 * 1024 + 1, arrayBuffer),
      },
    ]);

    await waitFor(() => {
      expect(onToast).toHaveBeenCalledWith(expect.stringContaining("超过 20MB"));
    });
    expect(arrayBuffer).not.toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalledWith("save_attachment_blob", expect.anything());
  });

  it("一次粘贴超过 20 个附件时不读取文件", async () => {
    const onToast = vi.fn();
    const arrayBuffer = vi.fn();
    render(<Composer {...base} onToast={onToast} />);
    const items = Array.from({ length: 21 }, (_, index) => ({
      kind: "file",
      type: "image/png",
      getAsFile: () => makeSizedImage(`image-${index}.png`, 1, arrayBuffer),
    }));

    firePasteWith(screen.getByRole("textbox"), items);

    await waitFor(() => {
      expect(onToast).toHaveBeenCalledWith(expect.stringContaining("附件最多 20 个"));
    });
    expect(arrayBuffer).not.toHaveBeenCalled();
  });

  it("一次粘贴总大小超过 64MB 时不读取文件", async () => {
    const onToast = vi.fn();
    const arrayBuffer = vi.fn();
    render(<Composer {...base} onToast={onToast} />);
    const items = Array.from({ length: 4 }, (_, index) => ({
      kind: "file",
      type: "image/png",
      getAsFile: () => makeSizedImage(`image-${index}.png`, 17 * 1024 * 1024, arrayBuffer),
    }));

    firePasteWith(screen.getByRole("textbox"), items);

    await waitFor(() => {
      expect(onToast).toHaveBeenCalledWith(expect.stringContaining("附件总大小最多 64MB"));
    });
    expect(arrayBuffer).not.toHaveBeenCalled();
  });

  it("并发粘贴完成时仍严格限制为 20 个附件", async () => {
    const onToast = vi.fn();
    render(<Composer {...base} onToast={onToast} />);
    const textarea = screen.getByRole("textbox");
    const makeItems = (prefix: string) => Array.from({ length: 11 }, (_, index) => ({
      kind: "file",
      type: "image/png",
      getAsFile: () => makeImageFile(`${prefix}-${index}.png`),
    }));

    firePasteWith(textarea, makeItems("first"));
    firePasteWith(textarea, makeItems("second"));

    await waitFor(() => {
      expect(screen.getAllByRole("button", { name: "移除附件" })).toHaveLength(20);
    });
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("discard_attachment_blob", expect.objectContaining({
        path: expect.stringContaining(".png"),
      }));
    });
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("附件最多 20 个"));
  });

  it("并发粘贴完成时使用最新附件总大小，不能绕过 64MB 上限", async () => {
    const onToast = vi.fn();
    render(<Composer {...base} onToast={onToast} />);
    const textarea = screen.getByRole("textbox");
    const makeItems = (prefix: string) => Array.from({ length: 4 }, (_, index) => ({
      kind: "file",
      type: "image/png",
      getAsFile: () => makeSizedImage(`${prefix}-${index}.png`, 9 * 1024 * 1024),
    }));

    firePasteWith(textarea, makeItems("first"));
    firePasteWith(textarea, makeItems("second"));

    await waitFor(() => {
      expect(screen.getAllByRole("button", { name: "移除附件" })).toHaveLength(7);
    });
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("附件总大小最多 64MB"));
  });

  it("移除未发送的粘贴图片时删除临时文件", async () => {
    render(<Composer {...base} />);
    firePasteWith(screen.getByRole("textbox"), [
      { kind: "file", type: "image/png", getAsFile: () => makeImageFile("remove.png") },
    ]);

    await screen.findByText("remove.png");
    fireEvent.click(screen.getByRole("button", { name: "移除附件" }));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("discard_attachment_blob", {
        path: "/fake/appdata/clipboard-images/remove.png",
      });
    });
  });

  it("组件卸载时清理未发送的粘贴图片", async () => {
    const view = render(<Composer {...base} />);
    firePasteWith(screen.getByRole("textbox"), [
      { kind: "file", type: "image/png", getAsFile: () => makeImageFile("draft.png") },
    ]);
    await screen.findByText("draft.png");

    view.unmount();

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("discard_attachment_blob", {
        path: "/fake/appdata/clipboard-images/draft.png",
      });
    });
  });

  it("历史消息恢复的附件移除时不删除原文件", async () => {
    render(
      <Composer
        {...base}
        externalText="重新发送"
        externalAttachments={["/fake/appdata/clipboard-images/history.png"]}
        externalTextNonce={1}
      />,
    );
    await screen.findByText("history.png");
    vi.mocked(invoke).mockClear();

    fireEvent.click(screen.getByRole("button", { name: "移除附件" }));

    expect(invoke).not.toHaveBeenCalledWith("discard_attachment_blob", expect.anything());
  });

  it("发送成功后保留已转交给会话的粘贴图片", async () => {
    base.onSend.mockResolvedValue(true);
    render(<Composer {...base} />);
    firePasteWith(screen.getByRole("textbox"), [
      { kind: "file", type: "image/png", getAsFile: () => makeImageFile("sent.png") },
    ]);
    await screen.findByText("sent.png");
    vi.mocked(invoke).mockClear();

    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(base.onSend).toHaveBeenCalled());
    await waitFor(() => expect(screen.queryByText("sent.png")).not.toBeInTheDocument());
    expect(invoke).not.toHaveBeenCalledWith("discard_attachment_blob", expect.anything());
  });

  it("发送被拒绝后保留附件，之后移除仍会清理临时文件", async () => {
    base.onSend.mockResolvedValue(false);
    render(<Composer {...base} />);
    firePasteWith(screen.getByRole("textbox"), [
      { kind: "file", type: "image/png", getAsFile: () => makeImageFile("retry.png") },
    ]);
    await screen.findByText("retry.png");
    vi.mocked(invoke).mockClear();

    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(base.onSend).toHaveBeenCalled());
    expect(screen.getByText("retry.png")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "移除附件" }));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("discard_attachment_blob", {
        path: "/fake/appdata/clipboard-images/retry.png",
      });
    });
  });
});

describe("Composer drag-drop via Tauri native event", () => {
  beforeEach(() => {
    dragDropCallback = null;
  });

  it("registers onDragDropEvent listener on mount and unlistens on unmount", async () => {
    const { unmount } = render(<Composer {...base} />);
    // Effect is flushed synchronously by testing-library after render.
    expect(dragDropCallback).not.toBeNull();
    unmount();
    expect(unlistenMock).toHaveBeenCalled();
  });

  it("drop with image paths adds them as attachments", async () => {
    render(<Composer {...base} />);
    expect(dragDropCallback).not.toBeNull();
    act(() => {
      dragDropCallback!({
        payload: {
          type: "drop",
          paths: ["/Users/me/Pictures/a.png", "/Users/me/Pictures/b.jpg"],
          position: { x: 0, y: 0 },
        },
      });
    });
    expect(await screen.findByText("a.png")).toBeInTheDocument();
    expect(await screen.findByText("b.jpg")).toBeInTheDocument();
  });

  it("does not attach a late native inspection result to another task", async () => {
    let resolve!: (value: unknown) => void;
    vi.mocked(invoke).mockImplementation(async (cmd, args) => {
      if (cmd === "filesystem_attachment_stats") return new Promise(accept => { resolve = accept; });
      return defaultInvoke(cmd, (args ?? {}) as Parameters<typeof defaultInvoke>[1]);
    });
    const onToast = vi.fn();
    const view = render(<Composer {...base} draftKey="drop-source" onToast={onToast} />);
    act(() => dragDropCallback!({ payload: { type: "drop", paths: ["/work/late.txt"], position: { x: 0, y: 0 } } }));
    view.rerender(<Composer {...base} draftKey="drop-target" onToast={onToast} />);
    await act(async () => resolve({ files: [{ inputPath: "/work/late.txt", path: "/work/late.txt", sizeBytes: 4 }], rejected: [] }));
    expect(screen.queryByText("late.txt")).toBeNull();
    expect(onToast).not.toHaveBeenCalled();
  });

  it("drop accepts text/code/pdf paths as attachments", async () => {
    render(<Composer {...base} />);
    expect(dragDropCallback).not.toBeNull();
    act(() => {
      dragDropCallback!({
        payload: {
          type: "drop",
          paths: ["/Users/me/notes.txt", "/Users/me/code.ts", "/Users/me/spec.pdf"],
          position: { x: 0, y: 0 },
        },
      });
    });
    expect(await screen.findByText("notes.txt")).toBeInTheDocument();
    expect(await screen.findByText("code.ts")).toBeInTheDocument();
    expect(await screen.findByText("spec.pdf")).toBeInTheDocument();
  });

  it("drop validates real file size before reporting success", async () => {
    const onToast = vi.fn();
    vi.mocked(invoke).mockImplementationOnce(async (cmd, args?: unknown) => {
      if (cmd === "filesystem_attachment_stats") {
        const path = (args as { paths: string[] }).paths[0];
        return {
          files: [{ inputPath: path, path, sizeBytes: 20 * 1024 * 1024 + 1 }],
          rejected: [],
        };
      }
      return undefined;
    });
    render(<Composer {...base} onToast={onToast} />);
    act(() => {
      dragDropCallback!({
        payload: {
          type: "drop",
          paths: ["/Users/me/too-large.pdf"],
          position: { x: 0, y: 0 },
        },
      });
    });

    await waitFor(() => expect(onToast).toHaveBeenCalledWith(expect.stringContaining("超过 20MB")));
    expect(screen.queryByText("too-large.pdf")).toBeNull();
  });

  it("drop reports duplicates instead of claiming they were added again", async () => {
    const onToast = vi.fn();
    render(<Composer {...base} onToast={onToast} />);
    const event = {
      payload: {
        type: "drop" as const,
        paths: ["/Users/me/repeat.txt"],
        position: { x: 0, y: 0 },
      },
    };
    act(() => dragDropCallback!(event));
    expect(await screen.findByText("repeat.txt")).toBeInTheDocument();
    onToast.mockClear();
    act(() => dragDropCallback!(event));

    await waitFor(() => {
      expect(onToast).toHaveBeenCalledWith(expect.stringContaining("重复文件"));
    });
    expect(screen.getAllByText("repeat.txt")).toHaveLength(1);
  });

  it("drop reports how many files were skipped via toast when unsupported mixed in", async () => {
    const onToast = vi.fn();
    render(<Composer {...base} onToast={onToast} />);
    act(() => {
      dragDropCallback!({
        payload: {
          type: "drop",
          paths: ["/Users/me/a.png", "/Users/me/evil.exe", "/Users/me/code.ts"],
          position: { x: 0, y: 0 },
        },
      });
    });
    expect(await screen.findByText("a.png")).toBeInTheDocument();
    expect(await screen.findByText("code.ts")).toBeInTheDocument();
    expect(screen.queryByText("evil.exe")).toBeNull();
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("跳过"));
  });

  it("drag-hover shows the drop overlay", () => {
    render(<Composer {...base} />);
    expect(dragDropCallback).not.toBeNull();
    act(() => {
      dragDropCallback!({
        payload: { type: "enter", paths: [], position: { x: 0, y: 0 } },
      });
    });
    expect(screen.getByText("松开添加附件")).toBeInTheDocument();
  });

  it("拖拽覆盖层提示支持的文件类型", () => {
    render(<Composer {...base} />);
    act(() => {
      dragDropCallback!({
        payload: { type: "enter", paths: [], position: { x: 0, y: 0 } },
      });
    });
    expect(
      screen.getByText(/支持图片、PDF、DOCX、XLSX、PPTX、代码、文本与数据文件/),
    ).toBeInTheDocument();
  });

  it("drag-leave hides the drop overlay", async () => {
    render(<Composer {...base} />);
    expect(dragDropCallback).not.toBeNull();
    act(() => {
      dragDropCallback!({
        payload: { type: "enter", paths: [], position: { x: 0, y: 0 } },
      });
    });
    expect(screen.getByText("松开添加附件")).toBeInTheDocument();
    act(() => {
      dragDropCallback!({ payload: { type: "leave" } });
    });
    await waitFor(() => {
      expect(screen.queryByText("松开添加附件")).toBeNull();
    });
  });
});
