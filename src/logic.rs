//! Host-side mixer logic that dspMixFx performs on top of the raw parameters.
//! Re-implemented from MemAPI_UR44 (smd_SetPrmRel*, chkChStripPatch, morphGetTblData)
//! and its item tables (pairItemTbl, morphItemTbl, fxItemTbl…), decompiled with Ghidra.
//!
//! Everything here is pure: given the current values and one user edit, it returns the
//! full list of parameter writes to send to the UR44.

use crate::params;
use crate::protocol::Address;
use std::collections::HashMap;

pub type Values = HashMap<Address, i32>;
pub type Writes = Vec<(Address, i32)>;

pub fn addr(name: &str, ch: u16) -> Address {
    let p = params::by_name(name).unwrap_or_else(|| panic!("unknown parameter {name}"));
    Address { id: p.id, channel: ch }
}

pub fn get(v: &Values, name: &str, ch: u16) -> i32 {
    v.get(&addr(name, ch)).copied().unwrap_or_else(|| params::by_name(name).map_or(0, |p| p.default))
}

/// pairItemTbl: settings mirrored across a linked stereo input pair.
pub const PAIRED: [&str; 11] = [
    "ChPairOnf", "ChHpfOnf", "ChHpfFreq", "ChSendMixBus1SoloOnf", "ChSendMixBus2SoloOnf", "ChSendMixBus1Onf",
    "ChSendMixBus2Onf", "ChSendFxBusOnf", "ChSendMixBus1Level", "ChSendMixBus2Level", "ChSendFxBusLevel",
];
/// faderLinkItemTbl: only mirrored when the pair's fader link is on.
pub const FADER_LINK: [&str; 2] = ["ChSendMixBus1Level", "ChSendMixBus2Level"];

/// morphItemTbl (channel strip values driven by the MORPH knob), with the matching
/// morph-table parameters and the clip ranges used by smd_SetPrmRelMorph.
pub const MORPH_ITEMS: [&str; 17] = [
    "FxCompAttack", "FxCompRelease", "FxCompRatio", "FxCompKnee", "FxCompThreshOfst", "FxCompMakeupOfst",
    "FxCompScFreq", "FxCompScGain", "FxCompScQual", "FxEqHsvFreq", "FxEqHsvGain", "FxEqMidFreq",
    "FxEqMidGain", "FxEqMidQual", "FxEqLsvFreq", "FxEqLsvGain", "FxEqPeqGain",
];
pub const MORPH_TABLE: [&str; 17] = [
    "MorphCompAttack", "MorphCompRelease", "MorphCompRatio", "MorphCompKnee", "MorphCompThreshOfst",
    "MorphCompMakeupOfst", "MorphCompScFreq", "MorphCompScGain", "MorphCompScQual", "MorphEqHsvFreq",
    "MorphEqHsvGain", "MorphEqMidFreq", "MorphEqMidGain", "MorphEqMidQual", "MorphEqLsvFreq",
    "MorphEqLsvGain", "MorphEqPeqGain",
];
const MORPH_CLIP: [(i32, i32); 17] = [
    (0x39, 0x11b), (0x18, 300), (0, 0x78), (0, 2), (0, 200), (0, 200), (4, 0x7c), (0, 0x168), (0, 0x3c),
    (0x3c, 0x7c), (0, 0x168), (4, 0x7c), (0, 0x168), (0, 0x3c), (4, 0x48), (0, 0x168), (0, 0x168),
];
pub const MORPH_POINTS: u16 = 9;
pub const STRIPS: u16 = 4;

/// smd_SetPrmRelRevType: defaults written when the REV-X type changes (Hall, Room, Plate).
pub const REV_ITEMS: [&str; 10] = [
    "FxRevTime", "FxRevDiff", "FxRevIniDly", "FxRevHpf", "FxRevLpf", "FxRevRoomSize", "FxRevHighRatio",
    "FxRevLowRatio", "FxRevDecay", "FxRevLowFreq",
];
const REV_DEFAULTS: [[i32; 10]; 3] = [
    [23, 10, 2, 4, 50, 29, 8, 12, 27, 32], // Hall
    [6, 8, 2, 6, 47, 15, 7, 11, 15, 32],   // Room
    [21, 8, 2, 12, 52, 18, 9, 10, 5, 32],  // Plate
];

