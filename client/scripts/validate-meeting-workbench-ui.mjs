import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output=reviewOutput(mkdtempSync(join(tmpdir(),"echo-meeting-workbench-")));
const server=await createUiReviewServer(0,{fixture:"/scripts/fixtures/meeting-workbench-review.tsx"});
const base=`http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
const browser=await(await reviewChromium()).launch({headless:true,executablePath:process.env.CHROME_BIN||"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"});
let layouts=0;
try {
  for(const width of [1440,1024,720,360]) for(const theme of ["light","dark"]) for(const font of [13,18]) {
    const page=await browser.newPage({viewport:{width,height:800}}), errors=[];
    page.on("pageerror",error=>errors.push(error.message));
    await page.goto(`${base}?theme=${theme}&font=${font}&platform=${theme==="dark"?"macos":"windows"}&collapsed=${width===1024?1:0}`);
    await page.getByRole("heading",{name:"产品评审周会",exact:true}).waitFor();
    await page.waitForFunction(()=>document.querySelector("audio")?.readyState>=1);
    assert.equal(await page.getByRole("combobox",{name:"本次处理模型"}).count(),0);
    assert.equal(await page.getByRole("button",{name:/导入音频|导入录音/}).count(),0);
    assert.equal(await page.getByRole("button",{name:"系统播放器",exact:true}).count(),0);
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
    const playerBox=await page.locator(".meeting-player").boundingBox();assert(playerBox.y+playerBox.height<=801);
    if(width===1440&&theme==="light"&&font===13) await page.screenshot({path:join(output,"workbench-initial.png")});
    await page.getByRole("link",{name:"00:00:08",exact:true}).click();
    assert.equal(await page.getByRole("tab",{name:"转写原文"}).getAttribute("aria-selected"),"true");
    assert.equal(await page.locator("audio").evaluate(audio=>audio.paused),true);
    await page.getByRole("button",{name:"播放 00:00:16 的原文",exact:true}).click();
    await page.getByRole("button",{name:"暂停播放",exact:true}).waitFor();
    assert(await page.locator("audio").evaluate(audio=>audio.currentTime>=16&&audio.currentTime<19));
    await page.getByRole("button",{name:"暂停播放",exact:true}).click();
    await page.getByRole("combobox",{name:"播放速度"}).selectOption("1.5");
    assert.equal(await page.getByText("暂时无法播放，请点击播放按钮重试",{exact:true}).count(),0);
    assert.equal(await page.locator("audio").evaluate(audio=>audio.playbackRate),1.5);
    await page.getByRole("textbox",{name:"查找原文"}).fill("第 3 段");
    assert.equal(await page.locator(".meeting-transcript__passage").count(),1);
    await page.screenshot({path:join(output,`original-${width}-${theme}-${font}.png`)});
    await page.getByRole("tab",{name:"会议纪要"}).click();
    await page.getByRole("button",{name:"编辑纪要",exact:true}).click();
    await page.getByRole("textbox",{name:"编辑会议纪要"}).fill("## 人工校对\n\n这条修改不能丢失。");
    await page.getByRole("button",{name:"返回阅读（保留草稿）",exact:true}).click();
    await page.getByRole("button",{name:"编辑纪要",exact:true}).click();
    assert((await page.getByRole("textbox",{name:"编辑会议纪要"}).inputValue()).includes("不能丢失"));
    await page.getByRole("button",{name:"保存纪要",exact:true}).click();
    await page.getByRole("heading",{name:"人工校对",exact:true}).waitFor();
    await page.screenshot({path:join(output,`minutes-${width}-${theme}-${font}.png`)});
    assert.deepEqual(errors,[]);layouts+=1;await page.close();
  }
  const repair=await browser.newPage({viewport:{width:720,height:800}});
  await repair.goto(`${base}?scenario=recording-retry`);
  await repair.getByText(/本地短句转写暂不可用/).waitFor();
  await repair.evaluate(() => window.__meetingWorkbench.repairSpeech());
  await repair.getByRole('button',{name:'重试转写',exact:true}).click();
  await repair.getByText(/增量转写 \d+/).first().waitFor();
  assert.equal(await repair.locator('.meeting-recording-controls').count(),1,'local retry must keep the active recording');
  assert.equal(await repair.evaluate(()=>window.__meetingWorkbench.calls.filter(call=>call.command==='meeting_capture_stop').length),0);
  assert.equal(await repair.evaluate(()=>window.__meetingWorkbench.calls.filter(call=>call.command==='meeting_set_transcription_connection').length),0);
  await repair.screenshot({path:join(output,'local-inference-retried.png')});
  await repair.getByRole('button',{name:'结束录音',exact:true}).click();
  await repair.getByRole('heading',{name:'会议摘要',exact:true}).waitFor();
  await repair.close();
  for(const scenario of ["empty","recording-error","local-only"]) {
    const page=await browser.newPage({viewport:{width:1024,height:800}});
    await page.goto(`${base}?scenario=${scenario}`);
    if(scenario==="empty" || scenario==="local-only") {
      const both=page.getByRole("button",{name:/两者都录/});await both.waitFor();await both.click();
      const automatic=page.getByRole("checkbox",{name:"录音结束后自动生成会议纪要"});
      assert.equal(await automatic.isChecked(),false);
      if(scenario==="empty") await automatic.check();
      await page.getByRole("button",{name:"开始录音",exact:true}).click();
    }
    if(scenario==="recording-error") {
      await page.getByText(/短句转写暂不可用/).waitFor();
      await page.getByRole("button", { name: "重试转写", exact: true }).click();
    }
    await page.getByText(/增量转写 \d+/).first().waitFor();
    await page.getByRole("button",{name:"暂停录音",exact:true}).click();
    await page.getByRole("button",{name:"继续录音",exact:true}).click();
    await page.screenshot({path:join(output,`${scenario}-live.png`)});
    await page.getByRole("button",{name:"结束录音",exact:true}).click();
    if(scenario==="local-only") {
      await page.getByText(/转写已完成/).waitFor();
      await page.getByRole("tab",{name:"会议纪要"}).click();
      const calls=await page.evaluate(()=>window.__meetingWorkbench.calls.filter(call=>call.command==="meeting_process"||call.command==="meeting_regenerate_minutes"));
      assert.deepEqual(calls.map(call=>[call.command,Boolean(call.args.autoMinutes)]),[["meeting_process",false]]);
      page.once("dialog",async dialog=>{assert.match(dialog.message(),/review\.example/);await dialog.accept();});
      await page.getByRole("button",{name:"生成会议纪要",exact:true}).click();
      await page.getByRole("heading",{name:"更新后的纪要",exact:true}).waitFor();
    } else {
      await page.getByRole("heading",{name:"会议摘要",exact:true}).waitFor();
      assert.equal(await page.getByRole("tab",{name:"会议纪要"}).getAttribute("aria-selected"),"true");
    }
    assert.equal(await page.locator(".meeting-recording-controls").count(),0);
    assert.equal(await page.evaluate(()=>window.__meetingWorkbench.calls.filter(call=>call.command==="meeting_process").length),1);
    await page.close();
  }
  const page=await browser.newPage({viewport:{width:720,height:680}});
  await page.goto(`${base}?scenario=audio-error`);
  await page.getByRole("button",{name:"系统播放器",exact:true}).click();
  assert.equal(await page.evaluate(()=>window.__meetingWorkbench.calls.filter(call=>call.command==="meeting_open_audio").length),1);
  await page.close();
  const save=await browser.newPage({viewport:{width:1024,height:800}});
  await save.goto(`${base}?scenario=save-error`);
  await save.getByRole("button",{name:"新建录音",exact:true}).click();
  await save.getByRole("button",{name:/两者都录/}).click();
  await save.getByRole("checkbox",{name:"录音结束后自动生成会议纪要"}).check();
  await save.getByRole("button",{name:"开始录音",exact:true}).click();
  await save.getByText(/增量转写 \d+/).first().waitFor();
  await save.getByRole("button",{name:"结束录音",exact:true}).click();
  await save.getByRole("button",{name:"重试保存",exact:true}).click();
  await save.getByRole("heading",{name:"会议摘要",exact:true}).waitFor();
  assert.equal(await save.evaluate(()=>window.__meetingWorkbench.calls.filter(call=>call.command==="meeting_capture_stop").length),2);
  await save.close();
  const conflict=await browser.newPage({viewport:{width:1024,height:800}});
  await conflict.goto(base);await conflict.getByRole("tab",{name:"转写原文"}).click();
  await conflict.getByRole("button",{name:"校对原文",exact:true}).click();
  await conflict.getByRole("textbox",{name:"第 1 段转写",exact:true}).fill("尚未保存的人工校对");
  await conflict.evaluate(()=>window.__meetingWorkbench.replaceTranscript());
  await conflict.getByRole("button",{name:"保存校对",exact:true}).click();
  await conflict.getByText(/最新转写与原校对草稿有冲突/).waitFor();
  assert.equal(await conflict.getByRole("textbox",{name:"第 1 段转写",exact:true}).inputValue(),"尚未保存的人工校对");
  await conflict.getByRole("button",{name:"舍弃校对，使用最新转写",exact:true}).click();
  assert.equal(await conflict.getByRole("textbox",{name:"第 1 段转写",exact:true}).inputValue(),"服务端更正后的原文");
  await conflict.close();
  const long=await browser.newPage({viewport:{width:1024,height:800}});
  await long.goto(`${base}?scenario=long`);await long.getByRole("tab",{name:"转写原文"}).click();
  assert.equal(await long.locator(".meeting-transcript__passage").count(),1800);
  await long.getByRole("textbox",{name:"查找原文"}).fill("第 1800 段");
  assert.equal(await long.locator(".meeting-transcript__passage").count(),1);
  await long.close();
  console.log(`Passed ${layouts} meeting layouts, actual WAV playback/seeking/speed, draft retention, incremental updates, local transcription retry, automatic minutes, 1800 passages and fallback. Evidence: ${output}`);
} finally {await browser.close();await server.close();}
