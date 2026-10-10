/**
 * Mermaid is normally rendered into the document, where a percentage sized
 * root SVG is useful. We render the result as a standalone Blob image instead.
 * A standalone image needs concrete dimensions; otherwise Chromium can use the
 * root `width="100%"` as the image viewport and clip the right/bottom edges.
 */
export type MermaidDiagramKind = "mindmap" | "flowchart" | "other";

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

  return {
    startOnLoad: false,
    securityLevel: "strict" as const,
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
      maxNodeWidth: 240,
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
    : ["#75a1d9", "#72b795", "#a08bd0", "#d6b66e", "#d996b5", "#70b4bd", "#d9a078", "#8d9fce"];
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
      .section-root .nodeLabel{font-weight:600;}`,
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

  return svg.replace(root[0], `<svg${attributes}>`);
}

function formatNumber(value: number): string {
  return Number.isInteger(value) ? String(value) : String(Number(value.toFixed(3)));
}