/// Guitar amp parameters per amp type (fxItemTbl … fxItemTbl_4), reset to defaults on type change.
pub fn guitar_items(amp: usize) -> Vec<&'static str> {
    let prefix = ["GtrFxClean", "GtrFxCrunch", "GtrFxLead", "GtrFxDrive"][amp.min(3)];
    let mut v: Vec<&'static str> =
        params::PARAMS.iter().filter(|p| p.name.starts_with(prefix)).map(|p| p.name).collect();
    if amp == 1 {
        v.push("GtrFxnCrunchHidMaxOut"); // Yamaha's typo, part of the Crunch set
    }
    v
}

fn is_input(name: &str) -> bool {
    name.starts_with("Ch")
}

pub fn linked(v: &Values, ch: u16) -> bool {
    get(v, "ChPairOnf", ch & !1) != 0
}

/// Compute all writes caused by the user setting `name[ch] = val`.
pub fn edit(v: &Values, name: &str, ch: u16, val: i32) -> Writes {
    let mut w: Writes = vec![(addr(name, ch), val)];

    match name {
        "ChPairOnf" => {
            let (l, r) = (ch & !1, ch | 1);
            w = vec![(addr(name, l), val), (addr(name, r), val)];
            if val != 0 {
                let fader_link = get(v, "ChFaderLinkOnf", l) != 0;
                for item in PAIRED.iter().filter(|i| **i != "ChPairOnf") {
                    if FADER_LINK.contains(item) && !fader_link {
                        continue;
                    }
                    w.push((addr(item, r), get(v, item, l)));
                }
                // A stereo pair carries at most one insert (from the left channel); a guitar
                // amp is mono-only, so it is removed.
                for (side, c) in [(0, l), (1, r)] {
                    let _ = side;
                    for item in ["ChGtrFxInsPrePatch", "ChGtrFxInsPostPatch"] {
                        if get(v, item, c) != 0 {
                            w.push((addr(item, c), 0));
                        }
                    }
                }
                for item in ["ChFxInsPrePatch", "ChFxInsPostPatch"] {
                    if get(v, item, r) != 0 {
                        w.push((addr(item, r), 0));
                    }
                }
            }
        }
        "FxRevType" if get(v, name, 0) != val => {
            let d = REV_DEFAULTS[val.clamp(0, 2) as usize];
            for (item, dv) in REV_ITEMS.iter().zip(d) {
                w.push((addr(item, 0), dv));
            }
        }
        "GtrFxType" if get(v, name, 0) != val => {
            for item in guitar_items(val.clamp(0, 3) as usize) {
                let p = params::by_name(item).expect("guitar param");
                w.push((addr(item, 0), p.default));
            }
        }
        "FxMorphValue" => w.extend(morph(v, ch, val)),
        _ => {}
    }

    // Mirror within a linked pair.
    if is_input(name) && name != "ChPairOnf" && PAIRED.contains(&name) && linked(v, ch) {
        let fader_link = get(v, "ChFaderLinkOnf", ch & !1) != 0;
        if !(FADER_LINK.contains(&name) && !fader_link) {
            w.push((addr(name, ch ^ 1), val));
        }
    }

    // smd_SetPrmRelSolo: the global solo flag follows "any solo is on".
    if name.contains("SoloOnf") || name == "ChPairOnf" {
        let mut after = v.clone();
        for (a, x) in &w {
            after.insert(*a, *x);
        }
        let any = (0..6).any(|c| get(&after, "ChSendMixBus1SoloOnf", c) != 0 || get(&after, "ChSendMixBus2SoloOnf", c) != 0)
            || get(&after, "DawSendMixBus1SoloOnf", 0) != 0
            || get(&after, "DawSendMixBus2SoloOnf", 0) != 0;
        if i32::from(any) != get(v, "SysSoloOnf", 0) {
            w.push((addr("SysSoloOnf", 0), i32::from(any)));
        }
    }
    w
}

