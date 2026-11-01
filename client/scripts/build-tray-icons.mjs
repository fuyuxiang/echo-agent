// Windows uses the app artwork; macOS uses its E/orbit/satellite template.
import { copyFileSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
const root = fileURLToPath(new URL("../", import.meta.url));
const require = createRequire(import.meta.url);

export function renderIcon(input, output, size) {
  const cli = require.resolve("@tauri-apps/cli/tauri.js");
  const args = [cli, "icon", input, "--output", output, ...(size ? ["--png", String(size)] : [])];
  const result = spawnSync(process.execPath, args, { cwd: root, encoding: "utf8", maxBuffer: 2 * 1024 * 1024 });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(result.stderr || result.stdout || "Icon generation failed");
}

export function buildTrayIcons(output = join(root, "src-tauri/icons/tray")) {
  const temporary = mkdtempSync(join(tmpdir(), "echo-tray-icons-"));
  try {
    mkdirSync(output, { recursive: true });
    for (const [name, input, size] of [
      ["macos@2x.png", "src-tauri/icons/tray/mark.svg", 44],
      ["windows-64.png", "app-icon.png", 64],
    ]) {
      const directory = join(temporary, name);
      renderIcon(join(root, input), directory, size);
      copyFileSync(join(directory, `${size}x${size}.png`), join(output, name));
    }
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  buildTrayIcons();
  console.log("Tray icons regenerated. Run pnpm icons:generate to refresh all assets and their manifest.");
}
