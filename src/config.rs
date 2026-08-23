//! Small persisted user settings (`%APPDATA%\diskutility\config.json`):
//! the preferred backup destination, the automatic-backup schedule, and
//! whether updates should be installed automatically on launch.

use serde::{Deserialize, Serialize};
use utility_core::config::CoreSettings;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// `auto_update` and `notify`, shared with every *Utility tool (flattened:
    /// the JSON keys are unchanged).
    #[serde(flatten)]
    pub core: CoreSettings,
    /// Folder that backup images default to. Stored as typed by the user
    /// (after mapped-drive → UNC resolution, see `App::validate_backup_dir`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
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
    /// Unix seconds until which scheduled runs are skipped; `u64::MAX` =
    /// paused until resumed by hand. The task stays registered — the runner
    /// itself checks this and exits quietly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<u64>,
}

/// Marker value for "paused until I resume it".
pub const PAUSED_INDEFINITELY: u64 = u64::MAX;

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
            paused_until: None,
        }
    }
}

pub const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
pub const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June",
    "July", "August", "September", "October", "November", "December",
];

impl Schedule {
    pub fn is_paused(&self, now: u64) -> bool {
        self.paused_until.is_some_and(|t| t > now)
    }

    /// "paused until 18:30", "paused until Tue 08:00", "paused until resumed"
    /// — or None when active. An expired pause reads as active.
    pub fn pause_text(&self, now: u64) -> Option<String> {
        let t = self.paused_until?;
        if t <= now {
            return None;
        }
        if t == PAUSED_INDEFINITELY {
            return Some("paused until you resume it".into());
        }
        let when = chrono::DateTime::from_timestamp(t as i64, 0)
            .map(|d| d.with_timezone(&chrono::Local))
            .map(|d| {
                if t - now < 20 * 3600 {
                    d.format("%H:%M").to_string()
                } else {
                    d.format("%a %d %b %H:%M").to_string()
                }
            })
            .unwrap_or_default();
        Some(format!("paused until {when}"))
    }

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

/// `%APPDATA%\diskutility` — config, log and the scheduled-backup status files.
pub use utility_core::config::dir;

pub fn load() -> Config {
    utility_core::config::load()
}

pub fn save(cfg: &Config) -> Result<(), String> {
    utility_core::config::save(cfg)
}

#[cfg(test)]
mod pause_tests {
    use super::*;

    #[test]
    fn pause_semantics_and_serde() {
        let mut s = Schedule::default();
        let now = 1_000_000;
        assert!(!s.is_paused(now));
        assert_eq!(s.pause_text(now), None);
        // timed pause, still in the future
        s.paused_until = Some(now + 3600);
        assert!(s.is_paused(now));
        assert!(s.pause_text(now).unwrap().starts_with("paused until "));
        // expired pause reads as active
        assert!(!s.is_paused(now + 3600));
        assert_eq!(s.pause_text(now + 3601), None);
        // indefinite
        s.paused_until = Some(PAUSED_INDEFINITELY);
        assert!(s.is_paused(u64::MAX - 1));
        assert_eq!(s.pause_text(now).as_deref(), Some("paused until you resume it"));
        // round-trips, and is omitted from JSON when unset
        let json = serde_json::to_string(&s).unwrap();
        let back: Schedule = serde_json::from_str(&json).unwrap();
        assert_eq!(back.paused_until, Some(PAUSED_INDEFINITELY));
        s.paused_until = None;
        assert!(!serde_json::to_string(&s).unwrap().contains("paused_until"));
        // old config.json without the field still loads
        let old: Schedule = serde_json::from_str(&json.replace(",\"paused_until\":18446744073709551615", "")).unwrap();
        assert_eq!(old.paused_until, None);
    }
}
