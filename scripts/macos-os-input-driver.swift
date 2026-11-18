import ApplicationServices
import Cocoa
import Foundation

guard CommandLine.arguments.count >= 4,
      let parsedPID = Int32(CommandLine.arguments[1]) else {
    fputs("usage: macos-os-input-driver <pid> <os-input|rapid-live-resize> <driver-result-path>\n", stderr)
    exit(2)
}
let pid = pid_t(parsedPID)
let mode = CommandLine.arguments[2]
let driverResultPath = CommandLine.arguments[3]

// Existing-window scrolling is deliberately nonprompting and does not enter the
// isolated smoke setup (AXRaise, resize, keyboard input or candidate launch).
if mode == "scroll-existing" {
    runExistingScroll()
    exit(0)
}
guard CommandLine.arguments.count == 4 else {
    fputs("invalid smoke arguments\n", stderr)
    exit(2)
}

let trustOptions = [
    kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true
] as CFDictionary
guard AXIsProcessTrustedWithOptions(trustOptions) else {
    fputs("Accessibility permission is required for macOS OS-input smoke.\n", stderr)
    exit(2)
}

let application = AXUIElementCreateApplication(pid)
let windowDeadline = Date().addingTimeInterval(10)
var window: AXUIElement?
while Date() < windowDeadline && window == nil {
    var windowsValue: CFTypeRef?
    if AXUIElementCopyAttributeValue(application, kAXWindowsAttribute as CFString, &windowsValue) == .success {
        window = (windowsValue as? [AXUIElement])?.first
    }
    if window == nil {
        Thread.sleep(forTimeInterval: 0.1)
    }
}
guard let window else {
    fputs("fesTerm did not expose an accessibility window.\n", stderr)
    exit(1)
}

AXUIElementPerformAction(window, kAXRaiseAction as CFString)

func writeDriverResult(_ status: String) {
    try? "status=\(status)\n".write(toFile: driverResultPath, atomically: true, encoding: .utf8)
}

func setWindowSize(_ width: CGFloat, _ height: CGFloat) -> Bool {
    var position = CGPoint(x: 100, y: 100)
    var size = CGSize(width: width, height: height)
    guard let positionValue = AXValueCreate(.cgPoint, &position),
          let sizeValue = AXValueCreate(.cgSize, &size) else {
        return false
    }
    return AXUIElementSetAttributeValue(window, kAXPositionAttribute as CFString, positionValue) == .success &&
        AXUIElementSetAttributeValue(window, kAXSizeAttribute as CFString, sizeValue) == .success
}

func windowFrame() -> (position: CGPoint, size: CGSize)? {
    var positionValue: CFTypeRef?
    var sizeValue: CFTypeRef?
    guard AXUIElementCopyAttributeValue(window, kAXPositionAttribute as CFString, &positionValue) == .success,
          AXUIElementCopyAttributeValue(window, kAXSizeAttribute as CFString, &sizeValue) == .success,
          let positionValue,
          let sizeValue,
          CFGetTypeID(positionValue) == AXValueGetTypeID(),
          CFGetTypeID(sizeValue) == AXValueGetTypeID() else {
        return nil
    }
    let positionAXValue = unsafeBitCast(positionValue, to: AXValue.self)
    let sizeAXValue = unsafeBitCast(sizeValue, to: AXValue.self)
    var position = CGPoint.zero
    var size = CGSize.zero
    guard AXValueGetValue(positionAXValue, .cgPoint, &position),
          AXValueGetValue(sizeAXValue, .cgSize, &size) else {
        return nil
    }
    return (position, size)
}

func postMouse(_ type: CGEventType, at point: CGPoint) -> Bool {
    guard let event = CGEvent(
        mouseEventSource: nil,
        mouseType: type,
        mouseCursorPosition: point,
        mouseButton: .left
    ) else {
        return false
    }
    event.setIntegerValueField(.mouseEventClickState, value: 1)
    event.post(tap: .cghidEventTap)
    return true
}

