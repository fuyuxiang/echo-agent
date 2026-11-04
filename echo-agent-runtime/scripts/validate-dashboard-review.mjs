/** Browser checks for validate-dashboard-review.py's disposable source Runtime.
 * Uses actual HTTP, Dashboard WS and compiled page chunks. No production data.
 */
import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE).href : 'playwright');
const base=process.env.ECHO_UI_BASE_URL,admin=process.env.ECHO_UI_TOKEN,reader=process.env.ECHO_UI_READER_TOKEN;
if(!base||!admin||!reader)throw new Error('Disposable Gateway credentials and URL are required');
const output=process.env.ECHO_UI_OUTPUT||'/tmp/echo-dashboard-review';await mkdir(output+'/screenshots',{recursive:true});
const browser=await chromium.launch({headless:true,...(process.env.ECHO_UI_BROWSER_PATH?{executablePath:process.env.ECHO_UI_BROWSER_PATH}:{})});
const context=await browser.newContext({locale:'en-US',viewport:{width:1440,height:1000}}),page=await context.newPage();
const errors=[],checks=[],layouts=[];page.on('pageerror',error=>errors.push(String(error)));
let networkBlocked=false,rejectNextSocket=false;const sockets=[];
await page.routeWebSocket('**/ws/dashboard',ws=>{
 if(networkBlocked){ws.close({code:1001,reason:'review offline'});return;}
 if(rejectNextSocket){ws.onMessage(message=>{if(JSON.parse(String(message)).type==='auth'){ws.send(JSON.stringify({type:'auth_error'}));ws.close();}});return;}
 sockets.push(ws);ws.connectToServer();
});
const api=await browser.newContext({extraHTTPHeaders:{Authorization:'Bearer '+admin}});
const prefix=(await (await api.request.get(base+'/meta')).json()).api_prefix;
const index=await (await api.request.get(base+'/')).text(),assets=[];
for(const match of index.matchAll(/<script[^>]+src="([^"]+)"/g)){const response=await api.request.get(base+match[1]);assert.equal(response.status(),200);assets.push({path:match[1],sha256:createHash('sha256').update(await response.body()).digest('hex')});}
const routes=['','sessions','memory','skills','knowledge','channels','cron','kanban','logs','config','analytics'];
async function login(token){await page.goto(base+'/login');await page.locator('#echo-token').fill(token);await page.getByRole('button',{name:'Log in',exact:true}).click();}
try{
 await login('invalid-review-token');await page.getByRole('alert').waitFor();assert.equal(new URL(page.url()).pathname,'/login');checks.push('invalid token stays on login');
 await login(admin);await page.waitForURL(url=>url.pathname!='/login');
 async function remember(marker){const response=await api.request.post(base+prefix+'/message',{data:{platform:'api',user_id:'ui-review',chat_id:'ui-review',session_key:'cli:ui-review-memory',text:marker,wait:true,timeout_seconds:20}});assert.equal(response.status(),200);}
 await remember('UI_MEMORY_OLD_681');await remember('UI_MEMORY_CURRENT_927');
 await page.goto(base+'/memory');await page.getByRole('button',{name:'Semantic',exact:true}).click();
 await page.getByText('UI_MEMORY_CURRENT_927',{exact:true}).waitFor();
 const oldVersion=page.getByText('UI_MEMORY_OLD_681',{exact:true}).locator('..').locator('..');
 await oldVersion.getByText('Replaced by a newer version',{exact:true}).waitFor();
 await oldVersion.getByRole('button',{name:'Delete this memory entry',exact:true}).click();
 await page.getByRole('dialog').getByText(/current version, other episode summaries, and future learning are unaffected/).waitFor();
 await page.getByRole('dialog').getByRole('button',{name:'Delete',exact:true}).click();
 await page.getByText('UI_MEMORY_OLD_681',{exact:true}).waitFor({state:'hidden'});
 assert.equal(await page.getByText('UI_MEMORY_CURRENT_927',{exact:true}).count(),1);
 await page.getByText('UI_MEMORY_CURRENT_927',{exact:true}).locator('..').locator('..').getByRole('button',{name:'Delete this memory entry',exact:true}).click();
 await page.getByRole('dialog').getByText(/Old history cannot automatically save this fact again/).waitFor();
 await page.getByRole('dialog').getByRole('button',{name:'Delete',exact:true}).click();
 await page.getByText('UI_MEMORY_CURRENT_927',{exact:true}).waitFor({state:'hidden'});
 await remember('UI_MEMORY_AUTOMATIC_426');
 const forgotten=await (await api.request.post(base+prefix+'/memory/search',{data:{query:'UI_MEMORY_CURRENT_927',all_scopes:true}})).json();
 assert.deepEqual(forgotten.results,[]);await remember('UI_MEMORY_CURRENT_927');
 const restored=await (await api.request.post(base+prefix+'/memory/search',{data:{query:'UI_MEMORY_CURRENT_927',all_scopes:true}})).json();
 assert(restored.results.some(result=>result.entry.content==='UI_MEMORY_CURRENT_927'));
 checks.push('real memory tools create versions; old-version cleanup preserves the current fact; explicit forget blocks automatic recreation and permits explicit remembering');
 for(const session of ['A','B']){const response=await api.request.post(base+prefix+'/message',{data:{platform:'api',user_id:'ui-review',chat_id:'ui-review',session_key:'cli:ui-review-'+session,text:'SESSION_'+session+'_BODY',wait:true,timeout_seconds:20}});assert.equal(response.status(),200);assert.equal((await response.json()).status,'completed');}
 await page.goto(base+'/sessions');let unblock;const gate=new Promise(resolve=>{unblock=resolve;});
 await page.route('**/sessions/cli%3Aui-review-A/history?*',async route=>{const response=await route.fetch();await gate;await route.fulfill({response});});
 await page.getByRole('button').filter({hasText:'cli:ui-review-A'}).click();
 await page.getByRole('button').filter({hasText:'cli:ui-review-B'}).click();await page.getByText('SESSION_B_BODY',{exact:true}).waitFor();unblock();
 await page.waitForLoadState('networkidle');assert.equal(await page.getByText('SESSION_A_BODY',{exact:true}).count(),0);await page.unroute('**/sessions/cli%3Aui-review-A/history?*');checks.push('late session A history cannot overwrite selected B');
 await page.getByRole('button').filter({hasText:'cli:ui-review-A'}).click();await page.getByText('SESSION_A_BODY',{exact:true}).waitFor();
 let releaseReset,resetStarted;const resetGate=new Promise(resolve=>{releaseReset=resolve;}),resetBegan=new Promise(resolve=>{resetStarted=resolve;});
 await page.route('**/sessions/cli%3Aui-review-A',async route=>{const response=await route.fetch();resetStarted();await resetGate;await route.fulfill({response});});
 await page.getByRole('button',{name:'Reset session',exact:true}).click();await page.getByRole('dialog').getByRole('button',{name:'Reset session',exact:true}).click();await resetBegan;
 await page.getByRole('button').filter({hasText:'cli:ui-review-B'}).click();await page.getByText('SESSION_B_BODY',{exact:true}).waitFor();releaseReset();await page.waitForLoadState('networkidle');assert.equal(await page.getByText('SESSION_B_BODY',{exact:true}).count(),1);await page.unroute('**/sessions/cli%3Aui-review-A');checks.push('late reset response preserves selected session B');
 await page.goto(base+'/kanban');await page.getByPlaceholder('New task…').fill('UI review task');
 await page.getByRole('button',{name:'Create',exact:true}).click();await page.getByText('UI review task',{exact:true}).waitFor();
 assert.equal(await page.getByText('UI review task',{exact:true}).count(),1);checks.push('HTTP/WS task create produces one card');
 // Cut actual browser transports. Create a task with a separate HTTP client,
 // then assert re-authentication fetches the event missed during the outage.
 networkBlocked=true;await context.setOffline(true);await Promise.all(sockets.map(ws=>ws.close({code:1001,reason:'review offline'})));await page.getByRole('status').filter({hasText:'disconnected'}).waitFor({timeout:10000});
 const created=await api.request.post(base+prefix+'/tasks',{data:{title:'UI task created while offline'}});assert.equal(created.status(),201);
 await page.screenshot({path:output+'/screenshots/offline.png',fullPage:true});networkBlocked=false;await context.setOffline(false);
 await page.getByText('UI task created while offline',{exact:true}).waitFor({timeout:20000});checks.push('visible disconnect and reconnect snapshot repair');
 await page.getByRole('button',{name:'View task details: UI review task',exact:true}).click();await page.getByRole('dialog').waitFor();await page.keyboard.press('Escape');assert.equal(await page.getByRole('dialog').count(),0);checks.push('task drawer opens and Escape closes it');
 const longTitle='UI_LONG_TITLE_'+ 'x'.repeat(180);const longTask=await api.request.post(base+prefix+'/tasks',{data:{title:longTitle,description:'UI_LONG_TEXT_'+ 'description '.repeat(100)}});assert.equal(longTask.status(),201);
 await page.goto(base+'/knowledge');const longName='UI_LONG_PATH_'+ 'x'.repeat(160)+'.md';
 await page.locator('input[type=file]').setInputFiles([{name:longName,mimeType:'text/markdown',buffer:Buffer.from('UI document body')},{name:'rejected.exe',mimeType:'application/octet-stream',buffer:Buffer.from('unsupported')}]);
 await page.getByText(/skipped unsupported files: rejected.exe/).waitFor();await page.getByText(longName,{exact:true}).waitFor();checks.push('multipart upload displays partial acceptance and skipped filename');
 // Switch identities through supported UI and verify both UI and server gates.
 await page.getByRole('button',{name:'Sign out',exact:true}).click();await page.locator('#echo-token').waitFor();
 await page.locator('#echo-token').fill(reader);await page.getByRole('button',{name:'Log in',exact:true}).click();await page.waitForURL(url=>url.pathname!='/login');
 await page.goto(base+'/memory');await page.getByText(/requires an admin token/).waitFor();assert.equal(await page.getByText('SESSION_A_BODY',{exact:true}).count(),0);
 await page.goto(base+'/config');await page.getByText(/This action needs an admin token/).waitFor();
 const denied=await page.request.patch(base+prefix+'/config',{headers:{Authorization:'Bearer '+reader},data:{changes:{'memory.enabled':true}}});assert.equal(denied.status(),403);
 await page.goto(base+'/knowledge');assert.equal(await page.getByRole('button',{name:'Upload document',exact:true}).isDisabled(),true);checks.push('admin to reader switch removes write permissions; server rejects writes');
 await page.getByRole('button',{name:'Sign out',exact:true}).click();await page.locator('#echo-token').fill(admin);await page.getByRole('button',{name:'Log in',exact:true}).click();await page.waitForURL(url=>url.pathname!='/login');
 // Leave the shell socket mounted while the page-specific listener unmounts.
 // Reject its next authentication and verify the supported login return path.
 await page.goto(base+'/memory');await page.waitForLoadState('networkidle');
 await page.locator('a[href="/config"]').click();await page.waitForURL(url=>url.pathname==='/config');
 rejectNextSocket=true;await sockets.at(-1).close({code:1001,reason:'review reauthentication'});
 await page.waitForURL(url=>url.pathname==='/login',{timeout:15000});
 assert.equal(await page.evaluate(()=>localStorage.getItem('echo_token')),null);
 assert.equal(await page.evaluate(()=>sessionStorage.getItem('echo_return_to')),'/config');
 rejectNextSocket=false;await page.locator('#echo-token').fill(admin);await page.getByRole('button',{name:'Log in',exact:true}).click();
 await page.waitForURL(url=>url.pathname==='/config');checks.push('shell socket rejects expired identity after SPA navigation and login returns to the original page');
 for(const width of [1440,1024,768]){await page.setViewportSize({width,height:1000});for(const route of routes){await page.goto(base+'/'+route);await page.locator('main').waitFor();await page.waitForLoadState('networkidle');const dimensions=await page.evaluate(()=>({root:document.documentElement.scrollWidth,viewport:innerWidth,main:document.querySelector('main').clientWidth,mainScroll:document.querySelector('main').scrollWidth}));assert.equal(dimensions.root>dimensions.viewport,false,`${route}: root overflow at ${width}`);assert.equal(dimensions.mainScroll>dimensions.main,false,`${route}: content overflow at ${width}`);assert.equal(await page.getByRole('alert').filter({hasText:'failed to render'}).count(),0);await page.screenshot({path:output+`/screenshots/${route||'overview'}-${width}.png`,fullPage:true});layouts.push({route,width,...dimensions});}}
 await page.getByRole('button',{name:'中',exact:true}).click();for(const route of routes){await page.goto(base+'/'+route);await page.waitForLoadState('networkidle');await page.screenshot({path:output+`/screenshots/${route||'overview'}-zh-768.png`,fullPage:true});}
 checks.push('11 pages at 3 widths; Chinese page sweep');assert.deepEqual(errors,[]);
 await writeFile(output+'/browser-review.json',JSON.stringify({versions:{node:process.version,browser:browser.version()},assets,checks,layouts,errors},null,2));console.log(JSON.stringify({checks,pages:layouts.length,chinesePages:routes.length,errors}));
}catch(error){await page.screenshot({path:output+'/screenshots/failure.png',fullPage:true});await writeFile(output+'/browser-review-failure.json',JSON.stringify({checks,errors,error:String(error),body:await page.locator('body').innerText()},null,2));throw error;}finally{await api.close();await browser.close();}
