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
            const tool = (id, status) => ({kind:'tool_call',streamId:'stage-1',stageTitle:'核对资料与准备演示结构',toolCall:{toolCallId:id,kind:'read_file',title:'检查健康资料 · ' + id,status,content:[]}});
            const parts = [
              {kind:'text',phase:'commentary',streamId:'stage-1',stageTitle:'核对资料与准备演示结构',text:'资料来源已经确认。正在交叉核对数据来源与演示结构。'},
              {kind:'thought',streamId:'stage-1',stageTitle:'核对资料与准备演示结构',text:'详细分析。'.repeat(200)+'分析原文末尾'},
              tool('ongoing','in_progress'),tool('old','completed'),tool('recent','completed'),tool('latest','completed')
            ];
            const set = (parts, complete = false, idle = false) => store.setMessages([{id,role:'assistant',complete,
              startedAt:Date.now()-76000,lastActivityAt:Date.now()-(idle?45000:0),stopReason:complete?'end_turn':undefined,parts}]);
            document.documentElement.style.fontSize = (16 * 18 / 13) + 'px';
            set(parts);
            await wait(() => document.querySelector('.execution-process__title')?.textContent === '核对资料与准备演示结构');
            let entry = document.querySelector('.execution-process__header');
            assert(entry.getAttribute('aria-expanded') === 'false', 'Records must default to collapsed');
            assert(document.querySelectorAll('.execution-process button').length === 1, 'Only one record entry');
            const process = document.querySelector('.execution-process'), box = process.getBoundingClientRect();
            assert(process.scrollWidth <= process.clientWidth + 1 && box.x >= 0 && box.right <= innerWidth + 1, 'Progress overflows native window');
            assert(getComputedStyle(document.querySelector('.execution-process__excerpt p')).whiteSpace === 'normal', 'Progress must wrap');
            entry.click();
            await wait(() => document.querySelectorAll('.execution-process .toolcall').length === 4);
            const body = document.querySelector('.execution-process__body');
            assert(getComputedStyle(body).overflowY === 'visible', 'Records must use transcript scrolling');
            const focused = body.querySelector('[data-tool-call-id="ongoing"]');
            focused.focus();
            const completed = parts.map(part => part.kind === 'tool_call' ? {...part,toolCall:{...part.toolCall,status:'completed'}} : part);
            set(completed);
            await wait(() => focused.textContent.includes('完成'));
            assert(document.activeElement === focused, 'Updating tools must retain keyboard focus');
            body.querySelector('summary').click();
            await wait(() => document.querySelector('.execution-process__read-more'));
            document.querySelector('.execution-process__read-more').click();
            await wait(() => document.querySelector('.execution-process__reasoning-body')?.textContent.includes('分析原文末尾'));
            document.querySelector('.execution-process__read-more').click();
            await wait(() => !document.querySelector('.execution-process__reasoning-body')?.textContent.includes('分析原文末尾'));
            entry.click();
            set(parts,false,true);
            await wait(() => document.querySelector('.execution-process__waiting-note'));
            set(parts);
            await wait(() => !document.querySelector('.execution-process__waiting-note'));
            await new Promise(resolve => { window.resolveProgressSnapshot = resolve; report({action:'snapshot'}); });
            const answering = [...completed,{kind:'text',phase:'final_answer',streamId:'answer-1',text:'这是已经核对好的最终答复。'}];
            set(answering);
            await wait(() => document.querySelector('.msg__writing'));
            assert(!document.querySelector('.execution-process__excerpt'), 'Answer must retire progress excerpt');
            assert(document.querySelectorAll('.msg--assistant [role=status]').length === 1, 'Announce generation once');
            set(answering,true);
            await wait(() => document.querySelector('.execution-process__title')?.textContent === '本轮已完成');
            assert(!document.querySelector('.execution-process--running, .msg__writing'), 'Completion must remove running feedback');
            report({action:'done',viewport:[innerWidth,innerHeight],checks:['native layout','wrapping progress','single record entry','all operations','keyboard focus','long analysis','actual wait','answer feedback','completion']});
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
