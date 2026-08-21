#[cfg(not(windows))]
compile_error!("diskutility targets Windows (uses PowerShell storage cmdlets and \\\\.\\PhysicalDrive access)");

mod app;
mod bench;
mod disks;
mod logger;
mod ops;
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
        match update::self_update() {
            Ok(msg) => {
                println!("{msg}");
                return Ok(());
            }
            Err(e) => anyhow::bail!("update failed: {e}"),
        }
    }

    // Non-interactive mode for quick checks / scripting: `diskutility --list`
    if std::env::args().any(|a| a == "--list" || a == "-l") {
        return list_disks();
    }

    let mut terminal = ratatui::init();
    let _ = crossterm_set_title();
    let result = app::App::new().run(&mut terminal);
    ratatui::restore();
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
