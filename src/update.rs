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

/// Only ever install binaries published under this repository's releases.
fn allowed_url(url: &str) -> bool {
    url.starts_with(&format!("https://github.com/{REPO}/releases/download/"))
        && !url.contains('\'')
        && !url.contains(char::is_whitespace)
}

/// Download the latest release, verify it against the release's
/// `checksums.txt`, sanity-check it, then swap it in over the running
/// executable (Windows allows renaming a running exe, just not deleting it —
/// the old version is parked as *.old and removed on next start).
/// Outcome of `self_update_with`: whether a new binary was actually installed.
pub fn updated(msg: &str) -> bool {
    msg.starts_with("Updated ")
}

/// Same as `self_update`, reporting each step through `progress` (used by
/// the Shift+U dialog in the TUI and the launch-time auto-update).
pub fn self_update_with(progress: &dyn Fn(&str)) -> Result<String, String> {
    let current = env!("CARGO_PKG_VERSION");
    logger::log("update: checking latest release");
    progress("Checking the latest release on GitHub…");
    let Some((tag, url)) = check_latest()? else {
        return Ok(format!("diskutility v{current} is already the latest version."));
    };
    progress(&format!("{tag} is available — downloading…"));
    if !allowed_url(&url) {
        return Err(format!("refusing to download from unexpected location: {url}"));
    }
    let Some(base) = url.rsplit_once('/').map(|(b, _)| b) else {
        return Err("malformed download url".into());
    };
    let sums_url = format!("{base}/checksums.txt");
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate own exe: {e}"))?;
    let staged = exe.with_extension("exe.new");
    let backup = exe.with_extension("exe.old");
    logger::log(format!("update: downloading {tag} from {url}"));
    let out = ops::run_ps(&format!(
        "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
         Invoke-WebRequest -Uri '{url}' -OutFile '{staged}' -UseBasicParsing
         $sums = (Invoke-WebRequest -Uri '{sums}' -UseBasicParsing).Content
         if ($sums -is [byte[]]) {{ $sums = [Text.Encoding]::UTF8.GetString($sums) }}
         $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath '{staged}').Hash.ToLower()
         Write-Output \"HASH:$hash\"
         Write-Output 'SUMS:'
         Write-Output $sums",
        url = ops::ps_quote(&url),
        sums = ops::ps_quote(&sums_url),
        staged = ops::ps_quote(&staged.display().to_string()),
    ));
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            let _ = std::fs::remove_file(&staged);
            return Err(e);
        }
    };
    progress("Verifying SHA-256 against checksums.txt…");
    if let Err(e) = verify_download(&out, &staged) {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }
    progress("Swapping the new executable in…");
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(&exe, &backup).map_err(|e| format!("cannot move current exe aside: {e}"))?;
    if let Err(e) = std::fs::rename(&staged, &exe) {
        let _ = std::fs::rename(&backup, &exe);
        return Err(format!("could not install update (rolled back): {e}"));
    }
    logger::log(format!("update: installed {tag}"));
    Ok(format!(
        "Updated v{current} → {tag} (SHA-256 verified). Restart diskutility to run the new version."
    ))
}

/// Check the staged download: expected hash listed in checksums.txt, actual
/// hash matches, it is a PE image, and it identifies itself as diskutility.
fn verify_download(ps_out: &str, staged: &std::path::Path) -> Result<(), String> {
    let actual = ps_out
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("HASH:"))
        .ok_or("download did not produce a hash")?
        .to_ascii_lowercase();
    let sums_start = ps_out.find("SUMS:").ok_or("checksums.txt was not fetched")?;
    let expected = ps_out[sums_start + 5..]
        .lines()
        .map(str::trim)
        .filter_map(|l| {
            let (h, name) = l.split_once(char::is_whitespace)?;
            (name.trim().trim_start_matches('*') == "diskutility.exe").then(|| h.to_ascii_lowercase())
        })
        .next()
        .ok_or("checksums.txt has no entry for diskutility.exe — refusing to install an unverified binary")?;
    if expected.len() != 64 || actual.len() != 64 {
        return Err("malformed SHA-256 in checksums.txt".into());
    }
    if expected != actual {
        logger::log(format!("update: CHECKSUM MISMATCH expected {expected} got {actual}"));
        return Err("SHA-256 of the downloaded file does not match checksums.txt — download corrupted or tampered; aborting".into());
    }
    logger::log(format!("update: sha256 verified {actual}"));
    let size = std::fs::metadata(staged)
        .map_err(|e| format!("download missing: {e}"))?
        .len();
    if size < 200_000 {
        return Err(format!("downloaded file is suspiciously small ({size} bytes) — aborting"));
    }
    let mut head = [0u8; 2];
    std::fs::File::open(staged)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head))
        .map_err(|e| format!("cannot read download: {e}"))?;
    if &head != b"MZ" {
        return Err("downloaded file is not a Windows executable — aborting".into());
    }
    let probe = std::process::Command::new(staged)
        .arg("--version")
        .output()
        .map_err(|e| format!("new binary failed to start: {e}"))?;
    let banner = String::from_utf8_lossy(&probe.stdout);
    if !probe.status.success() || !banner.starts_with("diskutility v") {
        return Err(format!("new binary did not identify itself as diskutility ({})", banner.trim()));
    }
    logger::log(format!("update: new binary reports '{}'", banner.trim()));
    Ok(())
}

/// Run the (freshly installed) executable in this console with the same
/// arguments and wait for it — used to restart into a new version. Returns
/// the child's exit code.
pub fn relaunch() -> Result<i32, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate own exe: {e}"))?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    logger::log(format!("update: relaunching {} {}", exe.display(), args.join(" ")));
    let status = std::process::Command::new(&exe)
        .args(&args)
        .status()
        .map_err(|e| format!("could not start the new version: {e}"))?;
    Ok(status.code().unwrap_or(0))
}

/// Remove the parked previous version left behind by a self-update.
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_release_assets_from_our_repo_are_allowed() {
        assert!(allowed_url("https://github.com/viorizz/diskutility/releases/download/v1.0.0/diskutility.exe"));
        assert!(!allowed_url("https://github.com/evil/diskutility/releases/download/v1.0.0/diskutility.exe"));
        assert!(!allowed_url("http://github.com/viorizz/diskutility/releases/download/v1.0.0/diskutility.exe"));
        assert!(!allowed_url("https://github.com/viorizz/diskutility/releases/download/v1'; Remove-Item x; '/a.exe"));
    }

    #[test]
    fn checksum_mismatch_is_rejected_before_touching_the_file() {
        let out = format!("HASH:{}\nSUMS:\n{}  diskutility.exe\n", "a".repeat(64), "b".repeat(64));
        let err = verify_download(&out, std::path::Path::new("does-not-exist.exe")).unwrap_err();
        assert!(err.contains("does not match"), "{err}");
    }

    #[test]
    fn missing_checksum_entry_is_rejected() {
        let out = format!("HASH:{}\nSUMS:\n{}  other.exe\n", "a".repeat(64), "a".repeat(64));
        let err = verify_download(&out, std::path::Path::new("does-not-exist.exe")).unwrap_err();
        assert!(err.contains("no entry"), "{err}");
    }

    #[test]
    fn matching_checksum_proceeds_to_file_checks() {
        let out = format!("HASH:{}\nSUMS:\n{}  diskutility.exe\n", "a".repeat(64), "A".repeat(64));
        let err = verify_download(&out, std::path::Path::new("does-not-exist.exe")).unwrap_err();
        assert!(err.contains("download missing"), "{err}");
    }
}
