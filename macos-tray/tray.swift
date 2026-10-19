// monctl-tray — menu bar app to toggle individual displays on/off
//
// Same private API as the monctl CLI (CGSConfigureDisplayEnabled); shares its
// state file (~/Library/Application Support/monctl/state.json) so the two stay
// in sync. A disabled display vanishes from the system like an unplugged cable.
//
// Build:  swiftc -O tray.swift -o ~/.local/bin/monctl-tray

import AppKit
import Carbon
import CoreGraphics
import Darwin

// MARK: - private APIs (resolved at runtime)

private func privateFn<T>(_ name: String) -> T? {
    guard let sym = dlsym(dlopen(nil, RTLD_NOW), name) else { return nil }
    return unsafeBitCast(sym, to: T.self)
}
private typealias CGSSetDisplayEnabledFn = @convention(c) (OpaquePointer, UInt32, Bool) -> Int32
private typealias CoreDisplayInfoFn = @convention(c) (UInt32) -> Unmanaged<CFDictionary>?

// MARK: - display enumeration

private struct DisplayInfo {
    let id: CGDirectDisplayID
    let active: Bool
    let isMain: Bool
    let name: String
}

private func displayName(_ id: CGDirectDisplayID) -> String {
    guard let fn: CoreDisplayInfoFn = privateFn("CoreDisplay_DisplayCreateInfoDictionary"),
          let ref = fn(id) else { return "Display \(id)" }
    let info = ref.takeRetainedValue() as NSDictionary
    let raw = info["DisplayProductName"] ?? info["kDisplayProductName"]
    if let s = raw as? String, !s.isEmpty { return s }
    if let d = raw as? [String: String], let s = d.values.sorted().first, !s.isEmpty { return s }
    return "Display \(id)"
}

private func onlineDisplays() -> [DisplayInfo] {
    var count: UInt32 = 0
    CGGetOnlineDisplayList(0, nil, &count)
    guard count > 0 else { return [] }
    var ids = [CGDirectDisplayID](repeating: 0, count: Int(count))
    CGGetOnlineDisplayList(count, &ids, &count)
    return ids.prefix(Int(count)).map { id in
        DisplayInfo(id: id, active: CGDisplayIsActive(id) != 0, isMain: CGDisplayIsMain(id) != 0, name: displayName(id))
    }
}

// MARK: - shared state file (id -> name of displays monctl turned off)

private let stateURL = FileManager.default
    .urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    .appendingPathComponent("monctl/state.json")

private func loadState() -> [CGDirectDisplayID: String] {
    guard let data = try? Data(contentsOf: stateURL),
          let raw = try? JSONDecoder().decode([String: String].self, from: data) else { return [:] }
    var result: [CGDirectDisplayID: String] = [:]
    for (k, v) in raw { if let id = UInt32(k) { result[id] = v } }
    return result
}

private func saveState(_ ids: [CGDirectDisplayID: String]) {
    let raw = Dictionary(uniqueKeysWithValues: ids.map { (String($0.key), $0.value) })
    let data = (try? JSONEncoder().encode(raw)) ?? Data("{}".utf8)
    try? FileManager.default.createDirectory(at: stateURL.deletingLastPathComponent(), withIntermediateDirectories: true)
    try? data.write(to: stateURL)
}

// MARK: - core operation

private func setDisplayEnabled(_ id: CGDirectDisplayID, _ enabled: Bool) -> String? {
    guard let fn: CGSSetDisplayEnabledFn = privateFn("CGSConfigureDisplayEnabled") else {
        return "CGSConfigureDisplayEnabled not found on this macOS version"
    }
    var config: CGDisplayConfigRef?
    var err = CGBeginDisplayConfiguration(&config)
    guard err == .success else { return "CGBeginDisplayConfiguration failed: \(err.rawValue)" }
    guard let cfg = config else { return "display config is nil" }
    err = CGError(rawValue: fn(cfg, id, enabled))!
    guard err == .success else {
        CGCancelDisplayConfiguration(cfg)
        return "CGSConfigureDisplayEnabled failed: \(err.rawValue)"
    }
    err = CGCompleteDisplayConfiguration(cfg, .forSession)
    guard err == .success else { return "CGCompleteDisplayConfiguration failed: \(err.rawValue)" }
    return nil
}

