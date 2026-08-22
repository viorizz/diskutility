use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::app::AppEvent;
use crate::disks::{human, Disk};
use crate::logger;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CHUNK: usize = 4 * 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;

pub enum OpEvent {
    Log(String),
    Progress(f64, String),
    Sample(u64),
    Done(Result<String, String>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preset {
    Windows,
    MacOs,
    Linux,
    Ps5,
    Universal,
}

pub const PRESETS: [Preset; 5] = [
    Preset::Windows,
    Preset::MacOs,
    Preset::Linux,
    Preset::Ps5,
    Preset::Universal,
];

impl Preset {
    pub fn name(self) -> &'static str {
        match self {
            Preset::Windows => "Windows",
            Preset::MacOs => "macOS",
            Preset::Linux => "Linux",
            Preset::Ps5 => "PlayStation 5",
            Preset::Universal => "Universal",
        }
    }

    pub fn fs_display(self) -> &'static str {
        match self {
            Preset::Windows => "NTFS · GPT",
            Preset::MacOs => "exFAT · GPT",
            Preset::Linux => "ext4 · GPT (via WSL 2)",
            Preset::Ps5 => "exFAT · MBR",
            Preset::Universal => "FAT32 / exFAT · MBR",
        }
    }

    pub fn desc(self) -> &'static str {
        match self {
            Preset::Windows => "NTFS on a GPT disk. Best for Windows-only drives: journaling, permissions, files over 4 GB, no practical size limits.",
            Preset::MacOs => "exFAT on GPT — full read/write on macOS and Windows. Native APFS/HFS+ can only be created by macOS itself, so exFAT is the standard choice when preparing a drive for a Mac from Windows.",
            Preset::Linux => "ext4 on GPT, created through WSL 2 (must be installed: `wsl --install`). If WSL is unavailable this fails cleanly — exFAT (macOS/Universal preset) is also readable on Linux.",
            Preset::Ps5 => "exFAT on MBR — what the PS5 accepts for USB media drives and game backups. Note: for USB extended storage the PS5 insists on formatting the drive itself in Settings > Storage.",
            Preset::Universal => "Maximum compatibility: FAT32 for disks up to 32 GB, exFAT above that (FAT32 can't be created larger on Windows). Works in TVs, cameras, car stereos, printers, consoles.",
        }
    }

    pub fn scheme(self) -> &'static str {
        match self {
            Preset::Windows | Preset::MacOs | Preset::Linux => "GPT",
            Preset::Ps5 | Preset::Universal => "MBR",
        }
    }

    pub fn label_limit(self) -> usize {
        match self {
            Preset::Windows => 32,
            Preset::Linux => 16,
            _ => 11,
        }
    }
}

// ---------------------------------------------------------------------------
// PowerShell plumbing
// ---------------------------------------------------------------------------

pub fn run_ps(script: &str) -> Result<String, String> {
    run_ps_impl(script, true)
}

/// Same as run_ps but only writes to the log when the command fails —
/// used for the frequent disk rescans so the log stays readable.
pub fn run_ps_quiet(script: &str) -> Result<String, String> {
    run_ps_impl(script, false)
}

/// Prepended to every script: silence progress records (they pollute stderr
/// as CLIXML), force UTF-8 output so unicode survives the pipe, and pin the
/// module search path to system-owned directories so a module dropped in the
/// user's Documents folder can't shadow the Storage cmdlets while we run
/// elevated.
const PS_PRELUDE: &str = "$ProgressPreference='SilentlyContinue'\n\
    try { [Console]::OutputEncoding = [Text.Encoding]::UTF8 } catch {}\n\
    $env:PSModulePath = \"$env:SystemRoot\\System32\\WindowsPowerShell\\v1.0\\Modules;$env:ProgramFiles\\WindowsPowerShell\\Modules\"\n";

/// Absolute path of Windows PowerShell. `Command::new("powershell")` would
/// search the directory of our own exe first — which is user-writable when
/// installed to %LOCALAPPDATA% — and we usually run elevated, so never resolve
/// the interpreter by name.
pub fn powershell_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}

