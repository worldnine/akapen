// ime — macOS input-source (IME) control CLI.
//
// Carbon TIS API; no accessibility permission required.
//
//   ime get                       print the current input-source ID
//   ime list                      list enabled sources (ID<TAB>name)
//   ime abc                       select the ABC layout (US fallback)
//   ime jp                        select the first enabled Japanese source
//   ime set <id>                  select the source with the given ID
//   ime watch <abc|jp|set <id>> --app <bundle-id|process-name>
//                                 run forever; every time the given app
//                                 becomes frontmost, perform the switch.
//                                 (e.g. force 英数 when the terminal that
//                                 hosts herdr gains focus)
//   ime guard <abc|jp|set <id>> --app <bundle-id> [--interval <secs>]
//                                 [--suspend-file <path>] [--verbose]
//                                 run forever; while the given app is
//                                 frontmost, keep re-applying the switch so
//                                 the input source never stays Japanese
//                                 (undoes herdr's restore-after-prefix, etc.)
//   ime guard suspend|resume|status [--suspend-file <path>]
//                                 pause/resume the guard from another tool
//                                 (e.g. a composer that needs Japanese);
//                                 the guard skips all switching while the
//                                 suspend file exists
//
// Exit status: 0 on success; 1 when the requested source does not exist;
// 2 on unknown commands or bad usage. `ime watch`/`ime guard` run until
// killed.
//
// akapen embeds this file (src/ime.rs, include_str!) and builds it into
// ~/.cache/akapen/ime-<hash>; `scripts/install-ime.sh` installs the same
// source as a standalone `ime` binary for other tools (focus watchers,
// plugin wrappers, keybindings, ...).
import AppKit
import Carbon
import Darwin

func currentID() -> String {
    guard let s = TISCopyCurrentKeyboardInputSource()?.takeRetainedValue() else { return "" }
    if let p = TISGetInputSourceProperty(s, kTISPropertyInputSourceID) {
        return Unmanaged<CFString>.fromOpaque(p).takeUnretainedValue() as String
    }
    return ""
}

func id(_ s: TISInputSource) -> String {
    if let p = TISGetInputSourceProperty(s, kTISPropertyInputSourceID) {
        return Unmanaged<CFString>.fromOpaque(p).takeUnretainedValue() as String
    }
    return ""
}

func name(_ s: TISInputSource) -> String {
    if let p = TISGetInputSourceProperty(s, kTISPropertyLocalizedName) {
        return Unmanaged<CFString>.fromOpaque(p).takeUnretainedValue() as String
    }
    return ""
}

func languages(_ s: TISInputSource) -> [String] {
    guard let p = TISGetInputSourceProperty(s, kTISPropertyInputSourceLanguages),
          let arr = Unmanaged<CFArray>.fromOpaque(p).takeUnretainedValue() as? [String] else { return [] }
    return arr
}

func enabledSources() -> [TISInputSource] {
    guard let list = TISCreateInputSourceList(
        [kTISPropertyInputSourceIsEnabled: kCFBooleanTrue] as CFDictionary, false)?
        .takeRetainedValue() as? [TISInputSource] else { return [] }
    return list
}

func findSource(id target: String) -> TISInputSource? {
    guard let list = TISCreateInputSourceList(
        [kTISPropertyInputSourceID: target] as CFDictionary, false)?
        .takeRetainedValue() as? [TISInputSource] else { return nil }
    return list.first
}

/// The first enabled source for Japanese. Sources are tried in order of
/// decreasing confidence, so `ime jp` lands in kana mode rather than in an
/// IME's ascii mode when both exist:
///   1. "ja" language + kana-mode marker (`Japanese` in the ID: Kotoeri
///      Hiragana, Google base, azooKey 日本語, Gyaim, ...)
///   2. "ja" language, skipping known ascii-mode IDs
///   3. any "ja" source (e.g. AquaSKK, whose single mode has no marker)
///   4. legacy substring heuristic as a last resort
func findJapanese() -> TISInputSource? {
    let sources = enabledSources()
    let isAsciiMode: (TISInputSource) -> Bool = {
        let i = id($0)
        if i.contains("RomajiTyping") && !i.hasSuffix(".Japanese") { return true }
        return i.hasSuffix(".Roman") || i.hasSuffix(".Romaji")
    }
    for s in sources where languages(s).contains("ja") && id(s).contains("Japanese") { return s }
    for s in sources where languages(s).contains("ja") && !isAsciiMode(s) { return s }
    for s in sources where languages(s).contains("ja") { return s }
    for s in sources {
        let i = id(s)
        if i.contains("Japanese") || i.contains("Kotoeri") || i.contains("google")
            || i.contains("atok") || i.contains("justsystems") {
            return s
        }
    }
    return nil
}

