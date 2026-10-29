//! Reusable interchange plan → SoundCraft session, and the reverse export.
//!
//! Readers supply rational seconds. Endpoints are rounded independently (nearest, ties away
//! from zero), so adjacent edits share exactly the same boundary without cumulative drift.
//! No processes, UI, or filesystem access.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod aaf;
mod aaf_write;
pub mod dawproject;

use serde::{Deserialize, Serialize};
use soundcraft_model::{
    AutoParam, AutomationLane, ChannelFormat, Clip, ClipId, Fade, FadeShape, MarkerKind, Session, Source, SourceId, Track, TrackId, TrackKind,
};
use soundcraft_time::{FrameRate, MAX_POSITION, SampleRate};
use std::collections::HashMap;

pub use aaf_write::write_aaf;
pub use dawproject::{read_dawproject, write_dawproject};

#[derive(Debug, thiserror::Error)]
#[error("interchange: {0}")]
pub struct ImportError(pub String);
type Result<T> = std::result::Result<T, ImportError>;
fn invalid(s: impl Into<String>) -> ImportError {
    ImportError(s.into())
}

/// Rational seconds [numerator, denominator]. Both must be nonnegative, denominator > 0.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rational(pub i64, pub i64);

impl Default for Rational {
    fn default() -> Self {
        Rational(0, 1)
    }
}

impl Rational {
    pub fn samples(self, hz: u32) -> Result<i64> {
        if self.0 < 0 || self.1 <= 0 {
            return Err(invalid("negative time or invalid denominator"));
        }
        let n = i128::from(self.0) * i128::from(hz);
        let d = i128::from(self.1);
        let v = (n + d / 2) / d;
        if v > i128::from(MAX_POSITION) {
            return Err(invalid("time exceeds SoundCraft limit"));
        }
        i64::try_from(v).map_err(|_| invalid("time overflow"))
    }
}

/// Fade curve carried through interchange. AAF's fade type only defines linear amplitude and
/// linear power; the other three are extension values this reader writes and reads back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    Linear,
    #[default]
    EqualPower,
    SCurve,
    Exponential,
    Logarithmic,
}

impl Curve {
    fn shape(self) -> FadeShape {
        match self {
            Curve::Linear => FadeShape::Linear,
            Curve::EqualPower => FadeShape::EqualPower,
            Curve::SCurve => FadeShape::SCurve,
            Curve::Exponential => FadeShape::Exponential,
            Curve::Logarithmic => FadeShape::Logarithmic,
        }
    }