/// Escape a value for use inside a single-quoted PowerShell string literal.
pub fn ps_quote(s: &str) -> String {
    s.replace('\'', "''")
}

fn run_ps_impl(script: &str, verbose: bool) -> Result<String, String> {
    if verbose {
        logger::log_block("powershell script", script);
    }
    let full_script = format!("{PS_PRELUDE}{script}");
    let utf16: Vec<u8> = full_script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let encoded = B64.encode(&utf16);
    let started = Instant::now();
    let out = Command::new(powershell_exe())
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &encoded,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| {
            logger::log(format!("FAILED to launch powershell: {e}"));
            format!("failed to launch powershell: {e}")
        })?;

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr_raw = String::from_utf8_lossy(&out.stderr).to_string();
    let stderr = decode_clixml(&stderr_raw).unwrap_or(stderr_raw);
    let code = out.status.code();

    let log_result = |header: &str| {
        logger::log(format!(
            "{header}: exit code {code:?} after {:.1}s",
            started.elapsed().as_secs_f64()
        ));
        if !stdout.trim().is_empty() {
            logger::log_block("stdout", stdout.trim());
        }
        if !stderr.trim().is_empty() {
            logger::log_block("stderr", stderr.trim());
        }
    };

    if out.status.success() {
        if verbose {
            log_result("powershell ok");
        }
        Ok(stdout)
    } else {
        if !verbose {
            // quiet mode still dumps everything on failure
            logger::log_block("powershell script (failed)", script);
        }
        log_result("powershell FAILED");
        let msg = if stderr.trim().is_empty() { &stdout } else { &stderr };
        let short: Vec<&str> = msg
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('+') && !l.starts_with("At line"))
            .take(4)
            .collect();
        Err(if short.is_empty() {
            "powershell command failed (press c to copy the full log)".into()
        } else {
            short.join(" ")
        })
    }
}

