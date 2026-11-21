// Round-trip latency through the UR44's internal loopback (MIX 1 → computer inputs).
// Usage: swift roundtrip.swift <device-substring> <sample-rate> <buffer-frames> [seconds]
// Plays a short quiet click on output 1/2 every 250 ms and finds it on every input channel.
// Latency = input sample time of the detected click − output sample time it was written at.
import CoreAudio
import Foundation

let args = CommandLine.arguments
let want = args.count > 1 ? args[1] : "UR44"
let rate = args.count > 2 ? Double(args[2])! : 48000
let bufFrames = args.count > 3 ? UInt32(args[3])! : 32
let seconds = args.count > 4 ? Double(args[4])! : 4

func devices() -> [AudioObjectID] {
    var a = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDevices, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0; AudioObjectGetPropertyDataSize(AudioObjectID(kAudioObjectSystemObject), &a, 0, nil, &size)
    var ids = [AudioObjectID](repeating: 0, count: Int(size) / 4)
    AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &a, 0, nil, &size, &ids)
    return ids
}
func name(_ id: AudioObjectID) -> String {
    var ad = AudioObjectPropertyAddress(mSelector: kAudioObjectPropertyName, mScope: kAudioObjectPropertyScopeGlobal, mElement: 0)
    var n: Unmanaged<CFString>?; var ns = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    AudioObjectGetPropertyData(id, &ad, 0, nil, &ns, &n)
    return (n?.takeRetainedValue() as String?) ?? ""
}
func setProp<T>(_ id: AudioObjectID, _ sel: AudioObjectPropertySelector, _ v: T) -> OSStatus {
    var ad = AudioObjectPropertyAddress(mSelector: sel, mScope: kAudioObjectPropertyScopeGlobal, mElement: 0)
    var val = v
    return AudioObjectSetPropertyData(id, &ad, 0, nil, UInt32(MemoryLayout<T>.size), &val)
}
func getU32(_ id: AudioObjectID, _ sel: AudioObjectPropertySelector, _ scope: AudioObjectPropertyScope) -> UInt32 {
    var ad = AudioObjectPropertyAddress(mSelector: sel, mScope: scope, mElement: 0)
    var v: UInt32 = 0; var s = UInt32(4)
    AudioObjectGetPropertyData(id, &ad, 0, nil, &s, &v)
    return v
}

guard let dev = devices().first(where: { name($0).contains(want) }) else { print("no device"); exit(1) }
_ = setProp(dev, kAudioDevicePropertyNominalSampleRate, rate)
Thread.sleep(forTimeInterval: 0.5)
_ = setProp(dev, kAudioDevicePropertyBufferFrameSize, bufFrames)
let actualBuf = getU32(dev, kAudioDevicePropertyBufferFrameSize, kAudioObjectPropertyScopeGlobal)
let latIn = getU32(dev, kAudioDevicePropertyLatency, kAudioObjectPropertyScopeInput)
let safIn = getU32(dev, kAudioDevicePropertySafetyOffset, kAudioObjectPropertyScopeInput)
let latOut = getU32(dev, kAudioDevicePropertyLatency, kAudioObjectPropertyScopeOutput)
let safOut = getU32(dev, kAudioDevicePropertySafetyOffset, kAudioObjectPropertyScopeOutput)

final class State {
    var clicksAt: [Double] = []       // output sample times of clicks
    var found: [Double] = []          // measured latencies (samples)
    var nextClick: Double = -1
    var armed: [Double] = []          // pending click times waiting for detection
    var peak: Float = 0
    var period: Double = 0
    var debug: [String] = []
    var cycleFrames: Double = 0       // frames processed by this IOProc so far
    var appClicks: [Double] = []      // click positions in IOProc frame count
    var appFound: [Double] = []
}
let st = State()
st.period = rate * 0.25

