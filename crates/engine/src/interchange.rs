//! AAF and DAWproject session interchange.
//!
//! The AAF reader is the one in `soundcraft-interchange` (Vic Herrera's structured-storage walk,
//! extended with fades, clip gain, volume/pan, markers and embedded essence). Export writes that
//! same subset, or a DAWproject zip when the path ends in `.dawproject`.

use crate::{EngineError, Result};
use serde_json::{Value, json};
use soundcraft_audio_io::{BitDepth, EncodeOptions, FileFormat};
use soundcraft_interchange::aaf::{AafOptions, MediaError, MediaInfo, MediaResolver, read_aaf, wav_info};
use soundcraft_interchange::{
    Curve, ImportClip, ImportTrack, Marker, Media, Plan, Point, Rational, TimecodeRate, build_session, read_dawproject, write_aaf, write_dawproject,
};
use soundcraft_model::{AutoParam, ClipContent, FadeShape, MarkerKind, Session, SourceAudio, SourceId, TrackId, TrackKind};
use soundcraft_time::MAX_POSITION;
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn bad(message: impl Into<String>) -> EngineError {
    EngineError::Io(message.into())
}

fn rate_of(session: &Session) -> u32 {
    session.sample_rate.hz()
}

fn rat(samples: i64, rate: u32) -> Rational {
    Rational(samples.max(0), i64::from(rate.max(1)))
}

fn curve(shape: FadeShape) -> Curve {
    Curve::from_shape(shape)
}

/// Build an interchange plan from the audio tracks named in `which` (every audio track when empty).
pub(crate) fn plan_from_session(session: &Session, which: &[TrackId], embed: bool) -> Result<Plan> {
    let rate = rate_of(session);
    let chosen: Vec<_> = session.tracks.iter().filter(|t| t.kind == TrackKind::Audio && (which.is_empty() || which.contains(&t.id))).collect();
    if chosen.is_empty() {
        return Err(bad("no audio tracks to export"));
    }
    let mut sources = Vec::new();
    let mut keys: HashMap<SourceId, String> = HashMap::new();
    let mut tracks = Vec::new();
    for track in chosen {
        let mut clips = Vec::new();
        for clip in track.clips() {
            let ClipContent::Audio { source, offset } = clip.content else { continue };
            let src = session.source(source).ok_or_else(|| bad(format!("{}: missing source", clip.name)))?;
            if src.channels != u16::try_from(track.channels()).unwrap_or(0) {
                return Err(bad(format!("{}: clip channel count does not match the track", clip.name)));
            }
            let key = if let Some(key) = keys.get(&source) {
                key.clone()
            } else {
                let key = format!("s{}", source.0);
                let embedded = if embed { encode_source(session, source)? } else { Vec::new() };
                let path = if embed { String::new() } else { format!("{key}.wav") };
                sources.push((source, key.clone(), path, embedded));
                keys.insert(source, key.clone());
                key
            };
            let mut name = clip.name.clone();
            if name.is_empty() {
                name = "Clip".into();
            }
            soundcraft_model::truncate_chars(&mut name, soundcraft_model::MAX_NAME_CHARS);
            clips.push(ImportClip {
                name,
                source: key.clone(),
                start: rat(clip.start, rate),
                end: rat(clip.end(), rate),
                offset: rat(offset, rate),
                gain_db: clip.gain_db,
                fade_in: rat(clip.fade_in.len, rate),
                fade_out: rat(clip.fade_out.len, rate),
                fade_in_curve: curve(clip.fade_in.shape),
                fade_out_curve: curve(clip.fade_out.shape),
            });
        }
        if clips.is_empty() {
            continue;
        }
        let volume = lane_points(track, &AutoParam::Volume, rate);
        let pan = lane_points(track, &AutoParam::Pan(0), rate);
        let mut name = track.name.clone();
        soundcraft_model::truncate_chars(&mut name, soundcraft_model::MAX_NAME_CHARS);
        tracks.push(ImportTrack { name, channels: u16::try_from(track.channels()).unwrap_or(1), clips, volume, pan });
    }
    if tracks.is_empty() {
        return Err(bad("the chosen tracks have no audio clips"));
    }
    let mut media = Vec::new();
    for (id, key, path, embedded) in sources {
        let src = session.source(id).ok_or_else(|| bad("missing source"))?;
        if embed && embedded.is_empty() {
            return Err(bad(format!("{}: audio is not loaded", src.name)));
        }
        let mut name = src.name.clone();
        if name.is_empty() {
            name = "Audio".into();
        }
        soundcraft_model::truncate_chars(&mut name, soundcraft_model::MAX_NAME_CHARS);
        media.push(Media { key, name, path, channels: src.channels, frames: src.frames, sample_rate: rate, embedded_wav: embedded });
    }
    let (n, d) = session.frame_rate.rational();
    let markers = session
        .markers
        .iter()
        .filter(|m| m.kind == MarkerKind::Marker)
        .map(|m| {
            let mut name = m.name.clone();
            if name.is_empty() {
                name = "Marker".into();
            }
            soundcraft_model::truncate_chars(&mut name, soundcraft_model::MAX_NAME_CHARS);
            Marker { name, at: rat(m.start, rate), comment: m.comments.clone() }
        })
        .collect();
    let mut title = session.name.clone();
    if title.is_empty() {
        title = "Session".into();
    }
    soundcraft_model::truncate_chars(&mut title, soundcraft_model::MAX_NAME_CHARS);
    Ok(Plan {
        schema_version: 1,
        name: title,
        sample_rate: rate,
        timecode_start: rat(session.timecode_start, rate),
        frame_rate: Some(TimecodeRate { rate: Rational(n, d), drop: session.frame_rate.is_drop() }),
        sources: media,
        tracks,
        markers,
        warnings: Vec::new(),
    })
}

