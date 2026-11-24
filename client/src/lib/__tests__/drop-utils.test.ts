import { describe, it, expect } from "vitest";
import { basenameOf, extOf } from "../drop-utils";

describe("extOf", () => {
  it("取小写后缀含点", () => {
    expect(extOf("/a/b/c.TS")).toBe(".ts");
    expect(extOf("readme.MD")).toBe(".md");
  });
  it("无后缀返回空串", () => {
    expect(extOf("/a/b/Makefile")).toBe("");
    expect(extOf("noext")).toBe("");
  });
  it("只取最后一段的后缀(忽略目录里的点)", () => {
    expect(extOf("/a.b/c/file.ts")).toBe(".ts");
  });
});

describe("basenameOf", () => {
  it("正斜杠与反斜杠都能取文件名", () => {
    expect(basenameOf("/a/b/c.ts")).toBe("c.ts");
    expect(basenameOf("C:\\a\\b\\c.ts")).toBe("c.ts");
  });
  it("无目录返回原文", () => {
    expect(basenameOf("c.ts")).toBe("c.ts");
  });
});
