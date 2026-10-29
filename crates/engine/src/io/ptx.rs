//! Open a Pro Tools session or template into the current document.
//!
//! The reader is `soundcraft-ptx` (`docs/formats/ptx.md`). Mono and stereo clips, markers,
//! media paths, and the sample rate are the fields an interchange plan of schema 1 can
//! carry. Track kind, folders, I/O paths, plug-in names, and the picture rate have no
//! column there, so they are applied on the session directly.

use super::{Engine, EngineError, Path, Result};
use serde::Serialize;
use serde_json::{Value, json};
use soundcraft_midi::{Note, Sequence};
use soundcraft_model::{
    BitDepthSetting, ChannelFormat, Clip, Insert, MarkerKind, OutputPath, Route, Source, SourceId, TrackId, TrackKind, truncate_chars,
};
use soundcraft_ptx::{Document, Lane, Region, Role};
use soundcraft_time::{FrameRate, MAX_POSITION, SampleRate, TempoMap};

const MAX_FILE: u64 = soundcraft_ptx::MAX_FILE_BYTES;

/// What an import restored, and what it did not.
#[derive(Debug, Clone, Serialize)]
pub struct Restored {
    pub audio_tracks: usize,
    pub midi_tracks: usize,
    pub folders: usize,
    pub clips: usize,
    pub markers: usize,
    pub busses: usize,
    pub plugin_names: usize,
    pub sample_rate: u32,
}

/// A Pro Tools file opened as the current session. `notes` is the fidelity report.
#[derive(Debug, Clone)]
pub struct ProToolsOpened {
    pub missing: Vec<String>,
    pub notes: Vec<String>,
    pub restored: Restored,
}

impl ProToolsOpened {
    pub fn to_json(&self, name: &str, tracks: usize) -> Value {
        json!({
            "name": name,
            "tracks": tracks,
            "sample_rate": self.restored.sample_rate,
            "missing": self.missing,
            "fidelity": self.notes,
            "restored": self.restored,
        })
    }
}

/// Replace the open session with the Pro Tools file at `path`.
pub fn open_pro_tools(e: &mut Engine, path: &Path) -> Result<ProToolsOpened> {
    let bytes = super::read_limited(path, MAX_FILE)?;
    let name = path.to_str().unwrap_or("Session");
    let (mut session, mut notes) = session_from(&bytes, name)?;
    session.folder = path.parent().map(std::path::Path::to_path_buf);
    let missing = load_media(&mut session, path);
    adopt(e, session, missing, &mut notes)
}

/// Replace the open session with Pro Tools bytes (the browser has no path to resolve media against).
pub fn open_pro_tools_bytes(e: &mut Engine, name: &str, bytes: &[u8]) -> Result<ProToolsOpened> {
    let (session, mut notes) = session_from(bytes, name)?;
    let missing = session.sources.iter().map(|s| s.path.clone()).collect();
    adopt(e, session, missing, &mut notes)
}

fn adopt(e: &mut Engine, session: soundcraft_model::Session, missing: Vec<String>, notes: &mut Vec<String>) -> Result<ProToolsOpened> {
    let restored = restored_of(&session);
    e.replace_session(session);
    e.path = None;
    for note in notes.iter() {
        e.message(format!("import: {note}"));
    }
    Ok(ProToolsOpened { missing, notes: std::mem::take(notes), restored })
}

