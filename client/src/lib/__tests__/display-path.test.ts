import { describe, expect, it } from "vitest";
import { displayPath, isManagedTaskWorkspace, shortWorkspacePath } from "../display-path";

describe("工作目录显示", () => {
  it("长路径只在显示时移除扩展前缀，保留 UNC 语义", () => {
    const drive = String.raw`\\?\C:\projects\中文`;
    const unc = String.raw`\\?\UNC\server\share\project`;
    expect(displayPath(drive)).toBe(String.raw`C:\projects\中文`);
    expect(displayPath(unc)).toBe(String.raw`\\server\share\project`);
    expect(displayPath(String.raw`\\server\share`)).toBe(String.raw`\\server\share`);
    expect(shortWorkspacePath(drive)).toBe("projects/中文");
    expect(drive).toBe(String.raw`\\?\C:\projects\中文`);
  });

  it("只将默认工作区 Tasks 下的 UUID 目录显示为任务文件夹", () => {
    const id = "c2856c21-0e33-43a3-8541-bc06cebcdc89";
    expect(isManagedTaskWorkspace(`/Documents/EchoAgent/Tasks/${id}`, "/Documents/EchoAgent/")).toBe(true);
    expect(isManagedTaskWorkspace(`/Projects/Tasks/${id}`, "/Documents/EchoAgent")).toBe(false);
    expect(isManagedTaskWorkspace(`/Documents/EchoAgent/Tasks/${id}/output`, "/Documents/EchoAgent")).toBe(false);
    expect(isManagedTaskWorkspace("/Documents/EchoAgent/Tasks/reports", "/Documents/EchoAgent")).toBe(false);
    expect(isManagedTaskWorkspace(`\\\\?\\C:\\Users\\Me\\EchoAgent\\Tasks\\${id}`, "c:\\users\\me\\echoagent")).toBe(true);
  });
});
