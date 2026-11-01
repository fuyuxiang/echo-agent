/**
 * Mermaid is normally rendered into the document, where a percentage sized
 * root SVG is useful. We render the result as a standalone Blob image instead.
 * A standalone image needs concrete dimensions; otherwise Chromium can use the
 * root `width="100%"` as the image viewport and clip the right/bottom edges.
 */
export type MermaidDiagramKind = "mindmap" | "flowchart" | "other";

export const MINDMAP_LABEL_MAX_WIDTH = 240;

export function detectMermaidDiagramKind(source: string): MermaidDiagramKind {
  const firstLine = source.trimStart()
    .replace(/^---\s*\r?\n[\s\S]*?\r?\n---\s*(?:\r?\n|$)/u, "")
    .replace(/^(?:\s*%%\{[\s\S]*?\}%%\s*|\s*%%[^\r\n]*(?:\r?\n|$))*/u, "")
    .trimStart().split(/\r?\n/u)[0]?.trim().toLowerCase() ?? "";

  if (firstLine.startsWith("mindmap")) return "mindmap";
  if (firstLine.startsWith("flowchart") || firstLine.startsWith("graph ")) return "flowchart";
  return "other";
}

export function createMermaidConfig(
  kind: MermaidDiagramKind,
  theme: "light" | "dark",
  secure: string[],
) {
  const protectedKeys = new Set(secure);
  const mermaidTheme: "base" | "dark" | "default" = kind === "mindmap" ? "base" : theme === "dark" ? "dark" : "default";
  // These values must remain stable because the output is decoded outside the
  // application DOM. Agent supplied init directives cannot switch them back to
  // percentage sizing or a different font after initialization.
  protectedKeys.add("htmlLabels");
  protectedKeys.add("securityLevel");
  protectedKeys.add("fontFamily");
  protectedKeys.add("useMaxWidth");
  if (kind === "mindmap") {
    // The layout and its embedded label CSS form one measurement contract.
    // Replacing either through an init directive would undo wrapping before
    // shape sizing, or make the exported image differ from the measured DOM.
    protectedKeys.add("layout");
    protectedKeys.add("themeCSS");
  }

  return {
    startOnLoad: false,
    securityLevel: "strict" as const,
    // Keep document styling and readable labels stable across Mermaid upgrades.
    look: "classic" as const,
    themeVariables: { fontSize: "16px" },
    secure: [...protectedKeys],
    theme: mermaidTheme,
    ...(kind === "mindmap" ? createMindmapTheme(theme) : {}),
    // Flowchart labels are kept as SVG text so the downloaded file has no
    // foreignObject dependency. Mindmaps need HTML labels for reliable wrapping
    // of long mixed Chinese/Latin labels.
    htmlLabels: kind === "mindmap",
    fontFamily: '"PingFang SC", "Microsoft YaHei", "Noto Sans CJK SC", sans-serif',
    useMaxWidth: false,
    flowchart: {
      useMaxWidth: false,
      wrappingWidth: 240,
    },
    mindmap: {
      useMaxWidth: false,
      maxNodeWidth: MINDMAP_LABEL_MAX_WIDTH,
      padding: 12,
    },
  };
}

function createMindmapTheme(theme: "light" | "dark") {
  const dark = theme === "dark";
  const text = dark ? "#e6edf5" : "#203247";
  const colors = dark
    ? ["#244058", "#243c57", "#25463c", "#3b3154", "#4d3f27", "#4d2e41", "#25464c", "#443b2d", "#303953"]
    : ["#d9edff", "#e4efff", "#e2f4eb", "#ede7fb", "#fff2d8", "#fbe5ef", "#dff3f5", "#ffebdc", "#e8ecfa"];
  const accents = dark
    ? ["#78b1f0", "#6fc79a", "#af98eb", "#dec180", "#e79fbf", "#7bc4cc", "#e8b589", "#96aae5"]
    : ["#426fa6", "#397d5b", "#795cab", "#967134", "#a3567e", "#357d87", "#9b673f", "#5f74b2"];
  const themeVariables: Record<string, string | boolean> = {
    darkMode: dark, fontSize: "16px", textColor: text, primaryTextColor: text,
    git0: colors[0], gitBranchLabel0: text,
  };
  for (let index = 0; index < 12; index += 1) {
    themeVariables[`cScale${index}`] = colors[index % colors.length];
    themeVariables[`cScaleLabel${index}`] = text;
    themeVariables[`cScaleInv${index}`] = accents[(index + accents.length - 1) % accents.length];
  }
  return {
    layout: "echo-mindmap",
    themeVariables,
    themeCSS: accents.map((color, index) => `.section-edge-${index}{stroke:${color};}
      .mindmap-node.section-${index} rect,.mindmap-node.section-${index} path{stroke:${color};stroke-width:1px;}`)
      .join("\n") + `\n.edge{stroke-width:1.8px;}.edge-depth-1{stroke-width:3px;}
      .section-root .nodeLabel{font-weight:600;}
      /* This must be embedded in the SVG, and active before insertNode measures
         the label. Do not use the mixed-case foreignObject type in selectors:
         Mermaid's sanitizer lowercases it, which fails in standalone XML. */
      .nodes .label div{display:inline-block!important;width:auto!important;
        max-width:${MINDMAP_LABEL_MAX_WIDTH}px!important;white-space:pre-wrap!important;
        overflow-wrap:anywhere!important;word-break:normal!important;text-align:center!important;}`,
  };
}

