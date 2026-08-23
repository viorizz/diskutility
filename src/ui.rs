use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Gauge, List, ListItem, ListState, Padding, Paragraph, Sparkline, Wrap};
use ratatui::Frame;
use utility_core::ui::{draw_footer as core_footer, draw_header as core_header, modal_block, spinner, HeaderStatus, Hint, Theme};

use crate::app::{pause_options, schedule_fields, schedule_value, App, InputPurpose, Modal, PendingAction, ProgressState};
use crate::config::Schedule;
use crate::jobs;
use crate::disks::{fit, human, Disk};
use crate::ops::PRESETS;

/// The shared *Utility look, DiskUtility's accent.
pub const THEME: Theme = Theme::DISK;
const ACCENT: Color = THEME.accent;
const ACCENT_SOFT: Color = THEME.accent_soft;
const DIM: Color = THEME.dim;
const TEXT: Color = THEME.text;
const OK_C: Color = THEME.ok;
const ERR_C: Color = THEME.err;
const WARN_C: Color = THEME.warn;
const SEL_BG: Color = THEME.sel_bg;
const BORDER: Color = THEME.border;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    // An extra row above the footer appears while a scheduled backup runs.
    let bar_h = if app.jobs.is_empty() { 0 } else { 1 };
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(bar_h),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(f, rows[0], app);

    let cols = Layout::horizontal([Constraint::Length(46), Constraint::Min(30)]).split(rows[1]);
    draw_disk_list(f, cols[0], app);
    draw_details(f, cols[1], app);
    if !app.jobs.is_empty() {
        draw_jobs_bar(f, rows[2], app);
    }
    draw_footer(f, rows[3], app);

    match &app.modal {
        Modal::None => {}
        Modal::Help => draw_help(f, area),
        Modal::Unlock { buf } => draw_unlock(f, area, buf),
        Modal::Presets { idx } => draw_presets(f, area, *idx),
        Modal::EraseMenu { idx } => draw_erase_menu(f, area, *idx),
        Modal::TestMenu { idx } => draw_test_menu(f, area, *idx),
        Modal::Health { title, report } => draw_health(f, area, app, title, report.as_ref()),
        Modal::BackupMenu { idx } => draw_backup_menu(f, area, app, *idx),
        Modal::Input { purpose, buf } => draw_input(f, area, app, purpose, buf),
        Modal::Confirm { action, buf } => draw_confirm(f, area, app, action, buf),
        Modal::Progress(p) => draw_progress(f, area, app, p),
        Modal::Update { steps, done } => draw_update(f, area, app, steps, done.as_ref()),
        Modal::Schedule { s, field, installed } => draw_schedule(f, area, app, s, *field, *installed),
        Modal::ManageMenu { idx } => draw_manage_menu(f, area, app, *idx),
        Modal::Backups { idx } => draw_backups(f, area, app, *idx),
        Modal::StopJob { pid } => draw_stop_job(f, area, app, *pid),
        Modal::ConfirmConcurrent { path } => draw_confirm_concurrent(f, area, app, path),
        Modal::PauseMenu { idx, .. } => draw_pause_menu(f, area, app, *idx),
    }
}

fn bordered(title: Line<'static>) -> Block<'static> {
    THEME.bordered(title)
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let mut badges = Vec::new();
    if app.unlocked {
        badges.push(Span::styled("⛨ PROTECTIONS OFF", Style::new().fg(ERR_C).bold()));
    }
    let status = HeaderStatus {
        update_available: app.update.clone(),
        badges,
        alert: app.unlocked,
        show_elevation: true,
        elevated: app.elevated,
    };
    core_header(f, area, &THEME, &crate::APP, &status);
}

fn draw_disk_list(f: &mut Frame, area: Rect, app: &App) {
    let title = if app.scanning {
        Line::from(vec![
            Span::styled(" Disks ", Style::new().fg(TEXT).bold()),
            Span::styled(
                format!("{} scanning… ", spinner(app.tick)),
                Style::new().fg(ACCENT_SOFT),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(" Disks ", Style::new().fg(TEXT).bold()),
            Span::styled(format!("({}) ", app.disks.len()), Style::new().fg(DIM)),
        ])
    };
    let block = bordered(title).padding(Padding::horizontal(1));

    if app.disks.is_empty() && !app.scanning {
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(
            Paragraph::new("no disks found — press r to rescan").style(Style::new().fg(DIM)),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .disks
        .iter()
        .map(|d| {
            let protected = d.is_protected();
            let num_style = if protected {
                Style::new().fg(WARN_C)
            } else {
                Style::new().fg(ACCENT_SOFT)
            };
            let tag = if protected { "⛨" } else { " " };
            let line = Line::from(vec![
                Span::styled(format!("{} ", tag), Style::new().fg(WARN_C)),
                Span::styled(format!("{:>2} ", d.number), num_style.bold()),
                Span::styled(format!("{:<20} ", fit(&d.name, 20)), Style::new().fg(TEXT)),
                Span::styled(format!("{:>9} ", human(d.size)), Style::new().fg(DIM)),
                Span::styled(format!("{:<5}", fit(&d.bus, 5)), Style::new().fg(DIM)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(SEL_BG).add_modifier(Modifier::BOLD));
    let mut state = ListState::default();
    state.select(if app.disks.is_empty() { None } else { Some(app.selected) });
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_details(f: &mut Frame, area: Rect, app: &App) {
    let block = bordered(Line::from(Span::styled(
        " Details ",
        Style::new().fg(TEXT).bold(),
    )))
    .padding(Padding::new(1, 1, 0, 0));

    let Some(d) = app.disks.get(app.selected) else {
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(
            Paragraph::new("select a disk").style(Style::new().fg(DIM)),
            inner,
        );
        return;
    };

    let kv = |k: &str, v: String, c: Color| {
        Line::from(vec![
            Span::styled(format!(" {:<9}", k), Style::new().fg(DIM)),
            Span::styled(v, Style::new().fg(c)),
        ])
    };

    let mut lines: Vec<Line> = vec![
        kv("Name", d.name.clone(), TEXT),
        kv(
            "Disk",
            format!("#{} · {} · {}", d.number, d.bus, d.style),
            TEXT,
        ),
        kv(
            "Serial",
            if d.serial.is_empty() { "—".into() } else { d.serial.clone() },
            TEXT,
        ),
        kv("Size", human(d.size), TEXT),
        kv(
            "Health",
            format!(
                "{}{}",
                if d.health.is_empty() { "Unknown".to_string() } else { d.health.clone() },
                if d.offline { " · OFFLINE" } else { " · online" }
            ),
            if d.health == "Healthy" { OK_C } else { WARN_C },
        ),
    ];

    let mut flags: Vec<&str> = Vec::new();
    if d.system {
        flags.push("SYSTEM");
    }
    if d.boot {
        flags.push("BOOT");
    }
    if d.readonly {
        flags.push("READ-ONLY");
    }
    if !flags.is_empty() {
        lines.push(kv("Flags", format!("{} — protected", flags.join(" · ")), WARN_C));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " Partitions",
        Style::new().fg(ACCENT_SOFT).bold(),
    )));

    if d.partitions.is_empty() {
        lines.push(Line::from(Span::styled(
            "   none — disk is blank (RAW)",
            Style::new().fg(DIM).italic(),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!(
                "   {:<3} {:<4} {:<7} {:<16} {:>10} {:>12}",
                "#", "DRV", "FS", "LABEL", "SIZE", "FREE"
            ),
            Style::new().fg(DIM),
        )));
        for p in &d.partitions {
            let letter = if p.letter.is_empty() {
                "—".to_string()
            } else {
                format!("{}:", p.letter)
            };
            let fs = if p.fs.is_empty() {
                fit(&p.kind, 7)
            } else {
                p.fs.clone()
            };
            let free = if p.free > 0 { format!("{} free", human(p.free)) } else { "—".into() };
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "   {:<3} {:<4} {:<7} {:<16} {:>10} {:>12}",
                    p.number,
                    letter,
                    fit(&fs, 7),
                    fit(&p.label, 16),
                    human(p.size),
                    free
                ),
                Style::new().fg(TEXT),
            )]));
        }
    }

    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// One-line status bar: progress of backups running in other processes
/// (a Task Scheduler run, or another diskutility window).
fn draw_jobs_bar(f: &mut Frame, area: Rect, app: &App) {
    let Some(j) = app.jobs.first() else { return };
    if area.height == 0 {
        return;
    }
    let pct = (j.frac.clamp(0.0, 1.0) * 100.0) as u16;
    let cols = Layout::horizontal([Constraint::Length(22), Constraint::Min(20), Constraint::Length(24)]).split(area);
    let tag = if app.jobs.len() > 1 {
        format!(" ⏱ {} backups ", app.jobs.len())
    } else {
        format!(" ⏱ {} backup ", j.kind.label())
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(tag, Style::new().fg(WARN_C).bold()),
            Span::styled(spinner(app.tick), Style::new().fg(WARN_C)),
        ])),
        cols[0],
    );
    let label = format!("{pct}%  {} → {}  ·  {}", j.disk, j.image, j.detail);
    f.render_widget(
        Gauge::default()
            .gauge_style(Style::new().fg(if j.cancelling() { ERR_C } else { ACCENT }).bg(BORDER))
            .ratio(j.frac.clamp(0.0, 1.0))
            .label(Span::styled(label, Style::new().fg(TEXT).bold())),
        cols[1],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Shift+B", Style::new().fg(ACCENT_SOFT).bold()),
            Span::styled(" backups panel", Style::new().fg(DIM)),
        ])),
        cols[2],
    );
}

