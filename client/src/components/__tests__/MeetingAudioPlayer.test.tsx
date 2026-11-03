// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MeetingRecord } from "@/lib/meeting-minutes";
const mocks = vi.hoisted(() => ({ url: vi.fn(), open: vi.fn() }));
vi.mock("@/lib/meeting-minutes", () => ({ meetingAudioUrl: mocks.url, meetingOpenAudio: mocks.open, formatMeetingDuration: (seconds: number) => String(Math.floor(seconds)) }));
import { MeetingAudioPlayer, type MeetingPlayerHandle } from "../MeetingAudioPlayer";
const record: MeetingRecord = { id:"player", title:"录音", createdAt:0,updatedAt:0,status:"completed",modelId:"m",providerId:"p",durationSeconds:60,recordedSamples:960000,audioPath:"unused",transcript:[],chunks:[] };
let paused: WeakMap<HTMLMediaElement, boolean>;
beforeEach(() => {
  vi.clearAllMocks(); paused=new WeakMap();
  mocks.url.mockResolvedValue("meeting-audio://localhost/granted-record/token"); mocks.open.mockResolvedValue(undefined);
  vi.spyOn(HTMLMediaElement.prototype,"pause").mockImplementation(function(this:HTMLMediaElement){paused.set(this,true);this.dispatchEvent(new Event("pause"));});
  vi.spyOn(HTMLMediaElement.prototype,"play").mockImplementation(function(this:HTMLMediaElement){paused.set(this,false);this.dispatchEvent(new Event("play"));return Promise.resolve();});
  vi.spyOn(HTMLMediaElement.prototype,"paused","get").mockImplementation(function(this:HTMLMediaElement){return paused.get(this)??true;});
});
async function metadata(container: HTMLElement) {
  await waitFor(()=>expect(container.querySelector("audio")).toHaveAttribute("src",expect.stringContaining("meeting-audio:")));
  const audio=container.querySelector("audio")!;
  Object.defineProperty(audio,"readyState",{value:1,configurable:true});Object.defineProperty(audio,"duration",{value:60,configurable:true});
  fireEvent.loadedMetadata(audio);return audio;
}
describe("meeting in-app playback",()=>{
  it("does not autoplay on open and keeps playback when only metadata changes",async()=>{
    const onPosition=vi.fn();const view=render(<MeetingAudioPlayer record={record} onPosition={onPosition}/>);
    expect(screen.getByRole("button",{name:"播放录音"})).toBeDisabled();
    const audio=await metadata(view.container);
    expect(HTMLMediaElement.prototype.play).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button",{name:"播放录音"}));
    expect(HTMLMediaElement.prototype.play).toHaveBeenCalledTimes(1);
    audio.currentTime=12;fireEvent.timeUpdate(audio);
    view.rerender(<MeetingAudioPlayer record={{...record,title:"重命名"}} onPosition={onPosition}/>);
    expect(mocks.url).toHaveBeenCalledTimes(1);expect(audio.currentTime).toBe(12);
    view.unmount();expect(audio.paused).toBe(true);
  });
  it("defers a seek until metadata is available, then distinguishes source navigation and playback",async()=>{
    const onPosition=vi.fn();const control={current:null as MeetingPlayerHandle|null};
    const view=render(<MeetingAudioPlayer record={{...record,id:"seek"}} request={{time:8,autoplay:false,sequence:1}} control={control} onPosition={onPosition}/>);
    const audio=await metadata(view.container);
    expect(audio.currentTime).toBe(8);expect(HTMLMediaElement.prototype.play).not.toHaveBeenCalled();
    act(()=>control.current!.seek({time:20,autoplay:true,sequence:2}));
    expect(audio.currentTime).toBe(20);expect(HTMLMediaElement.prototype.play).toHaveBeenCalledTimes(1);
    view.rerender(<MeetingAudioPlayer record={{...record,id:"seek"}} request={{time:20,autoplay:true,sequence:2}} control={control} onPosition={onPosition}/>);
    expect(HTMLMediaElement.prototype.play).toHaveBeenCalledTimes(1);
    fireEvent.change(screen.getByRole("combobox",{name:"播放速度"}),{target:{value:"1.5"}});expect(audio.playbackRate).toBe(1.5);
    fireEvent.change(screen.getByRole("slider",{name:"播放进度"}),{target:{value:"3"}});
    fireEvent.click(screen.getByRole("button",{name:"后退 10 秒"}));expect(audio.currentTime).toBe(0);
  });
  it("offers system playback only after a failure and permits retry",async()=>{
    mocks.url.mockRejectedValueOnce(new Error("missing file"));
    const view=render(<MeetingAudioPlayer record={{...record,id:"failed"}} onPosition={vi.fn()}/>);
    fireEvent.click(await screen.findByRole("button",{name:"系统播放器"}));expect(mocks.open).toHaveBeenCalledWith("failed");
    fireEvent.click(screen.getByRole("button",{name:"重试播放"}));
    await metadata(view.container);
    expect(screen.queryByRole("button",{name:"系统播放器"})).toBeNull();
  });
  it("does not report a deliberate pause as a playback failure",async()=>{
    const onToast=vi.fn();
    vi.mocked(HTMLMediaElement.prototype.play).mockRejectedValueOnce(new DOMException("interrupted","AbortError"));
    const view=render(<MeetingAudioPlayer record={{...record,id:"abort"}} onPosition={vi.fn()} onToast={onToast}/>);
    await metadata(view.container);fireEvent.click(screen.getByRole("button",{name:"播放录音"}));
    await act(async()=>{});expect(onToast).not.toHaveBeenCalled();
  });
});
