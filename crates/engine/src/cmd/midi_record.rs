//! MIDI recording: channel messages captured from MIDI inputs become clips on the record-enabled
//! MIDI and instrument tracks.
//!
//! The host stamps each message with the timeline sample it was played at and hands the take over
//! at stop. Notes still held at the end of the take end there. A punch range drops what fell
//! outside it. Loop recording (`pass`) splits the take into playlist lanes unless MIDI Merge is on,
//! in which case every pass folds into the same clip.
//!
//! The take assembly follows the approach in storytold/soundcraft#1085 (lenetwizz).

use super::{CommandSpec, always, bad, position_param, tracks_param};

use crate::{Engine, Result, cmd};
use serde_json::{Value, json};
use soundcraft_midi::ops::{QuantizeOptions, quantize};
use soundcraft_midi::{CtrlEvent, CtrlKind, Note, Sequence};
use soundcraft_model::{Clip, ClipContent, MidiEndpoint, Playlist, TrackId};
use soundcraft_time::{Range, Samples, TICKS_PER_QUARTER};
use std::collections::HashMap;

/// Messages kept from one take. A longer take keeps the earliest ones.
const MAX_EVENTS: usize = 500_000;
const MAX_PASSES: usize = 1_000;

/// One captured channel message.
struct Ev {
    at: Samples,
    status: u8,
    data1: u8,
    data2: u8,
    port: Option<String>,
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "midi.record_take",
        "Record MIDI Take",
        [],
        None,
        "{start, end, events: [[sample, status, data1, data2, port?], …], tracks?: [ids], pass?: samples, punch_start?, punch_end?, merge?: bool} — notes and controllers become a clip on each record-enabled MIDI or instrument track",
        always,
        record_take
    )]
}

fn record_take(e: &mut Engine, p: &Value) -> Result<Value> {
    let id = "midi.record_take";
    let start = position_param(e, id, p, "start")?.ok_or_else(|| bad(id, "`start` required"))?.max(0);
    let end = position_param(e, id, p, "end")?.ok_or_else(|| bad(id, "`end` required"))?;
    if end <= start {
        return Err(bad(id, "`end` must come after `start`"));
    }
    let events = parse_events(p.get("events")).ok_or_else(|| bad(id, "`events` must be a list of [sample, status, data1, data2]"))?;
    let tracks = target_tracks(e, p, id)?;
    let pass = position_param(e, id, p, "pass")?.filter(|n| *n > 0);
    let punch = punch_range(e, id, p)?;
    let merge = p.get("merge").and_then(Value::as_bool).unwrap_or_else(|| e.session().edit.flag("options.midi_merge"));
    let quantize_on = e.session().edit.flag("midi.input_quantize");
    if tracks.is_empty() {
        return Err(bad(id, "no record-enabled instrument or MIDI track"));
    }
    let mut clips = Vec::new();
    let mut notes = 0usize;
    let mut controllers = 0usize;
    let take = TakeIn { start, end, pass, punch, merge, quantize_on, events: &events };
    for t in tracks {
        let (n, c, ids) = place_track(e, t, &take);
        notes += n;
        controllers += c;
        clips.extend(ids);
    }
    Ok(json!({"clips": clips, "notes": notes, "controllers": controllers}))
}

fn target_tracks(e: &Engine, p: &Value, id: &str) -> Result<Vec<TrackId>> {
    let tracks = if p.get("tracks").is_some() || p.get("track").is_some() {
        tracks_param(e, id, p)?
    } else {
        e.session().tracks.iter().filter(|t| t.kind.is_midi() && t.mixer.record_arm && !t.inactive).map(|t| t.id).collect()
    };
    if let Some(t) = tracks.iter().find(|t| !e.session().track(**t).is_some_and(|x| x.kind.is_midi())) {
        return Err(bad(id, format!("track {} is not a MIDI or instrument track", t.0)));
    }
    Ok(tracks)
}

fn punch_range(e: &Engine, id: &str, p: &Value) -> Result<Option<Range>> {
    let a = position_param(e, id, p, "punch_start")?;
    let b = position_param(e, id, p, "punch_end")?;
    match (a, b) {
        (Some(a), Some(b)) if b > a => Ok(Some(Range::new(a, b))),
        (None, None) => Ok(None),
        _ => Err(bad(id, "`punch_start` and `punch_end` are a pair, with the end after the start")),
    }
}

