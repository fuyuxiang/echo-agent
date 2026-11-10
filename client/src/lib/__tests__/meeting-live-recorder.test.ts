// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
const mocks=vi.hoisted(()=>({invoke:vi.fn()}));
vi.mock("@tauri-apps/api/core",()=>({invoke:mocks.invoke,convertFileSrc:(path:string)=>path}));
describe("process lifetime meeting transcription",()=>{
  let record:any, active:boolean, fail:boolean, failureReason:string, processResolve:(value:any)=>void;
  beforeEach(()=>{
    vi.resetModules();vi.useFakeTimers();mocks.invoke.mockReset();active=false;fail=false;failureReason='network unavailable';
    record={id:"live-1",title:"录音",modelId:"MiniMax",providerId:"personal",status:"recording",recordedSamples:0,durationSeconds:0,transcript:[],chunks:[],live:{processedSamples:0}};
    mocks.invoke.mockImplementation(async(command:string,args:any)=>{
      if(command==="meeting_create")return {...record};
      if(command==="meeting_capture_start"){active=true;return {meetingId:record.id,active:true,paused:false,level:.2,recordedSamples:0};}
      if(command==="meeting_capture_status")return active?{meetingId:record.id,active:true,paused:false,level:.3,recordedSamples:record.recordedSamples}:null;
      if(command==="meeting_get")return {...record};
      if(command==="meeting_transcribe_live"){
        if(fail)throw new Error(failureReason);
        record={...record,live:{processedSamples:record.recordedSamples},transcript:[{id:0,start:0,end:4,speaker:"S1",text:"增量结果"}]};return {record:{...record},waiting:false};
      }
      if(command==="meeting_capture_stop"){active=false;record={...record,status:"recorded"};return {...record};}
      if(command==="meeting_process")return new Promise(resolve=>{processResolve=resolve;});
      throw new Error("Unexpected command "+command+JSON.stringify(args));
    });
  });
  afterEach(()=>vi.useRealTimers());
  it.each(["system", "both"] as const)("publishes successive native transcription results and saves in %s mode", async (mode) => {
    const {meetingRecorder}=await import("../meeting-minutes");
    await meetingRecorder.start("录音", mode);
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_capture_start", { meetingId: "live-1", mode });
    expect(mocks.invoke.mock.calls.some(([command])=>command==="meeting_check_connection")).toBe(false);
    record={...record,recordedSamples:64000,durationSeconds:4};
    await vi.advanceTimersByTimeAsync(1200);
    expect(meetingRecorder.current().meeting?.transcript[0]?.text).toBe("增量结果");
    record = { ...record, recordedSamples: 128000, durationSeconds: 8 };
    await vi.advanceTimersByTimeAsync(1200);
    expect(meetingRecorder.current()).toMatchObject({
      active: true, paused: false, error: null, meeting: { live: { processedSamples: 128000 } },
    });
    const saved=await meetingRecorder.stop();expect(saved.status).toBe("recorded");expect(meetingRecorder.current().active).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_process",expect.objectContaining({meetingId:"live-1"}));
    expect(meetingRecorder.current().processingIds).toEqual(["live-1"]);
    record={...record,status:"completed",minutes:"自动纪要"};processResolve(record);
    await vi.advanceTimersByTimeAsync(1);
    expect(meetingRecorder.current().meeting?.minutes).toBe("自动纪要");
    expect(meetingRecorder.current().processingIds).toEqual([]);
  });
  it("keeps capture active on ASR errors and retries locally after backoff",async()=>{
    const {meetingRecorder}=await import("../meeting-minutes");
    await meetingRecorder.start("录音","both");record={...record,recordedSamples:64000,durationSeconds:4};fail=true;
    await vi.advanceTimersByTimeAsync(1200);
    expect(meetingRecorder.current()).toMatchObject({active:true,transcription:{state:"unavailable"},error:null});
    const calls=mocks.invoke.mock.calls.filter(([command])=>command==="meeting_transcribe_live").length;
    await vi.advanceTimersByTimeAsync(500);expect(mocks.invoke.mock.calls.filter(([command])=>command==="meeting_transcribe_live")).toHaveLength(calls);
    fail=false;await vi.advanceTimersByTimeAsync(4500);
    expect(meetingRecorder.current().meeting?.live?.processedSamples).toBe(64000);
    await meetingRecorder.prepareForExit();await meetingRecorder.stop();
    expect(mocks.invoke.mock.calls.some(([command])=>command==="meeting_process")).toBe(false);
  });
  it("keeps newly saved capture issues when an older live transcription replies", async () => {
    const { meetingRecorder } = await import("../meeting-minutes");
    const invoke = mocks.invoke.getMockImplementation()!;
    let finishLive!: (reply: unknown) => void;
    const pendingLive = new Promise(resolve => { finishLive = resolve; });
    mocks.invoke.mockImplementation((command, args) => command === "meeting_transcribe_live" ? pendingLive : invoke(command, args));
    await meetingRecorder.start("混合录音", "both");
    const earlier = { ...record, recordedSamples: 16_000, durationSeconds: 1, live: { processedSamples: 16_000 } };
    record = { ...record, recordedSamples: 16_000, durationSeconds: 1 };
    await vi.advanceTimersByTimeAsync(1_200);
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_transcribe_live", { meetingId: "live-1", finalize: false });
    const captureIssues = [{ source: "system", startSample: 16_000, endSample: 17_600, occurrences: 2 }];
    record = { ...record, recordedSamples: 32_000, durationSeconds: 2, captureIssues };
    await vi.advanceTimersByTimeAsync(500);
    finishLive({ record: earlier, waiting: false });
    await vi.advanceTimersByTimeAsync(1);
    expect(meetingRecorder.current()).toMatchObject({
      active: true, error: null, meeting: { captureIssues, live: { processedSamples: 16_000 } },
    });
    await meetingRecorder.prepareForExit();
    expect(await meetingRecorder.stop()).toMatchObject({ status: "recorded", captureIssues });
  });
  it("does not replace a new active recording when the previous recording finishes its minutes",async()=>{
    const {meetingRecorder}=await import("../meeting-minutes");
    await meetingRecorder.start("第一场","both");
    record={...record,recordedSamples:64000,durationSeconds:4};
    await vi.advanceTimersByTimeAsync(1200);await meetingRecorder.stop();await vi.advanceTimersByTimeAsync(1);
    const first={...record,status:"completed",minutes:"第一场纪要"};
    record={...record,id:"live-2",status:"recording",recordedSamples:0,durationSeconds:0,transcript:[],live:{processedSamples:0}};
    await meetingRecorder.start("第二场","both");
    processResolve(first);await vi.advanceTimersByTimeAsync(1);
    expect(meetingRecorder.current()).toMatchObject({active:true,meeting:{id:"live-2",status:"recording"}});
    await meetingRecorder.prepareForExit();await meetingRecorder.stop();
  });
  it("manually retries a local inference failure without stopping capture", async () => {
    const { meetingRecorder } = await import("../meeting-minutes");
    await meetingRecorder.start("本地录音", "both");
    record = { ...record, recordedSamples: 64000, durationSeconds: 4 };
    fail = true;
    failureReason = "本地转写程序意外退出，可重试";
    await vi.advanceTimersByTimeAsync(1200);
    expect(meetingRecorder.current()).toMatchObject({ active: true, transcription: { state: "unavailable", error: failureReason } });
    fail = false;
    await meetingRecorder.retryTranscription();
    expect(meetingRecorder.current()).toMatchObject({
      active: true, meeting: { live: { processedSamples: 64000 } }, transcription: { state: "listening" },
    });
    expect(mocks.invoke.mock.calls.some(([command]) => command === "meeting_capture_stop")).toBe(false);
    await meetingRecorder.prepareForExit();
    await meetingRecorder.stop();
  });
});
