import ApplicationServices
import Cocoa
import Foundation

guard CommandLine.arguments.count == 4,
      let parsedPID = Int32(CommandLine.arguments[1]) else {
    fputs("usage: macos-os-input-driver <pid> <os-input|rapid-live-resize> <driver-result-path>\n", stderr)
    exit(2)
}
let pid = pid_t(parsedPID)
let mode = CommandLine.arguments[2]
let driverResultPath = CommandLine.arguments[3]

let trustOptions = [
    kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true
] as CFDictionary
guard AXIsProcessTrustedWithOptions(trustOptions) else {
    fputs("Accessibility permission is required for macOS OS-input smoke.\n", stderr)
    exit(2)
}

let application = AXUIElementCreateApplication(pid)
var windowsValue: CFTypeRef?
guard AXUIElementCopyAttributeValue(application, kAXWindowsAttribute as CFString, &windowsValue) == .success,
      let window = (windowsValue as? [AXUIElement])?.first else {
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

func postKey(_ keyCode: CGKeyCode) {
    CGEvent(keyboardEventSource: nil, virtualKey: keyCode, keyDown: true)?.post(tap: .cghidEventTap)
    CGEvent(keyboardEventSource: nil, virtualKey: keyCode, keyDown: false)?.post(tap: .cghidEventTap)
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
