import assert from "node:assert/strict";

/** Exercise the production settings actions, including the formerly covered corner. */
export async function assertSettingsActionsSeparated(page) {
  const close = page.getByRole("button", { name: "关闭设置", exact: true });
  const add = page.getByRole("button", { name: "添加模型", exact: true });
  await page.getByRole("region", { name: "模型目录", exact: true }).waitFor();
  await add.waitFor();
  const closeBox = await close.boundingBox();
  const addBox = await add.boundingBox();
  assert(closeBox && addBox, "Settings actions must be visible");
  const overlapWidth = Math.max(0, Math.min(closeBox.x + closeBox.width, addBox.x + addBox.width) - Math.max(closeBox.x, addBox.x));
  const overlapHeight = Math.max(0, Math.min(closeBox.y + closeBox.height, addBox.y + addBox.height) - Math.max(closeBox.y, addBox.y));
  assert.equal(overlapWidth * overlapHeight, 0, "Close settings covers the add-model action");

  // The upper-right corner used to hit the settings close button at minimum size.
  await add.click({ position: { x: addBox.width - 3, y: 3 } });
  const editor = page.getByRole("dialog", { name: "添加模型", exact: true });
  await editor.waitFor();
  assert(await page.getByRole("dialog", { name: "设置", exact: true }).isVisible(), "Adding a model closed settings");
  await editor.getByRole("button", { name: "取消", exact: true }).click();
  await editor.waitFor({ state: "hidden" });
  assert(await close.isVisible(), "Cancelling model creation must return to settings");
  return { close: closeBox, add: addBox, overlapArea: 0, cornerClickOpenedEditor: true };
}