/// Windows PowerShell 5.1 serializes its error stream as CLIXML when stdio is
/// redirected. Extract the human-readable <S S="Error"> payloads.
fn decode_clixml(s: &str) -> Option<String> {
    if !s.contains("#< CLIXML") {
        return None;
    }
    let mut messages = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("<S S=\"Error\">") {
        rest = &rest[i + 13..];
        let Some(j) = rest.find("</S>") else { break };
        let decoded = rest[..j]
            .replace("_x000D__x000A_", "\n")
            .replace("_x000D_", "")
            .replace("_x000A_", "\n")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&");
        messages.push(decoded);
        rest = &rest[j..];
    }
    if messages.is_empty() {
        None
    } else {
        Some(messages.concat())
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct HealthReport {
    #[serde(default)]
    pub health: String,
    #[serde(default)]
    pub media: String,
    #[serde(default)]
    pub spindle: u32,
    #[serde(default)]
    pub usage: String,
    #[serde(default)]
    pub rc_ok: bool,
    #[serde(default)]
    pub wear: i64,
    #[serde(default)]
    pub temp: i64,
    #[serde(default)]
    pub temp_max: i64,
    #[serde(default)]
    pub hours: i64,
    #[serde(default)]
    pub read_err: i64,
    #[serde(default)]
    pub write_err: i64,
}

pub fn query_health(n: u32) -> Result<HealthReport, String> {
    let script = format!(
        r#"$ErrorActionPreference='Stop'
$n = {n}
$pd = Get-PhysicalDisk | Where-Object {{ $_.DeviceId -eq $n }} | Select-Object -First 1
if (-not $pd) {{ throw 'no physical-disk information available for this device' }}
$rc = $null
try {{ $rc = $pd | Get-StorageReliabilityCounter -ErrorAction Stop }} catch {{}}
$o = [pscustomobject]@{{
  health = [string]$pd.HealthStatus
  media  = [string]$pd.MediaType
  spindle = [uint32]$pd.SpindleSpeed
  usage  = [string]$pd.Usage
  rc_ok  = [bool]($null -ne $rc)
  wear   = if ($rc -and $null -ne $rc.Wear) {{ [int64]$rc.Wear }} else {{ -1 }}
  temp   = if ($rc -and $rc.Temperature -gt 0) {{ [int64]$rc.Temperature }} else {{ -1 }}
  temp_max = if ($rc -and $rc.TemperatureMax -gt 0) {{ [int64]$rc.TemperatureMax }} else {{ -1 }}
  hours  = if ($rc -and $null -ne $rc.PowerOnHours) {{ [int64]$rc.PowerOnHours }} else {{ -1 }}
  read_err = if ($rc -and $null -ne $rc.ReadErrorsTotal) {{ [int64]$rc.ReadErrorsTotal }} else {{ -1 }}
  write_err = if ($rc -and $null -ne $rc.WriteErrorsTotal) {{ [int64]$rc.WriteErrorsTotal }} else {{ -1 }}
}}
ConvertTo-Json -InputObject $o -Compress"#
    );
    let out = run_ps(&script)?;
    serde_json::from_str(out.trim()).map_err(|e| format!("could not parse health data: {e}"))
}

pub fn spawn_health(tx: Sender<AppEvent>, n: u32) {
    thread::spawn(move || {
        let _ = tx.send(AppEvent::Health(query_health(n)));
    });
}

pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Prepare a disk for destructive work: log its exact state, clear
/// write-protection (Set-Disk, falling back to diskpart), bring it online,
/// and wipe existing partitions — leaving it RAW. Every step narrates via
/// Write-Output so the log shows exactly how far it got.
fn prep_prelude(disk: &Disk, allow_protected: bool) -> String {
    let n = disk.number;
    let protect_line = if allow_protected {
        "Write-Output 'prep: SAFETY OVERRIDE ACTIVE - system/boot protection bypassed by user'"
    } else {
        "if ($d.IsBoot -or $d.IsSystem) { throw 'refusing to touch the Windows system disk' }"
    };
    // Identity check: disk numbers are reassigned when devices are plugged or
    // unplugged, so the number the user confirmed may now name a different
    // device. Compare the live disk against the snapshot that was confirmed and
    // refuse if anything that identifies the hardware has changed.
    let identity = format!(
        "$expSerial = '{serial}'\n\
         $expSize = [uint64]{size}\n\
         $expName = '{name}'\n\
         $liveSerial = ([string]$d.SerialNumber).Trim()\n\
         $liveName = ([string]$d.FriendlyName).Trim()\n\
         if ([uint64]$d.Size -ne $expSize -or ($expName -ne '' -and $liveName -ne $expName) -or ($expSerial -ne '' -and $liveSerial -ne $expSerial)) {{\n\
           throw \"disk $n is no longer the device you confirmed (expected '$expName' serial '$expSerial' $expSize bytes, found '$liveName' serial '$liveSerial' $($d.Size) bytes). Disk numbers change when drives are plugged or unplugged - rescan (r) and retry.\"\n\
         }}\n\
         Write-Output 'prep: disk identity verified'",
        serial = ps_quote(&disk.serial),
        size = disk.size,
        name = if disk.name == crate::disks::UNKNOWN_NAME { String::new() } else { ps_quote(&disk.name) },
    );
    format!(
        r#"$ErrorActionPreference='Stop'
$ConfirmPreference='None'
$n = {n}
$d = Get-Disk -Number $n
Write-Output "prep: disk $n · style=$($d.PartitionStyle) · readonly=$($d.IsReadOnly) · offline=$($d.IsOffline) · offlineReason=$($d.OfflineReason) · health=$($d.HealthStatus) · bus=$($d.BusType) · size=$($d.Size)"
{identity}
{protect_line}
if ($d.IsReadOnly) {{
  Write-Output 'prep: disk reports write-protected - clearing with Set-Disk'
  try {{ Set-Disk -Number $n -IsReadOnly $false }} catch {{ Write-Output "prep: Set-Disk error: $($_.Exception.Message.Trim())" }}
  Start-Sleep -Milliseconds 400
  $d = Get-Disk -Number $n
  $dp = ''
  if ($d.IsReadOnly) {{
    Write-Output 'prep: still read-only after Set-Disk - trying diskpart'
    $dp = (@("select disk $n", "attributes disk clear readonly", "attributes disk") | diskpart | Out-String).Trim()
    Write-Output $dp
    Start-Sleep -Milliseconds 400
    Update-HostStorageCache
    $d = Get-Disk -Number $n
  }}
  if ($d.IsReadOnly) {{
    $wp = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\StorageDevicePolicies' -ErrorAction SilentlyContinue).WriteProtect
    if ($wp -eq 1) {{ throw 'Windows policy is forcing removable disks read-only (HKLM\SYSTEM\CurrentControlSet\Control\StorageDevicePolicies\WriteProtect = 1). Set it to 0, replug the drive, and retry.' }}
    if ($dp -match 'Current Read-only State\s*:\s*Yes') {{ throw 'the DEVICE itself is write-protected at hardware level (diskpart reports Current Read-only State = Yes even after clearing the attribute). Windows cannot override this. Power-cycle the enclosure: eject it, unplug USB and power for ~10 seconds, replug, then retry. Also check for a physical lock switch and try another cable or port. If it always comes back read-only, the drive inside may have failed into read-only mode.' }}
    throw 'disk is still write-protected after Set-Disk and diskpart - see the diskpart output above in the log (press c to copy it).'
  }}
  Write-Output 'prep: write protection cleared'
}}
if ($d.IsOffline) {{ Write-Output 'prep: bringing disk online'; Set-Disk -Number $n -IsOffline $false }}
if ($d.PartitionStyle -ne 'RAW') {{ Write-Output 'prep: wiping existing partitions (Clear-Disk)'; Clear-Disk -Number $n -RemoveData -RemoveOEM -Confirm:$false }}
Write-Output 'prep: disk ready (RAW, writable, online)'
"#
    )
}

/// Run the prep script and mirror its narration lines into the UI log.
pub fn run_prep(tx: &Sender<AppEvent>, disk: &Disk, allow_protected: bool) -> Result<(), String> {
    let out = run_ps(&prep_prelude(disk, allow_protected))?;
    for l in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        log(tx, l.to_string());
    }
    Ok(())
}

fn log(tx: &Sender<AppEvent>, msg: impl Into<String>) {
    let _ = tx.send(AppEvent::Op(OpEvent::Log(msg.into())));
}

fn done(tx: &Sender<AppEvent>, result: Result<String, String>) {
    let _ = tx.send(AppEvent::Op(OpEvent::Done(result)));
}

// ---------------------------------------------------------------------------
// Format
// ---------------------------------------------------------------------------

pub fn spawn_format(tx: Sender<AppEvent>, disk: Disk, preset: Preset, label: String, allow_protected: bool) {
    thread::spawn(move || {
        let result = do_format(&tx, &disk, preset, &label, allow_protected);
        done(&tx, result);
    });
}

fn do_format(
    tx: &Sender<AppEvent>,
    disk: &Disk,
    preset: Preset,
    label: &str,
    allow_protected: bool,
) -> Result<String, String> {
    if preset == Preset::Linux {
        return format_ext4(tx, disk, label, allow_protected);
    }
    let fs = match preset {
        Preset::Windows => "NTFS",
        Preset::MacOs | Preset::Ps5 => "exFAT",
        Preset::Universal => {
            if disk.size <= 32 * GIB {
                "FAT32"
            } else {
                "exFAT"
            }
        }
        Preset::Linux => unreachable!(),
    };
    let scheme = preset.scheme();
    log(tx, format!("Plan: {scheme} partition table · {fs} · label '{label}'"));
    if preset == Preset::Universal && fs == "exFAT" {
        log(tx, "Disk is larger than 32 GB — Windows can't create FAT32 that big, using exFAT.");
    }

    log(tx, "Step 1/3 — prepare disk (write-protection, online, wipe old partitions)…");
    run_prep(tx, disk, allow_protected)?;

    log(tx, format!("Step 2/3 — initialize {scheme}, create partition…"));
    let script = format!(
        "$ErrorActionPreference='Stop'\n\
         $n = {n}\n\
         Initialize-Disk -Number $n -PartitionStyle {scheme} -Confirm:$false | Out-Null\n\
         Write-Output 'init: disk initialized as {scheme}'\n\
         Start-Sleep -Milliseconds 500\n\
         $p = New-Partition -DiskNumber $n -UseMaximumSize -AssignDriveLetter\n\
         Start-Sleep -Milliseconds 700\n\
         Write-Output \"PART-OK:$($p.PartitionNumber):$($p.DriveLetter)\"",
        n = disk.number
    );
    let out = run_ps(&script)?;
    let marker = out
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("PART-OK:"))
        .ok_or("partition step produced no PART-OK marker (press c to copy the full log)")?;
    let mut fields = marker.splitn(3, ':');
    fields.next();
    let pnum: u32 = fields.next().unwrap_or("1").trim().parse().unwrap_or(1);
    let letter: String = fields
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    log(
        tx,
        format!(
            "  partition {pnum} created{}",
            if letter.is_empty() { String::new() } else { format!(" — drive letter {letter}:") }
        ),
    );

    log(tx, format!("Step 3/3 — format as {fs}, label '{label}'…"));
    let script = format!(
        "$ErrorActionPreference='Stop'\n\
         $p = Get-Partition -DiskNumber {n} -PartitionNumber {pnum}\n\
         Format-Volume -Partition $p -FileSystem {fs} -NewFileSystemLabel '{lab}' -Confirm:$false | Out-Null\n\
         Write-Output 'format: complete'",
        n = disk.number,
        lab = label.replace('\'', "''"),
    );
    run_ps(&script)?;

    let mounted = if letter.is_empty() { String::new() } else { format!(" — mounted as {letter}:") };
    Ok(format!(
        "Disk {} formatted as {fs} ({scheme}), label '{label}'{mounted}",
        disk.number
    ))
}

