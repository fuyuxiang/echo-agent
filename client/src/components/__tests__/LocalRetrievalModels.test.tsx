import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
const api=vi.hoisted(()=>({invoke:vi.fn(),pick:vi.fn(),tauri:true}));
vi.mock("@tauri-apps/api/core",()=>({invoke:api.invoke}));
vi.mock("@/lib/agent-client",()=>({filesystemPickDirectory:api.pick,openExternalUrl:vi.fn()}));
vi.mock("@/lib/tauri-kb-reader",()=>({isTauriAvailable:()=>api.tauri}));
import { LocalRetrievalModels } from "../LocalRetrievalModels";
const missing={state:"missing",downloadedBytes:0,totalBytes:400_000_000,embeddingReady:false,rerankerReady:false};
beforeEach(()=>{api.tauri=true;api.invoke.mockReset();api.pick.mockReset();api.invoke.mockResolvedValue(missing);});
describe("本地检索模型准备",()=>{
  it("离线导入失败保留可用模型，单独展示操作错误且可重新检查",async()=>{
    const ready={...missing,state:"ready",source:"bundled",embeddingReady:true,rerankerReady:true,operationError:null as string|null};
    api.pick.mockResolvedValue("/invalid/models");
    api.invoke.mockImplementation(async(command:string)=>command==="retrieval_models_import"
      ? {...ready,operationError:"离线模型导入未完成：文件缺失"} : ready);
    render(<LocalRetrievalModels/>);
    await screen.findByText("本地语义检索已就绪");
    fireEvent.click(screen.getByText("模型维护"));
    fireEvent.click(screen.getByRole("button",{name:"导入离线模型"}));
    expect(await screen.findByRole("alert")).toHaveTextContent("离线模型导入未完成：文件缺失 当前模型仍可使用。");
    expect(screen.getByText("本地语义检索已就绪")).toBeInTheDocument();
    expect(screen.queryByText("本地模型需要修复")).toBeNull();
    fireEvent.click(screen.getByRole("button",{name:"检查并修复模型"}));
    await waitFor(()=>expect(screen.queryByRole("alert")).toBeNull());
  });
  it("原生导入参数错误也不会把就绪模型显示为故障",async()=>{
    const ready={...missing,state:"ready",source:"repaired",embeddingReady:true,rerankerReady:true};
    api.pick.mockResolvedValue("/removed/folder");
    api.invoke.mockImplementation(async(command:string)=>{
      if(command==="retrieval_models_import") throw new Error("模型文件夹不存在");
      return ready;
    });
    render(<LocalRetrievalModels/>);await screen.findByText("本地语义检索已就绪");
    fireEvent.click(screen.getByText("模型维护"));
    fireEvent.click(screen.getByRole("button",{name:"导入离线模型"}));
    expect(await screen.findByRole("alert")).toHaveTextContent("模型文件夹不存在 当前模型仍可使用。");
    expect(screen.getByText("本地语义检索已就绪")).toBeInTheDocument();
  });
  it("不支持推理的电脑提前说明关键词回退，不提供无效下载",async()=>{
    api.invoke.mockResolvedValue({...missing,state:"unsupported",message:"本机暂不支持语义模型"});
    render(<LocalRetrievalModels/>);expect(await screen.findByText("关键词检索可用")).toBeInTheDocument();
    expect(screen.queryByRole("button",{name:/修复/})).not.toBeInTheDocument();
    expect(screen.queryByRole("button",{name:"导入离线模型"})).not.toBeInTheDocument();
  });
  it("内置资源异常时提供修复，保持关键词回退且不自动联网",async()=>{
    render(<LocalRetrievalModels/>);
    expect(await screen.findByRole("button",{name:"修复本地模型"})).toBeEnabled();
    expect(screen.getByText(/关键词搜索仍可使用/)).toBeInTheDocument();
    expect(api.invoke).toHaveBeenCalledWith("retrieval_models_status");
    expect(api.invoke).not.toHaveBeenCalledWith("retrieval_models_prepare",expect.anything());
  });
  it("下载中显示进度并可暂停",async()=>{
    let current=missing;
    api.invoke.mockImplementation(async(command:string)=>{
      if (command==="retrieval_models_prepare") current={...missing,state:"downloading",downloadedBytes:200_000_000};
      if (command==="retrieval_models_cancel") {current=missing;return true;}
      return current;
    });
    render(<LocalRetrievalModels/>);
    fireEvent.click(await screen.findByRole("button",{name:"修复本地模型"}));
    expect(await screen.findByText(/正在下载修复文件 · 50%/)).toBeInTheDocument();
    fireEvent.click(await screen.findByRole("button",{name:"暂停修复"}));
    await waitFor(()=>expect(api.invoke).toHaveBeenCalledWith("retrieval_models_cancel",undefined));
  });
  it("下载失败可重试且保留离线导入",async()=>{
    api.invoke.mockResolvedValue({...missing,state:"error",message:"下载中断，可继续"});
    render(<LocalRetrievalModels/>);
    expect(await screen.findByRole("alert")).toHaveTextContent("下载中断，可继续");
    expect(screen.getByRole("button",{name:"重试修复模型"})).toBeEnabled();
    expect(screen.getByRole("button",{name:"导入离线模型"})).toBeEnabled();
  });
  it("离线导入只传入用户选择的目录，取消选择无副作用",async()=>{
    api.pick.mockResolvedValueOnce(null).mockResolvedValueOnce("/offline/models");
    render(<LocalRetrievalModels/>);
    const button=await screen.findByRole("button",{name:"导入离线模型"});
    fireEvent.click(button);await waitFor(()=>expect(api.pick).toHaveBeenCalledTimes(1));
    expect(api.invoke.mock.calls.some(([command])=>command==="retrieval_models_import")).toBe(false);
    fireEvent.click(button);await waitFor(()=>expect(api.invoke).toHaveBeenCalledWith("retrieval_models_import",{directory:"/offline/models"}));
  });
  it("就绪说明资料仍会交给对话模型，浏览器预览隐藏原生准备入口",async()=>{
    api.invoke.mockResolvedValue({...missing,state:"ready",source:"bundled",embeddingReady:true,rerankerReady:true});
    const view=render(<LocalRetrievalModels/>);
    expect(await screen.findByText("本地语义检索已就绪")).toBeInTheDocument();
    expect(screen.getByText(/用于回答的资料会交给当前对话模型/)).toBeInTheDocument();
    expect(screen.getByText(/客户端已内置模型，无需下载/)).toBeInTheDocument();
    expect(api.invoke.mock.calls.every(([command])=>command==="retrieval_models_status")).toBe(true);
    const maintenance=screen.getByText("模型维护").closest("details");
    expect(maintenance?.open).toBe(false);
    fireEvent.click(screen.getByText("模型维护"));
    fireEvent.click(screen.getByRole("button",{name:"检查并修复模型"}));
    await waitFor(()=>expect(api.invoke).toHaveBeenCalledWith("retrieval_models_prepare",undefined));
    expect(screen.queryByRole("button",{name:/准备本地模型/})).not.toBeInTheDocument();
    view.unmount();api.tauri=false;api.invoke.mockClear();
    const preview=render(<LocalRetrievalModels/>);expect(preview.container).toBeEmptyDOMElement();expect(api.invoke).not.toHaveBeenCalled();
  });
  it("已修复的本地副本准确显示来源",async()=>{
    api.invoke.mockResolvedValue({...missing,state:"ready",source:"repaired",embeddingReady:true,rerankerReady:true});
    render(<LocalRetrievalModels/>);
    expect(await screen.findByText(/使用已校验的本地模型/)).toBeInTheDocument();
    expect(screen.queryByText(/客户端已内置模型，无需下载/)).not.toBeInTheDocument();
  });
});
