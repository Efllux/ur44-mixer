//! Per-user data folder (settings, last state, scenes, the launcher endpoint).
//!   macOS:   ~/Library/Application Support/UR44 Mixer
//!   Windows: %APPDATA%\UR44 Mixer
//!   others:  $XDG_CONFIG_HOME/ur44-mixer (or ~/.config/ur44-mixer)

use std::path::PathBuf;

#[cfg(target_os = "macos")]
pub fn support_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Library/Application Support/UR44 Mixer")
}

#[cfg(windows)]
pub fn support_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|h| PathBuf::from(h).join("AppData").join("Roaming"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("UR44 Mixer")
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn support_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ur44-mixer")
}

/// Open a folder in the system file manager (Finder, Explorer, …).
pub fn reveal(dir: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(windows)]
    let opener = "explorer";
    #[cfg(not(any(target_os = "macos", windows)))]
    let opener = "xdg-open";
    let _ = std::process::Command::new(opener).arg(dir).spawn();
}
