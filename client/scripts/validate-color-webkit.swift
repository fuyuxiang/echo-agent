import AppKit
import WebKit

final class ColorReviewWindow: NSWindow {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}
// Default stays offscreen. --active briefly activates only this owned window
// and restores the prior app if the user has not switched elsewhere.
final class NativeColorReview: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
    let view: WKWebView
    let window: NSWindow
    let base: String
    let output: URL
    var index = 0
    var results: [[String: Any]] = []
    let active = CommandLine.arguments.contains("--active")
    let previousApp = NSWorkspace.shared.frontmostApplication
    let cases = ["theme=light&font=13", "theme=dark&font=13", "theme=light&font=18", "theme=dark&font=18"]
    init(base: String, output: URL) {
        self.base=base; self.output=output
        let config=WKWebViewConfiguration()
        view=WKWebView(frame:NSRect(x:0,y:0,width:1280,height:800),configuration:config)
        let frame=NSScreen.main?.visibleFrame ?? NSRect(x:0,y:0,width:1440,height:900)
        let showActive=CommandLine.arguments.contains("--active")
        window=ColorReviewWindow(contentRect:NSRect(x:showActive ? frame.minX+16 : -4000,y:showActive ? frame.maxY-800 : 0,width:1280,height:800),styleMask:.borderless,backing:.buffered,defer:false)
        super.init()
        config.userContentController.add(self,name:"nativeColor")
        window.contentView=view; view.navigationDelegate=self
        window.ignoresMouseEvents=true
    }
    func close() {
        let owned=NSWorkspace.shared.frontmostApplication?.processIdentifier==ProcessInfo.processInfo.processIdentifier
        window.orderOut(nil)
        if active && owned { previousApp?.activate(options:[]) }
    }
    func fail(_ message: String) -> Never {
        fputs("Native color review failed: \(message)\n",stderr)
        close(); exit(1)
    }
    func start() {
        if active { NSApplication.shared.activate(ignoringOtherApps:true);window.makeKeyAndOrderFront(nil);window.makeFirstResponder(view) }
        else {window.orderFrontRegardless()}
        next()
    }
    func next() {
        if index==cases.count {
            do { try JSONSerialization.data(withJSONObject:results,options:[.prettyPrinted,.sortedKeys]).write(to:output.appendingPathComponent("native-colors.json")) } catch { fail(String(describing:error)) }
            print("Native WKWebView color review passed: \(results.count) form configurations.")
            close(); exit(0)
        }
        let large=index>=2
        window.setContentSize(NSSize(width:large ? 1024:1280,height:large ? 680:800))
        print("Native colors loading \(index+1)/\(cases.count): \(cases[index])"); fflush(stdout)
        view.load(URLRequest(url:URL(string:base+"/__ui-review?"+cases[index]+(active ? "&active=1":""))!))
    }
    func webView(_ webView:WKWebView,didFinish navigation:WKNavigation!) {
        webView.evaluateJavaScript(#"""
        void (async () => {
          const report=data=>window.webkit.messageHandlers.nativeColor.postMessage(data);
          try {
            const wait=async predicate=>{const until=Date.now()+60000;while(!predicate()){if(Date.now()>until)throw Error('Native form load timeout');await new Promise(r=>setTimeout(r,40));}};
            const theme=new URLSearchParams(location.search).get('theme');
            const active=new URLSearchParams(location.search).has('active');
            await wait(()=>document.querySelector('#native-check') && document.documentElement.dataset.theme===theme);
            if(active)await wait(()=>document.hasFocus());
            const {inspectRenderedColors}=await import('/scripts/ui-color-audit.mjs');
            const assert=(ok,message)=>{if(!ok)throw Error(message);};
            const check=document.querySelector('#native-check');
            assert(check.checked,'Native checkbox starts checked');check.click();assert(!check.checked,'Native checkbox toggles');check.click();
            const capture=()=>{
              const audit=inspectRenderedColors();
              assert(audit.text.length>8 && audit.controls.length>=4,'Native controls must actually render');
              assert(!audit.failures.length && !audit.controlFailures.length,'Native contrast failure: '+JSON.stringify([...audit.failures,...audit.controlFailures]));
              return audit;
            };
            const original=capture();
            const toggle=document.querySelector('#switch-theme');toggle.click();
            await wait(()=>document.documentElement.dataset.theme!==theme);
            const toggled=capture();toggle.click();await wait(()=>document.documentElement.dataset.theme===theme);
            const scheme=getComputedStyle(document.documentElement).colorScheme;
            const accent=getComputedStyle(check).accentColor;
            assert(scheme===theme && typeof accent==='string' && accent.length>0 && accent!=='auto','Native color scheme and accent must resolve');
            const input=document.querySelector('input[aria-label="名称"]');
            input.focus();
            if(active){
              window.nativeColorKeyboardReady=false;
              window.webkit.messageHandlers.nativeColor.postMessage({keyboardFocus:true});
              await wait(()=>window.nativeColorKeyboardReady);
              input.focus();
            }
            if(active)assert(document.activeElement===input && input.matches(':focus-visible'),'Native input must actually have visible keyboard focus');
            const focused=capture();
            report({theme,active,viewport:[innerWidth,innerHeight],scheme,accent,checked:check.checked,original,toggled,focused});
          } catch(error){report({error:String(error)+'\n'+error.stack});}
        })();
        """#) { _,error in if let error=error {self.fail(String(describing:error))} }
    }
    func userContentController(_ controller:WKUserContentController,didReceive message:WKScriptMessage) {
        guard let data=message.body as? [String:Any] else {fail("Invalid native result")}
        if data["keyboardFocus"] as? Bool == true {
            // DOM click() switches WebKit to pointer modality. Send a real Tab
            // through this owned window before asserting keyboard focus-visible.
            window.makeFirstResponder(view)
            for type in [NSEvent.EventType.keyDown, NSEvent.EventType.keyUp] {
                if let event=NSEvent.keyEvent(with:type,location:.zero,modifierFlags:[],timestamp:ProcessInfo.processInfo.systemUptime,windowNumber:window.windowNumber,context:nil,characters:"\t",charactersIgnoringModifiers:"\t",isARepeat:false,keyCode:48) { window.sendEvent(event) }
            }
            view.evaluateJavaScript("window.nativeColorKeyboardReady=true") { _,error in if let error=error {self.fail(String(describing:error))} }
            return
        }
        if let error=data["error"] {fail(String(describing:error))}
        results.append(data)
        view.takeSnapshot(with:nil) { image,error in
            guard let image=image,let tiff=image.tiffRepresentation,let bitmap=NSBitmapImageRep(data:tiff),let png=bitmap.representation(using:.png,properties:[:]) else {self.fail(String(describing:error))}
            do {try png.write(to:self.output.appendingPathComponent("native-form-\(self.index).png"))} catch {self.fail(String(describing:error))}
            self.index+=1;self.next()
        }
    }
    func webView(_ webView:WKWebView,didFail navigation:WKNavigation!,withError error:Error){fail(error.localizedDescription)}
    func webView(_ webView:WKWebView,didFailProvisionalNavigation navigation:WKNavigation!,withError error:Error){fail(error.localizedDescription)}
    func webViewWebContentProcessDidTerminate(_ webView:WKWebView){fail("WebKit content process terminated")}
}
guard CommandLine.arguments.count>=3 else {fatalError("Expected base URL and output directory")}
let app=NSApplication.shared
app.setActivationPolicy(.accessory)
let activity=ProcessInfo.processInfo.beginActivity(options:[.userInitiatedAllowingIdleSystemSleep],reason:"Validate native form colors")
let review=NativeColorReview(base:CommandLine.arguments[1],output:URL(fileURLWithPath:CommandLine.arguments[2],isDirectory:true))
Timer.scheduledTimer(withTimeInterval:180,repeats:false){_ in review.fail("Native review timed out")}
review.start();app.run()