fn format_ext4(
    tx: &Sender<AppEvent>,
    disk: &Disk,
    label: &str,
    allow_protected: bool,
) -> Result<String, String> {
    let label: String = label
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(16)
        .collect();
    let n = disk.number;
    log(tx, "ext4 via WSL 2 — checking WSL, cleaning disk, attaching it to Linux…");
    let script = format!(
        r#"{prep}$env:WSL_UTF8='1'
wsl.exe --status *> $null
if ($LASTEXITCODE -ne 0) {{ throw 'WSL 2 is not installed (run: wsl --install), or no default distro exists. Alternatively use the macOS or Universal preset (exFAT is readable on Linux).' }}
$before = @(wsl.exe -u root -e sh -c 'ls /dev/sd? 2>/dev/null')
wsl.exe --mount \\.\PHYSICALDRIVE{n} --bare
if ($LASTEXITCODE -ne 0) {{ throw 'wsl --mount failed. It requires an elevated terminal and WSL 2.' }}
try {{
  Start-Sleep -Milliseconds 1500
  $after = @(wsl.exe -u root -e sh -c 'ls /dev/sd? 2>/dev/null')
  $dev = $after | Where-Object {{ $_ -and ($before -notcontains $_) }} | Select-Object -First 1
  if (-not $dev) {{ throw 'could not locate the attached disk inside WSL' }}
  Write-Output "wsl: disk attached as $dev"
  wsl.exe -u root -e sh -c "set -e; printf 'label: gpt\ntype=0FC63DAF-8483-4772-8E79-3D69D8477DE4\n' | sfdisk --wipe always $dev; sleep 1; mkfs.ext4 -F -q -L '{label}' ${{dev}}1"
  if ($LASTEXITCODE -ne 0) {{ throw 'partitioning or mkfs.ext4 failed inside WSL' }}
}} finally {{
  wsl.exe --unmount \\.\PHYSICALDRIVE{n} *> $null
}}
'OK'"#,
        prep = prep_prelude(disk, allow_protected),
    );
    let out = run_ps(&script)?;
    for l in out.lines().map(str::trim).filter(|l| !l.is_empty() && *l != "OK") {
        log(tx, l.to_string());
    }
    Ok(format!(
        "Disk {n} formatted as ext4 (GPT), label '{label}'. Windows can't read ext4 — the drive will show as unformatted here; that's expected."
    ))
}

