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

// Inspect only labels and control state on fresh empty profiles, never wallet secrets.
func attribute(_ element: AXUIElement, _ key: String) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, key as CFString, &value) == .success else { return nil }
    return value
}
func nodes() -> [AXUIElement] {
    var all: [AXUIElement] = []
    func visit(_ node: AXUIElement, _ depth: Int) {
        guard depth < 40 && all.count < 2000 else { return }
        all.append(node)
        for child in attribute(node, kAXChildrenAttribute) as? [AXUIElement] ?? [] { visit(child, depth + 1) }
    }
    visit(app, 0)
    return all
}
func matches(_ node: AXUIElement, _ label: String) -> Bool {
    return [kAXTitleAttribute, kAXDescriptionAttribute, kAXValueAttribute].contains {
        (attribute(node, $0) as? String) == label
    }
}
func waitFor(_ label: String) -> AXUIElement? {
    for _ in 0..<50 {
        if let node = nodes().first(where: { matches($0, label) }) { return node }
        Thread.sleep(forTimeInterval: 0.1)
    }
    return nil
}
func require(_ condition: Bool, _ message: String) {
    if !condition { fputs("FAIL: \(message)\n", stderr); exit(1) }
}
func press(_ label: String) {
    let control = nodes().first { node in
        var actions: CFArray?
        guard AXUIElementCopyActionNames(node, &actions) == .success,
              (actions as? [String] ?? []).contains(kAXPressAction) else { return false }
        let children = attribute(node, kAXChildrenAttribute) as? [AXUIElement] ?? []
        return matches(node, label) || children.contains { matches($0, label) }
    }
    if control == nil {
        for node in nodes() where matches(node, label) {
            print("Missing action diagnostic role:", attribute(node, kAXRoleAttribute) ?? "unknown" as CFString)
        }
    }
    require(control != nil, "Missing action: \(label)")
    require(AXUIElementPerformAction(control!, kAXPressAction as CFString) == .success, "Action failed: \(label)")
}
if CommandLine.arguments.count > 2 {
    require(AXIsProcessTrusted(), "Native accessibility unavailable")
    if CommandLine.arguments[2] == "wallet" {
        require(waitFor("Your Burst wallet") != nil, "Wallet onboarding heading")
        press("Create New Wallet")
        require(waitFor("Generate Wallet") != nil, "Native wallet generation action")
        press("Generate Wallet")
        require(waitFor("Back Up Your Wallet") != nil, "Native wallet keypair and recovery phrase returned")
        for label in ["Continue without file", "Save & Continue"] {
            let control = nodes().first { matches($0, label) && (attribute($0,kAXRoleAttribute) as? String) == kAXButtonRole }
            require(control != nil && (attribute(control!, kAXEnabledAttribute) as? Bool) == false, "Unacknowledged backup must block continuation")
        }
        print("GUI_PASS wallet native keypair creation and unacknowledged backup guards; no secrets recorded")
    } else {
        require(waitFor("Your node is isolated") != nil, "Live native RPC telemetry rendered")
        require(waitFor("Devnet — test funds only, no real value. Test faucet disabled.") != nil, "Live Dev network identity rendered")
        require(waitFor("Connection") != nil, "Dashboard connection action")
        press("Connection")
        require(waitFor("Node connection") != nil, "Connection dialog opened")
        require(waitFor("Node URL") != nil, "Connection input label")
        press("Close")
        require(waitFor("Connection") != nil, "Connection dialog close")
        require(!nodes().contains { matches($0,"Node connection") }, "Connection dialog dismissed")
        print("GUI_PASS dashboard live Dev RPC telemetry/network identity and connection dialog")
    }
}