fn fmt_elapsed(secs: u64) -> String {
    crate::ops::fmt_secs_pub(secs as f64)
}

/// Shift+B: everything about backups in flight — running jobs (with a gauge
/// each), the registered schedule, and interrupted images that the next
/// backup would resume.
fn draw_backups(f: &mut Frame, area: Rect, app: &App, idx: usize) {
    let partials = app.resumable_partials();
    let h = (15 + app.jobs.len() as u16 * 4 + partials.len().max(1) as u16).min(area.height.saturating_sub(2));
    let inner = modal_block(f, area, 90, h, "Backups", ACCENT);
    let dim = |t: String| Line::from(Span::styled(t, Style::new().fg(DIM)));
    let head = |t: &'static str| Line::from(Span::styled(t, Style::new().fg(ACCENT_SOFT).bold()));
    let mut lines: Vec<Line> = Vec::new();

    lines.push(head("Running now"));
    if app.jobs.is_empty() {
        lines.push(dim("  none — nothing is being backed up in the background".into()));
    }
    let mut y_gauges: Vec<(u16, &jobs::Job)> = Vec::new();
    for (i, j) in app.jobs.iter().enumerate() {
        let sel = i == idx;
        let style = if sel { Style::new().fg(ACCENT_SOFT).bg(SEL_BG).bold() } else { Style::new().fg(TEXT).bold() };
        let started = chrono::DateTime::from_timestamp(j.started as i64, 0)
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(if sel { "▸ " } else { "  " }, style),
            Span::styled(format!("{:<10}", j.kind.label()), style),
            Span::styled(format!("{}  →  {}", j.disk, j.image), Style::new().fg(TEXT)),
        ]));
        lines.push(dim(format!(
            "             pid {} · started {started} · running {} · {}",
            j.pid,
            fmt_elapsed(j.elapsed_secs()),
            j.detail
        )));
        y_gauges.push((lines.len() as u16, j));
        lines.push(Line::default()); // gauge row, drawn below
        lines.push(Line::default());
    }

    lines.push(head("Schedule"));
    match &app.config.schedule {
        Some(sc) => {
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", sc.disk_name), Style::new().fg(TEXT).bold()),
                Span::styled(format!("{} → {}", sc.describe(), sc.dest_dir), Style::new().fg(TEXT)),
            ]));
            lines.push(dim(format!(
                "  keep {} · task {}",
                if sc.keep == 0 { "all".to_string() } else { sc.keep.to_string() },
                if crate::schedule::is_installed() { "registered" } else { "NOT registered (a to fix)" }
            )));
            match sc.pause_text(jobs::now_unix()) {
                Some(t) => lines.push(Line::from(Span::styled(
                    format!("  ⏸ {t} — scheduled runs are skipped (p to resume)"),
                    Style::new().fg(WARN_C).bold(),
                ))),
                None => lines.push(dim("  active (p to pause for a while or until further notice)".into())),
            }
        }
        None => lines.push(dim("  none — press a on a disk to schedule automatic backups".into())),
    }
    lines.push(Line::default());

    lines.push(head("Interrupted images (resumed by the next backup of that disk)"));
    if partials.is_empty() {
        lines.push(dim("  none".into()));
    }
    for (path, done, size) in &partials {
        lines.push(Line::from(vec![
            Span::styled(format!("  {path}"), Style::new().fg(TEXT)),
            Span::styled(
                format!("   {} of {} ({}%)", human(*done), human(*size), done * 100 / (*size).max(1)),
                Style::new().fg(WARN_C),
            ),
        ]));
    }
    lines.push(Line::default());
    lines.push(dim("↑↓ select a running backup · x stop it (partial image kept) · p pause/resume schedule · r refresh · Esc close".into()));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    for (y, j) in y_gauges {
        if y >= inner.height {
            break;
        }
        let r = Rect { x: inner.x + 13, y: inner.y + y, width: inner.width.saturating_sub(14), height: 1 };
        f.render_widget(
            Gauge::default()
                .gauge_style(Style::new().fg(if j.cancelling() { ERR_C } else { ACCENT }).bg(BORDER))
                .ratio(j.frac.clamp(0.0, 1.0))
                .label(Span::styled(format!("{:.0}%", j.frac * 100.0), Style::new().fg(TEXT).bold())),
            r,
        );
    }
}

