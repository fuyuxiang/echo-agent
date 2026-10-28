/** Browser audit of a disposable acceptance workspace, including real HTTP/WS races.
 * Requires the qa-csv-v2 and qa-memory-after-restart session fixtures described in
 * docs/qa/local-acceptance-report.md. Creates three pending tasks; does not run them.
 * ECHO_UI_CREDENTIALS_FILE points to private JSON with admin_token and api_token.
 * Set ECHO_UI_BASE_URL, optional ECHO_UI_OUTPUT, PLAYWRIGHT_MODULE and
 * ECHO_UI_BROWSER_PATH. Never run against a workspace containing personal data.
 */
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE).href : 'playwright');
import {readFile,writeFile,mkdir} from 'node:fs/promises';
const root=process.env.ECHO_UI_OUTPUT || '/tmp/echo-dashboard-acceptance',base=process.env.ECHO_UI_BASE_URL;
if (!base || !process.env.ECHO_UI_CREDENTIALS_FILE) throw new Error('ECHO_UI_BASE_URL and ECHO_UI_CREDENTIALS_FILE are required');
const secrets=JSON.parse(await readFile(process.env.ECHO_UI_CREDENTIALS_FILE,'utf8'));
const browser=await chromium.launch({headless:true,...(process.env.ECHO_UI_BROWSER_PATH ? {executablePath:process.env.ECHO_UI_BROWSER_PATH} : {})});
const context=await browser.newContext({locale:'en-US',viewport:{width:1440,height:1000}});const page=await context.newPage();
let deferTaskEvents=false,networkBlocked=false;const deferredEvents=[],sockets=[];await page.routeWebSocket('**/ws/dashboard',ws=>{if(networkBlocked){ws.close({code:1001,reason:'QA offline'});return;}sockets.push(ws);const server=ws.connectToServer();server.onMessage(message=>{let event;try{event=JSON.parse(String(message));}catch{}if(deferTaskEvents&&event?.type==='task_created')deferredEvents.push(()=>ws.send(message));else ws.send(message);});});
const errors=[];page.on('pageerror',e=>errors.push(String(e)));const checks=[];
await mkdir(root+'/screenshots-after',{recursive:true});
async function login(token){await page.goto(base+'/login');await page.locator('#echo-token').fill(token);await page.getByRole('button',{name:'Log in',exact:true}).click();}
try{
 await login('qa-invalid-token');assert.equal(new URL(page.url()).pathname,'/login');checks.push('invalid token retains login');
 await login(secrets.api_token);await page.waitForURL(url=>url.pathname!='/login');await page.goto(base+'/kanban');await page.getByRole('button',{name:/Create/}).waitFor();assert.equal(await page.getByRole('button',{name:/Create/}).isDisabled(),true);checks.push('ordinary token cannot create tasks');
 await page.getByRole('button',{name:'Sign out'}).click();await page.waitForURL(base+'/login');checks.push('logout routes to login');await page.screenshot({path:root+'/screenshots-after/auth-logout-after-1440.png',fullPage:true});
 await login(secrets.admin_token);await page.waitForURL(url=>url.pathname!='/login');await page.goto(base+'/kanban');assert.equal(await page.getByRole('button',{name:/Create/}).isDisabled(),false);checks.push('admin scope refreshed after token switch');
 // The real task_created WS event arrives before a deliberately delayed HTTP response.
 let release;const gate=new Promise(resolve=>release=resolve);
 await page.route('**/api/v1/tasks',async route=>{if(route.request().method()!=='POST')return route.continue();const response=await route.fetch();await gate;await route.fulfill({response});});
 const title='QA WS-first single card '+Date.now();await page.getByPlaceholder('New task…').fill(title);await page.getByRole('button',{name:/Create/}).click();
 await page.getByText(title,{exact:true}).waitFor();assert.equal(await page.getByText(title,{exact:true}).count(),1);release();await page.waitForLoadState('networkidle');assert.equal(await page.getByText(title,{exact:true}).count(),1);await page.reload();await page.getByText(title,{exact:true}).waitFor();assert.equal(await page.getByText(title,{exact:true}).count(),1);checks.push('WS first then HTTP produces one card before and after reload');await page.unroute('**/api/v1/tasks');
 // Hold real WS events so the HTTP response is authoritative first.
 deferTaskEvents=true;const httpTitle='QA HTTP-first single card '+Date.now();await page.getByPlaceholder('New task…').fill(httpTitle);await page.getByRole('button',{name:/Create/}).click();await page.getByText(httpTitle,{exact:true}).waitFor();assert.equal(await page.getByText(httpTitle,{exact:true}).count(),1);deferTaskEvents=false;deferredEvents.splice(0).forEach(send=>send());await page.waitForLoadState('networkidle');assert.equal(await page.getByText(httpTitle,{exact:true}).count(),1);checks.push('HTTP first then WS also produces one card');
 // Delay A's real history response while B becomes the selected session.
 await page.goto(base+'/sessions');await page.getByRole('button',{name:/cli:qa-csv-v2/}).waitFor();let releaseA;const gateA=new Promise(resolve=>releaseA=resolve);
 await page.route('**/sessions/cli%3Aqa-csv-v2/history?*',async route=>{const response=await route.fetch();await gateA;await route.fulfill({response});});
 await page.getByRole('button',{name:/cli:qa-csv-v2/}).click();await page.getByRole('button',{name:/cli:qa-memory-after-restart/}).click();await page.getByText('Search memory for qa_lantern_code and report the synthetic code.',{exact:true}).waitFor();releaseA();await page.waitForLoadState('networkidle');assert.equal(await page.getByText('Read qa-sales.csv using a file tool.',{exact:false}).count(),0);checks.push('late A history does not appear in selected B');await page.unroute('**/sessions/cli%3Aqa-csv-v2/history?*');
 await page.goto(base+'/kanban');networkBlocked=true;await context.setOffline(true);await Promise.all(sockets.map(ws=>ws.close({code:1001,reason:'QA offline'})));const api=await browser.newContext({extraHTTPHeaders:{Authorization:'Bearer '+secrets.admin_token}});const response=await api.request.post(base+'/api/v1/tasks',{data:{title:'QA offline-created task '+Date.now()}});assert.equal(response.status(),201);const offlineTask=(await response.json()).task;networkBlocked=false;await context.setOffline(false);await page.getByText(offlineTask.title,{exact:true}).waitFor({timeout:20000});checks.push('reconnect reconciles task created while browser offline');await api.close();
 /* Config save/running semantics are exercised sequentially by the repository validator. */
 await page.goto(base+'/config'); const results=[];
 for(const width of [1440,1280,1024]){await page.setViewportSize({width,height:1000});for(const route of ['','sessions','memory','skills','knowledge','channels','cron','kanban','logs','config','analytics']){await page.goto(base+'/'+route);await page.locator('main').waitFor();await page.waitForLoadState('networkidle');const overflow=await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth);assert.equal(overflow,false,route+': root overflow');await page.screenshot({path:root+`/screenshots-after/${route||'overview'}-${width}.png`,fullPage:true});results.push({width,route,overflow});}}
 await page.getByRole('button',{name:'中',exact:true}).click();for(const route of ['','sessions','memory','skills','knowledge','channels','cron','kanban','logs','config','analytics']){await page.goto(base+'/'+route);await page.waitForLoadState('networkidle');await page.screenshot({path:root+`/screenshots-after/${route||'overview'}-zh-1024.png`,fullPage:true});}
 assert.deepEqual(errors,[]);await writeFile(root+'/browser-final.json',JSON.stringify({checks,results,errors},null,2));console.log(JSON.stringify({checks,pages:results.length,chinesePages:11,errors}));
}catch(e){await page.screenshot({path:root+'/screenshots-after/interaction-failure.png',fullPage:true});await writeFile(root+'/browser-interaction-failure.json',JSON.stringify({checks,errors,message:String(e),body:await page.locator('body').innerText()},null,2));throw e;}finally{await browser.close();}
