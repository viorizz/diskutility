# Disk Utility

[![CI](https://github.com/viorizz/diskutility/actions/workflows/ci.yml/badge.svg)](https://github.com/viorizz/diskutility/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/viorizz/diskutility)](https://github.com/viorizz/diskutility/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A fast terminal disk utility for Windows, written in Rust with a
Claude Code / OpenCode-style TUI ([ratatui](https://ratatui.rs)).
Create, format, erase disks and write bootable images — from the keyboard.

## Install

**One-liner (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/viorizz/diskutility/main/install.ps1 | iex
```

**Manual:** download `diskutility.exe` from the
[latest release](https://github.com/viorizz/diskutility/releases/latest)
(verify with `checksums.txt`).

**winget:** (once the package is accepted into the community repository)

```powershell
winget install viorizz.diskutility
```

**From source:**

```powershell
cargo install --git https://github.com/viorizz/diskutility
```

**Stay up to date:** the TUI shows a banner when a new release is out — press
`Shift+U` to download it (SHA-256 verified) and restart into the new version
right there, or toggle **auto-update on launch** in the same dialog. From a
script, `diskutility --update` does the same.

```
╭──────────────────────────────────────────────────────────────────╮
│ ✦ Disk Utility  v0.1.0   format · erase · write images  ● admin  │
╰──────────────────────────────────────────────────────────────────╯
╭ Disks (3) ─────────────────────────╮╭ Details ──────────────────╮
│ ⛨  0 CT2000T705SSD3  1.8 TB NVMe   ││ Name    SATECHI DISK      │
│    1 CT4000P3PSSD8   3.6 TB NVMe   ││ Disk    #2 · USB · RAW    │
│ ▸  2 SATECHI DISK    1.8 TB USB    ││ ...                       │
╰────────────────────────────────────╯╰───────────────────────────╯
 ↑↓ select · f format · e erase · i write image · r rescan · q quit
```

## Format presets

| Preset        | Filesystem    | Scheme | Notes                                                        |
| ------------- | ------------- | ------ | ------------------------------------------------------------ |
| Windows       | NTFS          | GPT    | journaling, permissions, >4 GB files                         |
| macOS         | exFAT         | GPT    | full R/W on macOS + Windows (APFS/HFS+ require a Mac)        |
| Linux         | ext4          | GPT    | real ext4, created through WSL 2                             |
| PlayStation 5 | exFAT         | MBR    | USB media/backup drives (game storage is formatted by PS5)   |
| Universal     | FAT32 / exFAT | MBR    | FAT32 ≤ 32 GB, exFAT above; TVs, cameras, consoles           |

## Operations

- **Format** — clean, partition (GPT/MBR), format, assign a drive letter.
- **Quick erase** — destroy the partition table (fast).
- **Secure erase** — zero-fill every sector with live speed/ETA (raw `\\.\PhysicalDriveN` writes).
- **Write image** — dd-style sector writer for `.iso` / `.img` / `.raw` / `.bin` / `.wic`,
  followed by an automatic **bit-for-bit read-back verification**.
- **Benchmark** (`b`) — sequential + random 4K speeds with a live graph.
  The read benchmark is non-destructive and safe on any disk; the full
  read/write benchmark wipes the target first. All I/O is unbuffered
  (`FILE_FLAG_NO_BUFFERING`), so numbers measure the disk, not the cache.
- **Capacity test** (`b`) — h2testw-style pattern write + verify that exposes
  counterfeit "fake capacity" flash drives. Quick mode samples the whole
  address space in minutes; full mode proves every byte.
- **Surface scan** (`b`) — non-destructive read of every sector that pinpoints
  unreadable 4 KiB blocks.
- **Health** (`h`) — SMART wear, temperature, power-on hours and error counts
  (needs an elevated terminal; USB bridges usually block passthrough).
- **Hex viewer** (`x`) — read-only sector browser: `←` `→` step, `PgUp`/`PgDn`
  jump 256 sectors, `g` goes to a sector or a byte offset (`1.5G`), and known
  structures (MBR, GPT header, NTFS/exFAT/FAT boot sectors) are labelled.
  `diskutility --hex <disk> [sector]` dumps one from the command line.
- **Backup** (`s`) — full sector image of a disk to an `.img` file, with free
  space and same-disk checks. Restore it onto any disk with `i`.
- **Clone** (`d`) — sector-for-sector disk-to-disk copy with the partition
  table written last (no mid-copy automount), followed by verification.
- **Manage** (`m`) — assign/change/remove drive letters, rename a volume,
  bring a disk online/offline, safely eject, clear the read-only flag. Nothing
  here erases data; protected disks are refused.
- **Automatic backups** (`a`) — schedule a full image of the selected disk
  every N minutes/hours, daily, weekly, monthly or yearly. The app registers a
  Windows Task Scheduler job (`DiskUtility Backup`) that runs
  `diskutility --scheduled-backup` elevated while you are logged on — nothing
  needs to stay open or autostart. The disk is matched by serial and size (disk
  numbers shift), images go to your saved backup destination (`n`) as
  `auto-<disk>-<serial>-<timestamp>.img`, and older images beyond the *keep*
  count are pruned. If the disk is not connected at run time, nothing happens
  and the log says so. When a run finishes, fails or is stopped, a Windows
  notification tells you the outcome (`"notify": false` in `config.json`
  turns it off).
- **Backups panel** (`Shift+B`) — every backup currently running, whether it
  is the scheduled job in its hidden process or a manual one in another
  diskutility window: disk, image, a progress gauge, speed, ETA, elapsed time
  and pid. Select one and press `x` to stop it. The panel also shows the
  registered schedule and any interrupted images waiting to be resumed. While
  a background backup runs, a one-line status bar with the gauge sits above
  the footer. Backups never overlap by accident: the scheduled job skips its
  run while another backup is live, and the `s` menu warns you and asks for
  confirmation before starting a second one.
- **Pause scheduled backups** (`p` in the Backups panel or in the schedule
  editor) — for 1 hour, 6 hours, 24 hours, 7 days, or until you resume them.
  The task stays registered; each run just exits without doing anything until
  the pause lifts. Handy when the NAS is full or you are about to unplug the
  disk. A backup already running is not affected.
- **Resumable backups** — an image is written as `name.img.partial` and only
  renamed to `name.img` once complete, so a `.img` file is always a finished
  backup. If a backup is stopped (`x` in the panel, Esc, a crash, a reboot), the next
  backup of the same disk into the same folder — scheduled or manual —
  continues from where it left off instead of starting over. The earlier part
  reflects the disk as it was then, so resume only makes sense for a disk you
  are not writing to in between.

## Safety

- The Windows **system/boot disk is refused by default**, at the UI layer *and* inside every worker script.
- The disk hosting the running executable is refused by default.
- Every destructive action requires **typing the disk number** in a confirmation dialog.
- Writes require an **elevated (administrator) terminal**; otherwise the app is read-only.
- **Safety override** (`u`): for experts, protections can be disabled for the
  session by typing `UNLOCK` in a warning dialog. The header turns red with a
  `⛨ PROTECTIONS OFF` badge, and acting on a protected disk then requires
  typing `DESTROY <disk number>` to confirm. Press `u` again to re-lock.
- **Internal (non-USB/SD) disks** are not blocked, but the confirmation dialog
  flags them and demands the `DESTROY <n>` phrase, and lists the volumes and
  used space on the target so a wrong pick is obvious.
- **Device identity is re-checked** right before anything is written: the
  target's serial number, size and model must still match what you confirmed.
  Disk numbers shift when drives are plugged/unplugged; if the list changed
  under you, the operation is refused and nothing is touched.
- An image that lives on the disk being written is refused.
- PowerShell is always launched by absolute path (`%SystemRoot%\System32\...`)
  with `-NoProfile` and a module path pinned to system directories, and every
  script receives only integers and quoted/sanitized strings.
- Updates (`Shift+U`, `--update`, or auto-update on launch) download only
  from this repository's GitHub releases, verify the SHA-256 against the
  release's `checksums.txt`, and check the new binary runs before swapping it
  in. The start-up update check is the TUI's only network access; disable it
  (and auto-update) with `--no-update-check` or `DISKUTILITY_NO_UPDATE_CHECK=1`.
- Scheduled backups are plain Task Scheduler jobs you can inspect or delete in
  **Task Scheduler** (`taskschd.msc`); the command line they run is visible
  there. They only read the source disk.

## Build & run

No Visual Studio needed — just the Rust toolchain (`rustup`). Build from
VS Code / Cursor (`Ctrl+Shift+B`, tasks included) or any terminal:

```powershell
cargo build --release          # → target/release/diskutility.exe
.\target\release\diskutility.exe          # TUI (run elevated for write ops)
.\target\release\diskutility.exe --list   # read-only disk listing, no TUI
```

### Releasing

Every release carries detailed patch notes, kept in [CHANGELOG.md](CHANGELOG.md).
To cut a release: bump `version` in `Cargo.toml`, add a `## vX.Y.Z — date`
section to `CHANGELOG.md` describing every change, commit, then push the
`vX.Y.Z` tag. The release workflow builds the exe, extracts that section as the
GitHub release body (GitHub's auto-generated comparison link is appended), and
fails if the section is missing. `packaging/sync-release-notes.ps1` pushes the
changelog onto already-published releases (needs `gh auth login`). A `winget`
job then submits the version to microsoft/winget-pkgs when the `WINGET_TOKEN`
secret is configured — see [packaging/winget](packaging/winget/README.md).

## Troubleshooting

Everything the app does is logged: every PowerShell script it runs, the full
stdout/stderr, exit codes, and each operation step — timestamped in
`diskutility.log` next to the exe (falls back to `%TEMP%`). Press **`c`**
anywhere (including inside a failed operation dialog) to copy the entire
session log to the clipboard, ready to paste into a bug report.

Write-protected drives: the prep step clears the read-only attribute with
`Set-Disk`, retries with `diskpart attributes disk clear readonly`, and fails
with a clear message if the enclosure has a hardware lock.

## Requirements

- Windows 10/11 (uses PowerShell Storage cmdlets + raw device I/O)
- Administrator rights for anything destructive
- WSL 2 only if you use the Linux/ext4 preset
