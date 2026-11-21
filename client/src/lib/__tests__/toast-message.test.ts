import { describe, expect, it } from "vitest";
import { toastMessage } from "../toast-message";

describe("toastMessage", () => {
  it("keeps concise and actionable product copy, including partial success", () => {
    expect(toastMessage("已归档")).toEqual({ summary: "已归档" });
    expect(toastMessage("已导入 2 条，剩余内容未保存，请重试")).toEqual({ summary: "已导入 2 条，剩余内容未保存，请重试" });
    expect(toastMessage("Error: 名称已存在，请换一个名称")).toEqual({ summary: "名称已存在，请换一个名称" });
  });
  it.each([
    ["读取失败：ENOENT: no such file or directory", "读取失败：找不到所需文件或内容，请刷新后重试"],
    ["保存失败：EACCES (os error 13)", "保存失败：没有访问权限，请检查授权"],
    ["保存失败：写入文件 失败：HTTP 403 Forbidden <!DOCTYPE HTML PUBLIC '-//W3C//DTD HTML 4.01//EN'>", "保存失败：没有访问权限，请检查授权"],
    ["保存失败：写入文件 失败：HTTP 403 Forbidden <p>Resource not found</p>", "保存失败：没有访问权限，请检查授权"],
    ["读取失败：HTTP 404 Not Found <p>Previous request returned 403</p>", "读取失败：找不到所需文件或内容，请刷新后重试"],
    ["保存失败：HTTP 507 Insufficient Storage <p>Upstream timed out</p>", "保存失败：存储空间不足，请清理后重试"],
    ["加载失败：TypeError: Failed to fetch", "加载失败：暂时无法连接服务，请检查网络后重试"],
    ['ApiError { status: 401, message: "Unauthorized" }', "登录或模型凭据已失效，请检查设置"],
    ["Error: request timed out", "等待响应超时，请稍后重试"],
    ["保存失败：QuotaExceededError", "保存失败：存储空间不足，请清理后重试"],
    ["⚠️ Error: Failed to fetch", "暂时无法连接服务，请检查网络后重试"],
    ["Error: unexpected response", "暂时无法完成，请重试或查看详情"],
  ])("explains %s and preserves diagnostics", (raw, summary) => {
    expect(toastMessage(raw)).toEqual({ summary, details: raw });
  });
  it("shows a partial-cleanup warning before disclosing backend details", () => {
    const raw = '对话已删除，部分记忆未能清理，请到记忆页面检查。\nSqliteError { code: "BUSY" }';
    const result = toastMessage(raw);
    expect(result.summary).toBe("对话已删除，部分记忆未能清理，请到记忆页面检查。");
    expect(result.details).toBe(raw);
  });
  it("does not turn a successful export path into a failure", () => {
    expect(toastMessage("已导出 /Users/test/report.pdf").summary).toContain("已导出");
  });
  it.each([
    "已归档“HTTP 403 问题排查”",
    "已下载“TypeError.log”（2 KB）",
    "已更新“ENOENT排查.md”",
    "已归档“保存失败：HTTP 403”",
    "正在读取“EACCES 使用说明”",
    "Successfully downloaded TypeError.log",
    "下载失败：文件“TypeError.log”正在被占用，请关闭后重试",
  ])("keeps user-owned names and readable product explanations: %s", message => {
    expect(toastMessage(message)).toEqual({ summary: message });
  });
  it("uses an explicit kind for ambiguous copy and never classifies an operation's filename", () => {
    expect(toastMessage("HTTP 403 使用说明", "info")).toEqual({ summary: "HTTP 403 使用说明" });
    expect(toastMessage("HTTP 403", "error").summary).toBe("没有访问权限，请检查授权");
    expect(toastMessage("下载 TypeError.log 失败：EACCES", "error").summary)
      .toBe("下载 TypeError.log 失败：没有访问权限，请检查授权");
  });
});
