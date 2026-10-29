//! MIDI devices: enumerate, hot-plug, and move channel messages between the hardware and the
//! audio thread.
//!
//! Inputs are opened on macOS (CoreMIDI), Windows (WinMM) and Linux (ALSA sequencer) through
//! midir. The callbacks only push into [`InQueue`]. The audio thread drains that queue; a
//! separate thread sends [`OutRing`] messages, so the audio callback never calls into the driver.
//! Other platforms have the same API and no ports.

use crate::midi_sync::{InQueue, OutRing, RawMidi, RecordRing, StampedMidi};
use soundcraft_mix::{ALL_PORTS, MidiOut, MidiPorts};
use soundcraft_model::Session;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// One port, as the Setup window shows it.
#[derive(Debug, Clone)]
pub struct PortInfo {
    pub name: String,
    pub enabled: bool,
    pub open: bool,
    pub error: Option<String>,
    pub activity: u64,
}

/// Shared with the audio callback. Cloning only clones the `Arc`s.
#[derive(Clone)]
pub struct MidiLink {
    pub input: Arc<InQueue>,
    pub output: Arc<OutRing>,
    pub record: Arc<RecordRing>,
    /// Timeline sample the take has reached. The audio thread stores it; the UI reads it at stop.
    pub record_end: Arc<AtomicI64>,
    pub ports: Arc<MidiPorts>,
    clock: Arc<MidiClock>,
}

impl MidiLink {
    pub fn now_us(&self) -> u64 {
        self.clock.us()
    }

    pub fn take_recorded(&self) -> Vec<StampedMidi> {
        self.record.pop_all()
    }
}

struct MidiClock {
    epoch: Instant,
}