fn encode_source(session: &Session, id: SourceId) -> Result<Vec<u8>> {
    let audio = session.pool.get(id).ok_or_else(|| bad("audio is not loaded"))?;
    soundcraft_audio_io::encode(&audio.buffer, &EncodeOptions { format: FileFormat::Wav, bit_depth: BitDepth::Int24, dither: false, bwf: None })
        .map_err(|e| bad(e.to_string()))
}

fn lane_points(track: &soundcraft_model::Track, param: &AutoParam, rate: u32) -> Vec<Point> {
    track.lane(param).map(|lane| lane.points.iter().map(|p| Point { at: rat(p.at, rate), value: p.value }).collect()).unwrap_or_default()
}

/// Write sibling WAVs and point the plan at those file names. `dir` is the export's folder.
pub(crate) fn link_beside(plan: &mut Plan, dir: &Path, session: &Session) -> Result<()> {
    for media in &mut plan.sources {
        let id = media.key.strip_prefix('s').and_then(|n| n.parse().ok()).map(SourceId);
        let id = id.ok_or_else(|| bad("internal source key"))?;
        let bytes = encode_source(session, id)?;
        let name = format!("{}.wav", sanitize(&media.name));
        let path = dir.join(&name);
        crate::io::write_file(&path.to_string_lossy(), &bytes)?;
        media.path = name;
        media.embedded_wav.clear();
    }
    Ok(())
}

fn sanitize(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('_');
        }
    }
    if out.is_empty() { "audio".into() } else { out }
}

struct LocalMedia {
    media_dir: PathBuf,
}

impl MediaResolver for LocalMedia {
    fn resolve(&mut self, locator: &Path) -> std::result::Result<MediaInfo, MediaError> {
        let name = locator.file_name().ok_or_else(|| MediaError::Invalid(format!("locator has no file name: {}", locator.display())))?;
        let chosen = self.media_dir.join(name);
        if chosen.parent() != Some(self.media_dir.as_path()) {
            return Err(MediaError::Invalid(format!("relink escapes the media folder: {}", chosen.display())));
        }
        if !crate::io::file_found(&chosen) {
            return Err(MediaError::Missing(chosen.display().to_string()));
        }
        let file = crate::io::open_read(&chosen).map_err(|e| MediaError::Invalid(e.to_string()))?;
        let info = wav_info(BufReader::new(file)).map_err(|e| MediaError::Invalid(format!("{}: {e}", chosen.display())))?;
        let from = (chosen != locator).then(|| locator.display().to_string());
        Ok(MediaInfo {
            path: chosen.display().to_string(),
            relinked_from: from,
            channels: info.channels,
            frames: info.frames,
            sample_rate: info.sample_rate,
        })
    }
}

