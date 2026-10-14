use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::device::BlockDevice;
use crate::error::{Error, Result};
use crate::size;

pub const CHUNK_SIZE: usize = 4 << 20;
const ZERO_BLOCK: usize = 64 << 10;

/// How far a long copy has got.
#[derive(Debug, Clone)]
pub struct Progress {
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub bytes_per_second: f64,
}

impl Progress {
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            1.0
        } else {
            (self.done as f64 / self.total as f64).min(1.0)
        }
    }

    pub fn remaining(&self) -> Option<Duration> {
        (self.bytes_per_second >= 1.0 && self.done < self.total)
            .then(|| Duration::from_secs_f64((self.total - self.done) as f64 / self.bytes_per_second))
    }

    /// "12.3 GB of 55.9 GB, 38 MB/s, about 19 min left"
    pub fn summary(&self) -> String {
        let mut text = format!("{} of {}", size::format(self.done), size::format(self.total));
        if self.bytes_per_second >= 1.0 {
            text += &format!(", {}/s", size::format(self.bytes_per_second as u64));
        }
        if let Some(left) = self.remaining() {
            text += &format!(", {} left", how_long(left));
        }
        text
    }
}

pub fn how_long(time: Duration) -> String {
    let seconds = time.as_secs_f64();
    if seconds >= 3600.0 {
        let hours = (seconds / 3600.0) as u64;
        let minutes = ((seconds % 3600.0) / 60.0) as u64;
        format!("about {hours} h {minutes} min")
    } else if seconds >= 90.0 {
        format!("about {} min", (seconds / 60.0).round() as u64)
    } else {
        format!("{} s", (seconds.round() as u64).max(1))
    }
}

pub trait ProgressSink {
    fn report(&mut self, progress: &Progress);
}

impl<F: FnMut(&Progress)> ProgressSink for F {
    fn report(&mut self, progress: &Progress) {
        self(progress)
    }
}

/// For when nobody is watching.
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report(&mut self, _: &Progress) {}
}

/// Lets another thread stop a long job. Clones all stop together.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CopyOptions {
    /// Leave blocks that are all zeros unwritten. Only right when the destination is a brand new
    /// sparse file, anywhere else the zeros have to land on top of whatever is there.
    pub skip_zero_blocks: bool,
    /// flush the destination every so many bytes, 0 means only at the end
    pub flush_every: u64,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Copies the bytes from `start` up to `length` and returns their SHA-256 as lowercase hex.
/// Nothing before `start` is read or written.
#[allow(clippy::too_many_arguments)]
pub fn copy(
    from: &dyn BlockDevice,
    to: &dyn BlockDevice,
    length: u64,
    options: &CopyOptions,
    progress: &mut dyn ProgressSink,
    cancel: &Cancel,
    phase: &str,
    start: u64,
) -> Result<String> {
    if length > from.len() {
        return Err(Error::invalid(format!("{} is only {} bytes", from.name(), from.len())));
    }
    if length > to.len() {
        return Err(Error::invalid(format!("{} is only {} bytes", to.name(), to.len())));
    }
    if start > length {
        return Err(Error::invalid("the start is outside the copy"));
    }
    if !to.can_write() {
        return Err(Error::Unsupported(format!("{} was opened read-only", to.name())));
    }

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut meter = Meter::new(phase, length - start, progress);
    let mut done = start;
    let mut unflushed = 0u64;

    while done < length {
        cancel.check()?;
        let n = (buffer.len() as u64).min(length - done) as usize;
        let chunk = &mut buffer[..n];

        from.read_at(done, chunk)?;
        hasher.update(&*chunk);
        if options.skip_zero_blocks {
            write_non_zero(to, done, chunk)?;
        } else {
            to.write_at(done, chunk)?;
        }

        done += n as u64;
        unflushed += n as u64;
        if options.flush_every > 0 && unflushed >= options.flush_every {
            to.flush()?;
            unflushed = 0;
        }
        meter.report(done - start, false);
    }
    to.flush()?;
    meter.report(done - start, true);
    Ok(hex(&hasher.finalize()))
}

