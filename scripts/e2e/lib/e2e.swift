// susurro e2e helper. Build + run through ./e2e (compiles once into bin/). Subcommands:
//   front                               frontmost app: bundle id, pid, name (JSON)
//   key <down|up|tap> <key> [mods]      post a key via CGEvent; key = F13..F20, space, escape, return, a-z or a keycode;
//                                       mods = comma list of cmd,alt,ctrl,shift. fn / keycode 63 are refused.
//   axvalue <pid> [window title]        AXValue of the focused UI element of that app; with a title, refuses
//                                       unless the app's focused window has that title (never read Pablo's docs)
//   axclose <pid> <window title>        press the close button of that app's window with that title
//   windows [owner substring]           on-screen windows (id, owner, pid, title, bounds) as JSON
//   shot <window id> <out.png>          screencapture -x -o -l <id>
//   agg create <name> [uid]             public aggregate input device over the default input; prints its UID
//                                       (uid must start with susurro-e2e-; reuse one to re-plug the "same" device)
//   agg destroy <uid>                   destroy an aggregate created by `agg create`
import AppKit
import ApplicationServices
import CoreAudio
import Foundation

func die(_ s: String) -> Never { FileHandle.standardError.write((s + "\n").data(using: .utf8)!); exit(1) }
func json(_ o: Any) { print(String(data: try! JSONSerialization.data(withJSONObject: o, options: [.sortedKeys]), encoding: .utf8)!) }

let a = Array(CommandLine.arguments.dropFirst())
guard let cmd = a.first else { die("usage: see the header of e2e.swift") }

let keys: [String: CGKeyCode] = [
    "f13": 105, "f14": 107, "f15": 113, "f16": 106, "f17": 64, "f18": 79, "f19": 80, "f20": 90,
    "space": 49, "escape": 53, "return": 36, "tab": 48,
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14,
    "r": 15, "y": 16, "t": 17, "o": 31, "u": 32, "i": 34, "p": 35, "l": 37, "j": 38, "k": 40, "n": 45, "m": 46,
]

func audioProp(_ obj: AudioObjectID, _ sel: AudioObjectPropertySelector, _ out: UnsafeMutableRawPointer, _ size: Int) -> OSStatus {
    var addr = AudioObjectPropertyAddress(mSelector: sel, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
    var sz = UInt32(size)
    return AudioObjectGetPropertyData(obj, &addr, 0, nil, &sz, out)
}

func deviceUID(_ id: AudioObjectID) -> String? {
    var uid: Unmanaged<CFString>?
    let st = withUnsafeMutablePointer(to: &uid) { audioProp(id, kAudioDevicePropertyDeviceUID, $0, MemoryLayout<Unmanaged<CFString>?>.size) }
    guard st == 0, let u = uid else { return nil }
    return u.takeRetainedValue() as String
}

switch cmd {
case "front":
    guard let app = NSWorkspace.shared.frontmostApplication else { die("no frontmost app") }
    json(["bundle_id": app.bundleIdentifier ?? "", "pid": app.processIdentifier, "name": app.localizedName ?? ""])

case "key":
    guard a.count >= 3, ["down", "up", "tap"].contains(a[1]) else { die("key <down|up|tap> <key> [mods]") }
    let k = a[2].lowercased()
    if k == "fn" { die("refused: fn is bound by the system") }
    guard let code = keys[k] ?? CGKeyCode(k) else { die("unknown key \(k)") }
    if code == 63 { die("refused: fn is bound by the system") }
    // Modifiers are posted as their own key events first: hotkey matching reads the modifier state, not only event flags.
    var mods: [(CGKeyCode, CGEventFlags)] = []
    for m in (a.count > 3 ? a[3] : "").split(separator: ",") {
        switch m.lowercased() {
        case "cmd": mods.append((55, .maskCommand))
        case "alt", "opt": mods.append((58, .maskAlternate))
        case "ctrl": mods.append((59, .maskControl))
        case "shift": mods.append((56, .maskShift))
        default: die("unknown modifier \(m) (fn is refused)")
        }
    }
    let src = CGEventSource(stateID: .hidSystemState)
    func post(_ code: CGKeyCode, _ down: Bool, _ flags: CGEventFlags) {
        guard let e = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: down) else { die("CGEvent failed") }
        e.flags = flags
        e.post(tap: .cghidEventTap)
        usleep(20_000)
    }
    let all = mods.reduce(CGEventFlags()) { $0.union($1.1) }
    if a[1] != "up" {
        var f = CGEventFlags()
        for (mc, mf) in mods { f.insert(mf); post(mc, true, f) }
        post(code, true, all)
    }
    if a[1] != "down" {
        post(code, false, all)
        var f = all
        for (mc, mf) in mods.reversed() { f.remove(mf); post(mc, false, f) }
    }

case "axvalue":
    guard a.count >= 2, let pid = pid_t(a[1]) else { die("axvalue <pid> [window title]") }
    let app = AXUIElementCreateApplication(pid)
    if a.count > 2 {
        var w: CFTypeRef?, t: CFTypeRef?
        AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute as CFString, &w)
        if let w { AXUIElementCopyAttributeValue(w as! AXUIElement, kAXTitleAttribute as CFString, &t) }
        guard (t as? String) == a[2] else { die("focused window is \((t as? String) ?? "none"), not \(a[2])") }
    }
    var el: CFTypeRef?
    guard AXUIElementCopyAttributeValue(app, kAXFocusedUIElementAttribute as CFString, &el) == .success, let el else { die("no focused element (Accessibility?)") }
    var v: CFTypeRef?
    guard AXUIElementCopyAttributeValue(el as! AXUIElement, kAXValueAttribute as CFString, &v) == .success else { die("focused element has no AXValue") }
    print((v as? String) ?? "\(v!)")