/// smd_SetPrmRelMorph: interpolate the strip's 9-point morph table at `m` (0..=120).
pub fn morph(v: &Values, strip: u16, m: i32) -> Writes {
    let m = m.clamp(0, 120);
    let pos = f64::from(m) / 15.0;
    let whole = pos as u16;
    let frac = pos - f64::from(whole);
    let (i0, i1) = if m == 0 { (0, 0) } else { (whole.min(8), if whole <= 7 { whole + 1 } else { 8 }) };
    let mut w = Writes::new();
    for k in 0..17 {
        let a = get(v, MORPH_TABLE[k], strip * MORPH_POINTS + i0);
        let b = get(v, MORPH_TABLE[k], strip * MORPH_POINTS + i1);
        let x = (frac * f64::from(b - a) + f64::from(a)) as i32; // C truncation, as in MemAPI
        let (lo, hi) = MORPH_CLIP[k];
        w.push((addr(MORPH_ITEMS[k], strip), x.clamp(lo, hi)));
    }
    w
}

/// Writes that load a Sweet Spot preset into `strip`'s morph table, then re-apply the morph.
pub fn load_sweet_spot(v: &Values, strip: u16, points: &[[i32; 17]; 9]) -> Writes {
    let mut w = Writes::new();
    let mut after = v.clone();
    for (p, point) in points.iter().enumerate() {
        for (k, val) in point.iter().enumerate() {
            let a = addr(MORPH_TABLE[k], strip * MORPH_POINTS + p as u16);
            w.push((a, *val));
            after.insert(a, *val);
        }
    }
    let m = get(v, "FxMorphValue", strip);
    w.extend(morph(&after, strip, m));
    w
}

// ---------- effect inserts (chkChStripPatch + manual p.34 limits) ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FxKind {
    None,
    ChannelStrip,
    GuitarAmp,
}

/// Insert position. INS.FX = the "Pre" patch (affects monitor and recording),
/// MON.FX = the "Post" patch (monitor only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FxPos {
    Mon,
    Ins,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insert {
    pub kind: FxKind,
    pub pos: FxPos,
    /// 1-based channel strip number for ChannelStrip.
    pub strip: u16,
}

pub fn insert_of(v: &Values, ch: u16) -> Insert {
    let pre = get(v, "ChFxInsPrePatch", ch);
    let post = get(v, "ChFxInsPostPatch", ch);
    if pre > 0 {
        return Insert { kind: FxKind::ChannelStrip, pos: FxPos::Ins, strip: pre as u16 };
    }
    if post > 0 {
        return Insert { kind: FxKind::ChannelStrip, pos: FxPos::Mon, strip: post as u16 };
    }
    if get(v, "ChGtrFxInsPrePatch", ch) > 0 {
        return Insert { kind: FxKind::GuitarAmp, pos: FxPos::Ins, strip: 0 };
    }
    if get(v, "ChGtrFxInsPostPatch", ch) > 0 {
        return Insert { kind: FxKind::GuitarAmp, pos: FxPos::Mon, strip: 0 };
    }
    Insert { kind: FxKind::None, pos: FxPos::Mon, strip: 0 }
}

/// DSP budget: 4 units; mono channel strip = 1, stereo = 2 (one per channel), guitar amp = 2.
fn cost(i: &Insert) -> u32 {
    match i.kind {
        FxKind::None => 0,
        FxKind::ChannelStrip => 1,
        FxKind::GuitarAmp => 2,
    }
}

