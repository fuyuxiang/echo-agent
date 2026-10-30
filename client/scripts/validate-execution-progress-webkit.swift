import AppKit
import WebKit

// Render the production ChatView in an owned, nonactivating panel. WebKit can
// defer ResizeObserver and timers in an offscreen window, so layout acceptance
// needs a visible rendering surface. No global input or existing app is used.
final class ProgressReview: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
    let view: WKWebView
    let window: NSWindow
    let base: String
    let output: URL
    init(base: String, output: URL) {
        self.base = base; self.output = output
        let config = WKWebViewConfiguration()
        view = WKWebView(frame: NSRect(x: 0, y: 0, width: 1024, height: 780), configuration: config)
        let screen = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1280, height: 800)
        window = NSPanel(contentRect: NSRect(x: screen.minX + 16, y: screen.maxY - 780, width: 1024, height: 780), styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        super.init()
        config.userContentController.add(self, name: "progressReview")
        window.collectionBehavior = [.moveToActiveSpace, .fullScreenAuxiliary]
        window.contentView = view; view.navigationDelegate = self
    }
    func start() {
        window.orderFrontRegardless()
        view.load(URLRequest(url: URL(string: base + "/__ui-review?surface=execution&state=running&theme=light")!))
    }
    func fail(_ message: String) {
        fputs("Native progress review failed: \(message)\n", stderr)
        window.orderOut(nil); exit(1)
    }
    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let data = message.body as? [String: Any], let action = data["action"] as? String else { return fail("Invalid review result") }
        if action == "fail" { return fail(String(describing: data["error"] ?? "Unknown error")) }
        if action == "progress" { print("Native progress: \(data["stage"] ?? "Unknown stage")"); fflush(stdout); return }
        if action == "snapshot" {
            view.takeSnapshot(with: nil) { image, error in
                guard let image = image, let tiff = image.tiffRepresentation, let bitmap = NSBitmapImageRep(data: tiff), let png = bitmap.representation(using: .png, properties: [:]) else { return self.fail(String(describing: error)) }
                do { try png.write(to: self.output.appendingPathComponent("native-progress.png")) } catch { return self.fail(String(describing: error)) }
                self.view.evaluateJavaScript("window.resolveProgressSnapshot()") { _, error in if let error = error { self.fail(String(describing: error)) } }
            }
        } else if action == "done" {
            do { try JSONSerialization.data(withJSONObject: data, options: [.prettyPrinted, .sortedKeys]).write(to: output.appendingPathComponent("native-results.json")) } catch { return fail(String(describing: error)) }
            print("Native WKWebView progress review passed: \(data)")
            window.orderOut(nil); exit(0)
        }
    }
    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
        print("Native progress: navigation started"); fflush(stdout)
    }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) { fail(error.localizedDescription) }
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) { fail(error.localizedDescription) }
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) { fail("WebKit content process terminated") }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        webView.evaluateJavaScript(#"""
        void (async () => {
          const report = data => window.webkit.messageHandlers.progressReview.postMessage(data);
          try {
            report({action:'progress',stage:'page loaded'});
            const assert = (ok, message) => { if (!ok) throw new Error(message); };
            const wait = async predicate => { const until = Date.now() + 60000; while (!predicate()) { if (Date.now() > until) throw new Error('UI load timeout'); await new Promise(resolve => setTimeout(resolve, 40)); } };
            await wait(() => document.querySelector('.execution-process__header'));
            report({action:'progress',stage:'production UI mounted'});
            const { useSessionStore } = await import('/src/stores/session-store.ts');
            const store = useSessionStore.getState(), id = store.streamingMessageId;
            const tool = (id, status) => ({kind:'tool_call',toolCall:{toolCallId:id,kind:'read_file',title:'检查健康资料与参考文献 · ' + id,status,content:[]}});
            const parts = [{kind:'thought',text:'较早的推理内容。'.repeat(100)+'最新决定：改用备用方案验证。'},tool('ongoing','in_progress'),tool('old','completed'),tool('recent','completed'),tool('latest','completed')];
            const set = (parts, complete = false) => store.setMessages([{id,role:'assistant',complete,startedAt:Date.now()-76000,parts}]);
            const latestVisible = () => {
              const excerpt = document.querySelector('.execution-process__excerpt p'), start = excerpt?.textContent.indexOf('改用备用方案验证。') ?? -1;
              if (start < 0) return false;
              const range = document.createRange(); range.setStart(excerpt.firstChild,start); range.setEnd(excerpt.firstChild,start+'改用备用方案验证。'.length);
              const tail = range.getBoundingClientRect(), box = excerpt.getBoundingClientRect();
              return tail.top >= box.top - 1 && tail.bottom <= box.bottom + 1 && box.height <= parseFloat(getComputedStyle(excerpt).lineHeight) * 2 + 1;
            };
            document.documentElement.style.fontSize = (16 * 18 / 13) + 'px';
            set(parts);
            await wait(() => document.querySelector('.execution-process__preview .toolcall--run'));
            const preview = document.querySelector('.execution-process__preview'), box = preview.getBoundingClientRect();
            assert(preview.querySelectorAll('.toolcall').length === 3, 'Preview must show at most three real tool calls');
            assert(preview.querySelector('.toolcall--run').textContent.includes('ongoing'), 'Earlier running work must stay visible');
            assert(preview.scrollWidth <= preview.clientWidth + 1 && box.x >= 0 && box.right <= innerWidth + 1, 'Preview overflows the native window');
            await wait(latestVisible);
            report({action:'progress',stage:'latest excerpt visible'});
            document.documentElement.style.fontSize = '16px';
            await wait(latestVisible);
            report({action:'progress',stage:'smaller font excerpt visible'});
            document.documentElement.style.fontSize = (16 * 18 / 13) + 'px';
            await wait(latestVisible);
            report({action:'progress',stage:'larger font excerpt visible'});
            assert(!document.querySelector('.msg__caret, .msg__writing'), 'Execution must not show an answer cursor');
            await new Promise(resolve => { window.resolveProgressSnapshot = resolve; report({action:'snapshot'}); });
            report({action:'progress',stage:'native snapshot saved'});

            const focused = preview.querySelector('[data-tool-call-id="ongoing"]'); focused.focus();
            const completedParts = parts.map(part => part.kind === 'tool_call' ? {...part,toolCall:{...part.toolCall,status:'completed'}} : part);
            set(completedParts);
            await wait(() => focused.textContent.includes('完成'));
            assert(document.activeElement === focused && focused.isConnected, 'Native tool completion must retain keyboard focus');
            assert(preview.querySelectorAll('.toolcall').length === 3, 'Focus retention must keep the preview bounded');
            document.querySelector('.execution-process__header').focus();
            await wait(() => !focused.isConnected);
            set([{kind:'thought',text:'早先进展。\n\n```ts\n'+'const draft = 1;\n'.repeat(300)+'```\n\n代码核对完成。'}]);
            await wait(() => document.querySelector('.execution-process__excerpt p')?.textContent === '代码核对完成。');
            report({action:'progress',stage:'focus retention and long fences passed'});
            set(parts);
            await wait(() => document.querySelector('.execution-process__preview .toolcall--run'));
            preview.querySelector('.execution-process__preview-more').click();
            await wait(() => document.querySelector('.execution-process__body .toolcall'));
            assert(document.querySelectorAll('.execution-process__body .toolcall').length === 4, 'Expand all must reveal the full history');
            assert(document.activeElement === document.querySelector('.execution-process__header'), 'Expand all must keep a stable focus target');
            document.querySelector('.execution-process__header').click();
            await wait(() => document.querySelector('.execution-process__preview .toolcall--run'));
            document.querySelector('.execution-process__preview .toolcall--run').focus();
            set([...completedParts,{kind:'text',text:'这是正式答复。'}]);
            await wait(() => document.querySelector('.execution-process__title')?.textContent === '正在生成回复');
            assert(!document.querySelector('.execution-process__preview'), 'Answer generation must retire the live preview');
            assert(document.activeElement === document.querySelector('.execution-process__header'), 'Answer transition must restore keyboard focus');
            report({action:'progress',stage:'answer transition passed'});
            assert(document.querySelectorAll('.msg--assistant [role="status"]').length === 1, 'Answer generation must have one live announcement');
            set([...completedParts,{kind:'text',text:Array.from({length:30},(_,i)=>'第 '+(i+1)+' 段：这是持续生成的长回复，底部应始终保留可见的生成状态。').join('\n\n')}]);
            await wait(() => {
              const status = document.querySelector('.msg__writing')?.getBoundingClientRect(), viewport = document.querySelector('.chatview__scroll')?.getBoundingClientRect(), header = document.querySelector('.execution-process__header')?.getBoundingClientRect();
              return status && viewport && header && status.top >= viewport.top && status.bottom <= viewport.bottom + 1 && header.bottom < viewport.top;
            });
            report({action:'progress',stage:'long-answer feedback visible'});
            set(parts);
            await wait(() => document.querySelector('.execution-process__preview .toolcall--run'));
            document.querySelector('.execution-process__preview .toolcall--run').focus();
            set(completedParts,true);
            await wait(() => document.querySelector('.execution-process__title')?.textContent === '查看过程');
            assert(!document.querySelector('.execution-process--running'), 'Completed response must not remain busy');
            assert(!document.querySelector('.msg__writing'), 'Completion must remove generation feedback');
            assert(document.activeElement === document.querySelector('.execution-process__header'), 'Completion must restore keyboard focus');
            report({action:'done',viewport:[innerWidth,innerHeight],previewHeight:box.height,checks:['native layout','latest two lines','font changes','long code fences','parallel activity','keyboard focus','expand all','long-answer feedback','completion']});
          } catch (error) { report({action:'fail',error:String(error)+'\n'+error.stack}); }
        })();
        """#) { _, error in if let error = error { self.fail(String(describing: error)) } }
    }
}

guard CommandLine.arguments.count == 3 else { fatalError("Expected base URL and output directory") }
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let activity = ProcessInfo.processInfo.beginActivity(options: [.userInitiatedAllowingIdleSystemSleep], reason: "Validate native execution progress rendering")
let review = ProgressReview(base: CommandLine.arguments[1], output: URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true))
Timer.scheduledTimer(withTimeInterval: 120, repeats: false) { _ in review.fail("Native review timed out") }
review.start()
app.run()
