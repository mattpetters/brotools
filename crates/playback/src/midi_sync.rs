//! MIDI message parsing, beat clock and MTC, and the queues between the device threads and the
//! audio thread. No device API: that lives in `midi_hw`.

use soundcraft_mix::MidiOut;
use soundcraft_time::{FrameRate, SampleRate, Samples, Timecode};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

/// A channel message as it arrived from a device, in microseconds since the hub's clock.
#[derive(Debug, Clone, Copy)]
pub struct RawMidi {
    pub us: u64,
    pub port: u8,
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
}

/// A channel message stamped with a timeline sample, for the take.
#[derive(Debug, Clone, Copy)]
pub struct StampedMidi {
    pub at: Samples,
    pub port: u8,
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
}

/// Running-status parser. System exclusive and system realtime are skipped.
/// Used by the device callback on macOS, Windows and Linux; kept on other targets for the tests.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", target_os = "windows")), allow(dead_code))]
#[derive(Debug, Default)]
pub struct Parser {
    running: u8,
    data: [u8; 2],
    got: u8,
    need: u8,
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos", target_os = "windows")), allow(dead_code))]
impl Parser {
    pub fn push(&mut self, bytes: &[u8], mut emit: impl FnMut(u8, u8, u8)) {
        for &b in bytes {
            self.byte(b, &mut emit);
        }
    }

    fn byte(&mut self, b: u8, emit: &mut impl FnMut(u8, u8, u8)) {
        if b >= 0xF8 {
            return;
        }
        if b >= 0xF0 {
            self.running = 0;
            self.got = 0;
            return;
        }
        if b & 0x80 != 0 {
            self.running = b;
            self.got = 0;
            self.need = match b & 0xF0 {
                0xC0 | 0xD0 => 1,
                _ => 2,
            };
            return;
        }
        if self.running == 0 || self.need == 0 {
            return;
        }
        if let Some(slot) = self.data.get_mut(usize::from(self.got)) {
            *slot = b & 0x7F;
        }
        self.got = self.got.saturating_add(1);
        if self.got >= self.need {
            let d2 = if self.need == 1 { 0 } else { self.data[1] };
            emit(self.running, self.data[0], d2);
            self.got = 0;
        }
    }
}

/// Sample offset of an event that arrived at `event_us`, in a block of `frames` that ends at
/// `now_us`. Events older than the block land at 0; events ahead of it land on the last sample.
pub fn sample_offset(event_us: u64, now_us: u64, frames: usize, sample_rate: u32) -> u32 {
    let frames = frames.max(1);
    let sr = u64::from(sample_rate.max(1));
    let block_us = u64::try_from(frames).unwrap_or(u64::MAX).saturating_mul(1_000_000) / sr;
    let start = now_us.saturating_sub(block_us);
    let dt = event_us.saturating_sub(start);
    let off = dt.saturating_mul(sr) / 1_000_000;
    u32::try_from(off).unwrap_or(u32::MAX).min(u32::try_from(frames - 1).unwrap_or(u32::MAX))
}

/// Many MIDI callbacks, one audio thread. The audio thread only `try_lock`s, so it never waits.
pub struct InQueue {
    q: Mutex<VecDeque<RawMidi>>,
    pub dropped: AtomicUsize,
    cap: usize,
}

impl InQueue {
    pub fn new(cap: usize) -> Self {
        let cap = cap.clamp(2, 65_536);
        InQueue { q: Mutex::new(VecDeque::with_capacity(cap)), dropped: AtomicUsize::new(0), cap }
    }

