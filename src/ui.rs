use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph, Sparkline, Wrap};
use ratatui::Frame;

use crate::app::{App, InputPurpose, Modal, PendingAction, ProgressState};
use crate::disks::{fit, human, Disk};
use crate::ops::PRESETS;

const ACCENT: Color = Color::Rgb(222, 119, 87);
const ACCENT_SOFT: Color = Color::Rgb(245, 173, 130);
const BORDER: Color = Color::Rgb(82, 82, 96);
const DIM: Color = Color::Rgb(132, 134, 148);
const TEXT: Color = Color::Rgb(226, 224, 218);
const OK_C: Color = Color::Rgb(139, 202, 128);
const ERR_C: Color = Color::Rgb(238, 105, 105);
const WARN_C: Color = Color::Rgb(238, 189, 94);
const SEL_BG: Color = Color::Rgb(58, 42, 36);

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(f, rows[0], app);

    let cols = Layout::horizontal([Constraint::Length(46), Constraint::Min(30)]).split(rows[1]);
    draw_disk_list(f, cols[0], app);
    draw_details(f, cols[1], app);
    draw_footer(f, rows[2], app);

    match &app.modal {
        Modal::None => {}
        Modal::Help => draw_help(f, area),
        Modal::Unlock { buf } => draw_unlock(f, area, buf),
        Modal::Presets { idx } => draw_presets(f, area, *idx),
        Modal::EraseMenu { idx } => draw_erase_menu(f, area, *idx),
        Modal::TestMenu { idx } => draw_test_menu(f, area, *idx),
        Modal::Input { purpose, buf } => draw_input(f, area, purpose, buf),
        Modal::Confirm { action, buf } => draw_confirm(f, area, app, action, buf),
        Modal::Progress(p) => draw_progress(f, area, app, p),
    }
}

fn bordered(title: Line<'static>) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
        .title(title)
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let border = if app.unlocked { ERR_C } else { ACCENT };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let left = Line::from(vec![
        Span::styled(" ✦ ", Style::new().fg(ACCENT).bold()),
        Span::styled("Disk Utility", Style::new().fg(TEXT).bold()),
        Span::styled(
            format!(
                "  v{} · build {}",
                env!("CARGO_PKG_VERSION"),
                crate::build_stamp()
            ),
            Style::new().fg(DIM),
        ),
        Span::styled("   format · erase · write images", Style::new().fg(DIM).italic()),
    ]);
    f.render_widget(Paragraph::new(left), inner);

    let mut spans: Vec<Span> = Vec::new();
    if let Some(tag) = &app.update {
        spans.push(Span::styled(
            format!("⬆ {tag} available — diskutility --update"),
            Style::new().fg(ACCENT_SOFT),
        ));
        spans.push(Span::styled(" · ", Style::new().fg(DIM)));
    }
    if app.unlocked {
        spans.push(Span::styled("⛨ PROTECTIONS OFF", Style::new().fg(ERR_C).bold()));
        spans.push(Span::styled(" · ", Style::new().fg(DIM)));
    }
    if app.elevated {
        spans.push(Span::styled("● administrator ", Style::new().fg(OK_C)));
    } else {
        spans.push(Span::styled("⚠ not elevated — read-only ", Style::new().fg(WARN_C)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Right),
        inner,
    );
}

fn draw_disk_list(f: &mut Frame, area: Rect, app: &App) {
    let title = if app.scanning {
        Line::from(vec![
            Span::styled(" Disks ", Style::new().fg(TEXT).bold()),
            Span::styled(
                format!("{} scanning… ", SPINNER[app.tick % SPINNER.len()]),
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

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    if let Some((msg, is_err, _)) = &app.status {
        let style = if *is_err {
            Style::new().fg(ERR_C).bold()
        } else {
            Style::new().fg(OK_C)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {msg}"), style))),
            area,
        );
        return;
    }
    let key = |k: &'static str| Span::styled(k, Style::new().fg(ACCENT_SOFT).bold());
    let txt = |t: &'static str| Span::styled(t, Style::new().fg(DIM));
    let line = Line::from(vec![
        txt(" "),
        key("↑↓"),
        txt(" select · "),
        key("f"),
        txt(" format · "),
        key("e"),
        txt(" erase · "),
        key("i"),
        txt(" write image · "),
        key("b"),
        txt(" test · "),
        key("r"),
        txt(" rescan · "),
        key("c"),
        txt(" copy log · "),
        key("u"),
        txt(if app.unlocked { " re-lock · " } else { " override · " }),
        key("?"),
        txt(" help · "),
        key("q"),
        txt(" quit"),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

// ---------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------

fn modal_rect(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn modal_block(f: &mut Frame, area: Rect, w: u16, h: u16, title: &str, color: Color) -> Rect {
    let rect = modal_rect(area, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::new().fg(color).bold(),
        )))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    inner
}

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
    let inner = modal_block(f, area, 72, 14, "Test disk", ACCENT);
    let opts: [(&str, &str); 4] = [
        (
            "Read benchmark  (safe)",
            "Sequential + random 4K read speed with a live graph. Non-destructive — safe on any disk, even the system disk. Starts immediately.",
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

fn draw_input(f: &mut Frame, area: Rect, purpose: &InputPurpose, buf: &str) {
    let (title, hint) = match purpose {
        InputPurpose::Label(p) => (
            format!("Volume label — {} ({})", p.name(), p.fs_display()),
            "letters, numbers, space, - _ .   ·   Enter continue · Esc cancel",
        ),
        InputPurpose::IsoPath => (
            "Path to disk image (.iso / .img)".to_string(),
            "paste or type the full path   ·   Enter continue · Esc cancel",
        ),
    };
    let inner = modal_block(f, area, 68, 7, &title, ACCENT);
    let lines = vec![
        Line::from(vec![
            Span::styled("❯ ", Style::new().fg(ACCENT).bold()),
            Span::styled(buf.to_string(), Style::new().fg(TEXT)),
            Span::styled("▌", Style::new().fg(ACCENT_SOFT)),
        ]),
        Line::default(),
        Line::from(Span::styled(hint, Style::new().fg(DIM))),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_confirm(f: &mut Frame, area: Rect, app: &App, action: &PendingAction, buf: &str) {
    let disk: Option<&Disk> = app.disks.get(app.selected);
    let reasons: Vec<&str> = disk.map(|d| app.protection_reasons(d)).unwrap_or_default();
    let protected = !reasons.is_empty();
    let height = if protected { 16 } else { 13 };
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
        Line::default(),
    ];
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
                        format!("{} ", SPINNER[app.tick % SPINNER.len()]),
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

fn draw_help(f: &mut Frame, area: Rect) {
    let inner = modal_block(f, area, 74, 21, "Help", ACCENT);
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
        key("b", "benchmark & capacity tests (read-only or destructive)"),
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
