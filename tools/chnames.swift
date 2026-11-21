// Print a device's input/output channel counts and names. Usage: swift chnames.swift <name-substring>
import CoreAudio
import Foundation
let want = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "UR44"
var a = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDevices, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
var size: UInt32 = 0; AudioObjectGetPropertyDataSize(AudioObjectID(kAudioObjectSystemObject), &a, 0, nil, &size)
var ids = [AudioObjectID](repeating: 0, count: Int(size)/4); AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &a, 0, nil, &size, &ids)
func str(_ id: AudioObjectID, _ sel: AudioObjectPropertySelector, _ scope: AudioObjectPropertyScope, _ el: UInt32) -> String {
    var ad = AudioObjectPropertyAddress(mSelector: sel, mScope: scope, mElement: el)
    var n: Unmanaged<CFString>?; var ns = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    if AudioObjectGetPropertyData(id, &ad, 0, nil, &ns, &n) != noErr { return "-" }
    return (n?.takeRetainedValue() as String?) ?? "-"
}
for id in ids where str(id, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, 0).contains(want) {
    print("device \(id):", str(id, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, 0))
    for (label, sc) in [("out", kAudioObjectPropertyScopeOutput), ("in", kAudioObjectPropertyScopeInput)] {
        var ad = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyStreamConfiguration, mScope: sc, mElement: 0)
        var s: UInt32 = 0; AudioObjectGetPropertyDataSize(id, &ad, 0, nil, &s)
        let buf = UnsafeMutableRawPointer.allocate(byteCount: Int(s), alignment: 16); defer { buf.deallocate() }
        AudioObjectGetPropertyData(id, &ad, 0, nil, &s, buf)
        let abl = UnsafeMutableAudioBufferListPointer(buf.assumingMemoryBound(to: AudioBufferList.self))
        let n = abl.reduce(0) { $0 + Int($1.mNumberChannels) }
        print(" ", label, n, "channels:", n == 0 ? [] : (1...n).map { str(id, kAudioObjectPropertyElementName, sc, UInt32($0)) })
    }
}
