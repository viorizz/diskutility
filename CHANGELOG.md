# Changelog

Detailed patch notes for every release of Disk Utility. The release workflow
copies the matching `## vX.Y.Z` section of this file into the GitHub release
body, so **every release must have its section here before the tag is pushed**
(the workflow fails otherwise). GitHub's auto-generated "Full Changelog"
comparison link is appended below these notes.

## v0.4.8 — 2026-08-23

### Changed
- **Built on [utility-core](https://github.com/viorizz/utility-core)** — the shared foundation of the *Utility line. The logger, the SHA-256-verified self-updater, toast notifications, the `%APPDATA%\diskutility\config.json` load/save, the hardened PowerShell runner (absolute `powershell.exe`, pinned `PSModulePath`, CLIXML error decoding), the elevation check, and the header / footer / modal chrome now come from that crate instead of private copies. Behaviour, file locations, log format, the `--version` banner and every keybinding are unchanged; the `DISKUTILITY_NO_UPDATE_CHECK` / `--no-update-check` opt-out still works.
- Nothing else: no new features in this release, it exists so DiskUtility, AccountUtility and MouseUtility evolve together.

## v0.4.7 — 2026-08-23

### Added
- **Manage menu (`m`)** — quick, non-destructive actions for the selected disk, each run with the same disk-identity check as every other operation and reported in the progress dialog:
  - **Assign or change a drive letter** for any partition with a filesystem. The prompt lists the letters that are actually free (it also excludes mapped network drives, which `Get-Volume` would miss) and refuses A–C and letters in use.
  - **Remove a drive letter** — the volume is unmounted but its data is untouched; assign a letter again to use it.
  - **Rename a volume label** (characters invalid in labels are stripped; 32-character cap, FAT32's 11-character limit is left to Windows to report).
  - **Bring online / take offline** — an offline disk is left alone by Windows (no automount, no indexing), which is the safe state for a drive you are about to image or hand to another machine.
  - **Safely eject** — flushes and ejects through the shell's *Eject* verb (the same thing as the tray icon) and tells you when a program is still holding the drive open. A disk without any drive letter is taken offline instead, which also flushes it.
  - **Clear the read-only flag** (`Set-Disk -IsReadOnly $false`), with a pointer to the enclosure diagnostics when the device refuses.
  The system/boot disk and the disk hosting the app are refused unless the safety override is on; the menu needs an elevated terminal.

- **Notification when a scheduled backup finishes** — the headless `--scheduled-backup` run now raises a Windows toast ("Disk Utility · Scheduled backup finished" with the image path and size, or "Scheduled backup FAILED" with the reason — disk not connected, folder unreachable, not enough space, write error…) so you no longer have to open `diskutility.log` to know whether last night's backup happened. Uses the built-in WinRT toast API through PowerShell; no extra dependency. Set `"notify": false` in `config.json` to silence it (default on). A failed toast is only logged and never affects the backup result.

- **Backups panel (`Shift+B`)** — shows every backup in progress, across processes: the scheduled job (which runs in an invisible Task Scheduler process, so until now the app had no idea it existed) and manual backups started in other diskutility windows. Each row has kind, disk, image path, a progress gauge, speed/ETA, start time, elapsed time and pid; `↑↓` selects one and `x` stops it after a confirmation. Below that: the registered schedule (disk, cadence, destination, keep count, whether the task is actually registered) and the interrupted images that the next backup would resume, with how much of each is done. While a background backup runs, a one-line **status bar** with the same gauge sits above the footer (`Shift+X` also opens the panel).
- **How it works** — every backup publishes `%APPDATA%\diskutility\jobs\<pid>.json` (rewritten every half second, heartbeated every 2 s, removed when the job ends) and polls for `<pid>.cancel`. Stopping a job from the panel drops that marker; the owning process notices within a heartbeat and aborts like Esc would — the partial image is kept for resuming, the disk is only ever read. A record whose heartbeat is older than 15 s belongs to a dead process and is discarded. This replaces the single `scheduled-backup.json` from earlier in this release.
- **Resumable backups** — images are now written to `<name>.img.partial` with a `<name>.img.partial.json` sidecar recording the source disk's name, serial and size, and renamed to `<name>.img` only after the final flush. Consequences: a `.img` is always complete (previously a killed run left a truncated `.img` that `i` would happily restore and the retention pruner counted as a backup); and when a backup of the same disk into the same folder starts again — the next scheduled run, or `s` by hand — it finds the partial, trims any torn last chunk, seeks both the disk and the file to that offset and continues, with speed/ETA computed from this session only. Works after Esc, `x` in the Backups panel, a crash, a reboot or a full destination. The log states the resume explicitly, plus a reminder that the earlier part reflects the disk as it was when first read. A partial is only ever matched by serial + size (or name + size for disks without a serial); the scheduler's free-space check counts just the remainder.
- **No accidental overlapping backups** — the scheduled runner skips its run while any other backup is live ("a manual backup is already running (pid …, 42%) — this run was skipped"; Task Scheduler already prevented two instances of the job itself, but a manual run could previously read the same disk twice concurrently). In the TUI, the `s` menu shows an inline warning listing the running backups, and choosing a destination then opens a confirmation — `y` starts anyway (logged), `b` jumps to the Backups panel, Esc aborts — so two backups only ever run together on purpose.

- **Pause scheduled backups** — `p` in the Backups panel or in the schedule editor (`a`) opens a menu: pause for 1 hour, 6 hours, 24 hours, 7 days, or until you resume it; when paused, the first entry is *Resume now*. The pause is stored as `paused_until` in `config.json`; the Task Scheduler job stays registered and `--scheduled-backup` simply logs "skipped — paused until …" and exits (no notification) until the pause lifts by itself or is cleared. No elevation is needed to pause or resume, it survives reboots, and a backup already in progress keeps running. The schedule editor and the Backups panel both show the paused state.

### Changed
- Footer shows `m  manage`; Help gained a line for it, plus a `Shift+B` line.

### Fixed
- **`--update` / `Shift+U` failed with "checksums.txt has no entry for diskutility.exe"** even though the release's `checksums.txt` was correct. GitHub serves release assets as `application/octet-stream`, so PowerShell's `Invoke-WebRequest ... .Content` returned the file as a `byte[]`; written to the pipe, that became one integer per line and the SHA-256 parser found no `diskutility.exe` entry. The updater now decodes the bytes as UTF-8 before parsing. `install.ps1` had the same latent bug and is fixed the same way — use it (or `winget upgrade`) to get past v0.4.6, since the old binary cannot self-update.

## v0.4.6 — 2026-08-22

### Added
- **Automatic backups on a schedule (`a`)** — select a disk and press `a` to open the schedule editor. Rows are navigated with Up/Down and changed with Left/Right: frequency (every N minutes, every N hours, daily, weekly, monthly, yearly), start hour and minute (5-minute steps), weekday, day of month (1–28), month, and how many images to keep (1–30, or all). The summary line spells the result out ("weekly on Sunday at 03:00"). Enter saves the schedule to `config.json` and registers a **Windows Task Scheduler** job named `DiskUtility Backup` with `schtasks.exe` (run level *highest*, interactive logon — so it can reach your NAS without storing a password); `x` removes both. The task runs `diskutility --scheduled-backup` whether or not the app is open, so nothing has to autostart or stay resident. Requires an elevated terminal and a saved backup destination (`n`).
- **`diskutility --scheduled-backup`** — the headless entry point the task runs. It finds the scheduled disk by serial number and size (never by disk number, which shifts as drives come and go), writes `auto-<disk>-<serial>-<YYYYMMDD-HHMMSS>.img` into the scheduled folder using the same verified backup engine as `s`, then deletes the oldest images beyond the keep count (it also prunes first if the folder is short on space). A disk that is not connected at run time is reported and skipped. Everything goes to `diskutility.log`, and progress prints to the console if you run it by hand.
- **In-app update with `Shift+U`** — when the header shows a newer release, `Shift+U` opens an Update dialog: `y` downloads it, verifies the SHA-256 against the release's `checksums.txt`, checks the binary runs, swaps it in, and on Enter the app quits and relaunches the new version in the same terminal with the same arguments. `n`/Esc declines. Progress steps are shown live in the dialog and failures leave the current version untouched.
- **Auto-update on launch** — press `a` in the Update dialog to toggle it (stored as `auto_update` in `config.json`). When on, the app checks for a release before the TUI starts, installs it with the same verification, and restarts itself. It honours `--no-update-check` / `DISKUTILITY_NO_UPDATE_CHECK`.
- **winget packaging** — `packaging/winget/make-manifests.ps1` generates validated manifests for any published release (pulls the SHA-256 from its `checksums.txt`), `packaging/winget/submit.ps1` submits or updates `viorizz.diskutility` through `wingetcreate`, and a new `winget` job in the release workflow submits every future version automatically once the `WINGET_TOKEN` repository secret is set (it is skipped without it). The first submission is a one-time manual step documented in `packaging/winget/README.md`.

### Changed
- **Header banner** now says `⬆ vX.Y.Z available — Shift+U to update` instead of pointing at the command line.
- **Footer and help** list `a  auto` and `Shift+U`; the Help dialog grew two rows.
- `diskutility --update` prints each step (check, download, verify, swap) as it goes.
- The startup update check's opt-out logic is shared between the TUI and the launch-time auto-update.

## v0.4.5 — 2026-08-22

### Added
- **Saved backup destination** — press `n` (from the disk list or inside the new backup menu) to set a default folder for backup images, such as a mapped network drive or UNC share. The path is validated before it is accepted: it must be absolute (`Z:\backups` or `\\server\share\backups`), the folder must exist, and the app writes and deletes a small probe file to prove the location is writable before trusting it with a multi-hour backup. Entering an empty path clears the saved destination.
- **Persistent settings file** — settings are now stored in `%APPDATA%\diskutility\config.json` (pretty-printed JSON, currently a single optional `backup_dir` key). The file and folder are created on first save; a corrupt or missing file falls back to defaults and is noted in the session log.
- **Backup destination menu** — pressing `s` now opens a "Backup — where should the image go?" picker instead of jumping straight to a path prompt. It lists your saved destination (if set), your home folder, and "Custom path…", showing the free space next to each folder. Navigate with Up/Down or `j`/`k`, choose with Enter, press `n` to set or change the saved destination, Esc to cancel.
- **Image size and free-space check in the backup prompt** — the backup path prompt and menu now show the size of the image that will be produced (the raw sector size of the selected disk). As you type a destination path, the prompt shows the free space there, colored green if the image fits and red with a "will not fit" warning if it does not.
- **Mapped network drives resolved to UNC paths** — when the saved destination starts with a mapped drive letter, the app looks up the drive's UNC target in `HKCU\Network` and stores/opens that form instead (for example `Z:\backups` becomes `\\nas\share\backups`). This is needed because mapped drives are usually invisible to an elevated (UAC) process; the resolution is recorded in the session log. If a drive-letter folder cannot be reached while running elevated, the error message suggests using the `\\server\share` form.

### Changed
- **Suggested backup filename now includes the date** — the default image name is `disk<N>-<name>-<YYYYMMDD>.img` (previously `disk<N>-<name>.img`), built under whichever folder you picked in the backup menu. Choosing "Custom path…" starts with an empty prompt.
- **Footer and help updated** — the footer shows the new `n  backup dir` shortcut, and the Help dialog (one line taller) documents `n` as "set a network drive / folder as the default backup destination".
- **Input prompts grow to fit** — the path prompt dialog expands from 7 to 9 rows when the image-size/free-space line is shown, and its text now wraps instead of being clipped.

## v0.4.4 — 2026-08-22

### Fixed
- **Clone no longer overwrites the end of the cloned data on a larger target** — the "clear the target's old end-of-disk GPT area" step introduced in v0.4.3 zeroed the last 1 MiB of the *target*. When the target was less than 1 MiB larger than the source, that range reached back into the freshly cloned data and wiped the clone's own backup GPT header. The cleared range now starts at `max(target.size − 1 MiB, source.size)` (rounded up to a sector), and the step is skipped entirely when there is nothing beyond the source's size to clear.
- **Identity check no longer rejects disks without a friendly name** — the pre-operation identity check (added in v0.4.3 for backup/clone sources) compared the live `FriendlyName` against the name shown in the list. Disks that report no name are displayed with a placeholder, so the comparison always failed and those disks could not be backed up or cloned. The name is now only compared when the disk actually has one; size and serial are still checked.

## v0.4.3 — 2026-08-22

Hardening pass over the backup and clone features introduced in v0.4.2, from a code review.

### Added
- **Source identity check before backup and clone** — before reading a single sector, the app re-queries the source disk with `Get-Disk` and verifies that its size, friendly name and serial number still match the device you selected. Disk numbers shift when drives are plugged or unplugged; if the device at that number changed, the operation aborts with a message asking you to rescan (`r`) and retry. This is a read-only check — it never runs the mutating prep script on a source disk.
- **FAT32 destination check for backups** — a backup image of a disk ≥ 4 GiB is refused up front when the destination volume is FAT32 (queried via `GetVolumeInformationW`), since FAT32 cannot hold files over 4 GiB and the write would otherwise fail at exactly that point. The error suggests an NTFS or exFAT destination.
- **Clone clears the stale backup GPT on a larger target** — `Clear-Disk` only wipes the primary partition table; a target larger than the source keeps its *old* backup GPT header and entries in its last sectors. The cloned primary header points its backup at the source's last LBA, so partitioning tools would see a stale table at end-of-disk and offer to "restore" it. After the copy, the clone now zeroes the last 1 MiB of the target (see v0.4.4 for a fix to this range).

### Changed
- **Backup image files are never overwritten** — the image is opened with `create_new`, so an existing file at that path is an error at the OS level, not just a UI check.
- **Backup writes use `FILE_FLAG_SEQUENTIAL_SCAN`** — multi-TB image streams no longer bloat the Windows cache manager while being written.
- **"Live snapshot" notice only when volumes are actually mounted** — the reminder to close programs writing to the source disk now triggers only when a partition has a drive letter, not merely when partitions exist.

## v0.4.2 — 2026-08-22

### Added
- **Disk backup to image file (`s`)** — new action that reads the selected disk sector-for-sector through an uncached direct handle and writes it to an `.img` file. Progress is reported every 250 ms; the disk itself is never written to. On success the message reminds you that the image can be restored onto any disk of at least that size with `i` (write image).
- **Backup path prompt with a sensible default** — pressing `s` opens an input dialog pre-filled with `%USERPROFILE%\disk<N>-<name>.img`, where `<name>` is the disk name reduced to alphanumerics, `-` and `_` (max 24 chars). Surrounding quotes are stripped and `.img` is appended automatically if no extension is given.
- **Backup destination validation** — the prompt refuses to proceed if the path is empty or not a full path, if the parent folder does not exist, or if the file already exists (existing files are never overwritten). It also checks free space on the destination volume (via `GetDiskFreeSpaceExW`) and rejects the backup if there is less free space than the disk's size, telling you how much is available versus needed.
- **Backup cannot land on the disk being imaged** — the destination's drive letter is resolved (after canonicalizing the path) and compared against the selected disk's partitions; if it matches, the backup is refused with an explanation, since an image written onto its own source would never be consistent.
- **Disk-to-disk clone (`d`)** — new action that copies the selected disk sector-for-sector onto another disk. Pressing `d` asks for the target disk number; a confirmation dialog then spells out exactly which disk will be overwritten (`Overwrite THIS disk with a sector-for-sector clone of disk N (name · size), then verify`).
- **Clone target validation** — the target must be an existing disk number, must not be the same device as the source (matched by number/serial/size/name, or by identical serial and size), and must be at least as large as the source ("shrinking clones are not supported").
- **Clone verification pass** — after the copy, the source and target are both re-read and compared chunk by chunk. A mismatch aborts with `VERIFICATION FAILED at offset …` and the byte offset of the first difference, with a hint that a live source that changed mid-copy or a faulty target is the likely cause. Cancelling during verification reports that the clone itself was already fully written.
- **Clone brings the target online afterwards** — once verified, the app runs `Set-Disk -IsOffline $false` and `Update-HostStorageCache` on the target. The success message notes that Windows may keep the clone offline if its disk signature collides with the source, and suggests unplugging the source or onlining it in Disk Management.
- **Clone target goes through the existing prep pipeline** — the target is wiped via the same prep script as other destructive actions (identity check, write-protection check, wipe) before any data is written; the source is only ever opened for reading.
- **Live-snapshot notice** — both backup and clone log a note when the source has mounted volumes, reminding you to close programs writing to it for a consistent image.
- **Footer and help updates** — the footer now lists `s backup` and `d clone` (with `i write image` shortened to `i write` and `c copy log` to `c log` to make room), and the `?` help screen documents both new keys. README gains entries for Backup and Clone (plus Surface scan and Health, which were missing).

### Changed
- **Partition table is written last when writing images (`i`)** — the first chunk of the image (which holds the partition table) is now held back and written after the rest of the image. Previously, once a valid table landed at sector 0, Windows could automount the new volumes mid-write and then refuse further raw writes to them. A `Writing the partition table (first sectors)…` log line marks this final step. Clone uses the same ordering.
- **Input dialog is wider** (68 to 78 columns) to fit the longer backup/clone prompts, and the help modal grew two rows for the new keys.

### Fixed
- **Confirmation re-check now verifies the confirmed target, not the highlighted row** — after confirming a destructive action, the app previously compared the snapshot against whichever disk was currently selected. It now looks up the disk by number in the live list and checks it is still the same device, which is what makes clone safe when the confirmed target is not the selected row. Clone additionally re-validates the source disk the same way before starting, aborting with "nothing was touched" if either disk changed.

### Security
- **Clone targets are subject to the same protection guards as the selected disk** — a new target guard refuses to clone onto a protected disk (system/boot etc.) unless the `u` safety override is active, and requires an elevated terminal. Backup and clone both require administrator rights up front.
- **Cancelled backup cleans up** — cancelling a backup deletes the partial image file rather than leaving a truncated `.img` behind; cancelling a clone warns that the target holds an incomplete clone and should be erased before use.

## v0.4.1 — 2026-08-22

### Added
- **Drive health panel (`h`)** — select a disk and press `h` to open a Health dialog showing Windows' health status, media type (with spindle RPM for HDDs) and usage, plus SMART/reliability counters: wear (% of rated endurance used), current and max recorded temperature, power-on hours (with an approximate years figure), and total read/write error counts. Values are colour-coded: wear turns yellow at 70 % and red at 90 %, temperature turns yellow at 55 °C and red at 70 °C, any non-zero error count is highlighted. The panel is listed in the footer and the `?` help screen; `c` copies the session log from inside it.
- **Health panel explains missing counters** — if SMART counters are unavailable, the dialog tells you why: either the app is not running elevated (restart as administrator), or the device blocks SMART passthrough (typical for USB enclosures — connect via SATA/NVMe instead).
- **`diskutility --health <disk number>` CLI flag** — prints the same health summary and SMART counters to the console and exits, for scripting or quick checks without opening the TUI.
- **Surface scan (safe)** — new second entry in the `b` Test menu. Reads every sector of the disk non-destructively and, when a chunk fails, re-probes it 4 KiB at a time to pinpoint the exact unreadable blocks. Reports PASSED with 0 bad blocks and elapsed time, or lists the count and first/last bad block offsets with a back-up warning. Aborts early after more than 2000 unreadable blocks with a "this drive is failing" message. Runs without a confirmation dialog (like the read benchmark), is cancellable, and works on protected disks.
- **Confirmation dialog now shows what's on the target** — a new "Contains" line lists each recognised volume on the disk with its drive letter, filesystem and used space (or "no recognised volumes"), so picking the wrong disk is obvious before you type the phrase.
- **`--no-update-check` flag / `DISKUTILITY_NO_UPDATE_CHECK=1` env var** — disables the start-up update check, which is the TUI's only network access. The opt-out is recorded in the log.
- **Log rotation** — `diskutility.log` is now capped at 5 MiB; once it exceeds that it is parked as `diskutility.log.1` (replacing any previous backup) and a fresh log is started.
- **CI now runs `cargo test`** alongside clippy, build and the smoke test; new unit tests cover the updater's URL allow-list and checksum verification.

### Changed
- **Internal (non-removable) disks get extra friction** — disks whose bus is not USB, SD, MMC, 1394 or virtual are no longer treated like a USB stick. They are not blocked, but the confirmation dialog shows a bold "⚠ INTERNAL DISK: not a removable device (bus …)" warning and requires the long `DESTROY <n>` phrase instead of just the disk number.
- **Test menu reordered** — with the surface scan inserted, the `b` menu now has five entries: Read benchmark, Surface scan, Full benchmark, Capacity (quick), Capacity (full).
- **Help screen updated** — `b` is described as "benchmark, surface scan & capacity tests" and a new `h` entry for drive health was added.
- **Update success message** now states "(SHA-256 verified)".

### Fixed
- **Destructive actions act on the disk you confirmed, not the currently highlighted row** — the target disk is snapshotted when you enter a menu, and the confirmation dialog and image-size check use that snapshot. If a rescan (e.g. a drive being unplugged) changes the list while the dialog is open, the operation is refused with "the disk list changed since you selected disk N — nothing was touched" instead of running on whatever now occupies that row.
- **Image-on-target refusal** — writing an image that is stored on the very disk being written is rejected up front ("it would be destroyed while being written"), since the prep step would wipe the image's volume mid-write. UNC share paths are allowed; volume-GUID paths that can't be placed are refused rather than guessed.
- **Double-check of protection and elevation at launch time** — protection reasons (not only boot/system flags) and administrator rights are re-verified right before an operation starts, rather than relying solely on the earlier menu guard.
- **Clipboard temp file cleanup** — the temporary file used by `c` (copy log) is now deleted after the copy.

### Security
- **Device identity re-check before any write** — every destructive operation's prep script now compares the live disk's serial number, size and friendly name against the snapshot you confirmed and throws "disk N is no longer the device you confirmed … rescan (r) and retry" if any differ. This protects against Windows renumbering disks when drives are plugged or unplugged between selection and execution.
- **PowerShell is launched by absolute path** (`%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`) instead of by name, so a `powershell.exe` dropped next to the (user-writable) app binary can't be picked up while running elevated. The script prelude also pins `PSModulePath` to system directories so a rogue module in the user's Documents folder can't shadow the Storage cmdlets.
- **All string values passed into PowerShell are now single-quote-escaped**, including file paths for the clipboard helper, updater URLs and disk serial/name in the identity check.
- **Self-updater hardening (`--update`)** — downloads are only accepted from `https://github.com/<this repo>/releases/download/` (URLs containing quotes or whitespace are rejected), TLS 1.2 is forced, the release's `checksums.txt` is fetched and the SHA-256 of the download must match its `diskutility.exe` entry, the file must be a `MZ` PE image at least 200 KB, and the new binary is executed with `--version` and must identify itself as `diskutility v…` before it replaces the running exe. Any failure deletes the staged download and aborts.
- **`install.ps1` hardening** — the installer now also requires `checksums.txt` in the release, verifies both asset URLs point at this repo's GitHub releases, downloads to a staged `.new` file, checks its SHA-256 against `checksums.txt`, and only then moves it into place; a mismatch deletes the download and aborts with a "corrupted or tampered" error.
- **README documents the safety model** — new bullets describe internal-disk warnings, the identity re-check, the image-on-target refusal, PowerShell launch hardening, and the verified updater / opt-out flags.

## v0.4.0 — 2026-08-22

### Added
- **New "Test disk" menu on `b`** — press `b` with a disk selected to open a four-option menu: Read benchmark, Full benchmark, Quick capacity test, and Full capacity test. Navigate with `↑↓`/`jk`, confirm with Enter. The footer and help screen (`?`) now list the `b` key.
- **Read benchmark (non-destructive)** — measures sequential read over the first 1 GiB (or the whole disk if smaller) in 4 MiB chunks, then runs a 5-second random 4 KiB read test, reporting MB/s and IOPS. It modifies nothing, so it starts immediately with no confirmation dialog and is allowed even on protected/system disks (administrator rights are still required).
- **Full read/write benchmark (destructive)** — wipes the disk via the standard prep step, then measures sequential write, sequential read, random 4K write (5 s), and random 4K read (5 s). The final summary reports all four figures; the disk is left blank (RAW) afterwards. Requires the usual typed confirmation.
- **Quick capacity test (destructive)** — h2testw-style counterfeit-flash detector. Writes 1 MiB deterministic pattern blocks at roughly 256 evenly spaced points across the entire address space (always including the very last MiB), flushes, reopens the disk, and reads every sample back. Because fake-capacity drives wrap high addresses onto low ones, a mismatch surfaces within minutes; the failure message names the offset where data did not survive. Refuses disks smaller than 4 MiB.
- **Full capacity test (destructive)** — writes a deterministic pattern over every byte of the disk, then reads the entire disk back and compares. Reports the exact first mismatching offset as the estimated real usable capacity. Takes two full-disk passes, so the confirm prompt warns it may take hours. The disk is left blank (RAW) on success; cancelling mid-way warns that partial test patterns remain on the disk.
- **Live speed graph in the progress dialog** — the progress modal now has a 3-line sparkline that plots throughput samples (updated every 250 ms, last 240 samples kept) during benchmarks, capacity tests, secure erase, and image writes.
- **Phase labels in progress text** — long-running operations now prefix their progress line with the current phase (e.g. `writing:`, `verifying:`, `seq read:`, `4K write:`) alongside bytes done, speed, and ETA.

### Changed
- **Write image now verifies after writing** — after the sector-by-sector write and flush, the image is re-read from the source file and compared bit-for-bit against the disk through a fresh unbuffered handle. A mismatch aborts with `VERIFICATION FAILED at offset …` and suggests running the disk tests (`b`). The confirm dialog text, help entry, and success message all now mention verification.
- **Cancelling during image verification** reports that the image itself was already fully written, so you know the write completed even if the check did not.
- **All benchmark and capacity-test I/O bypasses the Windows cache** — devices are opened with `FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH` using 4 KiB-aligned buffers, so reported speeds reflect the physical disk rather than RAM caching.
- **Cancellability is now opt-out rather than opt-in** — every operation except Format and Quick erase can be cancelled from the progress dialog (previously only Secure erase and Write image could). This covers the new benchmarks and capacity tests.
- **README updated** to document the read-back verification on image writes and the new Benchmark / Capacity test features.

### Security
- **Destructive tests go through the same guard path as erase** — Full benchmark and both capacity tests require the protected-disk guard check and the typed confirmation dialog, and they run the existing prep/wipe routine that honours the safety-override (`u`) setting. Only the read-only benchmark skips confirmation.

## v0.3.0 — 2026-08-22

First public release.

### Added
- **Terminal UI** built on ratatui: disk list with type/size/bus, details pane for the selected disk (name, number, bus, partition style, partitions and letters), header showing version and elevation status (`● admin`), and a footer with the key map. `↑↓`/`jk` select, `r` rescans, `?` shows help, `q` quits.
- **Format (`f`)** — clean, partition (GPT or MBR), format and assign a drive letter using one of five presets: Windows (NTFS/GPT), macOS (exFAT/GPT), Linux (real ext4 created through WSL 2, GPT), PlayStation 5 (exFAT/MBR) and Universal (FAT32 ≤ 32 GB, exFAT above, MBR).
- **Quick erase (`e`)** — destroys the partition table; takes seconds.
- **Secure erase** — zero-fills every sector through raw `\\.\PhysicalDriveN` writes, with live speed and ETA, cancellable from the progress dialog.
- **Write image (`i`)** — dd-style sector writer for `.iso` / `.img` / `.raw` / `.bin` / `.wic` files, for creating bootable USB media; cancellable.
- **Non-interactive listing** — `diskutility --list` / `-l` prints the disk table without starting the TUI; `--version` / `-V` prints the version and build stamp.
- **Self-update** — the TUI shows a banner when a newer GitHub release exists; `diskutility --update` downloads and installs it.
- **One-line installer** — `install.ps1` (`irm …/install.ps1 | iex`) downloads the latest release, and the repo ships a winget manifest under `packaging/winget`.
- **Session log** — every PowerShell script the app runs, its full stdout/stderr and exit code, and each operation step are timestamped into `diskutility.log` next to the executable (falling back to `%TEMP%`). Press `c` anywhere — including inside a failed-operation dialog — to copy the whole session log to the clipboard.
- **Write-protected drive handling** — the prep step clears the read-only attribute with `Set-Disk`, retries with `diskpart attributes disk clear readonly`, and fails with a clear message if the enclosure has a hardware lock.

### Security
- The Windows **system/boot disk is refused by default**, both in the UI and inside every worker script.
- The disk hosting the running executable is refused by default.
- Every destructive action requires **typing the disk number** in a confirmation dialog.
- Writes require an **elevated (administrator) terminal**; without it the app is read-only.
- **Safety override (`u`)** — protections can be disabled for the session by typing `UNLOCK` in a warning dialog. The header turns red with a `⛨ PROTECTIONS OFF` badge, and acting on a protected disk then requires typing `DESTROY <disk number>`. Press `u` again to re-lock.
