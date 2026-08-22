use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use crate::config::{self, Config, Frequency, Schedule, MONTHS, WEEKDAYS};
use crate::schedule;
use crate::disks::{self, Disk};
use crate::logger;
use crate::ops::{self, OpEvent, Preset, PRESETS};

pub enum AppEvent {
    Disks(Result<Vec<Disk>, String>),
    Op(OpEvent),
    Update(String),
    /// Progress line from an in-app update (Shift+U → y).
    UpdateStep(String),
    UpdateDone(Result<String, String>),
    Health(Result<ops::HealthReport, String>),
}

/// One editable row of the schedule editor (`a` key). Which rows are shown
/// depends on the frequency.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SchedField {
    Frequency,
    Every,
    Hour,
    Minute,
    Weekday,
    Day,
    Month,
    Keep,
}

impl SchedField {
    pub fn label(self) -> &'static str {
        match self {
            SchedField::Frequency => "Frequency",
            SchedField::Every => "Every",
            SchedField::Hour => "Hour",
            SchedField::Minute => "Minute",
            SchedField::Weekday => "Weekday",
            SchedField::Day => "Day of month",
            SchedField::Month => "Month",
            SchedField::Keep => "Keep images",
        }
    }
}

/// Rows shown for a schedule, in display order.
pub fn schedule_fields(s: &Schedule) -> Vec<SchedField> {
    use SchedField::*;
    let mut v = vec![Frequency];
    match s.frequency {
        crate::config::Frequency::Minutes => v.push(Every),
        crate::config::Frequency::Hourly => v.extend([Every, Hour, Minute]),
        crate::config::Frequency::Daily => v.extend([Hour, Minute]),
        crate::config::Frequency::Weekly => v.extend([Weekday, Hour, Minute]),
        crate::config::Frequency::Monthly => v.extend([Day, Hour, Minute]),
        crate::config::Frequency::Yearly => v.extend([Month, Day, Hour, Minute]),
    }
    v.push(Keep);
    v
}

/// Display value of one schedule row.
pub fn schedule_value(s: &Schedule, f: SchedField) -> String {
    match f {
        SchedField::Frequency => s.frequency.label().to_string(),
        SchedField::Every => match s.frequency {
            crate::config::Frequency::Minutes => format!("{} minute(s)", s.every),
            _ => format!("{} hour(s)", s.every),
        },
        SchedField::Hour => format!("{:02} h", s.hour),
        SchedField::Minute => format!("{:02} min", s.minute),
        SchedField::Weekday => WEEKDAYS[s.weekday as usize % 7].to_string(),
        SchedField::Day => s.day.to_string(),
        SchedField::Month => MONTHS[(s.month.clamp(1, 12) - 1) as usize].to_string(),
        SchedField::Keep => if s.keep == 0 { "all (never delete)".into() } else { format!("last {}", s.keep) },
    }
}

/// Cycle one row by `delta` (Left = -1, Right = +1), wrapping.
fn schedule_adjust(s: &mut Schedule, f: SchedField, delta: i32) {
    fn wrap(v: u32, lo: u32, hi: u32, delta: i32) -> u32 {
        let span = (hi - lo + 1) as i32;
        let cur = v.clamp(lo, hi) as i32 - lo as i32;
        (((cur + delta) % span + span) % span) as u32 + lo
    }
    match f {
        SchedField::Frequency => {
            let i = Frequency::ALL.iter().position(|x| *x == s.frequency).unwrap_or(2);
            s.frequency = Frequency::ALL[wrap(i as u32, 0, 5, delta) as usize];
            // keep `every` sensible when switching between minutes and hours
            s.every = match s.frequency {
                Frequency::Minutes => s.every.clamp(1, 1439),
                _ => s.every.clamp(1, 23),
            };
        }
        SchedField::Every => {
            s.every = match s.frequency {
                Frequency::Minutes => {
                    const STEPS: [u32; 9] = [1, 5, 10, 15, 20, 30, 45, 60, 120];
                    let i = STEPS.iter().position(|x| *x >= s.every).unwrap_or(STEPS.len() - 1);
                    STEPS[wrap(i as u32, 0, STEPS.len() as u32 - 1, delta) as usize]
                }
                _ => wrap(s.every, 1, 23, delta),
            };
        }
        SchedField::Hour => s.hour = wrap(s.hour, 0, 23, delta),
        SchedField::Minute => s.minute = wrap(s.minute / 5 * 5, 0, 59, delta * 5),
        SchedField::Weekday => s.weekday = wrap(s.weekday, 0, 6, delta),
        SchedField::Day => s.day = wrap(s.day, 1, 28, delta),
        SchedField::Month => s.month = wrap(s.month, 1, 12, delta),
        SchedField::Keep => s.keep = wrap(s.keep, 0, 30, delta),
    }
}

/// True when the startup update check was disabled by flag or env var.
pub fn update_check_opted_out() -> bool {
    std::env::args().any(|a| a == "--no-update-check")
        || std::env::var_os("DISKUTILITY_NO_UPDATE_CHECK").is_some_and(|v| !v.is_empty() && v != "0")
}

pub enum InputPurpose {
    Label(Preset),
    IsoPath,
    BackupPath,
    /// Folder to remember as the default backup destination (`n` key).
    BackupDir,
    CloneTarget,
}

pub enum PendingAction {
    Format { preset: Preset, label: String },
    EraseQuick,
    EraseSecure,
    WriteIso { path: PathBuf, size: u64 },
    BenchFull,
    Capacity { full: bool },
    CloneFrom { source: Disk },
}

impl PendingAction {
    pub fn title(&self, disk: u32) -> String {
        match self {
            PendingAction::Format { preset, .. } => {
                format!("Formatting disk {disk} — {}", preset.name())
            }
            PendingAction::EraseQuick => format!("Erasing disk {disk} (quick)"),
            PendingAction::EraseSecure => format!("Erasing disk {disk} (zero-fill)"),
            PendingAction::WriteIso { .. } => format!("Writing image to disk {disk}"),
            PendingAction::BenchFull => format!("Benchmarking disk {disk} (read + write)"),
            PendingAction::Capacity { full: false } => format!("Capacity test on disk {disk} (quick)"),
            PendingAction::Capacity { full: true } => format!("Capacity test on disk {disk} (full)"),
            PendingAction::CloneFrom { source } => {
                format!("Cloning disk {} → disk {disk}", source.number)
            }
        }
    }

