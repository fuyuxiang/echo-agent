import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createHash } from "node:crypto";
import ts from "typescript";
import { beforeEach, describe, expect, it, vi } from "vitest";

const disk = { content: "before", folderCreated: false };
const request = vi.fn<(type: string, payload: unknown) => Promise<unknown>>();

// Evaluate the real bridge class with a tiny FileService provider. Loading all
// of Theia in jsdom requires Lumino browser APIs that the desktop test runner
// does not provide; this still executes the actual update/createFolder methods.
function createService() {
  const source = readFileSync(resolve(process.cwd(), "vendor/theia-platform/examples/echo-coding-bridge/src/browser/echo-file-service.ts"), "utf8");
  const javascript = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2021, experimentalDecorators: true },
  }).outputText;
  const module = { exports: {} as Record<string, unknown> };
  const imports: Record<string, unknown> = {
    "@theia/core/shared/inversify": { injectable: () => (target: unknown) => target },
    "@theia/filesystem/lib/browser/file-service": {
      FileService: class {
        modifiedSince() { return false; }
        async update(_resource: unknown, changes: Array<{ text: string }>) {
          disk.content = changes[0].text;
          return { size: disk.content.length };
        }
        async createFolder() {
          disk.folderCreated = true;
          return { isDirectory: true };
        }
      },
    },
    "@theia/filesystem/lib/common/files": {ETAG_DISABLED:"",etag:(stat:{mtime:number;size:number}) => `${stat.mtime}:${stat.size}`,
      FileOperationError:class extends Error {},FileOperationResult:{FILE_MODIFIED_SINCE:3},hasReadWriteCapability:()=>true},
    "./echo-editor-protocol": {textHash:async (text:string) => createHash("sha256").update(text).digest("hex")},
    "./echo-host-bridge": { echoHostBridge: { enabled: true, request } },
  };
  const load = (id: string) => {
    if (!(id in imports)) throw new Error(`Unexpected Theia import: ${id}`);
    return imports[id];
  };
  new Function("require", "module", "exports", javascript)(load, module, module.exports);
  const Service = module.exports.EchoFileService as new () => {
    update: (resource: unknown, changes: unknown, options: unknown) => Promise<unknown>;
    createFolder: (resource: unknown) => Promise<unknown>;
  };
  return new Service();
}

const resource = { path: { fsPath: () => "/project/source.ts" },toString:() => "file:///project/source.ts" };

describe("EchoFileService mutation gate", () => {
  beforeEach(() => {
    disk.content = "before";
    disk.folderCreated = false;
    request.mockReset();
  });

  it("checks the gate before an incremental editor save reaches the provider", async () => {
    request.mockRejectedValueOnce(new Error("正在验证当前改动"));

    await expect(createService().update(resource, [{ text: "after" }], {}))
      .rejects.toThrow("正在验证当前改动");
    expect(disk.content).toBe("before");
    expect(request).toHaveBeenCalledWith("echo/before-mutation", {
      operation: "write", paths: ["/project/source.ts"],
    });
  });

  it("syncs after an allowed incremental save and gates folder creation", async () => {
    request.mockResolvedValue({ taskId: "task-1", closeRound: false });
    const service = createService();
    await service.update(resource, [{ text: "after" }], {});
    expect(disk.content).toBe("after");
    expect(request).toHaveBeenCalledWith("echo/after-mutation", {
      ticket: { taskId: "task-1", closeRound: false }, success: true,
    });

    request.mockReset();
    request.mockRejectedValueOnce(new Error("阶段禁止修改"));
    await expect(service.createFolder(resource)).rejects.toThrow("阶段禁止修改");
    expect(disk.folderCreated).toBe(false);
  });

  it("does not ask Monaco to retry a write that already reached disk", async () => {
    request.mockResolvedValueOnce({ taskId: "task-1", closeRound: false })
      .mockRejectedValueOnce(new Error("任务同步失败"));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      await expect(createService().update(resource, [{ text: "after" }], {})).resolves.toEqual({ size: 5 });
      expect(disk.content).toBe("after");
      expect(warn).toHaveBeenCalledOnce();
    } finally {
      warn.mockRestore();
    }
  });
});
