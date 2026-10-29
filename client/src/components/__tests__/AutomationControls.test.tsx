import { StrictMode } from "react";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AutomationApprovalCard } from "../AutomationControls";
import { useSessionAutomation } from "@/lib/use-session-automation";
import type { AutomationStatus } from "@/lib/automation-client";
import * as client from "@/lib/automation-client";

vi.mock("@/lib/automation-client", () => ({
  automationStatus: vi.fn(), automationPendingApprovals: vi.fn(),
  automationResolveApproval: vi.fn(async () => true),
  onAutomationStatus: vi.fn(async () => () => {}),
  onAutomationApproval: vi.fn(async () => () => {}),
  onAutomationApprovalClosed: vi.fn(async () => () => {}),
}));
const status: AutomationStatus = {
  sessionId:"s1",mode:"computer_use",paused:false,computerAuthorized:true,allowPrivateNetwork:false,
  computer:{available:true,platform:"macOS",screenCapture:true,inputControl:true},browser:{available:true},browserRunning:false,browserHasData:true,
};
const request = {requestId:"r1",sessionId:"s1",tool:"browser_click",title:"确认发送",description:"核对目标后允许本次操作",details:{}};
function Harness({id="s1",inspect=false,onToast=vi.fn()}:{id?:string;inspect?:boolean;onToast?:(message:string)=>void}) {
  const automation=useSessionAutomation(id);
  return <>{inspect && <><output data-testid="state">{JSON.stringify(automation.status)}</output><output data-testid="error">{automation.error}</output><button onClick={automation.reconnect}>Reconnect</button></>}
    <AutomationApprovalCard automation={automation} onToast={onToast}/></>;
}

