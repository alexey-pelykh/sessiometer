import AppKit

// Skeleton menu-bar (LSUIElement) app: it creates a single NSStatusItem and does
// nothing else. The real menu-bar surface — icon, panel, and the AF_UNIX socket
// client that consumes the daemon's frozen status snapshot — is #168. This target
// exists only to prove the apps/menubar/ home and the dual-toolchain CI harness
// (#311), per ADR-0010. It links no Rust and shares no build graph with the crate.

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.title = "S"
        statusItem = item
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