    pub fn push(&self, ev: RawMidi) {
        let mut q = self.q.lock().unwrap_or_else(PoisonError::into_inner);
        if q.len() >= self.cap {
            q.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        q.push_back(ev);
    }

    /// Move everything out. Empty when the device thread holds the queue right now.
    pub fn drain(&self, out: &mut Vec<RawMidi>) {
        let Ok(mut q) = self.q.try_lock() else { return };
        out.extend(q.drain(..));
    }
}

/// Single-producer single-consumer queue of hardware-output messages (audio thread → sender
/// thread). One slot stays empty so head and tail can wrap without a lock.
pub struct OutRing {
    packed: Box<[AtomicU32]>,
    offset: Box<[AtomicU32]>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
    pub dropped: AtomicUsize,
}

impl OutRing {
    pub fn new(cap: usize) -> Self {
        let n = cap.max(16).next_power_of_two();
        OutRing {
            packed: (0..n).map(|_| AtomicU32::new(0)).collect(),
            offset: (0..n).map(|_| AtomicU32::new(0)).collect(),
            mask: n - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }

    pub fn push(&self, ev: MidiOut) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= self.mask {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let i = head & self.mask;
        let packed = u32::from(ev.port) << 24 | u32::from(ev.status) << 16 | u32::from(ev.data1) << 8 | u32::from(ev.data2);
        if let Some(slot) = self.packed.get(i) {
            slot.store(packed, Ordering::Relaxed);
        }
        if let Some(slot) = self.offset.get(i) {
            slot.store(ev.offset, Ordering::Relaxed);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
        true
    }

    pub fn pop(&self) -> Option<MidiOut> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        let i = tail & self.mask;
        let packed = self.packed.get(i).map(|s| s.load(Ordering::Relaxed)).unwrap_or(0);
        let offset = self.offset.get(i).map(|s| s.load(Ordering::Relaxed)).unwrap_or(0);
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(MidiOut { offset, port: (packed >> 24) as u8, status: (packed >> 16) as u8, data1: (packed >> 8) as u8, data2: packed as u8 })
    }
}

/// Recorded messages. The audio thread produces; the UI thread takes them at stop.
pub struct RecordRing {
    at: Box<[AtomicI64]>,
    packed: Box<[AtomicU32]>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
    pub dropped: AtomicUsize,
}

impl RecordRing {
    pub fn new(cap: usize) -> Self {
        let n = cap.max(16).next_power_of_two();
        RecordRing {
            at: (0..n).map(|_| AtomicI64::new(0)).collect(),
            packed: (0..n).map(|_| AtomicU32::new(0)).collect(),
            mask: n - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }

    pub fn push(&self, ev: StampedMidi) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= self.mask {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let i = head & self.mask;
        let packed = u32::from(ev.port) << 24 | u32::from(ev.status) << 16 | u32::from(ev.data1) << 8 | u32::from(ev.data2);
        if let Some(slot) = self.at.get(i) {
            slot.store(ev.at, Ordering::Relaxed);
        }
        if let Some(slot) = self.packed.get(i) {
            slot.store(packed, Ordering::Relaxed);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
        true
    }

    pub fn pop_all(&self) -> Vec<StampedMidi> {
        let mut out = Vec::new();
        while let Some(ev) = self.pop() {
            out.push(ev);
            if out.len() >= self.mask {
                break;
            }
        }
        out
    }

    fn pop(&self) -> Option<StampedMidi> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        let i = tail & self.mask;
        let at = self.at.get(i).map(|s| s.load(Ordering::Relaxed)).unwrap_or(0);
        let packed = self.packed.get(i).map(|s| s.load(Ordering::Relaxed)).unwrap_or(0);
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(StampedMidi { at, port: (packed >> 24) as u8, status: (packed >> 16) as u8, data1: (packed >> 8) as u8, data2: packed as u8 })
    }
}

/// MIDI beat clock (24 per quarter) plus Start / Continue / Stop.
#[derive(Debug, Default)]
pub struct BeatClock {
    next: f64,
    running: bool,
}

impl BeatClock {
    /// Append this block's realtime bytes: `(sample offset, status)`.
    pub fn take(&mut self, playing: bool, pos: Samples, frames: usize, bpm: f64, sample_rate: f64, out: &mut Vec<(u32, u8)>) {
        if playing && !self.running {
            out.push((0, if pos == 0 { 0xFA } else { 0xFB }));
            self.running = true;
            self.next = pos as f64;
        }
        if !playing {
            if self.running {
                out.push((0, 0xFC));
                self.running = false;
            }
            return;
        }
        let step = samples_per_clock(bpm, sample_rate);
        let end = pos as f64 + frames as f64;
        for _ in 0..64 {
            if self.next >= end {
                break;
            }
            let off = (self.next - pos as f64).max(0.0);
            if off < frames as f64 {
                out.push((off as u32, 0xF8));
            }
            self.next += step;
        }
    }
}

fn samples_per_clock(bpm: f64, sample_rate: f64) -> f64 {
    let bpm = if bpm.is_finite() && bpm > 1.0 { bpm } else { 120.0 };
    let sr = if sample_rate.is_finite() && sample_rate > 1.0 { sample_rate } else { 48_000.0 };
    (sr * 60.0 / (bpm * 24.0)).max(1.0)
}

/// Session clock a block of MTC is generated against.
#[derive(Clone, Copy)]
pub struct MtcWhen {
    pub rate: FrameRate,
    pub sr: SampleRate,
    pub origin: Samples,
}

/// Quarter-frame MIDI time code while playing. Rates MTC cannot name produce nothing.
#[derive(Debug, Default)]
pub struct MtcGen {
    next: f64,
    piece: u8,
    hours: i64,
    minutes: i64,
    seconds: i64,
    frames: i64,
    latched: bool,
}

impl MtcGen {
    pub fn reset(&mut self, pos: Samples) {
        self.next = pos as f64;
        self.piece = 0;
        self.latched = false;
    }

