//! Control channel from the launcher (`ur44-open`, or a second `ur44-mixer`) to the running
//! background agent: one command line per connection ("show", "settings", "quit").
//!
//!   Unix:    a socket file, `agent.sock` in the support folder.
//!   Windows: TCP on 127.0.0.1 (random port). The port and a per-run token are written to
//!            `agent.port` in the support folder (%APPDATA%, private to the user); a request
//!            must carry the token, and the agent answers "ok" so the sender knows it was heard.

use crate::paths::support_dir;
use std::io::{BufRead, BufReader};

#[cfg(unix)]
mod imp {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    fn socket_path() -> PathBuf {
        support_dir().join("agent.sock")
    }

    pub struct Listener(UnixListener);

    impl Listener {
        pub fn bind() -> std::io::Result<Listener> {
            let path = socket_path();
            let _ = std::fs::create_dir_all(support_dir());
            let _ = std::fs::remove_file(&path);
            UnixListener::bind(&path).map(Listener)
        }

        pub fn incoming(&self) -> impl Iterator<Item = String> + '_ {
            self.0.incoming().flatten().map(|s| {
                let mut line = String::new();
                let _ = BufReader::new(s).read_line(&mut line);
                line.trim().to_string()
            })
        }
    }

    pub fn send(cmd: &str) -> bool {
        match UnixStream::connect(socket_path()) {
            Ok(mut s) => {
                let _ = writeln!(s, "{cmd}");
                true
            }
            Err(_) => false,
        }
    }

    pub fn cleanup() {
        let _ = std::fs::remove_file(socket_path());
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::time::Duration;

    fn port_file() -> PathBuf {
        support_dir().join("agent.port")
    }

    fn new_token() -> String {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut parts = [0u64; 2];
        for p in &mut parts {
            let mut h = RandomState::new().build_hasher();
            h.write_u32(std::process::id());
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            );
            *p = h.finish();
        }
        format!("{:016x}{:016x}", parts[0], parts[1])
    }

    pub struct Listener {
        tcp: TcpListener,
        token: String,
    }

    impl Listener {
        pub fn bind() -> std::io::Result<Listener> {
            let tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
            let port = tcp.local_addr()?.port();
            let token = new_token();
            std::fs::create_dir_all(support_dir())?;
            std::fs::write(port_file(), format!("{port} {token}\n"))?;
            Ok(Listener { tcp, token })
        }

        pub fn incoming(&self) -> impl Iterator<Item = String> + '_ {
            self.tcp.incoming().flatten().filter_map(|s| {
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                let mut reader = BufReader::new(s.take(256));
                reader.read_line(&mut line).ok()?;
                let (token, cmd) = line.trim().split_once(' ')?;
                if token != self.token {
                    return None;
                }
                let mut s = reader.into_inner().into_inner();
                let _ = s.write_all(b"ok\n");
                Some(cmd.to_string())
            })
        }
    }

    pub fn send(cmd: &str) -> bool {
        let Ok(info) = std::fs::read_to_string(port_file()) else {
            return false;
        };
        let Some((port, token)) = info.trim().split_once(' ') else {
            return false;
        };
        let Ok(port) = port.parse::<u16>() else {
            return false;
        };
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
            return false;
        };
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        if writeln!(s, "{token} {cmd}").is_err() {
            return false;
        }
        let mut reply = String::new();
        BufReader::new(s).read_line(&mut reply).is_ok() && reply.trim() == "ok"
    }

    pub fn cleanup() {
        let _ = std::fs::remove_file(port_file());
    }
}

pub use imp::{Listener, cleanup, send};
