//! Small persisted user settings (`%APPDATA%\diskutility\config.json`).
//! Currently just the preferred backup destination — typically a mapped
//! network drive or UNC share the user wants every `.img` to land on.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::logger;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Folder that backup images default to. Stored as typed by the user
    /// (after mapped-drive → UNC resolution, see `App::validate_backup_dir`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    Some(base.join("diskutility").join("config.json"))
}

pub fn load() -> Config {
    let Some(p) = path() else { return Config::default() };
    match std::fs::read_to_string(&p) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            logger::log(format!("config: {} is not valid JSON ({e}) — using defaults", p.display()));
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

pub fn save(cfg: &Config) -> Result<(), String> {
    let p = path().ok_or("APPDATA is not set — cannot save settings")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&p, text).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
    logger::log(format!("config saved to {}", p.display()));
    Ok(())
}
