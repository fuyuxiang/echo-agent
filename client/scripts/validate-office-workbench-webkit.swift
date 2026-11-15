import AppKit
import WebKit

// Own only this test window and its isolated fixture. No existing application,
// global input, screen capture or desktop-control session is involved.
final class OfficeReview: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
    let view: WKWebView
    let window: NSWindow
    let url: URL
    let output: URL
    init(url: URL, output: URL) {
        self.url = url; self.output = output
        let config = WKWebViewConfiguration()
        view = WKWebView(frame: NSRect(x: 0, y: 0, width: 1120, height: 850), configuration: config)
        // WKWebView suspends canvas animation frames in offscreen windows.
        // Use our own nonactivating surface to verify PDF painting as well.
        let screen = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1280, height: 900)
        window = NSPanel(contentRect: NSRect(x: screen.minX + 16, y: screen.maxY - 850, width: 1120, height: 850), styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        super.init()
        config.userContentController.add(self, name: "officeReview")
        view.navigationDelegate = self; window.contentView = view
    }
    func start() { window.orderFrontRegardless(); view.load(URLRequest(url: url)) }
    func fail(_ message: String) { fputs("Office WKWebView review failed: \(message)\n", stderr); window.orderOut(nil); exit(1) }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) { fail(error.localizedDescription) }
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) { fail(error.localizedDescription) }
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) { fail("WebKit content process terminated") }
    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let data = message.body as? [String: Any], let action = data["action"] as? String else { return fail("Invalid test result") }
        if action == "fail" { return fail(String(describing: data["error"] ?? "Unknown error")) }
        if action == "snapshot", let name = data["name"] as? String, ["pptx", "docx", "xlsx", "pdf"].contains(name) {
            view.takeSnapshot(with: nil) { image, error in
                guard let tiff = image?.tiffRepresentation, let bitmap = NSBitmapImageRep(data: tiff), let png = bitmap.representation(using: .png, properties: [:]) else { return self.fail(String(describing: error)) }
                do { try png.write(to: self.output.appendingPathComponent("webkit-\(name).png")) } catch { return self.fail(String(describing: error)) }
                self.view.evaluateJavaScript("window.resolveOfficeSnapshot()") { _, error in if let error = error { self.fail(String(describing: error)) } }
            }
        } else if action == "done" {
            do { try JSONSerialization.data(withJSONObject: data, options: [.prettyPrinted, .sortedKeys]).write(to: output.appendingPathComponent("webkit-results.json")) } catch { return fail(String(describing: error)) }
            print("Office WKWebView: PPTX/DOCX/XLSX/PDF original previews passed."); window.orderOut(nil); exit(0)
        }
    }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        view.evaluateJavaScript(#"""
        void (async () => {
          const report = data => webkit.messageHandlers.officeReview.postMessage(data);
          const wait = async (predicate, description) => { const until = Date.now() + 60000; while (!predicate()) { if (Date.now() > until) throw new Error('Timed out: ' + description + '\n' + document.body.innerText); await new Promise(r => setTimeout(r, 40)); } };
          const assert = (ok, message) => { if (!ok) throw new Error(message); };
          const snapshot = name => new Promise(resolve => { window.resolveOfficeSnapshot = resolve; report({action:'snapshot',name}); });
          const rendered = () => document.querySelector('.office-workbench__stage .office-page[data-page-rendered=true]');
          const open = async filename => { const tab = [...document.querySelectorAll('[role=tab]')].find(n=>n.textContent===filename); tab.click(); await wait(()=>document.querySelector('.office-workbench__file strong')?.textContent===filename,'opening '+filename); };
          try {
            await wait(rendered,'PowerPoint original page');
            const image=document.querySelector('.office-workbench__stage img');assert(image.naturalWidth>100,'PPT must paint original SVG');await snapshot('pptx');
            await open('春日计划.docx');await wait(rendered,'Word original page');
            const frame=document.querySelector('.office-workbench__stage iframe');assert(frame.contentDocument.querySelectorAll('table tr').length===4,'Word must keep the original table');
            assert(document.querySelector('.office-workbench__page-count').textContent==='1 / 2','Word must preserve section boundaries');await snapshot('docx');
            await open('经营数据.xlsx');await wait(()=>document.querySelector('[data-cell-address="B3"]'),'Excel cells');
            assert(document.querySelector('[data-cell-address="B3"]').textContent.includes('120,500.50'),'Excel currency format');await snapshot('xlsx');
            await open('春天.pdf');await wait(rendered,'PDF original page');
            const canvas=document.querySelector('.office-workbench__stage canvas');const pixels=canvas.getContext('2d').getImageData(0,0,canvas.width,canvas.height).data;
            let ink=0;for(let i=0;i<pixels.length;i+=16)if(pixels[i]<210||pixels[i+1]<210||pixels[i+2]<210)ink++;assert(ink>300,'PDF must paint real page content');await snapshot('pdf');
            report({action:'done',formats:['pptx','docx','xlsx','pdf'],viewport:[innerWidth,innerHeight]});
          } catch(error) { report({action:'fail',error:String(error)+'\n'+error.stack}); }
        })();
        """#) { _, error in if let error = error { self.fail(String(describing: error)) } }
    }
}
guard CommandLine.arguments.count == 3, let url = URL(string: CommandLine.arguments[1]) else { fatalError("Expected isolated fixture URL and output directory") }
let app = NSApplication.shared; app.setActivationPolicy(.accessory)
let activity = ProcessInfo.processInfo.beginActivity(options: [.userInitiatedAllowingIdleSystemSleep], reason: "Validate document previews in WKWebView")
let review = OfficeReview(url: url, output: URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true))
Timer.scheduledTimer(withTimeInterval: 240, repeats: false) { _ in review.fail("Native preview review timed out") }
review.start(); app.run()