/// Reads the bytes from `start` up to `length` and returns their SHA-256 as lowercase hex.
pub fn hash(
    device: &dyn BlockDevice,
    length: u64,
    progress: &mut dyn ProgressSink,
    cancel: &Cancel,
    phase: &str,
    start: u64,
) -> Result<String> {
    if length > device.len() {
        return Err(Error::invalid(format!(
            "{} is only {} bytes",
            device.name(),
            device.len()
        )));
    }
    if start > length {
        return Err(Error::invalid("the start is outside the range to read"));
    }

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut meter = Meter::new(phase, length - start, progress);
    let mut done = start;

    while done < length {
        cancel.check()?;
        let n = (buffer.len() as u64).min(length - done) as usize;
        device.read_at(done, &mut buffer[..n])?;
        hasher.update(&buffer[..n]);
        done += n as u64;
        meter.report(done - start, false);
    }
    meter.report(done - start, true);
    Ok(hex(&hasher.finalize()))
}

/// Writes only the stretches that have something in them, so the holes of a sparse file stay holes.
fn write_non_zero(to: &dyn BlockDevice, offset: u64, chunk: &[u8]) -> Result<()> {
    let mut pos = 0;
    while pos < chunk.len() {
        while pos < chunk.len() && is_empty(chunk, pos) {
            pos += ZERO_BLOCK;
        }
        if pos >= chunk.len() {
            break;
        }
        let start = pos;
        while pos < chunk.len() && !is_empty(chunk, pos) {
            pos += ZERO_BLOCK;
        }
        let end = pos.min(chunk.len());
        to.write_at(offset + start as u64, &chunk[start..end])?;
    }
    Ok(())
}

fn is_empty(chunk: &[u8], pos: usize) -> bool {
    let end = (pos + ZERO_BLOCK).min(chunk.len());
    chunk[pos..end].iter().all(|&b| b == 0)
}

/// Speed over the last few seconds, and no more than a handful of reports a second.
struct Meter<'a> {
    clock: Instant,
    samples: VecDeque<(f64, u64)>,
    phase: String,
    total: u64,
    sink: &'a mut dyn ProgressSink,
    last_report: f64,
}

impl<'a> Meter<'a> {
    fn new(phase: &str, total: u64, sink: &'a mut dyn ProgressSink) -> Self {
        let mut samples = VecDeque::new();
        samples.push_back((0.0, 0));
        sink.report(&Progress {
            phase: phase.to_string(),
            done: 0,
            total,
            bytes_per_second: 0.0,
        });
        Meter {
            clock: Instant::now(),
            samples,
            phase: phase.to_string(),
            total,
            sink,
            last_report: f64::NEG_INFINITY,
        }
    }

