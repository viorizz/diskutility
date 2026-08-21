//! Direct-I/O test engine: benchmarks and capacity verification.
//! All device access uses FILE_FLAG_NO_BUFFERING | WRITE_THROUGH so results
//! measure the disk, not the Windows cache. That requires sector-aligned
//! buffers, offsets, and lengths — hence AlignedBuf and the 4 KiB granularity.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::app::AppEvent;
use crate::disks::{human, Disk};
use crate::ops::{self, OpEvent};

const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
const SEQ_CHUNK: usize = 4 * 1024 * 1024;
const SEQ_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB per sequential pass
const RAND_SECS: f64 = 5.0;
const SAMPLE_MIB: usize = 1024 * 1024;

// ---------------------------------------------------------------------------
// Sector-aligned buffer (NO_BUFFERING requires 4 KiB alignment)
// ---------------------------------------------------------------------------

pub struct AlignedBuf {
    ptr: *mut u8,
    layout: std::alloc::Layout,
    len: usize,
}

unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    pub fn new(len: usize) -> Self {
        let layout = std::alloc::Layout::from_size_align(len, 4096).expect("bad layout");
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "allocation failed");
        Self { ptr, layout, len }
    }

    pub fn as_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    pub fn as_ref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe { std::alloc::dealloc(self.ptr, self.layout) }
    }
}

// ---------------------------------------------------------------------------
// Deterministic pattern generator (splitmix64) — the seed IS the disk offset,
// so verification can regenerate the expected data without storing anything.
// ---------------------------------------------------------------------------

fn splitmix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn fill_pattern(buf: &mut [u8], seed: u64) {
    let mut x = seed ^ 0xD15C_D15C_D15C_D15C;
    for chunk in buf.chunks_mut(8) {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let b = splitmix(x).to_le_bytes();
        chunk.copy_from_slice(&b[..chunk.len()]);
    }
}

// ---------------------------------------------------------------------------
// Device access
// ---------------------------------------------------------------------------

pub fn open_direct(n: u32, write: bool) -> Result<File, String> {
    let mut opts = OpenOptions::new();
    opts.read(true)
        .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH);
    if write {
        opts.write(true);
    }
    opts.open(format!(r"\\.\PHYSICALDRIVE{n}")).map_err(|e| {
        format!(r"cannot open \\.\PHYSICALDRIVE{n} (direct): {e} — administrator required")
    })
}

fn send(tx: &Sender<AppEvent>, ev: OpEvent) {
    let _ = tx.send(AppEvent::Op(ev));
}