fn parse_events(v: Option<&Value>) -> Option<Vec<Ev>> {
    let arr = v?.as_array()?;
    let mut out = Vec::with_capacity(arr.len().min(MAX_EVENTS));
    for ev in arr.iter().take(MAX_EVENTS) {
        out.push(parse_one(ev)?);
    }
    Some(out)
}

fn parse_one(ev: &Value) -> Option<Ev> {
    let a = ev.as_array()?;
    let at = a.first()?.as_i64()?;
    let byte = |i: usize| a.get(i).and_then(Value::as_u64).filter(|b| *b <= 0xFF).map(|b| b as u8);
    let port = a.get(4).and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty());
    Some(Ev { at, status: byte(1)?, data1: byte(2)? & 0x7F, data2: byte(3)? & 0x7F, port })
}

struct TakeIn<'a> {
    start: Samples,
    end: Samples,
    pass: Option<Samples>,
    punch: Option<Range>,
    merge: bool,
    quantize_on: bool,
    events: &'a [Ev],
}

fn place_track(e: &mut Engine, track: TrackId, take: &TakeIn<'_>) -> (usize, usize, Vec<soundcraft_model::ClipId>) {
    let kept: Vec<&Ev> = take.events.iter().filter(|ev| accepts(e, track, ev)).collect();
    if let Some(pass) = take.pass.filter(|_| !take.merge) {
        place_passes(e, track, take, pass, &kept)
    } else {
        let range = loop_or_span(take.start, take.end, take.pass.filter(|_| take.merge), take.punch);
        let folded = fold_events(&kept, take.start, take.pass.filter(|_| take.merge));
        place_one(e, track, range, take.quantize_on, take.merge, &folded)
    }
}

fn accepts(e: &Engine, track: TrackId, ev: &Ev) -> bool {
    if input_filtered(e, ev.status) {
        return false;
    }
    let Some(t) = e.session().track(track) else { return false };
    MidiEndpoint::of(&t.mixer.input).accepts(ev.port.as_deref(), ev.status & 0x0F)
}

fn input_filtered(e: &Engine, status: u8) -> bool {
    let key = match status & 0xF0 {
        0x80 | 0x90 => "midi.filter.notes",
        0xA0 => "midi.filter.poly_aftertouch",
        0xB0 => "midi.filter.cc",
        0xC0 => "midi.filter.program",
        0xD0 => "midi.filter.mono_aftertouch",
        0xE0 => "midi.filter.pitch_bend",
        _ => return true,
    };
    e.session().edit.flag(key)
}

fn loop_or_span(start: Samples, end: Samples, pass: Option<Samples>, punch: Option<Range>) -> Range {
    let span = match pass {
        Some(p) if p > 0 => Range::new(start, start.saturating_add(p)),
        _ => Range::new(start, end),
    };
    punch.and_then(|p| span.intersect(&p)).unwrap_or(span)
}

fn fold_events(events: &[&Ev], start: Samples, pass: Option<Samples>) -> Vec<(Samples, [u8; 3])> {
    events
        .iter()
        .map(|ev| {
            let at = match pass {
                Some(p) if p > 0 => start.saturating_add(ev.at.saturating_sub(start).rem_euclid(p)),
                _ => ev.at,
            };
            (at, [ev.status, ev.data1, ev.data2])
        })
        .collect()
}

fn place_passes(e: &mut Engine, track: TrackId, take: &TakeIn<'_>, pass: Samples, events: &[&Ev]) -> (usize, usize, Vec<soundcraft_model::ClipId>) {
    let mut notes = 0usize;
    let mut controllers = 0usize;
    let mut clips = Vec::new();
    let passes = usize::try_from(take.end.saturating_sub(take.start) / pass.max(1)).unwrap_or(1).clamp(1, MAX_PASSES);
    for i in 0..passes {
        let origin = take.start.saturating_add(pass.saturating_mul(i as i64));
        let mine: Vec<(Samples, [u8; 3])> = events
            .iter()
            .filter(|ev| ev.at >= origin && ev.at < origin.saturating_add(pass))
            .map(|ev| (take.start.saturating_add(ev.at.saturating_sub(origin)), [ev.status, ev.data1, ev.data2]))
            .collect();
        if mine.is_empty() {
            continue;
        }
        if i > 0 {
            new_playlist(e, track);
        }
        let range = loop_or_span(take.start, take.start.saturating_add(pass), None, take.punch);
        let (n, c, ids) = place_one(e, track, range, take.quantize_on, false, &mine);
        notes += n;
        controllers += c;
        clips.extend(ids);
    }
    (notes, controllers, clips)
}

