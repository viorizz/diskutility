#[cfg(not(windows))]
compile_error!("diskutility targets Windows (uses PowerShell storage cmdlets and \\\\.\\PhysicalDrive access)");

mod app;
mod bench;
mod config;
mod disks;
mod jobs;
mod ops;
mod schedule;
mod ui;

// Shared with the other *Utility tools.
pub use utility_core::{logger, notify};

use utility_core::{cli, AppInfo};

pub static APP: AppInfo = AppInfo {
    name: "diskutility",
    display_name: "Disk Utility",
    repo: "viorizz/diskutility",
    tagline: "format · erase · write images",
    version: env!("CARGO_PKG_VERSION"),
    build_epoch: env!("BUILD_EPOCH"),
};

fn main() -> anyhow::Result<()> {
    // `--version`, `--update` and the update-check opt-out.
    if let Some(code) = cli::handle_common_args(&APP) {
        std::process::exit(code);
    }
    utility_core::init(&APP);

    // `diskutility --scheduled-backup` — headless run launched by the
    // Task Scheduler job registered with the `a` key.
    if std::env::args().any(|a| a == schedule::CLI_FLAG) {
        let notify = config::load().core.notify;
        return match schedule::run_headless() {
            Ok(schedule::Outcome::Skipped(msg)) => {
                println!("{msg}");
                Ok(())
            }
            Ok(schedule::Outcome::Done(msg)) => {
                println!("{msg}");
                if notify {
                    notify::toast("Scheduled backup finished", &msg);
                }
                Ok(())
            }
            Err(e) => {
                if notify {
                    let title = if e.starts_with("cancelled") { "Scheduled backup stopped" } else { "Scheduled backup FAILED" };
                    notify::toast(title, &e);
                }
                anyhow::bail!("scheduled backup failed: {e}")
            }
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

    // `diskutility --hex <disk> [sector]` — dump one 512-byte sector and exit.
    if let Some(i) = args.iter().position(|a| a == "--hex") {
        let n: u32 = args
            .get(i + 1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("usage: diskutility --hex <disk number> [sector]"))?;
        let lba: u64 = args.get(i + 2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let data = ops::read_raw(n, lba * 512 / 4096 * 4096, 4096).map_err(|e| anyhow::anyhow!(e))?;
        let start = (lba * 512 % 4096) as usize;
        let sector = data.get(start..start + 512).ok_or_else(|| anyhow::anyhow!("short read"))?;
        println!("disk {n} sector {lba} (byte offset {})", lba * 512);
        for (r, chunk) in sector.chunks(16).enumerate() {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            let ascii: String = chunk.iter().map(|b| if b.is_ascii_graphic() || *b == b' ' { *b as char } else { '.' }).collect();
            println!("{:04x}  {}  {}", r * 16, hex.join(" "), ascii);
        }
        return Ok(());
    }

    // `diskutility --snapshot` — render one frame after the disk scan and exit.
    if std::env::args().any(|a| a == "--snapshot") {
        let mut app = app::App::new();
        print!("{}", utility_core::shell::snapshot(&mut app, 120, 36, std::time::Duration::from_secs(15))?);
        return Ok(());
    }

    // Non-interactive mode for quick checks / scripting: `diskutility --list`
    if std::env::args().any(|a| a == "--list" || a == "-l") {
        return list_disks();
    }

    // Opt-in automatic update on launch (Shift+U → a in the TUI). Network
    // access still honours --no-update-check / DISKUTILITY_NO_UPDATE_CHECK.
    cli::auto_update_on_launch(&APP, config::load().core.auto_update);

    let mut app = app::App::new();
    std::process::exit(utility_core::shell::launch(&mut app)?);
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
