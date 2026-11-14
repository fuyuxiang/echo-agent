const list=document.querySelector('#tabs');
for(const tab of await chrome.tabs.query({currentWindow:true})){
 if(!/^https?:/.test(tab.url||''))continue;
 const label=document.createElement('label'),input=document.createElement('input');input.type='checkbox';input.value=String(tab.id);input.checked=!!tab.active;
 label.append(input,document.createTextNode((tab.title||tab.url).slice(0,100)));list.append(label);
}
document.querySelector('#connect').onclick=async()=>{const result=await chrome.runtime.sendMessage({kind:'pair',endpoint:document.querySelector('#endpoint').value,tabs:[...list.querySelectorAll('input:checked')].map(i=>Number(i.value))});document.querySelector('#status').textContent=result.error||'已连接；仅本次任务可控制所选标签页';};
document.querySelector('#disconnect').onclick=async()=>{await chrome.runtime.sendMessage({kind:'disconnect'});document.querySelector('#status').textContent='已断开，控制权已交回';};
