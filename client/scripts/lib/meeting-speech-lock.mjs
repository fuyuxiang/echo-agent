import { mkdir, mkdtemp, readdir, rm } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout } from "node:timers/promises";

async function hasOtherOwner(root, own) {
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const owner = join(root, entry.name);
    const match = entry.name.match(/^([1-9]\d*)-[A-Za-z0-9]{6}$/);
    if (owner === own || !entry.isDirectory() || !match) continue;
    const pid = Number(match[1]);
    if (pid > 0x7fffffff) continue;
    try {
      process.kill(pid, 0);
    } catch (error) {
      if (error.code === "ESRCH") {
        await rm(owner, { recursive: true, force: true });
        continue;
      }
      if (error.code !== "EPERM") throw error;
    }
    return true;
  }
  return false;
}

export async function withMeetingSpeechLock(root, prepare, timeoutMs = 20 * 60_000) {
  await mkdir(root, { recursive: true });
  const deadline = performance.now() + timeoutMs;
  do {
    // Announce before checking other owners. If two processes see each
    // other, both withdraw their own marker and retry with a small jitter.
    // Unique names let a dead owner's marker be removed without touching
    // any marker created by a new process.
    const own = await mkdtemp(join(root, `${process.pid}-`));
    try {
      if (!await hasOtherOwner(root, own)) return await prepare();
    } finally {
      await rm(own, { recursive: true, force: true });
    }
    if (performance.now() >= deadline) break;
    await setTimeout(100 + Math.random() * 150);
  } while (performance.now() < deadline);
  throw new Error(`等待其他进程准备语音资源超时：${root}`);
}