    pub fn summary(&self) -> String {
        match self {
            PendingAction::Format { preset, label } => format!(
                "Format as {} — label '{label}'",
                preset.fs_display()
            ),
            PendingAction::EraseQuick => {
                "Quick erase — destroy the partition table and all filesystems".into()
            }
            PendingAction::EraseSecure => {
                "Secure erase — overwrite EVERY byte on the disk with zeros".into()
            }
            PendingAction::WriteIso { path, size } => format!(
                "Write image '{}' ({}) sector-by-sector, then verify",
                path.file_name().and_then(|f| f.to_str()).unwrap_or("?"),
                disks::human(*size)
            ),
            PendingAction::BenchFull => {
                "Full benchmark — wipes the disk, then measures seq/4K read & write".into()
            }
            PendingAction::Capacity { full: false } => {
                "Quick capacity test — wipes the disk, writes & verifies pattern samples".into()
            }
            PendingAction::Capacity { full: true } => {
                "FULL capacity test — wipes the disk, writes & verifies EVERY byte (hours)".into()
            }
            PendingAction::CloneFrom { source } => format!(
                "Overwrite THIS disk with a sector-for-sector clone of disk {} ({} · {}), then verify",
                source.number,
                source.name,
                disks::human(source.size)
            ),
        }
    }

    fn cancellable(&self) -> bool {
        !matches!(self, PendingAction::Format { .. } | PendingAction::EraseQuick)
    }
}

pub struct ProgressState {
    pub title: String,
    pub pct: Option<f64>,
    pub detail: String,
    pub samples: Vec<u64>,
    pub logs: Vec<String>,
    pub started: Instant,
    pub done: Option<Result<String, String>>,
    pub cancel: Arc<AtomicBool>,
    pub cancellable: bool,
}

pub enum Modal {
    None,
    Help,
    Unlock { buf: String },
    Presets { idx: usize },
    EraseMenu { idx: usize },
    TestMenu { idx: usize },
    /// Where should the backup image go? Entries come from `App::backup_choices`.
    BackupMenu { idx: usize },
    Health { title: String, report: Option<Result<ops::HealthReport, String>> },
    Input { purpose: InputPurpose, buf: String },
    Confirm { action: PendingAction, buf: String },
    Progress(ProgressState),
    /// Shift+U: offer to install an available update / toggle auto-update.
    /// `steps` fills while the download runs; `done` ends it.
    Update { steps: Vec<String>, done: Option<Result<String, String>> },
    /// `a`: edit the automatic backup schedule for the selected disk.
    Schedule { s: Schedule, field: usize, installed: bool },
}

pub struct App {
    pub disks: Vec<Disk>,
    pub selected: usize,
    pub scanning: bool,
    pub elevated: bool,
    pub unlocked: bool,
    pub update: Option<String>,
    pub modal: Modal,
    pub status: Option<(String, bool, Instant)>,
    pub tick: usize,
    pub config: Config,
    app_drive: Option<char>,
    /// Disk snapshotted by `guard()` for the action being configured/confirmed.
    pending_target: Option<Disk>,
    tx: mpsc::Sender<AppEvent>,
    rx: mpsc::Receiver<AppEvent>,
    quit: bool,
    /// Set after a successful in-app update: main relaunches the new exe.
    restart: bool,
}

