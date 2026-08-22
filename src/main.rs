#[cfg(not(windows))]
compile_error!("diskutility targets Windows (uses PowerShell storage cmdlets and \\\\.\\PhysicalDrive access)");

mod app;
mod bench;
mod config;
mod disks;
mod logger;
mod ops;
mod schedule;
mod ui;
mod update;

/// Human-readable compile timestamp, baked in by build.rs.
pub fn build_stamp() -> String {
    env!("BUILD_EPOCH")
        .parse::<i64>()
        .ok()
        .and_then(|e| chrono::DateTime::from_timestamp(e, 0))
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("diskutility v{} (built {})", env!("CARGO_PKG_VERSION"), build_stamp());
        return Ok(());
    }

    logger::init(
        &format!("{} · built {}", env!("CARGO_PKG_VERSION"), build_stamp()),
        ops::is_elevated(),
    );
    update::cleanup();

    if std::env::args().any(|a| a == "--update") {
        match update::self_update_with(&|step| println!("{step}")) {
            Ok(msg) => {
                println!("{msg}");
                return Ok(());
            }
            Err(e) => anyhow::bail!("update failed: {e}"),
        }
    }

    // `diskutility --scheduled-backup` — headless run launched by the
    // Task Scheduler job registered with the `a` key.
    if std::env::args().any(|a| a == schedule::CLI_FLAG) {
        return match schedule::run_headless() {
            Ok(msg) => {
                println!("{msg}");
                Ok(())
            }
            Err(e) => anyhow::bail!("scheduled backup failed: {e}"),
        };
    }

    // `diskutility --health <disk number>` — print SMART/health data and exit
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--health") {
        let n: u32 = args
            .get(i + 1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("usage: diskutility --health <disk number>"))?;
        return match ops::query_health(n) {
            Ok(h) => {
                println!("disk {n}: health={} media={} spindle={} usage={}", h.health, h.media, h.spindle, h.usage);
                if h.rc_ok {
                    println!("  wear={}% temp={}C (max {}C) power-on={}h read-errors={} write-errors={}",
                        h.wear, h.temp, h.temp_max, h.hours, h.read_err, h.write_err);
                } else if !ops::is_elevated() {
                    println!("  detailed SMART counters require an elevated (administrator) terminal");
                } else {
                    println!("  detailed SMART counters unavailable (USB bridge or unsupported device)");
                }
                Ok(())
            }
            Err(e) => anyhow::bail!("health query failed: {e}"),
        };
    }

    // Non-interactive mode for quick checks / scripting: `diskutility --list`
    if std::env::args().any(|a| a == "--list" || a == "-l") {
        return list_disks();
    }

    // Opt-in automatic update on launch (Shift+U → a in the TUI). Network
    // access still honours --no-update-check / DISKUTILITY_NO_UPDATE_CHECK.
    if config::load().auto_update && !app::update_check_opted_out() {
        println!("diskutility: auto-update is on — checking for a new release…");
        match update::self_update_with(&|step| println!("  {step}")) {
            Ok(msg) if update::updated(&msg) => {
                println!("  {msg}");
                println!("  restarting…");
                std::process::exit(update::relaunch().map_err(|e| anyhow::anyhow!(e))?);
            }
            Ok(msg) => println!("  {msg}"),
            Err(e) => println!("  update skipped: {e}"),
        }
    }

    let mut terminal = ratatui::init();
    let _ = crossterm_set_title();
    let mut app = app::App::new();
    let result = app.run(&mut terminal);
    ratatui::restore();
    if app.restart_requested() {
        println!("diskutility: restarting into the new version…");
        std::process::exit(update::relaunch().map_err(|e| anyhow::anyhow!(e))?);
    }
    result
}

fn crossterm_set_title() -> std::io::Result<()> {
    use ratatui::crossterm::{execute, terminal::SetTitle};
    execute!(std::io::stdout(), SetTitle("Disk Utility"))
}

fn list_disks() -> anyhow::Result<()> {
    match disks::enumerate() {
        Ok(list) => {
            println!("{:<4} {:<34} {:>10} {:<6} {:<5} FLAGS", "#", "NAME", "SIZE", "BUS", "STYLE");
            for d in &list {
                let mut flags = Vec::new();
                if d.system { flags.push("SYSTEM"); }
                if d.boot { flags.push("BOOT"); }
                if d.readonly { flags.push("RO"); }
                if d.offline { flags.push("OFFLINE"); }
                println!(
                    "{:<4} {:<34} {:>10} {:<6} {:<5} {}",
                    d.number,
                    disks::fit(&d.name, 34),
                    disks::human(d.size),
                    d.bus,
                    d.style,
                    flags.join(",")
                );
                for p in &d.partitions {
                    let letter = if p.letter.is_empty() { "-".into() } else { format!("{}:", p.letter) };
                    println!(
                        "       └ part {}  {:<3} {:<7} {:<14} {:>10}", p.number, letter, p.fs, disks::fit(&p.label, 14), disks::human(p.size)
                    );
                }
            }
            Ok(())
        }
        Err(e) => anyhow::bail!("failed to enumerate disks: {e}"),
    }
}