// MARK: - launch at login (LaunchAgent)

private let agentLabel = "cn.williamwong.monctl-tray"

private func binaryPath() -> String {
    let fm = FileManager.default
    if let exe = Bundle.main.executablePath, fm.isExecutableFile(atPath: exe) { return exe }
    let arg0 = CommandLine.arguments.first ?? "monctl-tray"
    return arg0.hasPrefix("/") ? arg0 : fm.currentDirectoryPath + "/" + arg0
}

private func agentPlistURL() -> URL {
    FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("LaunchAgents/\(agentLabel).plist")
}

private func agentInstalled() -> Bool {
    FileManager.default.fileExists(atPath: agentPlistURL().path)
}

@discardableResult
private func launchctl(_ args: [String]) -> String? {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/bin/launchctl")
    p.arguments = args
    let pipe = Pipe()
    p.standardOutput = pipe
    p.standardError = pipe
    do { try p.run() } catch { return nil }
    p.waitUntilExit()
    return String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8)
}

private func agentRunsMe() -> Bool {
    let out = launchctl(["print", "gui/\(getuid())/\(agentLabel)"]) ?? ""
    return out.contains("pid = \(getpid())")
}

private func writeAgentPlist() throws {
    let plist = """
    <?xml version="1.0" encoding="UTF-8"?>
    <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
    <plist version="1.0">
    <dict>
        <key>Label</key>
        <string>\(agentLabel)</string>
        <key>ProgramArguments</key>
        <array>
            <string>\(binaryPath())</string>
        </array>
        <key>RunAtLoad</key>
        <true/>
        <key>KeepAlive</key>
        <dict>
            <key>SuccessfulExit</key>
            <false/>
        </dict>
    </dict>
    </plist>
    """
    try FileManager.default.createDirectory(at: agentPlistURL().deletingLastPathComponent(), withIntermediateDirectories: true)
    try plist.write(to: agentPlistURL(), atomically: true, encoding: .utf8)
}

// MARK: - keep awake (delegates to the monctl CLI's `awake` holder)

private func monctlPath() -> String? {
    let dir = (binaryPath() as NSString).deletingLastPathComponent
    let sibling = dir + "/monctl"
    if FileManager.default.isExecutableFile(atPath: sibling) { return sibling }
    return nil
}

private func runMonctlAwake(_ args: [String]) {
    guard let monctl = monctlPath() else { return }
    let p = Process()
    p.executableURL = URL(fileURLWithPath: monctl)
    p.arguments = ["awake"] + args
    p.standardOutput = Pipe()
    p.standardError = Pipe()
    try? p.run()
}

/// nil when nothing is holding; otherwise remaining seconds.
private func awakeRemaining() -> Double? {
    let url = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("monctl/awake.json")
    guard let s = try? String(contentsOf: url, encoding: .utf8) else { return nil }
    func num(_ key: String) -> Double? {
        guard let r = s.range(of: "\"\(key)\":") else { return nil }
        let digits = s[r.upperBound...].prefix { $0.isNumber }
        return Double(digits)
    }
    guard let deadline = num("deadline"), let heartbeat = num("heartbeat") else { return nil }
    if num("release") == 1 { return nil }
    let now = Date().timeIntervalSince1970
    if now - heartbeat > 15 { return nil } // stale holder (crash/reboot)
    return max(0, deadline - now)
}

private func fmtRemaining(_ secs: Double) -> String {
    let s = Int(secs)
    if s >= 3600 { return "\(s / 3600)h\(String(format: "%02d", (s % 3600) / 60))m" }
    if s >= 60 { return "\(s / 60)m\(String(format: "%02d", s % 60))s" }
    return "\(s)s"
}

// MARK: - global recovery hotkey (Ctrl+Option+Shift+D = turn every display on)

