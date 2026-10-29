//! Live MIDI: events from hardware inputs, played on the instruments of record-enabled and
//! input-monitored MIDI and instrument tracks, and forwarded to a track's MIDI output.
//!
//! The audio host fills [`crate::MixEngine::midi_in`] before each block. Notes reach the
//! instrument at their sample offset. The sustain pedal (CC 64) holds note-offs back until it
//! lifts. The same events, plus notes the clips play, are copied to [`crate::MixEngine::midi_out`]
//! when the track has a MIDI output port.
//!
//! Port names live in [`MidiPorts`]. Slots are written once (when a device is first seen) and read
//! without locking, so the audio thread can look a name up.

use soundcraft_dsp::Plugin;
use soundcraft_model::{MidiEndpoint, Session, Track};
use std::sync::OnceLock;

/// Most ports one engine tells apart. Later ones still reach tracks that listen to every port.
pub const MAX_MIDI_PORTS: usize = 32;

/// A port index meaning "every open output" (clock and MTC) or "no port".
pub const ALL_PORTS: u8 = u8::MAX;

/// A channel message from a device, for this block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LiveMidi {
    /// Sample offset within the block.
    pub offset: u32,
    /// Index into [`MidiPorts`]. [`ALL_PORTS`] when the port has no slot.
    pub port: u8,
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
}

impl LiveMidi {
    /// MIDI channel 0..=15.
    pub fn channel(self) -> u8 {
        self.status & 0x0F
    }
}

/// A channel message for a hardware MIDI output, at a sample offset in the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MidiOut {
    pub offset: u32,
    pub port: u8,
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
}

/// Names of ports, by the index events carry.
#[derive(Debug)]
pub struct MidiPorts {
    names: Vec<OnceLock<String>>,
}

impl Default for MidiPorts {
    fn default() -> Self {
        MidiPorts { names: (0..MAX_MIDI_PORTS).map(|_| OnceLock::new()).collect() }
    }
}

impl MidiPorts {
    /// The index of `name`, giving it a free slot the first time. [`ALL_PORTS`] when all are taken.
    /// Not for the audio thread (it allocates the first time a name is seen).
    pub fn assign(&self, name: &str) -> u8 {
        for (i, slot) in self.names.iter().enumerate() {
            if slot.get_or_init(|| name.to_string()) == name {
                return u8::try_from(i).unwrap_or(ALL_PORTS);
            }
        }
        ALL_PORTS
    }

    pub fn name(&self, port: u8) -> Option<&str> {
        self.names.get(usize::from(port)).and_then(OnceLock::get).map(String::as_str)
    }

    /// The index of a name already assigned. Does not allocate.
    pub fn index_of(&self, name: &str) -> Option<u8> {
        self.names
            .iter()
            .enumerate()
            .find_map(|(i, slot)| (slot.get().map(String::as_str) == Some(name)).then_some(u8::try_from(i).unwrap_or(ALL_PORTS)))
    }
}

/// Whether `t` should sound live MIDI right now.
///
/// Record-enabled and input-monitored tracks always do. With Options › MIDI Thru, and nothing
/// armed or monitored, the selected MIDI or instrument track does (so a keyboard plays the track
/// you are looking at).
pub fn hears_live_midi(s: &Session, t: &Track) -> bool {
    if t.inactive || !t.kind.is_midi() {
        return false;
    }
    if t.mixer.record_arm || t.mixer.input_monitor {
        return true;
    }
    if !s.edit.flag("options.midi_thru") {
        return false;
    }
    let armed = s.tracks.iter().any(|x| !x.inactive && x.kind.is_midi() && (x.mixer.record_arm || x.mixer.input_monitor));
    !armed && s.edit.selected_tracks.contains(&t.id)
}

/// Kinds the MIDI Input Filter drops. The key is a static string, so the audio thread does not
/// allocate to look it up.
pub fn filtered(s: &Session, status: u8) -> bool {
    let key = match status & 0xF0 {
        0x80 | 0x90 => "midi.filter.notes",
        0xA0 => "midi.filter.poly_aftertouch",
        0xB0 => "midi.filter.cc",
        0xC0 => "midi.filter.program",
        0xD0 => "midi.filter.mono_aftertouch",
        0xE0 => "midi.filter.pitch_bend",
        _ => return true,
    };
    s.edit.flag(key)
}

