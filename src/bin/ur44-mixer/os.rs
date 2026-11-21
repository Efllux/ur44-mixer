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