fn session_from(bytes: &[u8], name: &str) -> Result<(soundcraft_model::Session, Vec<String>)> {
    let doc = soundcraft_ptx::parse(bytes).map_err(|err| EngineError::Io(err.to_string()))?;
    let rate = doc.sample_rate.and_then(|hz| SampleRate::new(hz).ok()).unwrap_or(SampleRate::HZ_48000);
    let mut notes = Vec::new();
    if doc.sample_rate.is_none() {
        notes.push("sample rate was not in the file; using 48000".to_string());
    }
    let mut title = stem(name);
    truncate_chars(&mut title, soundcraft_model::MAX_NAME_CHARS);
    if title.is_empty() {
        title = "Session".to_string();
    }
    let mut session = soundcraft_model::Session::new(title, rate);
    session.bit_depth = BitDepthSetting::Int24;
    apply_picture(&mut session, doc.frame_rate_code, &mut notes);
    let ids = add_lanes(&mut session, &doc.lanes, &mut notes);
    add_sources(&mut session, &doc);
    place_audio(&mut session, &doc, &ids, &mut notes);
    place_markers(&mut session, &doc, &mut notes);
    place_paths(&mut session, &doc, &ids, &mut notes);
    place_plugins(&mut session, &doc.plugins, &ids, &mut notes);
    place_midi(&mut session, &doc, &ids, rate, &mut notes);
    if let Some(version) = doc.version {
        notes.push(format!("version word {version} on the scale the 2019 reader used for Pro Tools 5–12"));
    }
    notes.extend(doc.notes);
    standing(name, &mut notes);
    Ok((session, notes))
}

fn standing(name: &str, notes: &mut Vec<String>) {
    notes.push("tempo and meter were not restored (120 BPM, 4/4)".to_string());
    notes.push("session bit depth was not restored (24-bit)".to_string());
    notes.push("track colors were not restored".to_string());
    notes.push("sends, hardware inputs, fade curves, clip gain, and volume or pan automation were not restored".to_string());
    notes.push("aux, master, and instrument tracks are not distinguished from audio or MIDI".to_string());
    if soundcraft_ptx::is_pro_tools_path(name) && name.to_ascii_lowercase().ends_with(".ptxt") {
        notes.push("opened a template; the same records are read as a session".to_string());
    }
    notes.push("saving writes a SoundCraft session and does not modify the Pro Tools file".to_string());
}

fn apply_picture(session: &mut soundcraft_model::Session, code: Option<u32>, notes: &mut Vec<String>) {
    let Some(code) = code else {
        notes.push("picture rate was not in the file".to_string());
        return;
    };
    let Some((rate, exact)) = picture_rate(code) else {
        notes.push(format!("picture-rate code {code} is not a known rate"));
        return;
    };
    session.frame_rate = rate;
    if !exact {
        notes.push(format!("picture-rate code {code} has no separate drop-frame rate here; using {}", rate.label()));
    }
}

fn picture_rate(code: u32) -> Option<(FrameRate, bool)> {
    Some(match code {
        0x01 => (FrameRate::Fps24, true),
        0x02 => (FrameRate::Fps25, true),
        0x03 => (FrameRate::Fps29_97, true),
        0x05 => (FrameRate::Fps29_97Drop, true),
        0x07 => (FrameRate::Fps30, true),
        0x08 => (FrameRate::Fps30, false),
        0x09 => (FrameRate::Fps23_976, true),
        0x0a => (FrameRate::Fps50, true),
        0x0b => (FrameRate::Fps59_94, true),
        0x0c => (FrameRate::Fps59_94Drop, true),
        0x0d => (FrameRate::Fps60, true),
        0x0e => (FrameRate::Fps60, false),
        0x0f => (FrameRate::Fps47_952, true),
        0x10 => (FrameRate::Fps48, true),
        0x11 => (FrameRate::Fps100, true),
        0x12 => (FrameRate::Fps119_88, true),
        0x13 => (FrameRate::Fps119_88, false),
        0x14 => (FrameRate::Fps120, true),
        0x15 => (FrameRate::Fps120, false),
        _ => return None,
    })
}

fn add_lanes(session: &mut soundcraft_model::Session, lanes: &[Lane], notes: &mut Vec<String>) -> Vec<TrackId> {
    let mut ids = Vec::new();
    for lane in lanes {
        let kind = match lane.role {
            Role::Audio => TrackKind::Audio,
            Role::Midi => TrackKind::Midi,
            Role::Folder => TrackKind::Folder,
        };
        let (format, approx) = layout(lane.channels);
        if lane.role == Role::Audio && approx {
            notes.push(format!("{} is {} channels; using {}", lane.name, lane.channels, format.label()));
        }
        let mut name = lane.name.clone();
        truncate_chars(&mut name, soundcraft_model::MAX_NAME_CHARS);
        if name.is_empty() {
            name = kind.default_name().to_string();
        }
        ids.push(session.add_track(kind, format, Some(&name)));
    }
    ids
}