fn draw_stop_job(f: &mut Frame, area: Rect, app: &App, pid: u32) {
    let inner = modal_block(f, area, 70, 10, "Stop this backup?", WARN_C);
    let t = |x: String| Line::from(Span::styled(x, Style::new().fg(TEXT)));
    let (kind, disk, image) = app
        .jobs
        .iter()
        .find(|j| j.pid == pid)
        .map(|j| (j.kind.label().to_string(), j.disk.clone(), j.image.clone()))
        .unwrap_or_default();
    let lines = vec![
        t(format!("Running: {kind} backup of {disk}")),
        t(format!("Image:   {image}")),
        Line::default(),
        t("The process will abort within a few seconds. The partial image is kept".into()),
        t("and the next backup of this disk resumes from it. The disk itself is only read.".into()),
        Line::default(),
        Line::from(vec![
            Span::styled("y", Style::new().fg(ERR_C).bold()),
            Span::styled(" stop the backup   ", Style::new().fg(TEXT)),
            Span::styled("Esc / n", Style::new().fg(ACCENT_SOFT).bold()),
            Span::styled(" let it run", Style::new().fg(TEXT)),
        ]),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_confirm_concurrent(f: &mut Frame, area: Rect, app: &App, path: &std::path::Path) {
    let inner = modal_block(f, area, 76, 12 + app.jobs.len() as u16, "⚠ A backup is already running", WARN_C);
    let t = |x: String| Line::from(Span::styled(x, Style::new().fg(TEXT)));
    let mut lines = vec![];
    for j in &app.jobs {
        lines.push(Line::from(vec![
            Span::styled(format!("  {} · ", j.kind.label()), Style::new().fg(WARN_C).bold()),
            Span::styled(format!("{} → {} · {:.0}%", j.disk, j.image, j.frac * 100.0), Style::new().fg(TEXT)),
        ]));
    }
    lines.push(Line::default());
    lines.push(t(format!("You are about to start another one: {}", path.display())));
    lines.push(Line::default());
    lines.push(t("Two backups at once read the disk(s) concurrently and share the destination's".into()));
    lines.push(t("bandwidth — each will take roughly twice as long. If this is the same disk and".into()));
    lines.push(t("folder, it is better to let the running one finish (it can be stopped and".into()));
    lines.push(t("resumed later from the Backups panel).".into()));
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("y", Style::new().fg(ERR_C).bold()),
        Span::styled(" start anyway   ", Style::new().fg(TEXT)),
        Span::styled("b", Style::new().fg(ACCENT_SOFT).bold()),
        Span::styled(" open the Backups panel   ", Style::new().fg(TEXT)),
        Span::styled("Esc / n", Style::new().fg(ACCENT_SOFT).bold()),
        Span::styled(" don't start", Style::new().fg(TEXT)),
    ]));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_pause_menu(f: &mut Frame, area: Rect, app: &App, idx: usize) {
    let paused = app.schedule_paused();
    let opts = pause_options(paused);
    let inner = modal_block(f, area, 66, 9 + opts.len() as u16, "Pause scheduled backups", WARN_C);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(sc) = &app.config.schedule {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", sc.disk_name), Style::new().fg(TEXT).bold()),
            Span::styled(sc.describe(), Style::new().fg(TEXT)),
        ]));
        lines.push(Line::from(Span::styled(
            match sc.pause_text(jobs::now_unix()) {
                Some(t) => format!("currently {t}"),
                None => "currently active".into(),
            },
            Style::new().fg(if paused { WARN_C } else { OK_C }),
        )));
        lines.push(Line::default());
    }
    for (i, (label, _)) in opts.iter().enumerate() {
        let sel = i == idx;
        let style = if sel { Style::new().fg(ACCENT_SOFT).bg(SEL_BG).bold() } else { Style::new().fg(TEXT) };
        lines.push(Line::from(Span::styled(format!("{}{label}", if sel { "▸ " } else { "  " }), style)));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "While paused the task stays registered but every run exits without doing anything.",
        Style::new().fg(DIM),
    )));
    lines.push(Line::from(Span::styled(
        "A timed pause lifts itself; a backup already running is not affected.",
        Style::new().fg(DIM),
    )));
    lines.push(Line::from(Span::styled("Enter choose · Esc back", Style::new().fg(DIM))));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let hints: &[Hint] = &[
        ("↑↓", "select"),
        ("f", "format"),
        ("e", "erase"),
        ("i", "write"),
        ("s", "backup"),
        ("n", "backup dir"),
        ("d", "clone"),
        ("a", "auto"),
        ("m", "manage"),
        ("b", "test"),
        ("h", "health"),
        ("r", "rescan"),
        ("c", "log"),
        ("u", if app.unlocked { "re-lock" } else { "override" }),
        ("?", "help"),
        ("q", "quit"),
    ];
    core_footer(f, area, &THEME, app.status.as_ref().map(|(m, e, _)| (m.as_str(), *e)), hints);
}

// ---------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------

