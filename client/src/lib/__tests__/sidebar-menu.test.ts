import { afterEach, describe, expect, it } from "vitest";
import {
  SIDEBAR_MENU_STORAGE_KEY, isSidebarMenuItemActive, loadSidebarMenuLayout,
  moveSidebarMenuItem, normalizeSidebarMenuLayout, saveSidebarMenuLayout,
} from "../sidebar-menu";

afterEach(() => localStorage.removeItem(SIDEBAR_MENU_STORAGE_KEY));

describe("sidebar menu preferences", () => {
  it("repairs stale, duplicated and malformed entries without losing destinations", () => {
    const result = normalizeSidebarMenuLayout({
      primary: ["automation", "retired", "automation", "__proto__", null],
      more: ["projects", "automation"],
    });
    expect(result).toEqual({
      primary: ["automation", "organization", "coding", "capabilities"],
      more: ["projects", "knowledge"],
    });
    expect(normalizeSidebarMenuLayout({ primary: "projects" })).toEqual(normalizeSidebarMenuLayout(null));
  });

  it("falls back to defaults when storage is corrupted", () => {
    localStorage.setItem(SIDEBAR_MENU_STORAGE_KEY, "{broken");
    expect(loadSidebarMenuLayout()).toEqual(normalizeSidebarMenuLayout(null));
  });

  it("moves between groups and respects before/after insertion boundaries", () => {
    const initial = normalizeSidebarMenuLayout(null);
    const moved = moveSidebarMenuItem(initial, "knowledge", "primary", 0);
    const reordered = moveSidebarMenuItem(moved, "knowledge", "primary", 3);
    expect(reordered.primary).toEqual(["projects", "organization", "knowledge", "coding", "capabilities"]);
    expect(reordered.more).toEqual(["automation"]);
    expect(initial.primary).toHaveLength(4);
    expect(moveSidebarMenuItem(reordered, "knowledge", "primary", 2)).toEqual(reordered);
  });

  it("persists ordering and permits either group to be empty", () => {
    let layout = normalizeSidebarMenuLayout(null);
    for (const id of [...layout.more]) layout = moveSidebarMenuItem(layout, id, "primary");
    saveSidebarMenuLayout(layout);
    expect(loadSidebarMenuLayout()).toEqual(layout);
    expect(loadSidebarMenuLayout().more).toEqual([]);
    for (const id of [...layout.primary]) layout = moveSidebarMenuItem(layout, id, "more");
    saveSidebarMenuLayout(layout);
    expect(loadSidebarMenuLayout().primary).toEqual([]);
    expect(loadSidebarMenuLayout().more).toHaveLength(6);
  });

  it("recognizes aliases independently of the entry's placement", () => {
    expect(isSidebarMenuItemActive("automation", "自动化")).toBe(true);
    expect(isSidebarMenuItemActive("capabilities", "插件")).toBe(true);
    expect(isSidebarMenuItemActive("organization", "插件")).toBe(false);
  });
});
