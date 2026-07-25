import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { Sidebar } from "./Sidebar";

describe("Sidebar", () => {
  it("默认中文渲染菜单,点 EN 切英文", () => {
    render(<MemoryRouter><Sidebar /></MemoryRouter>);
    expect(screen.getByText("概览")).toBeInTheDocument();
    fireEvent.click(screen.getByText("EN"));
    expect(screen.getByText("Overview")).toBeInTheDocument();
  });
});
