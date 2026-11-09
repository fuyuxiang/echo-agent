import assert from "node:assert/strict";
import { join } from "node:path";

export async function validateSidebarScroll(page, output) {
  const ready = () => page.locator(".sidebar__content[data-catalog-ready]").waitFor();
  const more = (id) => page.locator(`.sidebar__catalog-more[aria-controls="${id}"]`);
  const openMore = async (id) => {
    const control = more(id);
    await control.click();
    await page.locator(`#${id}:not([data-catalog-preview])`).waitFor();
  };
  const closeMore = async (id) => {
    await more(id).click();
    await page.locator(`#${id}[data-catalog-preview]`).waitFor();
  };
  const validateHeadings = async () => {
    await page.mouse.move(page.viewportSize().width - 1, 0);
    const headings = await page.locator(".sidebar__list-section").evaluateAll(sections => sections.map(section => {
      const label = section.querySelector(".sidebar__section-label");
      const style = getComputedStyle(label);
      const box = label.getBoundingClientRect();
      const textBox = label.querySelector("span").getBoundingClientRect();
      const chevronBox = label.querySelector("svg").getBoundingClientRect();
      return {
        name: label.textContent.replace(/\s*\(.*\)/, ""),
        left: box.left,
        top: box.top,
        chevronAfterText: chevronBox.left >= textBox.right,
        borderTopWidth: getComputedStyle(section).borderTopWidth,
        typography: [style.fontFamily, style.fontSize, style.fontWeight, style.color, style.padding, box.height],
      };
    }));
    assert.deepEqual(headings.map(heading => heading.name), ["任务", "项目", "专家团"], "sidebar lists should appear in task, project, chat order");
    for (const heading of headings) {
      assert.deepEqual(heading.typography, headings[0].typography, `${heading.name}: list heading style should match tasks`);
      assert.equal(heading.left, headings[0].left, `${heading.name}: list headings should align`);
      assert.equal(heading.borderTopWidth, "0px", `${heading.name}: list sections should have no separator`);
      assert.ok(heading.chevronAfterText, `${heading.name}: collapse arrow should follow the label`);
    }
    assert.ok(headings[0].top < headings[1].top && headings[1].top < headings[2].top, "list headings should follow their document order visually");
    const contentBottom = await page.locator(".sidebar__content").evaluate(element => element.getBoundingClientRect().bottom);
    const catalogFits = await page.locator(".sidebar__content").evaluate(element => element.scrollHeight <= element.clientHeight + 1);
    if (catalogFits) assert.ok(headings[2].top + headings[2].typography.at(-1) <= contentBottom + 1, "chat heading should remain visible inside the sidebar");
  };
  const snapshot = () => page.evaluate(() => {
    const rect = (selector) => {
      const box = document.querySelector(selector).getBoundingClientRect();
      return { top: box.top, bottom: box.bottom };
    };
    const scroll = (selector) => {
      const element = document.querySelector(selector);
      return { top: element.scrollTop, clientHeight: element.clientHeight, scrollHeight: element.scrollHeight };
    };
    return {
      content: rect(".sidebar__content"),
      taskHeader: rect(".sidebar__list-section--tasks > .sidebar__section-head"),
      projectHeader: rect(".sidebar__list-section--projects > .sidebar__section-head"),
      taskListRect: rect("#sidebar-task-list"),
      tasks: scroll("#sidebar-task-list"),
      projects: scroll("#sidebar-project-list"),
    };
  });

  let layouts = 0;
  await page.setViewportSize({ width: 1024, height: 720 });
  for (const variant of ["tasks", "projects", "both", "short"]) {
    await page.goto(`http://127.0.0.1:1439/__ui-review?surface=sidebar-scroll&variant=${variant}`);
    await ready();
    const before = await snapshot();
    assert.ok(before.taskHeader.top >= before.content.top - 1, `${variant}: task heading escapes the sidebar`);
    assert.ok(before.projectHeader.bottom <= before.content.bottom + 1, `${variant}: project heading is hidden below the sidebar`);
    assert.ok(before.tasks.clientHeight > 30 && before.projects.clientHeight > 30, `${variant}: a list has no usable height`);
    if (variant === "short") {
      assert.ok(before.projectHeader.top - before.taskListRect.bottom < 20, "short lists should stay grouped together");
    }
    await page.screenshot({ path: join(output, `sidebar-scroll-${variant}.png`) });
    layouts += 1;

    if (["tasks", "both"].includes(variant)) {
      const moreTasks = more("sidebar-task-list");
      assert.match(await moreTasks.getAttribute("aria-label"), /^查看更多任务（\d+）$/);
      const firstHidden = page.locator("#sidebar-task-list [inert] .sidebar__conv-select").first();
      const firstNewTask = await firstHidden.getAttribute("aria-label");
      await moreTasks.focus();
      await page.keyboard.press("Enter");
      assert.equal(await page.locator(":focus").getAttribute("aria-label"), firstNewTask);
      await page.keyboard.press("Tab");
      assert.ok(await page.locator("#sidebar-task-list :focus").count(), "Tab must stay within the expanded task list");
      await page.keyboard.press("Escape");
      assert.equal(await moreTasks.getAttribute("aria-expanded"), "false");
      assert.ok(await moreTasks.evaluate(button => button === document.activeElement), "Escape restores focus to the task disclosure");
      await openMore("sidebar-task-list");
      await page.locator("#sidebar-task-list").evaluate(element => { element.scrollTop = element.scrollHeight; });
      const after = await snapshot();
      assert.ok(after.tasks.top > 0, `${variant}: tasks do not scroll`);
      assert.equal(after.projects.top, before.projects.top, `${variant}: task scrolling moved projects`);
      assert.ok(Math.abs(after.taskHeader.top - before.taskHeader.top) < 1, `${variant}: task heading moved`);
      assert.ok(Math.abs(after.projectHeader.top - before.projectHeader.top) < 1, `${variant}: project heading moved`);
      await closeMore("sidebar-task-list");
    }
    if (["projects", "both"].includes(variant)) {
      const previous = await snapshot();
      await openMore("sidebar-project-list");
      await page.locator("#sidebar-project-list").evaluate(element => { element.scrollTop = element.scrollHeight; });
      const after = await snapshot();
      assert.ok(after.projects.top > 0, `${variant}: projects do not scroll`);
      assert.equal(after.tasks.top, previous.tasks.top, `${variant}: project scrolling moved tasks`);
      assert.ok(Math.abs(after.taskHeader.top - previous.taskHeader.top) < 1, `${variant}: task heading moved`);
      assert.ok(Math.abs(after.projectHeader.top - previous.projectHeader.top) < 1, `${variant}: project heading moved`);
      await closeMore("sidebar-project-list");
    }
    if (variant === "both") {
      const previous = await snapshot();
      await page.getByRole("button", { name: /^项目 \(/ }).click();
      await page.waitForFunction(height => document.querySelector("#sidebar-task-list").clientHeight > height, previous.tasks.clientHeight);
      const expandedTaskHeight = await page.locator("#sidebar-task-list").evaluate(element => element.clientHeight);
      assert.ok(expandedTaskHeight > previous.tasks.clientHeight + 20, "collapsing projects should give tasks more room");
      await page.getByRole("button", { name: /^项目 \(/ }).click();
      await page.locator("#sidebar-project-list").waitFor();
      const beforeTaskCollapse = await snapshot();
      await page.getByRole("button", { name: /^任务 \(/ }).click();
      await page.waitForFunction(height => document.querySelector("#sidebar-project-list").clientHeight > height, beforeTaskCollapse.projects.clientHeight);
      const expandedProjectHeight = await page.locator("#sidebar-project-list").evaluate(element => element.clientHeight);
      assert.ok(expandedProjectHeight > beforeTaskCollapse.projects.clientHeight + 20, "collapsing tasks should give projects more room");
    }
  }

  await page.setViewportSize({ width: 1024, height: 832 });
  await page.goto("http://127.0.0.1:1439/__ui-review?surface=home&platform=mac&windowChrome=1&sidebarCatalog=fits");
  const tasks = page.locator("#sidebar-task-list .sidebar__conv");
  await ready();
  const fits = await snapshot();
  assert.equal(await tasks.count(), 8, "the expanded task section should render all eight tasks");
  assert.equal(fits.tasks.clientHeight, fits.tasks.scrollHeight, "all eight rows fit without scrolling at normal height");
  assert.equal(await more("sidebar-task-list").isVisible(), false);
  const lastTask = await tasks.last().boundingBox();
  assert.ok(lastTask.y + lastTask.height <= fits.taskListRect.bottom, "the last task should be fully visible");
  layouts++;

  await page.setViewportSize({ width: 1024, height: 600 });
  await page.goto("http://127.0.0.1:1439/__ui-review?surface=sidebar-scroll&variant=both");
  await ready();
  const compact = await snapshot();
  assert.ok(compact.projectHeader.bottom <= compact.content.bottom + 1, "project heading should remain visible at compact window height");
  assert.ok(compact.tasks.clientHeight > 20 && compact.projects.clientHeight > 20, "both lists should remain usable at compact window height");

  for (const height of [600, 720]) {
    await page.setViewportSize({width:1024,height});
    await page.goto("http://127.0.0.1:1439/__ui-review?surface=sidebar-scroll&variant=both&groups=18&projectSessions=1");
    const groupList = page.getByRole("region",{name:"最近专家团"});
    await ready();
    await validateHeadings();
    const before = await snapshot();
    assert.ok(before.tasks.clientHeight >= 36 && before.projects.clientHeight >= 36, `all groups must retain a full row at height ${height}: ${JSON.stringify(before)}`);
    assert.ok(before.projectHeader.bottom <= before.content.bottom + 1, "groups must not push projects outside the sidebar");
    const catalog = page.locator(".sidebar__content");
    if (await catalog.evaluate(element => element.scrollHeight > element.clientHeight + 1)) {
      await page.locator("#sidebar-task-list").hover();
      await page.mouse.wheel(0, 200);
      await page.waitForFunction(() => document.querySelector(".sidebar__content").scrollTop > 0);
      await catalog.evaluate(element => { element.scrollTop = 0; });
    }
    const groupSizes = await groupList.evaluate(element => ({height:element.clientHeight,scrollHeight:element.scrollHeight}));
    assert.ok(groupSizes.height >= 30 && groupSizes.scrollHeight > groupSizes.height, `groups must show a full compact row and scroll at height ${height}: ${JSON.stringify(groupSizes)}`);
    await openMore("sidebar-task-list");
    assert.equal((await snapshot()).projects.clientHeight, before.projects.clientHeight);
    await openMore("sidebar-group-list");
    assert.equal(await page.locator("#sidebar-task-list").getAttribute("data-catalog-preview"), "");
    assert.equal(await groupList.locator(".group-sidebar__row:not([inert])").count(), 18, "expert groups beyond twelve must be accessible");
    await groupList.evaluate(element => {element.scrollTop=element.scrollHeight;});
    const after = await snapshot();
    assert.equal(after.tasks.top,before.tasks.top,"scrolling groups must not move tasks");
    assert.equal(after.projects.top,before.projects.top,"scrolling groups must not move projects");
    const openProject = page.getByRole("button", { name: /展开项目 01.*对话/ });
    await openProject.focus();
    await page.keyboard.press("Enter");
    assert.equal(await groupList.getAttribute("data-catalog-preview"), "");
    assert.equal(await page.locator("#sidebar-project-list .sidebar__children .sidebar__conv").count(), 14);
    assert.equal(await page.locator("#sidebar-project-list .sidebar__children").evaluate(element => getComputedStyle(element).overflowY), "visible");
    const collapseProject = page.getByRole("button", { name: /收起项目 01.*对话/ });
    assert.ok(await collapseProject.evaluate(button => {
      const box = button.getBoundingClientRect();
      return button === document.activeElement && button.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2));
    }), "Opening conversations keeps the focused project disclosure visible");
    await page.keyboard.press("Tab");
    assert.match(await page.locator(":focus").getAttribute("aria-label"), /^项目会话 1\b/);
    const lastChild = page.locator("#sidebar-project-list .sidebar__children .sidebar__conv-select").last();
    await lastChild.focus();
    assert.ok(await lastChild.evaluate(element => {
      const row = element.getBoundingClientRect();
      const list = element.closest(".sidebar__group").getBoundingClientRect();
      const project = element.closest("[data-catalog-project]").querySelector("[data-catalog-project-heading]").getBoundingClientRect();
      return row.top >= project.bottom - 1 && row.bottom <= list.bottom + 1 && project.top >= list.top - 1;
    }), "the last project conversation must be reachable through the project scroller");
    const children = await snapshot();
    assert.ok(children.tasks.clientHeight >= 36, "project children must leave a complete task row");
    assert.match(await page.getByRole("button", { name: /^项目 \(/ }).textContent(), /项目 \(18\)/);
    await page.screenshot({path:join(output,`sidebar-three-lists-${height}.png`)});
    await collapseProject.click();
    assert.ok(await openProject.evaluate(button => {
      const box = button.getBoundingClientRect();
      return button.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2));
    }), "Collapsing a scrolled project returns to its heading");
    await page.waitForFunction(({ tasks, projects }) =>
      document.querySelector("#sidebar-task-list").clientHeight === tasks.clientHeight
      && document.querySelector("#sidebar-project-list").clientHeight === projects.clientHeight, before);
    await page.getByRole("button",{name:/^专家团 \(/}).click();
    assert.equal(await groupList.count(),0);
    // At 600px, removing the single 30px group row leaves less than one 36px
    // task or project row. At 720px the freed space can show another full row.
    if (height === 720) {
      await page.waitForFunction(({ tasks, projects }) =>
        document.querySelector("#sidebar-task-list").clientHeight > tasks.clientHeight
        || document.querySelector("#sidebar-project-list").clientHeight > projects.clientHeight, before);
    }
    await page.getByRole("button",{name:"查看全部专家团"}).waitFor();
    layouts++;
  }

  for (const theme of ["light", "dark"]) {
    await page.setViewportSize({ width: 1024, height: 720 });
    await page.goto(`http://127.0.0.1:1439/__ui-review?surface=sidebar-scroll&variant=short&groups=1&theme=${theme}`);
    await page.getByRole("region", { name: "最近专家团" }).waitFor();
    await validateHeadings();
    await page.locator(".sidebar").screenshot({ path: join(output, `sidebar-short-chats-${theme}.png`) });
    layouts++;
  }

  await page.setViewportSize({ width: 1024, height: 720 });
  await page.goto("http://127.0.0.1:1439/__ui-review?surface=sidebar-scroll&variant=short");
  await page.getByRole("button", { name: "筛选任务" }).click();
  const filterVisible = await page.locator("#sidebar-task-filter-menu").evaluate((menu) => {
    const box = menu.getBoundingClientRect();
    const projectHeader = document.querySelector(".sidebar__list-section--projects > .sidebar__section-head").getBoundingClientRect();
    const element = document.elementFromPoint(box.left + box.width / 2, Math.max(box.top + 12, projectHeader.top + 12));
    return element !== null && menu.contains(element);
  });
  assert.ok(filterVisible, "task filter popover should remain above the project section");
  return layouts;
}
