use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
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
            // The first chunk holds the partition table. It is written LAST:
            // as soon as a valid table lands at sector 0 Windows may automount
            // the new volumes and then refuse further raw writes to them.
            let first_len = image_size.min(CHUNK as u64) as usize;
            let mut first = vec![0u8; first_len.div_ceil(512) * 512];
            src.read_exact(&mut first[..first_len])
                .map_err(|e| format!("read failed at 0: {e}"))?;
            dev.seek(SeekFrom::Start(first.len() as u64))
                .map_err(|e| format!("seek failed: {e}"))?;
            let mut written: u64 = first_len as u64;
            let mut remaining = image_size - first_len as u64;
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
            log(&tx, "Writing the partition table (first sectors)…");
            dev.seek(SeekFrom::Start(0)).map_err(|e| format!("seek failed: {e}"))?;
            dev.write_all(&first).map_err(|e| format!("write failed at 0: {e}"))?;
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
// Backup (disk → image file) and clone (disk → disk)
// ---------------------------------------------------------------------------

/// Free bytes on the volume that holds `path` (its parent directory).
pub fn free_space(path: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty())?;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free = 0u64;
    let mut total = 0u64;
    let mut total_free = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut total_free) };
    (ok != 0).then_some(free)
}

/// Filesystem name ("NTFS", "FAT32", "exFAT"…) of the volume at a drive letter.
pub fn volume_filesystem(drive: char) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
    let root: Vec<u16> = format!("{drive}:\\").encode_utf16().chain(std::iter::once(0)).collect();
    let mut fs = [0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
    Some(String::from_utf16_lossy(&fs[..len]))
}

/// Read-only identity check (no Set-Disk): the live disk at this number must
/// still be the device the user selected. Used for read sources, where the
/// mutating prep script is not appropriate.
pub fn verify_identity(disk: &Disk) -> Result<(), String> {
    let script = format!(
        "$ErrorActionPreference='Stop'\n\
         $d = Get-Disk -Number {n}\n\
         $liveSerial = ([string]$d.SerialNumber).Trim()\n\
         $liveName = ([string]$d.FriendlyName).Trim()\n\
         if ([uint64]$d.Size -ne [uint64]{size} -or ('{name}' -ne '' -and $liveName -ne '{name}') -or ('{serial}' -ne '' -and $liveSerial -ne '{serial}')) {{ throw \"disk {n} is no longer the device you selected (found '$liveName' serial '$liveSerial' $($d.Size) bytes) - disk numbers change when drives are plugged or unplugged; rescan (r) and retry\" }}\n\
         Write-Output 'identity: verified'",
        n = disk.number,
        size = disk.size,
        // an empty FriendlyName is shown as UNKNOWN_NAME; don't compare it
        name = if disk.name == crate::disks::UNKNOWN_NAME { String::new() } else { ps_quote(&disk.name) },
        serial = ps_quote(&disk.serial),
    );
    run_ps(&script).map(|_| ())
}

const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;

/// Suffix of an image that is still being written: `foo.img.partial`. Next to
/// it, `foo.img.partial.json` records which disk it belongs to so a later run
/// can pick it up where it left off. Only a finished, flushed image is renamed
/// to the bare `.img` name, so a `.img` file is always complete.
pub const PARTIAL_SUFFIX: &str = ".partial";

/// Identity stored in the partial sidecar — must match the disk to resume.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PartialMeta {
    pub name: String,
    pub serial: String,
    pub size: u64,
}

impl PartialMeta {
    pub fn of(d: &Disk) -> Self {
        PartialMeta { name: d.name.clone(), serial: d.serial.clone(), size: d.size }
    }
}

/// An interrupted image that can be continued for this disk.
#[derive(Debug, Clone)]
pub struct Resumable {
    /// Final `.img` path (the partial is `path + ".partial"`).
    pub path: PathBuf,
    /// Bytes already on disk, rounded down to a whole chunk.
    pub done: u64,
}

pub fn partial_path(img: &std::path::Path) -> PathBuf {
    let mut p = img.as_os_str().to_owned();
    p.push(PARTIAL_SUFFIX);
    PathBuf::from(p)
}

fn sidecar_path(img: &std::path::Path) -> PathBuf {
    let mut p = partial_path(img).into_os_string();
    p.push(".json");
    PathBuf::from(p)
}

