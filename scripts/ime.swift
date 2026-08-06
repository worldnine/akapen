// macOS input-source helper for akapen (Carbon TIS API; no
// accessibility permission required).
//   ime get        — print the current input-source ID
//   ime abc        — select the ABC (ASCII) source
//   ime jp         — select the first enabled Japanese source
//   ime set <id>   — select the source with the given ID
import Carbon
import Darwin

func currentID() -> String {
    guard let s = TISCopyCurrentKeyboardInputSource()?.takeRetainedValue() else { return "" }
    if let p = TISGetInputSourceProperty(s, kTISPropertyInputSourceID) {
        return Unmanaged<CFString>.fromOpaque(p).takeUnretainedValue() as String
    }
    return ""
}

func findSource(id: String) -> TISInputSource? {
    guard let list = TISCreateInputSourceList(
        [kTISPropertyInputSourceID: id] as CFDictionary, false)?
        .takeRetainedValue() as? [TISInputSource] else { return nil }
    return list.first
}

func findJapanese() -> TISInputSource? {
    guard let list = TISCreateInputSourceList(
        [kTISPropertyInputSourceIsEnabled: kCFBooleanTrue] as CFDictionary, false)?
        .takeRetainedValue() as? [TISInputSource] else { return nil }
    for s in list {
        if let p = TISGetInputSourceProperty(s, kTISPropertyInputSourceID) {
            let id = Unmanaged<CFString>.fromOpaque(p).takeUnretainedValue() as String
            if id.contains("Japanese") || id.contains("Kotoeri") || id.contains("google") {
                return s
            }
        }
    }
    return nil
}

func select(_ s: TISInputSource) {
    TISSelectInputSource(s)
    usleep(150_000) // the switch is async; give it a beat
}

let args = CommandLine.arguments
switch args.count > 1 ? args[1] : "get" {
case "abc":
    if let s = findSource(id: "com.apple.keylayout.ABC") { select(s); print("abc") }
case "jp":
    if let s = findJapanese() { select(s); print("jp") }
case "set":
    if args.count > 2, let s = findSource(id: args[2]) { select(s); print(args[2]) }
default:
    print(currentID())
}
