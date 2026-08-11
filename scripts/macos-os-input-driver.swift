import ApplicationServices
import Cocoa
import Foundation

guard CommandLine.arguments.count == 2,
      let parsedPID = Int32(CommandLine.arguments[1]) else {
    fputs("usage: macos-os-input-driver <pid>\n", stderr)
    exit(2)
}
let pid = pid_t(parsedPID)

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
let sizes: [(CGFloat, CGFloat)] = [(420, 260), (860, 540), (560, 360), (860, 540)]
for (width, height) in sizes {
    var position = CGPoint(x: 100, y: 100)
    var size = CGSize(width: width, height: height)
    guard let positionValue = AXValueCreate(.cgPoint, &position),
          let sizeValue = AXValueCreate(.cgSize, &size),
          AXUIElementSetAttributeValue(window, kAXPositionAttribute as CFString, positionValue) == .success,
          AXUIElementSetAttributeValue(window, kAXSizeAttribute as CFString, sizeValue) == .success else {
        fputs("could not resize fesTerm through Accessibility.\n", stderr)
        exit(1)
    }
    Thread.sleep(forTimeInterval: 1)
}

let clickPoint = CGPoint(x: 530, y: 370)
for type in [CGEventType.leftMouseDown, .leftMouseUp] {
    guard let event = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: clickPoint, mouseButton: .left) else {
        fputs("could not create mouse event.\n", stderr)
        exit(1)
    }
    event.post(tap: .cghidEventTap)
}

func postKey(_ keyCode: CGKeyCode) {
    CGEvent(keyboardEventSource: nil, virtualKey: keyCode, keyDown: true)?.post(tap: .cghidEventTap)
    CGEvent(keyboardEventSource: nil, virtualKey: keyCode, keyDown: false)?.post(tap: .cghidEventTap)
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