pub(crate) fn read_aaf_plan(path: &str, media_dir: &str, composition: Option<&str>) -> Result<Plan> {
    let bytes = crate::io::read_capped(Path::new(path), 512 * 1024 * 1024)?;
    let aaf = crate::io::canonical(Path::new(path));
    let media = crate::io::canonical(Path::new(media_dir));
    if !crate::io::dir_found(&media) {
        return Err(bad("choose the folder that holds the AAF's WAV files"));
    }
    let stem = aaf.file_stem().and_then(|s| s.to_str()).unwrap_or("Session");
    let base = aaf.parent().unwrap_or(Path::new("."));
    let options = AafOptions { composition, base_dir: base, default_name: stem };
    read_aaf(Cursor::new(bytes), &options, &mut LocalMedia { media_dir: media }).map_err(|e| bad(e.to_string()))
}

pub(crate) fn read_path(path: &str, media_dir: Option<&str>) -> Result<Plan> {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "ptx" | "ptf" | "pts" => Err(bad(
            "Pro Tools .ptx/.ptf is not read. libptformat is LGPL-2.1-or-later, which this project does not link or copy, and there is no complete public specification to implement clean-room. Export AAF from Pro Tools, or use DAWproject.",
        )),
        "omf" => Err(bad(
            "OMF import is not implemented. The usable Open Media Framework library is LGPL, which this project does not copy. Export AAF or DAWproject instead.",
        )),
        "dawproject" => {
            let bytes = crate::io::read_capped(Path::new(path), 64 * 1024 * 1024)?;
            let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("Session");
            let mut plan = read_dawproject(&bytes, stem).map_err(|e| bad(e.to_string()))?;
            let base = Path::new(path).parent().unwrap_or(Path::new("."));
            for media in &mut plan.sources {
                if media.embedded_wav.is_empty() && !media.path.is_empty() && !Path::new(&media.path).is_absolute() {
                    media.path = base.join(&media.path).display().to_string();
                }
            }
            Ok(plan)
        }
        _ => {
            let dir = media_dir.map(PathBuf::from).unwrap_or_else(|| Path::new(path).parent().unwrap_or(Path::new(".")).to_path_buf());
            read_aaf_plan(path, &dir.to_string_lossy(), None)
        }
    }
}

