import {describe,it,expect} from 'vitest';
import {meetingTranscriptionIssue} from '../meeting-transcription-error';

describe('meeting transcription recovery guidance',()=>{
  it.each(['当前模型已不存在，请重新选择会议模型','会议模型 API Key 未配置或已失效','组织模型配置已过期，请重新同步','转写服务返回 401 Unauthorized','转写服务返回 404 Not Found','转写服务未完成识别：quota unavailable'])('requires connection repair for %s',reason=>{
    expect(meetingTranscriptionIssue(reason)).toMatchObject({settings:true});
    expect(meetingTranscriptionIssue(reason).message).not.toContain('网络恢复');
  });
  it.each(['连接转写服务失败：timeout','转写服务返回 503 Service Unavailable','network unavailable'])('retries transient service failures for %s',reason=>{
    expect(meetingTranscriptionIssue(reason)).toMatchObject({settings:false});
    expect(meetingTranscriptionIssue(reason).message).toContain('自动重试');
  });
});