func runOsInputSmoke() {
    let sizes: [(CGFloat, CGFloat)] = [(420, 260), (860, 540), (560, 360), (860, 540)]
    for (width, height) in sizes {
        guard setWindowSize(width, height) else {
            fputs("could not resize fesTerm through Accessibility.\n", stderr)
            exit(1)
        }
        Thread.sleep(forTimeInterval: 1)
    }

    let clickPoint = CGPoint(x: 530, y: 370)
    for type in [CGEventType.leftMouseDown, .leftMouseUp] {
        guard postMouse(type, at: clickPoint) else {
            fputs("could not create mouse event.\n", stderr)
            exit(1)
        }
    }

    if ProcessInfo.processInfo.environment["FESTERM_NATIVE_KEYBOARD_ROUTING_SMOKE"] == "1" {
        postKey(8, flags: .maskCommand) // Copy, no terminal interrupt
        postKey(35, flags: [.maskCommand, .maskShift]) // Command palette
        Thread.sleep(forTimeInterval: 1)
        postKey(53) // Escape closes palette, not terminal input
        Thread.sleep(forTimeInterval: 1)
        postKey(35, flags: [.maskCommand, .maskShift])
        Thread.sleep(forTimeInterval: 1)
        for key: CGKeyCode in [35, 0, 1, 17, 14] { postKey(key) } // paste
        postKey(36) // Palette Paste requests the identified clipboard read
        Thread.sleep(forTimeInterval: 1)
        postKey(11, flags: .maskControl)
        postKey(11, flags: [.maskControl, .maskShift])
    }
    postKey(48) // Tab
    postKey(126) // Up Arrow
    for scalar in "os-input-ok".unicodeScalars {
        guard let event = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true) else {
            fputs("could not create keyboard event.\n", stderr)
            exit(1)
        }
        var codeUnit = UniChar(scalar.value)
        event.keyboardSetUnicodeString(stringLength: 1, unicodeString: &codeUnit)
        event.post(tap: .cghidEventTap)
    }
    postKey(36) // Return
}

func runRapidLiveResizeSmoke() {
    guard let initialFrame = windowFrame() else {
        fputs("could not read fesTerm's native window frame.\n", stderr)
        exit(1)
    }

    // This is intentionally a physical corner drag, not AXSize assignment.
    // Accessibility is used only to discover the exposed window frame; the
    // WindowServer receives CGEvent down/drag/up events at the resize handle.
    let grabPoint = CGPoint(
        x: initialFrame.position.x + initialFrame.size.width - 2,
        y: initialFrame.position.y + initialFrame.size.height - 2
    )
    guard postMouse(.mouseMoved, at: grabPoint),
          postMouse(.leftMouseDown, at: grabPoint) else {
        fputs("could not begin physical fesTerm corner drag.\n", stderr)
        exit(1)
    }

    let targetSizes: [(CGFloat, CGFloat)] = [
        (520, 360), (760, 500), (600, 400), (840, 540),
        (680, 440), (880, 560), (640, 420), (900, 580),
    ]
    for index in 0..<64 {
        let (width, height) = targetSizes[index % targetSizes.count]
        let target = CGPoint(
            x: initialFrame.position.x + width,
            y: initialFrame.position.y + height
        )
        guard postMouse(.leftMouseDragged, at: target) else {
            fputs("could not post physical fesTerm corner drag event.\n", stderr)
            exit(1)
        }
        Thread.sleep(forTimeInterval: 0.025)
    }

    let finalPoint = CGPoint(
        x: initialFrame.position.x + targetSizes.last!.0,
        y: initialFrame.position.y + targetSizes.last!.1
    )
    guard postMouse(.leftMouseUp, at: finalPoint) else {
        fputs("could not create mouse event.\n", stderr)
        exit(1)
    }

    Thread.sleep(forTimeInterval: 0.2)
    guard let finalFrame = windowFrame(),
          abs(finalFrame.size.width - initialFrame.size.width) > 20 ||
              abs(finalFrame.size.height - initialFrame.size.height) > 20 else {
        fputs("physical fesTerm corner drag did not change the native window size.\n", stderr)
        exit(1)
    }
    writeDriverResult("pass")
}

func postKey(_ keyCode: CGKeyCode, flags: CGEventFlags = []) {
    for down in [true, false] {
        let event = CGEvent(keyboardEventSource: nil, virtualKey: keyCode, keyDown: down)
        event?.flags = flags
        event?.post(tap: .cghidEventTap)
    }
}

switch mode {
case "os-input":
    runOsInputSmoke()
case "rapid-live-resize":
    runRapidLiveResizeSmoke()
default:
    fputs("unsupported macOS OS-input smoke mode: \(mode)\n", stderr)
    exit(2)
}