private func enableAllDisplaysCore() {
    let displays = onlineDisplays()
    var ids = Set(displays.filter { !$0.active }.map { $0.id })
    ids.formUnion(loadState().keys)
    ids.subtract(displays.filter { $0.active }.map { $0.id })
    for id in ids.sorted() {
        if setDisplayEnabled(id, true) == nil {
            var st = loadState()
            st.removeValue(forKey: id)
            saveState(st)
        }
    }
}

private func installRecoveryHotkey() {
    var hotKeyRef: EventHotKeyRef?
    var hotKeyID = EventHotKeyID(signature: OSType(0x6D6F_6E63) /* "monc" */, id: 1)
    var eventType = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
    let callback: EventHandlerUPP = { _, event, _ -> OSStatus in
        var id = EventHotKeyID()
        guard let event else { return noErr }
        GetEventParameter(event, EventParamName(kEventParamDirectObject), EventParamType(typeEventHotKeyID), nil, MemoryLayout<EventHotKeyID>.size, nil, &id)
        if id.id == 1 {
            enableAllDisplaysCore()
        }
        return noErr
    }
    InstallEventHandler(GetApplicationEventTarget(), callback, 1, &eventType, nil, nil)
    // kVK_ANSI_D = 2; controlKey | optionKey | shiftKey
    RegisterEventHotKey(2, UInt32(controlKey | optionKey | shiftKey), hotKeyID, GetApplicationEventTarget(), 0, &hotKeyRef)
}

