// These page fixtures exercise populated runtime lists. A mounted React root
// can still contain Suspense or IPC loading states, so require real content.
const pageContent = new Map([
  ["page:技能", ".capability-page--skills .sk-inst-row"],
  ["page:连接器", ".capability-page--connectors .mcp-item"],
  ["page:项目", ".project-management .template-grid > *"],
]);

export async function waitForVisualPolishSurface(page, spec) {
  if (spec.startsWith("page:")) {
    const selector = pageContent.get(spec);
    if (!selector) throw new Error(`Missing visual review readiness condition: ${spec}`);
    await page.locator(selector).first().waitFor({ state: "visible" });
    return;
  }
  await page.locator("#root > *").first().waitFor({ state: "visible" });
}