impl App {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            config: config::load(),
            disks: Vec::new(),
            selected: 0,
            scanning: false,
            elevated: ops::is_elevated(),
            unlocked: false,
            update: None,
            modal: Modal::None,
            status: None,
            tick: 0,
            pending_target: None,
            app_drive: std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().and_then(|s| s.chars().next()))
                .map(|c| c.to_ascii_uppercase()),
            tx,
            rx,
            quit: false,
            restart: false,
        }
    }

    pub fn restart_requested(&self) -> bool {
        self.restart
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        self.refresh();
        // Startup update check is the only network access in the TUI; it can
        // be switched off with --no-update-check or DISKUTILITY_NO_UPDATE_CHECK=1.
        if update_check_opted_out() {
            logger::log("update check skipped (opted out)");
        } else {
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                if let Ok(Some((tag, _))) = crate::update::check_latest() {
                    let _ = tx.send(AppEvent::Update(tag));
                }
            });
        }
        while !self.quit {
            self.tick = self.tick.wrapping_add(1);
            if let Some((_, _, t)) = &self.status {
                if t.elapsed() > Duration::from_secs(5) {
                    self.status = None;
                }
            }
            terminal.draw(|f| crate::ui::draw(f, self))?;
            while let Ok(ev) = self.rx.try_recv() {
                self.on_app_event(ev);
            }
            if event::poll(Duration::from_millis(80))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        self.on_key(k);
                    }
                }
            }
        }
        Ok(())
    }

    fn on_app_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Disks(result) => {
                self.scanning = false;
                match result {
                    Ok(list) => {
                        logger::log(format!("scan complete: {} disk(s) found", list.len()));
                        self.disks = list;
                        if self.selected >= self.disks.len() {
                            self.selected = self.disks.len().saturating_sub(1);
                        }
                    }
                    Err(e) => self.error(format!("disk scan failed: {e}")),
                }
            }
            AppEvent::Op(op) => {
                if let Modal::Progress(p) = &mut self.modal {
                    match op {
                        OpEvent::Log(s) => {
                            logger::log(format!("op: {s}"));
                            p.logs.push(s);
                        }
                        OpEvent::Progress(frac, detail) => {
                            p.pct = Some(frac);
                            p.detail = detail;
                        }
                        OpEvent::Sample(v) => {
                            p.samples.push(v);
                            if p.samples.len() > 240 {
                                p.samples.remove(0);
                            }
                        }
                        OpEvent::Done(r) => {
                            match &r {
                                Ok(m) => logger::log(format!("op SUCCEEDED: {m}")),
                                Err(m) => logger::log(format!("op FAILED: {m}")),
                            }
                            p.done = Some(r);
                        }
                    }
                }
            }
            AppEvent::Update(tag) => {
                logger::log(format!("update available: {tag}"));
                self.update = Some(tag);
            }
            AppEvent::UpdateStep(line) => {
                if let Modal::Update { steps, .. } = &mut self.modal {
                    steps.push(line);
                }
            }
            AppEvent::UpdateDone(r) => {
                if let Ok(m) = &r {
                    if crate::update::updated(m) {
                        self.restart = true;
                    }
                }
                if let Modal::Update { done, .. } = &mut self.modal {
                    *done = Some(r);
                }
            }
            AppEvent::Health(r) => {
                match &r {
                    Ok(h) => logger::log(format!("health: {h:?}")),
                    Err(e) => logger::log(format!("health query failed: {e}")),
                }
                if let Modal::Health { report, .. } = &mut self.modal {
                    *report = Some(r);
                }
            }
        }
    }

    fn refresh(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(AppEvent::Disks(disks::enumerate()));
        });
    }

    fn info(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        logger::log(format!("status: {msg}"));
        self.status = Some((msg, false, Instant::now()));
    }

    fn error(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        logger::log(format!("ERROR: {msg}"));
        self.status = Some((msg, true, Instant::now()));
    }

    fn copy_log(&mut self) {
        match logger::copy_to_clipboard() {
            Ok(n) => self.info(format!(
                "session log copied to clipboard ({n} lines) — paste it anywhere"
            )),
            Err(e) => self.error(format!("could not copy log: {e}")),
        }
    }

    pub fn selected_disk(&self) -> Option<&Disk> {
        self.disks.get(self.selected)
    }

    /// Why a disk is protected from destructive operations (empty = not protected).
    pub fn protection_reasons(&self, disk: &Disk) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if disk.system || disk.boot {
            reasons.push("Windows system/boot disk");
        }
        if let Some(drive) = self.app_drive {
            if disk
                .partitions
                .iter()
                .any(|p| p.letter.chars().next().map(|c| c.to_ascii_uppercase()) == Some(drive))
            {
                reasons.push("hosts this running app");
            }
        }
        reasons
    }

    /// The disk a pending destructive action will run on: the snapshot taken
    /// when the user entered the menu, not whatever the list currently shows.
    pub fn target_disk(&self) -> Option<&Disk> {
        self.pending_target.as_ref()
    }

    /// Internal (non-removable) disks are the classic wrong-pick: a data SSD
    /// sitting next to the USB stick you meant. They aren't blocked, but they
    /// get the long confirmation phrase and a warning in the dialog.
    pub fn internal_bus_warning(disk: &Disk) -> Option<String> {
        let removable = matches!(
            disk.bus.to_ascii_uppercase().as_str(),
            "USB" | "SD" | "MMC" | "1394" | "VIRTUAL" | "FILE BACKED VIRTUAL"
        );
        if removable {
            None
        } else {
            Some(format!(
                "not a removable device (bus {})",
                if disk.bus.is_empty() { "unknown" } else { &disk.bus }
            ))
        }
    }

    /// The phrase the user must type to confirm an action on the target disk.
    /// Protected disks (with the override active) and internal disks demand
    /// the scarier phrase.
    pub fn confirm_phrase(&self) -> String {
        match self.target_disk() {
            Some(d)
                if !self.protection_reasons(d).is_empty()
                    || Self::internal_bus_warning(d).is_some() =>
            {
                format!("DESTROY {}", d.number)
            }
            Some(d) => d.number.to_string(),
            None => String::new(),
        }
    }

    /// Returns true if a destructive action is allowed on the selected disk,
    /// and snapshots that disk as the pending target.
    fn guard(&mut self) -> bool {
        let Some(disk) = self.selected_disk() else {
            self.error("no disk selected");
            return false;
        };
        let reasons = self.protection_reasons(disk);
        let number = disk.number;
        if !reasons.is_empty() && !self.unlocked {
            self.error(format!(
                "disk {number} is protected ({}) — press u to enable the safety override",
                reasons.join(", ")
            ));
            return false;
        }
        if !self.elevated {
            self.error("administrator rights required — restart this app from an elevated terminal");
            return false;
        }
        self.pending_target = self.selected_disk().cloned();
        true
    }

    /// Like `guard()` but for an explicitly chosen disk (the clone target).
    fn guard_target(&mut self, target: &Disk) -> bool {
        let reasons = self.protection_reasons(target);
        if !reasons.is_empty() && !self.unlocked {
            self.error(format!(
                "disk {} is protected ({}) — press u to enable the safety override",
                target.number,
                reasons.join(", ")
            ));
            return false;
        }
        if !self.elevated {
            self.error("administrator rights required — restart this app from an elevated terminal");
            return false;
        }
        self.pending_target = Some(target.clone());
        true
    }

    /// True when two scan results describe the same physical device.
    fn same_device(a: &Disk, b: &Disk) -> bool {
        a.number == b.number && a.serial == b.serial && a.size == b.size && a.name == b.name
    }

    /// The live entry for a previously snapshotted disk, if it is still the
    /// same device at the same number.
    fn live_match(&self, snapshot: &Disk) -> Option<&Disk> {
        self.disks
            .iter()
            .find(|d| d.number == snapshot.number)
            .filter(|d| Self::same_device(d, snapshot))
    }

    /// Drive letter (uppercase) of a drive-letter path, after canonicalizing.
    fn drive_of(path: &std::path::Path) -> Option<char> {
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let s = resolved.to_str()?.trim_start_matches(r"\\?\").to_string();
        let mut it = s.chars();
        match (it.next(), it.next()) {
            (Some(c), Some(':')) if c.is_ascii_alphabetic() => Some(c.to_ascii_uppercase()),
            _ => None,
        }
    }

    /// Destination choices for the backup menu: (label, folder) — `None`
    /// means "type a custom path".
    pub fn backup_choices(&self) -> Vec<(String, Option<String>)> {
        let mut v = Vec::new();
        if let Some(dir) = &self.config.backup_dir {
            v.push((format!("Saved destination   {dir}"), Some(dir.clone())));
        }
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into());
        v.push((format!("Home folder         {home}"), Some(home)));
        v.push(("Custom path…".to_string(), None));
        v
    }

    /// Suggested image filename for the selected disk: `disk2-WD_BLACK-20260822.img`.
    fn backup_filename(disk: &Disk) -> String {
        let safe_name: String = disk
            .name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(24)
            .collect();
        let date = chrono::Local::now().format("%Y%m%d");
        format!("disk{}-{safe_name}-{date}.img", disk.number)
    }

    fn open_backup_input(&mut self, dir: Option<String>) {
        let Some(disk) = self.selected_disk() else {
            self.error("no disk selected");
            return;
        };
        let name = Self::backup_filename(disk);
        let buf = match dir {
            Some(d) => format!("{}\\{name}", d.trim_end_matches(['\\', '/'])),
            None => String::new(),
        };
        self.modal = Modal::Input { purpose: InputPurpose::BackupPath, buf };
    }

    /// UNC target of a mapped network drive letter (`Z:` → `\\nas\backups`),
    /// read from HKCU\Network where persistent mappings are recorded. Mapped
    /// drives are usually invisible to an elevated process (UAC gives it a
    /// separate token), so the UNC form is what we store and open.
    fn mapped_drive_target(letter: char) -> Option<String> {
        let script = format!(
            "(Get-ItemProperty -LiteralPath 'HKCU:\\Network\\{}' -ErrorAction SilentlyContinue).RemotePath",
            letter.to_ascii_uppercase()
        );
        let out = ops::run_ps_quiet(&script).ok()?;
        let t = out.trim();
        (t.starts_with("\\\\") && t.len() > 2).then(|| t.to_string())
    }

    /// Validate a folder for the saved backup destination. Returns the path
    /// to store (UNC-resolved for mapped drives) or `None` to clear it.
    fn validate_backup_dir(&self, raw: &str) -> Result<Option<String>, String> {
        let trimmed = raw.trim().trim_matches('"').trim().trim_end_matches(['\\', '/']).to_string();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let mut dir = if trimmed.len() == 2 && trimmed.ends_with(':') { format!("{trimmed}\\") } else { trimmed };
        let is_unc = dir.starts_with("\\\\");
        let letter = dir
            .chars()
            .next()
            .filter(|c| c.is_ascii_alphabetic() && dir.get(1..2) == Some(":"));
        if !is_unc && letter.is_none() {
            return Err(r"give an absolute folder: Z:\backups or \\server\share\backups".into());
        }
        if let Some(l) = letter {
            if let Some(unc) = Self::mapped_drive_target(l) {
                let rest = dir[2..].trim_start_matches('\\').to_string();
                dir = if rest.is_empty() { unc } else { format!("{unc}\\{rest}") };
                logger::log(format!("backup dir: {l}: is a mapped drive → using {dir}"));
            }
        }
        let p = std::path::Path::new(&dir);
        if !p.is_dir() {
            let hint = if letter.is_some() && self.elevated {
                r" (mapped network drives are often invisible to an elevated process — use the \\server\share form)"
            } else {
                ""
            };
            return Err(format!("folder not reachable: {dir}{hint}"));
        }
        // prove it is writable before trusting it with a multi-hour backup
        let probe = p.join(format!(".diskutility-probe-{}", std::process::id()));
        std::fs::write(&probe, b"").map_err(|e| format!("cannot write to {dir}: {e}"))?;
        let _ = std::fs::remove_file(&probe);
        Ok(Some(dir))
    }

    fn validate_backup(&self, raw: &str) -> Result<PathBuf, String> {
        let trimmed = raw.trim().trim_matches('"').trim();
        if trimmed.is_empty() {
            return Err("enter a destination path for the image file".into());
        }
        let mut path = PathBuf::from(trimmed);
        if path.extension().is_none() {
            path.set_extension("img");
        }
        if path.exists() {
            return Err("that file already exists — choose a new name (nothing is overwritten)".into());
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or("give a full path, e.g. D:\\backups\\disk2.img")?;
        if !parent.is_dir() {
            return Err(format!("folder does not exist: {}", parent.display()));
        }
        let disk = self.selected_disk().ok_or("no disk selected")?;
        // the image must not land on the disk being imaged: the copy would
        // chase its own tail and never be consistent
        if let Some(drive) = Self::drive_of(parent) {
            if disk.partitions.iter().any(|p| p.letter.chars().next().map(|c| c.to_ascii_uppercase()) == Some(drive)) {
                return Err(format!(
                    "destination is on disk {} ({drive}:) — the disk being backed up. Save the image somewhere else.",
                    disk.number
                ));
            }
            // FAT32 caps files at 4 GiB; the write would die at exactly that point
            if let Some(fs) = ops::volume_filesystem(drive) {
                if fs.eq_ignore_ascii_case("FAT32") && disk.size >= 4 * 1024 * 1024 * 1024 {
                    return Err(format!(
                        "{drive}: is FAT32, which cannot hold files over 4 GiB — a {} image won't fit. Use an NTFS or exFAT destination.",
                        disks::human(disk.size)
                    ));
                }
            }
        }
        if let Some(free) = ops::free_space(&path) {
            if free < disk.size {
                return Err(format!(
                    "not enough free space: {} available, {} needed for a full image of disk {}",
                    disks::human(free),
                    disks::human(disk.size),
                    disk.number
                ));
            }
        }
        Ok(path)
    }

    fn validate_clone_target(&self, raw: &str) -> Result<(Disk, Disk), String> {
        let source = self.selected_disk().cloned().ok_or("no disk selected")?;
        let n: u32 = raw
            .trim()
            .parse()
            .map_err(|_| "type the number of the TARGET disk (the one that will be overwritten)")?;
        let target = self
            .disks
            .iter()
            .find(|d| d.number == n)
            .cloned()
            .ok_or(format!("there is no disk {n} — check the list"))?;
        if Self::same_device(&target, &source)
            || (target.size == source.size && target.serial == source.serial && !source.serial.is_empty())
        {
            return Err("source and target are the same device — pick a different target".into());
        }
        if target.size < source.size {
            return Err(format!(
                "target disk {n} ({}) is smaller than the source ({}) — shrinking clones are not supported",
                disks::human(target.size),
                disks::human(source.size)
            ));
        }
        Ok((source, target))
    }

    fn start_backup(&mut self, path: PathBuf) {
        let Some(disk) = self.selected_disk().cloned() else {
            self.error("no disk selected");
            return;
        };
        logger::log(format!(
            "action start: backup — source disk {} · {} · serial {} · {} → {}",
            disk.number,
            disk.name,
            disk.serial,
            disks::human(disk.size),
            path.display()
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = ProgressState {
            title: format!("Backing up disk {} to image", disk.number),
            pct: None,
            detail: String::new(),
            samples: Vec::new(),
            logs: vec![format!(
                "Source: disk {} · {} · {}",
                disk.number,
                disk.name,
                disks::human(disk.size)
            )],
            started: Instant::now(),
            done: None,
            cancel: cancel.clone(),
            cancellable: true,
        };
        ops::spawn_backup(self.tx.clone(), disk, path, cancel);
        self.modal = Modal::Progress(progress);
    }

    fn on_key(&mut self, k: KeyEvent) {
        // Ctrl+C: cancel a running op, otherwise quit.
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            if let Modal::Progress(p) = &self.modal {
                if p.done.is_none() {
                    if p.cancellable {
                        p.cancel.store(true, Ordering::Relaxed);
                    }
                    return;
                }
            }
            self.quit = true;
            return;
        }

        let modal = std::mem::replace(&mut self.modal, Modal::None);
        match modal {
            Modal::None => self.key_normal(k),
            Modal::Help => { /* any key closes help */ }
            Modal::Unlock { mut buf } => match k.code {
                KeyCode::Esc => {}
                KeyCode::Backspace => {
                    buf.pop();
                    self.modal = Modal::Unlock { buf };
                }
                KeyCode::Enter => {
                    if buf.trim() == "UNLOCK" {
                        self.unlocked = true;
                        logger::log("SAFETY OVERRIDE ENABLED by user (this session)");
                        self.info("safety override ACTIVE — protected disks can now be modified");
                    } else {
                        self.error("type UNLOCK (in capitals) to disable the protections");
                        self.modal = Modal::Unlock { buf };
                    }
                }
                KeyCode::Char(c) if !c.is_control() => {
                    buf.push(c.to_ascii_uppercase());
                    self.modal = Modal::Unlock { buf };
                }
                _ => self.modal = Modal::Unlock { buf },
            },
            Modal::Presets { mut idx } => match k.code {
                KeyCode::Esc => {}
                KeyCode::Up | KeyCode::Char('k') => {
                    idx = (idx + PRESETS.len() - 1) % PRESETS.len();
                    self.modal = Modal::Presets { idx };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    idx = (idx + 1) % PRESETS.len();
                    self.modal = Modal::Presets { idx };
                }
                KeyCode::Enter => {
                    self.modal = Modal::Input {
                        purpose: InputPurpose::Label(PRESETS[idx]),
                        buf: "UNTITLED".into(),
                    };
                }
                _ => self.modal = Modal::Presets { idx },
            },
            Modal::EraseMenu { mut idx } => match k.code {
                KeyCode::Esc => {}
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') => {
                    idx = 1 - idx;
                    self.modal = Modal::EraseMenu { idx };
                }
                KeyCode::Enter => {
                    let action = if idx == 0 {
                        PendingAction::EraseQuick
                    } else {
                        PendingAction::EraseSecure
                    };
                    self.modal = Modal::Confirm { action, buf: String::new() };
                }
                _ => self.modal = Modal::EraseMenu { idx },
            },
            Modal::TestMenu { mut idx } => match k.code {
                KeyCode::Esc => {}
                KeyCode::Up | KeyCode::Char('k') => {
                    idx = (idx + 4) % 5;
                    self.modal = Modal::TestMenu { idx };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    idx = (idx + 1) % 5;
                    self.modal = Modal::TestMenu { idx };
                }
                KeyCode::Enter => match idx {
                    0 => self.start_safe_test(false),
                    1 => self.start_safe_test(true),
                    _ => {
                        if self.guard() {
                            let action = match idx {
                                2 => PendingAction::BenchFull,
                                3 => PendingAction::Capacity { full: false },
                                _ => PendingAction::Capacity { full: true },
                            };
                            self.modal = Modal::Confirm { action, buf: String::new() };
                        }
                    }
                },
                _ => self.modal = Modal::TestMenu { idx },
            },
            Modal::BackupMenu { mut idx } => {
                let choices = self.backup_choices();
                let n = choices.len().max(1);
                match k.code {
                    KeyCode::Esc => {}
                    KeyCode::Up | KeyCode::Char('k') => {
                        idx = (idx + n - 1) % n;
                        self.modal = Modal::BackupMenu { idx };
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        idx = (idx + 1) % n;
                        self.modal = Modal::BackupMenu { idx };
                    }
                    KeyCode::Enter => {
                        let dir = choices.get(idx).and_then(|(_, d)| d.clone());
                        self.open_backup_input(dir);
                    }
                    KeyCode::Char('n') => {
                        let buf = self.config.backup_dir.clone().unwrap_or_default();
                        self.modal = Modal::Input { purpose: InputPurpose::BackupDir, buf };
                    }
                    _ => self.modal = Modal::BackupMenu { idx },
                }
            }
            Modal::Health { title, report } => match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('h') => {}
                KeyCode::Char('c') => {
                    self.copy_log();
                    self.modal = Modal::Health { title, report };
                }
                _ => self.modal = Modal::Health { title, report },
            },
            Modal::Input { purpose, mut buf } => match k.code {
                KeyCode::Esc => {}
                KeyCode::Backspace => {
                    buf.pop();
                    self.modal = Modal::Input { purpose, buf };
                }
                KeyCode::Enter => match purpose {
                    InputPurpose::Label(preset) => {
                        let label = sanitize_label(&buf, preset);
                        self.modal = Modal::Confirm {
                            action: PendingAction::Format { preset, label },
                            buf: String::new(),
                        };
                    }
                    InputPurpose::IsoPath => match self.validate_image(&buf) {
                        Ok((path, size)) => {
                            self.modal = Modal::Confirm {
                                action: PendingAction::WriteIso { path, size },
                                buf: String::new(),
                            };
                        }
                        Err(e) => {
                            self.error(e);
                            self.modal = Modal::Input { purpose, buf };
                        }
                    },
                    InputPurpose::BackupPath => match self.validate_backup(&buf) {
                        Ok(path) => self.start_backup(path),
                        Err(e) => {
                            self.error(e);
                            self.modal = Modal::Input { purpose, buf };
                        }
                    },
                    InputPurpose::BackupDir => match self.validate_backup_dir(&buf) {
                        Ok(dir) => {
                            let msg = match &dir {
                                Some(d) => format!("backup destination saved: {d}"),
                                None => "backup destination cleared".to_string(),
                            };
                            self.config.backup_dir = dir;
                            match config::save(&self.config) {
                                Ok(()) => self.info(msg),
                                Err(e) => self.error(format!("settings not saved: {e}")),
                            }
                        }
                        Err(e) => {
                            self.error(e);
                            self.modal = Modal::Input { purpose, buf };
                        }
                    },
                    InputPurpose::CloneTarget => match self.validate_clone_target(&buf) {
                        Ok((source, target)) => {
                            if self.guard_target(&target) {
                                self.modal = Modal::Confirm {
                                    action: PendingAction::CloneFrom { source },
                                    buf: String::new(),
                                };
                            }
                        }
                        Err(e) => {
                            self.error(e);
                            self.modal = Modal::Input { purpose, buf };
                        }
                    },
                },
                KeyCode::Char(c) if !c.is_control() => {
                    buf.push(c);
                    self.modal = Modal::Input { purpose, buf };
                }
                _ => self.modal = Modal::Input { purpose, buf },
            },
            Modal::Confirm { action, mut buf } => match k.code {
                KeyCode::Esc => self.info("cancelled — nothing was touched"),
                KeyCode::Backspace => {
                    buf.pop();
                    self.modal = Modal::Confirm { action, buf };
                }
                KeyCode::Enter => {
                    let expect = self.confirm_phrase();
                    if !expect.is_empty() && buf.trim() == expect {
                        self.start_action(action);
                    } else {
                        self.error(format!("type {expect} to confirm"));
                        self.modal = Modal::Confirm { action, buf };
                    }
                }
                KeyCode::Char(c) if c.is_ascii_alphanumeric() || c == ' ' => {
                    buf.push(c.to_ascii_uppercase());
                    self.modal = Modal::Confirm { action, buf };
                }
                _ => self.modal = Modal::Confirm { action, buf },
            },
            Modal::Update { steps, done } => self.key_update(k, steps, done),
            Modal::Schedule { s, field, installed } => self.key_schedule(k, s, field, installed),
            Modal::Progress(p) => {
                if k.code == KeyCode::Char('c') {
                    self.copy_log();
                    self.modal = Modal::Progress(p);
                } else if p.done.is_some() {
                    match k.code {
                        KeyCode::Enter | KeyCode::Esc => self.refresh(),
                        _ => self.modal = Modal::Progress(p),
                    }
                } else {
                    if k.code == KeyCode::Char('x') && p.cancellable {
                        logger::log("cancel requested by user");
                        p.cancel.store(true, Ordering::Relaxed);
                    }
                    self.modal = Modal::Progress(p);
                }
            }
        }
    }

    fn key_normal(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.disks.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = self.disks.len().saturating_sub(1)
            }
            KeyCode::Char('r') => {
                self.refresh();
                self.info("rescanning disks…");
            }
            KeyCode::Char('c') => self.copy_log(),
            KeyCode::Char('u') => {
                if self.unlocked {
                    self.unlocked = false;
                    logger::log("safety override disabled");
                    self.info("safety override disabled — protected disks are blocked again");
                } else {
                    self.modal = Modal::Unlock { buf: String::new() };
                }
            }
            KeyCode::Char('?') => self.modal = Modal::Help,
            KeyCode::Char('U') => self.modal = Modal::Update { steps: Vec::new(), done: None },
            KeyCode::Char('a') => self.open_schedule(),
            KeyCode::Char('f') => {
                if self.guard() {
                    self.modal = Modal::Presets { idx: 0 };
                }
            }
            KeyCode::Char('e') => {
                if self.guard() {
                    self.modal = Modal::EraseMenu { idx: 0 };
                }
            }
            KeyCode::Char('i')
                if self.guard() => {
                    self.modal = Modal::Input {
                        purpose: InputPurpose::IsoPath,
                        buf: String::new(),
                    };
                }
            KeyCode::Char('b') => {
                if self.selected_disk().is_some() {
                    self.modal = Modal::TestMenu { idx: 0 };
                } else {
                    self.error("no disk selected");
                }
            }
            KeyCode::Char('h') => {
                if let Some(disk) = self.selected_disk() {
                    let title = format!("Health — disk {} · {}", disk.number, disk.name);
                    ops::spawn_health(self.tx.clone(), disk.number);
                    self.modal = Modal::Health { title, report: None };
                } else {
                    self.error("no disk selected");
                }
            }
            KeyCode::Char('s') => {
                if !self.elevated {
                    self.error("administrator rights required — restart this app from an elevated terminal");
                } else if self.selected_disk().is_some() {
                    self.modal = Modal::BackupMenu { idx: 0 };
                } else {
                    self.error("no disk selected");
                }
            }
            KeyCode::Char('n') => {
                let buf = self.config.backup_dir.clone().unwrap_or_default();
                self.modal = Modal::Input { purpose: InputPurpose::BackupDir, buf };
            }
            KeyCode::Char('d') => {
                if !self.elevated {
                    self.error("administrator rights required — restart this app from an elevated terminal");
                } else if self.selected_disk().is_some() {
                    self.modal = Modal::Input { purpose: InputPurpose::CloneTarget, buf: String::new() };
                } else {
                    self.error("no disk selected");
                }
            }
            _ => {}
        }
    }

    // ----- Shift+U: update dialog -------------------------------------------

    fn key_update(&mut self, k: KeyEvent, steps: Vec<String>, done: Option<Result<String, String>>) {
        let running = !steps.is_empty() && done.is_none();
        match (k.code, &done) {
            // finished: any of these closes; after a successful install the
            // app exits and main relaunches the new version in this terminal
            (KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('r'), Some(_)) => {
                if self.restart {
                    self.quit = true;
                }
            }
            (_, Some(_)) => self.modal = Modal::Update { steps, done },
            _ if running => self.modal = Modal::Update { steps, done },
            (KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q'), None) => {}
            (KeyCode::Char('y'), None) if self.update.is_some() => {
                let tx = self.tx.clone();
                logger::log("update: started from the TUI (Shift+U)");
                std::thread::spawn(move || {
                    let step_tx = tx.clone();
                    let r = crate::update::self_update_with(&|line| {
                        let _ = step_tx.send(AppEvent::UpdateStep(line.to_string()));
                    });
                    let _ = tx.send(AppEvent::UpdateDone(r));
                });
                self.modal = Modal::Update { steps: vec!["Starting update…".into()], done: None };
            }
            (KeyCode::Char('a'), None) => {
                self.config.auto_update = !self.config.auto_update;
                let on = self.config.auto_update;
                match config::save(&self.config) {
                    Ok(()) => self.info(if on {
                        "automatic updates ON — new releases install when the app starts"
                    } else {
                        "automatic updates OFF"
                    }),
                    Err(e) => self.error(format!("settings not saved: {e}")),
                }
                self.modal = Modal::Update { steps, done };
            }
            _ => self.modal = Modal::Update { steps, done },
        }
    }

    // ----- a: automatic backup schedule ---------------------------------------

    fn open_schedule(&mut self) {
        if !self.elevated {
            self.error("administrator rights required to manage scheduled backups — restart from an elevated terminal");
            return;
        }
        let Some(disk) = self.selected_disk().cloned() else {
            self.error("no disk selected");
            return;
        };
        let Some(dir) = self.config.backup_dir.clone() else {
            self.error("set a backup destination first (n) — scheduled images go there");
            return;
        };
        let mut s = self.config.schedule.clone().unwrap_or_default();
        s.disk_serial = disk.serial.clone();
        s.disk_name = disk.name.clone();
        s.disk_size = disk.size;
        s.dest_dir = dir;
        let installed = self.config.schedule.is_some() && schedule::is_installed();
        self.modal = Modal::Schedule { s, field: 0, installed };
    }

    fn key_schedule(&mut self, k: KeyEvent, mut s: Schedule, mut field: usize, installed: bool) {
        let fields = schedule_fields(&s);
        field = field.min(fields.len() - 1);
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {}
            KeyCode::Up | KeyCode::Char('k') => {
                field = (field + fields.len() - 1) % fields.len();
                self.modal = Modal::Schedule { s, field, installed };
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                field = (field + 1) % fields.len();
                self.modal = Modal::Schedule { s, field, installed };
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('-') => {
                schedule_adjust(&mut s, fields[field], -1);
                self.modal = Modal::Schedule { s, field, installed };
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('+') | KeyCode::Char(' ') => {
                schedule_adjust(&mut s, fields[field], 1);
                self.modal = Modal::Schedule { s, field, installed };
            }
            KeyCode::Enter => {
                let desc = s.describe();
                match schedule::install(&s) {
                    Ok(()) => {
                        self.config.schedule = Some(s);
                        match config::save(&self.config) {
                            Ok(()) => {
                                logger::log(format!("scheduled backup registered: {desc}"));
                                self.info(format!(
                                    "scheduled backup saved — runs {desc} as task '{}'",
                                    schedule::TASK_NAME
                                ));
                            }
                            Err(e) => self.error(format!("task registered but settings not saved: {e}")),
                        }
                    }
                    Err(e) => {
                        self.error(format!("could not register the scheduled task: {e}"));
                        self.modal = Modal::Schedule { s, field, installed };
                    }
                }
            }
            KeyCode::Char('n') => {
                let buf = self.config.backup_dir.clone().unwrap_or_default();
                self.modal = Modal::Input { purpose: InputPurpose::BackupDir, buf };
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                let r = if installed { schedule::remove() } else { Ok(()) };
                match r {
                    Ok(()) => {
                        self.config.schedule = None;
                        match config::save(&self.config) {
                            Ok(()) => self.info("scheduled backup removed"),
                            Err(e) => self.error(format!("task removed but settings not saved: {e}")),
                        }
                    }
                    Err(e) => {
                        self.error(format!("could not remove the scheduled task: {e}"));
                        self.modal = Modal::Schedule { s, field, installed };
                    }
                }
            }
            _ => self.modal = Modal::Schedule { s, field, installed },
        }
    }

    fn validate_image(&self, raw: &str) -> Result<(PathBuf, u64), String> {
        let trimmed = raw.trim().trim_matches('"').trim();
        if trimmed.is_empty() {
            return Err("enter a path to an .iso or .img file".into());
        }
        let path = PathBuf::from(trimmed);
        let meta = std::fs::metadata(&path).map_err(|e| format!("cannot read file: {e}"))?;
        if !meta.is_file() {
            return Err("that path is not a file".into());
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(ext.as_str(), "iso" | "img" | "raw" | "bin" | "wic") {
            return Err(format!("unsupported extension '.{ext}' — expected .iso, .img, .raw, .bin or .wic"));
        }
        let disk = self.target_disk().ok_or("no disk selected")?;
        if meta.len() > disk.size {
            return Err(format!(
                "image ({}) is larger than disk {} ({})",
                disks::human(meta.len()),
                disk.number,
                disks::human(disk.size)
            ));
        }
        // The image must not live on the disk we are about to wipe: the prep
        // step would destroy its volume and the write would fail half-way,
        // taking the image with it.
        let resolved = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let image_drive = resolved
            .to_str()
            .map(|s| s.trim_start_matches(r"\\?\").to_string())
            .and_then(|s| {
                let mut it = s.chars();
                match (it.next(), it.next()) {
                    (Some(c), Some(':')) if c.is_ascii_alphabetic() => Some(c.to_ascii_uppercase()),
                    _ => None,
                }
            });
        match image_drive {
            Some(drive) => {
                let on_target = disk.partitions.iter().any(|p| {
                    p.letter.chars().next().map(|c| c.to_ascii_uppercase()) == Some(drive)
                });
                if on_target {
                    return Err(format!(
                        "the image is stored on disk {} ({drive}:) — it would be destroyed while being written. Move it to another drive first.",
                        disk.number
                    ));
                }
            }
            // UNC paths (\\server\share, canonicalized to \\?\UNC\...) are by
            // definition not on a local disk; anything else we can't place
            // (volume-GUID paths) is refused rather than guessed.
            None if resolved.to_string_lossy().contains(r"\UNC\")
                || resolved.to_string_lossy().starts_with(r"\\") && !resolved.to_string_lossy().starts_with(r"\\?\") => {}
            None => {
                return Err("image path must be a drive-letter path (e.g. D:\\images\\x.iso) or a UNC share path — volume-GUID paths are not supported".into());
            }
        }
        Ok((path, meta.len()))
    }

    fn start_action(&mut self, action: PendingAction) {
        let Some(disk) = self.target_disk().cloned() else {
            self.error("no target disk — select a disk and start again");
            return;
        };
        // The disk list may have been refreshed while the dialog was open (a
        // scan finishing, a drive unplugged): make sure the confirmed target is
        // still the same device at the same number before running anything.
        if self.live_match(&disk).is_none() {
            self.pending_target = None;
            self.error(format!(
                "the disk list changed since you selected disk {} — nothing was touched. Re-select the disk and retry.",
                disk.number
            ));
            return;
        }
        // belt-and-braces: protected disks always require the explicit override
        if !self.protection_reasons(&disk).is_empty() && !self.unlocked {
            self.error("refusing to touch a protected disk (press u to override)");
            return;
        }
        if !self.elevated {
            self.error("administrator rights required — restart this app from an elevated terminal");
            return;
        }
        let allow_protected = self.unlocked;
        if allow_protected && !self.protection_reasons(&disk).is_empty() {
            logger::log(format!(
                "WARNING: user is operating on PROTECTED disk {} with the safety override active",
                disk.number
            ));
        }
        logger::log(format!(
            "action start: {} — target disk {} · {} · {} · bus {}",
            action.summary(),
            disk.number,
            disk.name,
            disks::human(disk.size),
            disk.bus
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = ProgressState {
            title: action.title(disk.number),
            pct: None,
            detail: String::new(),
            samples: Vec::new(),
            logs: vec![format!(
                "Target: disk {} · {} · {}",
                disk.number,
                disk.name,
                disks::human(disk.size)
            )],
            started: Instant::now(),
            done: None,
            cancel: cancel.clone(),
            cancellable: action.cancellable(),
        };
        let tx = self.tx.clone();
        match action {
            PendingAction::Format { preset, label } => {
                ops::spawn_format(tx, disk, preset, label, allow_protected)
            }
            PendingAction::EraseQuick => ops::spawn_erase_quick(tx, disk, allow_protected),
            PendingAction::EraseSecure => ops::spawn_zero_fill(tx, disk, cancel, allow_protected),
            PendingAction::WriteIso { path, size } => {
                ops::spawn_write_iso(tx, disk, path, size, cancel, allow_protected)
            }
            PendingAction::BenchFull => {
                crate::bench::spawn_full_bench(tx, disk, cancel, allow_protected)
            }
            PendingAction::Capacity { full } => {
                crate::bench::spawn_capacity_test(tx, disk, full, cancel, allow_protected)
            }
            PendingAction::CloneFrom { source } => {
                if self.live_match(&source).is_none() {
                    self.pending_target = None;
                    self.error(format!(
                        "the source disk {} changed since you selected it — nothing was touched. Re-select and retry.",
                        source.number
                    ));
                    return;
                }
                logger::log(format!(
                    "clone source: disk {} · {} · serial {} · {}",
                    source.number,
                    source.name,
                    source.serial,
                    disks::human(source.size)
                ));
                ops::spawn_clone(tx, source, disk, cancel, allow_protected)
            }
        }
        self.modal = Modal::Progress(progress);
    }

    /// Read benchmark and surface scan are non-destructive, so they skip the
    /// confirm dialog and are allowed even on protected disks — they only
    /// need elevation.
    fn start_safe_test(&mut self, surface_scan: bool) {
        if !self.elevated {
            self.error("administrator rights required — restart this app from an elevated terminal");
            return;
        }
        let Some(disk) = self.selected_disk().cloned() else {
            self.error("no disk selected");
            return;
        };
        let what = if surface_scan { "surface scan" } else { "read benchmark" };
        logger::log(format!(
            "action start: {what} — target disk {} · {} · {}",
            disk.number,
            disk.name,
            disks::human(disk.size)
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = ProgressState {
            title: format!(
                "{} — disk {}",
                if surface_scan { "Surface scan" } else { "Read benchmark" },
                disk.number
            ),
            pct: None,
            detail: String::new(),
            samples: Vec::new(),
            logs: vec![format!(
                "Target: disk {} · {} · {}",
                disk.number,
                disk.name,
                disks::human(disk.size)
            )],
            started: Instant::now(),
            done: None,
            cancel: cancel.clone(),
            cancellable: true,
        };
        if surface_scan {
            crate::bench::spawn_surface_scan(self.tx.clone(), disk, cancel);
        } else {
            crate::bench::spawn_read_bench(self.tx.clone(), disk, cancel);
        }
        self.modal = Modal::Progress(progress);
    }
}

fn sanitize_label(raw: &str, preset: Preset) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        .take(preset.label_limit())
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        "UNTITLED".into()
    } else {
        cleaned
    }
}