impl MidiClock {
    fn us(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

struct Activity {
    counts: [AtomicU64; 32],
}

impl Activity {
    fn new() -> Self {
        Self { counts: std::array::from_fn(|_| AtomicU64::new(0)) }
    }

    #[cfg_attr(not(any(target_os = "linux", target_os = "macos", target_os = "windows")), allow(dead_code))]
    fn add(&self, port: u8) {
        if let Some(c) = self.counts.get(usize::from(port)) {
            c.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn get(&self, port: u8) -> u64 {
        self.counts.get(usize::from(port)).map(|c| c.load(Ordering::Relaxed)).unwrap_or(0)
    }
}

enum OutCmd {
    Set(Vec<String>),
    Quit,
}

/// The process's MIDI devices. One per app.
pub struct MidiHub {
    link: MidiLink,
    activity: Arc<Activity>,
    inputs: Vec<OpenIn>,
    listed_in: Vec<PortInfo>,
    listed_out: Vec<PortInfo>,
    out_tx: Option<Sender<OutCmd>>,
    out_thread: Option<std::thread::JoinHandle<()>>,
    last_scan: Option<Instant>,
    failed_in: Vec<(String, String)>,
    failed_out: Vec<(String, String)>,
}

struct OpenIn {
    name: String,
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    _conn: midir::MidiInputConnection<()>,
}

/// midir's backends. Everywhere else the hub lists nothing.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod platform {
    pub const MIDI: bool = true;
}
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod platform {
    pub const MIDI: bool = false;
}

impl Default for MidiHub {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiHub {
    pub fn new() -> Self {
        let link = MidiLink {
            input: Arc::new(InQueue::new(4_096)),
            output: Arc::new(OutRing::new(4_096)),
            record: Arc::new(RecordRing::new(65_536)),
            record_end: Arc::new(AtomicI64::new(0)),
            ports: Arc::new(MidiPorts::default()),
            clock: Arc::new(MidiClock { epoch: Instant::now() }),
        };
        let activity = Arc::new(Activity::new());
        let (out_tx, out_thread) = spawn_output(Arc::clone(&link.output), Arc::clone(&link.ports));
        MidiHub {
            link,
            activity,
            inputs: Vec::new(),
            listed_in: Vec::new(),
            listed_out: Vec::new(),
            out_tx,
            out_thread,
            last_scan: None,
            failed_in: Vec::new(),
            failed_out: Vec::new(),
        }
    }

    pub fn link(&self) -> MidiLink {
        self.link.clone()
    }

    pub fn inputs(&self) -> &[PortInfo] {
        &self.listed_in
    }

    pub fn outputs(&self) -> &[PortInfo] {
        &self.listed_out
    }

    /// Re-scan devices at most once a second and open whatever the session has enabled.
    pub fn poll(&mut self, session: &Session) {
        let due = self.last_scan.is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        if due {
            self.refresh(session);
        }
    }

    pub fn refresh(&mut self, session: &Session) {
        self.last_scan = Some(Instant::now());
        if !platform::MIDI {
            return;
        }
        let available_in = list_inputs();
        let available_out = list_outputs();
        let want_in = wanted(session, "midi.input.", &available_in);
        let want_out = wanted(session, "midi.output.", &available_out);
        self.sync_inputs(&want_in);
        if let Some(tx) = &self.out_tx {
            let _ = tx.send(OutCmd::Set(want_out.clone()));
        }
        self.listed_in = self.describe(&available_in, &want_in, true);
        self.listed_out = describe_out(&available_out, &want_out, &self.failed_out);
    }

    /// Push one message as if a device had sent it. `port` [`ALL_PORTS`] matches every track that
    /// listens to all devices.
    pub fn inject(&self, status: u8, data1: u8, data2: u8) {
        self.link.input.push(RawMidi { us: self.link.now_us(), port: ALL_PORTS, status, data1, data2 });
    }

    fn sync_inputs(&mut self, want: &[String]) {
        self.inputs.retain(|o| want.iter().any(|n| n == &o.name));
        self.failed_in.retain(|(n, _)| want.iter().any(|w| w == n));
        for name in want {
            if self.inputs.iter().any(|o| &o.name == name) {
                continue;
            }
            match open_input(name, &self.link, &self.activity) {
                Ok(open) => {
                    self.failed_in.retain(|(n, _)| n != name);
                    self.inputs.push(open);
                }
                Err(err) => {
                    log::warn!("MIDI input {name}: {err}");
                    if let Some(slot) = self.failed_in.iter_mut().find(|(n, _)| n == name) {
                        slot.1 = err;
                    } else {
                        self.failed_in.push((name.clone(), err));
                    }
                }
            }
        }
    }

    fn describe(&self, available: &[String], want: &[String], inputs: bool) -> Vec<PortInfo> {
        let _ = inputs;
        available
            .iter()
            .map(|name| {
                let index = self.link.ports.index_of(name).unwrap_or(ALL_PORTS);
                PortInfo {
                    name: name.clone(),
                    enabled: want.iter().any(|w| w == name),
                    open: self.inputs.iter().any(|o| &o.name == name),
                    error: self.failed_in.iter().find(|(n, _)| n == name).map(|(_, e)| e.clone()),
                    activity: self.activity.get(index),
                }
            })
            .collect()
    }
}

impl Drop for MidiHub {
    fn drop(&mut self) {
        if let Some(tx) = self.out_tx.take() {
            let _ = tx.send(OutCmd::Quit);
        }
        if let Some(t) = self.out_thread.take() {
            let _ = t.join();
        }
    }
}

fn wanted(session: &Session, prefix: &str, available: &[String]) -> Vec<String> {
    let allow: Vec<&str> = session.edit.flags.iter().filter_map(|f| f.strip_prefix(prefix)).collect();
    if allow.is_empty() {
        return available.to_vec();
    }
    available.iter().filter(|n| allow.contains(&n.as_str())).cloned().collect()
}

fn describe_out(available: &[String], want: &[String], failed: &[(String, String)]) -> Vec<PortInfo> {
    available
        .iter()
        .map(|name| PortInfo {
            name: name.clone(),
            enabled: want.iter().any(|w| w == name),
            open: want.iter().any(|w| w == name) && failed.iter().all(|(n, _)| n != name),
            error: failed.iter().find(|(n, _)| n == name).map(|(_, e)| e.clone()),
            activity: 0,
        })
        .collect()
}

fn spawn_output(ring: Arc<OutRing>, ports: Arc<MidiPorts>) -> (Option<Sender<OutCmd>>, Option<std::thread::JoinHandle<()>>) {
    if !platform::MIDI {
        return (None, None);
    }
    let (tx, rx) = channel();
    let spawn = std::thread::Builder::new().name("soundcraft-midi-out".into()).spawn(move || output_loop(rx, ring, ports));
    match spawn {
        Ok(t) => (Some(tx), Some(t)),
        Err(e) => {
            log::warn!("MIDI output thread: {e}");
            (None, None)
        }
    }
}

fn output_loop(rx: Receiver<OutCmd>, ring: Arc<OutRing>, ports: Arc<MidiPorts>) {
    let mut open: Vec<OpenOut> = Vec::new();
    loop {
        let mut quit = false;
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                OutCmd::Quit => quit = true,
                OutCmd::Set(names) => sync_outputs(&mut open, &names),
            }
        }
        if quit {
            break;
        }
        let mut sent = false;
        while let Some(ev) = ring.pop() {
            sent = true;
            send_out(&mut open, &ports, ev);
        }
        if !sent {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn send_out(open: &mut [OpenOut], ports: &MidiPorts, ev: MidiOut) {
    let mut buf = [0u8; 3];
    let n = message_len(&mut buf, ev);
    let bytes = buf.get(..n).unwrap_or(&[]);
    if ev.port == ALL_PORTS {
        for o in open.iter_mut() {
            let _ = o.send(bytes);
        }
    } else if let Some(name) = ports.name(ev.port)
        && let Some(o) = open.iter_mut().find(|o| o.name == name)
    {
        let _ = o.send(bytes);
    }
}

fn message_len(buf: &mut [u8; 3], ev: MidiOut) -> usize {
    buf[0] = ev.status;
    if ev.status >= 0xF8 {
        return 1;
    }
    buf[1] = ev.data1;
    if ev.status == 0xF1 || matches!(ev.status & 0xF0, 0xC0 | 0xD0) {
        return 2;
    }
    buf[2] = ev.data2;
    3
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn list_inputs() -> Vec<String> {
    list_ports(true)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn list_inputs() -> Vec<String> {
    Vec::new()
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn list_outputs() -> Vec<String> {
    list_ports(false)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn list_outputs() -> Vec<String> {
    Vec::new()
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn list_ports(input: bool) -> Vec<String> {
    let mut names = Vec::new();
    if input {
        match midir::MidiInput::new("SoundCraft") {
            Ok(midi) => push_unique(&mut names, midi.ports().iter().filter_map(|p| midi.port_name(p).ok())),
            Err(e) => log::warn!("MIDI inputs: {e}"),
        }
    } else {
        match midir::MidiOutput::new("SoundCraft") {
            Ok(midi) => push_unique(&mut names, midi.ports().iter().filter_map(|p| midi.port_name(p).ok())),
            Err(e) => log::warn!("MIDI outputs: {e}"),
        }
    }
    names
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn push_unique(names: &mut Vec<String>, found: impl Iterator<Item = String>) {
    for n in found {
        if !n.is_empty() && !names.contains(&n) {
            names.push(n);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn open_input(name: &str, link: &MidiLink, activity: &Arc<Activity>) -> Result<OpenIn, String> {
    let mut midi = midir::MidiInput::new("SoundCraft").map_err(|e| e.to_string())?;
    midi.ignore(midir::Ignore::All);
    let port = midi
        .ports()
        .into_iter()
        .find(|p| midi.port_name(p).ok().as_deref() == Some(name))
        .ok_or_else(|| format!("“{name}” is no longer connected"))?;
    let index = link.ports.assign(name);
    let queue = Arc::clone(&link.input);
    let clock = Arc::clone(&link.clock);
    let activity = Arc::clone(activity);
    let mut parser = crate::midi_sync::Parser::default();
    let conn = midi
        .connect(
            &port,
            "soundcraft-in",
            move |_stamp, msg, _| {
                let us = clock.us();
                parser.push(msg, |status, data1, data2| {
                    queue.push(RawMidi { us, port: index, status, data1, data2 });
                    activity.add(index);
                });
            },
            (),
        )
        .map_err(|e| e.to_string())?;
    Ok(OpenIn { name: name.to_string(), _conn: conn })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_input(_name: &str, _link: &MidiLink, _activity: &Arc<Activity>) -> Result<OpenIn, String> {
    Err("no MIDI devices on this platform".into())
}

struct OpenOut {
    name: String,
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    conn: Mutex<midir::MidiOutputConnection>,
}

impl OpenOut {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
            conn.send(bytes).map_err(|e| e.to_string())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = bytes;
            Err("no MIDI devices on this platform".into())
        }
    }
}

fn sync_outputs(open: &mut Vec<OpenOut>, want: &[String]) {
    open.retain(|o| want.iter().any(|n| n == &o.name));
    for name in want {
        if open.iter().any(|o| &o.name == name) {
            continue;
        }
        match open_output(name) {
            Ok(o) => open.push(o),
            Err(e) => log::warn!("MIDI output {name}: {e}"),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn open_output(name: &str) -> Result<OpenOut, String> {
    let midi = midir::MidiOutput::new("SoundCraft").map_err(|e| e.to_string())?;
    let port = midi
        .ports()
        .into_iter()
        .find(|p| midi.port_name(p).ok().as_deref() == Some(name))
        .ok_or_else(|| format!("“{name}” is no longer connected"))?;
    let conn = midi.connect(&port, "soundcraft-out").map_err(|e| e.to_string())?;
    Ok(OpenOut { name: name.to_string(), conn: Mutex::new(conn) })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_output(_name: &str) -> Result<OpenOut, String> {
    Err("no MIDI devices on this platform".into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use midir::os::unix::VirtualOutput;

    /// A virtual ALSA port, when the sequencer allows one, is listed and delivers a note.
    #[test]
    fn virtual_output_is_an_input_and_delivers_a_note() {
        let midi = match midir::MidiOutput::new("soundcraft-test") {
            Ok(m) => m,
            Err(_) => return,
        };
        let mut conn = match midi.create_virtual("SoundCraft test keys") {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut hub = MidiHub::new();
        hub.refresh(&soundcraft_model::Session::default());
        if !hub.inputs().iter().any(|p| p.name.contains("SoundCraft test keys")) {
            return;
        }
        conn.send(&[0x90, 60, 100]).expect("send");
        std::thread::sleep(Duration::from_millis(80));
        let mut got = Vec::new();
        hub.link().input.drain(&mut got);
        assert!(got.iter().any(|e| e.status == 0x90 && e.data1 == 60 && e.data2 == 100), "virtual note did not arrive: {got:?}");
    }
}