// MARK: - menu bar UI

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private let statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    private let menu = NSMenu()

    func applicationDidFinishLaunching(_ notification: Notification) {
        if let button = statusItem.button {
            button.image = NSImage(systemSymbolName: "display.2", accessibilityDescription: "monctl")
            button.image?.isTemplate = true
        }
        menu.autoenablesItems = false
        menu.delegate = self
        statusItem.menu = menu
        rebuildMenu()
        installRecoveryHotkey()
    }

    func menuNeedsUpdate(_ menu: NSMenu) { rebuildMenu() }

    private func addItem(_ title: String, action: Selector, target: AnyObject? = nil, enabled: Bool = true, id: CGDirectDisplayID? = nil) {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        item.target = target
        item.isEnabled = enabled
        if let id { item.representedObject = NSNumber(value: id) }
        menu.addItem(item)
    }

    private func rebuildMenu() {
        menu.removeAllItems()
        let displays = onlineDisplays()
        let activeCount = displays.filter(\.active).count

        for d in displays where d.active {
            var title = "关闭  \(d.name)"
            if d.isMain { title += "（主屏）" }
            if activeCount <= 1 { title += " — 最后一块屏，不能关" }
            addItem(title, action: #selector(toggleItem(_:)), target: self, enabled: activeCount > 1, id: d.id)
        }
        let remembered = loadState()
        let cgIDs = Set(displays.map(\.id))
        for (id, name) in remembered.sorted(by: { $0.key < $1.key }) where !cgIDs.contains(id) {
            addItem("开启  \(name)", action: #selector(toggleItem(_:)), target: self, enabled: true, id: id)
        }
        menu.addItem(.separator())
        addItem("只留主屏", action: #selector(soloMain), target: self, enabled: activeCount > 1)
        addItem("全部开启 ⌃⌥⇧D", action: #selector(allOn), target: self, enabled: activeCount < displays.count || !remembered.isEmpty)
        menu.addItem(.separator())
        if let monctl = monctlPath() {
            let remaining = awakeRemaining()
            let awakeTitle = remaining != nil
                ? "保持唤醒（剩余 \(fmtRemaining(remaining!))）"
                : "保持唤醒"
            let awakeItem = NSMenuItem(title: awakeTitle, action: nil, keyEquivalent: "")
            let sub = NSMenu()
            for (label, secs) in [("1 小时", "3600"), ("4 小时", "14400"), ("8 小时", "28800"), ("不限时", "")] {
                let it = NSMenuItem(title: label, action: #selector(awakeFor(_:)), keyEquivalent: "")
                it.target = self
                it.isEnabled = remaining == nil || secs == ""
                it.representedObject = secs as NSString
                sub.addItem(it)
            }
            sub.addItem(.separator())
            let off = NSMenuItem(title: "解除保持", action: #selector(awakeFor(_:)), keyEquivalent: "")
            off.target = self
            off.isEnabled = remaining != nil
            off.representedObject = "off" as NSString
            sub.addItem(off)
            awakeItem.submenu = sub
            menu.addItem(awakeItem)
            _ = monctl
        }
        let autoStart = NSMenuItem(title: "开机启动", action: #selector(toggleAutoStart(_:)), keyEquivalent: "")
        autoStart.target = self
        autoStart.state = agentInstalled() ? .on : .off
        menu.addItem(autoStart)
        addItem("退出", action: #selector(NSApplication.terminate(_:)))
    }

    @objc private func toggleItem(_ sender: NSMenuItem) {
        guard let id = (sender.representedObject as? NSNumber)?.uint32Value else { return }
        let displays = onlineDisplays()
        let turningOff = displays.contains(where: { $0.id == id && $0.active })
        if turningOff {
            guard displays.filter(\.active).count > 1 else { return alert("不能关掉最后一块屏") }
            if let err = setDisplayEnabled(id, false) { return alert("关闭失败：\(err)") }
            var state = loadState()
            state[id] = displays.first(where: { $0.id == id })?.name ?? "Display \(id)"
            saveState(state)
        } else {
            if let err = setDisplayEnabled(id, true) { return alert("开启失败：\(err)") }
            var state = loadState()
            state.removeValue(forKey: id)
            saveState(state)
        }
        statusItem.menu?.cancelTracking()
        rebuildMenu()
    }

    @objc private func soloMain() {
        let displays = onlineDisplays()
        for d in displays where d.active && !d.isMain {
            if let err = setDisplayEnabled(d.id, false) { return alert("关闭失败：\(err)") }
            var state = loadState()
            state[d.id] = d.name
            saveState(state)
        }
        statusItem.menu?.cancelTracking()
        rebuildMenu()
    }

    @objc private func allOn() {
        enableAllDisplaysCore()
        statusItem.menu?.cancelTracking()
        rebuildMenu()
    }

    private func setAutoStart(_ on: Bool) {
        let domain = "gui/\(getuid())"
        if on {
            do { try writeAgentPlist() } catch { return alert("写入 LaunchAgent 失败：\(error.localizedDescription)") }
            launchctl(["bootstrap", domain, agentPlistURL().path])
            if !agentRunsMe() {
                // this instance wasn't started by launchd: hand over to the agent copy
                launchctl(["kickstart", "-k", "\(domain)/\(agentLabel)"])
                exit(0)
            }
        } else {
            let wasMe = agentRunsMe()
            try? FileManager.default.removeItem(at: agentPlistURL()) // menu state first
            if wasMe {
                // bootout kills the launchd instance (us): leave a free-standing copy behind
                let p = Process()
                p.executableURL = URL(fileURLWithPath: binaryPath())
                try? p.run()
            }
            launchctl(["bootout", "\(domain)/\(agentLabel)"])
        }
    }

    @objc private func awakeFor(_ sender: NSMenuItem) {
        guard let arg = (sender.representedObject as? String) else { return }
        runMonctlAwake(arg == "" ? [] : [arg])
        statusItem.menu?.cancelTracking()
        rebuildMenu()
    }

    @objc private func toggleAutoStart(_ sender: NSMenuItem) {
        setAutoStart(sender.state != .on)
        rebuildMenu()
    }

    private func alert(_ msg: String) {
        let a = NSAlert()
        a.messageText = "monctl"
        a.informativeText = msg
        a.runModal()
    }
}

// MARK: - main

let app = NSApplication.shared
app.setActivationPolicy(.accessory)   // no Dock icon, no bundle needed
let delegate = AppDelegate()
app.delegate = delegate
app.run()
