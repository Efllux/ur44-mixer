//! Launcher behind "UR44 Mixer.app": asks the background agent to show its window and exits.
//! If the agent isn't running, starts it (it then opens its window by itself).

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

fn main() {
    let what = std::env::args().nth(1).unwrap_or_else(|| "show".into());
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let sock = home.join("Library/Application Support/UR44 Mixer/agent.sock");
    if let Ok(mut s) = UnixStream::connect(&sock) {
        let _ = writeln!(s, "{what}");
        return;
    }
    // Agent not running: start it from inside this bundle.
    let exe = std::env::current_exe().unwrap_or_default();
    let agent = exe
        .parent() // MacOS
        .and_then(|p| p.parent()) // Contents
        .map(|c| c.join("Helpers/UR44 Mixer Agent.app"))
        .unwrap_or_default();
    let _ = std::process::Command::new("/usr/bin/open").arg(&agent).status();
}
