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
  "automation-templates",
  "management-meetings",
];

async function assertProjectAlignment(page) {
  const elements = page.locator(".project-section__title, .project-grid, .template-grid");
  const positions = await elements.evaluateAll(nodes => nodes.map(node => ({
    className: node.className,
    left: node.getBoundingClientRect().left,
  })));
  assert.equal(positions.length, 4, "Both project sections must be visible");
  assert(positions.every(({ left }) => Math.abs(left - positions[0].left) < 1),
    `Project content edges must align: ${JSON.stringify(positions)}`);
}

console.log(`Management page screenshots: ${output}`);
try {
  for (const [width, height, theme, collapsed, font, platform] of (process.argv.includes("--navigation-only") ? [] : [
    [1440, 900, "light", false, 16, "mac"],
    [1440, 900, "light", true, 16, "mac"],
    [1440, 900, "light", false, 16, "other"],
    [1440, 900, "light", true, 16, "other"],
    [1100, 760, "dark", false, 16, "mac"],
    [768, 720, "light", true, 22, "mac"],
    [390, 680, "dark", true, 16, "mac"],
    [768, 720, "light", true, 22, "other"],
  ])) {
    const page = await browser.newPage({ viewport: { width, height } });
    await page.addInitScript(() => {
      const mac = new URLSearchParams(location.search).get("platform") === "mac";
      Object.defineProperty(navigator, "userAgent", {
        configurable: true,
        value: mac ? "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" : "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
      });
    });
    page.on("pageerror", (error) => errors.push(error.message));
    for (const surface of surfaces) {
      await page.goto(
        `${baseUrl}/__ui-review?surface=${surface}&platform=${platform}&windowChrome=1&theme=${theme}&collapsed=${collapsed ? 1 : 0}&extensionState=populated${surface === "projects" ? "&collectionAudit=1" : ""}`,
      );
      await page.locator(".page-toolbar").first().waitFor();
      await page.addStyleTag({
        content: `html { font-size: ${font}px !important; }`,
      });
      await page.waitForTimeout(250);
      const geometry = await page.evaluate(() => {
        const bounds = (selector) => {
          const rect = document.querySelector(selector)?.getBoundingClientRect();
          return rect && { left: rect.left, top: rect.top, right: rect.right };
        };
        const main = document
          .querySelector(".app__main")
          .getBoundingClientRect();
        const header = document
          .querySelector(".page-toolbar")
          .getBoundingClientRect();
        const compact = document.querySelector(".management-toolbar");
        const navigation = [
          ...document.querySelectorAll(".window-sidebar-navigation"),
        ].filter((element) => element.getBoundingClientRect().width > 0);
        const overflow = [...document.querySelectorAll(".app__main *")]
          .filter((element) => {
            if (
              element.closest('[hidden], [aria-hidden="true"]') ||
              !element.checkVisibility() ||
              element.closest(".management-toolbar nav .panel-navigation__links")
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
          headerTop: header.top,
          headerHeight: header.height,
          headerLeftGap: header.left - main.left,
          headerRightGap: main.right - header.right,
          compact: !!compact,
          compactContent: compact?.firstElementChild?.getBoundingClientRect().left,
          compactEnd: compact?.lastElementChild?.getBoundingClientRect().right,
          caption: bounds(".titlebar--caption-only"),
          headerDescription: !!compact?.querySelector("p, input"),
          mainPadding: parseFloat(getComputedStyle(document.querySelector(".app__main")).paddingBlockStart),
          navigation: navigation.length,
          overflow,
          blankHeader: !!document.querySelector(".main-page-controls"),
          navigationLeft: navigation[0]?.getBoundingClientRect().left,
          navigationRight: navigation[0]?.getBoundingClientRect().right,
          scheduleTabs: bounds(".atm-tabs"),
          windowToggle: (() => {
            const { x, y, width, height } = document.querySelector(".window-sidebar-toggle").getBoundingClientRect();
            return { x, y, width, height };
          })(),
        };
      });
      assert(!geometry.blankHeader, `${surface}: empty window header remains`);
      const clearance = (collapsed || platform === "other") && !geometry.compact ? 52 : 0;
      assert.equal(geometry.mainPadding, clearance, `${surface}: reserve only the window controls' vertical space`);
      assert(
        geometry.offset >= clearance - 1 && geometry.offset <= clearance + 36,
        `${surface}: unnecessary leading space (${geometry.offset})`,
      );
      assert.equal(
        geometry.navigation,
        1,
        `${surface}: window controls must appear once`,
      );
      assert.deepEqual(geometry.windowToggle, { x: platform === "mac" ? 88 : 12, y: 10, width: 32, height: 32 }, `${surface}: window toggle must use the same coordinates on every page`);
      if (surface !== "management-meetings") {
        assert(geometry.compact, `${surface}: missing shared management toolbar`);
        assert.equal(geometry.headerHeight, 52, `${surface}: toolbar height`);
        assert.equal(geometry.offset, 0, `${surface}: toolbar starts at the top`);
        assert(Math.abs(geometry.headerLeftGap) <= 1 && Math.abs(geometry.headerRightGap) <= 1, `${surface}: toolbar must span the main column`);
        assert(!geometry.headerDescription, `${surface}: description or search crowds the toolbar`);
        if (collapsed)
          assert(geometry.compactContent >= geometry.navigationRight + 12, `${surface}: overlaps window controls`);
        if (platform === "other") {
          assert.equal(geometry.caption.top, geometry.headerTop, `${surface}: Windows controls sit in the same row`);
          assert(geometry.compactEnd <= geometry.caption.left - 12, `${surface}: page actions overlap Windows controls`);
        }
        if (geometry.scheduleTabs)
          assert(geometry.scheduleTabs.top >= geometry.headerTop + geometry.headerHeight, "Schedule tabs belong with the list controls");
      } else if (collapsed) {
        assert(
          geometry.headerTop >= geometry.windowToggle.y + geometry.windowToggle.height,
          `${surface}: window controls overlap the page header`,
        );
      }
      assert.deepEqual(
        geometry.overflow,
        [],
        `${surface} ${width}: horizontal overflow`,
      );
      if (surface === "projects") {
        await page.locator(".project-grid").waitFor();
        await assertProjectAlignment(page);
      }
      layouts.push({ surface, width, theme, font, platform, collapsed, ...geometry });
      writeFileSync(join(output, "layouts.json"), JSON.stringify(layouts, null, 2));
      await page.screenshot({
        path: join(output, `${surface}-${width}-${theme}-${platform}-${collapsed ? "closed" : "open"}.png`),
      });
    }
    await page.close();
  }
  const navigationPage = await browser.newPage({ viewport: { width: 768, height: 760 } });
  navigationPage.setDefaultNavigationTimeout(60_000);
  navigationPage.on("pageerror", error => errors.push(error.message));
  for (const surface of surfaces) {
    await navigationPage.goto(`${baseUrl}/__ui-review?surface=${surface}&platform=mac&collapsed=1${surface === "projects" ? "&collectionAudit=1" : ""}`);
    await navigationPage.getByRole("button", { name: "展开侧边栏", exact: true }).waitFor();
    const toggle = navigationPage.locator(".window-sidebar-toggle");
    const before = await toggle.boundingBox();
    const original = await toggle.elementHandle();
    await navigationPage.getByRole("button", { name: "展开侧边栏", exact: true }).click();
    await navigationPage.locator(".app__body:not(.app__body--collapsed)").waitFor();
    assert.equal(await navigationPage.locator(".page-toolbar__navigation").count(), 0, `${surface}: duplicate controls after expanding sidebar`);
    assert.deepEqual(await toggle.boundingBox(), before, `${surface}: toggle moves while expanding`);
    assert.ok(await toggle.evaluate((element, original) => element === original, original), `${surface}: toggle replaced while expanding`);
    await original.dispose();
    if (surface === "projects") {
      await navigationPage.locator(".project-grid").waitFor();
      await assertProjectAlignment(navigationPage);
      const search = navigationPage.getByRole("textbox", { name: "搜索项目", exact: true });
      await search.fill("没有这个项目");
      await navigationPage.getByText("没有匹配的项目", { exact: true }).waitFor();
      await navigationPage.getByRole("button", { name: "清空搜索项目", exact: true }).click();
      assert.equal(await search.inputValue(), "");
      await navigationPage.getByRole("button", { name: "列表视图", exact: true }).click();
      await navigationPage.locator('.project-grid[data-view="list"]').waitFor();
      await navigationPage.getByRole("button", { name: "卡片视图", exact: true }).click();
      await navigationPage.getByRole("button", { name: "打开项目 产品需求全流程", exact: true }).click();
      await navigationPage.locator(".pd-crumb").waitFor();
      await toggle.click();
      await navigationPage.locator(".pd-topbar.page-toolbar--window-navigation").waitFor();
      const crumb = await navigationPage.locator(".pd-crumb").boundingBox();
      const tabs = await navigationPage.locator(".pd-tabs-row").boundingBox();
      assert.equal(crumb.x, tabs.x, "Project breadcrumb and detail tabs must align after collapsing");
      assert(crumb.y >= before.y + before.height, "Project breadcrumb must clear the window controls");
      await navigationPage.getByRole("button", { name: "参与者", exact: true }).click();
      await navigationPage.getByText("项目参与者（本机备注）", { exact: true }).waitFor();
    }
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
  for (const [capability, label, text] of [
    ["unavailable", "组织知识暂不可用", "暂不可用"],
    ["no-scope", "暂无团队或组织共享权限", "无共享权限"],
  ]) {
    await navigationPage.setViewportSize({ width: 1024, height: 680 });
    await navigationPage.goto(`${baseUrl}/__ui-review?surface=organization-shell&platform=mac&orgCapability=${capability}`);
    await navigationPage.getByRole("navigation", { name: "组织功能" }).getByRole("button", { name: /^文档/ }).click();
    const status = navigationPage.getByRole("status", { name: label, exact: true });
    assert.equal(await status.innerText(), text);
    assert.ok(await status.evaluate(element => {
      const box = element.getBoundingClientRect();
      return element.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2));
    }), "Organization capability status must stay visible on resource tabs");
  }
  await navigationPage.goto(`${baseUrl}/__ui-review?surface=management-schedules&platform=mac&background=off`);
  const backgroundNotice = navigationPage.getByRole("status", { name: "定时任务后台运行", exact: true });
  await backgroundNotice.getByText(/关闭主窗口会停止本机定时任务/).waitFor();
  await navigationPage.getByRole("button", { name: "编辑定时任务「每日 AI 新闻推送」", exact: true }).click();
  const editor = navigationPage.getByRole("dialog", { name: "编辑定时任务", exact: true });
  const draftName = editor.getByRole("textbox", { name: "名称", exact: true });
  await draftName.fill("保留未保存的计划名称");
  await editor.getByRole("button", { name: "后台运行设置", exact: true }).click();
  const settings = navigationPage.getByRole("dialog", { name: "设置", exact: true });
  const toggleBackground = settings.getByRole("switch", { name: "关闭窗口后继续运行", exact: true });
  await settings.getByText(/关闭主窗口将停止任务执行和自动化调度/).waitFor();
  assert.equal(await toggleBackground.isChecked(), false);
  await toggleBackground.press("Space");
  await settings.getByText("已开启后台运行。", { exact: true }).waitFor();
  await backgroundNotice.waitFor({ state: "hidden" });
  await settings.getByRole("button", { name: "关闭设置", exact: true }).click();
  await settings.waitFor({ state: "hidden" });
  assert.equal(await draftName.inputValue(), "保留未保存的计划名称");
  assert.ok(await draftName.evaluate(element => element === document.activeElement), "Return to the draft after its background warning resolves");
  await navigationPage.close();
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ output, layouts: layouts.length, navigationLayouts: 8, flows: ["expand-sidebar-on-management-pages", "collapse-sidebar-in-project-detail", "organization-load-failure/retry", "organization-capability-status", "background-settings-preserve-draft"] }));
} finally {
  await browser.close();
  await server.close();
}
