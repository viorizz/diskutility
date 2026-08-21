use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use crate::disks::{self, Disk};
use crate::logger;
use crate::ops::{self, OpEvent, Preset, PRESETS};

pub enum AppEvent {
    Disks(Result<Vec<Disk>, String>),
    Op(OpEvent),
    Update(String),
}

pub enum InputPurpose {
    Label(Preset),
    IsoPath,
}

pub enum PendingAction {
    Format { preset: Preset, label: String },
    EraseQuick,
    EraseSecure,
    WriteIso { path: PathBuf, size: u64 },
    BenchFull,
    Capacity { full: bool },
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
    Input { purpose: InputPurpose, buf: String },
    Confirm { action: PendingAction, buf: String },
    Progress(ProgressState),
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
    app_drive: Option<char>,
    tx: mpsc::Sender<AppEvent>,
    rx: mpsc::Receiver<AppEvent>,
    quit: bool,
}

impl App {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            disks: Vec::new(),
            selected: 0,
            scanning: false,
            elevated: ops::is_elevated(),
            unlocked: false,
            update: None,
            modal: Modal::None,
            status: None,
            tick: 0,
            app_drive: std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().and_then(|s| s.chars().next()))
                .map(|c| c.to_ascii_uppercase()),
            tx,
            rx,
            quit: false,
        }
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        self.refresh();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if let Ok(Some((tag, _))) = crate::update::check_latest() {
                let _ = tx.send(AppEvent::Update(tag));
            }
        });
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

    fn selected_disk(&self) -> Option<&Disk> {
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

    /// The phrase the user must type to confirm an action on the selected disk.
    /// Protected disks (with the override active) demand a scarier phrase.
    pub fn confirm_phrase(&self) -> String {
        match self.selected_disk() {
            Some(d) if !self.protection_reasons(d).is_empty() => format!("DESTROY {}", d.number),
            Some(d) => d.number.to_string(),
            None => String::new(),
        }
    }

    /// Returns true if a destructive action is allowed on the selected disk.
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
        true
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
                    idx = (idx + 3) % 4;
                    self.modal = Modal::TestMenu { idx };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    idx = (idx + 1) % 4;
                    self.modal = Modal::TestMenu { idx };
                }
                KeyCode::Enter => match idx {
                    0 => self.start_read_bench(),
                    _ => {
                        if self.guard() {
                            let action = match idx {
                                1 => PendingAction::BenchFull,
                                2 => PendingAction::Capacity { full: false },
                                _ => PendingAction::Capacity { full: true },
                            };
                            self.modal = Modal::Confirm { action, buf: String::new() };
                        }
                    }
                },
                _ => self.modal = Modal::TestMenu { idx },
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
            _ => {}
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
        let disk = self.selected_disk().ok_or("no disk selected")?;
        if meta.len() > disk.size {
            return Err(format!(
                "image ({}) is larger than disk {} ({})",
                disks::human(meta.len()),
                disk.number,
                disks::human(disk.size)
            ));
        }
        Ok((path, meta.len()))
    }

    fn start_action(&mut self, action: PendingAction) {
        let Some(disk) = self.selected_disk().cloned() else {
            self.error("no disk selected");
            return;
        };
        // belt-and-braces: protected disks always require the explicit override
        if disk.is_protected() && !self.unlocked {
            self.error("refusing to touch the Windows system/boot disk (press u to override)");
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
        }
        self.modal = Modal::Progress(progress);
    }

    /// Read benchmark is non-destructive, so it skips the confirm dialog and
    /// is allowed even on protected disks — it only needs elevation.
    fn start_read_bench(&mut self) {
        if !self.elevated {
            self.error("administrator rights required — restart this app from an elevated terminal");
            return;
        }
        let Some(disk) = self.selected_disk().cloned() else {
            self.error("no disk selected");
            return;
        };
        logger::log(format!(
            "action start: read benchmark — target disk {} · {} · {}",
            disk.number,
            disk.name,
            disks::human(disk.size)
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = ProgressState {
            title: format!("Read benchmark — disk {}", disk.number),
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
        crate::bench::spawn_read_bench(self.tx.clone(), disk, cancel);
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
