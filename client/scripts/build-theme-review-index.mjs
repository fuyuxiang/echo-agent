import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const root = resolve(process.argv[2] || "src-tauri/target/theme-review");
const metadataPath = join(root, "review-manifest.json");
const metadata = existsSync(metadataPath) ? JSON.parse(readFileSync(metadataPath, "utf8")) : {};
const baselineCaption = metadata.baselineRevision
  ? `修改前 · ${String(metadata.baselineRevision).slice(0, 12)}` : "修改前 · 基线提交未记录";
const escape = value => String(value).replace(/[&<>"']/g, character => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]);
const images = directory => {
  const manifest = join(root, directory, "screenshots.json");
  if (existsSync(manifest)) return JSON.parse(readFileSync(manifest, "utf8")).map(item => item.file).filter(file => existsSync(join(root, directory, file)));
  return existsSync(join(root, directory)) ? readdirSync(join(root, directory)).filter(name => name.endsWith(".png")) : [];
};
const figure = (directory, file, caption) => `<figure><a href="${encodeURI(`${directory}/${file}`)}" target="_blank"><img loading="lazy" src="${encodeURI(`${directory}/${file}`)}" alt="${escape(caption)}"></a><figcaption>${escape(caption)}</figcaption></figure>`;
const sections = [];
for (const [before, after, label] of (metadata.comparisons ?? [["before", "after", "业务页面"], ["before-targeted", "after-targeted", "关键状态"]])) {
  const originals = new Set(images(before));
  for (const file of images(after).sort((a,b) => { const rank = file => file.startsWith("home-") ? 0 : file.startsWith("conversation-") ? 1 : file.startsWith("page-项目-") ? 2 : file.startsWith("organization-") ? 3 : file.startsWith("model-") ? 4 : file.startsWith("coding-") ? 99 : 10; return rank(a)-rank(b) || a.localeCompare(b); })) {
    if (!originals.has(file)) continue;
    const note = file.startsWith("coding-") ? " · 宿主布局夹具，含占位 iframe；真实 IDE 见独立集成截图" : "";
    sections.push(`<article data-search="${escape(`${label} ${file}`.toLowerCase())}"><h2>${escape(label + " · " + file.replace(/\.png$/, "") + note)}</h2><div class="pair">${figure(before, file, baselineCaption)}${figure(after, file, "修改后 · 当前工作区")}</div></article>`);
  }
}
for (const [directory, label] of (metadata.supplemental ?? [["real-theia", "真实 Theia：完整工作台 / 编辑器 / diff / 本地终端"], ["local-editor-paint", "本地 Monaco：实际像素与实时切换"], ["scheduled-states", "定时任务：待确认 / 失败"], ["native-webkit", "原生 WKWebView 表单与键盘焦点"], ["lifecycle", "ThemeProvider 运行中切换与偏好恢复"]])) {
  for (const file of images(directory).filter(file => !file.includes("backdrop"))) sections.push(`<article data-search="${escape(`${label} ${file}`.toLowerCase())}"><h2>${escape(label)} · ${escape(file)}</h2>${figure(directory, file, label)}</article>`);
}
const evidenceLinks = readdirSync(root, { withFileTypes: true }).filter(item => item.isDirectory()).flatMap(item =>
  ["evidence.json", "report.json", "result.json", "native-renderer-report.json", "receipts.json", "native-colors.json"].filter(file => existsSync(join(root, item.name, file)))
    .map(file => `<a href="${encodeURI(`${item.name}/${file}`)}">${escape(`${item.name}/${file}`)}</a>`)).join(" · ");
writeFileSync(join(root, "index.html"), `<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>EchoAgent 主题验收对比</title><style>
*{box-sizing:border-box}body{margin:0;background:#f7f8fa;color:#20242e;font:14px/1.6 system-ui,sans-serif}header{position:sticky;top:0;z-index:1;background:#fffffff5;border-bottom:1px solid #e1e5eb;padding:16px 24px}h1{font-size:21px;margin:0 0 6px}p{margin:6px 0}input{width:min(560px,100%);padding:10px 12px;border:1px solid #7b8798;border-radius:8px;font:inherit}a{color:#4f5bd5}main{padding:20px;max-width:1900px;margin:auto}article{padding:16px;background:#fff;border:1px solid #e1e5eb;border-radius:12px;margin-bottom:20px}h2{font-size:14px;overflow-wrap:anywhere;margin:0 0 10px}.pair{display:grid;grid-template-columns:1fr 1fr;gap:16px}figure{margin:0;min-width:0}img{display:block;width:100%;height:auto;border:1px solid #e1e5eb;border-radius:6px}figcaption{padding-top:6px;color:#5f6877}[hidden]{display:none}details{margin:8px 0}@media(max-width:900px){.pair{grid-template-columns:1fr}header{position:static}}
</style><header><h1>EchoAgent Desktop · 浅深主题验收</h1><p>前后对比使用相同受控数据、窗口与字号；点击截图查看原图。对比页的业务数据与原生 IPC 使用隔离夹具。真实 Theia 使用本地后端与临时文件；WKWebView 使用真实原生渲染器。原生完整旅程以独立 result.json 为准。</p><input id="search" type="search" aria-label="筛选截图" placeholder="搜索页面、状态、light / dark、1024 / 1280、13 / 18"><p><a href="verification.json">最终检查与命令日志清单</a> · <a href="visual-review.json">人工截图复核记录</a></p><details><summary>原始测量证据（before 目录保留基线失败）</summary>${evidenceLinks}</details><p id="count"></p></header><main>${sections.join("\n")}</main><script>const articles=[...document.querySelectorAll('article')],search=document.querySelector('#search');function filter(){let count=0;for(const article of articles){article.hidden=!article.dataset.search.includes(search.value.toLowerCase());if(!article.hidden)count++;}document.querySelector('#count').textContent=count+' 组截图';}search.addEventListener('input',filter);filter();</script></html>`);
console.log(JSON.stringify({ index: join(root, "index.html"), groups: sections.length }));
