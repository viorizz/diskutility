use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct Logger {
    file: Option<File>,
    path: Option<PathBuf>,
    lines: Vec<String>,
}

static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();

fn with<R>(f: impl FnOnce(&mut Logger) -> R) -> R {
    let m = LOGGER.get_or_init(|| {
        let path = pick_path();
        let file = path
            .as_ref()
            .and_then(|p| OpenOptions::new().create(true).append(true).open(p).ok());
        Mutex::new(Logger { file, path, lines: Vec::new() })
    });
    let mut guard = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// Keep the append-only log bounded: once it passes 5 MiB, park it as
/// `diskutility.log.1` (replacing any previous backup) and start fresh.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

fn rotate(p: &PathBuf) {
    if std::fs::metadata(p).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let backup = p.with_extension("log.1");
        let _ = std::fs::remove_file(&backup);
        let _ = std::fs::rename(p, &backup);
    }
}

fn pick_path() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("diskutility.log");
            rotate(&p);
            if OpenOptions::new().create(true).append(true).open(&p).is_ok() {
                return Some(p);
            }
        }
    }
    let p = std::env::temp_dir().join("diskutility.log");
    rotate(&p);
    Some(p)
}

pub fn init(version: &str, elevated: bool) {
    log("═══════════════════════════════════════════════════════");
    log(format!(
        "session start · diskutility v{version} · {} · elevated={elevated} · {}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        std::env::consts::OS
    ));
    if let Some(p) = path() {
        log(format!("log file: {}", p.display()));
    }
}

pub fn path() -> Option<PathBuf> {
    with(|l| l.path.clone())
}

/// Append a (possibly multi-line) message; every line is timestamped,
/// continuation lines are marked with `|` so blocks stay readable.
pub fn log(msg: impl AsRef<str>) {
    let ts = chrono::Local::now().format("%H:%M:%S%.3f").to_string();
    with(|l| {
        for (i, raw) in msg.as_ref().split('\n').enumerate() {
            let line = raw.trim_end_matches('\r');
            let text = if i == 0 {
                format!("[{ts}] {line}")
            } else {
                format!("[{ts}]   | {line}")
            };
            if let Some(f) = &mut l.file {
                let _ = writeln!(f, "{text}");
            }
            l.lines.push(text);
        }
        if let Some(f) = &mut l.file {
            let _ = f.flush();
        }
    });
}

pub fn log_block(title: &str, body: &str) {
    log(format!("{title}:\n{}", body.trim_end()));
}

/// Copy the full session log to the Windows clipboard.
/// Goes through a temp file + Set-Clipboard so unicode survives
/// (clip.exe would mangle non-ANSI characters).
pub fn copy_to_clipboard() -> Result<usize, String> {
    let text = with(|l| l.lines.join("\r\n"));
    if text.is_empty() {
        return Err("log is empty".into());
    }
    let count = text.lines().count();
    let tmp = std::env::temp_dir().join("diskutility-clip.txt");
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| format!("temp file write failed: {e}"))?;
    let cmd = format!(
        "Get-Content -Raw -Encoding UTF8 '{}' | Set-Clipboard",
        crate::ops::ps_quote(&tmp.display().to_string())
    );
    let out = Command::new(crate::ops::powershell_exe())
        .args(["-NoProfile", "-NonInteractive", "-Command", &cmd])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("failed to launch powershell: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    if out.status.success() {
        Ok(count)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("Set-Clipboard failed").to_string())
    }
}