case "axclose":
    guard a.count == 3, let pid = pid_t(a[1]) else { die("axclose <pid> <title>") }
    let app = AXUIElementCreateApplication(pid)
    var ws: CFTypeRef?
    AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &ws)
    for w in (ws as? [AXUIElement]) ?? [] {
        var t: CFTypeRef?
        AXUIElementCopyAttributeValue(w, kAXTitleAttribute as CFString, &t)
        guard (t as? String) == a[2] else { continue }
        var b: CFTypeRef?
        guard AXUIElementCopyAttributeValue(w, kAXCloseButtonAttribute as CFString, &b) == .success, let b else { die("no close button") }
        exit(AXUIElementPerformAction(b as! AXUIElement, kAXPressAction as CFString) == .success ? 0 : 1)
    }
    die("no window titled \(a[2])")

case "windows":
    let filter = a.count > 1 ? a[1].lowercased() : nil
    let list = (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]]) ?? []
    json(list.compactMap { w -> [String: Any]? in
        let owner = w[kCGWindowOwnerName as String] as? String ?? ""
        if let f = filter, !owner.lowercased().contains(f) { return nil }
        return ["id": w[kCGWindowNumber as String] ?? 0, "owner": owner, "pid": w[kCGWindowOwnerPID as String] ?? 0,
                "title": w[kCGWindowName as String] ?? "", "layer": w[kCGWindowLayer as String] ?? 0,
                "bounds": w[kCGWindowBounds as String] ?? [:]]
    })

case "shot":
    guard a.count == 3 else { die("shot <window id> <out.png>") }
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
    p.arguments = ["-x", "-o", "-l", a[1], a[2]]
    try! p.run(); p.waitUntilExit()
    exit(p.terminationStatus)

case "agg":
    guard a.count == 3 || (a.count == 4 && a[1] == "create") else { die("agg create <name> [uid] | agg destroy <uid>") }
    if a[1] == "create" {
        var def = AudioObjectID(0)
        guard audioProp(AudioObjectID(kAudioObjectSystemObject), kAudioHardwarePropertyDefaultInputDevice, &def, 4) == 0, let sub = deviceUID(def) else { die("no default input") }
        let uid = a.count == 4 ? a[3] : "susurro-e2e-\(UUID().uuidString)"
        guard uid.hasPrefix("susurro-e2e-") else { die("refused: uid must start with susurro-e2e-") }
        let desc: [String: Any] = [
            kAudioAggregateDeviceNameKey: a[2], kAudioAggregateDeviceUIDKey: uid,
            kAudioAggregateDeviceIsPrivateKey: 0, // public: the app under test must see it
            kAudioAggregateDeviceSubDeviceListKey: [[kAudioSubDeviceUIDKey: sub]],
            kAudioAggregateDeviceMainSubDeviceKey: sub,
        ]
        var id = AudioObjectID(0)
        let st = AudioHardwareCreateAggregateDevice(desc as CFDictionary, &id)
        guard st == 0 else { die("create failed: \(st)") }
        print(uid)
    } else if a[1] == "destroy" {
        guard a[2].hasPrefix("susurro-e2e-") else { die("refused: only destroys devices made by agg create") }
        var addr = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyTranslateUIDToDevice, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var uid = a[2] as CFString
        var id = AudioObjectID(0)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        let st = withUnsafeMutablePointer(to: &uid) { AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &addr, UInt32(MemoryLayout<CFString>.size), $0, &size, &id) }
        guard st == 0, id != 0 else { die("no device with uid \(a[2])") }
        let d = AudioHardwareDestroyAggregateDevice(id)
        guard d == 0 else { die("destroy failed: \(d)") }
    } else { die("agg create <name> | agg destroy <uid>") }

default:
    die("unknown command \(cmd)")
}
