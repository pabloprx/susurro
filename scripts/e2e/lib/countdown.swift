// Countdown pill before GUI tests: `countdown [seconds] [message]`. Bottom-center, dark, no focus, click-through.
import AppKit

let args = CommandLine.arguments
let total = args.count > 1 ? Int(args[1]) ?? 5 : 5
let message = args.count > 2 ? args[2] : "susurro test · using your Mac in"

let app = NSApplication.shared
app.setActivationPolicy(.accessory)

let size = NSSize(width: 420, height: 56)
let screen = NSScreen.main!.visibleFrame
let frame = NSRect(x: screen.midX - size.width / 2, y: screen.minY + 24, width: size.width, height: size.height)
let panel = NSPanel(contentRect: frame, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
panel.level = .statusBar
panel.isOpaque = false
panel.backgroundColor = .clear
panel.hasShadow = true
panel.ignoresMouseEvents = true
panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]

let pill = NSView(frame: NSRect(origin: .zero, size: size))
pill.wantsLayer = true
pill.layer?.backgroundColor = NSColor(calibratedRed: 0.07, green: 0.07, blue: 0.07, alpha: 0.96).cgColor
pill.layer?.cornerRadius = 28
pill.layer?.borderWidth = 1
pill.layer?.borderColor = NSColor(calibratedRed: 1, green: 0.42, blue: 0.24, alpha: 0.55).cgColor

let dot = NSView(frame: NSRect(x: 22, y: 23, width: 10, height: 10))
dot.wantsLayer = true
dot.layer?.backgroundColor = NSColor(calibratedRed: 1, green: 0.42, blue: 0.24, alpha: 1).cgColor
dot.layer?.cornerRadius = 5
pill.addSubview(dot)

let label = NSTextField(labelWithString: "")
label.frame = NSRect(x: 44, y: 16, width: size.width - 64, height: 24)
label.font = NSFont.systemFont(ofSize: 15, weight: .medium)
label.textColor = NSColor(white: 0.93, alpha: 1)
pill.addSubview(label)
panel.contentView = pill

var left = total
func render() {
    label.stringValue = left > 0 ? "\(message) \(left)  ·  hands off" : "susurro test running  ·  move the mouse to stop"
}
render()
panel.orderFrontRegardless()
Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { t in
    left -= 1
    render()
    if left < 0 { t.invalidate(); panel.orderOut(nil); app.terminate(nil) }
}
app.run()