/// Select a source and wait (briefly, polling) for the switch to land —
/// the activation is async, and callers (focus hooks, wrappers that
/// immediately hand over to a tool reading keys) want the new state
/// before they continue. Returns as soon as the source matches; the poll
/// never burns more than ~0.6 s even if the switch is silently ignored.
/// For CJKV targets the macism-style engagement workaround runs first
/// (see [`engageCjkInputSource`]), because macOS often leaves the input
/// method disengaged after a bare TIS selection.
func select(_ s: TISInputSource, expect: String) {
    if currentID() == expect { return }
    TISSelectInputSource(s)
    if isCJKV(s) {
        engageCjkInputSource()
    }
    let deadline = Date().addingTimeInterval(0.6)
    while Date() < deadline {
        if currentID() == expect { return }
        usleep(20_000)
    }
}

func asciiSource() -> TISInputSource? {
    findSource(id: "com.apple.keylayout.ABC") ?? findSource(id: "com.apple.keylayout.US")
}

/// True for CJKV input methods (Japanese, Chinese, Korean, Vietnamese).
/// These are the sources affected by the TIS engagement bug (see
/// [`engageCjkInputSource`]).
func isCJKV(_ s: TISInputSource) -> Bool {
    let langs = languages(s)
    guard let first = langs.first else { return false }
    return first == "ja" || first == "ko" || first == "vi" || first.hasPrefix("zh")
}

/// Workaround for a long-standing macOS bug: `TISSelectInputSource` often
/// only changes the menubar icon for CJKV sources without actually engaging
/// the input method — the source reads Japanese while typing still produces
/// ASCII (see Karabiner-Elements#1602). The fix (same as laishulu/macism):
/// briefly become the active app with a key window so the input method
/// engages, then hand focus back by exiting. No accessibility permission
/// needed. `IME_ENGAGE_WAIT_MS` tunes the hold time (150 ms is the smallest
/// fully-stable value on macOS 26; 0 disables the workaround).
func engageCjkInputSource() {
    let waitMs = ProcessInfo.processInfo.environment["IME_ENGAGE_WAIT_MS"]
        .flatMap { Int($0) } ?? 150
    if waitMs <= 0 { return }
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    guard let screen = NSScreen.main else { return }
    let f = screen.visibleFrame
    let win = NSWindow(
        contentRect: NSRect(x: f.maxX - 11, y: f.minY + 8, width: 3, height: 3),
        styleMask: [.titled], // titled or isKeyWindow can't become true
        backing: .buffered,
        defer: false)
    win.isOpaque = true
    win.backgroundColor = NSColor.purple
    win.titlebarAppearsTransparent = true
    win.level = .screenSaver
    win.collectionBehavior = [.canJoinAllSpaces, .stationary]
    win.makeKeyAndOrderFront(nil)
    app.activate(ignoringOtherApps: true)
    // Run the event loop just long enough for the input method to engage,
    // then stop (not terminate) so the caller can finish and exit.
    DispatchQueue.main.asyncAfter(deadline: .now() + Double(waitMs) / 1000.0) {
        app.stop(nil)
    }
    app.run()
}

/// Default guard suspend marker. `ime guard` skips all switching while this
/// file exists; akapen's comment composer creates it for the duration of the
/// composer so Japanese comments still work under a running guard.
func defaultSuspendPath() -> String {
    if let env = ProcessInfo.processInfo.environment["IME_GUARD_SUSPEND_FILE"], !env.isEmpty {
        return env
    }
    let home = ProcessInfo.processInfo.environment["HOME"] ?? "."
    return "\(home)/.cache/ime-guard.suspend"
}

/// Per-pane IME state directory (see `ime pane-focus`). Override with
/// `IME_PANE_STATE_DIR` (tests and custom setups).
func paneStateDir() -> String {
    if let env = ProcessInfo.processInfo.environment["IME_PANE_STATE_DIR"], !env.isEmpty {
        return env
    }
    let home = ProcessInfo.processInfo.environment["HOME"] ?? "."
    return "\(home)/.cache/ime-panes"
}

