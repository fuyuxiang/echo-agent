import { beforeEach, expect, it } from 'vitest';
import { useSessionStore } from '../session-store';
import type { AcpSessionUpdate } from '@/lib/types';
beforeEach(()=>{localStorage.clear();useSessionStore.setState(useSessionStore.getInitialState());});
const store=()=>useSessionStore.getState();
const apply=(value:unknown)=>store().applyUpdate(value as AcpSessionUpdate);
const answer=(meta:Record<string,unknown>={})=>({__sessionId:'scheduled',sessionUpdate:'agent_message_chunk',content:{type:'text',text:'本次唯一结论'},_meta:{eventId:'answer-1',promptId:'prompt-1',...meta}});
const text=()=>store().transcripts.scheduled.messages.filter(m=>m.role==='assistant').map(m=>m.parts.filter(p=>p.kind==='text').map(p=>p.text).join(''));
it('后台定时任务的真实用户回显没有 promptId 时，后续输出仍须保留',()=>{
 store().setSession('other');
 store().startStreaming('scheduled','prompt-1');
 apply({__sessionId:'scheduled',sessionUpdate:'user_message_chunk',content:{type:'text',text:'执行任务',_meta:{promptIndex:0}},_meta:{eventId:'user-1',agentTimestampMs:100}});
 apply(answer());
 expect(text()).toContain('本次唯一结论');
});
it('已缓存会话打开历史期间，真实流式内容不能被回放屏蔽吞掉',()=>{
 store().setSession('scheduled');store().pushUser('执行任务');store().startStreaming('scheduled','prompt-1');
 store().setSession('other');store().setSession('scheduled');
 apply(answer({isReplay:false}));
 expect(text()).toContain('本次唯一结论');
});
it('同一份历史事件重复送达六次，过程和结论只展示一次',()=>{
 store().setSession('scheduled');
 for(let i=0;i<6;i++){
  apply({__sessionId:'scheduled',sessionUpdate:'user_message_chunk',content:{type:'text',text:'执行任务',_meta:{promptIndex:0}},_meta:{eventId:'user-1',isReplay:true}});
  apply({__sessionId:'scheduled',sessionUpdate:'tool_call',toolCallId:'tool-1',title:'读取资料',status:'completed',_meta:{eventId:'tool-1',promptId:'prompt-1',isReplay:true}});
  apply(answer({isReplay:true}));
  apply({__sessionId:'scheduled',sessionUpdate:'turn_completed',prompt_id:'prompt-1',stop_reason:'end_turn',_meta:{eventId:'complete-1',isReplay:true}});
 }
 expect(text()).toEqual(['本次唯一结论']);
});

it('不同真实轮次的相同文字分别保留，事件乱序也不丢消息',()=>{
 store().setSession('scheduled');
 apply(answer({eventId:'scheduled-20'}));
 apply({...answer({eventId:'scheduled-10'}),content:{type:'text',text:'后到的分片'}});
 apply({__sessionId:'scheduled',sessionUpdate:'turn_completed',prompt_id:'prompt-1',stop_reason:'end_turn',_meta:{eventId:'scheduled-30'}});
 store().pushUser('再执行一次');store().startStreaming('scheduled','prompt-2');
 apply(answer({eventId:'scheduled-40',promptId:'prompt-2'}));
 expect(text()).toEqual(['本次唯一结论后到的分片','本次唯一结论']);
});

it('事件身份随缓存保留，丢弃缓存后可以完整重建',()=>{
 store().setSession('scheduled');apply(answer());
 store().setSession('other');store().setSession('scheduled');store().clearReplaySuppression('scheduled');
 expect(apply(answer({isReplay:true}))).toBe(false);
 expect(text()).toEqual(['本次唯一结论']);
 store().dropSessionCache('scheduled');store().setSession('scheduled');
 apply(answer({isReplay:true}));
 expect(text()).toEqual(['本次唯一结论']);
});

it('暂停屏蔽的实时事件不会阻止后续从历史恢复同一事件',()=>{
 store().setSession('scheduled');store().startStreaming('scheduled','prompt-1');
 store().requestControl('scheduled','pause','prompt-1');store().confirmControl('scheduled','pause');
 expect(apply(answer())).toBe(false);
 apply(answer({isReplay:true}));
 expect(text().join('')).toBe('本次唯一结论');
});

it('引擎退出收尾保留事件身份，迟到回放不会重复已有结论',()=>{
 store().setSession('scheduled');store().startStreaming('scheduled','prompt-1');
 apply(answer());
 store().failAllStreaming('error','引擎暂时退出');
 expect(apply(answer({isReplay:true}))).toBe(false);
 expect(text()).toEqual(['本次唯一结论']);
 expect(store().streaming).toBe(false);
});