fn layout(channels: u16) -> (ChannelFormat, bool) {
    let format = match channels {
        1 => ChannelFormat::Mono,
        2 => ChannelFormat::Stereo,
        3 => ChannelFormat::Lcr,
        4 => ChannelFormat::Quad,
        5 => ChannelFormat::Surround50,
        6 => ChannelFormat::Surround51,
        8 => ChannelFormat::Surround71,
        _ => ChannelFormat::Stereo,
    };
    (format, channels == 0 || format.channels() != usize::from(channels.max(1)))
}

fn add_sources(session: &mut soundcraft_model::Session, doc: &Document) {
    for media in &doc.media {
        push_source(session, &media.name, media.frames.unwrap_or(1), 1);
    }
}

fn push_source(session: &mut soundcraft_model::Session, path: &str, frames: u64, channels: u16) -> SourceId {
    let id = SourceId(session.alloc());
    let mut name = stem(path);
    if name.is_empty() {
        name = "Audio".to_string();
    }
    truncate_chars(&mut name, soundcraft_model::MAX_NAME_CHARS);
    session.sources.push(Source {
        id,
        name,
        path: path.to_string(),
        channels: channels.max(1),
        frames: frames.max(1),
        sample_rate: session.sample_rate.hz(),
        format: if path.to_ascii_lowercase().ends_with(".aif") || path.to_ascii_lowercase().ends_with(".aiff") {
            soundcraft_audio_io::FileFormat::Aiff
        } else {
            soundcraft_audio_io::FileFormat::Wav
        },
        time_reference: 0,
        unsaved: false,
    });
    id
}

fn place_audio(session: &mut soundcraft_model::Session, doc: &Document, ids: &[TrackId], notes: &mut Vec<String>) {
    for place in &doc.placements {
        let Some(region) = doc.regions.get(place.region_index as usize) else {
            notes.push(format!("clip index {} is not in the clip list", place.region_index));
            continue;
        };
        let Some(track) = ids.get(place.lane_index).copied() else {
            notes.push(format!("clip {} is on a playlist that was not imported", region.name));
            continue;
        };
        if !place_one(session, doc, track, region, place.start, place.muted, notes) {
            continue;
        }
    }
    for id in ids {
        if let Some(track) = session.track_mut(*id)
            && let Some(list) = track.playlist_mut()
        {
            list.clips.sort_by_key(|c| c.start);
        }
    }
}

fn place_one(
    session: &mut soundcraft_model::Session,
    doc: &Document,
    track: TrackId,
    region: &Region,
    start: u64,
    muted: bool,
    notes: &mut Vec<String>,
) -> bool {
    let Some(length) = fit_u(region.length) else {
        notes.push(format!("clip {} has a length this session cannot store", region.name));
        return false;
    };
    if length <= 0 {
        notes.push(format!("clip {} has no length", region.name));
        return false;
    }
    let start = start.saturating_sub(region.sync);
    let Some(start) = fit_u(start) else {
        notes.push(format!("clip {} starts past the end of the timeline", region.name));
        return false;
    };
    let offset = fit_u(region.offset).unwrap_or(0);
    let channels = session.track(track).map(|t| u16::try_from(t.channels()).unwrap_or(1)).unwrap_or(1);
    let source = source_for(session, doc, region, channels);
    widen(session, source, offset, length, channels);
    let mut clip = Clip::audio(session.new_clip_id(), clipped_name(&region.name), source, offset, start, length);
    clip.muted = muted;
    if region.sync > 0 && region.sync < region.length {
        clip.sync_point = fit_u(region.sync).unwrap_or(0).min(length);
    }
    if let Some(list) = session.track_mut(track).and_then(|t| t.playlist_mut()) {
        list.clips.push(clip);
    }
    true
}