fn log(tx: &Sender<AppEvent>, msg: impl Into<String>) {
    send(tx, OpEvent::Log(msg.into()));
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Passes
// ---------------------------------------------------------------------------

/// Sequential pass over the first min(1 GiB, disk) bytes. Returns avg bytes/s.
fn seq_pass(
    tx: &Sender<AppEvent>,
    f: &mut File,
    disk_size: u64,
    write: bool,
    label: &str,
    cancel: &AtomicBool,
) -> Result<f64, String> {
    let total = SEQ_BYTES.min(disk_size / 4096 * 4096);
    let mut buf = AlignedBuf::new(SEQ_CHUNK);
    if write {
        fill_pattern(buf.as_mut(), 0xBE9C);
    }
    f.seek(SeekFrom::Start(0)).map_err(|e| format!("seek: {e}"))?;
    let start = Instant::now();
    let mut done = 0u64;
    let mut win_start = Instant::now();
    let mut win_bytes = 0u64;
    while done < total {
        if cancelled(cancel) {
            return Err("cancelled".into());
        }
        let want = (((total - done).min(SEQ_CHUNK as u64)) as usize) / 4096 * 4096;
        if want == 0 {
            break;
        }
        if write {
            f.write_all(&buf.as_ref()[..want]).map_err(|e| format!("{label}: {e}"))?;
        } else {
            f.read_exact(&mut buf.as_mut()[..want]).map_err(|e| format!("{label}: {e}"))?;
        }
        done += want as u64;
        win_bytes += want as u64;
        if win_start.elapsed() >= Duration::from_millis(250) {
            let inst = win_bytes as f64 / win_start.elapsed().as_secs_f64();
            send(tx, OpEvent::Sample(inst as u64));
            send(
                tx,
                OpEvent::Progress(
                    done as f64 / total as f64,
                    format!("{label}: {}/s · {} / {}", human(inst as u64), human(done), human(total)),
                ),
            );
            win_bytes = 0;
            win_start = Instant::now();
        }
    }
    Ok(done as f64 / start.elapsed().as_secs_f64().max(0.001))
}

/// Random 4 KiB pass for RAND_SECS seconds. Returns (IOPS, bytes/s).
fn rand_pass(
    tx: &Sender<AppEvent>,
    f: &mut File,
    disk_size: u64,
    write: bool,
    label: &str,
    cancel: &AtomicBool,
) -> Result<(f64, f64), String> {
    let blocks = disk_size / 4096;
    if blocks < 2 {
        return Err("disk too small for random test".into());
    }
    let mut buf = AlignedBuf::new(4096);
    if write {
        fill_pattern(buf.as_mut(), 0x4B4B);
    }
    let start = Instant::now();
    let mut count = 0u64;
    let mut win_start = Instant::now();
    let mut win_count = 0u64;
    let mut x = 0x1234_5678_9ABC_DEF0u64;
    loop {
        let elapsed = start.elapsed().as_secs_f64();
        if elapsed >= RAND_SECS {
            break;
        }
        if count.is_multiple_of(64) && cancelled(cancel) {
            return Err("cancelled".into());
        }
        x = splitmix(x);
        let off = (x % (blocks - 1)) * 4096;
        f.seek(SeekFrom::Start(off)).map_err(|e| format!("seek: {e}"))?;
        if write {
            f.write_all(buf.as_ref()).map_err(|e| format!("{label}: {e}"))?;
        } else {
            f.read_exact(buf.as_mut()).map_err(|e| format!("{label}: {e}"))?;
        }
        count += 1;
        win_count += 1;
        if win_start.elapsed() >= Duration::from_millis(250) {
            let iops = win_count as f64 / win_start.elapsed().as_secs_f64();
            send(tx, OpEvent::Sample((iops * 4096.0) as u64));
            send(
                tx,
                OpEvent::Progress(
                    (elapsed / RAND_SECS).min(1.0),
                    format!("{label}: {iops:.0} IOPS · {}/s", human((iops * 4096.0) as u64)),
                ),
            );
            win_count = 0;
            win_start = Instant::now();
        }
    }
    let secs = start.elapsed().as_secs_f64().max(0.001);
    Ok((count as f64 / secs, count as f64 * 4096.0 / secs))
}

// ---------------------------------------------------------------------------
// Spawners
// ---------------------------------------------------------------------------

pub fn spawn_read_bench(tx: Sender<AppEvent>, disk: Disk, cancel: Arc<AtomicBool>) {
    thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            log(&tx, "Non-destructive read benchmark — no data is modified.");
            let mut f = open_direct(disk.number, false)?;
            log(&tx, format!("Sequential read ({})…", human(SEQ_BYTES.min(disk.size))));
            let seq = seq_pass(&tx, &mut f, disk.size, false, "seq read", &cancel)?;
            log(&tx, format!("Sequential read: {}/s", human(seq as u64)));
            log(&tx, "Random 4K read (5 s)…");
            let (iops, bps) = rand_pass(&tx, &mut f, disk.size, false, "4K read", &cancel)?;
            log(&tx, format!("Random 4K read: {iops:.0} IOPS ({}/s)", human(bps as u64)));
            Ok(format!(
                "Read benchmark — sequential {}/s · random 4K {iops:.0} IOPS ({}/s)",
                human(seq as u64),
                human(bps as u64)
            ))
        })();
        send(&tx, OpEvent::Done(result));
    });
}

