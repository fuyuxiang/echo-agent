// Regenerate the tracked, code-native tray assets from the shared vector mark.
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { reviewChromium } from "./ui-review-runtime.mjs";
const root = new URL("../src-tauri/icons/tray/", import.meta.url);
const mark = readFileSync(new URL("mark.svg", root), "utf8");
const browser = await (await reviewChromium()).launch({ headless: true, channel: "chrome" });
try {
  const page = await browser.newPage({ deviceScaleFactor: 1 });
  for (const [name, size, fill] of [["macos@2x.png", 44, "black"], ["windows-64.png", 64, "#338BEB"]]) {
    await page.setViewportSize({ width: size, height: size });
    await page.setContent(`<style>html,body{margin:0;background:transparent}svg{display:block;width:100vw;height:100vh;fill:${fill}}</style>${mark}`);
    writeFileSync(new URL(name, root), await page.screenshot({ omitBackground: true }));
    console.log(fileURLToPath(new URL(name, root)));
  }
} finally { await browser.close(); }
