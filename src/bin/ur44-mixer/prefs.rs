//! Background-app preferences and the login item (no admin rights on either platform):
//!   macOS:   a per-user LaunchAgent;
//!   Windows: a value under HKCU\Software\Microsoft\Windows\CurrentVersion\Run.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrayMode {
    MenuBar,
    Hidden,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prefs {
    pub start_at_login: bool,
    pub tray: TrayMode,
    pub restore_on_connect: bool,
    /// Liquid Glass window background (translucent, native glass behind the controls).
    #[serde(default = "yes")]
    pub glass: bool,
}

fn yes() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            start_at_login: true,
            tray: TrayMode::MenuBar,
            restore_on_connect: true,
            glass: true,
        }
    }
}

pub fn support_dir() -> PathBuf {
    ur44::paths::support_dir()
}

fn prefs_path() -> PathBuf {
    support_dir().join("settings.json")
}

pub fn state_path() -> PathBuf {
    support_dir().join("last-state.json")
}

impl Prefs {
    pub fn load() -> Prefs {
        std::fs::read(prefs_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(support_dir());
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(prefs_path(), b);
        }
    }
}

#[cfg(target_os = "macos")]
mod login {
    use std::path::PathBuf;

    const AGENT_LABEL: &str = "local.ur44.mixer.agent";

    fn agent_plist() -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        home.join("Library/LaunchAgents")
            .join(format!("{AGENT_LABEL}.plist"))
    }

    /// Install or remove the login item. Only installed when running from an app bundle, so
    /// development builds in target/ never register themselves.
    pub fn set_login_item(enabled: bool) -> Result<(), String> {
        let path = agent_plist();
        if !enabled {
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe = exe.to_string_lossy();
        if !exe.contains(".app/Contents/MacOS/") {
            return Err("Start at login is only available in the installed app.".into());
        }
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{AGENT_LABEL}</string>
  <key>ProgramArguments</key><array><string>{exe}</string><string>--background</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
  <key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#
        );
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, plist).map_err(|e| e.to_string())
    }

    pub fn login_item_installed() -> bool {
        agent_plist().exists()
    }
}

#[cfg(windows)]
mod login {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "UR44 Mixer";

    /// The command line registered for this executable.
    fn command() -> Result<String, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        Ok(format!("\"{}\" --background", exe.display()))
    }

    /// Add or remove the Run value. Only added for an installed copy (not a cargo build in
    /// target\), so development builds never register themselves.
    pub fn set_login_item(enabled: bool) -> Result<(), String> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_SET_VALUE)
            .map_err(|e| format!("Cannot open the Run key: {e}"))?;
        if !enabled {
            return match key.delete_value(VALUE) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            };
        }
        let cmd = command()?;
        if cmd.to_ascii_lowercase().contains(r"\target\") {
            return Err("Start at login is only available in the installed app.".into());
        }
        key.set_value(VALUE, &cmd).map_err(|e| e.to_string())
    }

    /// Registered, and pointing at this executable.
    pub fn login_item_installed() -> bool {
        let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_READ)
        else {
            return false;
        };
        let current: Result<String, _> = key.get_value(VALUE);
        matches!((current, command()), (Ok(cur), Ok(cmd)) if cur.eq_ignore_ascii_case(&cmd))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod login {
    pub fn set_login_item(enabled: bool) -> Result<(), String> {
        if enabled {
            Err("Start at login is not supported on this system yet.".into())
        } else {
            Ok(())
        }
    }

    pub fn login_item_installed() -> bool {
        false
    }
}

pub use login::{login_item_installed, set_login_item};