pub fn spawn_full_bench(
    tx: Sender<AppEvent>,
    disk: Disk,
    cancel: Arc<AtomicBool>,
    allow_protected: bool,
) {
    thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            log(&tx, "Full benchmark — wiping disk first (all data destroyed).");
            ops::run_prep(&tx, disk.number, allow_protected)?;
            let mut f = open_direct(disk.number, true)?;
            log(&tx, "Sequential write…");
            let sw = seq_pass(&tx, &mut f, disk.size, true, "seq write", &cancel)?;
            log(&tx, format!("Sequential write: {}/s", human(sw as u64)));
            log(&tx, "Sequential read…");
            let sr = seq_pass(&tx, &mut f, disk.size, false, "seq read", &cancel)?;
            log(&tx, format!("Sequential read: {}/s", human(sr as u64)));
            log(&tx, "Random 4K write (5 s)…");
            let (wi, wb) = rand_pass(&tx, &mut f, disk.size, true, "4K write", &cancel)?;
            log(&tx, format!("Random 4K write: {wi:.0} IOPS ({}/s)", human(wb as u64)));
            log(&tx, "Random 4K read (5 s)…");
            let (ri, rb) = rand_pass(&tx, &mut f, disk.size, false, "4K read", &cancel)?;
            log(&tx, format!("Random 4K read: {ri:.0} IOPS ({}/s)", human(rb as u64)));
            Ok(format!(
                "Benchmark — seq R {}/s, W {}/s · 4K R {ri:.0} IOPS, W {wi:.0} IOPS. Disk left blank (RAW).",
                human(sr as u64),
                human(sw as u64)
            ))
        })();
        send(&tx, OpEvent::Done(result));
    });
}

pub fn spawn_capacity_test(
    tx: Sender<AppEvent>,
    disk: Disk,
    full: bool,
    cancel: Arc<AtomicBool>,
    allow_protected: bool,
) {
    thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            log(&tx, "Capacity test — wiping disk first (all data destroyed).");
            ops::run_prep(&tx, disk.number, allow_protected)?;
            if full {
                capacity_full(&tx, &disk, &cancel)
            } else {
                capacity_quick(&tx, &disk, &cancel)
            }
        })();
        send(&tx, OpEvent::Done(result));
    });
}

/// Quick mode: write 1 MiB pattern blocks at ~256 points spread across the
/// whole address space, then verify. Counterfeit flash wraps addresses, so
/// high-LBA samples come back corrupted within minutes instead of hours.
fn capacity_quick(tx: &Sender<AppEvent>, disk: &Disk, cancel: &AtomicBool) -> Result<String, String> {
    let sample = SAMPLE_MIB as u64;
    if disk.size < sample * 4 {
        return Err("disk too small for the capacity test".into());
    }
    let stride = (disk.size / 256).max(sample) / sample * sample;
    let mut offsets: Vec<u64> = (0..).map(|i| i * stride).take_while(|o| o + sample <= disk.size).collect();
    let last = (disk.size - sample) / sample * sample;
    if offsets.last() != Some(&last) {
        offsets.push(last);
    }
    let n = offsets.len();
    log(tx, format!(
        "Quick mode: {n} sample points of 1 MiB across {} (stride {})",
        human(disk.size),
        human(stride)
    ));

    let mut f = open_direct(disk.number, true)?;
    let mut buf = AlignedBuf::new(SAMPLE_MIB);
    log(tx, "Phase 1/2 — writing patterns…");
    for (i, &off) in offsets.iter().enumerate() {
        if cancelled(cancel) {
            return Err("cancelled".into());
        }
        fill_pattern(buf.as_mut(), off);
        f.seek(SeekFrom::Start(off)).map_err(|e| format!("seek: {e}"))?;
        f.write_all(buf.as_ref())
            .map_err(|e| format!("write at {}: {e}", human(off)))?;
        send(tx, OpEvent::Progress((i + 1) as f64 / (n * 2) as f64, format!("writing point {}/{n}", i + 1)));
    }
    f.sync_all().map_err(|e| format!("flush: {e}"))?;
    drop(f);

    let mut f = open_direct(disk.number, false)?;
    let mut expected = vec![0u8; SAMPLE_MIB];
    log(tx, "Phase 2/2 — reading back & verifying…");
    for (i, &off) in offsets.iter().enumerate() {
        if cancelled(cancel) {
            return Err("cancelled".into());
        }
        f.seek(SeekFrom::Start(off)).map_err(|e| format!("seek: {e}"))?;
        f.read_exact(buf.as_mut())
            .map_err(|e| format!("read at {}: {e}", human(off)))?;
        fill_pattern(&mut expected, off);
        if buf.as_ref() != expected.as_slice() {
            log(tx, format!("MISMATCH at offset {} (point {}/{n})", human(off), i + 1));
            return Err(format!(
                "FAKE OR FAULTY: data written at {} did not survive. Real usable capacity is likely below that point — the drive is lying about its size or has failing flash.",
                human(off)
            ));
        }
        send(tx, OpEvent::Progress((n + i + 1) as f64 / (n * 2) as f64, format!("verifying point {}/{n}", i + 1)));
    }
    Ok(format!(
        "Capacity test PASSED — all {n} sample points across {} verified. The advertised capacity is real. Disk left blank (RAW).",
        human(disk.size)
    ))
}

