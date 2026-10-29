import AppKit
import ApplicationServices
import Foundation

// Drive the shipped host, not a test-only runtime path. Missing GUI/TCC permissions
// are failures with evidence, never successful substitutes for UI interaction.
let appURL = URL(fileURLWithPath: CommandLine.arguments[1]).standardizedFileURL
let diagnostics = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
let bundleID = "dev.cloudboy.tokenbubble"
try FileManager.default.createDirectory(at: diagnostics, withIntermediateDirectories: true)

func record(_ text: String) {
    print(text)
    fflush(stdout)
}
func fail(_ message: String) -> Never {
    record("FAIL: \(message)")
    exit(1)
}
func waitUntil(_ label: String, seconds: Double = 15, _ condition: () -> Bool) {
    let deadline = Date().addingTimeInterval(seconds)
    while Date() < deadline {
        if condition() { record("PASS: \(label)"); return }
        RunLoop.current.run(until: Date().addingTimeInterval(0.15))
    }
    fail("Timed out: \(label)")
}
@discardableResult
func run(_ executable: String, _ arguments: [String]) -> Int32 {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: executable)
    process.arguments = arguments
    do { try process.run(); process.waitUntilExit(); return process.terminationStatus }
    catch { record("Could not run \(executable): \(error)"); return -1 }
}
func screenshot(_ name: String) {
    let destination = diagnostics.appendingPathComponent(name + ".png").path
    if run("/usr/sbin/screencapture", ["-x", destination]) != 0 {
        fail("Screenshot capture failed; runner needs an active GUI session and Screen Recording permission")
    }
    record("Screenshot: \(name).png")
}
func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    let result = AXUIElementCopyAttributeValue(element, name as CFString, &value)
    return result == .success ? value : nil
}
func text(_ element: AXUIElement, _ name: String) -> String {
    attribute(element, name) as? String ?? ""
}
func findElement(_ root: AXUIElement, _ matches: (AXUIElement) -> Bool) -> AXUIElement? {
    var queue: [(AXUIElement, Int)] = [(root, 0)]
    var visited = 0
    while !queue.isEmpty && visited < 600 {
        let (element, depth) = queue.removeFirst()
        visited += 1
        if matches(element) { return element }
        if depth >= 9 { continue }
        for key in ["AXChildren", "AXExtrasMenuBar", "AXMenuBar", "AXWindows"] {
            guard let value = attribute(element, key) else { continue }
            if CFGetTypeID(value) == AXUIElementGetTypeID() {
                queue.append((value as! AXUIElement, depth + 1))
            } else if let children = value as? [AXUIElement] {
                queue.append(contentsOf: children.map { ($0, depth + 1) })
            }
        }
    }
    return nil
}
func press(_ element: AXUIElement, _ label: String) {
    let error = AXUIElementPerformAction(element, kAXPressAction as CFString)
    if error != .success { fail("AXPress \(label) returned \(error.rawValue)") }
    record("Interaction: \(label)")
}
func escape() {
    CGEvent(keyboardEventSource: nil, virtualKey: 53, keyDown: true)?.post(tap: .cghidEventTap)
    CGEvent(keyboardEventSource: nil, virtualKey: 53, keyDown: false)?.post(tap: .cghidEventTap)
}

