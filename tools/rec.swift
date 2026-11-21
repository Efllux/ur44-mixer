// Record RMS level of one input channel of a device, printing "<epoch ms>\t<dBFS>" every buffer.
// Usage: rec <device-name-substring> <channel 0-based> <seconds>
import AVFoundation
import CoreAudio

let args = CommandLine.arguments
let devName = args[1], chan = Int(args[2])!, secs = Double(args[3])!

func inputDevice(matching s: String) -> AudioDeviceID? {
    var a = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDevices, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0
    AudioObjectGetPropertyDataSize(AudioObjectID(kAudioObjectSystemObject), &a, 0, nil, &size)
    var ids = [AudioDeviceID](repeating: 0, count: Int(size) / 4)
    AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &a, 0, nil, &size, &ids)
    for id in ids {
        var na = AudioObjectPropertyAddress(mSelector: kAudioObjectPropertyName, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var n: Unmanaged<CFString>?
        var ns = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        AudioObjectGetPropertyData(id, &na, 0, nil, &ns, &n)
        var ia = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyStreamConfiguration, mScope: kAudioObjectPropertyScopeInput, mElement: kAudioObjectPropertyElementMain)
        var isz: UInt32 = 0
        AudioObjectGetPropertyDataSize(id, &ia, 0, nil, &isz)
        if let name = n?.takeRetainedValue() as String?, name.contains(s), isz > 8 { return id }
    }
    return nil
}

guard let dev = inputDevice(matching: devName) else { print("no input device matching \(devName)"); exit(1) }
let engine = AVAudioEngine()
var devID = dev
AudioUnitSetProperty(engine.inputNode.audioUnit!, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0, &devID, UInt32(MemoryLayout<AudioDeviceID>.size))
let fmt = engine.inputNode.inputFormat(forBus: 0)
engine.inputNode.installTap(onBus: 0, bufferSize: 2048, format: fmt) { buf, _ in
    guard let d = buf.floatChannelData, chan < Int(buf.format.channelCount) else { return }
    let p = d[chan]
    var sum: Float = 0
    for i in 0..<Int(buf.frameLength) { sum += p[i] * p[i] }
    let rms = sqrt(sum / Float(max(buf.frameLength, 1)))
    let db = 20 * log10(max(rms, 1e-9))
    print(String(format: "%.0f\t%.1f", Date().timeIntervalSince1970 * 1000, db))
    fflush(stdout)
}
do { try engine.start() } catch { print("start failed: \(error)"); exit(2) }
FileHandle.standardError.write("recording \(fmt.channelCount) ch @ \(fmt.sampleRate) Hz\n".data(using: .utf8)!)
RunLoop.current.run(until: Date().addingTimeInterval(secs))
engine.stop()