    /// Append `(sample offset, 0xF1, data)` quarter frames for this block.
    pub fn take(&mut self, playing: bool, pos: Samples, frames: usize, when: MtcWhen, out: &mut Vec<(u32, u8, u8)>) {
        let Some(code) = mtc_rate(when.rate) else { return };
        if !playing {
            self.latched = false;
            return;
        }
        if !self.latched {
            self.reset(pos);
        }
        let step = (when.rate.samples_per_frame(when.sr) / 4.0).max(1.0);
        let end = pos as f64 + frames as f64;
        for _ in 0..32 {
            if self.next >= end {
                break;
            }
            if self.piece == 0 {
                let at = (self.next as i64).saturating_add(when.origin).max(0);
                let tc = Timecode::from_frames(when.rate.frames_at(at, when.sr), when.rate);
                self.hours = tc.hours;
                self.minutes = tc.minutes;
                self.seconds = tc.seconds;
                self.frames = tc.frames;
                self.latched = true;
            }
            let off = (self.next - pos as f64).max(0.0);
            if off < frames as f64 {
                out.push((off as u32, 0xF1, quarter_data(self, code)));
            }
            self.piece = (self.piece + 1) % 8;
            self.next += step;
        }
    }
}

fn mtc_rate(rate: FrameRate) -> Option<u8> {
    match rate {
        FrameRate::Fps24 | FrameRate::Fps23_976 => Some(0),
        FrameRate::Fps25 => Some(1),
        FrameRate::Fps29_97Drop => Some(2),
        FrameRate::Fps30 | FrameRate::Fps29_97 => Some(3),
        _ => None,
    }
}

fn quarter_data(g: &MtcGen, rate: u8) -> u8 {
    let nibble = match g.piece {
        0 => g.frames as u8 & 0x0F,
        1 => (g.frames as u8 >> 4) & 0x01,
        2 => g.seconds as u8 & 0x0F,
        3 => (g.seconds as u8 >> 4) & 0x07,
        4 => g.minutes as u8 & 0x0F,
        5 => (g.minutes as u8 >> 4) & 0x07,
        6 => g.hours as u8 & 0x0F,
        _ => ((g.hours as u8 >> 4) & 0x01) | ((rate & 0x03) << 1),
    };
    (g.piece << 4) | (nibble & 0x0F)
}

/// Where `prefix` (`"midi.beat_clock.destination="`) points, or every output when it is unset.
/// `prefix` is a static string, so this does not allocate.
pub fn destination_port(flags: &std::collections::BTreeSet<String>, prefix: &str, ports: &soundcraft_mix::MidiPorts) -> Option<u8> {
    match flags.iter().find_map(|f| f.strip_prefix(prefix)).filter(|n| !n.is_empty()) {
        Some(name) => ports.index_of(name),
        None => Some(soundcraft_mix::ALL_PORTS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_status_and_realtime_bytes() {
        let mut p = Parser::default();
        let mut got = Vec::new();
        p.push(&[0x90, 60, 100, 64, 80, 0xF8, 0x80, 60, 0], |s, a, b| got.push([s, a, b]));
        assert_eq!(got, vec![[0x90, 60, 100], [0x90, 64, 80], [0x80, 60, 0]]);
        got.clear();
        p.push(&[0xC0, 12, 0xD0, 40], |s, a, b| got.push([s, a, b]));
        assert_eq!(got, vec![[0xC0, 12, 0], [0xD0, 40, 0]]);
    }

    #[test]
    fn offsets_land_inside_the_block() {
        let sr = 48_000;
        let frames = 480; // 10 ms
        assert_eq!(sample_offset(10_000, 10_000, frames, sr), 479);
        assert_eq!(sample_offset(0, 10_000, frames, sr), 0);
        assert_eq!(sample_offset(5_000, 10_000, frames, sr), 240);
    }

    #[test]
    fn beat_clock_is_24_per_quarter_and_starts_and_stops() {
        let mut c = BeatClock::default();
        let mut out = Vec::new();
        // 120 bpm, 48 kHz: a quarter is 24 000 samples, so a clock is every 1 000 samples.
        c.take(true, 0, 2500, 120.0, 48_000.0, &mut out);
        assert_eq!(out.first().copied(), Some((0, 0xFA)));
        let clocks: Vec<u32> = out.iter().filter(|(_, s)| *s == 0xF8).map(|(o, _)| *o).collect();
        assert_eq!(clocks, vec![0, 1000, 2000]);
        out.clear();
        c.take(false, 2500, 512, 120.0, 48_000.0, &mut out);
        assert_eq!(out, vec![(0, 0xFC)]);
    }

    #[test]
    fn mtc_quarter_frames_at_25_fps() {
        let mut g = MtcGen::default();
        let mut out = Vec::new();
        let sr = SampleRate::new(48_000).unwrap();
        // 25 fps: 1920 samples per frame, 480 per quarter frame.
        let when = MtcWhen { rate: FrameRate::Fps25, sr, origin: 0 };
        g.take(true, 0, 2000, when, &mut out);
        assert!(out.len() >= 4, "{out:?}");
        assert!(out.iter().all(|(_, st, _)| *st == 0xF1));
        assert_eq!(out[0].2 >> 4, 0);
        assert_eq!(out[1].2 >> 4, 1);
        let fast = MtcWhen { rate: FrameRate::Fps120, sr, origin: 0 };
        g.take(true, 0, 100, fast, &mut out);
        // 120 fps is not an MTC rate: nothing further is added.
        let n = out.len();
        g.take(true, 10_000, 1000, fast, &mut out);
        assert_eq!(out.len(), n);
    }

    #[test]
    fn rings_keep_order_and_drop_when_full() {
        let q = InQueue::new(2);
        q.push(RawMidi { us: 1, port: 0, status: 0x90, data1: 1, data2: 1 });
        q.push(RawMidi { us: 2, port: 0, status: 0x90, data1: 2, data2: 1 });
        q.push(RawMidi { us: 3, port: 0, status: 0x90, data1: 3, data2: 1 });
        assert_eq!(q.dropped.load(Ordering::Relaxed), 1);
        let mut got = Vec::new();
        q.drain(&mut got);
        assert_eq!(got.iter().map(|e| e.data1).collect::<Vec<_>>(), vec![2, 3]);

        let ring = OutRing::new(4);
        assert!(ring.push(MidiOut { offset: 3, port: 1, status: 0x90, data1: 60, data2: 100 }));
        let ev = ring.pop().unwrap();
        assert_eq!((ev.offset, ev.port, ev.status, ev.data1, ev.data2), (3, 1, 0x90, 60, 100));
        assert!(ring.pop().is_none());
    }
}