/// 16 channels × 128 notes, one bit each. No heap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct NoteSet([u64; 32]);

impl NoteSet {
    fn index(channel: u8, note: u8) -> Option<(usize, u64)> {
        let i = usize::from(channel & 15) * 128 + usize::from(note & 127);
        Some((i >> 6, 1u64 << (i & 63)))
    }

    fn set(&mut self, channel: u8, note: u8, on: bool) {
        let Some((word, bit)) = Self::index(channel, note) else { return };
        let Some(w) = self.0.get_mut(word) else { return };
        if on { *w |= bit } else { *w &= !bit }
    }

    fn has(self, channel: u8, note: u8) -> bool {
        Self::index(channel, note).and_then(|(word, bit)| self.0.get(word).map(|w| w & bit != 0)).unwrap_or(false)
    }
}

/// Live notes this strip's instrument is holding, apart from notes the clips started.
#[derive(Debug, Default)]
pub(crate) struct LiveNotes {
    down: NoteSet,
    sustained: NoteSet,
    pedal: bool,
}

impl LiveNotes {
    pub(crate) fn clear(&mut self) {
        *self = LiveNotes::default();
    }

    /// Release what live input started. Clip notes are left to the scheduler.
    pub(crate) fn release(&mut self, inst: &mut dyn Plugin) {
        for ch in 0..16u8 {
            for note in 0..128u8 {
                if self.down.has(ch, note) || self.sustained.has(ch, note) {
                    inst.note_off(0, note);
                }
            }
        }
        self.clear();
    }
}

/// Where clip and live messages for one track are copied, when it has a MIDI output.
pub(crate) struct OutTap<'a> {
    pub port: u8,
    pub channel: Option<u8>,
    pub events: &'a mut Vec<MidiOut>,
}

impl OutTap<'_> {
    pub(crate) fn push(&mut self, offset: u32, status: u8, data1: u8, data2: u8) {
        if self.port == ALL_PORTS {
            return;
        }
        let status = match self.channel {
            Some(c) if (0x80..0xF0).contains(&status) => (status & 0xF0) | (c & 0x0F),
            _ => status,
        };
        self.events.push(MidiOut { offset, port: self.port, status, data1, data2 });
    }
}

/// The output port a track sends to, if that port has been seen.
pub(crate) fn output_port(t: &Track, ports: &MidiPorts) -> Option<(u8, Option<u8>)> {
    let spec = MidiEndpoint::of(&t.mixer.midi_output);
    let name = spec.device?;
    Some((ports.index_of(name).unwrap_or(ALL_PORTS), spec.channel))
}

/// Play `events` on this track's instrument and copy them to its MIDI output.
pub(crate) fn play_live(
    s: &Session,
    t: &Track,
    inst: &mut dyn Plugin,
    live: &mut LiveNotes,
    events: &[LiveMidi],
    ports: &MidiPorts,
    out: &mut Vec<MidiOut>,
) {
    if !hears_live_midi(s, t) {
        return;
    }
    let spec = MidiEndpoint::of(&t.mixer.input);
    let (out_port, out_ch) = output_port(t, ports).unwrap_or((ALL_PORTS, None));
    let mut tap = OutTap { port: out_port, channel: out_ch, events: out };
    for ev in events {
        if !kept(s, &spec, ports, ev) {
            continue;
        }
        apply_one(inst, live, ev, &mut tap);
    }
}

/// Copy live input to a MIDI output when the track has no instrument of its own.
pub(crate) fn forward_live(s: &Session, t: &Track, events: &[LiveMidi], ports: &MidiPorts, out: &mut Vec<MidiOut>) {
    if !hears_live_midi(s, t) {
        return;
    }
    let spec = MidiEndpoint::of(&t.mixer.input);
    let (out_port, out_ch) = output_port(t, ports).unwrap_or((ALL_PORTS, None));
    let mut tap = OutTap { port: out_port, channel: out_ch, events: out };
    for ev in events {
        if kept(s, &spec, ports, ev) {
            tap.push(ev.offset, ev.status, ev.data1, ev.data2);
        }
    }
}