// One operator-selected scroll, not a new adapter job mode or a scroll-success oracle.
func runExistingScroll() {
    func blocked(_ reason: String) -> Never {
        fputs("status=blocked\nreason=\(reason)\nevents_posted=0\n", stderr)
        exit(2)
    }
    guard CommandLine.arguments.count == 8,
          let x = Double(CommandLine.arguments[4]), x.isFinite,
          let y = Double(CommandLine.arguments[5]), y.isFinite,
          let detents = Int32(CommandLine.arguments[6]), (-12...12).contains(detents), detents != 0,
          pid > 0 else { blocked("invalid-arguments") }
    let expectedStart = CommandLine.arguments[7]
    guard !expectedStart.isEmpty, expectedStart.count <= 64 else { blocked("invalid-start-binding") }
    guard AXIsProcessTrusted(), CGPreflightPostEventAccess() else { blocked("access-missing") }
    guard let app = NSRunningApplication(processIdentifier: pid),
          let birth = app.launchDate,
          let identity = app.bundleIdentifier,
          ["dev.fes.festerm", "dev.fes.festerm.development"].contains(identity),
          let bundle = app.bundleURL, let executable = app.executableURL,
          executable.standardizedFileURL == bundle.appendingPathComponent("Contents/MacOS/festerm").standardizedFileURL
    else { blocked("not-festerm") }
    let query = Process(); query.executableURL = URL(fileURLWithPath: "/bin/ps")
    query.arguments = ["-p", String(pid), "-o", "lstart="]
    query.environment = ["LC_ALL": "C", "PATH": "/usr/bin:/bin"]
    let output = Pipe(); query.standardOutput = output; query.standardError = FileHandle.nullDevice
    do { try query.run() } catch { blocked("start-unavailable") }
    let queryDeadline = ProcessInfo.processInfo.systemUptime + 2
    while query.isRunning && ProcessInfo.processInfo.systemUptime < queryDeadline { Thread.sleep(forTimeInterval: 0.01) }
    if query.isRunning { query.terminate(); blocked("start-unavailable") }
    guard query.terminationStatus == 0,
          String(data: output.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) == expectedStart
    else { blocked("process-replaced") }
    let point = CGPoint(x: x, y: y)
    func frame(_ row: [String: Any]) -> CGRect? {
        guard let b = row[kCGWindowBounds as String] as? [String: Double],
              let x=b["X"], let y=b["Y"], let w=b["Width"], let h=b["Height"],
              [x,y,w,h].allSatisfy({$0.isFinite}) else { return nil }
        return CGRect(x:x,y:y,width:w,height:h)
    }
    func selectedWindow() -> Int? {
        guard !app.isTerminated, app.launchDate == birth,
              let current = NSRunningApplication(processIdentifier: pid), current.launchDate == birth,
              NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
              let rows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else { return nil }
        let owned = rows.filter { ($0[kCGWindowOwnerPID as String] as? Int) == Int(pid) && ($0[kCGWindowLayer as String] as? Int) == 0 }
        guard owned.count == 1, let window = frame(owned[0]), window.insetBy(dx:4,dy:4).contains(point),
              let number = owned[0][kCGWindowNumber as String] as? Int,
              let hit = rows.first(where: { frame($0)?.contains(point) == true }),
              (hit[kCGWindowOwnerPID as String] as? Int) == Int(pid),
              (hit[kCGWindowNumber as String] as? Int) == number else { return nil }
        return number
    }
    guard let window = selectedWindow() else { blocked("foreign-or-occluded-window") }
    guard let move = CGEvent(mouseEventSource:nil, mouseType:.mouseMoved, mouseCursorPosition:point, mouseButton:.left),
          let scroll = CGEvent(scrollWheelEvent2Source:nil, units:.line, wheelCount:1, wheel1:detents, wheel2:0, wheel3:0)
    else { blocked("event-unavailable") }
    // Construct everything and revalidate before the first event. No mouse button is pressed.
    guard AXIsProcessTrusted(), CGPreflightPostEventAccess(), selectedWindow() == window else { blocked("target-or-access-changed") }
    let deadline = ProcessInfo.processInfo.systemUptime + 1
    move.post(tap:.cghidEventTap)
    Thread.sleep(forTimeInterval:0.2)
    guard ProcessInfo.processInfo.systemUptime < deadline, AXIsProcessTrusted(), CGPreflightPostEventAccess(), selectedWindow() == window else {
        fputs("status=blocked\nreason=target-or-access-changed\nevents_posted=1\n", stderr)
        exit(2)
    }
    scroll.location = point; scroll.post(tap:.cghidEventTap)
    // Sending is not proof the content moved; operator before/after native captures decide that.
    do { try "status=sent\nevents_posted=2\n".write(toFile:driverResultPath, atomically:true, encoding:.utf8) }
    catch { fputs("status=sent-result-write-failed\nevents_posted=2\n", stderr); exit(1) }
}
