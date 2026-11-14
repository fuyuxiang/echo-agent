import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { BrowserTaskSettings } from "../BrowserTaskSettings";
import { useSessionStore } from "@/stores/session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import * as client from "@/lib/automation-client";
import type { AutomationStatus } from "@/lib/automation-client";
vi.mock("@/lib/automation-client",()=>({
  automationStatus:vi.fn(),automationPendingApprovals:vi.fn(async()=>[]),automationResolveApproval:vi.fn(async()=>true),
  automationClearBrowserData:vi.fn(),automationSetPrivateNetwork:vi.fn(),automationPairBrowser:vi.fn(),automationDisconnectBrowser:vi.fn(),automationUseIsolatedBrowser:vi.fn(),
  onAutomationStatus:vi.fn(async()=>()=>{}),onAutomationApproval:vi.fn(async()=>()=>{}),onAutomationApprovalClosed:vi.fn(async()=>()=>{}),
}));
const status:AutomationStatus={sessionId:"a",mode:"browser_use",paused:false,allowPrivateNetwork:false,browser:{available:true},computer:{available:true,platform:"macOS",screenCapture:true,inputControl:true},browserRunning:true,browserHasData:true};
const select=()=>screen.getByRole("combobox",{name:"管理浏览器数据的任务"});
const extensionStatus:AutomationStatus={...status,paused:true,extensionConnected:false,execution:{nativeIsolation:"process",backgroundInput:true,inputMode:"application_targeted_with_foreground_fallback",platform:"macos",fileDialog:true,windowClick:true,browserSource:"extension"}};

