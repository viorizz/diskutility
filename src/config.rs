//! Small persisted user settings (`%APPDATA%\diskutility\config.json`):
//! the preferred backup destination, the automatic-backup schedule, and
//! whether updates should be installed automatically on launch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::logger;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Folder that backup images default to. Stored as typed by the user
    /// (after mapped-drive → UNC resolution, see `App::validate_backup_dir`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
    /// Install a newer release automatically when the app starts (Shift+U → a).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_update: bool,
    /// Automatic backup registered in Windows Task Scheduler (`a` key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
}

/// How often the scheduled backup runs. Mirrors `schtasks /SC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Frequency {
    Minutes,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Frequency {
    pub const ALL: [Frequency; 6] = [
        Frequency::Minutes,
        Frequency::Hourly,
        Frequency::Daily,
        Frequency::Weekly,
        Frequency::Monthly,
        Frequency::Yearly,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Frequency::Minutes => "every N minutes",
            Frequency::Hourly => "every N hours",
            Frequency::Daily => "daily",
            Frequency::Weekly => "weekly",
            Frequency::Monthly => "monthly",
            Frequency::Yearly => "yearly",
        }
    }
}

/// A scheduled full-disk backup. The disk is identified by serial + size +
/// name (never by its number, which shifts when drives are plugged in).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub disk_serial: String,
    pub disk_name: String,
    pub disk_size: u64,
    /// Folder the images land in (normally `Config::backup_dir` at save time).
    pub dest_dir: String,
    pub frequency: Frequency,
    /// Repeat interval for `Minutes` (1–1439) and `Hourly` (1–23).
    pub every: u32,
    pub hour: u32,
    pub minute: u32,
    /// 0 = Monday … 6 = Sunday (weekly).
    pub weekday: u32,
    /// 1–28 (monthly / yearly).
    pub day: u32,
    /// 1–12 (yearly).
    pub month: u32,
    /// Images to keep for this schedule; older ones are deleted after a
    /// successful backup. 0 = keep everything.
    pub keep: u32,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            disk_serial: String::new(),
            disk_name: String::new(),
            disk_size: 0,
            dest_dir: String::new(),
            frequency: Frequency::Daily,
            every: 30,
            hour: 3,
            minute: 0,
            weekday: 6,
            day: 1,
            month: 1,
            keep: 3,
        }
    }
}

pub const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
pub const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June",
    "July", "August", "September", "October", "November", "December",
];

impl Schedule {
    /// Human summary: "daily at 03:00", "weekly on Sunday at 03:00", …
    pub fn describe(&self) -> String {
        let at = format!("{:02}:{:02}", self.hour, self.minute);
        match self.frequency {
            Frequency::Minutes => format!("every {} minute(s)", self.every),
            Frequency::Hourly => format!("every {} hour(s) from {at}", self.every),
            Frequency::Daily => format!("daily at {at}"),
            Frequency::Weekly => format!("weekly on {} at {at}", WEEKDAYS[self.weekday as usize % 7]),
            Frequency::Monthly => format!("monthly on day {} at {at}", self.day),
            Frequency::Yearly => format!(
                "yearly on {} {} at {at}",
                MONTHS[(self.month.clamp(1, 12) - 1) as usize],
                self.day
            ),
        }
    }
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
