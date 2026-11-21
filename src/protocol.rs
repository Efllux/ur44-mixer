//! UR44 (original, firmware 2.x) mixer/DSP SysEx protocol.
//!
//! Derived from Yamaha's `MemAPI_UR44.bundle` (ParameterChange::convert*) and verified
//! against a real UR44 on 2026-11-06. Multi-byte numbers are packed as big-endian
//! groups of 7 bits (`CmdTranslator::translateUint16` = 3 bytes, `translateUint32` = 5 bytes).

pub const SYSEX_START: u8 = 0xF0;
pub const SYSEX_END: u8 = 0xF7;
pub const YAMAHA: u8 = 0x43;
pub const GROUP: u8 = 0x3E;
pub const MODEL: u8 = 0x14;

/// Second header byte: message class (high nibble) + device number (low nibble, always 0).
pub const CLASS_PARAM_CHANGE: u8 = 0x10;
pub const CLASS_BULK_REQUEST: u8 = 0x20;
pub const CLASS_PARAM_REQUEST: u8 = 0x30;

const CMD_PARAM: [u8; 2] = [0x01, 0x01];
const CMD_VERSION: [u8; 3] = [0x00, 0x70, 0x02];
const CMD_METER: [u8; 2] = [0x02, 0x02];

/// A mixer parameter address: parameter id plus channel/index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address {
    pub id: u16,
    pub channel: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Host → device: ask for firmware versions.
    VersionRequest,
    /// Device → host: firmware component names and versions, e.g. ("BOOT", "V2.12").
    VersionReply(Vec<(String, String)>),
    /// Host → device: ask for the current value of a parameter.
    ParamRequest(Address),
    /// Device → host: current value of a parameter (also sent unsolicited on change).
    ParamValue(Address, u32),
    /// Host → device: start (true) or stop (false) the ~20 Hz meter stream.
    Meter(bool),
    /// Device → host: one frame of meter values (44 × 21-bit; level/peak-hold pairs).
    MeterFrame(Vec<u32>),
    /// Anything we don't decode yet (bulk dumps, ...), kept raw.
    Unknown(Vec<u8>),
}

pub fn encode_7bit(value: u32, bytes: usize, out: &mut Vec<u8>) {
    for i in (0..bytes).rev() {
        out.push(((value >> (7 * i)) & 0x7F) as u8);
    }
}

pub fn decode_7bit(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |acc, b| (acc << 7) | u32::from(b & 0x7F))
}

fn header(class: u8) -> Vec<u8> {
    vec![SYSEX_START, YAMAHA, class, GROUP, MODEL]
}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Message::VersionRequest => {
                let mut m = header(CLASS_PARAM_REQUEST);
                m.extend_from_slice(&CMD_VERSION);
                m.push(SYSEX_END);
                m
            }
            Message::ParamRequest(a) => {
                let mut m = header(CLASS_PARAM_REQUEST);
                m.extend_from_slice(&CMD_PARAM);
                encode_7bit(a.id.into(), 3, &mut m);
                encode_7bit(a.channel.into(), 3, &mut m);
                m.push(SYSEX_END);
                m
            }
            Message::ParamValue(a, v) => {
                let mut m = header(CLASS_PARAM_CHANGE);
                m.extend_from_slice(&CMD_PARAM);
                encode_7bit(a.id.into(), 3, &mut m);
                encode_7bit(a.channel.into(), 3, &mut m);
                encode_7bit(*v, 5, &mut m);
                m.push(SYSEX_END);
                m
            }
            Message::Meter(start) => {
                // ParameterChange::convertStartMeter: 02 02 32 METER_START(7F)/METER_STOP(00)
                let mut m = header(CLASS_PARAM_REQUEST);
                m.extend_from_slice(&CMD_METER);
                m.push(0x32);
                m.push(if *start { 0x7F } else { 0x00 });
                m.push(SYSEX_END);
                m
            }
            Message::VersionReply(_) | Message::MeterFrame(_) | Message::Unknown(_) => {
                unimplemented!("device-originated message")
            }
        }
    }

    pub fn decode(raw: &[u8]) -> Message {
        let unknown = || Message::Unknown(raw.to_vec());
        if raw.len() < 7
            || raw[0] != SYSEX_START
            || raw[1] != YAMAHA
            || raw[3] != GROUP
            || raw[4] != MODEL
            || raw[raw.len() - 1] != SYSEX_END
        {
            return unknown();
        }
        let class = raw[2] & 0xF0;
        let body = &raw[5..raw.len() - 1];

        if class == CLASS_PARAM_CHANGE && body.len() == 2 + 3 + 3 + 5 && body[..2] == CMD_PARAM {
            let addr = Address {
                id: decode_7bit(&body[2..5]) as u16,
                channel: decode_7bit(&body[5..8]) as u16,
            };
            return Message::ParamValue(addr, decode_7bit(&body[8..13]));
        }
        if class == CLASS_PARAM_REQUEST && body.len() == 2 + 3 + 3 && body[..2] == CMD_PARAM {
            return Message::ParamRequest(Address {
                id: decode_7bit(&body[2..5]) as u16,
                channel: decode_7bit(&body[5..8]) as u16,
            });
        }
        if class == CLASS_PARAM_CHANGE && body.len() >= 4 && body[..3] == CMD_VERSION {
            // body[3] is a status byte; then pairs of 8-byte NUL-padded ASCII fields.
            let fields: Vec<String> = body[4..]
                .chunks(8)
                .map(|c| String::from_utf8_lossy(c).trim_end_matches(['\0', ' ']).to_string())
                .collect();
            let pairs = fields
                .chunks(2)
                .filter(|p| p.len() == 2)
                .map(|p| (p[0].clone(), p[1].clone()))
                .collect();
            return Message::VersionReply(pairs);
        }
        if class == CLASS_PARAM_CHANGE && body.len() > 2 && body[..2] == CMD_METER && (body.len() - 2) % 3 == 0 {
            let vals = body[2..].chunks(3).map(decode_7bit).collect();
            return Message::MeterFrame(vals);
        }
        if class == CLASS_PARAM_REQUEST && body == CMD_VERSION {
            return Message::VersionRequest;
        }
        unknown()
    }
}