/// Look through `dir` for `*.img.partial` files whose sidecar says they belong
/// to `disk`; return the one with the most data. The partial's length is
/// trusted only after rounding down to a chunk boundary — every chunk was
/// written whole, so a torn last chunk from a hard kill is simply redone.
pub fn find_resumable(dir: &std::path::Path, disk: &Disk) -> Option<Resumable> {
    let want = PartialMeta::of(disk);
    let rd = std::fs::read_dir(dir).ok()?;
    let mut best: Option<Resumable> = None;
    for e in rd.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
        let Some(img_name) = name.strip_suffix(PARTIAL_SUFFIX) else { continue };
        if !img_name.ends_with(".img") {
            continue;
        }
        let img = dir.join(img_name);
        let Ok(meta_text) = std::fs::read_to_string(sidecar_path(&img)) else { continue };
        let Ok(meta) = serde_json::from_str::<PartialMeta>(&meta_text) else { continue };
        let same = meta.size == want.size
            && if want.serial.is_empty() { meta.name == want.name } else { meta.serial == want.serial };
        if !same {
            continue;
        }
        let len = e.metadata().map(|m| m.len()).unwrap_or(0);
        let done = (len / CHUNK as u64) * CHUNK as u64;
        if done > disk.size {
            continue;
        }
        if best.as_ref().is_none_or(|b| done > b.done) {
            best = Some(Resumable { path: img, done });
        }
    }
    best
}

/// Read the whole disk through an uncached handle into an image file.
/// Non-destructive for the disk. Data goes to `<path>.partial` and is renamed
/// to `path` only once fully written and flushed. If the destination folder
/// already holds a partial image of this same disk, that one is continued
/// instead (and `path` is ignored); a cancelled run keeps its partial so it
/// can be resumed.
pub fn spawn_backup(tx: Sender<AppEvent>, disk: Disk, path: PathBuf, cancel: Arc<AtomicBool>) {
    thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            let dir = path.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
            let resume = find_resumable(&dir, &disk);
            let (path, offset) = match &resume {
                Some(r) => (r.path.clone(), r.done),
                None => (path, 0),
            };
            let partial = partial_path(&path);
            if let Some(r) = &resume {
                log(&tx, format!(
                    "Resuming interrupted image {} — {} of {} already saved, {} to go",
                    partial.display(),
                    human(r.done),
                    human(disk.size),
                    human(disk.size - r.done)
                ));
                log(&tx, "Note: the earlier part reflects the disk as it was then — only safe if the disk has not been written to since.");
            } else {
                log(&tx, format!("Backing up disk {} ({}) → {}", disk.number, human(disk.size), path.display()));
            }
            log(&tx, "Verifying source identity…");
            verify_identity(&disk)?;
            if disk.partitions.iter().any(|p| !p.letter.is_empty()) {
                log(&tx, "Note: volumes are mounted — this is a live snapshot; close programs writing to the disk for a consistent image.");
            }
            let mut src = crate::bench::open_direct(disk.number, false)?;
            // SEQUENTIAL_SCAN keeps a multi-TB stream from bloating the cache
            // manager. Fresh: create_new so nothing is ever overwritten.
            // Resume: open the existing partial and drop any torn tail.
            let mut out = if resume.is_some() {
                let f = OpenOptions::new()
                    .write(true)
                    .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
                    .open(&partial)
                    .map_err(|e| format!("cannot open partial image: {e}"))?;
                f.set_len(offset).map_err(|e| format!("cannot trim partial image: {e}"))?;
                f
            } else {
                if path.exists() {
                    return Err(format!("{} already exists — refusing to overwrite a finished image", path.display()));
                }
                let f = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
                    .open(&partial)
                    .map_err(|e| format!("cannot create image file: {e}"))?;
                let meta = serde_json::to_string(&PartialMeta::of(&disk)).map_err(|e| e.to_string())?;
                std::fs::write(sidecar_path(&path), meta).map_err(|e| format!("cannot write sidecar: {e}"))?;
                f
            };
            if offset > 0 {
                src.seek(SeekFrom::Start(offset)).map_err(|e| format!("disk seek failed: {e}"))?;
                out.seek(SeekFrom::Start(offset)).map_err(|e| format!("image seek failed: {e}"))?;
            }
            let mut buf = crate::bench::AlignedBuf::new(CHUNK);
            let total = disk.size;
            let start = Instant::now();
            let mut last = Instant::now();
            let mut done_b = offset;
            while done_b < total {
                if cancel.load(Ordering::Relaxed) {
                    let _ = out.sync_all();
                    drop(out);
                    return Err(format!(
                        "cancelled — {} of {} kept in {}; start the same backup again to resume from there",
                        human(done_b),
                        human(total),
                        partial.display()
                    ));
                }
                let want = (total - done_b).min(CHUNK as u64) as usize;
                src.read_exact(&mut buf.as_mut()[..want])
                    .map_err(|e| format!("disk read failed at {}: {e}", human(done_b)))?;
                out.write_all(&buf.as_ref()[..want]).map_err(|e| {
                    format!(
                        "image write failed at {}: {e} (destination full?) — {} kept for resume",
                        human(done_b),
                        partial.display()
                    )
                })?;
                done_b += want as u64;
                if last.elapsed() >= Duration::from_millis(250) {
                    progress_with_base(&tx, done_b, total, done_b - offset, start, "backup");
                    last = Instant::now();
                }
            }
            out.sync_all().map_err(|e| format!("flush failed: {e}"))?;
            drop(out);
            std::fs::rename(&partial, &path).map_err(|e| {
                format!("image written but could not rename {} to its final name: {e}", partial.display())
            })?;
            let _ = std::fs::remove_file(sidecar_path(&path));
            progress_with_base(&tx, total, total, total - offset, start, "backup");
            let resumed = if offset > 0 { format!(" (resumed from {})", human(offset)) } else { String::new() };
            Ok(format!(
                "Backed up disk {} to {} ({}) in {}{resumed}. Restore it onto any disk of at least that size with i (write image).",
                disk.number,
                path.display(),
                human(total),
                fmt_secs(start.elapsed().as_secs_f64())
            ))
        })();
        done(&tx, result);
    });
}

