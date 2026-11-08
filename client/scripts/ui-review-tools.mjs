import { existsSync, realpathSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Browser launch and screenshot decoding must use the same installation.
// CI supplies an isolated package; local reviews can reuse the optional IDE tools.
function playwrightModule() {
  const configured = process.env.PLAYWRIGHT_MODULE;
  const candidate = configured || resolve("vendor/theia-platform/node_modules/playwright/index.mjs");
  if (existsSync(candidate)) return realpathSync(candidate);
  if (configured) throw new Error("PLAYWRIGHT_MODULE does not exist: " + configured);
  try {
    return fileURLToPath(import.meta.resolve("playwright"));
  } catch {
    throw new Error("Set PLAYWRIGHT_MODULE to an installed Playwright index.mjs, or prepare the Theia browser test dependencies.");
  }
}

export async function reviewChromium() {
  return (await import(pathToFileURL(playwrightModule()).href)).chromium;
}

export function reviewPng() {
  const playwrightRequire = createRequire(playwrightModule());
  const core = dirname(playwrightRequire.resolve("playwright-core/package.json"));
  return playwrightRequire(resolve(core, "lib/utilsBundle.js")).PNG;
}
