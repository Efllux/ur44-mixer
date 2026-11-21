//! Hand-written knowledge layered on top of the generated `params` table:
//! what the user may change, and human-readable labels (from the UR44 Operation Manual).

use crate::params::{self, Group, ParamDef};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Normal mixer setting; safe to change from software.
    ReadWrite,
    /// Mirrors hardware state (the two front-panel +48V switches, the sample rate set by macOS).
    /// dspMixFx only displays these.
    ReadOnly,
    /// Present in Yamaha's shared table but not used for the UR44; never shown or written.
    Hidden,
}

pub fn access(p: &ParamDef) -> Access {
    match p.name {
        "HaP48VOnf" | "SysFsCode" => Access::ReadOnly,
        _ => Access::ReadWrite,
    }
}

/// Human-readable channel label for a parameter + channel index.
pub fn channel_label(p: &ParamDef, ch: u16) -> String {
    match p.group {
        Group::Input | Group::InputSend => format!("Input {}", ch + 1),
        // Head-amp settings are per input pair; per the manual only 1/2 and 3/4 have +48V.
        Group::HeadAmp => match ch {
            0 => "Inputs 1/2 (MIC/HI-Z)".into(),
            1 => "Inputs 3/4 (MIC/LINE)".into(),
            _ => "Inputs 5/6 (LINE)".into(),
        },
        Group::DawReturn => "DAW".into(),
        Group::ChannelStrip => format!("Channel Strip {}", ch + 1),
        _ if p.channels == 1 => String::new(),
        _ => format!("#{}", ch + 1),
    }
}

/// Is this (parameter, channel) meaningful on the UR44?
pub fn is_present(p: &ParamDef, ch: u16) -> bool {
    if ch >= p.channels || access(p) == Access::Hidden {
        return false;
    }
    match p.name {
        // LINE INPUT 5/6 has no phantom power (manual p.7).
        "HaP48VOnf" => ch < 2,
        // "INPUT 5/6 LEVEL" (+4 dBu / -10 dBV, manual p.14) is the only head-amp
        // switch dspMixFx exposes; channels 0/1 of this parameter are unused.
        "HaInputGain" => ch == 2,
        _ => true,
    }
}

pub fn visible() -> impl Iterator<Item = &'static ParamDef> {
    params::PARAMS.iter().filter(|p| access(p) != Access::Hidden)
}

/// Display helpers shared by the CLI and the mixer app.
pub mod display {
    /// 0..127 mixer level. Known points: 0 = -inf, 103 = 0 dB (unity), 127 = +6 dB.
    pub fn level(v: i32) -> String {
        match v {
            0 => "-\u{221e}".into(),
            103 => "0 dB".into(),
            127 => "+6 dB".into(),
            _ => v.to_string(),
        }
    }

    pub fn pan(v: i32) -> String {
        match v {
            0 => "C".into(),
            v if v < 0 => format!("L{}", -v),
            v => format!("R{v}"),
        }
    }

    /// HPF cutoff options in manual order. Index mapping not yet confirmed on hardware.
    pub const HPF: [&str; 5] = ["120 Hz", "100 Hz", "80 Hz", "60 Hz", "40 Hz"];
    pub const REVERB_TYPES: [&str; 3] = ["Hall", "Room", "Plate"];
    /// Order as stored in MemAPI's string table (+4dBu precedes -10dBV).
    pub const INPUT56_LEVEL: [&str; 2] = ["+4 dBu", "-10 dBV"];
    pub const SAMPLE_RATES: [&str; 6] = ["44.1 kHz", "48 kHz", "88.2 kHz", "96 kHz", "176.4 kHz", "192 kHz"];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phantom_is_read_only_and_labelled() {
        let p = params::by_name("HaP48VOnf").unwrap();
        assert_eq!(access(p), Access::ReadOnly);
        assert_eq!(channel_label(p, 0), "Inputs 1/2 (MIC/HI-Z)");
        assert!(is_present(p, 1));
        assert!(!is_present(p, 2));
    }

    #[test]
    fn input56_level_only_on_line_pair() {
        let p = params::by_name("HaInputGain").unwrap();
        assert!(!is_present(p, 0));
        assert!(is_present(p, 2));
        assert_eq!(access(p), Access::ReadWrite);
    }

    #[test]
    fn display_helpers() {
        assert_eq!(display::level(103), "0 dB");
        assert_eq!(display::pan(-16), "L16");
        assert_eq!(display::pan(3), "R3");
    }
}