fn source_for(session: &mut soundcraft_model::Session, doc: &Document, region: &Region, channels: u16) -> SourceId {
    let index = region
        .media_index
        .and_then(|i| usize::try_from(i).ok())
        .filter(|i| doc.media.get(*i).is_some())
        .or_else(|| (doc.media.len() == 1).then_some(0));
    if let Some(index) = index
        && let Some(id) = session.sources.get(index).map(|s| s.id)
    {
        return id;
    }
    let name = if region.name.is_empty() { "Audio.wav".to_string() } else { format!("{}.wav", region.name) };
    push_source(session, &name, region.offset.saturating_add(region.length), channels)
}

fn widen(session: &mut soundcraft_model::Session, id: SourceId, offset: i64, length: i64, channels: u16) {
    let Some(source) = session.sources.iter_mut().find(|s| s.id == id) else { return };
    let need = u64::try_from(offset.saturating_add(length)).unwrap_or(u64::MAX);
    source.frames = source.frames.max(need).max(1);
    if source.channels < channels {
        source.channels = channels;
    }
}

fn place_markers(session: &mut soundcraft_model::Session, doc: &Document, notes: &mut Vec<String>) {
    for marker in &doc.markers {
        let Some(start) = fit_i(marker.start) else {
            notes.push(format!("memory location {} is outside the timeline", marker.name));
            continue;
        };
        let end = fit_i(marker.end).unwrap_or(start).max(start);
        let kind = if end > start { MarkerKind::Selection } else { MarkerKind::Marker };
        let mut name = marker.name.clone();
        truncate_chars(&mut name, soundcraft_model::MAX_NAME_CHARS);
        if name.is_empty() {
            name = "Marker".to_string();
        }
        let id = session.add_marker(&name, kind, start, end);
        if let Some(found) = session.markers.iter_mut().find(|m| m.id == id) {
            found.comments = marker.comment.clone();
            truncate_chars(&mut found.comments, soundcraft_model::MAX_COMMENT_CHARS);
        }
    }
}

fn place_paths(session: &mut soundcraft_model::Session, doc: &Document, ids: &[TrackId], notes: &mut Vec<String>) {
    for path in &doc.paths {
        let channels = if path.channels == 0 { 2 } else { path.channels };
        let (format, approx) = layout(channels);
        if path.channels == 0 || approx {
            notes.push(format!("I/O path {} width is unspecified; using {}", path.name, format.label()));
        }
        let mut name = path.name.clone();
        truncate_chars(&mut name, soundcraft_model::MAX_NAME_CHARS);
        session.add_bus(&name, format);
        session.outputs.push(OutputPath { name, first_channel: 0, format });
    }
    if doc.outputs.is_empty() {
        if !doc.paths.is_empty() {
            notes.push("bus paths were read; no per-track output record was present".to_string());
        }
        return;
    }
    let audio = audio_ids(session, ids);
    if doc.outputs.len() != audio.len() {
        notes.push(format!("{} output record(s) for {} audio track(s); assigned in playlist order", doc.outputs.len(), audio.len()));
    }
    for (track, name) in audio.iter().zip(&doc.outputs) {
        let mut bus_name = name.clone();
        truncate_chars(&mut bus_name, soundcraft_model::MAX_NAME_CHARS);
        let id = session.bus_by_name(&bus_name).map(|b| b.id).unwrap_or_else(|| session.add_bus(&bus_name, ChannelFormat::Stereo));
        if let Some(track) = session.track_mut(*track) {
            track.mixer.output = Route::Bus(id);
        }
    }
}

fn place_plugins(session: &mut soundcraft_model::Session, names: &[String], ids: &[TrackId], notes: &mut Vec<String>) {
    if names.is_empty() {
        notes.push("no plug-in names were found".to_string());
        return;
    }
    notes.push("plug-in parameters were not restored; names from plug-in records are placed on insert slots in list order".to_string());
    let audio = audio_ids(session, ids);
    let mut left = names.to_vec();
    for id in audio {
        let Some(track) = session.track_mut(id) else { continue };
        for slot in &mut track.mixer.inserts {
            if slot.is_some() || left.is_empty() {
                continue;
            }
            let name = left.remove(0);
            let mut label = format!("ptx:{name}");
            truncate_chars(&mut label, soundcraft_model::MAX_NAME_CHARS);
            *slot = Some(Insert::new(label));
        }
    }
    if !left.is_empty() {
        notes.push(format!("{} plug-in name(s) did not fit on an insert slot", left.len()));
    }
}