/// Meter value → dBFS. Calibrated with test tones: 8192 = 0 dBFS, 102.4 units per dB.
pub fn meter_db(v: u32) -> f32 {
    v as f32 / 102.4 - 80.0
}

/// Meter slot layout (pairs of level, peak-hold).
pub mod meter_slot {
    /// Input channel 0..6 level; peak-hold is +1.
    pub const fn input(ch: usize) -> usize {
        ch * 2
    }
    pub const DAW_L: usize = 12;
    pub const DAW_R: usize = 14;
    /// Mix output; which mix is selected by the PatchMasterMeter parameter.
    pub const MIX_L: usize = 16;
    pub const MIX_R: usize = 18;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    #[test]
    fn seven_bit_roundtrip() {
        for v in [0u32, 1, 127, 128, 0x3FFF, 0xFFFF, 0xFFFF_FFFF] {
            let mut b = Vec::new();
            encode_7bit(v, 5, &mut b);
            assert!(b.iter().all(|x| *x < 0x80));
            assert_eq!(decode_7bit(&b), v);
        }
    }

    // Bytes below were captured from a real UR44 (FW 2.12) on 2026-11-06.
    #[test]
    fn version_request_matches_capture() {
        assert_eq!(Message::VersionRequest.encode(), hex("F0 43 30 3E 14 00 70 02 F7"));
    }

    #[test]
    fn version_reply_from_capture() {
        let raw = hex("F0 43 10 3E 14 00 70 02 00 42 4F 4F 54 00 00 00 00 56 32 2E 31 32 20 00 00 4D 41 49 4E 00 00 00 00 56 32 2E 31 32 20 00 00 F7");
        assert_eq!(
            Message::decode(&raw),
            Message::VersionReply(vec![
                ("BOOT".into(), "V2.12".into()),
                ("MAIN".into(), "V2.12".into())
            ])
        );
    }

    #[test]
    fn param_request_matches_capture() {
        let m = Message::ParamRequest(Address { id: 1, channel: 0 });
        assert_eq!(m.encode(), hex("F0 43 30 3E 14 01 01 00 00 01 00 00 00 F7"));
    }

    #[test]
    fn param_reply_from_capture() {
        let raw = hex("F0 43 10 3E 14 01 01 00 00 01 00 00 00 00 00 00 00 01 F7");
        assert_eq!(Message::decode(&raw), Message::ParamValue(Address { id: 1, channel: 0 }, 1));
    }

    #[test]
    fn large_address_and_value_encode() {
        let m = Message::ParamValue(Address { id: 300, channel: 5 }, 0xFFFF_FFFF);
        let enc = m.encode();
        assert_eq!(Message::decode(&enc), m);
    }

    #[test]
    fn meter_start_stop_encode() {
        assert_eq!(Message::Meter(true).encode(), hex("F0 43 30 3E 14 02 02 32 7F F7"));
        assert_eq!(Message::Meter(false).encode(), hex("F0 43 30 3E 14 02 02 32 00 F7"));
    }

    #[test]
    fn meter_frame_from_capture() {
        // DAW L/R (slots 12-15) while playing a -20 dBFS tone, captured 2026-11-06.
        let mut raw = hex("F0 43 10 3E 14 02 02");
        let mut vals = vec![0u32; 44];
        vals[12] = 6123;
        vals[14] = 6123;
        for v in &vals {
            encode_7bit(*v, 3, &mut raw);
        }
        raw.push(0xF7);
        match Message::decode(&raw) {
            Message::MeterFrame(v) => {
                assert_eq!(v.len(), 44);
                assert_eq!(v[12], 6123);
                assert!((meter_db(v[12]) + 20.2).abs() < 0.1);
            }
            other => panic!("{other:?}"),
        }
    }
}
