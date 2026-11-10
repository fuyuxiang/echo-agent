import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { expect, it } from "vitest";

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
    await rm(scratch, { recursive: true, force: true });
  }
});