function replaceAttribute(attributes: string, name: string, value: string): string {
  const pattern = new RegExp(`\\s${name}\\s*=\\s*(?:"[^"]*"|'[^']*'|[^\\s>]+)`, "iu");
  const replacement = ` ${name}="${value}"`;
  return pattern.test(attributes) ? attributes.replace(pattern, replacement) : `${attributes}${replacement}`;
}

function readViewBox(rootAttributes: string): [number, number, number, number] | null {
  const match = rootAttributes.match(/\bviewBox\s*=\s*["']([^"']+)["']/iu);
  if (!match) return null;
  const values = match[1].trim().split(/[\s,]+/u).map(Number);
  if (values.length !== 4 || values.some((value) => !Number.isFinite(value))) return null;
  if (values[2] <= 0 || values[3] <= 0) return null;
  return values as [number, number, number, number];
}

/** Convert Mermaid's responsive root SVG into a self-contained image asset. */
export function normalizeStandaloneMermaidSvg(svg: string): string {
  const root = svg.match(/<svg\b([^>]*)>/iu);
  if (!root) return svg;
  const viewBox = readViewBox(root[1]);
  if (!viewBox) return svg;

  const [, , width, height] = viewBox;
  let attributes = root[1];
  attributes = replaceAttribute(attributes, "width", formatNumber(width));
  attributes = replaceAttribute(attributes, "height", formatNumber(height));
  attributes = replaceAttribute(attributes, "preserveAspectRatio", "xMidYMid meet");

  // Mermaid adds `max-width: ...px` for responsive DOM rendering. It is not
  // needed inside a Blob image and can cause a second viewport constraint.
  const styleMatch = attributes.match(/\sstyle\s*=\s*(["'])(.*?)\1/iu);
  if (styleMatch) {
    const style = styleMatch[2]
      .replace(/(?:^|;)\s*max-width\s*:\s*[^;]+;?/giu, "")
      .trim();
    attributes = style
      ? attributes.replace(styleMatch[0], ` style="${style}"`)
      : attributes.replace(styleMatch[0], "");
  }

  const normalized = svg.replace(root[0], `<svg${attributes}>`);
  return /\bclass\s*=\s*["'][^"']*\bmindmapDiagram\b/iu.test(attributes)
    ? padMindmapLabelBoxes(normalized)
    : normalized;
}

function padMindmapLabelBoxes(svg: string): string {
  // Mermaid sanitizes its SVG as HTML. XHTML labels can therefore contain
  // HTML void elements such as <br> rather than XML's <br/>. Serialize the
  // same inert HTML/SVG tree used for measurement before decoding it as an
  // image; a text replacement would also change escaped code or CSS strings.
  const template = document.createElement("template");
  template.innerHTML = svg;
  const root = template.content.firstElementChild;
  if (root?.localName !== "svg" || root.namespaceURI !== "http://www.w3.org/2000/svg"
    || template.content.childElementCount !== 1) {
    throw new Error("图表生成了无效的 SVG，请检查图表源码");
  }
  // HTML parsing treats xmlns on XHTML elements as an ordinary attribute.
  // XMLSerializer supplies their actual namespace; retaining the ordinary
  // declaration can produce duplicate xmlns attributes (notably in jsdom).
  for (const element of [root, ...root.querySelectorAll("*")]) {
    const namespace = element.getAttributeNode("xmlns");
    if (namespace && namespace.namespaceURI !== "http://www.w3.org/2000/xmlns/") {
      element.removeAttribute("xmlns");
    }
  }
  const xml = new DOMParser().parseFromString(new XMLSerializer().serializeToString(root), "image/svg+xml");
  if (xml.querySelector("parsererror")) throw new Error("图表生成了无效的 SVG，请检查图表源码");
  for (const box of xml.querySelectorAll(".nodes .label > foreignObject")) {
    if (box.hasAttribute("data-md-label-padding")) continue;
    const width = Number(box.getAttribute("width"));
    const height = Number(box.getAttribute("height"));
    if (!Number.isFinite(width) || !Number.isFinite(height) || width < 0 || height < 0) {
      throw new Error("图表标签尺寸无效，请检查图表源码");
    }
    // Empty labels are valid. Leave their zero-size box alone.
    if (!width || !height) continue;
    const slack = Math.max(8, Math.ceil(width * 0.06));
    box.setAttribute("width", formatNumber(width + slack));
    box.setAttribute("height", formatNumber(Math.ceil(height) + 2));
    box.setAttribute("data-md-label-padding", String(slack));
    // Preserve the label's original transform and natural content width.
    // Padding the clipping viewport after layout must not move the text or
    // enlarge the card, especially for short identifiers.
  }
  return new XMLSerializer().serializeToString(xml);
}

function formatNumber(value: number): string {
  return Number.isInteger(value) ? String(value) : String(Number(value.toFixed(3)));
}
