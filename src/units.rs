//! Raw parameter value → display text, using Yamaha's own display strings
//! (the strings the UR44 editor shows for each raw value).

#[path = "units_table.rs"]
mod table;
pub use table::*;

fn idx<'a>(t: &[&'a str], raw: i32, first: i32) -> &'a str {
    let i = (raw - first).clamp(0, t.len() as i32 - 1) as usize;
    t[i]
}

fn khz(s: &str) -> String {
    // Yamaha writes "20.0k"; show "20.0 kHz" / "500 Hz".
    match s.strip_suffix('k') {
        Some(k) => format!("{k} kHz"),
        None => format!("{s} Hz"),
    }
}

pub const KNEE: [&str; 3] = ["Soft", "Medium", "Hard"];
pub const GTR_TYPES: [&str; 4] = ["Clean", "Crunch", "Lead", "Drive"];

/// Human-readable value for a parameter. Falls back to the raw number.
pub fn format(name: &str, raw: i32) -> String {
    let n = name;
    if n.ends_with("Pan") || n.ends_with("Bal") && !n.starts_with("FxRev") {
        return idx(&PAN, raw, -16).to_string();
    }
    if n.ends_with("Level") {
        let s = idx(&FADER_DB, raw, 0);
        return if s == "-INF" { "-\u{221e}".into() } else { format!("{s} dB") };
    }
    match n {
        "ChHpfFreq" => format!("{} Hz", idx(&HPF_HZ, raw, 0)),
        "SysFsCode" => khz(idx(&SAMPLE_RATE, raw, 0)),
        "FxCompDrive" => idx(&DRIVE, raw, 0).to_string(),
        "FxCompAttack" | "MorphCompAttack" => format!("{} ms", idx(&ATTACK_MS, raw, 57)),
        "FxCompRelease" | "MorphCompRelease" => format!("{} ms", idx(&RELEASE_MS, raw, 24)),
        "FxCompRatio" | "MorphCompRatio" => {
            let s = idx(&RATIO, raw, 0);
            if s == "INF" { "\u{221e} : 1".into() } else { format!("{s} : 1") }
        }
        "FxCompKnee" | "MorphCompKnee" => KNEE[raw.clamp(0, 2) as usize].into(),
        "FxCompThreshOfst" | "FxCompMakeupOfst" => format!("{} dB", idx(&OFFSET_DB, raw, 0)),
        _ if n.ends_with("Freq") && (n.starts_with("FxComp") || n.starts_with("FxEq") || n.starts_with("Morph")) => {
            khz(idx(&FREQ, raw, 4))
        }
        _ if n.ends_with("Gain") && (n.starts_with("FxComp") || n.starts_with("FxEq") || n.starts_with("Morph")) => {
            format!("{} dB", idx(&GAIN_DB, raw, 0))
        }
        _ if n.ends_with("Qual") => idx(&Q, raw, 0).to_string(),
        "FxMorphValue" => format!("{:.1}", raw as f32 / 15.0 + 1.0),
        "FxRevType" => ["Hall", "Room", "Plate"][raw.clamp(0, 2) as usize].into(),
        "FxRevTime" => format!("{} s", idx(&REV_TIME_S, raw, 0)),
        "FxRevIniDly" => format!("{} ms", idx(&REV_INIDLY_MS, raw, 0)),
        "FxRevHpf" | "FxRevLpf" => khz(idx(&REV_FILTER_HZ, raw, 0)),
        "FxRevHighRatio" | "FxRevLowRatio" => idx(&REV_RATIO, raw, 1).to_string(),
        "FxRevBal" => format!("{} %", idx(&REV_BAL_PCT, raw, 1)),
        "FxRevLowFreq" => khz(idx(&REV_LOWFREQ_HZ, raw, 1)),
        "GtrFxType" => GTR_TYPES[raw.clamp(0, 3) as usize].into(),
        "GtrFxDriveType" => idx(&GTR_DRIVE_TYPE, raw, 0).to_string(),
        "GtrFxCrunchType" => ["Normal", "Bright"][raw.clamp(0, 1) as usize].into(),
        "GtrFxLeadSensitivity" => ["High", "Low"][raw.clamp(0, 1) as usize].into(),
        "GtrFxCleanChorus" => ["Chorus", "Off", "Vibrato"][raw.clamp(0, 2) as usize].into(),
        _ if n.ends_with("MicPosition") => ["Center", "Edge"][raw.clamp(0, 1) as usize].into(),
        _ if n.ends_with("SpType") => idx(&GTR_SPEAKER, raw, 0).to_string(),
        _ if n.starts_with("GtrFx") && !n.ends_with("Output") && !n.ends_with("SpAir") => {
            idx(&GTR_KNOB, raw, 0).to_string()
        }
        _ => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::format;

    #[test]
    fn known_values() {
        assert_eq!(format("ChSendMixBus1Level", 103), "0.00 dB");
        assert_eq!(format("ChSendMixBus1Level", 127), "6.00 dB");
        assert_eq!(format("ChSendMixBus1Level", 0), "-\u{221e}");
        assert_eq!(format("ChSendMixBus1Pan", -16), "L16");
        assert_eq!(format("ChHpfFreq", 2), "80 Hz");
        assert_eq!(format("FxEqMidFreq", 4), "20.0 Hz");
        assert_eq!(format("FxEqMidFreq", 124), "20.0 kHz");
        assert_eq!(format("FxEqHsvGain", 180), "0.0 dB");
        assert_eq!(format("FxCompAttack", 57), "0.092 ms");
        assert_eq!(format("FxCompRelease", 300), "999.0 ms");
        assert_eq!(format("FxEqMidQual", 60), "16.00");
        assert_eq!(format("SysFsCode", 1), "48 kHz");
        assert_eq!(format("GtrFxCleanSpType", 1), "BS 4x12");
    }
}
