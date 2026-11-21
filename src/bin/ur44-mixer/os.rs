//! Wording and features that differ between macOS and Windows (and other systems).

/// The native glass window background exists only on macOS (see glass.rs).
pub const GLASS: bool = cfg!(target_os = "macos");

#[cfg(target_os = "macos")]
mod text {
    pub const CONNECT_NOTE: &str = "Plug the UR44 straight into this Mac (avoid unpowered hubs).";
    pub const FILE_MANAGER: &str = "Show in Finder";
    pub const TRAY: &str = "Menu bar";
    pub const TRAY_UNAVAILABLE: &str = "Menu bar icon unavailable";
    pub const HIDDEN_NOTE: &str = "Invisible: no icon anywhere. Open \u{201c}UR44 Mixer\u{201d} from Applications or Spotlight to show this window.";
    pub const SAMPLE_RATE_NOTE: &str =
        "Set in Audio MIDI Setup or by your recording software. Effects need 96 kHz or lower.";
}

#[cfg(windows)]
mod text {
    pub const CONNECT_NOTE: &str = "Plug the UR44 straight into this PC (avoid unpowered hubs).";
    pub const FILE_MANAGER: &str = "Show in Explorer";
    pub const TRAY: &str = "Notification area";
    pub const TRAY_UNAVAILABLE: &str = "Notification area icon unavailable";
    pub const HIDDEN_NOTE: &str = "Invisible: no icon anywhere. Open \u{201c}UR44 Mixer\u{201d} from the Start menu to show this window.";
    pub const SAMPLE_RATE_NOTE: &str = "Set in the Windows sound settings, the UR44 driver panel or your recording software. Effects need 96 kHz or lower.";
}

#[cfg(not(any(target_os = "macos", windows)))]
mod text {
    pub const CONNECT_NOTE: &str =
        "Plug the UR44 straight into this computer (avoid unpowered hubs).";
    pub const FILE_MANAGER: &str = "Show folder";
    pub const TRAY: &str = "System tray";
    pub const TRAY_UNAVAILABLE: &str = "System tray icon unavailable";
    pub const HIDDEN_NOTE: &str =
        "Invisible: no icon anywhere. Start \u{201c}ur44-open\u{201d} to show this window.";
    pub const SAMPLE_RATE_NOTE: &str =
        "Set by your sound server or recording software. Effects need 96 kHz or lower.";
}

pub use text::*;

/// The waiting screen's note about CC MODE. Only macOS has a Yamaha-mode link (our own audio
/// driver + USB interface 4); elsewhere the mixer is reached over class-compliant MIDI only.
#[cfg(target_os = "macos")]
pub const CC_MODE_TITLE: &str = "If you use CC MODE";
#[cfg(target_os = "macos")]
pub const CC_MODE_NOTE: &str = "The UR44 works with this app with CC MODE on or off. CC MODE is only read at power-on: after changing it, switch the UR44 off and on again.";
#[cfg(not(target_os = "macos"))]
pub const CC_MODE_TITLE: &str = "Turn CC MODE on";
#[cfg(not(target_os = "macos"))]
pub const CC_MODE_NOTE: &str = "On this system the app reaches the mixer through the UR44\u{2019}s class-compliant MIDI port, which needs CC MODE on. CC MODE is only read at power-on: after changing it, switch the UR44 off and on again.";

/// Where Sweet Spot presets were looked for (shown when none are found).
pub fn presets_missing() -> String {
    let dirs: Vec<String> = ur44::ssd::search_dirs()
        .iter()
        .map(|d| d.display().to_string())
        .collect();
    if dirs.is_empty() {
        "Sweet Spot presets not found (set UR44_SSMD_DIR to their folder).".into()
    } else if cfg!(target_os = "macos") {
        format!("Sweet Spot presets not found in {}", dirs.join(", "))
    } else {
        "Sweet Spot presets not found. Install Yamaha\u{2019}s TOOLS for UR44 or set UR44_SSMD_DIR to the SSMD folder.".into()
    }
}