/// Sector-for-sector copy of `source` onto `target`. The target is wiped via
/// the prep script (identity check included); the source is only read.
pub fn spawn_clone(
    tx: Sender<AppEvent>,
    source: Disk,
    target: Disk,
    cancel: Arc<AtomicBool>,
    allow_protected: bool,
) {
    thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            log(&tx, format!(
                "Clone: disk {} ({} · {}) → disk {} ({} · {})",
                source.number, source.name, human(source.size),
                target.number, target.name, human(target.size)
            ));
            if source.size > target.size {
                return Err(format!(
                    "source ({}) is larger than target ({}) — shrinking clones are not supported",
                    human(source.size),
                    human(target.size)
                ));
            }
            if source.partitions.iter().any(|p| !p.letter.is_empty()) {
                log(&tx, "Note: source volumes are mounted — this is a live snapshot; close programs writing to it for a consistent clone.");
            }
            log(&tx, "Verifying source identity…");
            verify_identity(&source)?;
            log(&tx, "Preparing target (identity check, write-protection, wipe)…");
            run_prep(&tx, &target, allow_protected)?;
            let mut src = crate::bench::open_direct(source.number, false)?;
            let mut dst = open_physical(target.number)?;
            let mut buf = crate::bench::AlignedBuf::new(CHUNK);
            let total = source.size;
            let start = Instant::now();
            let mut last = Instant::now();
            // partition table (first chunk) is written last — see spawn_write_iso
            let first_len = total.min(CHUNK as u64) as usize;
            let mut first = crate::bench::AlignedBuf::new(CHUNK);
            src.read_exact(&mut first.as_mut()[..first_len])
                .map_err(|e| format!("source read failed at 0: {e}"))?;
            dst.seek(SeekFrom::Start(first_len as u64)).map_err(|e| format!("seek failed: {e}"))?;
            let mut done_b = first_len as u64;
            while done_b < total {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled — the target holds an incomplete clone; erase it before use".into());
                }
                let want = (total - done_b).min(CHUNK as u64) as usize;
                src.read_exact(&mut buf.as_mut()[..want])
                    .map_err(|e| format!("source read failed at {}: {e}", human(done_b)))?;
                dst.write_all(&buf.as_ref()[..want])
                    .map_err(|e| format!("target write failed at {}: {e}", human(done_b)))?;
                done_b += want as u64;
                if last.elapsed() >= Duration::from_millis(250) {
                    progress_with(&tx, done_b, total, start, "clone");
                    last = Instant::now();
                }
            }
            // A larger target keeps its OLD backup GPT header/entries at its
            // last LBAs (Clear-Disk only wipes the primary). The cloned primary
            // points its backup at source.size-1, so tools would see a stale
            // table at end-of-disk and offer to "restore" it — zero it.
            if target.size > source.size {
                const TAIL: u64 = 1024 * 1024;
                // never reach back into the cloned data: the clone's own backup
                // GPT lives in the last sectors of source.size
                let tail_start = target
                    .size
                    .saturating_sub(TAIL)
                    .max(source.size)
                    .div_ceil(512)
                    * 512;
                if tail_start < target.size {
                    let tail_len = (target.size - tail_start) as usize;
                    log(&tx, format!("Target is larger than source — clearing its old end-of-disk GPT area ({})", human(tail_len as u64)));
                    dst.seek(SeekFrom::Start(tail_start)).map_err(|e| format!("seek failed: {e}"))?;
                    dst.write_all(&vec![0u8; tail_len])
                        .map_err(|e| format!("target tail clear failed: {e}"))?;
                }
            }
            log(&tx, "Writing the partition table (first sectors)…");
            dst.seek(SeekFrom::Start(0)).map_err(|e| format!("seek failed: {e}"))?;
            dst.write_all(&first.as_ref()[..first_len])
                .map_err(|e| format!("target write failed at 0: {e}"))?;
            dst.sync_all().map_err(|e| format!("flush failed: {e}"))?;
            drop(dst);
            drop(src);
            progress_with(&tx, total, total, start, "clone");

            log(&tx, "Verifying — comparing target against source…");
            let mut src = crate::bench::open_direct(source.number, false)?;
            let mut dst = crate::bench::open_direct(target.number, false)?;
            let mut sbuf = crate::bench::AlignedBuf::new(CHUNK);
            let mut dbuf = crate::bench::AlignedBuf::new(CHUNK);
            let vstart = Instant::now();
            let mut last = Instant::now();
            let mut checked = 0u64;
            while checked < total {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled during verification — the clone itself was fully written".into());
                }
                let want = (total - checked).min(CHUNK as u64) as usize;
                src.read_exact(&mut sbuf.as_mut()[..want])
                    .map_err(|e| format!("source re-read failed at {}: {e}", human(checked)))?;
                dst.read_exact(&mut dbuf.as_mut()[..want])
                    .map_err(|e| format!("target read-back failed at {}: {e}", human(checked)))?;
                if sbuf.as_ref()[..want] != dbuf.as_ref()[..want] {
                    let pos = sbuf.as_ref()[..want]
                        .iter()
                        .zip(&dbuf.as_ref()[..want])
                        .position(|(a, b)| a != b)
                        .unwrap_or(0) as u64;
                    return Err(format!(
                        "VERIFICATION FAILED at offset {} — target does not match source (a live source that changed during the copy, or a faulty target). Retry with the source idle, or test the target (b).",
                        human(checked + pos)
                    ));
                }
                checked += want as u64;
                if last.elapsed() >= Duration::from_millis(250) {
                    progress_with(&tx, checked, total, vstart, "verify");
                    last = Instant::now();
                }
            }
            log(&tx, "Verification passed — target matches source sector-for-sector.");
            log(&tx, "Bringing the clone online…");
            let _ = run_ps(&format!(
                "Set-Disk -Number {} -IsOffline $false -ErrorAction SilentlyContinue\nUpdate-HostStorageCache",
                target.number
            ));
            Ok(format!(
                "Cloned disk {} → disk {} ({}) in {}. If Windows keeps the clone offline because its signature collides with the source, unplug the source or online it in Disk Management.",
                source.number,
                target.number,
                human(total),
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
    progress_with_base(tx, done_bytes, total, done_bytes, start, phase)
}

/// Like `progress_with`, but speed/ETA are computed from `session_bytes` —
/// the bytes moved since `start` — so a resumed job reports its real rate
/// instead of counting the data a previous run already saved.
pub fn progress_with_base(
    tx: &Sender<AppEvent>,
    done_bytes: u64,
    total: u64,
    session_bytes: u64,
    start: Instant,
    phase: &str,
) {
    let secs = start.elapsed().as_secs_f64().max(0.001);
    let speed = session_bytes as f64 / secs;
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

// ---------------------------------------------------------------------------
// Manage (m): quick non-destructive disk/volume operations
// ---------------------------------------------------------------------------

/// Quick operations that change how Windows presents a disk without
/// touching its data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManageOp {
    /// Assign or change the drive letter of a partition.
    SetLetter { partition: u32, letter: char },
    /// Remove the drive letter of a partition (volume stays intact, unmounted).
    RemoveLetter { partition: u32, letter: char },
    /// Rename the volume label of a lettered partition.
    SetLabel { letter: char, label: String },
    Online,
    Offline,
    /// Safely eject (flush + surprise-removal-safe) via the shell; falls back
    /// to taking the disk offline when no volume has a letter.
    Eject { letter: Option<char> },
    ClearReadOnly,
}

impl ManageOp {
    pub fn title(&self, disk: u32) -> String {
        match self {
            ManageOp::SetLetter { partition, letter } => format!("Disk {disk} · partition {partition} → {letter}:"),
            ManageOp::RemoveLetter { partition, letter } => format!("Disk {disk} · removing {letter}: from partition {partition}"),
            ManageOp::SetLabel { letter, .. } => format!("Disk {disk} · renaming volume {letter}:"),
            ManageOp::Online => format!("Disk {disk} · bringing online"),
            ManageOp::Offline => format!("Disk {disk} · taking offline"),
            ManageOp::Eject { .. } => format!("Disk {disk} · safely ejecting"),
            ManageOp::ClearReadOnly => format!("Disk {disk} · clearing read-only flag"),
        }
    }

    fn script(&self, n: u32) -> String {
        match self {
            ManageOp::SetLetter { partition, letter } => format!(
                "if (Test-Path '{letter}:') {{ throw '{letter}: is already in use' }}\n\
                 Set-Partition -DiskNumber $n -PartitionNumber {partition} -NewDriveLetter '{letter}'\n\
                 Write-Output 'partition {partition} is now {letter}:'"
            ),
            ManageOp::RemoveLetter { partition, letter } => format!(
                "Remove-PartitionAccessPath -DiskNumber $n -PartitionNumber {partition} -AccessPath '{letter}:\'\n\
                 Write-Output 'removed {letter}: (the volume is intact; assign a letter again to use it)'"
            ),
            ManageOp::SetLabel { letter, label } => format!(
                "Set-Volume -DriveLetter '{letter}' -NewFileSystemLabel '{label}'\n\
                 Write-Output \"volume {letter}: is now labelled '{label}'\"",
                label = ps_quote(label)
            ),
            ManageOp::Online => "Set-Disk -Number $n -IsOffline $false\n\
                 $d = Get-Disk -Number $n\n\
                 if ($d.IsOffline) { throw \"disk is still offline (reason: $($d.OfflineReason))\" }\n\
                 Write-Output 'disk is online'"
                .into(),
            ManageOp::Offline => "Set-Disk -Number $n -IsOffline $true\n\
                 Write-Output 'disk is offline — Windows will not touch it until it is brought online again'"
                .into(),
            ManageOp::Eject { letter: Some(l) } => format!(
                "$vol = (New-Object -ComObject Shell.Application).NameSpace(17).ParseName('{l}:')\n\
                 if (-not $vol) {{ throw 'the shell cannot see {l}:' }}\n\
                 $vol.InvokeVerb('Eject')\n\
                 Start-Sleep -Milliseconds 1500\n\
                 if (Test-Path '{l}:') {{ throw 'Windows refused to eject — a program is still using the drive (close Explorer windows and apps, then retry)' }}\n\
                 Write-Output 'ejected — safe to unplug'"
            ),
            ManageOp::Eject { letter: None } => "Set-Disk -Number $n -IsOffline $true\n\
                 Write-Output 'no drive letter to eject through the shell — the disk was taken offline instead, which flushes it; safe to unplug'"
                .into(),
            ManageOp::ClearReadOnly => "Set-Disk -Number $n -IsReadOnly $false\n\
                 Start-Sleep -Milliseconds 400\n\
                 $d = Get-Disk -Number $n\n\
                 if ($d.IsReadOnly) { throw 'still read-only after Set-Disk — the device itself may be write-protected (see the format prep diagnostics, or check for a lock switch / enclosure fault)' }\n\
                 Write-Output 'read-only flag cleared'"
                .into(),
        }
        .replace("$n", &n.to_string())
    }
}

/// Run a manage op: identity check first, then the short script. Narration
/// goes to the progress modal; the result message is the last output line.
pub fn spawn_manage(tx: Sender<AppEvent>, disk: Disk, op: ManageOp) {
    thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            log(&tx, "Verifying disk identity…");
            verify_identity(&disk)?;
            let script = format!(
                "$ErrorActionPreference='Stop'\n$ConfirmPreference='None'\n{}\nUpdate-HostStorageCache",
                op.script(disk.number)
            );
            let out = run_ps(&script)?;
            let lines: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            for l in &lines {
                log(&tx, l.to_string());
            }
            Ok(lines.last().map(|s| s.to_string()).unwrap_or_else(|| "done".into()))
        })();
        done(&tx, result);
    });
}