fn draw_presets(f: &mut Frame, area: Rect, idx: usize) {
    let inner = modal_block(f, area, 66, 16, "Format — choose a target", ACCENT);
    let rows = Layout::vertical([Constraint::Length(PRESETS.len() as u16), Constraint::Min(3)])
        .split(inner);

    let mut lines: Vec<Line> = Vec::new();
    for (i, p) in PRESETS.iter().enumerate() {
        let selected = i == idx;
        let marker = if selected { "▸ " } else { "  " };
        let style = if selected {
            Style::new().fg(ACCENT_SOFT).bg(SEL_BG).bold()
        } else {
            Style::new().fg(TEXT)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{:<16} {}", p.name(), p.fs_display()),
            style,
        )));
    }
    f.render_widget(Paragraph::new(lines), rows[0]);

    f.render_widget(
        Paragraph::new(PRESETS[idx].desc())
            .style(Style::new().fg(DIM))
            .wrap(Wrap { trim: true }),
        rows[1],
    );
}

fn draw_test_menu(f: &mut Frame, area: Rect, idx: usize) {
    let inner = modal_block(f, area, 72, 15, "Test disk", ACCENT);
    let opts: [(&str, &str); 5] = [
        (
            "Read benchmark  (safe)",
            "Sequential + random 4K read speed with a live graph. Non-destructive — safe on any disk, even the system disk. Starts immediately.",
        ),
        (
            "Surface scan  (safe)",
            "Reads every sector and pinpoints unreadable 4 KiB blocks. Non-destructive; takes as long as one full read of the disk. Starts immediately.",
        ),
        (
            "Full benchmark  (DESTROYS DATA)",
            "Sequential and random 4K read AND write speeds. The disk is wiped first and left blank afterwards.",
        ),
        (
            "Capacity test — quick  (DESTROYS DATA)",
            "Writes ~256 pattern samples across the whole address space and verifies them. Catches counterfeit 'fake capacity' drives in minutes.",
        ),
        (
            "Capacity test — full  (DESTROYS DATA)",
            "h2testw-style: writes and verifies EVERY byte. Definitive proof of real capacity and surface health, but takes hours on large disks.",
        ),
    ];
    let mut lines: Vec<Line> = Vec::new();
    for (i, (name, _)) in opts.iter().enumerate() {
        let selected = i == idx;
        let marker = if selected { "▸ " } else { "  " };
        let style = if selected {
            Style::new().fg(ACCENT_SOFT).bg(SEL_BG).bold()
        } else {
            Style::new().fg(TEXT)
        };
        lines.push(Line::from(Span::styled(format!("{marker}{name}"), style)));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(opts[idx].1, Style::new().fg(DIM))));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_erase_menu(f: &mut Frame, area: Rect, idx: usize) {
    let inner = modal_block(f, area, 62, 10, "Erase", ACCENT);
    let opts = [
        ("Quick erase", "Destroy the partition table. Fast; data is not overwritten."),
        ("Secure erase (zero-fill)", "Overwrite every byte with zeros. Slow; data is unrecoverable."),
    ];
    let mut lines: Vec<Line> = Vec::new();
    for (i, (name, _)) in opts.iter().enumerate() {
        let selected = i == idx;
        let marker = if selected { "▸ " } else { "  " };
        let style = if selected {
            Style::new().fg(ACCENT_SOFT).bg(SEL_BG).bold()
        } else {
            Style::new().fg(TEXT)
        };
        lines.push(Line::from(Span::styled(format!("{marker}{name}"), style)));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(opts[idx].1, Style::new().fg(DIM))));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_backup_menu(f: &mut Frame, area: Rect, app: &App, idx: usize) {
    let choices = app.backup_choices();
    let disk = app.selected_disk();
    let warn_h = if app.jobs.is_empty() { 0 } else { 2 + app.jobs.len() as u16 };
    let h = 9 + choices.len() as u16 + warn_h;
    let inner = modal_block(f, area, 78, h, "Backup — where should the image go?", ACCENT);
    let mut lines: Vec<Line> = Vec::new();
    if !app.jobs.is_empty() {
        lines.push(Line::from(Span::styled(
            "⚠ A backup is already running — starting another will ask you to confirm:",
            Style::new().fg(WARN_C).bold(),
        )));
        for j in &app.jobs {
            lines.push(Line::from(vec![
                Span::styled(format!("   {} · ", j.kind.label()), Style::new().fg(WARN_C)),
                Span::styled(format!("{} → {} · {:.0}%", j.disk, j.image, j.frac * 100.0), Style::new().fg(DIM)),
            ]));
        }
        lines.push(Line::default());
    }
    if let Some(d) = disk {
        lines.push(Line::from(vec![
            Span::styled("Image size  ", Style::new().fg(DIM)),
            Span::styled(human(d.size), Style::new().fg(TEXT).bold()),
            Span::styled(
                format!("   (raw sector image of disk {} · {})", d.number, d.name),
                Style::new().fg(DIM),
            ),
        ]));
        lines.push(Line::default());
    }
    for (i, (label, dir)) in choices.iter().enumerate() {
        let selected = i == idx;
        let marker = if selected { "▸ " } else { "  " };
        let style = if selected {
            Style::new().fg(ACCENT_SOFT).bg(SEL_BG).bold()
        } else {
            Style::new().fg(TEXT)
        };
        let free = dir
            .as_deref()
            .and_then(|d| crate::ops::free_space(&std::path::Path::new(d).join("x")))
            .map(|b| format!("  {} free", human(b)))
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}{label}"), style),
            Span::styled(free, Style::new().fg(DIM)),
        ]));
    }
    lines.push(Line::default());
    let hint = if app.config.backup_dir.is_some() {
        "Enter choose · n change the saved destination · Esc cancel"
    } else {
        "Enter choose · n save a network drive / folder as your go-to destination · Esc cancel"
    };
    lines.push(Line::from(Span::styled(hint, Style::new().fg(DIM))));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

/// "Image 1.86 TB · 3.2 TB free on \\nas\backups" for the backup path prompt.
fn backup_size_line(app: &App, buf: &str) -> Option<Line<'static>> {
    let disk = app.selected_disk()?;
    let mut spans = vec![
        Span::styled("Image size ", Style::new().fg(DIM)),
        Span::styled(human(disk.size), Style::new().fg(TEXT).bold()),
    ];
    let path = std::path::Path::new(buf.trim().trim_matches('"'));
    if let Some(free) = crate::ops::free_space(path) {
        let fits = free >= disk.size;
        spans.push(Span::styled("   ·   ", Style::new().fg(DIM)));
        spans.push(Span::styled(
            format!("{} free at destination", human(free)),
            Style::new().fg(if fits { OK_C } else { ERR_C }),
        ));
        if !fits {
            spans.push(Span::styled("  — will not fit", Style::new().fg(ERR_C).bold()));
        }
    }
    Some(Line::from(spans))
}

