import { describe, expect, it } from "vitest";
import { layoutMindmapTree } from "../mermaid-mindmap-layout";

function crowdedTree() {
  const nodes = [{ id: "root", width: 190, height: 190 }];
  const edges: { start: string; end: string }[] = [];
  for (const [branch, count] of [5, 4, 6, 2, 2, 4, 2, 1].entries()) {
    const id = `branch-${branch}`;
    nodes.push({ id, width: 120 + branch * 12, height: 48 });
    edges.push({ start: "root", end: id });
    for (let leaf = 0; leaf < count; leaf += 1) {
      const child = `${id}-${leaf}`;
      nodes.push({ id: child, width: leaf % 2 ? 360 : 140, height: leaf % 2 ? 72 : 40 });
      edges.push({ start: id, end: child });
    }
  }
  return { nodes, edges };
}

describe("Measured Mermaid mindmap layout", () => {
  it("keeps the 35-node tool map clear of overlaps and preserves every parent-child link", () => {
    const { nodes, edges } = crowdedTree();
    const positions = layoutMindmapTree(nodes, edges);
    expect(positions.size).toBe(35);
    expect(positions.get("root")).toEqual({ x: 0, y: 0 });
    for (let a = 0; a < nodes.length; a += 1) for (let b = a + 1; b < nodes.length; b += 1) {
      const first = nodes[a], second = nodes[b];
      const one = positions.get(first.id)!, two = positions.get(second.id)!;
      const separated = Math.abs(one.x - two.x) >= (first.width + second.width) / 2 + 16
        || Math.abs(one.y - two.y) >= (first.height + second.height) / 2 + 16;
      expect(separated, `${first.id} / ${second.id}`).toBe(true);
    }
    for (const edge of edges.filter((edge) => edge.start !== "root")) {
      const parent = positions.get(edge.start)!, child = positions.get(edge.end)!;
      expect(Math.hypot(child.x / 1.8, child.y)).toBeGreaterThan(Math.hypot(parent.x / 1.8, parent.y));
    }
    expect(layoutMindmapTree(nodes, edges)).toEqual(positions);
  });

  it("handles a single root and deep asymmetric branches with large explicit shapes", () => {
    expect(layoutMindmapTree([{ id: "only", width: 280, height: 200 }], [])).toEqual(new Map([["only", { x: 0, y: 0 }]]));
    const positions = layoutMindmapTree([
      { id: "root", width: 180, height: 180 },
      { id: "large", width: 400, height: 300 },
      { id: "child", width: 600, height: 240 },
      { id: "last", width: 120, height: 60 },
    ], [{ start: "root", end: "large" }, { start: "large", end: "child" }, { start: "child", end: "last" }]);
    for (const [one, two, width, height] of [["root", "large", 290, 240], ["large", "child", 500, 270], ["child", "last", 360, 150]] as const) {
      const first = positions.get(one)!, second = positions.get(two)!;
      expect(Math.abs(first.x - second.x) >= width + 20 || Math.abs(first.y - second.y) >= height + 20).toBe(true);
    }
  });
});