fn new_playlist(e: &mut Engine, track: TrackId) {
    let s = e.session_mut();
    if let Some(t) = s.track_mut(track) {
        t.playlists.push(Playlist::new(format!("{}.{:02}", t.name, t.playlists.len() + 1)));
        t.active_playlist = t.playlists.len() - 1;
    }
}

fn place_one(
    e: &mut Engine,
    track: TrackId,
    range: Range,
    quantize_on: bool,
    merge: bool,
    events: &[(Samples, [u8; 3])],
) -> (usize, usize, Vec<soundcraft_model::ClipId>) {
    if range.end <= range.start {
        return (0, 0, Vec::new());
    }
    let mut seq = to_sequence(e.session(), range, events);
    if quantize_on {
        quantize(&mut seq.notes, &quantize_opts(e.session()));
        seq.sort();
    }
    if seq.notes.is_empty() && seq.ctrls.is_empty() {
        return (0, 0, Vec::new());
    }
    let (notes, ctrls) = (seq.notes.len(), seq.ctrls.len());
    let s = e.session_mut();
    let id = if merge { merge_into(s, track, range, seq) } else { Some(place_new(s, track, range, seq)) };
    (notes, ctrls, id.into_iter().collect())
}

fn quantize_opts(s: &soundcraft_model::Session) -> QuantizeOptions {
    QuantizeOptions {
        grid_ticks: (s.edit.value("midi.input_quantize.grid_ticks", (TICKS_PER_QUARTER / 4) as f64) as i64).clamp(1, TICKS_PER_QUARTER * 16),
        strength: (s.edit.value("midi.input_quantize.strength", 100.0) / 100.0).clamp(0.0, 1.0) as f32,
        swing: (s.edit.value("midi.input_quantize.swing", 0.0) / 100.0).clamp(0.0, 1.0) as f32,
        ..QuantizeOptions::default()
    }
}

fn place_new(s: &mut soundcraft_model::Session, track: TrackId, range: Range, seq: Sequence) -> soundcraft_model::ClipId {
    let cid = s.new_clip_id();
    let name = s.track(track).map(|x| x.name.clone()).unwrap_or_default();
    crate::edit::place_clip(s, track, Clip::midi(cid, name, range.start, range.len().max(1), seq));
    cid
}

fn merge_into(s: &mut soundcraft_model::Session, track: TrackId, range: Range, seq: Sequence) -> Option<soundcraft_model::ClipId> {
    let host = s
        .track(track)
        .and_then(|t| t.clips().iter().find(|c| matches!(c.content, ClipContent::Midi { .. }) && c.range().overlaps(&range)).map(|c| c.id));
    let Some(host) = host else { return Some(place_new(s, track, range, seq)) };
    let sr = s.sample_rate;
    let tempo = s.tempo.clone();
    let clip = s.find_clip_mut(host)?;
    let old_start = clip.start;
    if range.start < clip.start {
        let shift = tempo.samples_to_ticks(old_start, sr).saturating_sub(tempo.samples_to_ticks(range.start, sr));
        if let ClipContent::Midi { sequence } = &mut clip.content {
            for n in &mut sequence.notes {
                n.start = n.start.saturating_add(shift);
            }
            for c in &mut sequence.ctrls {
                c.tick = c.tick.saturating_add(shift);
            }
        }
        clip.start = range.start;
        clip.length = clip.length.saturating_add(old_start.saturating_sub(range.start));
    }
    let base = tempo.samples_to_ticks(clip.start, sr);
    let add = tempo.samples_to_ticks(range.start, sr).saturating_sub(base);
    if let ClipContent::Midi { sequence } = &mut clip.content {
        for mut n in seq.notes {
            n.start = n.start.saturating_add(add).max(0);
            sequence.notes.push(n);
        }
        for mut c in seq.ctrls {
            c.tick = c.tick.saturating_add(add).max(0);
            sequence.ctrls.push(c);
        }
        sequence.sort();
    }
    if range.end > clip.end() {
        clip.trim_end_to(range.end);
    }
    Some(host)
}

