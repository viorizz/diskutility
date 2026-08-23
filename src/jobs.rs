//! Registry of backups in progress, shared between processes.
//!
//! Every backup — the headless `--scheduled-backup` run and an interactive
//! `s` backup in the TUI — publishes its state to
//! `%APPDATA%\diskutility\jobs\<pid>.json` while it runs, so that:
//!   * the TUI can show a progress bar and a "current backups" panel for work
//!     happening in another process,
//!   * the scheduler refuses to start on top of a backup already running,
//!   * any backup can be stopped from the TUI by dropping `<pid>.cancel`,
//!     which the owning process polls.
//!
//! A job file is rewritten at least every `HEARTBEAT`; one whose heartbeat is
//! older than `STALE_SECS` belongs to a process that died and is ignored
//! (and removed on the next listing).

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::disks::{self, Disk};
use crate::logger;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// Task Scheduler job (`diskutility --scheduled-backup`).
    Scheduled,
    /// Started with `s` in a TUI.
    Interactive,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Scheduled => "scheduled",
            Kind::Interactive => "manual",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub pid: u32,
    pub kind: Kind,
    /// "disk 2 (Samsung T7, 1.0 TB)"
    pub disk: String,
    /// Final image path.
    pub image: String,
    /// 0.0 – 1.0
    pub frac: f64,
    /// Last progress line ("backup: 1.2 GB / 2.0 TB · 410 MB/s · eta 1h02").
    pub detail: String,
    /// Unix seconds.
    pub started: u64,
    /// Unix seconds of the last write.
    pub updated: u64,
}

impl Job {
    pub fn is_mine(&self) -> bool {
        self.pid == std::process::id()
    }

    pub fn elapsed_secs(&self) -> u64 {
        now_unix().saturating_sub(self.started)
    }

    pub fn cancelling(&self) -> bool {
        self.detail.starts_with("cancelling")
    }
}

pub const STALE_SECS: u64 = 15;
pub const HEARTBEAT: Duration = Duration::from_secs(2);
const WRITE_EVERY: Duration = Duration::from_millis(500);

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn jobs_dir() -> Option<PathBuf> {
    config::dir().map(|d| d.join("jobs"))
}

fn job_path(pid: u32) -> Option<PathBuf> {
    jobs_dir().map(|d| d.join(format!("{pid}.json")))
}

fn cancel_path(pid: u32) -> Option<PathBuf> {
    jobs_dir().map(|d| d.join(format!("{pid}.cancel")))
}

fn write_job(j: &Job) {
    let Some(p) = job_path(j.pid) else { return };
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let Ok(json) = serde_json::to_string(j) else { return };
    // write-then-rename so a reader never sees a half-written file
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

fn clear(pid: u32) {
    if let Some(p) = job_path(pid) {
        let _ = std::fs::remove_file(p);
    }
    if let Some(p) = cancel_path(pid) {
        let _ = std::fs::remove_file(p);
    }
}

/// Publishes one backup for the lifetime of this value; the job file is
/// removed when it is dropped (normal end, error, or panic unwind).
pub struct Publisher {
    job: Job,
    last_write: Instant,
}

impl Publisher {
    pub fn start(kind: Kind, disk: &Disk, image: &std::path::Path, frac: f64) -> Self {
        let pid = std::process::id();
        // never inherit a cancel request meant for a previous job with our pid
        if let Some(p) = cancel_path(pid) {
            let _ = std::fs::remove_file(p);
        }
        let job = Job {
            pid,
            kind,
            disk: format!("disk {} ({}, {})", disk.number, disk.name, disks::human(disk.size)),
            image: image.display().to_string(),
            frac,
            detail: if frac > 0.0 { "resuming".into() } else { "starting".into() },
            started: now_unix(),
            updated: now_unix(),
        };
        write_job(&job);
        Self { job, last_write: Instant::now() }
    }

    #[cfg(test)]
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// Record progress; written to disk at most every half second.
    pub fn update(&mut self, frac: f64, detail: &str) {
        self.job.frac = frac;
        if !self.job.cancelling() {
            self.job.detail = detail.to_string();
        }
        self.flush_if_due(WRITE_EVERY);
    }

    /// Call regularly from the event loop so the heartbeat stays fresh even
    /// when the worker is silent (e.g. a long flush over the network).
    pub fn heartbeat(&mut self) {
        self.flush_if_due(HEARTBEAT);
    }

    pub fn mark_cancelling(&mut self) {
        self.job.detail = "cancelling…".into();
        self.flush_if_due(Duration::ZERO);
    }

    fn flush_if_due(&mut self, every: Duration) {
        if self.last_write.elapsed() >= every {
            self.job.updated = now_unix();
            write_job(&self.job);
            self.last_write = Instant::now();
        }
    }

    /// Has someone asked this job to stop (via `request_cancel`)?
    pub fn cancel_requested(&self) -> bool {
        cancel_path(self.job.pid).is_some_and(|p| p.exists())
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        clear(self.job.pid);
    }
}

/// Every live job, oldest first. Stale files (dead processes) are deleted.
pub fn list() -> Vec<Job> {
    let Some(dir) = jobs_dir() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let now = now_unix();
    let mut jobs = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(j) = serde_json::from_str::<Job>(&text) else {
            let _ = std::fs::remove_file(&p);
            continue;
        };
        if now.saturating_sub(j.updated) > STALE_SECS {
            logger::log(format!("jobs: dropping stale record for pid {} ({})", j.pid, j.image));
            clear(j.pid);
            continue;
        }
        jobs.push(j);
    }
    jobs.sort_by_key(|j| (j.started, j.pid));
    jobs
}

/// Live jobs owned by other processes.
pub fn others() -> Vec<Job> {
    list().into_iter().filter(|j| !j.is_mine()).collect()
}

/// Ask the process running `pid`'s backup to stop. It notices within a
/// heartbeat and keeps its partial image for resuming.
pub fn request_cancel(pid: u32) -> Result<(), String> {
    let p = cancel_path(pid).ok_or("APPDATA is not set")?;
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(&p, b"cancel").map_err(|e| format!("cannot write {}: {e}", p.display()))?;
    logger::log(format!("jobs: cancel requested for pid {pid}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_list_cancel_and_stale() {
        let tmp = std::env::temp_dir().join(format!("du-jobs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("diskutility")).unwrap();
        std::env::set_var("APPDATA", &tmp);
        assert!(list().is_empty());

        let d = Disk { name: "T7".into(), serial: "S".into(), size: 1 << 40, ..Default::default() };
        let mut p = Publisher::start(Kind::Interactive, &d, std::path::Path::new(r"Z:\a.img"), 0.25);
        let l = list();
        assert_eq!(l.len(), 1);
        assert!(l[0].is_mine());
        assert_eq!(l[0].frac, 0.25);
        assert_eq!(l[0].detail, "resuming");
        assert!(others().is_empty());

        assert!(!p.cancel_requested());
        request_cancel(p.job().pid).unwrap();
        assert!(p.cancel_requested());
        p.mark_cancelling();
        assert!(list()[0].cancelling());

        // a record from a dead process is dropped and its files removed
        let dead = Job { pid: 1, updated: now_unix() - STALE_SECS - 1, ..p.job().clone() };
        write_job(&dead);
        assert_eq!(list().len(), 1);
        assert!(!job_path(1).unwrap().exists());

        drop(p);
        assert!(list().is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
