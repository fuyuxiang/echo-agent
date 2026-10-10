import { createMermaidConfig, detectMermaidDiagramKind, normalizeStandaloneMermaidSvg } from "./mermaid-svg";

let renderQueue: Promise<unknown> = Promise.resolve();
let mindmapRegistered = false;

/** initialize() is global; keep each configuration paired with its queued render. */
export function renderStandaloneMermaid(id: string, code: string, theme: "light" | "dark"): Promise<string> {
  const result = renderQueue.then(async () => {
    const mermaid = (await import("mermaid")).default;
    const kind = detectMermaidDiagramKind(code);
    if (kind === "mindmap" && !mindmapRegistered) {
      mermaid.registerLayoutLoaders([{ name: "echo-mindmap", loader: () => import("./mermaid-mindmap-layout") }]);
      mindmapRegistered = true;
    }
    await document.fonts?.ready;
    mermaid.initialize(createMermaidConfig(kind, theme, mermaid.mermaidAPI.getConfig().secure ?? []));
    const container = document.createElement("div");
    container.setAttribute("aria-hidden", "true");
    container.style.cssText = "position:fixed;left:-10000px;top:0;width:1600px;visibility:hidden;pointer-events:none";
    document.body.appendChild(container);
    try {
      const { svg } = await mermaid.render(id, code, container);
      return normalizeStandaloneMermaidSvg(svg);
    } finally {
      // Mermaid may leave its temporary error SVG behind when parsing fails.
      container.remove();
    }
  });
  // An invalid graph must not prevent subsequent code blocks from rendering.
  renderQueue = result.catch(() => {});
  return result;
}