/// Full mode: write a deterministic pattern over EVERY byte, then read it all
/// back. Definitive, but takes as long as two full-disk passes.
fn capacity_full(tx: &Sender<AppEvent>, disk: &Disk, cancel: &AtomicBool) -> Result<String, String> {
    let total = disk.size / 4096 * 4096;
    let mut buf = AlignedBuf::new(SEQ_CHUNK);

    let mut f = open_direct(disk.number, true)?;
    log(tx, format!("Phase 1/2 — writing pattern over all {}…", human(total)));
    let mut done = 0u64;
    let start = Instant::now();
    let mut last = Instant::now();
    while done < total {
        if cancelled(cancel) {
            return Err("cancelled — disk contains partial test patterns".into());
        }
        let want = (((total - done).min(SEQ_CHUNK as u64)) as usize) / 4096 * 4096;
        fill_pattern(&mut buf.as_mut()[..want], done);
        f.seek(SeekFrom::Start(done)).map_err(|e| format!("seek: {e}"))?;
        f.write_all(&buf.as_ref()[..want])
            .map_err(|e| format!("write at {}: {e}", human(done)))?;
        done += want as u64;
        if last.elapsed() >= Duration::from_millis(250) {
            ops::progress_with(tx, done, total * 2, start, "writing");
            last = Instant::now();
        }
    }
    f.sync_all().map_err(|e| format!("flush: {e}"))?;
    drop(f);

    let mut f = open_direct(disk.number, false)?;
    let mut expected = vec![0u8; SEQ_CHUNK];
    log(tx, "Phase 2/2 — reading everything back & verifying…");
    let mut done = 0u64;
    let mut last = Instant::now();
    while done < total {
        if cancelled(cancel) {
            return Err("cancelled — disk contains test patterns".into());
        }
        let want = (((total - done).min(SEQ_CHUNK as u64)) as usize) / 4096 * 4096;
        f.seek(SeekFrom::Start(done)).map_err(|e| format!("seek: {e}"))?;
        f.read_exact(&mut buf.as_mut()[..want])
            .map_err(|e| format!("read at {}: {e}", human(done)))?;
        fill_pattern(&mut expected[..want], done);
        if buf.as_ref()[..want] != expected[..want] {
            let bad = buf.as_ref()[..want]
                .iter()
                .zip(&expected[..want])
                .position(|(a, b)| a != b)
                .unwrap_or(0) as u64;
            return Err(format!(
                "FAKE OR FAULTY: verification failed at offset {} — real usable capacity is about {}.",
                human(done + bad),
                human(done + bad)
            ));
        }
        done += want as u64;
        if last.elapsed() >= Duration::from_millis(250) {
            ops::progress_with(tx, total + done, total * 2, start, "verifying");
            last = Instant::now();
        }
    }
    Ok(format!(
        "Full capacity test PASSED — every byte of {} written and verified. Disk left blank (RAW).",
        human(total)
    ))
}