describe("设置中的任务浏览器管理",()=>{
  beforeEach(()=>{
    vi.clearAllMocks();Object.defineProperty(window,"__TAURI_INTERNALS__",{configurable:true,value:{}});
    useSessionStore.setState({sessionId:"a",streaming:false,sendNowPending:false,transcripts:{}});
    useSessionsStore.setState({independent:[{sessionId:"a",title:"任务甲",cwd:"/a",status:"completed"},{sessionId:"b",title:"任务乙",cwd:"/b",status:"completed"}]});
    vi.mocked(client.automationStatus).mockImplementation(async id=>({...status,sessionId:id}));
    vi.mocked(client.automationClearBrowserData).mockImplementation(async id=>({...status,sessionId:id,paused:true,browserHasData:false,browserRunning:false}));
    vi.mocked(client.automationSetPrivateNetwork).mockImplementation(async(id,allowed)=>({...status,sessionId:id,allowPrivateNetwork:allowed}));
    vi.mocked(client.automationPairBrowser).mockImplementation(async id=>{
      vi.mocked(client.automationStatus).mockResolvedValue({...extensionStatus,sessionId:id});
      return {endpoint:"ws://127.0.0.1:12345/owned-ticket",extensionPath:"/Applications/EchoAgent.app/Contents/Resources/browser-extension",expiresInSeconds:300};
    });
    vi.mocked(client.automationUseIsolatedBrowser).mockImplementation(async id=>({...status,sessionId:id,paused:true,extensionConnected:false}));
  });
  it("打开设置只读取当前任务，不自动修改权限或清理数据",async()=>{
    render(<BrowserTaskSettings/>);await screen.findByRole("checkbox");
    expect(select()).toHaveValue("a");expect(client.automationClearBrowserData).not.toHaveBeenCalled();expect(client.automationSetPrivateNetwork).not.toHaveBeenCalled();
  });
  it("切换任务后，内网访问只更新所选任务",async()=>{
    render(<BrowserTaskSettings/>);await screen.findByRole("checkbox");fireEvent.change(select(),{target:{value:"b"}});
    fireEvent.click(await screen.findByRole("checkbox"));
    await waitFor(()=>expect(client.automationSetPrivateNetwork).toHaveBeenCalledWith("b",true));
    expect(await screen.findByRole("checkbox")).toBeChecked();
  });
  it("清理前明确确认，可取消；确认后只清理所选任务",async()=>{
    render(<BrowserTaskSettings/>);fireEvent.click(await screen.findByRole("button",{name:"清除浏览器数据"}));
    const dialog=screen.getByRole("alertdialog");expect(dialog).toHaveTextContent("任务甲");expect(client.automationClearBrowserData).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button",{name:"取消"}));expect(client.automationClearBrowserData).not.toHaveBeenCalled();
    fireEvent.change(select(),{target:{value:"b"}});fireEvent.click(await screen.findByRole("button",{name:"清除浏览器数据"}));
    fireEvent.click(screen.getByRole("button",{name:"清除数据"}));
    await waitFor(()=>expect(client.automationClearBrowserData).toHaveBeenCalledWith("b"));
    expect(await screen.findByText("该任务的浏览器数据已清除。")).toBeInTheDocument();expect(screen.getByRole("button",{name:"清除浏览器数据"})).toBeDisabled();
  });
  it("确认后点击运行期间不能重复清理",async()=>{
    let finish!:(value:AutomationStatus)=>void;vi.mocked(client.automationClearBrowserData).mockReturnValueOnce(new Promise(resolve=>{finish=resolve;}));
    render(<BrowserTaskSettings/>);fireEvent.click(await screen.findByRole("button",{name:"清除浏览器数据"}));
    const confirm=screen.getByRole("button",{name:"清除数据"});fireEvent.click(confirm);fireEvent.click(confirm);
    expect(client.automationClearBrowserData).toHaveBeenCalledTimes(1);expect(select()).toBeDisabled();
    await act(async()=>finish({...status,browserHasData:false}));expect(select()).toBeEnabled();
  });
  it("运行中的任务不能清理浏览器数据",async()=>{
    useSessionStore.setState({streaming:true});render(<BrowserTaskSettings/>);
    expect(await screen.findByRole("button",{name:"清除浏览器数据"})).toBeDisabled();expect(screen.getByText("任务运行中，请先停止任务再清除。")).toBeInTheDocument();
  });
  it("失败保留数据和访问范围，可重试",async()=>{
    vi.mocked(client.automationSetPrivateNetwork).mockRejectedValueOnce(new Error("连接失败"));render(<BrowserTaskSettings/>);
    fireEvent.click(await screen.findByRole("checkbox"));expect(await screen.findByRole("alert")).toHaveTextContent("连接失败");
    expect(screen.getByRole("checkbox")).not.toBeChecked();expect(screen.getByRole("checkbox")).toBeEnabled();
    fireEvent.click(screen.getByRole("checkbox"));await waitFor(()=>expect(screen.getByRole("checkbox")).toBeChecked());
  });
  it("切换任务取消旧清理确认，不能误清另一个任务",async()=>{
    render(<BrowserTaskSettings/>);fireEvent.click(await screen.findByRole("button",{name:"清除浏览器数据"}));
    fireEvent.change(select(),{target:{value:"b"}});await waitFor(()=>expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(client.automationClearBrowserData).not.toHaveBeenCalled();
  });
  it("没有任务时提供选择提示，不发送空会话查询",async()=>{
    useSessionStore.setState({sessionId:null});useSessionsStore.setState({independent:[]});render(<BrowserTaskSettings/>);
    expect(screen.getByText("选择需要管理的任务，其他任务的浏览器数据不受影响。")).toBeInTheDocument();
    expect(client.automationStatus).not.toHaveBeenCalled();
  });
  it("未打开的后台任务仍在运行时也禁用清理",async()=>{
    useSessionStore.setState({sessionId:null});useSessionsStore.setState({independent:[{sessionId:"b",title:"后台任务",cwd:"/b",status:"working"}]});
    render(<BrowserTaskSettings/>);fireEvent.change(select(),{target:{value:"b"}});
    expect(await screen.findByRole("button",{name:"清除浏览器数据"})).toBeDisabled();
  });
  it("任务移除后旧操作的结果不锁住或覆盖新任务设置",async()=>{
    let fail!:(error:Error)=>void;vi.mocked(client.automationClearBrowserData).mockReturnValueOnce(new Promise((_,reject)=>{fail=reject;}));
    render(<BrowserTaskSettings/>);fireEvent.click(await screen.findByRole("button",{name:"清除浏览器数据"}));fireEvent.click(screen.getByRole("button",{name:"清除数据"}));
    await act(async()=>{useSessionStore.setState({sessionId:"b"});useSessionsStore.setState({independent:[{sessionId:"b",title:"任务乙",cwd:"/b",status:"completed"}]});});
    expect(select()).toBeEnabled();fireEvent.change(select(),{target:{value:"b"}});fireEvent.click(await screen.findByRole("checkbox"));
    await screen.findByText("访问范围已更新。");await act(async()=>fail(new Error("旧任务错误")));
    expect(screen.getByRole("checkbox")).toBeChecked();expect(screen.queryByText(/旧任务错误/)).toBeNull();expect(select()).toBeEnabled();
  });

  it.each([true,false])("内网访问的真实确认在设置内可见，批准=%s 后才变更范围",async approved=>{
    let approve!:(status:AutomationStatus)=>void,deny!:(error:Error)=>void;
    vi.mocked(client.automationSetPrivateNetwork).mockImplementationOnce((id)=>{
      const response=new Promise<AutomationStatus>((resolve,reject)=>{approve=resolve;deny=reject;});
      vi.mocked(client.onAutomationApproval).mock.calls[0][0]({requestId:"network",sessionId:id,tool:"automation_set_private_network",title:"允许访问本机和内网",description:"请确认当前任务确实需要此权限。",details:{allowed:true}});
      return response;
    });
    vi.mocked(client.automationResolveApproval).mockImplementationOnce(async(_,accepted)=>{
      if(accepted)approve({...status,allowPrivateNetwork:true});else deny(new Error("未获批准，访问范围未改变"));return true;
    });
    render(<BrowserTaskSettings/>);fireEvent.click(await screen.findByRole("checkbox"));
    const card=await screen.findByRole("region",{name:"操作授权"});expect(card.closest(".browser-task-settings")).not.toBeNull();
    expect(screen.getByRole("checkbox")).not.toBeChecked();expect(select()).toBeDisabled();
    fireEvent.click(screen.getByRole("button",{name:approved?"允许一次":"拒绝"}));
    await waitFor(()=>expect(screen.queryByRole("region",{name:"操作授权"})).toBeNull());
    if(approved)expect(screen.getByRole("checkbox")).toBeChecked();else {expect(screen.getByRole("checkbox")).not.toBeChecked();expect(screen.getByRole("alert")).toHaveTextContent("未获批准");}
    expect(select()).toBeEnabled();
  });

  it("配对前说明网络边界，生成地址后可主动取消并使用独立浏览器",async()=>{
    render(<BrowserTaskSettings/>);
    expect(await screen.findByText(/网页的请求不经过任务独立浏览器的网络隔离/)).toBeVisible();
    fireEvent.click(screen.getByRole("button",{name:"连接已登录浏览器"}));
    expect(await screen.findByRole("textbox",{name:"浏览器连接地址"})).toHaveValue("ws://127.0.0.1:12345/owned-ticket");
    expect(screen.getByRole("checkbox",{name:/允许导航到本机与内网页面/})).not.toBeChecked();
    expect(screen.getByText(/网页自身的请求仍按原浏览器的网络权限执行/)).toBeVisible();
    expect(client.automationUseIsolatedBrowser).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button",{name:"使用独立浏览器"}));
    await waitFor(()=>expect(client.automationUseIsolatedBrowser).toHaveBeenCalledWith("a"));
    expect(await screen.findByText("已切换为任务独立浏览器，可返回对话继续。")).toBeVisible();
    expect(screen.queryByRole("textbox",{name:"浏览器连接地址"})).toBeNull();
    expect(screen.getByRole("checkbox",{name:/允许访问本机与内网/})).toBeVisible();
  });

  it("意外断连不自动换登录环境，并保留重连和主动切换入口",async()=>{
    vi.mocked(client.automationStatus).mockResolvedValue({...extensionStatus,extensionConnected:true});
    render(<BrowserTaskSettings/>);
    await screen.findByText("已连接所选浏览器标签页");
    await act(async()=>vi.mocked(client.onAutomationStatus).mock.calls[0][0](extensionStatus));
    expect(screen.getByText("浏览器尚未连接，可重新连接或使用独立浏览器")).toBeVisible();
    expect(screen.getByRole("button",{name:"重新生成连接地址"})).toBeEnabled();
    expect(screen.getByRole("button",{name:"使用独立浏览器"})).toBeEnabled();
    expect(client.automationUseIsolatedBrowser).not.toHaveBeenCalled();
  });

  it("过期地址不再展示为可用连接，并提供重新生成和取消入口",async()=>{
    vi.mocked(client.automationPairBrowser).mockImplementationOnce(async()=>{
      vi.mocked(client.automationStatus).mockResolvedValue(extensionStatus);
      return {endpoint:"expired-ticket",extensionPath:"/synthetic/extension",expiresInSeconds:0};
    });
    render(<BrowserTaskSettings/>);
    fireEvent.click(await screen.findByRole("button",{name:"连接已登录浏览器"}));
    expect(await screen.findByText("连接地址已过期，请重新生成连接地址，或使用独立浏览器。")).toBeVisible();
    expect(screen.queryByRole("textbox",{name:"浏览器连接地址"})).toBeNull();
    expect(screen.getByRole("button",{name:"重新生成连接地址"})).toBeEnabled();
    expect(screen.getByRole("button",{name:"使用独立浏览器"})).toBeEnabled();
  });

  it("连接成功后移除配对地址，复制地址有明确反馈",async()=>{
    const copy=vi.fn().mockResolvedValue(undefined);
    const original=Object.getOwnPropertyDescriptor(navigator,"clipboard");
    Object.defineProperty(navigator,"clipboard",{configurable:true,value:{writeText:copy}});
    try {
      render(<BrowserTaskSettings/>);
      fireEvent.click(await screen.findByRole("button",{name:"连接已登录浏览器"}));
      fireEvent.click(await screen.findByRole("button",{name:"复制连接地址"}));
      expect(await screen.findByText("连接地址已复制。")).toBeVisible();
      expect(copy).toHaveBeenCalledWith("ws://127.0.0.1:12345/owned-ticket");
      await act(async()=>vi.mocked(client.onAutomationStatus).mock.calls[0][0]({...extensionStatus,extensionConnected:true}));
      expect(screen.queryByRole("textbox",{name:"浏览器连接地址"})).toBeNull();
      expect(screen.getByText("已连接所选浏览器标签页")).toBeVisible();
    } finally {
      if(original)Object.defineProperty(navigator,"clipboard",original);
      else Reflect.deleteProperty(navigator,"clipboard");
    }
  });

  it("切换失败保留当前浏览器与地址，允许重试",async()=>{
    vi.mocked(client.automationUseIsolatedBrowser).mockRejectedValueOnce(new Error("停止尚未确认"));
    render(<BrowserTaskSettings/>);
    fireEvent.click(await screen.findByRole("button",{name:"连接已登录浏览器"}));
    fireEvent.click(await screen.findByRole("button",{name:"使用独立浏览器"}));
    expect(await screen.findByRole("alert")).toHaveTextContent("停止尚未确认");
    expect(screen.getByRole("textbox",{name:"浏览器连接地址"})).toBeVisible();
    expect(screen.getByRole("button",{name:"使用独立浏览器"})).toBeEnabled();
  });

  it("模型空档仍有执行权时不允许更改浏览器来源",async()=>{
    vi.mocked(client.automationStatus).mockResolvedValue({...extensionStatus,task:{phase:"running",authorized:true,reason:null,scope:"桌面任务",generation:1},paused:false});
    render(<BrowserTaskSettings/>);
    expect(await screen.findByRole("button",{name:"重新生成连接地址"})).toBeDisabled();
    expect(screen.getByRole("button",{name:"使用独立浏览器"})).toBeDisabled();
  });

});