var procID: AudioDeviceIOProcID?
let status = AudioDeviceCreateIOProcIDWithBlock(&procID, dev, nil) { _, inData, inTime, outData, outTime in
    // ---- output: write clicks ----
    let outBufs = UnsafeMutableAudioBufferListPointer(outData)
    let outStart = outTime.pointee.mSampleTime
    var frames = 0
    for b in outBufs {
        let ch = Int(b.mNumberChannels)
        let n = Int(b.mDataByteSize) / 4 / max(ch, 1)
        frames = n
        if let p = b.mData?.assumingMemoryBound(to: Float32.self) { for i in 0..<(n * ch) { p[i] = 0 } }
    }
    if st.nextClick < 0 { st.nextClick = outStart + rate * 0.5 }
    while st.nextClick < outStart + Double(frames) {
        let idx = Int(st.nextClick - outStart)
        if idx >= 0 {
            for b in outBufs {
                let ch = Int(b.mNumberChannels)
                guard let p = b.mData?.assumingMemoryBound(to: Float32.self), ch >= 2 else { continue }
                p[idx * ch] = 0.03; p[idx * ch + 1] = 0.03   // -30 dBFS single-sample click on out 1/2
                break
            }
            st.armed.append(st.nextClick)
            st.appClicks.append(st.cycleFrames + Double(idx))
        }
        st.nextClick += st.period
    }
    // ---- input: detect clicks ----
    let inBufs = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: inData))
    let inStart = inTime.pointee.mSampleTime
    for b in inBufs {
        let ch = Int(b.mNumberChannels)
        guard ch > 0, let p = b.mData?.assumingMemoryBound(to: Float32.self) else { continue }
        let n = Int(b.mDataByteSize) / 4 / ch
        for i in 0..<n {
            var m: Float = 0
            for c in 0..<ch { m = max(m, abs(p[i * ch + c])) }
            st.peak = max(st.peak, m)
            if m > 0.005 && st.debug.count < 6 {
                st.debug.append("in t=\(inStart + Double(i)) v=\(m) armed=\(st.armed.prefix(3).map { Int($0) })")
            }
            if m > 0.005, let a = st.appClicks.first {
                let lat = st.cycleFrames + Double(i) - a
                if lat >= 2 * Double(actualBuf) && lat < rate * 0.2 { st.appFound.append(lat); st.appClicks.removeFirst() }
            }
            if m > 0.005, let first = st.armed.first {
                let t = inStart + Double(i)
                let lat = t - first
                if lat > 0 && lat < rate * 0.2 {
                    st.found.append(lat)
                    st.armed.removeFirst()
                }
            }
        }
        break
    }
    st.cycleFrames += Double(frames)
    while let f = st.appClicks.first, st.cycleFrames - f > rate * 0.2 { st.appClicks.removeFirst() }
    // drop clicks never seen
    while let f = st.armed.first, outStart - f > rate * 0.2 { st.armed.removeFirst() }
}
guard status == noErr, let pid = procID else { print("ioproc error \(status)"); exit(1) }
AudioDeviceStart(dev, pid)
Thread.sleep(forTimeInterval: seconds)
AudioDeviceStop(dev, pid)
AudioDeviceDestroyIOProcID(dev, pid)

let asorted = st.appFound.sorted()
let amed = asorted.isEmpty ? 0 : asorted[asorted.count / 2]
let ms = { (s: Double) in s / rate * 1000 }
let sorted = st.found.sorted()
let med = sorted.isEmpty ? 0 : sorted[sorted.count / 2]
print(String(format: "%@ @ %.0f Hz, buffer %u | APP ROUND TRIP %.0f samples = %.2f ms (n=%d, min %.0f max %.0f) | device-internal %.0f samples | reported in %u+%u out %u+%u",
             name(dev), rate, actualBuf, amed, ms(amed), asorted.count, asorted.first ?? 0, asorted.last ?? 0, med, latIn, safIn, latOut, safOut))