describe("统一会话中的自动化请求",()=>{
  beforeEach(()=>{
    vi.clearAllMocks();
    Object.defineProperty(window,"__TAURI_INTERNALS__",{configurable:true,value:{}});
    vi.mocked(client.automationStatus).mockImplementation(async id=>({...status,sessionId:id}));
    vi.mocked(client.automationPendingApprovals).mockResolvedValue([]);
  });
  it.each([
    status,
    {...status,mode:"browser_use",browserRunning:true},
    {...status,paused:true,task:{phase:"paused",reason:"user_paused",scope:"文稿",authorized:false,generation:2}},
    {...status,computer:{...status.computer,screenCapture:false}},
    {...status,computer:{...status.computer,desktopAvailable:false}},
  ] as AutomationStatus[])("状态和能力变化不产生模式专属控件：%j",async value=>{
    vi.mocked(client.automationStatus).mockResolvedValue(value);
    const view=render(<Harness/>);
    await waitFor(()=>expect(client.automationStatus).toHaveBeenCalled());
    await act(async()=>{});
    expect(view.container).toBeEmptyDOMElement();
  });
  it("具体请求复用普通授权卡片，默认聚焦拒绝，详情收起且不泄露查询密钥",async()=>{
    vi.mocked(client.automationPendingApprovals).mockResolvedValue([{...request,details:{url:"https://example.com/send?token=secret-query",text:"secret-input",application:"合成应用",characters:18}}]);
    render(<Harness/>);
    const card=await screen.findByRole("region",{name:"操作授权"});
    expect(card).toHaveClass("perm-inline");
    expect(card).not.toHaveClass("automation-approval");
    expect(screen.getByText("操作详情").closest("details")).not.toHaveAttribute("open");
    await waitFor(()=>expect(screen.getByRole("button",{name:"拒绝"})).toHaveFocus());
    expect(card.textContent).not.toMatch(/secret-query|secret-input/);
    fireEvent.click(screen.getByRole("button",{name:"允许一次"}));
    await waitFor(()=>expect(client.automationResolveApproval).toHaveBeenCalledWith("r1",true));
    await waitFor(()=>expect(screen.queryByRole("region",{name:"操作授权"})).toBeNull());
  });
  it("拒绝请求只处理该次操作",async()=>{
    vi.mocked(client.automationPendingApprovals).mockResolvedValue([request]);render(<Harness/>);
    fireEvent.click(await screen.findByRole("button",{name:"拒绝"}));
    await waitFor(()=>expect(client.automationResolveApproval).toHaveBeenCalledWith("r1",false));
  });
  it("网站、目标和上传文件常显，技术参数可展开，凭据和正文不会泄露", async () => {
    vi.mocked(client.automationPendingApprovals).mockResolvedValue([{...request,tool:"browser_upload",details:{
      url:"https://example.com/upload?token=secret-query#secret-fragment",files:["报告.pdf"],
      target:{name:"提交材料"},text:"secret-input",role:"button",
    }}]);
    render(<Harness/>);
    const card=await screen.findByRole("region",{name:"操作授权"});
    expect(screen.getByText("https://example.com/upload")).toBeVisible();
    expect(screen.getByText("报告.pdf")).toBeVisible();
    expect(screen.getByText("提交材料")).toBeVisible();
    expect(screen.getByText("操作详情").closest("details")).not.toHaveAttribute("open");
    expect(card.textContent).not.toMatch(/secret-query|secret-fragment|secret-input/);
  });
  it("请求失败保留卡片和重试入口，重复点击只提交一次",async()=>{
    vi.mocked(client.automationPendingApprovals).mockResolvedValue([request]);
    let reject!:(error:Error)=>void;
    vi.mocked(client.automationResolveApproval).mockReturnValueOnce(new Promise((_,fail)=>{reject=fail;}));
    render(<Harness/>);const allow=await screen.findByRole("button",{name:"允许一次"});
    fireEvent.click(allow);fireEvent.click(allow);expect(client.automationResolveApproval).toHaveBeenCalledTimes(1);
    await act(async()=>reject(new Error("连接暂不可用")));
    expect(screen.getByRole("alert")).toHaveTextContent("连接暂不可用");
    expect(screen.getByRole("button",{name:"允许一次"})).toBeEnabled();
    fireEvent.click(screen.getByRole("button",{name:"允许一次"}));
    await waitFor(()=>expect(client.automationResolveApproval).toHaveBeenCalledTimes(2));
  });
  it("切换任务立即隐藏旧状态，迟到的查询不能覆盖新状态",async()=>{
    let finish!:(next:AutomationStatus)=>void;
    vi.mocked(client.automationStatus).mockImplementation(id=>id==="s1"?new Promise(resolve=>{finish=resolve;}):Promise.resolve({...status,sessionId:id,paused:true}));
    const view=render(<Harness inspect/>);await waitFor(()=>expect(finish).toBeDefined());
    view.rerender(<Harness id="s2" inspect/>);
    await waitFor(()=>expect(screen.getByTestId("state")).toHaveTextContent('"sessionId":"s2"'));
    await act(async()=>finish(status));expect(screen.getByTestId("state")).toHaveTextContent('"paused":true');
  });
  it("较新事件优先于正在返回的旧查询",async()=>{
    let finish!:(next:AutomationStatus)=>void;vi.mocked(client.automationStatus).mockReturnValueOnce(new Promise(resolve=>{finish=resolve;}));
    render(<Harness inspect/>);await waitFor(()=>expect(finish).toBeDefined());
    const update=vi.mocked(client.onAutomationStatus).mock.calls[0][0];
    await act(async()=>{update({...status,paused:true});finish(status);});
    expect(screen.getByTestId("state")).toHaveTextContent('"paused":true');
  });
  it("已关闭的请求不会被迟到的查询或事件重新打开",async()=>{
    let finish!:(items:typeof request[])=>void;
    vi.mocked(client.automationPendingApprovals).mockReturnValueOnce(new Promise(resolve=>{finish=resolve;}));render(<Harness/>);
    await waitFor(()=>expect(finish).toBeDefined());
    await act(async()=>{
      vi.mocked(client.onAutomationApprovalClosed).mock.calls[0][0]({requestId:"r1",sessionId:"s1"});
      vi.mocked(client.onAutomationApproval).mock.calls[0][0](request);finish([request]);
    });expect(screen.queryByText("确认发送")).toBeNull();
  });
  it("快照返回前收到新请求仍保留较早的请求",async()=>{
    let finish!:(items:typeof request[])=>void;
    vi.mocked(client.automationPendingApprovals).mockReturnValueOnce(new Promise(resolve=>{finish=resolve;}));render(<Harness/>);
    await waitFor(()=>expect(finish).toBeDefined());
    await act(async()=>{vi.mocked(client.onAutomationApproval).mock.calls[0][0]({...request,requestId:"r2",title:"稍后确认"});finish([request]);});
    expect(screen.getByText("确认发送")).toBeInTheDocument();expect(screen.getByText("还有 1 项待审批")).toBeInTheDocument();
  });
  it("离开任务后不显示旧请求的回复错误",async()=>{
    vi.mocked(client.automationPendingApprovals).mockImplementation(async id=>id==="s1"?[request]:[]);
    let fail!:(error:Error)=>void;vi.mocked(client.automationResolveApproval).mockReturnValueOnce(new Promise((_,reject)=>{fail=reject;}));
    const onToast=vi.fn();const view=render(<Harness onToast={onToast}/>);
    fireEvent.click(await screen.findByRole("button",{name:"拒绝"}));view.rerender(<Harness id="s2" onToast={onToast}/>);
    await act(async()=>fail(new Error("旧请求错误")));expect(screen.queryByText(/旧请求错误/)).toBeNull();expect(onToast).not.toHaveBeenCalled();
  });
  it("监听注册失败释放已有监听，不保留半套订阅",async()=>{
    const unlisten=vi.fn();vi.mocked(client.onAutomationStatus).mockResolvedValueOnce(unlisten);
    vi.mocked(client.onAutomationApproval).mockRejectedValueOnce(new Error("监听失败"));const view=render(<Harness inspect/>);
    await waitFor(()=>expect(screen.getByTestId("error")).toHaveTextContent("监听失败"));view.unmount();expect(unlisten).toHaveBeenCalledTimes(1);
  });
  it("严格模式重复初始化清理旧监听，每次只保留一套订阅",async()=>{
    const unlisten=vi.fn();vi.mocked(client.onAutomationStatus).mockResolvedValue(unlisten);
    const view=render(<StrictMode><Harness inspect/></StrictMode>);
    await waitFor(()=>expect(screen.getByTestId("state")).toHaveTextContent('"sessionId":"s1"'));
    expect(client.automationStatus).toHaveBeenCalledTimes(1);expect(unlisten).toHaveBeenCalledTimes(1);
    view.unmount();expect(unlisten).toHaveBeenCalledTimes(2);
  });
  it("状态查询失败时仍呈现已收到的实际授权请求",async()=>{
    vi.mocked(client.automationStatus).mockRejectedValueOnce(new Error("状态读取失败"));
    vi.mocked(client.automationPendingApprovals).mockResolvedValueOnce([request]);render(<Harness inspect/>);
    expect(await screen.findByText("确认发送")).toBeInTheDocument();expect(screen.getByTestId("error")).toHaveTextContent("状态读取失败");
  });
  it("重新连接会恢复整套订阅，而不只是重读一次快照",async()=>{
    vi.mocked(client.onAutomationApproval).mockRejectedValueOnce(new Error("订阅中断"));render(<Harness inspect/>);
    await waitFor(()=>expect(screen.getByTestId("error")).toHaveTextContent("订阅中断"));
    fireEvent.click(screen.getByRole("button",{name:"Reconnect"}));
    await waitFor(()=>expect(screen.getByTestId("state")).toHaveTextContent('"sessionId":"s1"'));
    expect(client.onAutomationApproval).toHaveBeenCalledTimes(2);
    await act(async()=>vi.mocked(client.onAutomationStatus).mock.calls[1][0]({...status,paused:true}));
    expect(screen.getByTestId("state")).toHaveTextContent('"paused":true');
  });

  it("浏览器状态查询未返回时也立即显示已有的操作授权请求",async()=>{
    vi.mocked(client.automationStatus).mockReturnValueOnce(new Promise(()=>{}));
    vi.mocked(client.automationPendingApprovals).mockResolvedValueOnce([request]);render(<Harness/>);
    expect(await screen.findByRole("button",{name:"允许一次"})).toBeEnabled();
  });

});
