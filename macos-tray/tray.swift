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
    try? data.write(to: stateURL, options: .atomic) // the CLI may read it concurrently
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

/// nil when nothing is holding; otherwise remaining seconds and whether the
/// screen is kept on (the hold's --display flag).
private func awakeState() -> (remaining: Double?, display: Bool) {
    let url = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("monctl/awake.json")
    guard let s = try? String(contentsOf: url, encoding: .utf8) else { return (nil, false) }
    func num(_ key: String) -> Double? {
        guard let r = s.range(of: "\"\(key)\":") else { return nil }
        let digits = s[r.upperBound...].prefix { $0.isNumber }
        return Double(digits)
    }
    guard let deadline = num("deadline"), let heartbeat = num("heartbeat") else { return (nil, false) }
    if num("release") == 1 { return (nil, false) }
    let now = Date().timeIntervalSince1970
    if now - heartbeat > 15 { return (nil, false) } // stale holder (crash/reboot)
    return (max(0, deadline - now), num("display") == 1)
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
    let hotKeyID = EventHotKeyID(signature: OSType(0x6D6F_6E63) /* "monc" */, id: 1)
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

// MARK: - resolutions (native: includes the HiDPI modes System Settings hides)

private struct ModeChoice {
    let mode: CGDisplayMode
    let title: String
    let current: Bool
}

private func modeChoices(_ id: CGDirectDisplayID) -> [ModeChoice] {
    let opts = [kCGDisplayShowDuplicateLowResolutionModes: kCFBooleanTrue] as CFDictionary
    guard let all = CGDisplayCopyAllDisplayModes(id, opts) as? [CGDisplayMode] else { return [] }
    let cur = CGDisplayCopyDisplayMode(id)
    var seen = Set<String>()
    var out: [ModeChoice] = []
    let sorted = all.filter { $0.isUsableForDesktopGUI() }.sorted {
        ($0.width, $0.height, $0.pixelWidth, $0.refreshRate) > ($1.width, $1.height, $1.pixelWidth, $1.refreshRate)
    }
    for m in sorted {
        let hidpi = m.pixelWidth > m.width
        let hz = Int(m.refreshRate.rounded())
        let key = "\(m.width)x\(m.height)-\(hidpi)-\(hz)"
        if seen.contains(key) { continue }
        seen.insert(key)
        let isCur = cur.map { $0.ioDisplayModeID == m.ioDisplayModeID && $0.pixelWidth == m.pixelWidth && $0.width == m.width } ?? false
        out.append(ModeChoice(mode: m, title: "\(m.width) × \(m.height)\(hz > 0 ? " @\(hz)Hz" : "")\(hidpi ? "  HiDPI" : "")", current: isCur))
    }
    return out
}

private func setMode(_ id: CGDirectDisplayID, _ mode: CGDisplayMode) -> String? {
    var config: CGDisplayConfigRef?
    guard CGBeginDisplayConfiguration(&config) == .success, let cfg = config else { return "CGBeginDisplayConfiguration failed" }
    let err = CGConfigureDisplayWithDisplayMode(cfg, id, mode, nil)
    guard err == .success else { CGCancelDisplayConfiguration(cfg); return "切换失败：\(err.rawValue)" }
    return CGCompleteDisplayConfiguration(cfg, .permanently) == .success ? nil : "CGCompleteDisplayConfiguration failed"
}

// MARK: - monctl CLI (virtual displays, HiDPI, casting live in the CLI)

/// Run the sibling monctl and return (exit code, output). Call off the main thread.
private func monctlRun(_ args: [String]) -> (Int32, String) {
    guard let path = monctlPath() else { return (-1, "monctl not found next to the tray app") }
    let p = Process()
    p.executableURL = URL(fileURLWithPath: path)
    p.arguments = args
    let pipe = Pipe()
    p.standardOutput = pipe
    p.standardError = pipe
    do { try p.run() } catch { return (-1, error.localizedDescription) }
    let data = pipe.fileHandleForReading.readDataToEndOfFile()
    p.waitUntilExit()
    return (p.terminationStatus, String(decoding: data, as: UTF8.self))
}

private struct VirtualInfo {
    let id: String
    let name: String
}

/// Live virtual displays from `monctl virtual`: lines like `12 "name" (2560x1440 @60Hz, HiDPI)`.
private func virtualDisplays() -> [VirtualInfo] {
    let (code, out) = monctlRun(["virtual"])
    guard code == 0 else { return [] }
    return out.split(separator: "\n").compactMap { line in
        let parts = line.split(separator: "\"", omittingEmptySubsequences: false)
        guard parts.count >= 3, let id = parts[0].split(separator: " ").first else { return nil }
        return VirtualInfo(id: String(id), name: String(parts[1]))
    }
}

// MARK: - tablet casting (`monctl cast` as a child process)

private final class Caster {
    private(set) var process: Process?
    private(set) var url: String?
    private(set) var tablets = 0
    private var buffer = ""
    var onChange: () -> Void = {}

    var running: Bool { process?.isRunning ?? false }

    func start() -> String? {
        guard let path = monctlPath() else { return "找不到 monctl（应与托盘程序放在同一目录）" }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = ["cast"]
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = pipe
        pipe.fileHandleForReading.readabilityHandler = { [weak self] h in
            let chunk = String(decoding: h.availableData, as: UTF8.self)
            DispatchQueue.main.async { self?.consume(chunk) }
        }
        p.terminationHandler = { [weak self] _ in
            DispatchQueue.main.async {
                self?.process = nil
                self?.url = nil
                self?.tablets = 0
                self?.onChange()
            }
        }
        do { try p.run() } catch { return "启动投屏失败：\(error.localizedDescription)" }
        process = p
        return nil
    }

    func stop() {
        process?.terminate() // workers notice their server is gone and end, taking the tablet screens along
    }

    private func consume(_ chunk: String) {
        buffer += chunk
        while let nl = buffer.firstIndex(of: "\n") {
            let line = String(buffer[..<nl])
            buffer.removeSubrange(...nl)
            let t = line.trimmingCharacters(in: .whitespaces)
            if url == nil, t.hasPrefix("http://") { url = t }
            if line.hasPrefix("+ ") { tablets += 1 }
            if line.hasPrefix("- "), tablets > 0 { tablets -= 1 }
        }
        onChange()
    }
}

/// Floating panel with the QR code the tablet camera opens directly.
private final class QRPanel {
    private var panel: NSPanel?

    func show(url: String) {
        let size: CGFloat = 260
        let p = panel ?? NSPanel(contentRect: NSRect(x: 0, y: 0, width: size + 40, height: size + 110),
                                 styleMask: [.titled, .closable, .utilityWindow], backing: .buffered, defer: false)
        p.title = "投屏到平板"
        p.isFloatingPanel = true
        p.hidesOnDeactivate = false
        let view = NSView(frame: p.contentRect(forFrameRect: p.frame))
        let image = NSImageView(frame: NSRect(x: 20, y: 90, width: size, height: size))
        image.image = qrImage(url, size: size)
        image.imageScaling = .scaleNone
        view.addSubview(image)
        let label = NSTextField(wrappingLabelWithString: "\(url)\n用平板相机扫码，或在浏览器打开。\n安卓平板插 USB 线（已开 USB 调试）会自动打开。")
        label.frame = NSRect(x: 20, y: 12, width: size, height: 72)
        label.alignment = .center
        label.isSelectable = true
        view.addSubview(label)
        p.contentView = view
        p.center()
        panel = p
        NSApp.activate(ignoringOtherApps: true)
        p.makeKeyAndOrderFront(nil)
    }

    func close() { panel?.orderOut(nil) }

    private func qrImage(_ text: String, size: CGFloat) -> NSImage? {
        guard let f = CIFilter(name: "CIQRCodeGenerator") else { return nil }
        f.setValue(Data(text.utf8), forKey: "inputMessage")
        f.setValue("M", forKey: "inputCorrectionLevel")
        guard let ci = f.outputImage else { return nil }
        let scale = floor(size / ci.extent.width)
        let scaled = ci.transformed(by: CGAffineTransform(scaleX: scale, y: scale)) // nearest: crisp modules
        let rep = NSCIImageRep(ciImage: scaled)
        let img = NSImage(size: rep.size)
        img.addRepresentation(rep)
        return img
    }
}

// MARK: - menu bar UI

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private let statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    private let menu = NSMenu()
    private let caster = Caster()
    private let qr = QRPanel()
    private var virtuals: [VirtualInfo] = []
    private var shownQRFor: String?

    func applicationWillTerminate(_ notification: Notification) {
        caster.stop()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        if let button = statusItem.button {
            button.image = NSImage(systemSymbolName: "display.2", accessibilityDescription: "monctl")
            button.image?.isTemplate = true
        }
        menu.autoenablesItems = false
        menu.delegate = self
        statusItem.menu = menu
        caster.onChange = { [weak self] in
            guard let self else { return }
            // pop the QR code up as soon as the server prints its address
            if let url = self.caster.url, self.shownQRFor != url {
                self.shownQRFor = url
                self.qr.show(url: url)
            }
            if !self.caster.running { self.shownQRFor = nil; self.qr.close() }
            self.rebuildMenu()
        }
        refreshVirtuals()
        rebuildMenu()
        installRecoveryHotkey()
    }

    /// `monctl virtual` spawns a process: keep it off the menu-opening path.
    private func refreshVirtuals() {
        DispatchQueue.global().async {
            let v = virtualDisplays()
            DispatchQueue.main.async { self.virtuals = v; self.rebuildMenu() }
        }
    }

    /// Run monctl in the background, alert on failure, then refresh.
    private func monctlAsync(_ args: [String]) {
        DispatchQueue.global().async {
            let (code, out) = monctlRun(args)
            let v = virtualDisplays()
            DispatchQueue.main.async {
                self.virtuals = v
                if code != 0 { self.alert(out.trimmingCharacters(in: .whitespacesAndNewlines)) }
                self.rebuildMenu()
            }
        }
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        rebuildMenu()
        refreshVirtuals()
    }

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
        let activeNames = Set(displays.filter(\.active).map(\.name))
        for (id, name) in remembered.sorted(by: { $0.key < $1.key })
        where !cgIDs.contains(id) && !activeNames.contains(name) {
            // macOS display ids shift across reboots/reconnects; a remembered
            // entry whose name matches a display that's active right now is
            // the same panel under a new id, not a second screen. The state
            // entry stays so 全部开启 ⌃⌥⇧D still tries it.
            addItem("开启  \(name)", action: #selector(toggleItem(_:)), target: self, enabled: true, id: id)
        }
        menu.addItem(.separator())
        addItem("只留主屏", action: #selector(soloMain), target: self, enabled: activeCount > 1)
        addItem("全部开启 ⌃⌥⇧D", action: #selector(allOn), target: self, enabled: activeCount < displays.count || !remembered.isEmpty)
        menu.addItem(.separator())
        addDisplayTools(displays.filter(\.active))
        menu.addItem(.separator())
        if let monctl = monctlPath() {
            let (remaining, displayHold) = awakeState()
            let keep = NSMenuItem(title: "屏幕常亮", action: #selector(toggleKeepDisplay(_:)), keyEquivalent: "")
            keep.target = self
            keep.state = displayHold ? .on : .off
            menu.addItem(keep)
            let awakeTitle: String
            if let r = remaining {
                awakeTitle = r > 5 * 365 * 24 * 3600
                    ? "保持唤醒（不限时\(displayHold ? "，屏幕常亮" : "")）"
                    : "保持唤醒（剩余 \(fmtRemaining(r))\(displayHold ? "，屏幕常亮" : "")）"
            } else {
                awakeTitle = "保持唤醒"
            }
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

    private func submenuItem(_ title: String, _ sub: NSMenu) -> NSMenuItem {
        let it = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        it.submenu = sub
        return it
    }

    private func subItem(_ sub: NSMenu, _ title: String, _ action: Selector, _ obj: Any? = nil, checked: Bool = false, enabled: Bool = true) {
        let it = NSMenuItem(title: title, action: action, keyEquivalent: "")
        it.target = self
        it.representedObject = obj
        it.state = checked ? .on : .off
        it.isEnabled = enabled
        sub.addItem(it)
    }

    /// 分辨率 / HiDPI / 虚拟显示器 / 投屏到平板
    private func addDisplayTools(_ active: [DisplayInfo]) {
        // resolutions: one submenu per display (flat when there is just one)
        let res = NSMenu()
        for d in active {
            let target: NSMenu
            if active.count == 1 {
                target = res
            } else {
                target = NSMenu()
                res.addItem(submenuItem(d.name, target))
            }
            for c in modeChoices(d.id).prefix(40) {
                subItem(target, c.title, #selector(pickMode(_:)), [NSNumber(value: d.id), c.mode] as NSArray, checked: c.current)
            }
        }
        menu.addItem(submenuItem("分辨率", res))

        // HiDPI on real screens (macOS refuses it on most non-Retina monitors)
        let hidpi = NSMenu()
        let hidpiOn = Set(virtuals.compactMap { $0.name.hasPrefix("HiDPI ") ? String($0.name.dropFirst(6)) : nil })
        // monctl's own virtual screens (vendor "mn", product "vc" - see src/virt.rs)
        // are canvases already, HiDPI on them makes no sense
        for d in active where !(CGDisplayVendorNumber(d.id) == 0x6d6e && CGDisplayModelNumber(d.id) == 0x7663) {
            subItem(hidpi, d.name, #selector(toggleHiDPI(_:)), String(d.id), checked: hidpiOn.contains(String(d.id)))
        }
        if hidpi.items.isEmpty { subItem(hidpi, "没有可用的屏幕", #selector(noop), enabled: false) }
        menu.addItem(submenuItem("HiDPI 清晰模式", hidpi))

        // virtual displays
        let virt = NSMenu()
        for (label, size) in [("新建 1920 × 1080", "1920x1080"), ("新建 2560 × 1440", "2560x1440"), ("新建 3840 × 2160 (4K)", "3840x2160")] {
            subItem(virt, label, #selector(addVirtual(_:)), size)
        }
        let mine = virtuals.filter { !$0.name.hasPrefix("HiDPI ") }
        if !mine.isEmpty {
            virt.addItem(.separator())
            for v in mine { subItem(virt, "移除 \(v.name)", #selector(removeVirtual(_:)), v.id) }
            subItem(virt, "全部移除", #selector(removeVirtual(_:)), "all")
        }
        menu.addItem(submenuItem("虚拟显示器", virt))

        // tablets
        if caster.running {
            let n = caster.tablets
            addItem(n > 0 ? "停止投屏（已连接 \(n) 台平板）" : "停止投屏（等待平板连接）", action: #selector(toggleCast), target: self)
            addItem("显示投屏二维码…", action: #selector(showQR), target: self, enabled: caster.url != nil)
        } else {
            addItem("投屏到平板…", action: #selector(toggleCast), target: self)
        }
    }

    @objc private func noop() {}

    @objc private func pickMode(_ sender: NSMenuItem) {
        guard let pair = sender.representedObject as? NSArray, pair.count == 2,
              let id = (pair[0] as? NSNumber)?.uint32Value else { return }
        let mode = pair[1] as! CGDisplayMode
        if let err = setMode(id, mode) { alert(err) }
        rebuildMenu()
    }

    @objc private func toggleHiDPI(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        monctlAsync(sender.state == .on ? ["hidpi", "off", id] : ["hidpi", id])
    }

    @objc private func addVirtual(_ sender: NSMenuItem) {
        guard let size = sender.representedObject as? String else { return }
        monctlAsync(["virtual", "add", size])
    }

    @objc private func removeVirtual(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        monctlAsync(["virtual", "rm", id])
    }

    @objc private func toggleCast() {
        if caster.running {
            caster.stop()
        } else if let err = caster.start() {
            alert(err)
        }
        rebuildMenu()
    }

    @objc private func showQR() {
        if let url = caster.url { qr.show(url: url) }
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

    /// One-click "keep the screen on": an unlimited awake hold with --display,
    /// released the same way. Shares the single hold with 保持唤醒.
    @objc private func toggleKeepDisplay(_ sender: NSMenuItem) {
        let (_, displayHold) = awakeState()
        runMonctlAwake(displayHold ? ["off"] : ["--display"])
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