// ---------------------------------------------------------------------------
// Erase
// ---------------------------------------------------------------------------

pub fn spawn_erase_quick(tx: Sender<AppEvent>, disk: Disk, allow_protected: bool) {
    thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            log(&tx, "Removing partition table and filesystem structures…");
            run_prep(&tx, &disk, allow_protected)?;
            Ok(format!("Disk {} erased — it is now blank (RAW/uninitialized).", disk.number))
        })();
        done(&tx, result);
    });
}

pub fn spawn_zero_fill(
    tx: Sender<AppEvent>,
    disk: Disk,
    cancel: Arc<AtomicBool>,
    allow_protected: bool,
) {
    thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            log(&tx, "Preparing disk (write-protection, wipe partitions)…");
            run_prep(&tx, &disk, allow_protected)?;
            log(&tx, format!("Overwriting all {} with zeros — every sector, start to end…", human(disk.size)));
            let mut dev = open_physical(disk.number)?;
            log(&tx, format!(r"raw device \\.\PHYSICALDRIVE{} opened for writing", disk.number));
            let zeros = vec![0u8; CHUNK];
            let total = disk.size;
            let start = Instant::now();
            let mut last = Instant::now();
            let mut written: u64 = 0;
            while written < total {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled — the disk was partially wiped and is blank".into());
                }
                let want = ((total - written).min(CHUNK as u64)) as usize;
                dev.write_all(&zeros[..want])
                    .map_err(|e| format!("write failed at {}: {e}", human(written)))?;
                written += want as u64;
                if last.elapsed() >= Duration::from_millis(250) {
                    progress(&tx, written, total, start);
                    last = Instant::now();
                }
            }
            dev.sync_all().map_err(|e| format!("flush failed: {e}"))?;
            progress(&tx, total, total, start);
            Ok(format!(
                "Secure erase complete — {} zero-filled in {}. Disk is blank (RAW).",
                human(total),
                fmt_secs(start.elapsed().as_secs_f64())
            ))
        })();
        done(&tx, result);
    });
}