fn draw_input(f: &mut Frame, area: Rect, app: &App, purpose: &InputPurpose, buf: &str) {
    let (title, hint) = match purpose {
        InputPurpose::Label(p) => (
            format!("Volume label — {} ({})", p.name(), p.fs_display()),
            "letters, numbers, space, - _ .   ·   Enter continue · Esc cancel",
        ),
        InputPurpose::IsoPath => (
            "Path to disk image (.iso / .img)".to_string(),
            "paste or type the full path   ·   Enter continue · Esc cancel",
        ),
        InputPurpose::BackupPath => (
            "Backup — save a full image of the selected disk to".to_string(),
            "full path for the .img file (must not be on the disk itself)   ·   Enter start · Esc cancel",
        ),
        InputPurpose::BackupDir => (
            "Default backup destination (network drive or folder)".to_string(),
            r"Z:\backups or \\server\share\backups · mapped drives are stored as UNC · empty clears   ·   Enter save · Esc cancel",
        ),
        InputPurpose::CloneTarget => (
            "Clone — which disk should be OVERWRITTEN with a copy of the selected disk?".to_string(),
            "type the TARGET disk number from the list   ·   Enter continue · Esc cancel",
        ),
        InputPurpose::DriveLetter { partition, .. } => (
            format!("Drive letter for partition {partition}"),
            "type one letter   ·   Enter apply · Esc cancel",
        ),
        InputPurpose::VolumeLabel { letter } => (
            format!("New label for volume {letter}:"),
            "up to 32 characters (11 on FAT32)   ·   Enter apply · Esc cancel",
        ),
    };
    let extra = match purpose {
        InputPurpose::BackupPath => backup_size_line(app, buf),
        InputPurpose::DriveLetter { free, .. } => Some(Line::from(vec![
            Span::styled("Available  ", Style::new().fg(DIM)),
            Span::styled(
                free.iter().map(|c| format!("{c}:")).collect::<Vec<_>>().join(" "),
                Style::new().fg(OK_C),
            ),
        ])),
        _ => None,
    };
    let h = if extra.is_some() { 9 } else { 7 };
    let inner = modal_block(f, area, 78, h, &title, ACCENT);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("❯ ", Style::new().fg(ACCENT).bold()),
            Span::styled(buf.to_string(), Style::new().fg(TEXT)),
            Span::styled("▌", Style::new().fg(ACCENT_SOFT)),
        ]),
        Line::default(),
    ];
    if let Some(l) = extra {
        lines.push(l);
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled(hint, Style::new().fg(DIM))));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_confirm(f: &mut Frame, area: Rect, app: &App, action: &PendingAction, buf: &str) {
    let disk: Option<&Disk> = app.target_disk();
    let reasons: Vec<&str> = disk.map(|d| app.protection_reasons(d)).unwrap_or_default();
    let protected = !reasons.is_empty();
    let internal = disk.and_then(App::internal_bus_warning);
    let volumes: Vec<String> = disk
        .map(|d| {
            d.partitions
                .iter()
                .filter(|p| !p.letter.is_empty() || !p.fs.is_empty())
                .map(|p| {
                    let letter = if p.letter.is_empty() { String::new() } else { format!("{}: ", p.letter) };
                    let used = p.size.saturating_sub(p.free);
                    format!("{letter}{} {} used", if p.fs.is_empty() { "?" } else { &p.fs }, human(used))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut height = 14;
    if protected {
        height += 3;
    }
    if internal.is_some() {
        height += 2;
    }
    let inner = modal_block(f, area, 70, height, "⚠ DESTRUCTIVE OPERATION", ERR_C);
    let (num, dname, dsize) = disk
        .map(|d| (d.number.to_string(), d.name.clone(), human(d.size)))
        .unwrap_or_default();
    let phrase = app.confirm_phrase();

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Target   ", Style::new().fg(DIM)),
            Span::styled(
                format!("disk {num} · {dname} · {dsize}"),
                Style::new().fg(TEXT).bold(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Action   ", Style::new().fg(DIM)),
            Span::styled(action.summary(), Style::new().fg(TEXT)),
        ]),
        Line::from(vec![
            Span::styled("Contains ", Style::new().fg(DIM)),
            Span::styled(
                if volumes.is_empty() { "no recognised volumes".to_string() } else { volumes.join(" · ") },
                Style::new().fg(WARN_C),
            ),
        ]),
        Line::default(),
    ];
    if let Some(w) = &internal {
        lines.push(Line::from(Span::styled(
            format!("⚠ INTERNAL DISK: {w}. Is this really the drive you meant?"),
            Style::new().fg(WARN_C).bold(),
        )));
        lines.push(Line::default());
    }
    if protected {
        lines.push(Line::from(Span::styled(
            format!("⛨ PROTECTED DISK: {}", reasons.join(" · ")),
            Style::new().fg(ERR_C).bold(),
        )));
        lines.push(Line::from(Span::styled(
            "Safety override is ACTIVE. This can destroy Windows or kill this app mid-write.",
            Style::new().fg(ERR_C),
        )));
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled(
        "ALL DATA ON THIS DISK WILL BE PERMANENTLY LOST.",
        Style::new().fg(ERR_C).bold(),
    )));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!("Type {phrase} and press Enter to proceed:"),
        Style::new().fg(TEXT),
    )));
    lines.push(Line::from(vec![
        Span::styled("❯ ", Style::new().fg(ERR_C).bold()),
        Span::styled(buf.to_string(), Style::new().fg(TEXT).bold()),
        Span::styled("▌", Style::new().fg(ERR_C)),
    ]));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_unlock(f: &mut Frame, area: Rect, buf: &str) {
    let inner = modal_block(f, area, 74, 15, "⛨ Disable safety protections", ERR_C);
    let warn = |t: &'static str| Line::from(Span::styled(t, Style::new().fg(TEXT)));
    let lines = vec![
        warn("These disks are currently blocked from all destructive operations:"),
        Line::from(Span::styled(
            "  • the Windows system/boot disk",
            Style::new().fg(WARN_C),
        )),
        Line::from(Span::styled(
            "  • the disk hosting this running app",
            Style::new().fg(WARN_C),
        )),
        Line::default(),
        warn("Disabling them is for experts — e.g. wiping a disk pulled from another"),
        warn("machine that still carries boot flags. One wrong disk number here can"),
        warn("destroy your Windows installation. The override lasts until you press"),
        warn("u again or close the app, and protected disks demand a DESTROY phrase."),
        Line::default(),
        Line::from(Span::styled(
            "Type UNLOCK and press Enter to proceed (Esc to keep protections):",
            Style::new().fg(ERR_C).bold(),
        )),
        Line::from(vec![
            Span::styled("❯ ", Style::new().fg(ERR_C).bold()),
            Span::styled(buf.to_string(), Style::new().fg(TEXT).bold()),
            Span::styled("▌", Style::new().fg(ERR_C)),
        ]),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_progress(f: &mut Frame, area: Rect, app: &App, p: &ProgressState) {
    let color = match &p.done {
        Some(Ok(_)) => OK_C,
        Some(Err(_)) => ERR_C,
        None => ACCENT,
    };
    let inner = modal_block(f, area, 76, 19, &p.title, color);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(2),
        Constraint::Length(1),
    ])
    .split(inner);

    // live speed graph (populated by benchmark & raw-write operations)
    if !p.samples.is_empty() && p.done.is_none() {
        let width = rows[2].width as usize;
        let tail = &p.samples[p.samples.len().saturating_sub(width)..];
        f.render_widget(
            Sparkline::default()
                .data(tail)
                .style(Style::new().fg(ACCENT_SOFT)),
            rows[2],
        );
    }

    // progress row
    match (&p.done, p.pct) {
        (None, Some(frac)) => {
            let gauge = Gauge::default()
                .ratio(frac.clamp(0.0, 1.0))
                .gauge_style(Style::new().fg(ACCENT).bg(Color::Rgb(45, 45, 52)))
                .label(format!("{:.1}%", frac * 100.0));
            f.render_widget(gauge, rows[0]);
        }
        (None, None) => {
            let secs = p.started.elapsed().as_secs();
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("{} ", spinner(app.tick)),
                        Style::new().fg(ACCENT_SOFT).bold(),
                    ),
                    Span::styled(format!("working… {secs}s"), Style::new().fg(TEXT)),
                ])),
                rows[0],
            );
        }
        (Some(Ok(msg)), _) => {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("✓ ", Style::new().fg(OK_C).bold()),
                    Span::styled(msg.clone(), Style::new().fg(OK_C)),
                ]))
                .wrap(Wrap { trim: true }),
                rows[0].union(rows[1]),
            );
        }
        (Some(Err(msg)), _) => {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("✗ ", Style::new().fg(ERR_C).bold()),
                    Span::styled(msg.clone(), Style::new().fg(ERR_C)),
                ]))
                .wrap(Wrap { trim: true }),
                rows[0].union(rows[1]),
            );
        }
    }

    if p.done.is_none() && !p.detail.is_empty() {
        f.render_widget(
            Paragraph::new(p.detail.clone()).style(Style::new().fg(TEXT)),
            rows[1],
        );
    }

    // recent log lines
    let visible = rows[3].height as usize;
    let logs: Vec<Line> = p
        .logs
        .iter()
        .rev()
        .take(visible)
        .rev()
        .map(|l| Line::from(Span::styled(format!("· {l}"), Style::new().fg(DIM))))
        .collect();
    f.render_widget(Paragraph::new(logs).wrap(Wrap { trim: true }), rows[3]);

    // hint row
    let hint = match &p.done {
        Some(Ok(_)) => "Enter close (rescans disks) · c copy log",
        Some(Err(_)) => "c copy full log to clipboard (paste it for troubleshooting) · Enter close",
        None if p.cancellable => "x cancel · c copy log — do NOT unplug the drive",
        None => "c copy log — please wait, this step can't be cancelled",
    };
    f.render_widget(
        Paragraph::new(hint).style(Style::new().fg(DIM).italic()),
        rows[4],
    );
}

