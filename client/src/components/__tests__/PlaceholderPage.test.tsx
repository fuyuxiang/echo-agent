import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";

vi.mock("../experts-panel", () => ({
  ExpertsPanel: ({ initialTab, hideNavigation }: { initialTab?: string; hideNavigation?: boolean }) => (
    <div data-testid="experts-panel" data-embedded={hideNavigation}>{initialTab}</div>
  ),
}));
vi.mock("../PluginsPanel", () => ({
  PluginsPanel: () => <div>installed plugins</div>,
}));

import { PlaceholderPage } from "../PlaceholderPage";

describe("PlaceholderPage", () => {
  it("未注册路由显示明确错误，不伪装成待上线功能", () => {
    render(<PlaceholderPage label="某个未实现功能" />);
    expect(screen.getByRole("heading", { name: "无法打开「某个未实现功能」" })).toBeInTheDocument();
    expect(screen.getByText(/未注册该功能路由/)).toBeInTheDocument();
  });

  it.each(["代码开发", "助理", "灵感", "网页预览", "策略设置", "发现", "插件市场", "插件·市场"])(
    "已移除路由「%s」不会再渲染旧功能页",
    (label) => {
      render(<PlaceholderPage label={label} />);
      expect(
        screen.getByRole("heading", { name: `无法打开「${label}」` }),
      ).toBeInTheDocument();
    },
  );

  it.each([
    ["专家·技能·连接器", "experts"],
    ["技能", "skills"],
    ["连接器", "connectors"],
  ])("Slash 路由「%s」直达 %s 页签", async (label, tab) => {
    render(<PlaceholderPage label={label} />);
    expect(await screen.findByTestId("experts-panel")).toHaveTextContent(tab);
  });


  it("知识库页面只展示知识库，不混入个人记忆导航", async () => {
    render(<PlaceholderPage label="知识库" />);
    expect(await screen.findByRole("region", { name: "知识库" })).toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "资料与记忆" })).toBeNull();
    expect(screen.queryByText("个人记忆")).toBeNull();
  });

  it("扩展页面保持单层导航，可往返全部本地类别", async () => {
    function Page() {
      const [label, setLabel] = useState("专家·技能·连接器");
      return <PlaceholderPage label={label} onNavigate={setLabel} />;
    }
    render(<Page />);
    const panel = await screen.findByTestId("experts-panel");
    expect(panel).toHaveAttribute("data-embedded", "true");
    expect(screen.queryByText("专家、技能与连接器")).toBeNull();
    expect(screen.getAllByRole("navigation", { name: "专家·技能·连接器管理" })).toHaveLength(1);
    const nav = screen.getByRole("navigation", { name: "专家·技能·连接器管理" });
    expect(within(nav).getAllByRole("button").map(button => button.textContent)).toEqual(["专家", "专家团", "技能", "连接器", "插件"]);
    expect(screen.queryByRole("button", { name: "浏览市场" })).toBeNull();
    for (const [label, content] of [["技能", "skills"], ["连接器", "connectors"], ["专家", "experts"]]) {
      fireEvent.click(within(nav).getByRole("button", { name: label }));
      expect(await screen.findByTestId("experts-panel")).toHaveTextContent(content);
      expect(within(nav).getByRole("button", { name: label })).toHaveAttribute("aria-current", "page");
    }
    fireEvent.click(within(nav).getByRole("button", { name: "插件" }));
    expect(await screen.findByText("installed plugins")).toBeInTheDocument();
    fireEvent.click(within(nav).getByRole("button", { name: "专家" }));
    expect(await screen.findByTestId("experts-panel")).toHaveTextContent("experts");
  });
});