// ---------------------------------------------------------------------------
// Write ISO / disk image
// ---------------------------------------------------------------------------

pub fn spawn_write_iso(
    tx: Sender<AppEvent>,
    disk: Disk,
    path: PathBuf,
    image_size: u64,
    cancel: Arc<AtomicBool>,
    allow_protected: bool,
) {
    thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            log(&tx, format!("Image: {} ({})", path.display(), human(image_size)));
            if image_size > disk.size {
                return Err(format!(
                    "image ({}) is larger than the disk ({})",
                    human(image_size),
                    human(disk.size)
                ));
            }
            log(&tx, "Preparing disk (write-protection, wipe partitions)…");
            run_prep(&tx, &disk, allow_protected)?;
            log(&tx, "Writing image sector-by-sector…");
            let mut src = File::open(&path).map_err(|e| format!("cannot open image: {e}"))?;
            let mut dev = open_physical(disk.number)?;
            log(&tx, format!(r"raw device \\.\PHYSICALDRIVE{} opened for writing", disk.number));
            let mut buf = vec![0u8; CHUNK];
            let start = Instant::now();
            let mut last = Instant::now();
            let mut written: u64 = 0;
            let mut remaining = image_size;
            while remaining > 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled — the disk contains an incomplete image; erase it before use".into());
                }
                let want = remaining.min(CHUNK as u64) as usize;
                src.read_exact(&mut buf[..want])
                    .map_err(|e| format!("read failed at {}: {e}", human(written)))?;
                // device writes must be sector-aligned; pad the tail with zeros
                let padded = want.div_ceil(512) * 512;
                if padded > want {
                    buf[want..padded].fill(0);
                }
                dev.write_all(&buf[..padded])
                    .map_err(|e| format!("write failed at {}: {e}", human(written)))?;
                written += want as u64;
                remaining -= want as u64;
                if last.elapsed() >= Duration::from_millis(250) {
                    progress(&tx, written, image_size, start);
                    last = Instant::now();
                }
            }
            dev.sync_all().map_err(|e| format!("flush failed: {e}"))?;
            progress(&tx, image_size, image_size, start);
            drop(dev);

            // read everything back through an uncached handle and compare
            log(&tx, "Verifying — reading the written data back from the disk…");
            let mut src = File::open(&path).map_err(|e| format!("cannot reopen image: {e}"))?;
            let mut dev = crate::bench::open_direct(disk.number, false)?;
            let mut want_buf = vec![0u8; CHUNK];
            let mut disk_buf = crate::bench::AlignedBuf::new(CHUNK);
            let vstart = Instant::now();
            let mut last = Instant::now();
            let mut checked = 0u64;
            while checked < image_size {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled during verification — the image itself was fully written".into());
                }
                let want = (image_size - checked).min(CHUNK as u64) as usize;
                src.read_exact(&mut want_buf[..want])
                    .map_err(|e| format!("image re-read failed at {}: {e}", human(checked)))?;
                let mut aligned = want.div_ceil(4096) * 4096;
                if checked + aligned as u64 > disk.size {
                    aligned = want.div_ceil(512) * 512;
                }
                dev.read_exact(&mut disk_buf.as_mut()[..aligned])
                    .map_err(|e| format!("disk read-back failed at {}: {e}", human(checked)))?;
                if disk_buf.as_ref()[..want] != want_buf[..want] {
                    let pos = disk_buf.as_ref()[..want]
                        .iter()
                        .zip(&want_buf[..want])
                        .position(|(a, b)| a != b)
                        .unwrap_or(0) as u64;
                    return Err(format!(
                        "VERIFICATION FAILED at offset {} — the disk does not match the image. The drive may be faulty; retry or test it (b).",
                        human(checked + pos)
                    ));
                }
                checked += want as u64;
                if last.elapsed() >= Duration::from_millis(250) {
                    progress_with(&tx, checked, image_size, vstart, "verify");
                    last = Instant::now();
                }
            }
            log(&tx, "Verification passed — the disk matches the image bit-for-bit.");
            Ok(format!(
                "Image written to disk {} and verified bit-for-bit in {}. If Windows prompts to format new partitions, cancel those prompts.",
                disk.number,
                fmt_secs(start.elapsed().as_secs_f64())
            ))
        })();
        done(&tx, result);
    });
}

