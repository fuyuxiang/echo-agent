import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { expect, it, vi } from "vitest";
import { withMeetingSpeechLock } from "./lib/meeting-speech-lock.mjs";

vi.mock("node:fs/promises", async importOriginal => {
  const fs = await importOriginal();
  return { ...fs, rm: vi.fn(fs.rm) };
});

const run = promisify(execFile);
const helper = new URL("./lib/meeting-speech-lock.mjs", import.meta.url).href;

it("recovers a dead owner without letting live preparations overlap", async () => {
  const scratch = await mkdtemp(join(tmpdir(), "echo-speech-lock-"));
  try {
    const lock = join(scratch, "lock");
    const active = join(scratch, "active");
    const { stdout } = await run(process.execPath, ["-p", "process.pid"]);
    await mkdir(join(lock, `${stdout.trim()}-abcdef`), { recursive: true });
    const prepare = `
      import { mkdir, rmdir } from "node:fs/promises";
      import { setTimeout } from "node:timers/promises";
      const [helper, lock, active] = process.argv.slice(1);
      const { withMeetingSpeechLock } = await import(helper);
      await withMeetingSpeechLock(lock, async () => {
        await mkdir(active);
        await setTimeout(200);
        await rmdir(active);
      }, 5_000);
    `;
    const results = await Promise.allSettled(Array.from({ length: 3 }, () =>
      run(process.execPath, ["--input-type=module", "-e", prepare, helper, lock, active])));
    expect(results.filter(result => result.status === "rejected")).toEqual([]);
    expect(await readdir(lock)).toEqual([]);
  } finally {
    await rm(scratch, { recursive: true, force: true, maxRetries: 4 });
  }
});

it.each([true, false])("handles EPERM checking a stale marker (removed by peer: %s)", async removedByPeer => {
  const fs = await vi.importActual("node:fs/promises");
  const scratch = await mkdtemp(join(tmpdir(), "echo-speech-lock-cleanup-"));
  try {
    const { stdout } = await run(process.execPath, ["-p", "process.pid"]);
    const owner = join(scratch, `${stdout.trim()}-abcdef`);
    await mkdir(owner);
    const denied = Object.assign(new Error("operation not permitted"), { code: "EPERM" });
    vi.mocked(rm).mockImplementationOnce(async (path, options) => {
      expect(path).toBe(owner);
      if (removedByPeer) await fs.rm(path, options);
      throw denied;
    });
    const prepare = vi.fn(async () => "prepared");
    if (removedByPeer) {
      await expect(withMeetingSpeechLock(scratch, prepare)).resolves.toBe("prepared");
      expect(prepare).toHaveBeenCalledOnce();
      expect(await readdir(scratch)).toEqual([]);
    } else {
      await expect(withMeetingSpeechLock(scratch, prepare)).rejects.toBe(denied);
      expect(prepare).not.toHaveBeenCalled();
      expect(await readdir(scratch)).toEqual([`${stdout.trim()}-abcdef`]);
    }
    expect(rm).toHaveBeenCalledWith(owner, expect.objectContaining({ maxRetries: 4, retryDelay: 50 }));
  } finally {
    await fs.rm(scratch, { recursive: true, force: true, maxRetries: 4 });
  }
});