/// Turn time-stamped channel messages into a sequence whose ticks count from `range.start`.
/// A note still held at the end of the range ends there. Events outside the range are dropped.
fn to_sequence(s: &soundcraft_model::Session, range: Range, events: &[(Samples, [u8; 3])]) -> Sequence {
    let sr = s.sample_rate;
    let base = s.tempo.samples_to_ticks(range.start, sr);
    let tick = |at: Samples| s.tempo.samples_to_ticks(at.clamp(range.start, range.end), sr).saturating_sub(base).max(0);
    let end_tick = tick(range.end).max(1);
    let mut sorted: Vec<(Samples, [u8; 3])> = events.iter().copied().filter(|(at, _)| *at >= range.start && *at < range.end).collect();
    sorted.sort_by_key(|(at, _)| *at);
    let mut seq = Sequence::default();
    let mut open: HashMap<(u8, u8), (i64, u8)> = HashMap::new();
    for &(at, [status, d1, d2]) in &sorted {
        push_message(&mut seq, &mut open, status, d1, d2, tick(at), end_tick);
    }
    close_held(&mut seq, open, end_tick);
    seq.sort();
    seq
}

fn push_message(seq: &mut Sequence, open: &mut HashMap<(u8, u8), (i64, u8)>, status: u8, d1: u8, d2: u8, t: i64, end_tick: i64) {
    let ch = status & 0x0F;
    match status & 0xF0 {
        0x90 if d2 > 0 => {
            if let Some(prev) = open.insert((ch, d1), (t, d2)) {
                close_note(seq, ch, d1, prev, t, 64, end_tick);
            }
        }
        0x80 | 0x90 => {
            if let Some(on) = open.remove(&(ch, d1)) {
                let release = if status & 0xF0 == 0x80 { d2 } else { 64 };
                close_note(seq, ch, d1, on, t, release, end_tick);
            }
        }
        0xA0 => seq.ctrls.push(ctrl(t, ch, CtrlKind::PolyPressure(d1), i32::from(d2))),
        0xB0 => seq.ctrls.push(ctrl(t, ch, CtrlKind::Cc(d1), i32::from(d2))),
        0xC0 => seq.ctrls.push(ctrl(t, ch, CtrlKind::Program, i32::from(d1))),
        0xD0 => seq.ctrls.push(ctrl(t, ch, CtrlKind::ChannelPressure, i32::from(d1))),
        0xE0 => seq.ctrls.push(ctrl(t, ch, CtrlKind::PitchBend, ((i32::from(d2) << 7) | i32::from(d1)) - 8192)),
        _ => {}
    }
}

fn ctrl(tick: i64, channel: u8, kind: CtrlKind, value: i32) -> CtrlEvent {
    CtrlEvent { tick, channel, kind, value }
}

fn close_note(seq: &mut Sequence, ch: u8, pitch: u8, (from, vel): (i64, u8), to: i64, release: u8, end_tick: i64) {
    let length = to.saturating_sub(from).max(1).min(end_tick.saturating_sub(from).max(1));
    seq.notes.push(Note { pitch, velocity: vel.max(1), release_velocity: release, channel: ch, start: from, length });
}

