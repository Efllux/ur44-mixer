// Play a sine tone on a specific output device (left+right), for meter calibration.
// Usage: swift tone.swift <device-name-substring> <freq Hz> <level dBFS> <seconds>
import AVFoundation
import CoreAudio

let args = CommandLine.arguments
let devName = args[1], freq = Double(args[2])!, dbfs = Double(args[3])!, secs = Double(args[4])!

func deviceID(matching s: String) -> AudioDeviceID? {
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
        var oa = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyStreamConfiguration, mScope: kAudioObjectPropertyScopeOutput, mElement: kAudioObjectPropertyElementMain)
        var os: UInt32 = 0
        AudioObjectGetPropertyDataSize(id, &oa, 0, nil, &os)
        if let name = n?.takeRetainedValue() as String?, name.contains(s), os > 8 { return id }
    }
    return nil
}

guard let dev = deviceID(matching: devName) else { print("no output device matching \(devName)"); exit(1) }
let engine = AVAudioEngine()
var devID = dev
let unit = engine.outputNode.audioUnit!
AudioUnitSetProperty(unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0, &devID, UInt32(MemoryLayout<AudioDeviceID>.size))
let sr = engine.outputNode.outputFormat(forBus: 0).sampleRate
let amp = Float(pow(10.0, dbfs / 20.0))
var phase = 0.0
let fmt = AVAudioFormat(standardFormatWithSampleRate: sr, channels: 2)!
let src = AVAudioSourceNode(format: fmt) { _, _, frames, abl -> OSStatus in
    let bufs = UnsafeMutableAudioBufferListPointer(abl)
    for f in 0..<Int(frames) {
        let v = amp * Float(sin(phase))
        phase += 2 * .pi * freq / sr
        if phase > 2 * .pi { phase -= 2 * .pi }
        for b in bufs { b.mData!.assumingMemoryBound(to: Float.self)[f] = v }
    }
    return noErr
}
engine.attach(src)
engine.connect(src, to: engine.mainMixerNode, format: fmt)
engine.mainMixerNode.outputVolume = 1.0
try engine.start()
print("playing \(freq) Hz at \(dbfs) dBFS on device \(dev) for \(secs)s (sr \(sr))")
RunLoop.current.run(until: Date().addingTimeInterval(secs))
engine.stop()
