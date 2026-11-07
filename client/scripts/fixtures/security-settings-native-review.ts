import type { PermissionRule } from '../../src/lib/types';
import type { AutomationCapabilities } from '../../src/lib/automation-client';

/** UI-only responses. Browser isolation and approval still use the common review fixture. */
export function securitySettingsNativeReview(query: URLSearchParams) {
  if (!query.has('securitySettingsReview')) return null;
  const state=query.get('securityState') || 'ready';
  let rules: PermissionRule[]=[{action:'deny',tool:'bash',pattern:'rm *'},{action:'ask',tool:'legacy-tool',pattern:'existing:*'}];
  const calls={reads:0,saves:[] as PermissionRule[][],capabilities:0,requests:0};
  Object.assign(window,{securitySettingsReviewCalls:calls});
  let granted=state!=='needs-permissions',saveAttempts=0;
  const ready: AutomationCapabilities={browser:{available:true,browserName:'Google Chrome'},computer:{available:true,platform:'macOS',screenCapture:true,inputControl:true,desktopAvailable:true}};
  return {
    commands:new Set(['permission_list','permission_save','automation_capabilities','automation_request_computer_permissions']),
    invoke:async(command:string,args:any)=>{
      if(command==='permission_list'){
        if(state==='rules-error' && calls.reads++===0) throw new Error('隔离验证：权限规则暂不可读');
        return structuredClone(rules);
      }
      if(command==='permission_save'){
        calls.saves.push(structuredClone(args.rules));
        await new Promise(resolve=>setTimeout(resolve,120));
        if(state==='save-error' && saveAttempts++===0) throw new Error('隔离验证：配置暂不可写');
        rules=structuredClone(args.rules);return;
      }
      if(command==='automation_request_computer_permissions'){calls.requests++;granted=true;return ready.computer;}
      if(command==='automation_capabilities'){
        if(state==='check-error' && calls.capabilities++===0) throw new Error('隔离验证：操作状态暂不可读');
        if(state==='unsupported') return {...ready,browser:{available:false,reason:'未找到可用浏览器'},computer:{...ready.computer,available:false,reason:'当前环境不支持电脑操作'}};
        if(state==='desktop-unavailable') return {...ready,computer:{...ready.computer,desktopAvailable:false,reason:'请解锁当前桌面'}};
        if(state==='windows') return {...ready,computer:{...ready.computer,platform:'Windows'}};
        return {...ready,computer:{...ready.computer,screenCapture:granted,inputControl:granted}};
      }
    },
  };
}