/// Drive letters not currently in use (includes mapped network drives,
/// which `Test-Path` sees and `Get-Volume` does not).
pub fn free_drive_letters() -> Vec<char> {
    let script = "(([char[]](68..90)) | Where-Object { -not (Test-Path \"$($_):\") }) -join ''";
    match run_ps_quiet(script) {
        Ok(out) => out.trim().chars().filter(|c| c.is_ascii_uppercase()).collect(),
        Err(_) => ('D'..='Z').collect(),
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;

    fn disk(serial: &str, size: u64) -> Disk {
        Disk { name: "WD BLACK".into(), serial: serial.into(), size, ..Default::default() }
    }

    fn plant(dir: &std::path::Path, img: &str, meta: &PartialMeta, len: u64) {
        let f = File::create(partial_path(&dir.join(img))).unwrap();
        f.set_len(len).unwrap();
        std::fs::write(sidecar_path(&dir.join(img)), serde_json::to_string(meta).unwrap()).unwrap();
    }

    #[test]
    fn partial_and_sidecar_names() {
        let img = PathBuf::from(r"Z:\b\auto-x.img");
        assert_eq!(partial_path(&img), PathBuf::from(r"Z:\b\auto-x.img.partial"));
        assert_eq!(sidecar_path(&img), PathBuf::from(r"Z:\b\auto-x.img.partial.json"));
    }

    #[test]
    fn find_resumable_matches_identity_and_rounds_to_chunks() {
        let dir = std::env::temp_dir().join(format!("du-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = disk("S1", 100 * CHUNK as u64);
        // nothing yet
        assert!(find_resumable(&dir, &d).is_none());
        // 9 chunks + a torn tail → 9 whole chunks
        plant(&dir, "a.img", &PartialMeta::of(&d), 9 * CHUNK as u64 + 12345);
        let r = find_resumable(&dir, &d).unwrap();
        assert_eq!(r.done, 9 * CHUNK as u64);
        assert_eq!(r.path, dir.join("a.img"));
        // a bigger partial of the same disk wins
        plant(&dir, "b.img", &PartialMeta::of(&d), 20 * CHUNK as u64);
        assert_eq!(find_resumable(&dir, &d).unwrap().path, dir.join("b.img"));
        // other serial / other size: not ours
        plant(&dir, "c.img", &PartialMeta::of(&disk("S2", d.size)), 50 * CHUNK as u64);
        plant(&dir, "d.img", &PartialMeta::of(&disk("S1", d.size + 1)), 60 * CHUNK as u64);
        assert_eq!(find_resumable(&dir, &d).unwrap().path, dir.join("b.img"));
        // no sidecar → ignored
        File::create(partial_path(&dir.join("e.img"))).unwrap().set_len(90 * CHUNK as u64).unwrap();
        assert_eq!(find_resumable(&dir, &d).unwrap().path, dir.join("b.img"));
        // unnamed-serial disks fall back to the name
        let anon = Disk { name: "NoSerial".into(), serial: String::new(), size: 10, ..Default::default() };
        plant(&dir, "f.img", &PartialMeta::of(&anon), 0);
        assert_eq!(find_resumable(&dir, &anon).unwrap().path, dir.join("f.img"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
