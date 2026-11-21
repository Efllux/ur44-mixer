//! Scenes: complete mixer snapshots stored as JSON files (one per scene) in
//! ~/Library/Application Support/UR44 Mixer/Scenes. Import/export = copy files in Finder.

use crate::catalog::{self, Access};
use crate::logic::{self, Values, Writes};
use crate::params;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub ch: u16,
    pub value: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scene {
    pub name: String,
    pub device: String,
    pub format: u32,
    pub values: Vec<Entry>,
}

pub fn dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Library/Application Support/UR44 Mixer/Scenes")
}

/// Is this parameter part of a scene? Hardware-mirrored values are excluded.
fn in_scene(p: &params::ParamDef, ch: u16) -> bool {
    catalog::is_present(p, ch) && catalog::access(p) == Access::ReadWrite
}

pub fn capture(name: &str, v: &Values) -> Scene {
    let mut values = Vec::new();
    for p in params::PARAMS.iter() {
        for ch in (0..p.channels).filter(|&c| in_scene(p, c)) {
            values.push(Entry { name: p.name.into(), ch, value: logic::get(v, p.name, ch) });
        }
    }
    Scene { name: name.into(), device: "Steinberg UR44".into(), format: 1, values }
}

/// Writes that recall a scene. Effect inserts go last so the effects are configured first.
pub fn recall(s: &Scene) -> Writes {
    let mut w: Writes = Vec::new();
    let mut inserts: Writes = Vec::new();
    for e in &s.values {
        let Some(p) = params::by_name(&e.name) else { continue };
        if !in_scene(p, e.ch) {
            continue;
        }
        let a = logic::addr(p.name, e.ch);
        let x = e.value.clamp(p.min, p.max);
        if p.name.contains("InsP") {
            inserts.push((a, x));
        } else {
            w.push((a, x));
        }
    }
    w.extend(inserts);
    w
}

fn safe_file_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || " -_().".contains(c) { c } else { '_' }).collect();
    let s = s.trim().to_string();
    if s.is_empty() { "Untitled".into() } else { s }
}

pub fn save(s: &Scene) -> Result<PathBuf> {
    std::fs::create_dir_all(dir())?;
    let path = dir().join(format!("{}.json", safe_file_name(&s.name)));
    std::fs::write(&path, serde_json::to_vec_pretty(s)?).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn list() -> Vec<Scene> {
    let mut out: Vec<Scene> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|f| f.path().extension().is_some_and(|e| e == "json"))
        .filter_map(|f| serde_json::from_slice(&std::fs::read(f.path()).ok()?).ok())
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

pub fn delete(name: &str) -> Result<()> {
    let path = dir().join(format!("{}.json", safe_file_name(name)));
    std::fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_recall_roundtrip() {
        let mut v = Values::new();
        v.insert(logic::addr("MasterMixBus1Level", 0), 99);
        v.insert(logic::addr("HaP48VOnf", 0), 1); // read-only: must not be recalled
        let s = capture("t", &v);
        let w = recall(&s);
        assert!(w.contains(&(logic::addr("MasterMixBus1Level", 0), 99)));
        assert!(!w.iter().any(|(a, _)| *a == logic::addr("HaP48VOnf", 0)));
        // inserts are written last
        let first_insert = w.iter().position(|(a, _)| *a == logic::addr("ChFxInsPrePatch", 0)).unwrap();
        assert!(w[first_insert..].iter().all(|(a, _)| params::by_id(a.id).unwrap().name.contains("InsP")));
    }
}