fn draw_health(
    f: &mut Frame,
    area: Rect,
    app: &App,
    title: &str,
    report: Option<&Result<crate::ops::HealthReport, String>>,
) {
    let inner = modal_block(f, area, 70, 17, title, ACCENT);
    let kv = |k: &str, v: String, c: Color| {
        Line::from(vec![
            Span::styled(format!("{:<12}", k), Style::new().fg(DIM)),
            Span::styled(v, Style::new().fg(c)),
        ])
    };
    let na = || "n/a".to_string();
    let lines: Vec<Line> = match report {
        None => vec![Line::from(vec![
            Span::styled(
                format!("{} ", spinner(app.tick)),
                Style::new().fg(ACCENT_SOFT).bold(),
            ),
            Span::styled("querying drive health…", Style::new().fg(TEXT)),
        ])],
        Some(Err(e)) => vec![
            Line::from(Span::styled(format!("✗ {e}"), Style::new().fg(ERR_C))),
            Line::default(),
            Line::from(Span::styled("Esc to close", Style::new().fg(DIM))),
        ],
        Some(Ok(h)) => {
            let health_c = if h.health == "Healthy" { OK_C } else { ERR_C };
            let media = if h.spindle > 0 {
                format!("{} · {} rpm", h.media, h.spindle)
            } else {
                h.media.clone()
            };
            let mut v = vec![
                kv("Health", if h.health.is_empty() { na() } else { h.health.clone() }, health_c),
                kv("Media", if media.is_empty() { na() } else { media }, TEXT),
                kv("Usage", if h.usage.is_empty() { na() } else { h.usage.clone() }, TEXT),
                Line::default(),
                Line::from(Span::styled(
                    "SMART / reliability counters",
                    Style::new().fg(ACCENT_SOFT).bold(),
                )),
            ];
            if h.rc_ok {
                let wear_c = if h.wear >= 90 { ERR_C } else if h.wear >= 70 { WARN_C } else { OK_C };
                let temp_c = if h.temp >= 70 { ERR_C } else if h.temp >= 55 { WARN_C } else { OK_C };
                v.push(kv(
                    "Wear",
                    if h.wear < 0 { na() } else { format!("{} % of rated endurance used", h.wear) },
                    if h.wear < 0 { DIM } else { wear_c },
                ));
                v.push(kv(
                    "Temperature",
                    if h.temp < 0 {
                        na()
                    } else if h.temp_max > 0 {
                        format!("{} °C  (max recorded {} °C)", h.temp, h.temp_max)
                    } else {
                        format!("{} °C", h.temp)
                    },
                    if h.temp < 0 { DIM } else { temp_c },
                ));
                v.push(kv(
                    "Power-on",
                    if h.hours < 0 {
                        na()
                    } else {
                        format!("{} h  (~{:.1} years)", h.hours, h.hours as f64 / 8760.0)
                    },
                    if h.hours < 0 { DIM } else { TEXT },
                ));
                let err_c = |n: i64| if n < 0 { DIM } else if n > 0 { WARN_C } else { OK_C };
                v.push(kv(
                    "Read errors",
                    if h.read_err < 0 { na() } else { h.read_err.to_string() },
                    err_c(h.read_err),
                ));
                v.push(kv(
                    "Write errors",
                    if h.write_err < 0 { na() } else { h.write_err.to_string() },
                    err_c(h.write_err),
                ));
            } else if !app.elevated {
                v.push(Line::from(Span::styled(
                    "Detailed counters require an elevated (administrator) terminal — restart the app elevated for wear, temperature and error data.",
                    Style::new().fg(WARN_C),
                )));
            } else {
                v.push(Line::from(Span::styled(
                    "Detailed counters unavailable for this device. USB enclosures usually block SMART passthrough — connect the drive directly (SATA/NVMe) for wear, temperature and error data.",
                    Style::new().fg(DIM),
                )));
            }
            v.push(Line::default());
            v.push(Line::from(Span::styled("Esc close · c copy log", Style::new().fg(DIM))));
            v
        }
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let inner = modal_block(f, area, 74, 29, "Help", ACCENT);
    let key = |k: &'static str, d: &'static str| {
        Line::from(vec![
            Span::styled(format!("  {:<10}", k), Style::new().fg(ACCENT_SOFT).bold()),
            Span::styled(d, Style::new().fg(TEXT)),
        ])
    };
    let note = |t: &'static str| Line::from(Span::styled(t, Style::new().fg(DIM)));
    let lines = vec![
        key("↑↓ / jk", "select a disk"),
        key("f", "format — Windows, macOS, Linux, PS5 or Universal preset"),
        key("e", "erase — quick (partition table) or secure (zero-fill)"),
        key("i", "write a bootable .iso/.img image to the disk (verified)"),
        key("b", "benchmark, surface scan & capacity tests"),
        key("h", "drive health: SMART wear, temperature, hours, error counts"),
        key("s", "backup the whole disk to an .img file (restore with i)"),
        key("n", "set a network drive / folder as the default backup destination"),
        key("d", "clone the disk sector-for-sector onto another disk (verified)"),
        key("a", "automatic backups — schedule a Task Scheduler job for the disk"),
        key("m", "manage: drive letters, volume label, online/offline, eject, read-only"),
        key("Shift+U", "install an available update / toggle auto-update on launch"),
        key("Shift+B", "backups panel: running jobs (x stop), schedule (p pause), resumable images"),
        key("r", "rescan disks"),
        key("c", "copy the full session log to the clipboard (bug reports)"),
        key("u", "toggle safety override — allow protected disks (DANGEROUS)"),
        key("q / Esc", "quit"),
        Line::default(),
        Line::from(Span::styled(
            format!(
                "  Log file: {}",
                crate::logger::path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "unavailable".into())
            ),
            Style::new().fg(DIM),
        )),
        note("Destructive operations require an elevated (administrator)"),
        note("terminal and typing the disk number to confirm."),
        note("The Windows system/boot disk is always protected."),
        Line::default(),
        note("macOS preset uses exFAT (APFS/HFS+ can only be created by a Mac)."),
        note("Linux preset builds real ext4 through WSL 2."),
        note("PS5 media/backup drives use exFAT; game storage is formatted by the console."),
        Line::default(),
        note("press any key to close"),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_update(
    f: &mut Frame,
    area: Rect,
    app: &App,
    steps: &[String],
    done: Option<&Result<String, String>>,
) {
    let color = match done {
        Some(Ok(_)) => OK_C,
        Some(Err(_)) => ERR_C,
        None => ACCENT,
    };
    let h = 13 + steps.len().min(8) as u16;
    let inner = modal_block(f, area, 76, h, "Update", color);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("Installed  ", Style::new().fg(DIM)),
        Span::styled(format!("v{}", env!("CARGO_PKG_VERSION")), Style::new().fg(TEXT).bold()),
        Span::styled("   Latest  ", Style::new().fg(DIM)),
        match &app.update {
            Some(tag) => Span::styled(tag.clone(), Style::new().fg(ACCENT_SOFT).bold()),
            None => Span::styled("no newer release found", Style::new().fg(TEXT)),
        },
    ]));
    lines.push(Line::from(vec![
        Span::styled("Auto-update on launch  ", Style::new().fg(DIM)),
        if app.config.auto_update {
            Span::styled("ON", Style::new().fg(OK_C).bold())
        } else {
            Span::styled("off", Style::new().fg(TEXT))
        },
    ]));
    lines.push(Line::default());
    if steps.is_empty() && done.is_none() {
        if app.update.is_some() {
            lines.push(Line::from(Span::styled(
                "Download the new release, verify its SHA-256 against the published checksums, and restart into it?",
                Style::new().fg(TEXT),
            )));
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "y  update now   ·   n  not now   ·   a  toggle auto-update on launch",
                Style::new().fg(DIM),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "You are running the latest version (or the startup check is still running / disabled).",
                Style::new().fg(TEXT),
            )));
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "a  toggle auto-update on launch   ·   Esc  close",
                Style::new().fg(DIM),
            )));
        }
    } else {
        for st in steps.iter().rev().take(8).rev() {
            lines.push(Line::from(vec![
                Span::styled("  · ", Style::new().fg(DIM)),
                Span::styled(st.clone(), Style::new().fg(TEXT)),
            ]));
        }
        lines.push(Line::default());
        match done {
            None => lines.push(Line::from(Span::styled(
                format!("{} working — please wait", spinner(app.tick)),
                Style::new().fg(ACCENT_SOFT),
            ))),
            Some(Ok(m)) => {
                lines.push(Line::from(Span::styled(m.clone(), Style::new().fg(OK_C))));
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    if crate::update::updated(m) {
                        "Enter  restart into the new version now"
                    } else {
                        "Enter  close"
                    },
                    Style::new().fg(DIM),
                )));
            }
            Some(Err(e)) => {
                lines.push(Line::from(Span::styled(format!("✗ {e}"), Style::new().fg(ERR_C))));
                lines.push(Line::default());
                lines.push(Line::from(Span::styled("Enter  close", Style::new().fg(DIM))));
            }
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_schedule(f: &mut Frame, area: Rect, app: &App, s: &Schedule, field: usize, installed: bool) {
    let fields = schedule_fields(s);
    let h = 15 + fields.len() as u16;
    let inner = modal_block(f, area, 78, h, "Automatic backup — Windows Task Scheduler", ACCENT);
    let pause = app.config.schedule.as_ref().and_then(|sc| sc.pause_text(jobs::now_unix()));
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("Disk         ", Style::new().fg(DIM)),
        Span::styled(format!("{} · {}", s.disk_name, human(s.disk_size)), Style::new().fg(TEXT).bold()),
        Span::styled(
            if s.disk_serial.is_empty() { String::new() } else { format!("   serial {}", s.disk_serial) },
            Style::new().fg(DIM),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Destination  ", Style::new().fg(DIM)),
        Span::styled(s.dest_dir.clone(), Style::new().fg(TEXT)),
        Span::styled("   (change with n)", Style::new().fg(DIM)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Task         ", Style::new().fg(DIM)),
        if installed {
            Span::styled("registered", Style::new().fg(OK_C))
        } else {
            Span::styled("not registered yet", Style::new().fg(WARN_C))
        },
        Span::styled(
            "   runs elevated while you are logged on, even if this app is closed",
            Style::new().fg(DIM),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Status       ", Style::new().fg(DIM)),
        match &pause {
            Some(t) => Span::styled(format!("⏸ {t}"), Style::new().fg(WARN_C).bold()),
            None if installed => Span::styled("active", Style::new().fg(OK_C)),
            None => Span::styled("—", Style::new().fg(DIM)),
        },
        Span::styled("   (p to pause or resume)", Style::new().fg(DIM)),
    ]));
    lines.push(Line::default());
    for (i, fld) in fields.iter().enumerate() {
        let selected = i == field;
        let marker = if selected { "▸ " } else { "  " };
        let style = if selected {
            Style::new().fg(ACCENT_SOFT).bg(SEL_BG).bold()
        } else {
            Style::new().fg(TEXT)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}{:<14}", fld.label()), style),
            Span::styled(if selected { "◂ " } else { "  " }, Style::new().fg(ACCENT_SOFT)),
            Span::styled(format!("{:<22}", schedule_value(s, *fld)), style),
            Span::styled(if selected { " ▸" } else { "  " }, Style::new().fg(ACCENT_SOFT)),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("Runs ", Style::new().fg(DIM)),
        Span::styled(s.describe(), Style::new().fg(TEXT).bold()),
        Span::styled(
            "  — images are named auto-<disk>-<serial>-<date>.img; older ones are deleted past the keep count.",
            Style::new().fg(DIM),
        ),
    ]));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "↑↓ row · ←→ change · Enter save & register · n destination · p pause · x remove schedule · Esc cancel",
        Style::new().fg(DIM),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_manage_menu(f: &mut Frame, area: Rect, app: &App, idx: usize) {
    let items = app.manage_items();
    let disk = app.selected_disk();
    let h = 9 + items.len() as u16;
    let title = match disk {
        Some(d) => format!("Manage — disk {} · {}", d.number, d.name),
        None => "Manage".into(),
    };
    let inner = modal_block(f, area, 72, h, &title, ACCENT);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(d) = disk {
        lines.push(Line::from(vec![
            Span::styled("State  ", Style::new().fg(DIM)),
            Span::styled(
                if d.offline { "offline" } else { "online" },
                Style::new().fg(if d.offline { WARN_C } else { OK_C }),
            ),
            Span::styled(
                format!(
                    "  ·  {}  ·  {} partition(s){}",
                    d.style,
                    d.partitions.len(),
                    if d.readonly { "  ·  READ-ONLY" } else { "" }
                ),
                Style::new().fg(DIM),
            ),
        ]));
        lines.push(Line::default());
    }
    for (i, it) in items.iter().enumerate() {
        let selected = i == idx;
        let marker = if selected { "▸ " } else { "  " };
        let style = if selected {
            Style::new().fg(ACCENT_SOFT).bg(SEL_BG).bold()
        } else {
            Style::new().fg(TEXT)
        };
        lines.push(Line::from(Span::styled(format!("{marker}{}", it.label()), style)));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "These change how Windows mounts the disk; no data is erased.   Enter choose · Esc cancel",
        Style::new().fg(DIM),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// The scheduled-backup status bar appears above the footer with the
    /// percentage, disk, image, detail and the Shift+B hint.
    #[test]
    fn sched_status_bar_renders() {
        utility_core::register(&crate::APP);
        let mut app = App::new();
        app.jobs = vec![jobs::Job {
            pid: 1,
            kind: jobs::Kind::Scheduled,
            disk: "disk 2 (Samsung T7, 1.0 TB)".into(),
            image: r"Z:\backups\auto.img".into(),
            frac: 0.37,
            detail: "backup 410 MB/s".into(),
            started: 0,
            updated: 0,
        }];
        let mut term = Terminal::new(TestBackend::new(140, 24)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let row = |y: u16| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>();
        let bar = row(22);
        assert!(bar.contains("⏱"), "{bar}");
        assert!(bar.contains("37%"), "{bar}");
        assert!(bar.contains("Samsung T7"), "{bar}");
        assert!(bar.contains("auto.img"), "{bar}");
        assert!(bar.contains("scheduled backup"), "{bar}");
        assert!(bar.contains("Shift+B"), "{bar}");
        assert!(row(23).contains("select"), "footer: {}", row(23));

        // the Backups panel lists the job, the stop hint and the schedule section
        app.modal = Modal::Backups { idx: 0 };
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let all: String = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n")
            .collect();
        assert!(all.contains("Running now"), "{all}");
        assert!(all.contains("pid 1"), "{all}");
        assert!(all.contains("x stop it"), "{all}");
        assert!(all.contains("Schedule"), "{all}");
        // pause menu lists the options and the current state
        app.config.schedule = Some(crate::config::Schedule {
            disk_name: "T7".into(),
            paused_until: Some(crate::config::PAUSED_INDEFINITELY),
            ..Default::default()
        });
        app.modal = Modal::PauseMenu { idx: 0, from_editor: false };
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let all: String = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "
")
            .collect();
        assert!(all.contains("Resume scheduled backups now"), "{all}");
        assert!(all.contains("Pause for 7 days"), "{all}");
        assert!(all.contains("paused until you resume it"), "{all}");
        app.modal = Modal::None;

        app.jobs.clear();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let last = (0..buf.area.width).map(|x| buf[(x, 23)].symbol().to_string()).collect::<String>();
        assert!(last.contains("select") && !last.contains("backup panel"));
    }
}
