import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { GroupSettings } from "../GroupWorkspace";
import { groupSettings, type Group } from "../group-client";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));
vi.mock("../group-client", async original => ({ ...await original<typeof import("../group-client")>(), groupSettings: vi.fn(async () => {}) }));
const group = (): Group => ({id:"settings",name:"旧名称",goal:"原目标",cwd:"/work",sourceSessionId:null,sourceTeamId:null,coordinatorId:"c",members:[],messages:[],works:[],decisions:[],archived:false,pinned:false,paused:false,revision:1,updatedAt:1});
describe("专家团设置与外部更新", () => {
  it("已配置但不可用的模型明确显示且不能静默保存成其他模型", () => {
    const initial = group();
    initial.members = [{ id: "c", name: "协调员", expert: null, role: "协调", modelId: "missing", sessionId: null, discussionSessionId: null, generation: 0 }];
    render(<GroupSettings group={initial} models={[{ id: "available", label: "可用模型" }]} onSaved={vi.fn()} onError={vi.fn()} />);
    expect(screen.getByRole("combobox", { name: "模型" })).toHaveValue("missing");
    expect(screen.getByRole("option", { name: "不可用：missing" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "保存专家团设置" })).toBeDisabled();
    fireEvent.change(screen.getByRole("combobox", { name: "模型" }), { target: { value: "available" } });
    expect(screen.getByRole("button", { name: "保存专家团设置" })).toBeEnabled();
  });
  it("菜单重命名同步到未编辑的名称，保存时保留正在编辑的目标", async () => {
    const initial=group(), onSaved=vi.fn();
    const {rerender}=render(<GroupSettings group={initial} models={[]} onSaved={onSaved} onError={vi.fn()} />);
    fireEvent.change(screen.getByRole("textbox",{name:"协作目标"}),{target:{value:"未保存的新目标"}});
    rerender(<GroupSettings group={{...initial,name:"新名称",revision:2}} models={[]} onSaved={onSaved} onError={vi.fn()} />);
    expect(screen.getByRole("textbox",{name:"专家团名称"})).toHaveValue("新名称");
    expect(screen.getByRole("textbox",{name:"协作目标"})).toHaveValue("未保存的新目标");
    fireEvent.click(screen.getByRole("button",{name:"保存专家团设置"}));
    await waitFor(()=>expect(onSaved).toHaveBeenCalled());
    expect(groupSettings).toHaveBeenLastCalledWith("settings",expect.objectContaining({name:"新名称",goal:"未保存的新目标",expectedRevision:2}));
  });
  it("同一字段冲突保留输入，明确选择后才允许保存", () => {
    const initial=group(), props={models:[],onSaved:vi.fn(),onError:vi.fn()};
    const {rerender}=render(<GroupSettings group={initial} {...props} />);
    fireEvent.change(screen.getByRole("textbox",{name:"专家团名称"}),{target:{value:"我的名称"}});
    rerender(<GroupSettings group={{...initial,name:"其他窗口的名称",revision:2}} {...props} />);
    expect(screen.getByRole("alert")).toHaveTextContent("名称已有其他修改");
    expect(screen.getByRole("button",{name:"保存专家团设置"})).toBeDisabled();
    expect(screen.getByRole("textbox",{name:"专家团名称"})).toHaveValue("我的名称");
    fireEvent.click(screen.getByRole("button",{name:"使用最新设置"}));
    expect(screen.getByRole("textbox",{name:"专家团名称"})).toHaveValue("其他窗口的名称");
    expect(screen.getByRole("button",{name:"保存专家团设置"})).toBeEnabled();
  });
});
