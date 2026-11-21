import CoreAudio
import Foundation
func get<T>(_ id: AudioObjectID, _ sel: AudioObjectPropertySelector, _ scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal, _ def: T) -> T {
    var a = AudioObjectPropertyAddress(mSelector: sel, mScope: scope, mElement: kAudioObjectPropertyElementMain)
    var v = def; var s = UInt32(MemoryLayout<T>.size); AudioObjectGetPropertyData(id, &a, 0, nil, &s, &v); return v }
var a = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDevices, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
var size: UInt32 = 0; AudioObjectGetPropertyDataSize(AudioObjectID(kAudioObjectSystemObject), &a, 0, nil, &size)
var ids = [AudioObjectID](repeating: 0, count: Int(size)/4); AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &a, 0, nil, &size, &ids)
for id in ids {
    var na = AudioObjectPropertyAddress(mSelector: kAudioObjectPropertyName, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
    var n: Unmanaged<CFString>?; var ns = UInt32(MemoryLayout<Unmanaged<CFString>?>.size); AudioObjectGetPropertyData(id, &na, 0, nil, &ns, &n)
    let name = (n?.takeRetainedValue() as String?) ?? ""; guard name.contains("UR44") else { continue }
    print("device:", name, "transport:", get(id, kAudioDevicePropertyTransportType, kAudioObjectPropertyScopeGlobal, UInt32(0)) == kAudioDeviceTransportTypeUSB ? "USB" : "other")
    var ra = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyAvailableNominalSampleRates, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
    var rs: UInt32 = 0; AudioObjectGetPropertyDataSize(id, &ra, 0, nil, &rs)
    var rates = [AudioValueRange](repeating: AudioValueRange(), count: Int(rs)/MemoryLayout<AudioValueRange>.size); AudioObjectGetPropertyData(id, &ra, 0, nil, &rs, &rates)
    print("sample rates:", rates.map { Int($0.mMinimum) })
    let br: AudioValueRange = get(id, kAudioDevicePropertyBufferFrameSizeRange, kAudioObjectPropertyScopeGlobal, AudioValueRange())
    print("buffer size range:", Int(br.mMinimum), "-", Int(br.mMaximum), "current:", get(id, kAudioDevicePropertyBufferFrameSize, kAudioObjectPropertyScopeGlobal, UInt32(0)))
    for (label, sc) in [("input", kAudioObjectPropertyScopeInput), ("output", kAudioObjectPropertyScopeOutput)] {
        print("\(label): device latency", get(id, kAudioDevicePropertyLatency, sc, UInt32(0)), "safety offset", get(id, kAudioDevicePropertySafetyOffset, sc, UInt32(0)), "frames")
    }
}
