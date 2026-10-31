import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  createUiReviewServer,
  reviewChromium,
  reviewOutput,
} from "./ui-review-runtime.mjs";

const output = reviewOutput(
  mkdtempSync(join(tmpdir(), "echo-management-pages-")),
);
const server = await createUiReviewServer(0);
const baseUrl = `http://127.0.0.1:${server.httpServer.address().port}`;
const browser = await (
  await reviewChromium()
).launch({
  headless: true,
  executablePath:
    process.env.CHROME_BIN ||
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
});
const errors = [];
const layouts = [];
const surfaces = [
  "projects",
  "management-experts",
  "management-knowledge",
  "organization-shell",
  "management-schedules",
];
console.log(`Management page screenshots: ${output}`);
try {
  for (const [width, height, theme, collapsed, font] of (process.argv.includes("--navigation-only") ? [] : [
    [1440, 900, "light", false, 16],
    [1100, 760, "dark", false, 16],
    [768, 720, "light", true, 22],
    [390, 760, "dark", true, 16],
  ])) {
    const page = await browser.newPage({ viewport: { width, height } });
    page.on("pageerror", (error) => errors.push(error.message));
    for (const surface of surfaces) {
      await page.goto(
        `${baseUrl}/__ui-review?surface=${surface}&platform=mac&theme=${theme}&collapsed=${collapsed ? 1 : 0}&extensionState=populated`,
      );
      await page.locator(".page-toolbar").first().waitFor();
      await page.addStyleTag({
        content: `html { font-size: ${font}px !important; }`,
      });
      await page.waitForTimeout(250);
      const geometry = await page.evaluate(() => {
        const main = document
          .querySelector(".app__main")
          .getBoundingClientRect();
        const header = document
          .querySelector(".page-toolbar")
          .getBoundingClientRect();
        const navigation = [
          ...document.querySelectorAll(".page-toolbar__navigation"),
        ].filter((element) => element.getBoundingClientRect().width > 0);
        const overflow = [...document.querySelectorAll(".app__main *")]
          .filter((element) => {
            if (
              element.closest('[hidden], [aria-hidden="true"]') ||
              !element.checkVisibility()
            )
              return false;
            const rect = element.getBoundingClientRect();
            const style = getComputedStyle(element);
            return (
              rect.width > 0 &&
              rect.height > 0 &&
              style.position !== "fixed" &&
              (rect.left < main.left - 2 || rect.right > main.right + 2)
            );
          })
          .map((element) => element.className)
          .slice(0, 6);
        return {
          header: { className: document.querySelector(".page-toolbar").className, padding: getComputedStyle(document.querySelector(".page-toolbar")).padding, display: getComputedStyle(document.querySelector(".page-toolbar")).display, background: getComputedStyle(document.querySelector(".page-toolbar")).background, radius: getComputedStyle(document.querySelector(".page-toolbar")).borderRadius },
          offset: header.top - main.top,
          navigation: navigation.length,
          overflow,
          blankHeader: !!document.querySelector(".main-page-controls"),
          navigationLeft: navigation[0]?.getBoundingClientRect().left,
        };
      });
      assert(!geometry.blankHeader, `${surface}: empty window header remains`);
      assert(
        geometry.offset <= 36,
        `${surface}: unnecessary leading space (${geometry.offset})`,
      );
      assert.equal(
        geometry.navigation,
        collapsed ? 1 : 0,
        `${surface}: window controls must appear once`,
      );
      if (collapsed)
        assert(
          geometry.navigationLeft >= 84,
          `${surface}: native traffic lights overlap navigation`,
        );
      assert.deepEqual(
        geometry.overflow,
        [],
        `${surface} ${width}: horizontal overflow`,
      );
      layouts.push({ surface, width, theme, font, ...geometry });
      writeFileSync(join(output, "layouts.json"), JSON.stringify(layouts, null, 2));
      await page.screenshot({
        path: join(output, `${surface}-${width}-${theme}.png`),
      });
    }
    await page.close();
  }
  const navigationPage = await browser.newPage({ viewport: { width: 768, height: 760 } });
  navigationPage.on("pageerror", error => errors.push(error.message));
  for (const surface of surfaces) {
    await navigationPage.goto(`${baseUrl}/__ui-review?surface=${surface}&platform=mac&collapsed=1`);
    await navigationPage.getByRole("button", { name: "展开侧边栏", exact: true }).waitFor();
    await navigationPage.getByRole("button", { name: "展开侧边栏", exact: true }).click();
    await navigationPage.locator(".app__body:not(.app__body--collapsed)").waitFor();
    assert.equal(await navigationPage.locator(".page-toolbar__navigation").count(), 0, `${surface}: duplicate controls after expanding sidebar`);
  }
  for (const platform of ["mac", "other"]) for (const width of [1280, 768]) for (const surface of ["collapsed-project", "collapsed-capabilities"]) {
    await navigationPage.setViewportSize({ width, height: 760 });
    await navigationPage.goto(`${baseUrl}/__ui-review?surface=${surface}&platform=${platform}`);
    const navigation = navigationPage.locator(".page-toolbar__navigation");
    await navigation.waitFor();
    assert.equal(await navigation.count(), 1);
    const bounds = await navigation.boundingBox();
    assert(bounds.x >= (platform === "mac" ? 84 : 12), `${surface}: native window controls overlap navigation`);
    const title = await navigationPage.locator(surface === "collapsed-project" ? ".pd-crumb" : ".panel-navigation__links").boundingBox();
    assert(title.x >= bounds.x + bounds.width - 1 || title.y >= bounds.y + bounds.height - 1, `${surface}: title overlaps navigation`);
  }
  await navigationPage.goto(`${baseUrl}/__ui-review?surface=organization-shell&orgError=1`);
  await navigationPage.getByRole("alert").waitFor();
  assert.match(await navigationPage.getByRole("alert").innerText(), /暂时无法连接组织服务/);
  assert.equal(await navigationPage.getByLabel("组织知识概况").count(), 0, "Load failure must not look like zero resources");
  await navigationPage.getByRole("button", { name: "重试", exact: true }).click();
  await navigationPage.getByLabel("组织知识概况").waitFor();
  assert.equal(await navigationPage.getByRole("alert").count(), 0);
  await navigationPage.close();
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ output, layouts: layouts.length, navigationLayouts: 8, flows: ["expand-sidebar-on-five-pages", "organization-load-failure/retry"] }));
} finally {
  await browser.close();
  await server.close();
}