var running: NSRunningApplication?
waitUntil("Finder/open launched the packaged app") {
    running = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).first
    return running != nil
}
guard let app = running else { fail("No running bundle") }
defer { if !app.isTerminated { app.terminate() } }
record("pid=\(app.processIdentifier) bundle=\(app.bundleURL?.path ?? "nil") executable=\(app.executableURL?.path ?? "nil") activationPolicy=\(app.activationPolicy.rawValue)")
guard app.bundleURL?.standardizedFileURL == appURL else {
    fail("The running app is not the extracted release bundle")
}
waitUntil("menu-bar accessory activation policy (not a Dock app)") { app.activationPolicy == .accessory }
let pid = app.processIdentifier
func visibleWindows() -> [[String: Any]] {
    let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
    return windows.filter { window in
        guard (window[kCGWindowOwnerPID as String] as? Int) == Int(pid),
              // GPUI uses level 101 for usage popups and level 0 for Settings.
              let layer = window[kCGWindowLayer as String] as? Int,
              layer == 0 || layer == 101,
              let bounds = window[kCGWindowBounds as String] as? [String: Any],
              let width = bounds["Width"] as? Double,
              let height = bounds["Height"] as? Double else { return false }
        return width > 100 && height > 60
    }
}
Thread.sleep(forTimeInterval: 2)
record("Initial visible app windows: \(visibleWindows())")
guard visibleWindows().isEmpty else { fail("A normal window appeared at launch instead of menu-bar-only startup") }
let trusted = AXIsProcessTrusted()
let screenCapture = CGPreflightScreenCaptureAccess()
let eventPosting = CGPreflightPostEventAccess()
record("Permissions: accessibility=\(trusted), screenRecording=\(screenCapture), eventPosting=\(eventPosting)")
screenshot("01-menu-bar-launch")
let application = AXUIElementCreateApplication(pid)
if !trusted || !eventPosting {
    // Still exercise the normal LaunchServices reopen path and retain useful
    // evidence before failing honestly because the required clicks cannot run.
    guard run("/usr/bin/open", [appURL.path]) == 0 else { fail("LaunchServices reopen failed") }
    waitUntil("reopening the actual bundle shows its usage window") { !visibleWindows().isEmpty }
    record("Reopen window evidence: \(visibleWindows())")
    screenshot("02-reopen-without-accessibility")
    fail("Runner lacks Accessibility/event-posting permission. Enable it for the runner's GUI execution host in Privacy & Security; native status-menu interactions were NOT exercised")
}
guard screenCapture else {
    fail("Screen Recording permission unavailable: screenshots cannot establish real UI evidence")
}
var status: AXUIElement?
waitUntil("accessible TokenBubble menu-bar item") {
    status = findElement(application) { element in
        text(element, "AXIdentifier") == "tokenbubble.status-item" ||
        text(element, "AXDescription") == "TokenBubble" ||
        text(element, "AXTitle") == "🫧"
    }
    return status != nil
}
guard let statusItem = status else { fail("No accessible status item") }
record("Status item: role=\(text(statusItem, "AXRole")) title=\(text(statusItem, "AXTitle")) description=\(text(statusItem, "AXDescription")) identifier=\(text(statusItem, "AXIdentifier"))")
press(statusItem, "left-click status item")
waitUntil("status item opens the real usage panel") { !visibleWindows().isEmpty }
record("Usage window evidence: \(visibleWindows())")
screenshot("02-status-usage-panel")
escape()
waitUntil("Escape dismisses usage") { visibleWindows().isEmpty }
guard run("/usr/bin/open", [appURL.path]) == 0 else { fail("LaunchServices reopen failed") }
waitUntil("bundle reopen reveals the existing host usage panel") { !visibleWindows().isEmpty }
screenshot("03-reopened-usage-panel")
escape()
waitUntil("Escape dismisses reopened usage") { visibleWindows().isEmpty }

func rightClickStatus() {
    guard let positionValue = attribute(statusItem, "AXPosition"),
          let sizeValue = attribute(statusItem, "AXSize"),
          CFGetTypeID(positionValue) == AXValueGetTypeID(),
          CFGetTypeID(sizeValue) == AXValueGetTypeID() else {
        fail("Status item does not expose screen geometry")
    }
    var position = CGPoint.zero
    var size = CGSize.zero
    guard AXValueGetValue(positionValue as! AXValue, .cgPoint, &position),
          AXValueGetValue(sizeValue as! AXValue, .cgSize, &size) else {
        fail("Could not decode status geometry")
    }
    let point = CGPoint(x: position.x + size.width / 2, y: position.y + size.height / 2)
    CGEvent(mouseEventSource: nil, mouseType: .rightMouseDown, mouseCursorPosition: point, mouseButton: .right)?.post(tap: .cghidEventTap)
    CGEvent(mouseEventSource: nil, mouseType: .rightMouseUp, mouseCursorPosition: point, mouseButton: .right)?.post(tap: .cghidEventTap)
    record("Interaction: right-click status item at \(point)")
}
func selectMenu(_ title: String) {
    var item: AXUIElement?
    waitUntil("native menu contains \(title)") {
        item = findElement(application) { text($0, "AXTitle") == title }
        return item != nil
    }
    guard let item else { fail("Missing menu command \(title)") }
    press(item, title)
}
rightClickStatus()
selectMenu("Settings…")
var settings: AXUIElement?
waitUntil("native menu opens TokenBubble Settings") {
    settings = findElement(application) { text($0, "AXRole") == "AXWindow" && text($0, "AXTitle") == "TokenBubble Settings" }
    return settings != nil && !visibleWindows().isEmpty
}
screenshot("04-native-menu-settings")
guard let settingsWindow = settings,
      let close = attribute(settingsWindow, "AXCloseButton"),
      CFGetTypeID(close) == AXUIElementGetTypeID() else { fail("Settings window has no accessible close button") }
press(close as! AXUIElement, "close Settings")
waitUntil("settings close button closes the window") { visibleWindows().isEmpty }
rightClickStatus()
selectMenu("Quit TokenBubble")
waitUntil("native Quit terminates the actual app") { app.isTerminated }
screenshot("05-after-quit")
record("PASS: packaged macOS menu-bar lifecycle, panel toggle/dismissal, reopen, native Settings, and native Quit exercised")
