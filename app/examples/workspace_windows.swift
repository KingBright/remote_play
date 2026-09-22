// Manual macOS capture fixture for workspace_probe. Close both test windows or stop this process after acceptance.
// Floating windows remain capturable while the receiving UI is active.
import AppKit
final class ProbeView: NSView {
    var phase: CGFloat = 0
    let hue: CGFloat
    init(frame: NSRect, hue: CGFloat) { self.hue = hue; super.init(frame: frame) }
    required init?(coder: NSCoder) { fatalError() }
    override func draw(_ dirtyRect: NSRect) {
        NSColor(calibratedHue: hue, saturation: 0.7, brightness: 0.8, alpha: 1).setFill()
        bounds.fill()
        NSColor.white.setFill()
        NSRect(x: phase.truncatingRemainder(dividingBy: max(1, bounds.width - 60)), y: 20, width: 60, height: bounds.height - 40).fill()
    }
}
let app = NSApplication.shared
app.setActivationPolicy(.regular)
var windows: [NSWindow] = []
var views: [ProbeView] = []
for (index, size) in [NSSize(width: 720, height: 340), NSSize(width: 380, height: 600)].enumerated() {
    let window = NSWindow(contentRect: NSRect(x: CGFloat(120 + index * 180), y: 120, width: size.width, height: size.height), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
    window.level = .floating
    window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
    window.title = "RemotePlay V2 Probe \(index + 1)"
    let view = ProbeView(frame: NSRect(origin: .zero, size: size), hue: CGFloat(index) * 0.5 + 0.1)
    window.contentView = view
    window.makeKeyAndOrderFront(nil)
    window.orderFrontRegardless()
    windows.append(window); views.append(view)
}
let timer = Timer.scheduledTimer(withTimeInterval: 0.025, repeats: true) { _ in for view in views { view.phase += 5; view.needsDisplay = true } }
app.activate(ignoringOtherApps: true)
app.run()