/// Insert `plan` into `destination`. `selected` indexes tracks in the plan.
pub(crate) fn import_into(destination: &Session, plan: Plan, selected: &[usize], mapping: &str, offset: i64) -> Result<(Session, Value)> {
    if !matches!(mapping, "absolute" | "relative") {
        return Err(bad("mapping must be absolute or relative"));
    }
    if offset.unsigned_abs() > MAX_POSITION as u64 {
        return Err(bad("invalid track offset in samples"));
    }
    let mut chosen = HashSet::new();
    for n in selected {
        if *n >= plan.tracks.len() || !chosen.insert(*n) {
            return Err(bad("invalid or duplicate source track index"));
        }
    }
    if chosen.is_empty() {
        return Err(bad("select at least one source track"));
    }
    let mut plan = plan;
    let mut i = 0;
    plan.tracks.retain(|_| {
        let keep = chosen.contains(&i);
        i += 1;
        keep
    });
    let used: HashSet<String> = plan.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.source.clone())).collect();
    plan.sources.retain(|s| used.contains(&s.key));
    let media_bytes: Vec<Vec<u8>> = plan.sources.iter().map(|s| s.embedded_wav.clone()).collect();
    let built = build_session(plan).map_err(|e| bad(e.to_string()))?;
    let mut incoming = built.session;
    let empty = destination.tracks.is_empty() && destination.sources.is_empty();
    if !empty && incoming.sample_rate != destination.sample_rate {
        return Err(bad("sample rates differ; sample-rate conversion is not implemented"));
    }
    let origin = if empty { incoming.timecode_start } else { destination.timecode_start };
    let shift = if mapping == "absolute" { incoming.timecode_start.checked_sub(origin) } else { Some(0) }
        .and_then(|n| n.checked_add(offset))
        .ok_or_else(|| bad("time mapping overflow"))?;
    for t in &mut incoming.tracks {
        for c in &mut t.playlist_mut().ok_or_else(|| bad("source playlist missing"))?.clips {
            c.start =
                c.start.checked_add(shift).filter(|n| *n >= 0 && *n <= MAX_POSITION).ok_or_else(|| bad("mapped clip is outside the session"))?;
            if c.start.checked_add(c.length).is_none_or(|n| n > MAX_POSITION) {
                return Err(bad("mapped end exceeds the time limit"));
            }
        }
        for lane in &mut t.automation {
            for point in &mut lane.points {
                point.at = point
                    .at
                    .checked_add(shift)
                    .filter(|n| *n >= 0 && *n <= MAX_POSITION)
                    .ok_or_else(|| bad("mapped automation is outside the session"))?;
            }
        }
    }
    for marker in &mut incoming.markers {
        marker.start =
            marker.start.checked_add(shift).filter(|n| *n >= 0 && *n <= MAX_POSITION).ok_or_else(|| bad("mapped marker is outside the session"))?;
        marker.end = marker.start;
    }
    let mut total = 0u64;
    for (src, bytes) in incoming.sources.iter().zip(media_bytes.iter()) {
        let payload = if !bytes.is_empty() {
            bytes.clone()
        } else {
            if !Path::new(&src.path).is_absolute() {
                return Err(bad("source paths must be absolute"));
            }
            crate::io::read_capped(Path::new(&src.path), 512 * 1024 * 1024)?
        };
        total = total
            .checked_add(src.frames.saturating_mul(u64::from(src.channels)))
            .filter(|n| *n <= 1 << 27)
            .ok_or_else(|| bad("selected media exceeds the import memory limit"))?;
        let info = soundcraft_audio_io::probe(&payload, Some("wav")).map_err(|e| bad(e.to_string()))?;
        if (info.sample_rate, info.channels, info.frames) != (src.sample_rate, src.channels, src.frames) {
            return Err(bad(format!("media does not match the interchange file: {}", src.name)));
        }
        let (_, buffer) = soundcraft_audio_io::decode(&payload, Some("wav")).map_err(|e| bad(e.to_string()))?;
        incoming.pool.insert(src.id, Arc::new(SourceAudio::new(buffer)));
    }
    let mut target = destination.clone();
    if empty {
        target.sample_rate = incoming.sample_rate;
        target.frame_rate = incoming.frame_rate;
        target.timecode_start = incoming.timecode_start;
        if target.name == "Untitled" {
            target.name = incoming.name.clone();
        }
    }
    let mut source_ids = HashMap::new();
    for mut src in incoming.sources {
        let old = src.id;
        src.id = SourceId(target.alloc());
        let audio = incoming.pool.get(old).ok_or_else(|| bad("decoded media missing"))?.clone();
        target.pool.insert(src.id, audio);
        source_ids.insert(old, src.id);
        src.unsaved = true;
        target.sources.push(src);
    }
    let mut track_ids = Vec::new();
    for mut track in incoming.tracks {
        track.id = TrackId(target.alloc());
        track.name = target.unique_track_name(&track.name);
        let name = track.name.clone();
        for playlist in &mut track.playlists {
            playlist.name = name.clone();
            for clip in &mut playlist.clips {
                clip.id = soundcraft_model::ClipId(target.alloc());
                if let ClipContent::Audio { source, .. } = &mut clip.content {
                    *source = *source_ids.get(source).ok_or_else(|| bad("source mapping missing"))?;
                }
            }
        }
        track_ids.push(track.id);
        target.tracks.push(track);
    }
    for marker in incoming.markers {
        let id = target.add_marker(&marker.name, MarkerKind::Marker, marker.start, marker.end);
        if let Some(dst) = target.markers.iter_mut().find(|m| m.id == id) {
            dst.comments = marker.comments;
        }
    }
    let report = json!({
        "tracks": track_ids,
        "sources": source_ids.len(),
        "warnings": built.report.warnings,
        "handles": built.report.handles,
        "mapping": mapping,
        "shift_samples": shift,
    });
    Ok((target, report))
}

pub(crate) fn export_to(session: &Session, path: &str, tracks: &[TrackId], embed: bool) -> Result<Value> {
    let mut plan = plan_from_session(session, tracks, embed || !path_is_sibling_ok(path))?;
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("aaf").to_ascii_lowercase();
    let bytes = if !embed && ext != "dawproject" {
        let dir = Path::new(path).parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        link_beside(&mut plan, dir, session)?;
        write_aaf(&plan).map_err(|e| bad(e.to_string()))?
    } else if ext == "dawproject" {
        if !embed {
            let dir = Path::new(path).parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
            link_beside(&mut plan, dir, session)?;
        }
        write_dawproject(&plan).map_err(|e| bad(e.to_string()))?
    } else {
        write_aaf(&plan).map_err(|e| bad(e.to_string()))?
    };
    crate::io::write_file(path, &bytes)?;
    Ok(json!({"path": path, "tracks": plan.tracks.len(), "sources": plan.sources.len(), "markers": plan.markers.len(), "embedded": embed}))
}

fn path_is_sibling_ok(path: &str) -> bool {
    Path::new(path).parent().is_some()
}