fn place_midi(session: &mut soundcraft_model::Session, doc: &Document, ids: &[TrackId], rate: SampleRate, notes: &mut Vec<String>) {
    if doc.midi.is_empty() {
        return;
    }
    notes.push("MIDI clips start at sample 0; their timeline position was not a decoded record. Ticks use 120 BPM.".to_string());
    let tracks = midi_ids(session, ids);
    if tracks.is_empty() {
        notes.push("MIDI notes were found and there is no MIDI track to hold them".to_string());
        return;
    }
    let map = TempoMap::default();
    for (index, chunk) in doc.midi.iter().enumerate() {
        let track = tracks.get(index).copied().unwrap_or(tracks[0]);
        if tracks.get(index).is_none() {
            notes.push("an extra MIDI chunk shares the first MIDI track".to_string());
        }
        let mut sequence = Sequence::default();
        let mut end_tick = 1i64;
        for note in &chunk.notes {
            let start = model_tick(note.tick);
            let length = model_tick(note.length).max(1);
            end_tick = end_tick.max(start.saturating_add(length));
            sequence.notes.push(Note {
                pitch: note.pitch.min(127),
                velocity: note.velocity.clamp(1, 127),
                release_velocity: 64,
                channel: 0,
                start,
                length,
            });
        }
        sequence.sort();
        let length = map.tick_to_samples(end_tick, rate).max(1);
        let clip = Clip::midi(session.new_clip_id(), "MIDI", 0, length, sequence);
        if let Some(list) = session.track_mut(track).and_then(|t| t.playlist_mut()) {
            list.clips.push(clip);
        }
    }
}

fn audio_ids(session: &soundcraft_model::Session, ids: &[TrackId]) -> Vec<TrackId> {
    ids.iter().copied().filter(|id| session.track(*id).is_some_and(|t| t.kind == TrackKind::Audio)).collect()
}

fn midi_ids(session: &soundcraft_model::Session, ids: &[TrackId]) -> Vec<TrackId> {
    ids.iter().copied().filter(|id| session.track(*id).is_some_and(|t| t.kind == TrackKind::Midi)).collect()
}

fn load_media(session: &mut soundcraft_model::Session, file: &Path) -> Vec<String> {
    let refs: Vec<(SourceId, String)> = session.sources.iter().map(|s| (s.id, s.path.clone())).collect();
    let mut missing = Vec::new();
    let mut files = super::session::DecodedFiles::new();
    let rate = session.sample_rate.hz();
    for (id, path) in refs {
        match super::stored_path(&path, file) {
            Ok(resolved) => match super::session::decode_file(&mut files, &resolved, rate) {
                Ok(audio) => {
                    session.pool.insert(id, audio);
                }
                Err(err) => missing.push(err),
            },
            Err(why) => missing.push(format!("{path}: {why}")),
        }
    }
    missing
}

fn restored_of(session: &soundcraft_model::Session) -> Restored {
    Restored {
        audio_tracks: session.tracks.iter().filter(|t| t.kind == TrackKind::Audio).count(),
        midi_tracks: session.tracks.iter().filter(|t| t.kind == TrackKind::Midi).count(),
        folders: session.tracks.iter().filter(|t| t.kind == TrackKind::Folder).count(),
        clips: session.tracks.iter().map(|t| t.playlist().map(|p| p.clips.len()).unwrap_or(0)).sum(),
        markers: session.markers.len(),
        busses: session.busses.len(),
        plugin_names: session.tracks.iter().map(|t| t.mixer.inserts.iter().filter(|s| s.is_some()).count()).sum(),
        sample_rate: session.sample_rate.hz(),
    }
}

fn fit_u(value: u64) -> Option<i64> {
    let value = i64::try_from(value).ok()?;
    (0..=MAX_POSITION).contains(&value).then_some(value)
}

