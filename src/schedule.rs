//! Automatic backups through Windows Task Scheduler.
//!
//! The TUI (`a` key) saves a `Schedule` to config.json and registers a task
//! named `DiskUtility Backup` with `schtasks.exe`. The task runs
//! `diskutility --scheduled-backup` elevated, whether or not the TUI is open —
//! nothing has to stay resident or autostart. The task is bound to the
//! logged-on user's interactive session ("run only when user is logged on"),
//! which is what lets it reach mapped/UNC network destinations without
//! storing a password.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use crate::app::AppEvent;
use crate::config::{self, Frequency, Schedule};
use crate::disks::{self, Disk};
use crate::logger;
use crate::ops::{self, OpEvent};

pub const TASK_NAME: &str = "DiskUtility Backup";
pub const CLI_FLAG: &str = "--scheduled-backup";

fn schtasks_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    PathBuf::from(root).join(r"System32\schtasks.exe")
}

fn run_schtasks(args: &[String]) -> Result<String, String> {
    logger::log(format!("schtasks {}", args.join(" ")));
    let out = std::process::Command::new(schtasks_exe())
        .args(args)
        .output()
        .map_err(|e| format!("cannot run schtasks.exe: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !stdout.is_empty() {
        logger::log(format!("schtasks: {stdout}"));
    }
    if out.status.success() {
        Ok(stdout)
    } else {
        let msg = if stderr.is_empty() { stdout } else { stderr };
        Err(msg.trim_start_matches("ERROR: ").to_string())
    }
}

/// `schtasks /Create` trigger arguments for a schedule.
pub fn trigger_args(s: &Schedule) -> Vec<String> {
    let at = format!("{:02}:{:02}", s.hour % 24, s.minute % 60);
    let mut a: Vec<String> = Vec::new();
    match s.frequency {
        Frequency::Minutes => {
            a.extend(["/SC".into(), "MINUTE".into(), "/MO".into(), s.every.clamp(1, 1439).to_string()]);
        }
        Frequency::Hourly => {
            a.extend(["/SC".into(), "HOURLY".into(), "/MO".into(), s.every.clamp(1, 23).to_string(), "/ST".into(), at]);
        }
        Frequency::Daily => a.extend(["/SC".into(), "DAILY".into(), "/ST".into(), at]),
        Frequency::Weekly => {
            const D: [&str; 7] = ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"];
            a.extend(["/SC".into(), "WEEKLY".into(), "/D".into(), D[s.weekday as usize % 7].into(), "/ST".into(), at]);
        }
        Frequency::Monthly => {
            a.extend(["/SC".into(), "MONTHLY".into(), "/D".into(), s.day.clamp(1, 31).to_string(), "/ST".into(), at]);
        }
        Frequency::Yearly => {
            const M: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
            a.extend([
                "/SC".into(), "MONTHLY".into(),
                "/M".into(), M[(s.month.clamp(1, 12) - 1) as usize].into(),
                "/D".into(), s.day.clamp(1, 31).to_string(),
                "/ST".into(), at,
            ]);
        }
    }
    a
}

/// Register (or replace) the scheduled task for `s`, pointing at this exe.
pub fn install(s: &Schedule) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate own exe: {e}"))?;
    let exe = exe.to_str().ok_or("exe path is not valid UTF-8")?;
    if exe.contains('"') {
        return Err("exe path contains a quote character".into());
    }
    let mut args: Vec<String> = vec![
        "/Create".into(),
        "/F".into(),
        "/TN".into(),
        TASK_NAME.into(),
        "/TR".into(),
        format!("\"{exe}\" {CLI_FLAG}"),
        "/RL".into(),
        "HIGHEST".into(),
    ];
    args.extend(trigger_args(s));
    run_schtasks(&args).map(|_| ())
}

pub fn remove() -> Result<(), String> {
    run_schtasks(&["/Delete".into(), "/F".into(), "/TN".into(), TASK_NAME.into()]).map(|_| ())
}

/// True if the task exists in Task Scheduler.
pub fn is_installed() -> bool {
    run_schtasks(&["/Query".into(), "/TN".into(), TASK_NAME.into()]).is_ok()
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(24).collect()
}

/// Image file name for a scheduled run: `auto-<name>-<serial>-<timestamp>.img`.
fn image_name(disk: &Disk) -> String {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    format!("auto-{}-{}-{stamp}.img", clean(&disk.name), clean(&disk.serial))
}

/// Prefix shared by every image of this schedule — used for retention.
fn image_prefix(s: &Schedule) -> String {
    format!("auto-{}-{}-", clean(&s.disk_name), clean(&s.disk_serial))
}

