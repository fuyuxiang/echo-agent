import { describe, it, expect } from "vitest";
import {
  isPreviewableUrl,
  normalizePreviewUrl,
  safeRemoteImageUrl,
} from "../browser-preview";

describe("isPreviewableUrl", () => {
  it("允许 http/https 公网", () => {
    expect(isPreviewableUrl("https://example.com")).toBe(true);
    expect(isPreviewableUrl("http://example.com/path")).toBe(true);
  });
  it("拒绝非 http(s) 协议", () => {
    expect(isPreviewableUrl("file:///etc/passwd")).toBe(false);
    expect(isPreviewableUrl("data:text/html,<x>")).toBe(false);
    expect(isPreviewableUrl("javascript:alert(1)")).toBe(false);
    expect(isPreviewableUrl("blob:https://x/y")).toBe(false);
  });
  it("拒绝回环地址", () => {
    expect(isPreviewableUrl("http://localhost")).toBe(false);
    expect(isPreviewableUrl("http://127.0.0.1")).toBe(false);
    expect(isPreviewableUrl("http://[::1]")).toBe(false);
    expect(isPreviewableUrl("http://2130706433")).toBe(false);
    expect(isPreviewableUrl("http://ipc.localhost")).toBe(false);
    expect(isPreviewableUrl("http://asset.localhost")).toBe(false);
  });
  it("拒绝内网/链路本地段", () => {
    expect(isPreviewableUrl("http://10.0.0.1")).toBe(false);
    expect(isPreviewableUrl("http://192.168.1.1")).toBe(false);
    expect(isPreviewableUrl("http://169.254.1.1")).toBe(false);
    expect(isPreviewableUrl("http://172.16.0.1")).toBe(false);
    expect(isPreviewableUrl("http://172.31.255.255")).toBe(false);
    expect(isPreviewableUrl("http://100.64.0.1")).toBe(false);
    expect(isPreviewableUrl("http://[fd00::1]")).toBe(false);
    expect(isPreviewableUrl("http://[fe80::1]")).toBe(false);
    expect(isPreviewableUrl("http://printer.local")).toBe(false);
  });
  it("172.15/172.32 不在内网段,允许", () => {
    expect(isPreviewableUrl("http://172.15.0.1")).toBe(true);
    expect(isPreviewableUrl("http://172.32.0.1")).toBe(true);
  });
  it("无效 URL 返回 false", () => {
    expect(isPreviewableUrl("not a url")).toBe(false);
    expect(isPreviewableUrl("")).toBe(false);
  });
  it("拒绝账号凭据、尾点内网名和过长 URL", () => {
    expect(isPreviewableUrl("https://user:secret@example.com")).toBe(false);
    expect(isPreviewableUrl("https://localhost./resource")).toBe(false);
    expect(isPreviewableUrl(`https://example.com/${"x".repeat(4096)}`)).toBe(false);
  });
});

describe("safeRemoteImageUrl", () => {
  it("默认拒绝远程目录图片，避免目录源触发内网访问", () => {
    expect(safeRemoteImageUrl("https://cdn.example.com/avatar.png")).toBeUndefined();
    expect(safeRemoteImageUrl("http://cdn.example.com/avatar.png")).toBeUndefined();
    expect(safeRemoteImageUrl("https://127.0.0.1/avatar.png")).toBeUndefined();
    expect(safeRemoteImageUrl("https://user:secret@example.com/avatar.png")).toBeUndefined();
  });
});

describe("normalizePreviewUrl", () => {
  it("补全 https 协议", () => {
    expect(normalizePreviewUrl("example.com")).toBe("https://example.com/");
  });
  it("保留已有协议", () => {
    expect(normalizePreviewUrl("http://example.com")).toBe("http://example.com/");
  });
  it("保留文档锚点", () => {
    expect(normalizePreviewUrl("https://example.com/a#x")).toBe("https://example.com/a#x");
  });
  it("补全本地开发地址", () => {
    expect(normalizePreviewUrl("localhost:5173/settings")).toBe("http://localhost:5173/settings");
    expect(normalizePreviewUrl("[::1]:5173")).toBe("http://[::1]:5173/");
  });
  it("拒绝其他协议或明文凭据", () => {
    expect(normalizePreviewUrl("https://user:password@example.com")).toBeNull();
    expect(normalizePreviewUrl("javascript:alert(1)")).toBeNull();
    expect(normalizePreviewUrl("file:///x")).toBeNull();
  });
  it("空串返回 null", () => {
    expect(normalizePreviewUrl("")).toBeNull();
    expect(normalizePreviewUrl("   ")).toBeNull();
  });
});
