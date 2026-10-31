import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium } from "./ui-review-runtime.mjs";

const output = mkdtempSync(join(tmpdir(), "echo-model-presentation-"));
const server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/model-presentation-review.tsx" });
const evidence = [];
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const width of [1280, 768, 350]) for (const theme of ["light", "dark"]) for (const font of [16, 22]) {
    const page = await browser.newPage({ viewport: { width, height: 800 } });
    page.setDefaultNavigationTimeout(60000);
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?theme=${theme}`);
    const trigger = page.getByRole("button", { name: "任务模型：Echo Reed", exact: true });
    await trigger.waitFor();
    await page.evaluate((font) => { document.documentElement.style.fontSize = `${font}px`; }, font);
    await trigger.click();
    const menu = page.getByRole("listbox", { name: "任务模型" });
    assert.equal(await menu.getByRole("option").count(), 6);
    assert.equal(await page.locator(".model-selector__source").count(), 0, "sources are grouped rather than repeated as badges");
    assert.equal(await menu.getByRole("group").count(), 3);
    assert.doesNotMatch(await menu.textContent(), /echoagent-ojlab\/|organization\/|custom-[23]\//);
    const popup = page.getByRole("dialog", { name: "任务模型" });
    const layout = await popup.evaluate((element) => {
      const r = element.getBoundingClientRect();
      const fits = [...element.querySelectorAll("button, .model-selector__item-label, .model-selector__connection")].every((item) => item.scrollWidth <= item.clientWidth + 1);
      return { left: r.left, right: r.right, top: r.top, bottom: r.bottom, fits };
    });
    assert.ok(layout.left >= 0 && layout.right <= width + 1 && layout.top >= 0 && layout.bottom <= 801 && layout.fits, JSON.stringify({ width, theme, font, layout }));
    await popup.screenshot({ path: join(output, `models-${width}-${theme}-${font}.png`) });
    for (const [name, id] of [
      ["Echo Reed · 内置", "echoagent-ojlab/Echo Reed"],
      ["Echo Reed · 组织 · 研发团队", "organization/Echo Reed"],
      ["Echo Reed · 个人 · 我的网关", "custom-2/Echo Reed"],
      ["Echo Reed · 个人 · 备用网关", "custom-3/Echo Reed"],
    ]) {
      if (!(await menu.isVisible())) await trigger.click();
      await menu.getByRole("option", { name, exact: true }).click();
      assert.equal(await page.getByTestId("selected").getAttribute("data-model-id"), id);
      assert.equal(await trigger.getAttribute("title"), name);
      assert.equal(await page.getByRole("combobox").inputValue(), id);
    }
    await page.getByRole("combobox").selectOption("organization/Qwen/Qwen3-32B");
    assert.equal(await page.getByRole("button", { name: "任务模型：Qwen/Qwen3-32B", exact: true }).count(), 1);
    assert.equal(await page.getByRole("button", { name: "任务模型：Qwen/Qwen3-32B", exact: true }).getAttribute("title"), "Qwen/Qwen3-32B");

    // Distinct names need source groups, but no repeated connection subtitles.
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?theme=${theme}&catalog=distinct`);
    await page.evaluate((font) => { document.documentElement.style.fontSize = `${font}px`; }, font);
    const plainTrigger=page.getByRole("button", {name:"任务模型：Echo Reed",exact:true});
    await plainTrigger.click();
    const plainMenu=page.getByRole("listbox",{name:"任务模型"});
    assert.equal(await plainMenu.getByRole("option").count(),6);
    assert.equal(await page.locator(".model-selector__source, .model-selector__connection").count(),0);
    assert.equal(await plainMenu.getByRole("group").count(),2);
    assert.equal(await plainMenu.locator(".model-selector__group-label").count(),2);
    assert.equal(await plainMenu.locator(".model-selector__check").count(),1);
    await page.getByRole("dialog",{name:"任务模型"}).screenshot({path:join(output,`plain-models-${width}-${theme}-${font}.png`)});
    await plainMenu.getByRole("option",{name:"MiniMax-M3.1-Flash",exact:true}).click();
    assert.equal(await page.getByTestId("selected").getAttribute("data-model-id"),"organization/MiniMax-M3.1-Flash");
    assert.equal(await page.getByRole("button",{name:"任务模型：MiniMax-M3.1-Flash",exact:true}).getAttribute("title"),"MiniMax-M3.1-Flash");
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?theme=${theme}&catalog=aliases`);
    await page.evaluate((font) => { document.documentElement.style.fontSize = `${font}px`; }, font);
    const aliasTrigger = page.getByRole("button", {name:"任务模型：通用模型",exact:true});
    await aliasTrigger.click();
    await page.getByRole("listbox", {name:"任务模型",exact:true}).getByRole("option", {name:"通用模型 · 个人 · 我的网关 · strong",exact:true}).click();
    assert.equal(await page.getByTestId("selected").getAttribute("data-model-id"), "gateway/strong");
    assert.equal(await aliasTrigger.getAttribute("title"), "通用模型 · 个人 · 我的网关 · strong");
    await page.getByRole("combobox").selectOption("gateway/fast");
    assert.equal(await aliasTrigger.getAttribute("title"), "通用模型 · 个人 · 我的网关 · fast");
    await aliasTrigger.click();
    assert.equal(await page.locator(".model-selector__group-label").count(),0);
    await page.keyboard.press("Tab");
    assert.equal(await page.getByRole("button",{name:"管理模型"}).evaluate(el=>el===document.activeElement),true);
    await page.keyboard.press("Enter");
    assert.equal(await page.getByTestId("managed").getAttribute("data-model-id"),"gateway/fast");
    assert.equal(await page.getByRole("listbox").count(),0);
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false);
    await page.screenshot({path:join(output,`aliases-${width}-${theme}-${font}.png`)});
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?theme=${theme}&catalog=empty`);
    await page.getByRole("button", {name:"任务模型：配置模型",exact:true}).click();
    assert.equal(await page.getByTestId("managed").getAttribute("data-model-id"),"empty");
    assert.deepEqual(errors, []);
    evidence.push({ width, theme, font, layout });
    await page.close();
  }
  writeFileSync(join(output, "geometry.json"), JSON.stringify(evidence, null, 2));
  console.log(`Model presentation passed: ${evidence.length} layouts, grouped sources, duplicate names, exact routing, keyboard management and empty setup. Screenshots: ${output}`);
} finally {
  await browser?.close();
  await server.close();
}
