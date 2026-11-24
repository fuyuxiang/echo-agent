import { describe, it, expect } from "vitest";
import { blobToBytes, extractFilesFromClipboard } from "../clipboard-paste";
import { AttachmentKind } from "../user-message";

class FakeFile {
  name: string;
  type: string;
  size: number;
  content: Uint8Array;
  constructor(name: string, type: string, content = "x") {
    this.name = name;
    this.type = type;
    this.content = new TextEncoder().encode(content);
    this.size = this.content.byteLength;
  }
  async arrayBuffer(): Promise<ArrayBuffer> {
    // FakeFile 仅用于单测,避免依赖运行时 SharedArrayBuffer 判定。
    return this.content.buffer.slice(
      this.content.byteOffset,
      this.content.byteOffset + this.content.byteLength,
    ) as ArrayBuffer;
  }
}

class FakeDataTransferItem {
  kind: "file" | "string";
  type: string;
  file: FakeFile | null;
  constructor(kind: "file" | "string", type: string, file: FakeFile | null = null) {
    this.kind = kind;
    this.type = type;
    this.file = file;
  }
  getAsFile(): File | null {
    if (this.kind !== "file" || !this.file) return null;
    return this.file as unknown as File;
  }
}

describe("extractFilesFromClipboard", () => {
  it("returns empty list when items is null/undefined/empty", () => {
    expect(extractFilesFromClipboard(null)).toEqual([]);
    expect(extractFilesFromClipboard(undefined)).toEqual([]);
    expect(extractFilesFromClipboard([])).toEqual([]);
  });

  it("keeps files and reports attachment kinds for the composer", () => {
    const imageItem = new FakeDataTransferItem(
      "file",
      "image/png",
      new FakeFile("a.png", "image/png"),
    );
    const pdfItem = new FakeDataTransferItem(
      "file",
      "application/pdf",
      new FakeFile("b.pdf", "application/pdf"),
    );
    const textItem = new FakeDataTransferItem("string", "text/plain");
    const unsupported = new FakeDataTransferItem(
      "file",
      "image/bmp",
      new FakeFile("c.bmp", "image/bmp"),
    );

    const out = extractFilesFromClipboard([imageItem, pdfItem, textItem, unsupported]);
    expect(out.map(item => item.kind)).toEqual([AttachmentKind.Image, AttachmentKind.Document, AttachmentKind.Unsupported]);
    expect(out[0].blob).toBe(imageItem.file);
    expect(out[1].blob).toBe(pdfItem.file);
  });

  it("supports pasting several images at once", () => {
    const items = [
      new FakeDataTransferItem(
        "file",
        "image/png",
        new FakeFile("a.png", "image/png"),
      ),
      new FakeDataTransferItem(
        "file",
        "image/jpeg",
        new FakeFile("b.jpg", "image/jpeg"),
      ),
    ];
    const out = extractFilesFromClipboard(items);
    expect(out.map((i) => i.mime)).toEqual(["image/png", "image/jpeg"]);
    expect(out.every(item => item.kind === AttachmentKind.Image)).toBe(true);
  });

  it("skips file items whose getAsFile() returns null", () => {
    const brokenItem = new FakeDataTransferItem("file", "image/png", null);
    const realItem = new FakeDataTransferItem(
      "file",
      "image/png",
      new FakeFile("a.png", "image/png"),
    );
    const out = extractFilesFromClipboard([brokenItem, realItem]);
    expect(out).toHaveLength(1);
    expect(out[0].blob).toBe(realItem.file);
  });

  it("gives unnamed PNGs distinct names with the supported extension", () => {
    const item = new FakeDataTransferItem(
      "file",
      "image/png",
      new FakeFile("", "image/png"),
    );
    const items = extractFilesFromClipboard([item, item]);
    expect(items.map(i => i.suggestedName)).toEqual(["image.png", "2-image.png"]);
    expect(items.every(i => i.kind === AttachmentKind.Image)).toBe(true);
  });
});

describe("blobToBytes", () => {
  it("returns the full blob bytes", async () => {
    const blob = new FakeFile("a", "image/png", "hello") as unknown as Blob;
    const bytes = await blobToBytes(blob);
    expect(Array.from(bytes)).toEqual([
      "h".charCodeAt(0),
      "e".charCodeAt(0),
      "l".charCodeAt(0),
      "l".charCodeAt(0),
      "o".charCodeAt(0),
    ]);
  });

  it("propagates errors from arrayBuffer()", async () => {
    const blob = {
      arrayBuffer: () => Promise.reject(new Error("disk fail")),
    } as unknown as Blob;
    await expect(blobToBytes(blob)).rejects.toThrow("disk fail");
  });
});