fn kept(s: &Session, spec: &MidiEndpoint<'_>, ports: &MidiPorts, ev: &LiveMidi) -> bool {
    !filtered(s, ev.status) && spec.accepts(ports.name(ev.port), ev.channel())
}

fn apply_one(inst: &mut dyn Plugin, live: &mut LiveNotes, ev: &LiveMidi, tap: &mut OutTap<'_>) {
    let ch = ev.channel();
    let off = ev.offset;
    match ev.status & 0xF0 {
        0x90 if ev.data2 > 0 => {
            inst.note_on(off as usize, ev.data1, ev.data2);
            live.down.set(ch, ev.data1, true);
            live.sustained.set(ch, ev.data1, false);
            tap.push(off, ev.status, ev.data1, ev.data2);
        }
        0x80 | 0x90 => {
            live.down.set(ch, ev.data1, false);
            if live.pedal {
                live.sustained.set(ch, ev.data1, true);
            } else {
                inst.note_off(off as usize, ev.data1);
            }
            tap.push(off, ev.status, ev.data1, ev.data2);
        }
        0xB0 if ev.data1 == 64 => {
            let down = ev.data2 >= 64;
            if live.pedal && !down {
                release_sustained(inst, live, off);
            }
            live.pedal = down;
            inst.midi(off as usize, [0xB0 | ch, 64, ev.data2]);
            tap.push(off, ev.status, ev.data1, ev.data2);
        }
        0xB0 | 0xA0 | 0xC0 | 0xD0 | 0xE0 => {
            let d2 = if matches!(ev.status & 0xF0, 0xC0 | 0xD0) { 0 } else { ev.data2 };
            inst.midi(off as usize, [ev.status, ev.data1, d2]);
            tap.push(off, ev.status, ev.data1, ev.data2);
        }
        _ => {}
    }
}

fn release_sustained(inst: &mut dyn Plugin, live: &mut LiveNotes, off: u32) {
    for ch in 0..16u8 {
        for note in 0..128u8 {
            if live.sustained.has(ch, note) && !live.down.has(ch, note) {
                inst.note_off(off as usize, note);
            }
        }
    }
    live.sustained = NoteSet::default();
}

/// All-notes-off for a hardware port, queued at the start of a block after a seek.
pub(crate) fn panic(port: u8, out: &mut Vec<MidiOut>) {
    if port == ALL_PORTS {
        return;
    }
    for ch in 0..16u8 {
        out.push(MidiOut { offset: 0, port, status: 0xB0 | ch, data1: 123, data2: 0 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundcraft_model::{ChannelFormat, TrackKind};

    #[test]
    fn ports_keep_the_first_index_of_a_name() {
        let p = MidiPorts::default();
        assert_eq!(p.assign("Keys"), 0);
        assert_eq!(p.assign("Pad"), 1);
        assert_eq!(p.assign("Keys"), 0);
        assert_eq!(p.name(0), Some("Keys"));
        assert_eq!(p.index_of("Pad"), Some(1));
        assert_eq!(p.index_of("Missing"), None);
    }

    #[test]
    fn thru_uses_the_selection_only_when_nothing_is_armed() {
        let mut s = Session::default();
        let keys = s.add_track(TrackKind::Instrument, ChannelFormat::Stereo, Some("Keys"));
        let lead = s.add_track(TrackKind::Midi, ChannelFormat::Mono, Some("Lead"));
        s.edit.selected_tracks = vec![keys];
        assert!(!hears_live_midi(&s, s.track(keys).unwrap()), "thru is off until the option is set");
        s.edit.set_flag("options.midi_thru", true);
        assert!(hears_live_midi(&s, s.track(keys).unwrap()));
        assert!(!hears_live_midi(&s, s.track(lead).unwrap()));
        s.track_mut(lead).unwrap().mixer.record_arm = true;
        assert!(!hears_live_midi(&s, s.track(keys).unwrap()), "an armed track takes the input");
        assert!(hears_live_midi(&s, s.track(lead).unwrap()));
    }
}