    fn report(&mut self, done: u64, force: bool) {
        let now = self.clock.elapsed().as_secs_f64();
        self.samples.push_back((now, done));
        while self.samples.len() > 2 && now - self.samples.front().unwrap().0 > 8.0 {
            self.samples.pop_front();
        }
        if !force && now - self.last_report < 0.2 {
            return;
        }
        self.last_report = now;

        let (then, before) = *self.samples.front().unwrap();
        let speed = if now - then > 0.5 {
            (done - before) as f64 / (now - then)
        } else {
            0.0
        };
        self.sink.report(&Progress {
            phase: self.phase.clone(),
            done,
            total: self.total,
            bytes_per_second: speed,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{MemoryDevice, read_vec};

    fn patterned(length: u64, seed: u8) -> MemoryDevice {
        let device = MemoryDevice::new(length);
        let data: Vec<u8> = (0..300_000u32)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect();
        device.write_at(1 << 20, &data).unwrap();
        device.write_at(length - 4096, &data[..4096]).unwrap();
        device
    }

    fn sha(device: &dyn BlockDevice) -> String {
        hash(device, device.len(), &mut NoProgress, &Cancel::new(), "x", 0).unwrap()
    }

    #[test]
    fn a_copy_is_exact_and_hashes_what_it_copied() {
        let from = patterned(16 << 20, 1);
        let to = MemoryDevice::new(16 << 20);
        let digest = copy(
            &from,
            &to,
            16 << 20,
            &CopyOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
            "copy",
            0,
        )
        .unwrap();
        assert_eq!(digest, sha(&from));
        assert_eq!(sha(&to), digest);
    }

    #[test]
    fn the_known_hash_of_nothing_in_particular() {
        let from = MemoryDevice::new(0);
        assert_eq!(
            sha(&from),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn empty_blocks_can_be_skipped() {
        let from = patterned(16 << 20, 2);
        let to = MemoryDevice::new(16 << 20);
        let options = CopyOptions {
            skip_zero_blocks: true,
            ..Default::default()
        };
        let digest = copy(
            &from,
            &to,
            16 << 20,
            &options,
            &mut NoProgress,
            &Cancel::new(),
            "copy",
            0,
        )
        .unwrap();
        assert_eq!(sha(&to), digest);
        assert!(to.allocated_bytes() < from.len() / 2);
    }

    #[test]
    fn empty_blocks_are_written_over_old_data() {
        let from = MemoryDevice::new(8 << 20);
        let to = patterned(8 << 20, 3);
        copy(
            &from,
            &to,
            8 << 20,
            &CopyOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
            "copy",
            0,
        )
        .unwrap();
        assert_eq!(read_vec(&to, 1 << 20, 1000).unwrap(), vec![0u8; 1000]);
    }

    #[test]
    fn a_copy_can_start_part_way_in() {
        let from = patterned(8 << 20, 4);
        let to = patterned(8 << 20, 9);
        let before = read_vec(&to, 0, 2 << 20).unwrap();
        let digest = copy(
            &from,
            &to,
            8 << 20,
            &CopyOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
            "copy",
            2 << 20,
        )
        .unwrap();
        assert_eq!(read_vec(&to, 0, 2 << 20).unwrap(), before);
        assert_eq!(
            read_vec(&to, 2 << 20, 1 << 20).unwrap(),
            read_vec(&from, 2 << 20, 1 << 20).unwrap()
        );
        let only_the_tail = hash(&from, 8 << 20, &mut NoProgress, &Cancel::new(), "x", 2 << 20).unwrap();
        assert_eq!(digest, only_the_tail);
    }

    #[test]
    fn progress_counts_only_what_is_copied() {
        let from = MemoryDevice::new(10 << 20);
        let to = MemoryDevice::new(10 << 20);
        let mut seen = Vec::new();
        let mut sink = |p: &Progress| seen.push((p.done, p.total));
        copy(
            &from,
            &to,
            10 << 20,
            &CopyOptions::default(),
            &mut sink,
            &Cancel::new(),
            "copy",
            4 << 20,
        )
        .unwrap();
        assert_eq!(seen.first(), Some(&(0, 6 << 20)));
        assert_eq!(seen.last(), Some(&(6 << 20, 6 << 20)));
    }

    #[test]
    fn a_cancelled_copy_stops() {
        let from = MemoryDevice::new(32 << 20);
        let to = MemoryDevice::new(32 << 20);
        let cancel = Cancel::new();
        cancel.cancel();
        let result = copy(
            &from,
            &to,
            32 << 20,
            &CopyOptions::default(),
            &mut NoProgress,
            &cancel,
            "copy",
            0,
        );
        assert!(matches!(result, Err(Error::Cancelled)));
    }

    #[test]
    fn a_read_only_target_is_refused() {
        let from = MemoryDevice::new(1 << 20);
        let to = MemoryDevice::new(1 << 20);
        to.set_writable(false);
        assert!(
            copy(
                &from,
                &to,
                1 << 20,
                &CopyOptions::default(),
                &mut NoProgress,
                &Cancel::new(),
                "c",
                0
            )
            .is_err()
        );
    }

    #[test]
    fn lengths_that_dont_fit_are_refused() {
        let from = MemoryDevice::new(1 << 20);
        let to = MemoryDevice::new(2 << 20);
        let go = |length, start| {
            copy(
                &from,
                &to,
                length,
                &CopyOptions::default(),
                &mut NoProgress,
                &Cancel::new(),
                "c",
                start,
            )
        };
        assert!(go(2 << 20, 0).is_err());
        assert!(go(1 << 20, (1 << 20) + 1).is_err());
    }

    #[test]
    fn flushing_every_so_often_still_copies_everything() {
        let from = patterned(16 << 20, 5);
        let to = MemoryDevice::new(16 << 20);
        let options = CopyOptions {
            flush_every: 1 << 20,
            ..Default::default()
        };
        let digest = copy(&from, &to, 16 << 20, &options, &mut NoProgress, &Cancel::new(), "c", 0).unwrap();
        assert_eq!(sha(&to), digest);
    }

    #[test]
    fn the_summary_reads_naturally() {
        let p = Progress {
            phase: "x".into(),
            done: 13 << 30,
            total: 56 << 30,
            bytes_per_second: 38.0 * 1048576.0,
        };
        assert_eq!(p.summary(), "13 GB of 56 GB, 38 MB/s, about 19 min left");
        assert_eq!(how_long(Duration::from_secs(5400 + 120)), "about 1 h 32 min");
        assert_eq!(how_long(Duration::from_secs(20)), "20 s");
    }
}
