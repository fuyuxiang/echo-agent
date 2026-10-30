import AppKit
import WebKit

// Mirrors Wry 0.55's enabled native file-drop handler. HTML5 drop callbacks
// are consumed, while ordinary pointer events must still sort the menu.
final class FileDropWebView: WKWebView {
    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation { .copy }
    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation { .copy }
    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool { true }
    override func draggingExited(_ sender: NSDraggingInfo?) {}
}

final class Review: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
    let view: FileDropWebView
    let window: NSWindow
    let output: URL
    let base: String
    var serial = 0
    init(base: String, output: URL) {
        self.base = base; self.output = output
        let config = WKWebViewConfiguration()
        view = FileDropWebView(frame: NSRect(x: 0, y: 0, width: 1280, height: 900), configuration: config)
        window = NSWindow(contentRect: NSRect(x: -4000, y: 0, width: 1280, height: 900), styleMask: .borderless, backing: .buffered, defer: false)
        super.init()
        config.userContentController.add(self, name: "menuReview")
        window.contentView = view
        view.autoresizingMask = [.width, .height]
        view.navigationDelegate = self
    }
    func start() {
        window.orderFrontRegardless()
        window.makeFirstResponder(view)
        view.load(URLRequest(url: URL(string: base + "/__ui-review?surface=home&theme=light")!))
    }
    func fail(_ message: String) {
        fputs("Native menu review failed: \(message)\n", stderr)
        window.orderOut(nil); exit(1)
    }
    func resolve(_ token: Int) {
        view.evaluateJavaScript("window.nativeMenuResolvers[\(token)]?.()") { _, error in
            if let error = error { self.fail(String(describing: error)) }
        }
    }
    func mouse(_ type: NSEvent.EventType, x: Double, y: Double) {
        let point = NSPoint(x: x, y: view.isFlipped ? y : view.bounds.height - y)
        let location = view.convert(point, to: nil)
        serial += 1
        guard let event = NSEvent.mouseEvent(with: type, location: location, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window.windowNumber, context: nil, eventNumber: serial, clickCount: 1, pressure: type == .leftMouseUp ? 0 : 1) else { return fail("Cannot create owned-window mouse event") }
        NSApp.postEvent(event, atStart: false)
    }
    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let data = message.body as? [String: Any], let action = data["action"] as? String else { return fail("Invalid native request") }
        if action == "fail" { return fail(String(describing: data["error"] ?? "Unknown error")) }
        if action == "resize" {
            window.setContentSize(NSSize(width: 1024, height: 680))
            // WebKit throttles animation frames offscreen. Show only our own
            // isolated window briefly for the edge auto-scroll check.
            window.setFrameOrigin(NSPoint(x: 20, y: 20))
            window.orderFrontRegardless()
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { self.resolve(data["token"] as! Int) }
        } else if action == "drag" {
            let from = data["from"] as! [Double], to = data["to"] as! [Double], token = data["token"] as! Int
            guard from.count == 2, to.count == 2,
                [from, to].allSatisfy({ point in point.allSatisfy({ $0.isFinite }) && point[0] >= 0 && point[1] >= 0 && point[0] < view.bounds.width && point[1] < view.bounds.height }) else { return fail("Pointer target must stay inside the owned WebView") }
            let hold = (data["hold"] as? Double ?? 0) / 1000
            mouse(.leftMouseDown, x: from[0], y: from[1])
            for step in 1...20 {
                DispatchQueue.main.asyncAfter(deadline: .now() + Double(step) * 0.02) {
                    let fraction = Double(step) / 20
                    self.mouse(.leftMouseDragged, x: from[0] + (to[0] - from[0]) * fraction, y: from[1] + (to[1] - from[1]) * fraction)
                }
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.45 + hold) {
                self.mouse(.leftMouseUp, x: to[0], y: to[1])
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { self.resolve(token) }
            }
        } else if action == "done" {
            do {
                try JSONSerialization.data(withJSONObject: data, options: [.prettyPrinted, .sortedKeys]).write(to: output.appendingPathComponent("native-results.json"))
            } catch { return fail(String(describing: error)) }
            view.takeSnapshot(with: nil) { image, error in
                guard let image = image, let tiff = image.tiffRepresentation, let bitmap = NSBitmapImageRep(data: tiff), let png = bitmap.representation(using: .png, properties: [:]) else { return self.fail(String(describing: error)) }
                do { try png.write(to: self.output.appendingPathComponent("native-pointer.png")) } catch { return self.fail(String(describing: error)) }
                print("Native WKWebView menu pointer review passed: \(data)")
                self.window.orderOut(nil); exit(0)
            }
        }
    }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        let script = #"""
        void (async () => {
          const report = data => window.webkit.messageHandlers.menuReview.postMessage(data);
          try {
            const assert = (ok, message) => { if (!ok) throw new Error(message); };
            const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
            const wait = async predicate => { const end = Date.now() + 60000; while (!predicate()) { if (Date.now() > end) throw new Error('UI load timeout'); await delay(50); } };
            let token = 0; window.nativeMenuResolvers = {};
            const native = async data => { const current = ++token; const result = new Promise(resolve => window.nativeMenuResolvers[current] = resolve); report({...data, token: current}); await result; };
            await wait(() => document.querySelector('.sidebar__more-wrap > button'));
            document.querySelector('.sidebar__more-wrap > button').click();
            await wait(() => [...document.querySelectorAll('[role=menuitem]')].find(e => e.textContent === '自定义菜单栏'));
            [...document.querySelectorAll('[role=menuitem]')].find(e => e.textContent === '自定义菜单栏').click();
            await wait(() => document.querySelector('.sidebar-menu-customizer'));
            const dialog = () => document.querySelector('.sidebar-menu-customizer');
            const row = id => dialog().querySelector(`[data-menu-id="${id}"]`);
            const group = name => dialog().querySelector(`[data-menu-group="${name}"]`);
            const ids = name => [...group(name).querySelectorAll('[data-menu-id]')].map(e => e.dataset.menuId);
            let trustedDown = 0, trustedMove = 0, htmlDrag = 0, nativePointerActive = false;
            document.addEventListener('pointerdown', e => { if (e.isTrusted && e.target.closest('[data-menu-handle]')) { trustedDown++; nativePointerActive = true; } }, true);
            document.addEventListener('pointermove', e => { if (e.isTrusted && nativePointerActive) trustedMove++; }, true);
            document.addEventListener('pointerup', () => nativePointerActive = false, true); for (const type of ['dragstart', 'drop']) document.addEventListener(type, () => htmlDrag++, true);
            const drag = async (id, target, after = false, hold = 0) => {
              const handle = row(id).querySelector('[data-menu-handle]').getBoundingClientRect();
              const bounds = target.getBoundingClientRect();
              await native({ action: 'drag', from: [handle.x + handle.width / 2, handle.y + handle.height / 2], to: [bounds.x + 24, bounds.y + (after ? bounds.height - 6 : 6)], hold });
            };
            await drag('knowledge', row('projects'));
            assert(ids('primary')[0] === 'knowledge', 'Native pointer failed to move knowledge into primary: ' + ids('primary') + ', trusted downs=' + trustedDown + ', moves=' + trustedMove);
            await drag('knowledge', row('coding'), true);
            assert(ids('primary').join(',') === 'projects,organization,coding,knowledge,capabilities', 'Native same-group reorder failed');
            await drag('organization', row('automation'), true);
            assert(ids('more').join(',') === 'automation,organization', 'Native primary-to-more drag failed');
            const button = text => [...dialog().querySelectorAll('button')].find(e => e.textContent === text);
            button('恢复默认').click(); await delay(50);
            for (const id of ['projects', 'organization', 'coding', 'capabilities']) {
              row(id).querySelector('[data-menu-handle]').dispatchEvent(new KeyboardEvent('keydown', {key: 'ArrowRight', bubbles: true})); await delay(20);
            }
            assert(ids('primary').length === 0, 'Empty-group setup failed');
            await drag('projects', group('primary').querySelector('ul'));
            assert(ids('primary').join(',') === 'projects', 'Native drop into empty primary failed');
            button('恢复默认').click(); await delay(50);
            await native({action: 'resize'});
            document.documentElement.style.fontSize = (16 * 18 / 13) + 'px'; await delay(100);
            const body = dialog().querySelector('.sidebar-menu-customizer__body'); body.scrollTop = 0; let nativeMaxScroll = 0; body.addEventListener('scroll', () => nativeMaxScroll = Math.max(nativeMaxScroll, body.scrollTop));
            const handle = row('projects').querySelector('[data-menu-handle]').getBoundingClientRect(), bounds = body.getBoundingClientRect(); const nativeScrollBefore = {body: {top:bounds.top,bottom:bounds.bottom,height:bounds.height,scroll:body.scrollHeight,client:body.clientHeight}, handle: {x:handle.x,y:handle.y,height:handle.height}, viewport:{width:innerWidth,height:innerHeight}};
            await native({action: 'drag', from: [handle.x + handle.width / 2, handle.y + handle.height / 2], to: [bounds.x + 24, bounds.bottom - 18], hold: 1400});
            assert(nativeMaxScroll > 0, 'Native drag did not auto-scroll clipped modal: ' + JSON.stringify(nativeScrollBefore) + ', downs=' + trustedDown + ', current=' + JSON.stringify(ids('more')));
            assert(ids('more').includes('projects'), 'Native auto-scroll cross-group drop failed: ' + ids('more'));
            button('保存').click(); await delay(50);
            assert(!dialog(), 'Native save did not close modal');
            const saved = JSON.parse(localStorage.getItem('echoagent.sidebar-menu.v1'));
            assert(saved.more.includes('projects'), 'Native drag result was not saved');
            assert(trustedDown === 5 && trustedMove >= 40, 'Drag test must use trusted native mouse input');
            assert(htmlDrag === 0, 'Internal reorder must never enter native HTML5 drag path');
            report({action: 'done', trustedDown, trustedMove, htmlDrag, saved, nativeMaxScroll, checks: ['more-to-primary', 'same-group-sort', 'primary-to-more', 'empty-group', 'small-window-auto-scroll', 'save'], nativeFileDropHandler: true});
          } catch (error) { report({action: 'fail', error: String(error) + '\n' + error.stack}); }
        })();
        """#
        webView.evaluateJavaScript(script) { _, error in if let error = error { self.fail(String(describing: error)) } }
    }
}

guard CommandLine.arguments.count == 3 else { fatalError("Expected base URL and output directory") }
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let activity = ProcessInfo.processInfo.beginActivity(options: [.userInitiatedAllowingIdleSystemSleep], reason: "Verify owned native menu pointer gestures")
let review = Review(base: CommandLine.arguments[1], output: URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true))
Timer.scheduledTimer(withTimeInterval: 120, repeats: false) { _ in review.fail("Native review timed out") }
review.start()
app.run()
