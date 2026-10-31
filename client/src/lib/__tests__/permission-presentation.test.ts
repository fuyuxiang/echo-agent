import { describe, expect, it } from "vitest";
import { permissionPresentation } from "../permission-presentation";
import type { PermissionRequest } from "../types";

const request: PermissionRequest = {
  requestId: "approval", sessionId: "session", toolCallId: "shell", toolKind: "execute",
  title: "Execute `python3 check.py`", rawInput: { command: "python3 check.py" }, options: [],
};

describe("permission presentation", () => {
  it("命令仅展示一次，标题本地化，原始请求不会被改写", () => {
    const snapshot = JSON.stringify(request);
    const result = permissionPresentation(request);
    expect(result.title).toBe("是否允许运行这条命令？");
    expect(result.preview).toBe("python3 check.py");
    expect(result.details).toBeUndefined();
    expect(result.purpose).toBeUndefined();
    expect(JSON.stringify(request)).toBe(snapshot);
  });

  it("优先显示真实命令和已有操作说明，其他参数保留而不重复内容", () => {
    const result = permissionPresentation({ ...request, rawInput: {
      path: "/important/data", cwd: "/workspace", command: "python3 check.py", cmd: "python3 check.py",
      description: "核对交付文件，继续整理检查结果。", timeout: 120000,
    } });
    expect(result.preview).toBe("python3 check.py");
    expect(result.purpose).toBe("核对交付文件，继续整理检查结果。");
    expect(result.details).toEqual({ path: "/important/data", cwd: "/workspace", timeout: 120000 });
  });

  it.each(["execute", "bash", "shell", "run_terminal_command", "execute_command", "terminal"])("兼容终端工具名称 %s", toolKind => {
    expect(permissionPresentation({ ...request, toolKind }).category).toBe("terminal");
  });

  it("命令看起来像读取文件也不猜测用途或安全级别，保留不同的备用参数", () => {
    const result = permissionPresentation({ ...request, title: "Execute `Get-Content $f; Remove-Item $f`", rawInput: {
      command: "Get-Content $f; Remove-Item $f", cmd: "other command",
    } });
    expect(result.title).toBe("是否允许运行这条命令？");
    expect(result.purpose).toBeUndefined();
    expect(result.details).toEqual({ cmd: "other command" });
  });

  it("UseTool 包装中的命令与说明可读，包装身份和未展示参数保留", () => {
    const result = permissionPresentation({ ...request, toolKind: "other", rawInput: {
      variant: "UseTool", tool_name: "run_terminal_command", tool_input: {
        command: "python3 check.py", description: "核对交付文件。", cwd: "/workspace",
      },
    } });
    expect(result.category).toBe("terminal");
    expect(result.preview).toBe("python3 check.py");
    expect(result.purpose).toBe("核对交付文件。");
    expect(result.details).toEqual({ variant: "UseTool", tool_name: "run_terminal_command", tool_input: { cwd: "/workspace" } });
  });

  it("没有 rawInput 的旧请求仍可核对标题中的完整命令", () => {
    const result = permissionPresentation({ ...request, rawInput: undefined });
    expect(result.preview).toBe("python3 check.py");
    expect(result.details).toBeUndefined();
  });

  it("后端截断的命令标题不再重复展示，独立的附加请求信息仍保留", () => {
    const command = 'Get-Content "C:\\Users\\Taurus\\.echo-agent\\' + "encoded-cache/".repeat(50) + 'output.txt"';
    const result = permissionPresentation({ ...request, title: `Execute \`${command.slice(0, 500)}\n[truncated]`, rawInput: { command } });
    expect(result.preview).toBe(command);
    expect(result.details).toBeUndefined();
    const withContext = permissionPresentation({ ...request, title: `Execute \`${command}\` outside the workspace`, rawInput: { command } });
    expect(withContext.details).toEqual({ 原始请求: "Execute `[见上方操作内容]` outside the workspace" });
  });

  it("保留有意义的中文请求标题、待写入内容和非字符串参数", () => {
    const result = permissionPresentation({ ...request, toolKind: "edit", title: "是否允许写入本轮交付记录？",
      rawInput: { path: "/workspace/result.md", content: "交付正文", create: true } });
    expect(result.title).toBe("是否允许写入本轮交付记录？");
    expect(result.preview).toBe("/workspace/result.md");
    expect(result.details).toEqual({ content: "交付正文", create: true });
    expect(permissionPresentation({ ...request, toolKind: "other", rawInput: [false, 0] }).details).toEqual(expect.objectContaining({ 参数: [false, 0] }));
  });

  it("未知请求内容可以核对，过长说明不挤占默认视图且不丢失原文", () => {
    const title = "Custom operation needs access";
    expect(permissionPresentation({ ...request, toolKind: "other", title, rawInput: undefined }).preview).toBe(title);
    const description = "详细操作说明".repeat(100);
    const result = permissionPresentation({ ...request, rawInput: { command: "python3 check.py", description } });
    expect(result.purpose).toBeUndefined();
    expect(result.details).toEqual({ description });
  });
});
