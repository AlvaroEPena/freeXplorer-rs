//! Helpers for the tests: a small random number generator, devices that record what happens
//! to them, and a quick way to get a formatted volume.

use std::io;
use std::sync::{Arc, Mutex};

use crate::device::{BlockDevice, Device, MemoryDevice};
use crate::fatx::{self, FatxVolume, Flavor, FormatOptions, NodeId};

/// xorshift64*, enough to make repeatable noise.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 0 up to but not including `n`
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }

    pub fn range(&mut self, low: usize, high: usize) -> usize {
        low + self.below(high - low)
    }

    pub fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let word = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }
}

pub fn random_bytes(count: usize, seed: u64) -> Vec<u8> {
    let mut data = vec![0u8; count];
    Rng::new(seed).fill(&mut data);
    data
}

pub fn create(length: u64, spc: u32, flavor: Flavor) -> (Arc<MemoryDevice>, FatxVolume) {
    let device = Arc::new(MemoryDevice::new(length));
    let shared: Device = device.clone();
    fatx::format(
        &shared,
        0,
        length,
        &FormatOptions {
            flavor,
            sectors_per_cluster: spc,
            volume_id: Some(0x1234_ABCD),
            label: None,
        },
        |_| {},
    )
    .unwrap();
    let mut volume = FatxVolume::open(shared, true).unwrap();
    volume.scan_free_space(|_| {}).unwrap();
    (device, volume)
}

pub fn create_default() -> (Arc<MemoryDevice>, FatxVolume) {
    create(64 << 20, 0x20, Flavor::Xbox360)
}

pub fn reopen(device: &Arc<MemoryDevice>, old: FatxVolume) -> FatxVolume {
    drop(old);
    let shared: Device = device.clone();
    let mut volume = FatxVolume::open(shared, true).unwrap();
    volume.scan_free_space(|_| {}).unwrap();
    volume
}

pub fn read_all(vol: &mut FatxVolume, file: NodeId) -> Vec<u8> {
    let size = vol.info(file).size as usize;
    let mut data = vec![0u8; size];
    let n = vol.read(file, 0, &mut data).unwrap();
    assert_eq!(n, size);
    data
}

pub fn assert_clean(vol: &mut FatxVolume) {
    let report = vol.check(false, |_| {}).unwrap();
    let issues: Vec<String> = report
        .issues
        .iter()
        .map(|i| format!("{:?}: {}", i.severity, i.message))
        .collect();
    assert!(report.is_clean(), "{}", issues.join("\n"));
}

/// Counts what gets written, so a test can prove something stayed untouched.
pub struct RecordingDevice {
    inner: Device,
    stats: Mutex<(u64, u64)>,
}

impl RecordingDevice {
    pub fn new(inner: Device) -> Self {
        RecordingDevice {
            inner,
            stats: Mutex::new((u64::MAX, 0)),
        }
    }

    pub fn lowest_write(&self) -> u64 {
        self.stats.lock().unwrap().0
    }

    pub fn highest_write_end(&self) -> u64 {
        self.stats.lock().unwrap().1
    }
}

impl BlockDevice for RecordingDevice {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn len(&self) -> u64 {
        self.inner.len()
    }
    fn can_write(&self) -> bool {
        self.inner.can_write()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)
    }
    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        let mut stats = self.stats.lock().unwrap();
        stats.0 = stats.0.min(offset);
        stats.1 = stats.1.max(offset + data.len() as u64);
        drop(stats);
        self.inner.write_at(offset, data)
    }
    fn flush(&self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Keeps every write in order so it can be replayed up to any point.
pub struct JournalDevice {
    inner: Device,
    pub writes: Mutex<Vec<(u64, Vec<u8>)>>,
}

impl JournalDevice {
    pub fn new(inner: Device) -> Self {
        JournalDevice {
            inner,
            writes: Mutex::new(Vec::new()),
        }
    }
}

impl BlockDevice for JournalDevice {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn len(&self) -> u64 {
        self.inner.len()
    }
    fn can_write(&self) -> bool {
        true
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)
    }
    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        self.writes.lock().unwrap().push((offset, data.to_vec()));
        self.inner.write_at(offset, data)
    }
    fn flush(&self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// What a console drive has at 0x2000: the serial number as text, and at +0x58 the sectors
/// the console may use.
pub fn console_drive(length: u64, serial: &str, claimed: u64) -> MemoryDevice {
    let device = MemoryDevice::new(length);
    let mut text = serial.as_bytes().to_vec();
    text.resize(0x14, b' ');
    device.write_at(0x2000, &text).unwrap();
    device
        .write_at(0x2000 + 0x58, &((claimed / 512) as u32).to_le_bytes())
        .unwrap();
    device
}
