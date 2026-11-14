import AppKit
import Foundation

// Disposable native acceptance app. All output is confined to its runner's
// unique temporary directory; it never opens user documents or services.
final class FixtureField: NSTextField {
    var clicked: (() -> Void)?
    override func mouseDown(with event: NSEvent) { clicked?(); super.mouseDown(with: event) }
}
final class Fixture: NSObject, NSApplicationDelegate, NSTextFieldDelegate {
    var window: NSWindow!
    let field = FixtureField(frame: NSRect(x: 20, y: 100, width: 400, height: 30))
    let directory = ProcessInfo.processInfo.arguments[1]
    let role = ProcessInfo.processInfo.arguments[2]
    func write(_ name: String, _ value: String) {
        try? value.write(toFile: directory + "/" + role + "-" + name, atomically: true, encoding: .utf8)
    }
    func applicationDidFinishLaunching(_ notification: Notification) {
        window = NSWindow(contentRect: NSRect(x: 150, y: 150, width: 460, height: 220),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Echo isolated " + role
        window.backgroundColor = role == "target" ? NSColor(calibratedRed: 0.8, green: 0.15, blue: 0.15, alpha: 1)
            : NSColor(calibratedRed: 0.15, green: 0.75, blue: 0.2, alpha: 1)
        field.stringValue = role == "target" ? "INITIAL-TARGET" : "UNCHANGED-OTHER"
        field.setAccessibilityIdentifier("echo-background-input")
        field.delegate = self
        field.clicked = { self.write("click", "CLICK-RECEIVED") }
        window.contentView!.addSubview(field)
        let button = NSButton(title: "Commit fixture", target: self, action: #selector(commit))
        button.frame = NSRect(x: 20, y: 45, width: 180, height: 32)
        button.setAccessibilityIdentifier("echo-background-commit")
        window.contentView!.addSubview(button)
        let menu = NSMenu()
        let app = NSMenuItem(); app.submenu = NSMenu()
        let marker = NSMenuItem(title: "Record keyboard marker", action: #selector(keyMarker), keyEquivalent: "k")
        marker.keyEquivalentModifierMask = [.command, .shift]; marker.target = self
        app.submenu!.addItem(marker); menu.addItem(app)
        let edit = NSMenuItem(); edit.submenu = NSMenu(title: "Edit")
        let selectAll = NSMenuItem(title: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        edit.submenu!.addItem(selectAll); menu.addItem(edit); NSApp.mainMenu = menu
        if role == "target" {
            NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .leftMouseDown]) { event in
                if event.type == .leftMouseDown {
                    self.write("mouse-event", "window=\(event.windowNumber) point=\(event.locationInWindow) cg=\(String(describing:event.cgEvent?.location)) actual=\(self.window.windowNumber) frame=\(self.window.frame) field=\(self.field.frame)")
                    return event
                }
                let file = self.directory + "/target-events"
                let previous = (try? String(contentsOfFile: file, encoding: .utf8)) ?? ""
                let line = "code=\(event.keyCode) flags=\(event.modifierFlags.rawValue) characters=\(event.characters ?? "")\n"
                if previous.utf8.count < 16000 { try? (previous + line).write(toFile: file, atomically: true, encoding: .utf8) }
                return event
            }
        }
        window.makeFirstResponder(field)
        window.orderFrontRegardless()
        if role == "other" { NSApp.activate(ignoringOtherApps: true) }
        write("pid", String(ProcessInfo.processInfo.processIdentifier))
    }
    @objc func commit() { write("commit", field.stringValue) }
    @objc func keyMarker() { write("key", "KEY-RECEIVED") }
    func controlTextDidChange(_ notification: Notification) { write("change", field.stringValue) }
}
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let delegate = Fixture()
app.delegate = delegate
app.run()
