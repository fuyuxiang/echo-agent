import assert from "node:assert/strict";

/** Exercise the production settings actions, including the formerly covered corner. */
export async function assertSettingsActionsSeparated(page) {
  const close = page.getByRole("button", { name: "关闭设置", exact: true });
  const manage = page.getByRole("button", { name: "管理模型连接", exact: true });
  const add = page.getByRole("button", { name: "添加模型连接", exact: true });
  // Populated catalogs expose connection management from the model overview;
  // empty catalogs can add immediately. Wait for either loaded state first.
  await manage.or(add).first().waitFor();
  const navigatedToConnections = await manage.isVisible();
  if (navigatedToConnections) await manage.click();
  await add.waitFor();
  const closeBox = await close.boundingBox();
  const addBox = await add.boundingBox();
  assert(closeBox && addBox, "Settings actions must be visible");
  const overlapWidth = Math.max(0, Math.min(closeBox.x + closeBox.width, addBox.x + addBox.width) - Math.max(closeBox.x, addBox.x));
  const overlapHeight = Math.max(0, Math.min(closeBox.y + closeBox.height, addBox.y + addBox.height) - Math.max(closeBox.y, addBox.y));
  assert.equal(overlapWidth * overlapHeight, 0, "Close settings covers the add-connection action");

  // The upper-right corner used to hit the settings close button at minimum size.
  await add.click({ position: { x: addBox.width - 3, y: 3 } });
  const editor = page.getByRole("dialog", { name: "添加模型连接", exact: true });
  await editor.waitFor();
  assert(await page.getByRole("dialog", { name: "设置", exact: true }).isVisible(), "Adding a connection closed settings");
  await editor.getByRole("button", { name: "取消", exact: true }).click();
  await editor.waitFor({ state: "hidden" });
  assert(await close.isVisible(), "Cancelling the connection must return to settings");
  return { close: closeBox, add: addBox, overlapArea: 0, cornerClickOpenedEditor: true, navigatedToConnections };
}
