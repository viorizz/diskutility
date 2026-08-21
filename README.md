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

**From source:**

```powershell
cargo install --git https://github.com/viorizz/diskutility
```

**Stay up to date:** the TUI shows a banner when a new release is out — apply it with:

```powershell
diskutility --update
```

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

## Safety

- The Windows **system/boot disk is refused by default**, at the UI layer *and* inside every worker script.
- The disk hosting the running executable is refused by default.
- Every destructive action requires **typing the disk number** in a confirmation dialog.
- Writes require an **elevated (administrator) terminal**; otherwise the app is read-only.
- **Safety override** (`u`): for experts, protections can be disabled for the
  session by typing `UNLOCK` in a warning dialog. The header turns red with a
  `⛨ PROTECTIONS OFF` badge, and acting on a protected disk then requires
  typing `DESTROY <disk number>` to confirm. Press `u` again to re-lock.

## Build & run

No Visual Studio needed — just the Rust toolchain (`rustup`). Build from
VS Code / Cursor (`Ctrl+Shift+B`, tasks included) or any terminal:

```powershell
cargo build --release          # → target/release/diskutility.exe
.\target\release\diskutility.exe          # TUI (run elevated for write ops)
.\target\release\diskutility.exe --list   # read-only disk listing, no TUI
```

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
