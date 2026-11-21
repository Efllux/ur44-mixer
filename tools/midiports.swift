import CoreMIDI
import Foundation
func name(_ o: MIDIObjectRef, _ k: CFString = kMIDIPropertyDisplayName) -> String {
    var s: Unmanaged<CFString>?; if MIDIObjectGetStringProperty(o, k, &s) == noErr, let v = s?.takeRetainedValue() { return v as String }; return "?" }
func ip(_ o: MIDIObjectRef, _ k: CFString) -> Int32 { var v: Int32 = 0; MIDIObjectGetIntegerProperty(o, k, &v); return v }
for d in 0..<MIDIGetNumberOfDevices() {
    let dev = MIDIGetDevice(d); let n = name(dev, kMIDIPropertyName)
    guard n.lowercased().contains("ur44") else { continue }
    print("device: \(n) offline=\(ip(dev, kMIDIPropertyOffline)) driver=\(name(dev, kMIDIPropertyDriverOwner))")
    for e in 0..<MIDIDeviceGetNumberOfEntities(dev) { let ent = MIDIDeviceGetEntity(dev, e)
        print("  entity \(e): \(name(ent, kMIDIPropertyName)) src=\(MIDIEntityGetNumberOfSources(ent)) dst=\(MIDIEntityGetNumberOfDestinations(ent))") }
}
print("sources:"); for i in 0..<MIDIGetNumberOfSources() { print("  [\(i)] \(name(MIDIGetSource(i)))") }
print("destinations:"); for i in 0..<MIDIGetNumberOfDestinations() { print("  [\(i)] \(name(MIDIGetDestination(i)))") }
