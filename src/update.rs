use crate::logger;
use crate::ops;

pub const REPO: &str = "viorizz/diskutility";

fn parse_ver(s: &str) -> Option<[u64; 3]> {
    let s = s.trim().trim_start_matches('v');
    let mut parts = s.split('.');
    Some([
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ])
}

/// Query the latest GitHub release. Returns Some((tag, download_url)) only if
/// it is strictly newer than the running version.
pub fn check_latest() -> Result<Option<(String, String)>, String> {
    let script = format!(
        "$r = Invoke-RestMethod -Uri 'https://api.github.com/repos/{REPO}/releases/latest' -Headers @{{ 'User-Agent' = 'diskutility' }} -TimeoutSec 15\n\
         $a = $r.assets | Where-Object {{ $_.name -eq 'diskutility.exe' }} | Select-Object -First 1\n\
         Write-Output ($r.tag_name + '|' + $a.browser_download_url)"
    );
    let out = ops::run_ps_quiet(&script)?;
    let line = out.trim();
    let (tag, url) = line.split_once('|').ok_or("unexpected update-check output")?;
    let (tag, url) = (tag.trim().to_string(), url.trim().to_string());
    if url.is_empty() {
        return Ok(None);
    }
    let current = parse_ver(env!("CARGO_PKG_VERSION")).unwrap_or([0, 0, 0]);
    match parse_ver(&tag) {
        Some(latest) if latest > current => Ok(Some((tag, url))),
        _ => Ok(None),
    }
}

/// Download the latest release and swap it in over the running executable
/// (Windows allows renaming a running exe, just not deleting it — the old
/// version is parked as *.old and removed on next start).
pub fn self_update() -> Result<String, String> {
    let current = env!("CARGO_PKG_VERSION");
    logger::log("update: checking latest release");
    let Some((tag, url)) = check_latest()? else {
        return Ok(format!("diskutility v{current} is already the latest version."));
    };
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate own exe: {e}"))?;
    let staged = exe.with_extension("exe.new");
    let backup = exe.with_extension("exe.old");
    logger::log(format!("update: downloading {tag} from {url}"));
    ops::run_ps(&format!(
        "Invoke-WebRequest -Uri '{url}' -OutFile '{}' -UseBasicParsing",
        staged.display()
    ))?;
    let size = std::fs::metadata(&staged)
        .map_err(|e| format!("download missing: {e}"))?
        .len();
    if size < 200_000 {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("downloaded file is suspiciously small ({size} bytes) — aborting"));
    }
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(&exe, &backup).map_err(|e| format!("cannot move current exe aside: {e}"))?;
    if let Err(e) = std::fs::rename(&staged, &exe) {
        let _ = std::fs::rename(&backup, &exe);
        return Err(format!("could not install update (rolled back): {e}"));
    }
    logger::log(format!("update: installed {tag}"));
    Ok(format!(
        "Updated v{current} → {tag}. Restart diskutility to run the new version."
    ))
}

/// Remove the parked previous version left behind by a self-update.
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    }
}
