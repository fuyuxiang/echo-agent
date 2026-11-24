import { describe, expect, it } from "vitest";

import {
  FileTypeIcon,
  getIconComponentForName,
  getIconKeyForName,
} from "@/lib/file-type-icon";

describe("FileTypeIcon", () => {
  it("目录统一返回 Folder", () => {
    expect(getIconKeyForName("src", "directory")).toBe("Folder");
  });

  it("已知扩展名返回正确 lucide icon key", () => {
    expect(getIconKeyForName("a.ts", "file")).toBe("FileCode2");
    expect(getIconKeyForName("a.tsx", "file")).toBe("FileCode2");
    expect(getIconKeyForName("data.json", "file")).toBe("FileJson");
    expect(getIconKeyForName("README.md", "file")).toBe("Hash");
    expect(getIconKeyForName("image.png", "file")).toBe("Image");
    expect(getIconKeyForName("config.yaml", "file")).toBe("Settings");
    expect(getIconKeyForName("index.html", "file")).toBe("Globe");
  });

  it("未知扩展名走 default FileText", () => {
    expect(getIconKeyForName("a.zzz", "file")).toBe("FileText");
    expect(getIconKeyForName("no-extension", "file")).toBe("FileText");
  });

  it("大小写不敏感", () => {
    expect(getIconKeyForName("A.TS", "file")).toBe("FileCode2");
    expect(getIconKeyForName("data.JSON", "file")).toBe("FileJson");
  });

  it("返回 ReactElement", () => {
    const el = FileTypeIcon({ name: "a.ts", kind: "file" });
    expect(el).toBeTruthy();
    expect((el as { type?: unknown }).type).toBeTruthy();
  });

  it("getIconComponentForName 与 getIconKeyForName 一致", () => {
    const Comp = getIconComponentForName("a.ts", "file");
    expect(getIconKeyForName("a.ts", "file")).toBe(Comp.displayName ?? Comp.name);
  });
});
