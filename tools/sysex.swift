// Usage: swift sysex.swift <portIndexSubstring> <listenSeconds> [HEX ...]
// Listens on the matching UR44 source, sends each HEX SysEx to the matching destination,
// and prints everything received (hex). Only intended for read-only queries.
import CoreMIDI
import Foundation

func name(_ o: MIDIObjectRef) -> String {
    var s: Unmanaged<CFString>?
    if MIDIObjectGetStringProperty(o, kMIDIPropertyDisplayName, &s) == noErr, let v = s?.takeRetainedValue() { return v as String }
    return "?"
}
func hex(_ b: [UInt8]) -> String { b.map { String(format: "%02X", $0) }.joined(separator: " ") }
func parse(_ s: String) -> [UInt8] {
    let c = s.replacingOccurrences(of: " ", with: "")
    return stride(from: 0, to: c.count, by: 2).map {
        let i = c.index(c.startIndex, offsetBy: $0); return UInt8(c[i..<c.index(i, offsetBy: 2)], radix: 16)!
    }
}

let args = CommandLine.arguments
let portName = args.count > 1 ? args[1] : "Port 2"
let wait = args.count > 2 ? Double(args[2])! : 1.5
let msgs = args.dropFirst(3).map(parse)

var client = MIDIClientRef(); MIDIClientCreate("ur44probe" as CFString, nil, nil, &client)
var inPort = MIDIPortRef()
var buffer: [UInt8] = []
let t0 = Date()
MIDIInputPortCreateWithBlock(client, "in" as CFString, &inPort) { list, src in
    let srcName = src.map { String(describing: Unmanaged<AnyObject>.fromOpaque($0).takeUnretainedValue()) } ?? ""
    for p in list.unsafeSequence() {
        let bytes = withUnsafeBytes(of: p.pointee.data) { Array($0.prefix(Int(p.pointee.length))) }
        print(String(format: "[%6.3fs] RX %@: %@", Date().timeIntervalSince(t0), srcName, hex(bytes)))
    }
}
var dest: MIDIEndpointRef = 0
for i in 0..<MIDIGetNumberOfSources() {
    let s = MIDIGetSource(i); let n = name(s)
    if n.contains("UR44") {
        let ctx = Unmanaged.passRetained(n as NSString).toOpaque()
        MIDIPortConnectSource(inPort, s, ctx); print("listening: \(n)")
    }
}
for i in 0..<MIDIGetNumberOfDestinations() {
    let d = MIDIGetDestination(i); if name(d).contains("UR44") && name(d).hasSuffix(portName) { dest = d }
}
guard dest != 0 else { print("destination \(portName) not found"); exit(1) }
print("sending to: \(name(dest))")
var outPort = MIDIPortRef(); MIDIOutputPortCreate(client, "out" as CFString, &outPort)

RunLoop.current.run(until: Date().addingTimeInterval(0.5))
for m in msgs {
    print(String(format: "[%6.3fs] TX: %@", Date().timeIntervalSince(t0), hex(m)))
    var pl = MIDIPacketList(); var pkt = MIDIPacketListInit(&pl)
    m.withUnsafeBufferPointer { pkt = MIDIPacketListAdd(&pl, MemoryLayout<MIDIPacketList>.size, pkt, 0, m.count, $0.baseAddress!) }
    MIDISend(outPort, dest, &pl)
    RunLoop.current.run(until: Date().addingTimeInterval(wait))
}
if msgs.isEmpty { RunLoop.current.run(until: Date().addingTimeInterval(wait)) }
print("done")
