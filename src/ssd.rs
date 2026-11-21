//! Sweet Spot Morphing Data (.ssd) files, installed by Yamaha's TOOLS in
//! /Library/Application Support/Yamaha/SSMD/<folder>/<NN Name>.ssd
//!
//! Format (512 bytes): "SSMD" magic, version, 4-char id at 0x10, then at 0x20
//! nine morph points × 20 big-endian u16 (17 used, in `logic::MORPH_ITEMS` order).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub const POINTS: usize = 9;
pub const VALUES: usize = 17;

#[derive(Debug, Clone, PartialEq)]
pub struct SweetSpot {
    /// Display name, e.g. "Engineers' Preset / 08 MR Vocal".
    pub name: String,
    pub path: PathBuf,
    pub points: [[i32; VALUES]; POINTS],
}

pub fn parse(bytes: &[u8]) -> Result<[[i32; VALUES]; POINTS]> {
    if bytes.len() < 0x20 + POINTS * 40 || &bytes[..4] != b"SSMD" {
        bail!("not a Sweet Spot (.ssd) file");
    }
    let mut pts = [[0i32; VALUES]; POINTS];
    for (p, point) in pts.iter_mut().enumerate() {
        for (i, v) in point.iter_mut().enumerate() {
            let o = 0x20 + p * 40 + i * 2;
            *v = i32::from(u16::from_be_bytes([bytes[o], bytes[o + 1]]));
        }
    }
    Ok(pts)
}

pub fn load(path: &Path) -> Result<SweetSpot> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let folder = path.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().trim().to_string()).unwrap_or_default();
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    Ok(SweetSpot { name: format!("{folder} / {stem}"), path: path.to_path_buf(), points: parse(&bytes)? })
}

pub const SYSTEM_DIR: &str = "/Library/Application Support/Yamaha/SSMD";

/// All installed Sweet Spot files, sorted by folder then name.
pub fn installed() -> Vec<SweetSpot> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(SYSTEM_DIR) else { return out };
    for d in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else { continue };
        for f in files.flatten() {
            if f.path().extension().is_some_and(|e| e.eq_ignore_ascii_case("ssd")) {
                if let Ok(s) = load(&f.path()) {
                    out.push(s);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_preset_point0() {
        // First point of "01 Basic.ssd" (bytes 0x20..), matches the UR44's morph table on 2026-11-06.
        let mut b = vec![0u8; 512];
        b[..4].copy_from_slice(b"SSMD");
        let p0: [u16; 17] = [184, 159, 30, 1, 100, 70, 30, 133, 12, 112, 180, 72, 180, 12, 32, 180, 180];
        for (i, v) in p0.iter().enumerate() {
            b[0x20 + i * 2..0x22 + i * 2].copy_from_slice(&v.to_be_bytes());
        }
        let pts = parse(&b).unwrap();
        assert_eq!(pts[0][0], 184);
        assert_eq!(pts[0][7], 133);
        assert_eq!(pts[0][16], 180);
    }

    #[test]
    fn installed_presets_if_present() {
        // Only meaningful on a machine with Yamaha's presets installed.
        let all = installed();
        if !all.is_empty() {
            assert!(all.iter().any(|s| s.name.ends_with("01 Basic")));
        }
    }
}
