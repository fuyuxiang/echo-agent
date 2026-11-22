(expected, index, point) => {
  const path = globalThis.__echoFrameRegistration?.path();
  if (!path || JSON.stringify(path) !== JSON.stringify(expected)) return null;
  const child = window.frames[index];
  if (!child) return null;
  function owner(root) {
    for (const node of root.querySelectorAll("*")) {
      if (["iframe", "frame"].includes(node.localName) && node.contentWindow === child) return node;
      if (node.shadowRoot) {
        const found = owner(node.shadowRoot);
        if (found) return found;
      }
    }
    return null;
  }
  const element = owner(document);
  if (!element?.isConnected) return null;
  for (let node = element; node; node = node.parentElement || node.getRootNode().host) {
    const css = getComputedStyle(node);
    if (css.transform !== "none" || css.perspective !== "none" ||
        (css.translate && css.translate !== "none") || (css.rotate && css.rotate !== "none") ||
        (css.scale && css.scale !== "none") || Number(css.zoom || 1) !== 1) return null;
  }
  const box = element.getBoundingClientRect();
  let x = box.left + element.clientLeft;
  let y = box.top + element.clientTop;
  if (point) {
    x += point.x;
    y += point.y;
    let hit = document.elementFromPoint(x, y);
    for (let i = 0; i < 20 && hit?.shadowRoot; i++) {
      const inner = hit.shadowRoot.elementFromPoint?.(x, y);
      if (!inner || inner === hit) break;
      hit = inner;
    }
    if (hit !== element) return null;
  }
  return { x, y };
}
