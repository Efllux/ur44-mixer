//! USB-MIDI 1.0 event packets (4 bytes: cable/code index, then up to 3 MIDI bytes), as used by the
//! UR44's mixer interface (interface 4) in Yamaha mode. Only SysEx is needed.

/// Frame a complete SysEx message (F0 … F7) into USB-MIDI packets on cable 0.
pub fn frame_sysex(sx: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(sx.len().div_ceil(3) * 4);
    let mut chunks = sx.chunks(3).peekable();
    while let Some(c) = chunks.next() {
        let last = chunks.peek().is_none();
        // CIN 0x4: SysEx starts or continues; 0x5/0x6/0x7: SysEx ends with 1/2/3 bytes.
        let cin = if !last { 0x4 } else { [0, 0x5, 0x6, 0x7][c.len()] };
        out.push(cin);
        for i in 0..3 {
            out.push(*c.get(i).unwrap_or(&0));
        }
    }
    out
}

/// Reassembles SysEx messages from a stream of USB-MIDI packets.
#[derive(Default)]
pub struct Deframer {
    cur: Vec<u8>,
}

impl Deframer {
    /// Feed received bytes (whole 4-byte packets); calls `on_msg` for each complete SysEx.
    pub fn feed(&mut self, data: &[u8], mut on_msg: impl FnMut(Vec<u8>)) {
        for p in data.chunks_exact(4) {
            let (n, end) = match p[0] & 0x0F {
                0x4 => (3, false),
                0x5 => (1, true),
                0x6 => (2, true),
                0x7 => (3, true),
                _ => continue, // not SysEx (and not used by the mixer channel)
            };
            if p[1] == 0xF0 {
                self.cur.clear(); // a new message starts
            }
            self.cur.extend_from_slice(&p[1..1 + n]);
            if end {
                on_msg(std::mem::take(&mut self.cur));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_lengths() {
        for len in 2..40 {
            let mut sx = vec![0xF0];
            sx.extend((0..len - 2).map(|i| (i % 0x7F) as u8));
            sx.push(0xF7);
            let framed = frame_sysex(&sx);
            assert_eq!(framed.len() % 4, 0);
            let mut got = Vec::new();
            Deframer::default().feed(&framed, |m| got.push(m));
            assert_eq!(got, vec![sx]);
        }
    }

    #[test]
    fn version_request_framing() {
        // Sent to the UR44 on 2026-11-06 (spike): it answered with its firmware version.
        let f = frame_sysex(&[0xF0, 0x43, 0x30, 0x3E, 0x14, 0x00, 0x70, 0x02, 0xF7]);
        assert_eq!(f, vec![0x04, 0xF0, 0x43, 0x30, 0x04, 0x3E, 0x14, 0x00, 0x07, 0x70, 0x02, 0xF7]);
    }
}