/// Delete the oldest images beyond `keep` (by name — the timestamp sorts).
fn prune(s: &Schedule, dir: &std::path::Path) {
    if s.keep == 0 {
        return;
    }
    let prefix = image_prefix(s);
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut imgs: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(&prefix) && n.ends_with(".img"))
        .collect();
    imgs.sort();
    while imgs.len() > s.keep as usize {
        let old = imgs.remove(0);
        match std::fs::remove_file(dir.join(&old)) {
            Ok(()) => logger::log(format!("scheduled backup: pruned old image {old}")),
            Err(e) => logger::log(format!("scheduled backup: could not prune {old}: {e}")),
        }
    }
}

/// Headless entry point for the task: find the disk by identity, image it
/// into the scheduled folder, then prune old images. Prints progress to
/// stdout (visible if run by hand) and logs everything.
pub fn run_headless() -> Result<String, String> {
    let cfg = config::load();
    let s = cfg.schedule.ok_or("no schedule in config.json — set one up with the a key in the TUI")?;
    logger::log(format!(
        "scheduled backup: start — disk '{}' serial {} ({}) → {} · {}",
        s.disk_name,
        s.disk_serial,
        disks::human(s.disk_size),
        s.dest_dir,
        s.describe()
    ));
    if !ops::is_elevated() {
        return Err("scheduled backup needs administrator rights (the task should have RunLevel HIGHEST)".into());
    }
    let list = disks::enumerate().map_err(|e| format!("disk scan failed: {e}"))?;
    let disk = list
        .into_iter()
        .find(|d| {
            d.size == s.disk_size
                && (if s.disk_serial.is_empty() { d.name == s.disk_name } else { d.serial == s.disk_serial })
        })
        .ok_or_else(|| {
            format!(
                "scheduled disk not connected: '{}' serial {} ({}) — nothing was done",
                s.disk_name,
                s.disk_serial,
                disks::human(s.disk_size)
            )
        })?;
    let dir = std::path::Path::new(&s.dest_dir);
    if !dir.is_dir() {
        return Err(format!("backup folder not reachable: {}", s.dest_dir));
    }
    if let Some(free) = ops::free_space(&dir.join("x")) {
        if free < disk.size {
            // make room first if retention allows, then re-check
            prune(&s, dir);
            let free = ops::free_space(&dir.join("x")).unwrap_or(0);
            if free < disk.size {
                return Err(format!(
                    "not enough free space in {}: {} available, {} needed",
                    s.dest_dir,
                    disks::human(free),
                    disks::human(disk.size)
                ));
            }
        }
    }
    let path = dir.join(image_name(&disk));
    println!("diskutility: backing up disk {} ({}) → {}", disk.number, disk.name, path.display());
    let (tx, rx) = mpsc::channel::<AppEvent>();
    let cancel = Arc::new(AtomicBool::new(false));
    ops::spawn_backup(tx, disk, path, cancel);
    let mut last_pct = -1i64;
    let result = loop {
        match rx.recv() {
            Ok(AppEvent::Op(OpEvent::Log(l))) => {
                logger::log(format!("op: {l}"));
                println!("  {l}");
            }
            Ok(AppEvent::Op(OpEvent::Progress(frac, detail))) => {
                let pct = (frac * 100.0) as i64;
                if pct / 5 != last_pct / 5 {
                    last_pct = pct;
                    println!("  {pct:>3}%  {detail}");
                }
            }
            Ok(AppEvent::Op(OpEvent::Done(r))) => break r,
            Ok(_) => {}
            Err(_) => break Err("backup worker exited without reporting a result".into()),
        }
    };
    match &result {
        Ok(m) => {
            logger::log(format!("scheduled backup SUCCEEDED: {m}"));
            prune(&s, dir);
        }
        Err(e) => logger::log(format!("scheduled backup FAILED: {e}")),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_args_cover_every_frequency() {
        let mut s = Schedule { hour: 3, minute: 30, ..Default::default() };
        s.frequency = Frequency::Daily;
        assert_eq!(trigger_args(&s), ["/SC", "DAILY", "/ST", "03:30"]);
        s.frequency = Frequency::Weekly;
        s.weekday = 6;
        assert_eq!(trigger_args(&s), ["/SC", "WEEKLY", "/D", "SUN", "/ST", "03:30"]);
        s.frequency = Frequency::Minutes;
        s.every = 15;
        assert_eq!(trigger_args(&s), ["/SC", "MINUTE", "/MO", "15"]);
        s.frequency = Frequency::Yearly;
        s.month = 12;
        s.day = 24;
        assert_eq!(trigger_args(&s), ["/SC", "MONTHLY", "/M", "DEC", "/D", "24", "/ST", "03:30"]);
    }

    #[test]
    fn image_prefix_matches_image_name() {
        let s = Schedule { disk_name: "WD BLACK".into(), disk_serial: "AB/12".into(), ..Default::default() };
        let d = Disk { name: "WD BLACK".into(), serial: "AB/12".into(), ..Default::default() };
        assert!(image_name(&d).starts_with(&image_prefix(&s)));
    }
}
