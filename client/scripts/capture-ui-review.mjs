import { join } from "node:path";
import { writeFileSync } from "node:fs";
import assert from "node:assert/strict";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput("/private/tmp/echo-ui-review-capture");
const server = await createUiReviewServer();
const browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
const page = await browser.newPage();
const evidence = [];
let errors = [];
page.on("pageerror", error => errors.push(error.message));
try {
  for (const [width, height, theme] of [[1280, 800, "light"], [1024, 680, "dark"], [1920, 1080, "light"]]) {
    await page.setViewportSize({ width, height });
    for (const spec of (process.env.UI_REVIEW_SURFACES || "home,home:unconfigured,home:loading,home:error,general,model,memory,personal-memory,security,cloud-storage,notify-channels,weixin-unconnected,personalize,data,shortcuts,usage,agent-settings,help,agent-mail,archived,capabilities,organization,meeting,conversation,automation-edit,collapsed-project,coding,page:项目,page:自动化,page:知识库,page:技能管理,page:连接器管理,page:插件管理,page:插件市场,modal-short,modal-long,modal-error,details:expert,details:connector,details:token,details:upload,details:knowledge,results:markdown,results:code,results:sheet,results:empty,results:error,trust:short,trust:long,trust:loading,trust:error,execution:running,execution:complete,execution:error,execution:empty,runtime:running,runtime:complete,runtime:error,runtime:empty").split(",")) {
      errors = [];
      const [surface, variant] = spec.split(":");
      const params = new URLSearchParams({ surface, theme });
      if (variant) params.set(surface === "page" ? "label" : surface === "details" ? "detailVariant" : surface === "results" ? "resultVariant" : "state", variant);
      await page.goto(`http://127.0.0.1:1439/__ui-review?${params}`, { waitUntil: "domcontentloaded", timeout: 60000 });
      try { await page.locator("#root > *").first().waitFor({ timeout: 8000 }); }
      catch { errors.push("Fixture did not render a visible surface"); }
      await page.addStyleTag({ content: "*,*::before,*::after{animation:none!important;transition:none!important}" });
      // Allow real async fixture IPC and lazy panels to finish before capturing.
      await page.waitForTimeout(500);
      if (surface === "modal-error") {
        await page.getByRole("button", { name: "保存说明", exact: true }).click();
        await page.getByRole("alert").waitFor();
      }
      // Use the same reading position for the active-process comparison so the
      // header remains visible even when the corrected process opens by default.
      if (surface === "execution" && variant === "running") {
        await page.locator(".chatview__scroll").evaluate(element => { element.scrollTop = 0; });
        await page.waitForTimeout(100);
      }
      const filename = `${surface}${variant ? `-${variant}` : ""}-${width}-${theme}.png`;
      await page.screenshot({ path: join(output, filename) });
      const geometry = await page.evaluate(() => ({
        overflow: document.documentElement.scrollWidth > innerWidth + 1,
        chatScrollTop: document.querySelector(".chatview__scroll")?.scrollTop,
        errorColors: [...document.querySelectorAll('.app-dialog__error,.trust-dialog__error,.models-settings-panel__editor-error,.file-preview__feedback--error,.app-dialog__button--danger')].map(element => {
          const styles = getComputedStyle(element);
          const canvas = document.createElement("canvas"); canvas.width = canvas.height = 1;
          const context = canvas.getContext("2d");
          context.fillStyle = "white"; context.fillRect(0, 0, 1, 1);
          const ancestors = []; for (let node = element; node; node = node.parentElement) ancestors.unshift(node);
          for (const node of ancestors) { context.fillStyle = getComputedStyle(node).backgroundColor; context.fillRect(0, 0, 1, 1); }
          const background = [...context.getImageData(0, 0, 1, 1).data].slice(0, 3);
          context.fillStyle = styles.color; context.fillRect(0, 0, 1, 1);
          const foreground = [...context.getImageData(0, 0, 1, 1).data].slice(0, 3);
          const luminance = rgb => rgb.map(value => { const channel = value / 255; return channel <= .04045 ? channel / 12.92 : ((channel + .055) / 1.055) ** 2.4; }).reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
          const values = [luminance(background), luminance(foreground)].sort((a, b) => b - a);
          return { color: styles.color, background: styles.backgroundColor, fontSize: styles.fontSize, contrast: (values[0] + .05) / (values[1] + .05) };
        }),
        dialogs: [...document.querySelectorAll('[aria-modal="true"]')].map(element => {
          const { x, y, width, height } = element.getBoundingClientRect();
          return { x, y, width, height, name: element.getAttribute("aria-label") };
        }),
        text: document.querySelector("#root").innerText.slice(0, 220),
      }));
      evidence.push({ surface, variant, width, height, theme, filename, errors: [...errors], baseline: process.env.UI_REVIEW_BASELINE === "1", ...geometry });
      if (process.env.UI_REVIEW_BASELINE !== "1") assert.ok(geometry.errorColors.every(color => color.contrast >= 4.5), `${filename}: error text is hard to read: ${JSON.stringify(geometry.errorColors)}`);
      console.log(`${filename}: ${errors.length ? errors.join(" | ") : "captured"}`);
    }
  }
  writeFileSync(join(output, process.env.UI_REVIEW_MANIFEST || "capture.json"), JSON.stringify(evidence, null, 2));
} finally { await browser.close(); await server.close(); }
