//! Launcher behind "UR44 Mixer.app" (macOS) / the "UR44 Mixer" Start menu shortcut (Windows):
//! asks the background agent to show its window and exits. If the agent isn't running, starts
//! it (it then opens its window by itself).

// Windows release builds: no console window flashing up.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    let what = std::env::args().nth(1).unwrap_or_else(|| "show".into());
    allow_agent_to_focus();
    if ur44::ipc::send(&what) {
        return;
    }
    start_agent();
}

/// Agent not running: start it from inside this bundle.
#[cfg(target_os = "macos")]
fn start_agent() {
    let exe = std::env::current_exe().unwrap_or_default();
    let agent = exe
        .parent() // MacOS
        .and_then(|p| p.parent()) // Contents
        .map(|c| c.join("Helpers/UR44 Mixer Agent.app"))
        .unwrap_or_default();
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(&agent)
        .status();
}

/// Agent not running: start `ur44-mixer` from the same folder as this launcher.
#[cfg(not(target_os = "macos"))]
fn start_agent() {
    let exe = std::env::current_exe().unwrap_or_default();
    let agent = exe.with_file_name(if cfg!(windows) {
        "ur44-mixer.exe"
    } else {
        "ur44-mixer"
    });
    let _ = std::process::Command::new(agent).spawn();
}

/// Windows only lets the process the user just started take the foreground. The agent is
/// another process, so pass that right on before asking it to show its window.
#[cfg(windows)]
fn allow_agent_to_focus() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
    // SAFETY: plain Win32 call without pointers.
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
    }
}

#[cfg(not(windows))]
fn allow_agent_to_focus() {}
