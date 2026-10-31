import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ModelSelector } from "../ModelSelector";
import { ModelManagementContext } from "@/lib/model-management";

describe("ModelSelector", () => {
  it("同一连接的同名别名可从菜单、提示和无障碍名称区分，并选择正确路由", () => {
    const change = vi.fn();
    render(<ModelSelector modelId="gateway/fast" models={[
      {id:"gateway/fast",label:"通用模型",remoteModelId:"fast",providerId:"gateway",providerLabel:"我的网关"},
      {id:"gateway/strong",label:"通用模型",remoteModelId:"strong",providerId:"gateway",providerLabel:"我的网关"},
    ]} onModelChange={change} />);
    const trigger = screen.getByRole("button", {name:"通用模型"});
    expect(trigger).toHaveAttribute("title", "通用模型 · 个人 · 我的网关 · fast");
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("option", {name:"通用模型 · 个人 · 我的网关 · strong"}));
    expect(change).toHaveBeenCalledExactlyOnceWith("gateway/strong");
  });
  it("菜单默认只显示模型名称，不展示来源徽标或内部目录 ID", () => {
    render(<ModelSelector modelId="plain-model" models={[
      { id: "plain-model", label: "plain-model" },
      { id: "provider/model-id", label: "常用模型" },
    ]} onModelChange={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "plain-model" }));
    expect(screen.getByRole("option", { name: "plain-model" })).not.toHaveTextContent("个人");
    expect(document.querySelector(".model-selector__source")).toBeNull();
    expect(document.querySelector(".model-selector__connection")).toBeNull();
    expect(screen.getByRole("option", { name: /常用模型/ })).not.toHaveTextContent("provider/model-id");
  });
  it("长名称省略显示时仍能通过提示和菜单确认完整模型，切换保留真实 id", () => {
    const models = [
      { id: "connection/MiniMax-M3.1-Flash-20261019", label: "MiniMax-M3.1-Flash-超长模型名称" },
      { id: "another/actual-model", label: "另一个模型" },
    ];
    const change = vi.fn();
    render(<ModelSelector models={models} modelId={models[0].id} onModelChange={change} ariaLabel="开发任务模型" />);
    const trigger = screen.getByRole("button", { name: `开发任务模型：${models[0].label}` });
    expect(trigger).toHaveAttribute("title", models[0].label);
    fireEvent.click(trigger);
    expect(screen.getByRole("option", { name: new RegExp(models[0].label) })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("option", { name: /另一个模型/ }));
    expect(change).toHaveBeenCalledWith("another/actual-model");
  });

  it("内置、组织和个人同名模型可区分，选择回传原始 ID", () => {
    const models = [
      { id: "echoagent-ojlab/Echo Reed", source: "builtin" as const },
      { id: "organization/Echo Reed", source: "organization" as const, providerLabel: "研发团队" },
      { id: "custom-2/Echo Reed", remoteModelId: "Echo Reed", source: "personal" as const, providerLabel: "我的网关" },
      { id: "custom-3/Echo Reed", remoteModelId: "Echo Reed", source: "personal" as const, providerLabel: "备用网关" },
    ];
    const change = vi.fn();
    render(<ModelSelector models={models} modelId={models[0].id} onModelChange={change} />);
    const trigger = screen.getByRole("button", { name: "Echo Reed" });
    expect(trigger.title).toBe("Echo Reed · 内置");
    expect(trigger.textContent).toBe("Echo Reed");
    for (const [index, name] of ["Echo Reed · 内置", "Echo Reed · 组织 · 研发团队", "Echo Reed · 个人 · 我的网关", "Echo Reed · 个人 · 备用网关"].entries()) {
      fireEvent.click(trigger);
      const menu = screen.getByRole("listbox");
      expect(menu.querySelector(".model-selector__source")).toBeNull();
      expect(menu.textContent).not.toMatch(/echoagent-ojlab\/|organization\/|custom-[23]\//);
      fireEvent.click(screen.getByRole("option", { name }));
      expect(change).toHaveBeenLastCalledWith(models[index].id);
    }
  });

  it("不同来源轻量分组，模型行不重复来源且切换保留真实ID", () => {
    const models = [
      {id:"echoagent-ojlab/Echo Reed",source:"builtin" as const},
      {id:"organization/MiniMax-M3.1-Flash",remoteModelId:"MiniMax-M3.1-Flash",source:"organization" as const,providerLabel:"组织提供"},
      {id:"personal/gpt",label:"我的常用模型",source:"personal" as const,providerLabel:"个人网关"},
    ];
    const onModelChange=vi.fn();
    const view=render(<ModelSelector models={models} modelId={models[0].id} onModelChange={onModelChange}/>);
    const trigger=screen.getByRole("button",{name:"Echo Reed"});
    expect(trigger.title).toBe("Echo Reed");fireEvent.click(trigger);
    const menu=screen.getByRole("listbox");
    expect(screen.getAllByRole("group").map(group => group.getAttribute("aria-labelledby") && document.getElementById(group.getAttribute("aria-labelledby")!)?.textContent)).toEqual(["组织提供", "我的连接", "内置模型"]);
    expect(screen.getByRole("option", { name: "MiniMax-M3.1-Flash" }).textContent).toBe("MiniMax-M3.1-Flash");
    expect(menu.textContent).not.toContain("个人网关");
    fireEvent.click(screen.getByRole("option",{name:"MiniMax-M3.1-Flash"}));
    expect(onModelChange).toHaveBeenCalledWith(models[1].id);
    view.rerender(<ModelSelector models={models} modelId={models[1].id} onModelChange={onModelChange}/>);
    expect(screen.getByRole("button",{name:"MiniMax-M3.1-Flash"})).toHaveAttribute("title","MiniMax-M3.1-Flash");
    expect(view.container.textContent).toBe("MiniMax-M3.1-Flash");
  });

  it("菜单通过 body portal 渲染并在上方空间不足时向下翻转", () => {
    render(
      <div data-testid="clipping-card" style={{ overflow: "hidden" }}>
        <ModelSelector
          modelId="model-a"
          models={[{ id: "model-a", label: "模型 A" }]}
          onModelChange={vi.fn()}
        />
      </div>,
    );
    const trigger = screen.getByRole("button", { name: "模型 A" });
    trigger.getBoundingClientRect = () => ({
      x: 12,
      y: 10,
      top: 10,
      right: 132,
      bottom: 42,
      left: 12,
      width: 120,
      height: 32,
      toJSON: () => ({}),
    });

    fireEvent.click(trigger);

    const menu = screen.getByRole("listbox", { name: "选择模型" });
    const popup = screen.getByRole("dialog", { name: "选择模型" });
    expect(popup.parentElement).toBe(document.body);
    expect(popup).toHaveStyle({ position: "fixed", zIndex: "var(--echo-layer-popover)" });
    expect(popup).toHaveAttribute("data-placement", "bottom");
    expect(popup).toContainElement(menu);
  });

  it("弹窗内的 portal 菜单可显式使用弹窗局部层", () => {
    render(
      <ModelSelector
        modelId="model-a"
        models={[{ id: "model-a", label: "模型 A" }]}
        onModelChange={vi.fn()}
        menuZIndex="var(--echo-layer-dialog-popover)"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "模型 A" }));
    expect(screen.getByRole("dialog", { name: "选择模型" })).toHaveStyle({
      zIndex: "var(--echo-layer-dialog-popover)",
    });
  });

  it("加载模型信息时不显示未选择，恢复后显示实际模型", () => {
    const onModelChange = vi.fn();
    const { rerender } = render(<ModelSelector models={[{ id: "model-a", label: "模型 A" }]} modelLoading onModelChange={onModelChange} />);
    expect(screen.getByRole("button", { name: "正在同步模型…" })).toBeDisabled();
    expect(screen.queryByText("请选择模型")).toBeNull();
    rerender(<ModelSelector models={[{ id: "model-a", label: "模型 A" }]} modelId="model-a" onModelChange={onModelChange} />);
    expect(screen.getByRole("button", { name: "模型 A" })).toBeEnabled();
  });

  it("空配置时显示未配置模型，不显示 Runtime 默认 id", () => {
    render(
      <ModelSelector
        modelId="runtime-default"
        models={[]}
        onModelChange={vi.fn()}
      />,
    );

    const trigger = screen.getByRole("button", { name: "未配置模型" });
    expect(trigger).toBeDisabled();
    expect(screen.queryByText("runtime-default")).toBeNull();
  });

  it("有配置时显示并可切换模型", () => {
    const onModelChange = vi.fn();
    render(
      <ModelSelector
        modelId="glm-5"
        models={[
          { id: "glm-5", label: "GLM 5" },
          { id: "deepseek-chat", label: "DeepSeek Chat" },
        ]}
        onModelChange={onModelChange}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "GLM 5" }));
    fireEvent.click(screen.getByRole("option", { name: /DeepSeek Chat/ }));
    expect(onModelChange).toHaveBeenCalledWith("deepseek-chat");
  });

  it("选择模型时不触发父级的设置引导", () => {
    const onParentClick = vi.fn();
    const onModelChange = vi.fn();
    render(
      <div onClick={onParentClick}>
        <ModelSelector
          models={[{ id: "MiniMax-M3", label: "MiniMax M3" }]}
          onModelChange={onModelChange}
        />
      </div>,
    );

    fireEvent.click(screen.getByRole("button", { name: "请选择模型" }));
    fireEvent.click(screen.getByRole("option", { name: /MiniMax M3/ }));

    expect(onModelChange).toHaveBeenCalledWith("MiniMax-M3");
    expect(onParentClick).not.toHaveBeenCalled();
  });

  it("支持方向键导航、Escape 关闭与焦点恢复", () => {
    render(
      <ModelSelector
        modelId="glm-5"
        models={[
          { id: "glm-5", label: "GLM 5" },
          { id: "deepseek-chat", label: "DeepSeek Chat" },
        ]}
        onModelChange={vi.fn()}
      />,
    );
    const trigger = screen.getByRole("button", { name: "GLM 5" });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    const first = screen.getByRole("option", { name: /GLM 5/ });
    const second = screen.getByRole("option", { name: /DeepSeek Chat/ });
    expect(first).toHaveFocus();
    fireEvent.keyDown(first, { key: "ArrowDown" });
    expect(second).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(trigger).toHaveFocus();
  });

  it("同一页多个模型选择器具有独立可访问标识", () => {
    render(
      <>
        <ModelSelector
          ariaLabel="选择项目默认模型"
          modelId="model-a"
          models={[{ id: "model-a", label: "模型 A" }]}
          onModelChange={vi.fn()}
        />
        <ModelSelector
          ariaLabel="选择项目对话模型"
          modelId="model-a"
          models={[{ id: "model-a", label: "模型 A" }]}
          onModelChange={vi.fn()}
        />
      </>,
    );

    const first = screen.getByRole("button", { name: /选择项目默认模型/ });
    const second = screen.getByRole("button", { name: /选择项目对话模型/ });
    fireEvent.click(first);
    const firstControls = first.getAttribute("aria-controls");
    fireEvent.click(second);
    const secondControls = second.getAttribute("aria-controls");

    expect(firstControls).toBeTruthy();
    expect(secondControls).toBeTruthy();
    expect(firstControls).not.toBe(secondControls);
  });

  it("管理入口独立于选项列表，定位当前模型且先关闭项目抽屉", () => {
    const actions: string[] = [];
    const manage = vi.fn(id => actions.push(`manage:${id}`));
    const change = vi.fn();
    render(<ModelManagementContext.Provider value={manage}><ModelSelector modelId="org/a"
      models={[{ id: "org/a", label: "模型 A", source: "organization" }]}
      onModelChange={change} onBeforeManageModels={() => actions.push("close-drawer")} /></ModelManagementContext.Provider>);
    fireEvent.click(screen.getByRole("button", { name: "模型 A" }));
    const management = screen.getByRole("button", { name: "管理模型" });
    expect(screen.getByRole("listbox")).not.toContainElement(management);
    fireEvent.click(management);
    expect(actions).toEqual(["close-drawer", "manage:org/a"]);
    expect(change).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("空配置时可直接进入配置，加载时不触发管理", () => {
    const manage = vi.fn();
    const view = render(<ModelSelector models={[]} onModelChange={vi.fn()} onManageModels={manage} />);
    fireEvent.click(screen.getByRole("button", { name: "配置模型" }));
    expect(manage).toHaveBeenCalledExactlyOnceWith(undefined);
    view.rerender(<ModelSelector models={[]} modelLoading onModelChange={vi.fn()} onManageModels={manage} />);
    expect(screen.getByRole("button", { name: "正在同步模型…" })).toBeDisabled();
  });

  it("跨组方向键按视觉顺序导航，单一来源不显示冗余组标题", () => {
    const change = vi.fn();
    const view = render(<ModelSelector models={[
      { id: "b", label: "内置 B", source: "builtin" },
      { id: "p", label: "个人 P", source: "personal" },
      { id: "o", label: "组织 O", source: "organization" },
    ]} onModelChange={change} />);
    fireEvent.click(screen.getByRole("button", { name: "请选择模型" }));
    expect(screen.getByRole("option", { name: "组织 O" })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    expect(screen.getByRole("option", { name: "个人 P" })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "End" });
    expect(screen.getByRole("option", { name: "内置 B" })).toHaveFocus();
    fireEvent.keyDown(document, { key: "Escape" });
    view.rerender(<ModelSelector models={[{ id: "p", label: "个人 P", source: "personal" }]} onModelChange={change} />);
    fireEvent.click(screen.getByRole("button", { name: "请选择模型" }));
    expect(document.querySelector(".model-selector__group-label")).toBeNull();
  });
});