fn fit_i(value: i64) -> Option<i64> {
    (0..=MAX_POSITION).contains(&value).then_some(value)
}

fn model_tick(pt_ticks: u64) -> i64 {
    let ticks = pt_ticks / 1_000;
    i64::try_from(ticks).unwrap_or(i64::MAX)
}

fn stem(path: &str) -> String {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    base.rsplit_once('.').map(|(name, _)| name).unwrap_or(base).to_string()
}

fn clipped_name(name: &str) -> String {
    let mut name = if name.is_empty() { "Clip".to_string() } else { name.to_string() };
    truncate_chars(&mut name, soundcraft_model::MAX_NAME_CHARS);
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;

    #[test]
    fn a_synthetic_template_opens_with_a_fidelity_report() {
        let dir = std::env::temp_dir().join(format!("soundcraft-ptx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Demo.ptxt");
        std::fs::write(&path, soundcraft_ptx::template_bytes()).unwrap();
        let mut engine = Engine::default();
        let opened = open_pro_tools(&mut engine, &path).unwrap();
        assert_opened_session(&engine, &opened);
        let again = crate::io::open_session(&mut Engine::default(), &path).unwrap();
        assert!(again.iter().any(|m| m.contains("Kick.wav")));
        let junk = dir.join("nope.ptx");
        std::fs::write(&junk, b"this is not a session").unwrap();
        assert!(open_pro_tools(&mut Engine::default(), &junk).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn assert_opened_session(engine: &Engine, opened: &ProToolsOpened) {
        assert!(engine.path.is_none());
        assert_eq!(engine.session().sample_rate.hz(), 48_000);
        assert_eq!(engine.session().frame_rate, FrameRate::Fps23_976);
        let names: Vec<_> = engine.session().tracks.iter().map(|t| (t.name.as_str(), t.kind)).collect();
        assert_eq!(names, vec![("Kick", TrackKind::Audio), ("Room", TrackKind::Audio), ("Drums", TrackKind::Folder), ("Keys", TrackKind::Midi)]);
        assert_timeline(engine);
        assert_mix(engine);
        assert!(opened.notes.iter().any(|n| n.contains("tempo")));
        assert!(opened.notes.iter().any(|n| n.contains("bit depth")));
        assert!(opened.missing.iter().any(|m| m.contains("Kick.wav")));
        assert_eq!(opened.restored.audio_tracks, 2);
        assert_eq!(opened.restored.clips, 2);
    }

    fn assert_timeline(engine: &Engine) {
        assert_eq!(engine.session().tracks[0].channels(), 1);
        assert_eq!(engine.session().tracks[1].channels(), 2);
        let kick = engine.session().tracks[0].playlist().unwrap();
        assert_eq!(kick.clips.len(), 1);
        assert_eq!(kick.clips[0].name, "Kick_01");
        assert_eq!(kick.clips[0].start, 48_000);
        assert_eq!(kick.clips[0].length, 24_000);
        assert!(engine.session().tracks[1].playlist().unwrap().clips.is_empty());
        let midi = engine.session().tracks[3].playlist().unwrap();
        assert_eq!(midi.clips.len(), 1);
        let soundcraft_model::ClipContent::Midi { sequence } = &midi.clips[0].content else { panic!("midi") };
        assert_eq!(sequence.notes.len(), 2);
        assert_eq!(sequence.notes[0].pitch, 60);
        assert_eq!(sequence.notes[0].start, 0);
        assert_eq!(sequence.notes[1].start, 960);
    }

    fn assert_mix(engine: &Engine) {
        assert_eq!(engine.session().markers.len(), 1);
        assert_eq!(engine.session().markers[0].name, "Verse");
        assert_eq!(engine.session().markers[0].start, 96_000);
        assert!(engine.session().busses.iter().any(|b| b.name == "Reverb"));
        assert!(matches!(engine.session().tracks[0].mixer.output, Route::Bus(_)));
        assert!(engine.session().tracks[0].mixer.inserts.iter().any(|s| s.as_ref().is_some_and(|i| i.plugin == "ptx:Channel EQ")));
    }
}
