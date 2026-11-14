import AppKit
import Foundation

// A deliberately opaque UI: search/text/send are drawn, not AX controls.
// Sending writes only a runner-owned temporary file, with no network access.
final class Canvas: NSView {
    var text = ""
    var editing = false
    var output: ((String, String) -> Void)!
    override var acceptsFirstResponder: Bool { true }
    override func isAccessibilityElement() -> Bool { false }
    override func accessibilityChildren() -> [Any]? { [] }
    override var accessibilityFocusedUIElement: Any? { nil }
    override func draw(_ dirtyRect: NSRect) {
        NSColor(calibratedRed: 0.15, green: 0.5, blue: 0.7, alpha: 1).setFill(); bounds.fill()
        NSColor.white.setFill(); NSRect(x: 24, y: 150, width: 400, height: 35).fill()
        let attributes: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: 15), .foregroundColor: NSColor.black]
        (text.isEmpty ? "Search or compose fixture text" : text).draw(at: NSPoint(x: 30, y: 157), withAttributes: attributes)
        NSColor.lightGray.setFill(); NSRect(x: 24, y: 60, width: 140, height: 32).fill()
        "Send to fixture file".draw(at: NSPoint(x: 30, y: 67), withAttributes: attributes)
    }
    func find() { editing = true; window?.makeFirstResponder(self); output("find", "READY"); needsDisplay = true }
    override func mouseDown(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        if NSRect(x: 24, y: 150, width: 400, height: 35).contains(p) {
            find(); output("click", "SEARCH")
        } else if NSRect(x: 24, y: 60, width: 140, height: 32).contains(p) {
            output("sent", text)
        }
    }
    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { editing = false; output("escape", "ESCAPED"); return }
        guard editing else { return }
        if event.keyCode == 51 { if !text.isEmpty { text.removeLast() } }
        else if let chars = event.characters, !event.modifierFlags.contains(.command), chars != "\r" { text += chars }
        output("text", text); needsDisplay = true
    }
    override func scrollWheel(with event: NSEvent) { output("scroll", "SCROLLED") }
}
final class Fixture: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    var overlay: NSWindow?
    let canvas = Canvas(frame: NSRect(x: 0, y: 0, width: 460, height: 240))
    let directory = ProcessInfo.processInfo.arguments[1]
    let role = ProcessInfo.processInfo.arguments[2]
    func write(_ name: String, _ value: String) { try? value.write(toFile: directory + "/" + role + "-" + name, atomically: true, encoding: .utf8) }
    func applicationDidFinishLaunching(_ notification: Notification) {
        let overlayOnly = ProcessInfo.processInfo.arguments.contains("--overlay-only")
        if overlayOnly {
            window = NSPanel(contentRect: NSRect(x: 250, y: 250, width: 100, height: 80), styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
            window.level = .floating
        } else {
            window = NSWindow(contentRect: NSRect(x: 200, y: 200, width: 460, height: 240), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        }
        window.title = "Echo opaque " + role
        if ProcessInfo.processInfo.arguments.contains("--sharing-none") { window.sharingType = .none }
        canvas.output = { self.write($0, $1) }
        window.contentView = canvas
        let menu = NSMenu(); let edit = NSMenuItem(); edit.submenu = NSMenu(title: "Edit")
        let find = NSMenuItem(title: "Find in fixture", action: #selector(find), keyEquivalent: "f"); find.target = self
        edit.submenu!.addItem(find); menu.addItem(edit); NSApp.mainMenu = menu
        if overlayOnly { window.orderFrontRegardless() }
        else { window.makeKeyAndOrderFront(nil); window.makeFirstResponder(canvas); NSApp.activate(ignoringOtherApps: true) }
        write("pid", String(ProcessInfo.processInfo.processIdentifier))
        Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { _ in
            let command = try? String(contentsOfFile: self.directory + "/" + self.role + "-overlay-command", encoding: .utf8)
            if overlayOnly {
                if command == "hide" { self.window.orderOut(nil); self.write("overlay-hidden", "READY") }
                if command == "quit" { self.write("quit", "READY"); NSApp.terminate(nil) }
                return
            }
            if (command == "show" || command == "document") && self.overlay == nil {
                let frame = self.window.frame
                let rect = NSRect(x: frame.midX, y: frame.midY, width: 100, height: 80)
                let panel: NSWindow = command == "document"
                    ? NSWindow(contentRect: rect, styleMask: [.titled, .closable], backing: .buffered, defer: false)
                    : NSPanel(contentRect: rect, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
                if command != "document" { panel.setAccessibilitySubrole(.dialog) }
                panel.backgroundColor = .red; panel.level = .floating
                panel.orderFrontRegardless(); self.overlay = panel; self.write(command == "document" ? "document-shown" : "overlay-shown", "READY")
            } else if command == "hide" && self.overlay != nil {
                self.overlay?.orderOut(nil); self.overlay = nil; self.write("overlay-hidden", "READY")
            }
        }
    }
    @objc func find() { canvas.find() }
}
let app = NSApplication.shared; app.setActivationPolicy(.regular)
let fixture = Fixture(); app.delegate = fixture; app.run()