// ---------------------------------------------------------------------------
// Raw device helpers
// ---------------------------------------------------------------------------

fn open_physical(n: u32) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!(r"\\.\PHYSICALDRIVE{n}"))
        .map_err(|e| {
            logger::log(format!(r"open \\.\PHYSICALDRIVE{n} FAILED: {e}"));
            format!(r"cannot open \\.\PHYSICALDRIVE{n}: {e} — an elevated (administrator) terminal is required")
        })
}

fn progress(tx: &Sender<AppEvent>, done_bytes: u64, total: u64, start: Instant) {
    progress_with(tx, done_bytes, total, start, "");
}

pub fn progress_with(tx: &Sender<AppEvent>, done_bytes: u64, total: u64, start: Instant, phase: &str) {
    let secs = start.elapsed().as_secs_f64().max(0.001);
    let speed = done_bytes as f64 / secs;
    let eta = (total.saturating_sub(done_bytes)) as f64 / speed.max(1.0);
    let prefix = if phase.is_empty() { String::new() } else { format!("{phase}: ") };
    let detail = format!(
        "{prefix}{} / {}  ·  {}/s  ·  eta {}",
        human(done_bytes),
        human(total),
        human(speed as u64),
        fmt_secs(eta)
    );
    let frac = if total == 0 { 1.0 } else { done_bytes as f64 / total as f64 };
    let _ = tx.send(AppEvent::Op(OpEvent::Sample(speed as u64)));
    let _ = tx.send(AppEvent::Op(OpEvent::Progress(frac, detail)));
}

pub fn fmt_secs_pub(secs: f64) -> String {
    fmt_secs(secs)
}

fn fmt_secs(secs: f64) -> String {
    let s = secs as u64;
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}