/// Build the switch closure for a positional mode spec ("abc", "jp", or
/// "set <id>"). Nil when the spec is not a known mode.
func applier(for spec: [String]) -> (() -> Bool)? {
    switch spec.first ?? "" {
    case "abc":
        return {
            guard let s = asciiSource() else { return false }
            select(s, expect: id(s))
            return true
        }
    case "jp":
        return {
            guard let s = findJapanese() else { return false }
            select(s, expect: id(s))
            return true
        }
    case "set":
        guard spec.count > 1 else { return nil }
        return {
            guard let s = findSource(id: spec[1]) else { return false }
            select(s, expect: spec[1])
            return true
        }
    default:
        return nil
    }
}

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write((msg + "\n").data(using: .utf8)!)
    exit(1)
}

func usage() -> Never {
    FileHandle.standardError.write(
        ("usage: ime get|list|abc|jp|set <id>|watch <abc|jp|set <id>> --app <bundle-id>\n")
            .data(using: .utf8)!)
    exit(2)
}

let args = CommandLine.arguments
let cmd = args.count > 1 ? args[1] : "get"

switch cmd {
case "get":
    print(currentID())
case "abc":
    guard let s = asciiSource() else {
        fail("ime: no ASCII layout (ABC/US) enabled")
    }
    select(s, expect: id(s))
    print(id(s))
case "jp":
    guard let s = findJapanese() else {
        fail("ime: no enabled Japanese input source")
    }
    select(s, expect: id(s))
    print(id(s))
case "set":
    guard args.count > 2 else { usage() }
    guard let s = findSource(id: args[2]) else {
        fail("ime: no enabled source '\(args[2])'")
    }
    select(s, expect: args[2])
    print(args[2])
case "list":
    let sorted = enabledSources().sorted {
        name($0).localizedCaseInsensitiveCompare(name($1)) == .orderedAscending
    }
    for s in sorted {
        print("\(id(s))\t\(name(s))")
    }
case "pane-focus":
    // ime pane-focus [<pane-id>] [--default <abc|jp|<source-id>>]
    //
    // Per-pane input-source memory for terminal multiplexers (herdr). The
    // macOS input source is per-app, so every herdr pane shares one source;
    // without help, switching panes carries the previous pane's IME state
    // into the next one. This hook handler emulates per-pane state:
    //   - save the CURRENT source into the previously focused pane's slot
    //   - restore the newly focused pane's saved source (if any)
    //   - panes with no saved state get --default (if given), so a fresh
    //     pane lands on 英数 while known panes keep their own setting.
    // Pane id comes from the argument or HERDR_PANE_ID (herdr event hooks
    // inject it). State lives in `ime pane-state-dir` (default
    // ~/.cache/ime-panes/<pane-id> plus a .last marker).
    var paneID = ""
    var defaultSpec: [String] = []
    var i = 2
    while i < args.count {
        if args[i] == "--default", i + 1 < args.count {
            defaultSpec = [args[i + 1]]
            i += 2
        } else {
            paneID = args[i]
            i += 1
        }
    }
    if paneID.isEmpty {
        paneID = ProcessInfo.processInfo.environment["HERDR_PANE_ID"] ?? ""
    }
    guard !paneID.isEmpty else { exit(0) }
    let dir = paneStateDir()
    let fm = FileManager.default
    try? fm.createDirectory(atPath: dir, withIntermediateDirectories: true)
    let cur = currentID()
    let last = ((try? String(contentsOfFile: dir + "/.last", encoding: .utf8)) ?? "")
        .trimmingCharacters(in: .whitespacesAndNewlines)
    if !last.isEmpty, last != paneID {
        try? cur.write(toFile: dir + "/" + last, atomically: true, encoding: .utf8)
    }
    try? paneID.write(toFile: dir + "/.last", atomically: true, encoding: .utf8)
    let saved = ((try? String(contentsOfFile: dir + "/" + paneID, encoding: .utf8)) ?? "")
        .trimmingCharacters(in: .whitespacesAndNewlines)
    if !saved.isEmpty, saved != cur {
        if let s = findSource(id: saved) {
            select(s, expect: saved)
        }
    } else if saved.isEmpty, let d = defaultSpec.first, d != cur {
        let target = findSource(id: d) ?? (d == "abc" ? asciiSource() : nil)
            ?? (d == "jp" ? findJapanese() : nil)
        if let s = target {
            select(s, expect: id(s))
        }
    }
    exit(0)
case "watch":
    // ime watch <abc|jp|set <id>> --app <bundle-id|process-name>
    var appTargets: [String] = []
    var rest: [String] = []
    var i = 2
    while i < args.count {
        if args[i] == "--app", i + 1 < args.count {
            appTargets.append(args[i + 1])
            i += 2
        } else {
            rest.append(args[i])
            i += 1
        }
    }
    guard !appTargets.isEmpty, let apply = applier(for: rest) else { usage() }
    let matches: (NSRunningApplication) -> Bool = {
        appTargets.contains($0.bundleIdentifier ?? "") || appTargets.contains($0.localizedName ?? "")
    }
    let ws = NSWorkspace.shared
    if let front = ws.frontmostApplication, matches(front) {
        _ = apply()
    }
    ws.notificationCenter.addObserver(
        forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: .main
    ) { note in
        if let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication,
           matches(app) {
            _ = apply()
        }
    }
    NSApplication.shared.setActivationPolicy(.accessory)
    NSApplication.shared.run()
case "guard":
    // ime guard <abc|jp|set <id>> --app <bundle-id> [--interval <secs>] [--suspend-file <path>] [--verbose]
    // ime guard suspend|resume|status [--suspend-file <path>]
    var appTargets: [String] = []
    var interval = 1.0
    var suspendPath = defaultSuspendPath()
    var verbose = false
    var positional: [String] = []
    var i = 2
    while i < args.count {
        switch args[i] {
        case "--app":
            guard i + 1 < args.count else { usage() }
            appTargets.append(args[i + 1])
            i += 2
        case "--interval":
            guard i + 1 < args.count, let v = Double(args[i + 1]), v >= 0.2 else { usage() }
            interval = v
            i += 2
        case "--suspend-file":
            guard i + 1 < args.count else { usage() }
            suspendPath = args[i + 1]
            i += 2
        case "--verbose":
            verbose = true
            i += 1
        default:
            positional.append(args[i])
            i += 1
        }
    }
    let fm = FileManager.default
    switch positional.first ?? "" {
    case "suspend":
        try? fm.createDirectory(
            atPath: (suspendPath as NSString).deletingLastPathComponent,
            withIntermediateDirectories: true)
        fm.createFile(atPath: suspendPath, contents: Data())
        exit(0)
    case "resume":
        try? fm.removeItem(atPath: suspendPath)
        exit(0)
    case "status":
        print(fm.fileExists(atPath: suspendPath) ? "suspended" : "active")
        exit(0)
    default:
        guard !appTargets.isEmpty, let apply = applier(for: positional) else { usage() }
        let matches: (NSRunningApplication) -> Bool = {
            appTargets.contains($0.bundleIdentifier ?? "") || appTargets.contains($0.localizedName ?? "")
        }
        let ws = NSWorkspace.shared
        if verbose {
            FileHandle.standardError.write(
                ("ime guard: app=\(appTargets) mode=\(positional) interval=\(interval)s "
                + "suspend-file=\(suspendPath)\n").data(using: .utf8)!)
        }
        // Apply once immediately, then keep re-applying on a timer so the
        // source can never stay Japanese while the target app is frontmost
        // (herdr restores the pre-prefix source when prefix mode exits).
        // The frontmost check means the guard never touches other apps and
        // never fights the lock screen.
        while true {
            let front = ws.frontmostApplication
            let frontID = front?.bundleIdentifier ?? ""
            let suspended = fm.fileExists(atPath: suspendPath)
            if verbose {
                FileHandle.standardError.write(
                    ("ime guard: front=\(frontID) suspended=\(suspended) current=\(currentID())\n")
                        .data(using: .utf8)!)
            }
            if !suspended, let front = ws.frontmostApplication, matches(front) {
                _ = apply()
            }
            Thread.sleep(forTimeInterval: interval)
        }
    }
case "help", "--help", "-h":
    print("usage: ime get|list|abc|jp|set <id>|watch <abc|jp|set <id>> --app <bundle-id>|guard <abc|jp|set <id>> --app <bundle-id> [--interval <secs>]|pane-focus [<pane-id>] [--default <abc|jp|<source-id>>]")
default:
    FileHandle.standardError.write("ime: unknown command '\(cmd)'\n".data(using: .utf8)!)
    exit(2)
}
