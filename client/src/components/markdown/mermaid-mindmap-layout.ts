import type { InternalHelpers, LayoutData, SVG } from "mermaid";

type SizedNode = { id: string; width: number; height: number };
type TreeEdge = { start?: string; end?: string };
type Position = { x: number; y: number };

const NODE_GAP = 20;

/** Measured radial sectors retain the center-and-branches mindmap presentation. */
export function layoutMindmapTree(nodes: SizedNode[], edges: TreeEdge[]): Map<string, Position> {
  const positions = new Map<string, Position>();
  if (!nodes.length) return positions;
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const children = new Map(nodes.map((node) => [node.id, [] as string[]]));
  const parentIds = new Set<string>();
  for (const edge of edges) {
    if (!edge.start || !edge.end || !byId.has(edge.start) || !byId.has(edge.end)) {
      throw new Error("思维导图包含无效连接");
    }
    if (parentIds.has(edge.end)) throw new Error("思维导图节点有多个父节点");
    children.get(edge.start)!.push(edge.end);
    parentIds.add(edge.end);
  }
  const roots = nodes.filter((node) => !parentIds.has(node.id));
  if (roots.length !== 1) throw new Error("思维导图需要一个中心节点");
  const root = roots[0];
  const weights = new Map<string, number>();
  const visiting = new Set<string>();
  const measure = (id: string): number => {
    if (visiting.has(id)) throw new Error("思维导图包含循环连接");
    visiting.add(id);
    const items = children.get(id)!;
    const weight = Math.max(1, items.reduce((sum, child) => sum + measure(child), 0));
    weights.set(id, weight);
    visiting.delete(id);
    return weight;
  };
  measure(root.id);
  if (weights.size !== nodes.length) throw new Error("思维导图包含未连接的节点");
  const polar = new Map<string, { depth: number; angle: number }>();
  const allocate = (id: string, depth: number, start: number, span: number) => {
    polar.set(id, { depth, angle: start + span / 2 });
    let angle = start;
    for (const child of children.get(id)!) {
      const childSpan = span * weights.get(child)! / weights.get(id)!;
      allocate(child, depth + 1, angle, childSpan);
      angle += childSpan;
    }
  };
  allocate(root.id, 0, -Math.PI, Math.PI * 2);

  // Search a small deterministic set of orientations. Wider ellipses suit
  // Chinese categories plus long Latin tool names better than a fixed circle.
  // For each pair, separation on either axis is sufficient. Taking the maximum
  // of those pairwise requirements guarantees clear space for every node.
  let bestScore = Infinity;
  for (const ratio of [1.5, 1.8, 2.1]) for (let turn = 0; turn < 24; turn += 1) {
    const rotation = turn * Math.PI / 12;
    const units = nodes.map((node) => {
      const { depth, angle } = polar.get(node.id)!;
      return { x: depth * ratio * Math.cos(angle + rotation), y: depth * Math.sin(angle + rotation) };
    });
    let scale = 80;
    for (let a = 0; a < nodes.length; a += 1) for (let b = a + 1; b < nodes.length; b += 1) {
      const dx = Math.abs(units[a].x - units[b].x);
      const dy = Math.abs(units[a].y - units[b].y);
      const horizontal = dx < 1e-8 ? Infinity : ((nodes[a].width + nodes[b].width) / 2 + NODE_GAP) / dx;
      const vertical = dy < 1e-8 ? Infinity : ((nodes[a].height + nodes[b].height) / 2 + NODE_GAP) / dy;
      scale = Math.max(scale, Math.min(horizontal, vertical));
    }
    const candidate = units.map((point) => ({ x: point.x * scale, y: point.y * scale }));
    const width = Math.max(...candidate.map((point, index) => point.x + nodes[index].width / 2))
      - Math.min(...candidate.map((point, index) => point.x - nodes[index].width / 2));
    const height = Math.max(...candidate.map((point, index) => point.y + nodes[index].height / 2))
      - Math.min(...candidate.map((point, index) => point.y - nodes[index].height / 2));
    const score = Math.max(width, height * 1.8);
    if (score < bestScore) {
      bestScore = score;
      nodes.forEach((node, index) => positions.set(node.id, candidate[index]));
    }
  }
  return positions;
}

/** Mermaid's layout extension retains its parser, labels, shapes and SVG export. */
export async function render(data: LayoutData, svg: SVG, helpers: InternalHelpers): Promise<void> {
  const group = svg.select<SVGGElement>("g");
  helpers.insertMarkers(group, data.markers, data.type, data.diagramId);
  const edgePaths = group.append("g").attr("class", "edgePaths");
  const edgeLabels = group.append("g").attr("class", "edgeLabels");
  const nodeGroup = group.append("g").attr("class", "nodes");
  const nodeElements = new Map<string, Awaited<ReturnType<InternalHelpers["insertNode"]>>>();

  for (const node of data.nodes) {
    if (node.isGroup) throw new Error("思维导图不支持分组容器");
    // Unspecified shapes use compact rounded cards. Explicit circles, clouds,
    // squares and other Mermaid shapes still retain their original meaning.
    if (node.shape === "defaultMindmapNode") {
      node.shape = "rounded";
      node.width = 0;
      node.radius = 8;
      node.wrappingWidth = data.config.mindmap?.maxNodeWidth ?? 240;
    }
    const element = await helpers.insertNode(nodeGroup, node, { config: data.config, dir: "LR" });
    const box = element.node()!.getBBox();
    node.width = box.width;
    node.height = box.height;
    nodeElements.set(node.id, element);
  }

  const positions = layoutMindmapTree(
    data.nodes.map((node) => ({ id: node.id, width: node.width!, height: node.height! })),
    data.edges,
  );
  const byId = new Map(data.nodes.map((node) => [node.id, node]));
  for (const node of data.nodes) {
    const position = positions.get(node.id)!;
    node.x = position.x;
    node.y = position.y;
    nodeElements.get(node.id)!.attr("transform", `translate(${node.x}, ${node.y})`);
  }
  for (const edge of data.edges) {
    const start = byId.get(edge.start!)!;
    const end = byId.get(edge.end!)!;
    edge.points = [
      { x: start.x!, y: start.y! },
      { x: (start.x! + end.x!) / 2, y: (start.y! + end.y!) / 2 },
      { x: end.x!, y: end.y! },
    ];
    await helpers.insertEdgeLabel(edgeLabels, edge);
    const paths = helpers.insertEdge(edgePaths, edge, {}, data.type, start, end, data.diagramId);
    helpers.positionEdgeLabel(edge, paths);
  }
}
