import AppKit
import WebKit

// Owns a separate window and synthetic draft; never opens the user's tasks.
final class AutomationModelReview: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
    let view: WKWebView
    let window: NSWindow
    let base: String
    let output: URL
    var serial = 0
    init(base: String, output: URL) {
        self.base = base; self.output = output
        let config = WKWebViewConfiguration()
        view = WKWebView(frame: NSRect(x: 0, y: 0, width: 1024, height: 900), configuration: config)
        let screen = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1280, height: 900)
        window = NSPanel(contentRect: NSRect(x: screen.minX + 16, y: screen.maxY - 900, width: 1024, height: 900), styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        super.init()
        config.userContentController.add(self, name: "automationModelReview")
        window.contentView = view
        view.navigationDelegate = self
    }
    func start() {
        window.orderFrontRegardless()
        view.load(URLRequest(url: URL(string: base + "/__ui-review?surface=automation-edit&modelChoices=1&theme=light")!))
    }
    func finish(_ data: [String: Any], success: Bool) {
        do { try JSONSerialization.data(withJSONObject: data, options: [.prettyPrinted, .sortedKeys]).write(to: output.appendingPathComponent("native-results.json")) }
        catch { fputs("Unable to save review evidence: \(error)\n", stderr) }
        print("Native automation model review \(success ? "passed" : "failed"): \(data["checks"] ?? data["error"] ?? "No result")")
        window.orderOut(nil); exit(success ? 0 : 1)
    }
    func mouse(_ type: NSEvent.EventType, x: Double, y: Double) {
        let point = NSPoint(x: x, y: view.isFlipped ? y : view.bounds.height - y)
        serial += 1
        guard let event = NSEvent.mouseEvent(with: type, location: view.convert(point, to: nil), modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window.windowNumber, context: nil, eventNumber: serial, clickCount: 1, pressure: type == .leftMouseUp ? 0 : 1) else { return finish(["error": "Cannot create native mouse event"], success: false) }
        NSApp.postEvent(event, atStart: false)
    }
    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let data = message.body as? [String: Any], let action = data["action"] as? String else { return finish(["error": "Invalid review result"], success: false) }
        if action == "click", let x = data["x"] as? Double, let y = data["y"] as? Double, let token = data["token"] as? Int {
            guard x.isFinite, y.isFinite, x >= 0, y >= 0, x < view.bounds.width, y < view.bounds.height else { return finish(["error": "Click outside owned WebView"], success: false) }
            mouse(.leftMouseDown, x: x, y: y)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
                self.mouse(.leftMouseUp, x: x, y: y)
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                    self.view.evaluateJavaScript("window.automationModelResolvers[\(token)]?.()")
                }
            }
        } else if action == "done" || action == "fail" {
            finish(data, success: action == "done")
        }
    }
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) { finish(["error": error.localizedDescription], success: false) }
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) { finish(["error": "WebKit content process terminated"], success: false) }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        webView.evaluateJavaScript(#"""
        void (async () => {
          const report = data => window.webkit.messageHandlers.automationModelReview.postMessage(data);
          const events = [];
          const describe = element => element ? `${element.tagName}.${element.className || ''}:${element.textContent?.trim().slice(0, 60)}` : null;
          for (const type of ['mousedown','blur','focusout','mouseup','click']) document.addEventListener(type, event => {
            if (events.length < 80) events.push({type,target:describe(event.target),related:describe(event.relatedTarget)});
          }, true);
          const assert = (ok, message) => { if (!ok) throw new Error(message); };
          const wait = async predicate => { const until = Date.now() + 60000; while (!predicate()) { if (Date.now() > until) throw new Error('UI load timeout'); await new Promise(resolve => setTimeout(resolve, 40)); } };
          window.automationModelResolvers = {};
          let token = 0;
          const click = element => new Promise(resolve => {
            assert(element, 'Missing click target');
            const rect = element.getBoundingClientRect(), id = ++token;
            window.automationModelResolvers[id] = () => { delete window.automationModelResolvers[id]; resolve(); };
            report({action:'click',token:id,x:rect.x+rect.width/2,y:rect.y+rect.height/2});
          });
          const trigger = () => document.querySelector('.atm-prompt-chip');
          const options = () => [...document.querySelectorAll('.atm-chip-dropdown__menu .atm-chip-option')];
          try {
            await wait(trigger);
            await click(trigger());
            await wait(() => options().length === 4);
            await click(options().find(option => option.textContent.includes('Echo Brook')));
            assert(trigger().textContent.includes('Echo Brook'), 'Native pointer selection did not update the model');
            assert(!document.querySelector('.atm-chip-dropdown__menu'), 'Selection did not close menu');
            await click(trigger());
            await wait(() => options().length === 4);
            assert(options().find(option => option.classList.contains('active'))?.textContent.includes('Echo Brook'), 'Selected model checkmark was lost');
            await click(options().find(option => option.textContent.trim().startsWith('Auto')));
            assert(trigger().textContent.includes('Auto') && trigger().textContent.includes('Echo Reed'), 'Could not return to Auto');
            report({action:'done',checks:['native pointer selects model','menu closes after selection','selection survives reopening','Auto restores default policy'],events});
          } catch (error) { report({action:'fail',error:String(error),events}); }
        })();
        """#) { _, error in if let error = error { self.finish(["error": error.localizedDescription], success: false) } }
    }
}
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let review = AutomationModelReview(base: CommandLine.arguments[1], output: URL(fileURLWithPath: CommandLine.arguments[2]))
review.start()
DispatchQueue.main.asyncAfter(deadline: .now() + 120) { review.finish(["error": "Native review timed out"], success: false) }
app.run()