fn close_held(seq: &mut Sequence, open: HashMap<(u8, u8), (i64, u8)>, end_tick: i64) {
    let mut held: Vec<_> = open.into_iter().collect();
    held.sort_by_key(|(k, v)| (v.0, *k));
    for ((ch, pitch), on) in held {
        close_note(seq, ch, pitch, on, end_tick, 64, end_tick);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundcraft_model::{ChannelFormat, TrackKind};

    /// 120 bpm at 48 kHz: one quarter note is 24 000 samples and 960 ticks.
    const Q: i64 = 24_000;

    fn engine() -> (Engine, TrackId, TrackId) {
        let mut e = Engine::new(soundcraft_model::Session::default());
        let s = e.session_mut();
        let keys = s.add_track(TrackKind::Instrument, ChannelFormat::Stereo, Some("Keys"));
        let other = s.add_track(TrackKind::Instrument, ChannelFormat::Stereo, Some("Lead"));
        s.track_mut(keys).unwrap().mixer.record_arm = true;
        (e, keys, other)
    }

    fn clip_seq(e: &Engine, t: TrackId) -> (i64, i64, Sequence) {
        let c = e.session().track(t).unwrap().clips().first().cloned().unwrap();
        let (start, len) = (c.start, c.range().len());
        let ClipContent::Midi { sequence } = c.content else { panic!("not MIDI") };
        (start, len, sequence)
    }

    #[test]
    fn played_notes_and_controllers_become_a_clip_on_the_armed_track() {
        let (mut e, keys, lead) = engine();
        let events = json!([
            [Q, 0x90, 60, 100],
            [Q + Q / 2, 0xB0, 64, 127],
            [2 * Q, 0x80, 60, 30],
            [2 * Q, 0x91, 64, 90],
            [3 * Q, 0x91, 64, 0],
            [3 * Q, 0xE0, 0, 0x40],
            [4 * Q, 0x90, 67, 80]
        ]);
        let r = e.execute("midi.record_take", &json!({"start": Q, "end": 5 * Q, "events": events})).unwrap();
        assert_eq!(r["notes"], 3);
        assert_eq!(r["controllers"], 2);
        let (start, len, seq) = clip_seq(&e, keys);
        assert_eq!((start, len), (Q, 4 * Q));
        let got: Vec<(u8, u8, i64, i64, u8)> = seq.notes.iter().map(|n| (n.pitch, n.channel, n.start, n.length, n.velocity)).collect();
        assert_eq!(
            got,
            vec![
                (60, 0, 0, TICKS_PER_QUARTER, 100),
                (64, 1, TICKS_PER_QUARTER, TICKS_PER_QUARTER, 90),
                (67, 0, 3 * TICKS_PER_QUARTER, TICKS_PER_QUARTER, 80)
            ]
        );
        assert_eq!(seq.notes[0].release_velocity, 30);
        assert!(e.session().track(lead).unwrap().clips().is_empty());
        e.execute("edit.undo", &json!({})).unwrap();
        assert!(e.session().track(keys).unwrap().clips().is_empty());
    }

    #[test]
    fn a_track_hears_only_its_channel_and_port() {
        let (mut e, keys, _) = engine();
        e.session_mut().track_mut(keys).unwrap().mixer.input = MidiEndpoint { device: Some("Pad"), channel: Some(0) }.route();
        let events = json!([
            [0, 0x90, 60, 100, "Pad"],
            [Q, 0x80, 60, 0, "Pad"],
            [0, 0x91, 62, 100, "Pad"],
            [Q, 0x81, 62, 0, "Pad"],
            [0, 0x90, 64, 100, "Keys"],
            [Q, 0x80, 64, 0, "Keys"]
        ]);
        e.execute("midi.record_take", &json!({"start": 0, "end": 2 * Q, "events": events})).unwrap();
        let (_, _, seq) = clip_seq(&e, keys);
        assert_eq!(seq.notes.iter().map(|n| n.pitch).collect::<Vec<_>>(), vec![60]);
    }

    #[test]
    fn punch_drops_what_was_played_outside_it_and_bad_input_is_refused() {
        let (mut e, keys, _) = engine();
        let events = json!([[0, 0x90, 50, 100], [Q / 2, 0x80, 50, 0], [Q, 0x90, 52, 100], [2 * Q, 0x80, 52, 0]]);
        e.execute("midi.record_take", &json!({"start": 0, "end": 3 * Q, "punch_start": Q, "punch_end": 2 * Q + 10, "events": events})).unwrap();
        let (_, _, seq) = clip_seq(&e, keys);
        assert_eq!(seq.notes.iter().map(|n| n.pitch).collect::<Vec<_>>(), vec![52]);
        assert!(e.execute("midi.record_take", &json!({"start": 0, "end": Q, "events": "x"})).is_err());
        e.session_mut().track_mut(keys).unwrap().mixer.record_arm = false;
        assert!(e.execute("midi.record_take", &json!({"start": 0, "end": Q, "events": []})).is_err());
    }

    #[test]
    fn merge_adds_to_the_clip_under_the_playhead() {
        let (mut e, keys, _) = engine();
        e.execute("midi.record_take", &json!({"start": 0, "end": 2 * Q, "events": [[0, 0x90, 60, 100], [Q, 0x80, 60, 0]]})).unwrap();
        e.execute("midi.record_take", &json!({"start": Q, "end": 3 * Q, "merge": true, "events": [[Q, 0x90, 64, 80], [2 * Q, 0x80, 64, 0]]}))
            .unwrap();
        assert_eq!(e.session().track(keys).unwrap().clips().len(), 1);
        let (_, _, seq) = clip_seq(&e, keys);
        let pitches: Vec<u8> = seq.notes.iter().map(|n| n.pitch).collect();
        assert_eq!(pitches, vec![60, 64]);
    }
}
