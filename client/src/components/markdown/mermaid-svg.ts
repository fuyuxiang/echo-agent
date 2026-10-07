/**
 * Mermaid is normally rendered into the document, where a percentage sized
 * root SVG is useful. We render the result as a standalone Blob image instead.
 * A standalone image needs concrete dimensions; otherwise Chromium can use the
 * root `width="100%"` as the image viewport and clip the right/bottom edges.
 */
export type MermaidDiagramKind = "mindmap" | "flowchart" | "other";

export function detectMermaidDiagramKind(source: string): MermaidDiagramKind {
  const firstLine = source
    .replace(/^\s*%%\{[\s\S]*?\}%%\s*/u, "")
    .split(/\r?\n/u)
    .find((line) => line.trim() && !line.trimStart().startsWith("%%"))
    ?.trim()
    .toLowerCase() ?? "";

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
  const mermaidTheme: "dark" | "default" = theme === "dark" ? "dark" : "default";
  // These values must remain stable because the output is decoded outside the
  // application DOM. Agent supplied init directives cannot switch them back to
  // percentage sizing or a different font after initialization.
  protectedKeys.add("htmlLabels");
  protectedKeys.add("fontFamily");
  protectedKeys.add("useMaxWidth");

  return {
    startOnLoad: false,
    securityLevel: "strict" as const,
    secure: [...protectedKeys],
    theme: mermaidTheme,
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
      maxNodeWidth: 320,
      padding: 24,
    },
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