/// Assign or remove the effect on input `ch` (and its linked partner).
pub fn set_insert(v: &Values, ch: u16, kind: FxKind, pos: FxPos) -> Result<Writes, String> {
    if kind != FxKind::None && get(v, "SysFsCode", 0) > 3 {
        return Err("Channel Strip and Guitar Amp Classics are not available at 176.4/192 kHz.".into());
    }
    let pair = linked(v, ch);
    let chans: Vec<u16> = if pair { vec![ch & !1, ch | 1] } else { vec![ch] };
    if kind == FxKind::GuitarAmp && pair {
        return Err("Guitar Amp Classics works on mono channels only. Unlink the pair first.".into());
    }
    let others: Vec<(u16, Insert)> = (0..6).filter(|c| !chans.contains(c)).map(|c| (c, insert_of(v, c))).collect();
    let used_cost: u32 = others.iter().map(|(_, i)| cost(i)).sum();
    let gtr_elsewhere = others.iter().any(|(_, i)| i.kind == FxKind::GuitarAmp);

    let mut w = Writes::new();
    for &c in &chans {
        for item in ["ChFxInsPrePatch", "ChFxInsPostPatch", "ChGtrFxInsPrePatch", "ChGtrFxInsPostPatch"] {
            w.push((addr(item, c), 0));
        }
    }
    match kind {
        FxKind::None => {}
        FxKind::GuitarAmp => {
            if gtr_elsewhere {
                return Err("Guitar Amp Classics is already used on another channel.".into());
            }
            if used_cost + 2 > 4 {
                return Err("Not enough DSP left: remove a Channel Strip first.".into());
            }
            let item = if pos == FxPos::Ins { "ChGtrFxInsPrePatch" } else { "ChGtrFxInsPostPatch" };
            w.push((addr(item, ch), 1));
        }
        FxKind::ChannelStrip => {
            if used_cost + chans.len() as u32 > 4 {
                return Err("All Channel Strips are in use (a Guitar Amp counts as two).".into());
            }
            let taken: Vec<u16> = others.iter().filter(|(_, i)| i.kind == FxKind::ChannelStrip).map(|(_, i)| i.strip).collect();
            let mut free = (1..=STRIPS).filter(|s| !taken.contains(s));
            let item = if pos == FxPos::Ins { "ChFxInsPrePatch" } else { "ChFxInsPostPatch" };
            let mut strips = Vec::new();
            for &c in &chans {
                let s = free.next().ok_or("No free Channel Strip.")?;
                w.push((addr(item, c), i32::from(s)));
                strips.push(s);
            }
            for s in &strips {
                w.push((addr("FxStereoLinkOnf", s - 1), i32::from(pair)));
            }
        }
    }
    // Drop no-op writes.
    w.retain(|(a, x)| v.get(a) != Some(x));
    Ok(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(pairs: &[(&str, u16, i32)]) -> Values {
        pairs.iter().map(|(n, c, x)| (addr(n, *c), *x)).collect()
    }

    #[test]
    fn solo_sets_global_flag() {
        let v = vals(&[("SysSoloOnf", 0, 0)]);
        let w = edit(&v, "ChSendMixBus1SoloOnf", 2, 1);
        assert!(w.contains(&(addr("SysSoloOnf", 0), 1)));
    }

    #[test]
    fn linked_pair_mirrors_level_but_not_pan() {
        let v = vals(&[("ChPairOnf", 0, 1), ("ChFaderLinkOnf", 0, 1)]);
        let w = edit(&v, "ChSendMixBus1Level", 1, 90);
        assert!(w.contains(&(addr("ChSendMixBus1Level", 0), 90)));
        let w = edit(&v, "ChSendMixBus1Pan", 1, 16);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn rev_type_change_writes_defaults() {
        let v = vals(&[("FxRevType", 0, 0)]);
        let w = edit(&v, "FxRevType", 0, 1);
        assert!(w.contains(&(addr("FxRevTime", 0), 6)));
        assert!(w.contains(&(addr("FxRevDecay", 0), 15)));
    }

    #[test]
    fn morph_interpolates_between_points() {
        let mut v = Values::new();
        for k in 0..17 {
            v.insert(addr(MORPH_TABLE[k], 0), 100);
            v.insert(addr(MORPH_TABLE[k], 1), 200);
        }
        let w = morph(&v, 0, 7); // 7/15 of the way from point 0 to point 1
        let attack = w.iter().find(|(a, _)| *a == addr("FxCompAttack", 0)).unwrap().1;
        assert_eq!(attack, 146);
    }

    #[test]
    fn insert_budget() {
        let mut v = Values::new();
        // Guitar amp on input 1 uses 2 units.
        for (a, x) in set_insert(&v, 0, FxKind::GuitarAmp, FxPos::Ins).unwrap() {
            v.insert(a, x);
        }
        for ch in [1, 2] {
            for (a, x) in set_insert(&v, ch, FxKind::ChannelStrip, FxPos::Mon).unwrap() {
                v.insert(a, x);
            }
        }
        assert!(set_insert(&v, 3, FxKind::ChannelStrip, FxPos::Mon).is_err());
        assert!(set_insert(&v, 3, FxKind::GuitarAmp, FxPos::Mon).is_err());
        assert_eq!(insert_of(&v, 2).strip, 2);
    }
}
