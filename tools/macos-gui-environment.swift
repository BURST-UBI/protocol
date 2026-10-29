import AppKit
import ApplicationServices
let pid = pid_t(CommandLine.arguments[1])!
let app = AXUIElementCreateApplication(pid)
var windows: CFTypeRef?
let status = AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &windows)
let running = NSRunningApplication(processIdentifier: pid)
let result: [String: Any] = [
  "accessibilityTrusted": AXIsProcessTrusted(),
  "windowQueryStatus": status.rawValue,
  "windowCount": (windows as? [AXUIElement])?.count ?? 0,
  "applicationRunning": running != nil && !(running?.isTerminated ?? true),
  "activationPolicy": running?.activationPolicy.rawValue ?? -1,
  "sessionAvailable": CGSessionCopyCurrentDictionary() != nil
]
let data = try JSONSerialization.data(withJSONObject: result, options: [.prettyPrinted, .sortedKeys])
print(String(data: data, encoding: .utf8)!)