    pub fn from_shape(shape: FadeShape) -> Self {
        match shape {
            FadeShape::Linear => Curve::Linear,
            FadeShape::EqualPower => Curve::EqualPower,
            FadeShape::SCurve => Curve::SCurve,
            FadeShape::Exponential => Curve::Exponential,
            FadeShape::Logarithmic => Curve::Logarithmic,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema_version: u32,
    pub name: String,
    pub sample_rate: u32,
    pub timecode_start: Rational,
    pub frame_rate: Option<TimecodeRate>,
    pub sources: Vec<Media>,
    pub tracks: Vec<ImportTrack>,
    #[serde(default)]
    pub markers: Vec<Marker>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimecodeRate {
    pub rate: Rational,
    pub drop: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Media {
    pub key: String,
    pub name: String,
    pub path: String,
    pub channels: u16,
    pub frames: u64,
    pub sample_rate: u32,
    /// PCM WAV bytes when the essence is inside the interchange file. Empty means `path` is a link.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedded_wav: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportTrack {
    pub name: String,
    pub channels: u16,
    pub clips: Vec<ImportClip>,
    /// Fader automation in decibels. Empty leaves the fader at unity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume: Vec<Point>,
    /// First panner, −1 (left) to 1 (right). Empty leaves the panner centered (or hard-panned stereo).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pan: Vec<Point>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub at: Rational,
    pub value: f32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportClip {
    pub name: String,
    pub source: String,
    pub start: Rational,
    pub end: Rational,
    pub offset: Rational,
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default)]
    pub fade_in: Rational,
    #[serde(default)]
    pub fade_out: Rational,
    #[serde(default)]
    pub fade_in_curve: Curve,
    #[serde(default)]
    pub fade_out_curve: Curve,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    pub name: String,
    pub at: Rational,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Serialize)]
pub struct HandleReport {
    pub clip: String,
    pub before: i64,
    pub after: i64,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub warnings: Vec<String>,
    pub handles: Vec<HandleReport>,
}

pub struct Imported {
    pub session: Session,
    pub report: Report,
}

fn layout(channels: u16) -> Result<ChannelFormat> {
    match channels {
        1 => Ok(ChannelFormat::Mono),
        2 => Ok(ChannelFormat::Stereo),
        _ => Err(invalid("only mono/stereo supported")),
    }
}

fn name(s: &str) -> Result<()> {
    if s.chars().count() > soundcraft_model::MAX_NAME_CHARS || s.is_empty() {
        return Err(invalid("name empty or too long for SoundCraft"));
    }
    Ok(())
}

fn frame_rate(tc: &TimecodeRate) -> Result<FrameRate> {
    let Rational(n, d) = tc.rate;
    if n <= 0 || d <= 0 {
        return Err(invalid("invalid frame rate"));
    }
    FrameRate::ALL
        .into_iter()
        .find(|r| {
            let (rn, rd) = r.rational();
            i128::from(n) * i128::from(rd) == i128::from(rn) * i128::from(d) && r.is_drop() == tc.drop
        })
        .ok_or_else(|| invalid("unsupported frame rate/drop combination"))
}

fn finite_db(v: f32, what: &str) -> Result<f32> {
    if !v.is_finite() {
        return Err(invalid(format!("{what} is not a finite number")));
    }
    Ok(v.clamp(-144.0, 24.0))
}

fn lane(param: AutoParam, points: &[Point], rate: u32) -> Result<Option<AutomationLane>> {
    if points.is_empty() {
        return Ok(None);
    }
    if points.len() > 100_000 {
        return Err(invalid("too many automation points"));
    }
    let mut lane = AutomationLane::new(param);
    for p in points {
        let at = p.at.samples(rate)?;
        if !p.value.is_finite() {
            return Err(invalid("automation value is not finite"));
        }
        lane.set_point(at, p.value);
    }
    Ok(Some(lane))
}

/// Build transactionally. On any error no partial session is returned. The caller must verify
/// media bytes against the metadata before using the plan.
pub fn build_session(plan: Plan) -> Result<Imported> {
    if plan.schema_version != 1 {
        return Err(invalid("unsupported interchange schema"));
    }
    if plan.sources.len() > 100_000
        || plan.tracks.len() > 100_000
        || plan.markers.len() > 100_000
        || plan.tracks.iter().map(|t| t.clips.len()).sum::<usize>() > 100_000
    {
        return Err(invalid("too many sources/tracks/clips"));
    }
    name(&plan.name)?;
    let sr = SampleRate::new(plan.sample_rate).map_err(|e| ImportError(e.to_string()))?;
    let mut s = Session::new(&plan.name, sr);
    s.timecode_start = plan.timecode_start.samples(sr.hz())?;
    if let Some(tc) = &plan.frame_rate {
        s.frame_rate = frame_rate(tc)?;
    }
    let mut ids = HashMap::new();
    for m in &plan.sources {
        name(&m.name)?;
        layout(m.channels)?;
        if m.sample_rate != sr.hz() || m.frames == 0 || m.frames > MAX_POSITION as u64 {
            return Err(invalid("invalid media metadata or mixed sample rates (resampling unsupported)"));
        }
        if m.path.is_empty() && m.embedded_wav.is_empty() {
            return Err(invalid("media has neither a path nor embedded audio"));
        }
        if m.embedded_wav.len() > 32 * 1024 * 1024 {
            return Err(invalid("embedded media exceeds 32 MiB"));
        }
        let id = SourceId(s.alloc());
        if ids.insert(m.key.as_str(), (id, m.frames, m.channels)).is_some() {
            return Err(invalid("duplicate source key"));
        }
        s.sources.push(Source {
            id,
            name: m.name.clone(),
            path: if m.path.is_empty() { format!("{}.wav", m.name) } else { m.path.clone() },
            channels: m.channels,
            frames: m.frames,
            sample_rate: m.sample_rate,
            format: soundcraft_audio_io::FileFormat::Wav,
            time_reference: 0,
            unsaved: !m.embedded_wav.is_empty(),
        });
    }
    let mut report = Report { warnings: plan.warnings.clone(), handles: Vec::new() };
    for t in &plan.tracks {
        name(&t.name)?;
        let track_id = TrackId(s.alloc());
        // Construct directly: add_track intentionally renames duplicates, interchange must not.
        let mut track = Track::new(track_id, &t.name, TrackKind::Audio, layout(t.channels)?, s.next_track_color());
        let mut previous_end = 0;
        for c in &t.clips {
            name(&c.name)?;
            let (source, frames, channels) = ids.get(c.source.as_str()).ok_or_else(|| invalid("unknown source key"))?;
            if *channels != t.channels {
                return Err(invalid("clip/track layout mismatch"));
            }
            let start = c.start.samples(sr.hz())?;
            let end = c.end.samples(sr.hz())?;
            let offset = c.offset.samples(sr.hz())?;
            if end <= start || start < previous_end {
                return Err(invalid("zero/negative duration or overlapping/out-of-order clips"));
            }
            let length = end - start;
            let source_end = offset.checked_add(length).ok_or_else(|| invalid("source window overflow"))?;
            if source_end > *frames as i64 {
                return Err(invalid("clip extends beyond source; refusing truncated handles"));
            }
            let fade_in = c.fade_in.samples(sr.hz())?;
            let fade_out = c.fade_out.samples(sr.hz())?;
            if fade_in.saturating_add(fade_out) > length {
                return Err(invalid("fades are longer than the clip"));
            }
            let mut clip = Clip::audio(ClipId(s.alloc()), &c.name, *source, offset, start, length);
            clip.gain_db = finite_db(c.gain_db, "clip gain")?;
            clip.fade_in = Fade { len: fade_in, shape: c.fade_in_curve.shape(), curvature: 0.0 };
            clip.fade_out = Fade { len: fade_out, shape: c.fade_out_curve.shape(), curvature: 0.0 };
            track.playlist_mut().ok_or_else(|| invalid("missing playlist"))?.clips.push(clip);
            report.handles.push(HandleReport { clip: c.name.clone(), before: offset, after: *frames as i64 - source_end });
            previous_end = end;
        }
        if let Some(volume) = lane(AutoParam::Volume, &t.volume, sr.hz())? {
            track.automation.push(volume);
        }
        if let Some(pan) = lane(AutoParam::Pan(0), &t.pan, sr.hz())? {
            track.automation.push(pan);
        }
        s.tracks.push(track);
    }
    for m in &plan.markers {
        name(&m.name)?;
        let at = m.at.samples(sr.hz())?;
        let id = s.add_marker(&m.name, MarkerKind::Marker, at, at);
        if let Some(marker) = s.markers.iter_mut().find(|marker| marker.id == id) {
            let mut comment = m.comment.clone();
            soundcraft_model::truncate_chars(&mut comment, soundcraft_model::MAX_COMMENT_CHARS);
            marker.comments = comment;
        }
    }
    s.comments = "Imported by soundcraft-interchange schema v1.".into();
    Ok(Imported { session: s, report })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        Plan {
            schema_version: 1,
            name: "Test".into(),
            sample_rate: 48000,
            timecode_start: Rational(3600, 1),
            frame_rate: Some(TimecodeRate { rate: Rational(25, 1), drop: false }),
            warnings: vec![],
            sources: vec![Media {
                key: "s".into(),
                name: "src".into(),
                path: "a.wav".into(),
                channels: 1,
                frames: 480000,
                sample_rate: 48000,
                embedded_wav: Vec::new(),
            }],
            tracks: vec![ImportTrack {
                name: "DX".into(),
                channels: 1,
                clips: vec![ImportClip {
                    name: "cut".into(),
                    source: "s".into(),
                    start: Rational(1, 1),
                    end: Rational(3, 1),
                    offset: Rational(2, 1),
                    gain_db: -3.0,
                    fade_in: Rational(1, 100),
                    fade_out: Rational(1, 50),
                    fade_in_curve: Curve::Linear,
                    fade_out_curve: Curve::EqualPower,
                }],
                volume: vec![Point { at: Rational(0, 1), value: -6.0 }, Point { at: Rational(2, 1), value: 0.0 }],
                pan: vec![Point { at: Rational(1, 1), value: -0.5 }],
            }],
            markers: vec![Marker { name: "Verse".into(), at: Rational(4, 1), comment: "start".into() }],
        }
    }

    #[test]
    fn positions_offsets_handles_and_roundtrip() {
        let result = build_session(plan()).unwrap();
        let c = &result.session.tracks[0].clips()[0];
        assert_eq!((c.start, c.length, c.source_offset()), (48000, 96000, 96000));
        assert_eq!((result.report.handles[0].before, result.report.handles[0].after), (96000, 288000));
        assert_eq!(result.session.timecode_start, 172800000);
        assert_eq!(c.gain_db, -3.0);
        assert_eq!(c.fade_in.len, 480);
        assert_eq!(c.fade_out.len, 960);
        assert_eq!(c.fade_in.shape, FadeShape::Linear);
        assert_eq!(result.session.tracks[0].lane(&AutoParam::Volume).unwrap().points.len(), 2);
        assert_eq!(result.session.tracks[0].lane(&AutoParam::Pan(0)).unwrap().value_at(48000, 0.0), -0.5);
        assert_eq!(result.session.markers[0].name, "Verse");
        assert_eq!(result.session.markers[0].comments, "start");
        let back = Session::from_json(&result.session.to_json().unwrap()).unwrap();
        assert_eq!(back.tracks[0].clips()[0].gain_db, -3.0);
    }

    #[test]
    fn adjacent_fractional_frame_boundaries_never_drift() {
        for n in 1..10000 {
            let a = Rational(n * 1001, 24000).samples(48000).unwrap();
            let b = Rational((n + 1) * 1001, 24000).samples(48000).unwrap();
            assert_eq!(b - a, 2002);
        }
        assert_eq!(Rational(1, 96000).samples(48000).unwrap(), 1);
    }

    #[test]
    fn fractional_sample_boundaries_use_absolute_endpoints() {
        let mut p = plan();
        p.sample_rate = 44100;
        p.sources[0].sample_rate = 44100;
        p.sources[0].frames = 4410000;
        p.tracks[0].clips = (0..1000)
            .map(|n| ImportClip {
                name: "frame".into(),
                source: "s".into(),
                start: Rational(n * 1001, 24000),
                end: Rational((n + 1) * 1001, 24000),
                offset: Rational(n * 1001, 24000),
                gain_db: 0.0,
                fade_in: Rational::default(),
                fade_out: Rational::default(),
                fade_in_curve: Curve::EqualPower,
                fade_out_curve: Curve::EqualPower,
            })
            .collect();
        let s = build_session(p).unwrap().session;
        let clips = s.tracks[0].clips();
        for pair in clips.windows(2) {
            assert_eq!(pair[0].end(), pair[1].start);
        }
        assert_eq!(clips[999].end(), 1839338);
        assert_eq!(clips.iter().map(|c| c.length).sum::<i64>(), 1839338);
    }

    #[test]
    fn invalid_windows_and_metadata_are_rejected() {
        let mut p = plan();
        p.tracks[0].clips[0].offset = Rational(9, 1);
        assert!(build_session(p).is_err());
        let mut p = plan();
        p.sources[0].sample_rate = 44100;
        assert!(build_session(p).is_err());
        let mut p = plan();
        p.tracks[0].clips[0].fade_in = Rational(10, 1);
        assert!(build_session(p).is_err());
        assert!(Rational(1, 0).samples(48000).is_err());
        assert!(Rational(i64::MAX, 1).samples(48000).is_err());
        assert!(Rational(-1, 1).samples(48000).is_err());
    }

    #[test]
    fn stereo_names_and_order_are_preserved() {
        let mut p = plan();
        p.sources[0].channels = 2;
        p.tracks[0].channels = 2;
        p.tracks.push(ImportTrack { name: "DX".into(), channels: 1, clips: vec![], volume: vec![], pan: vec![] });
        let s = build_session(p).unwrap().session;
        assert_eq!(s.tracks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["DX", "DX"]);
        assert_eq!(s.tracks[0].channels(), 2);
    }
}
