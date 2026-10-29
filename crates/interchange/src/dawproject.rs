//! DAWproject (Bitwig, MIT) import and export for the interchange plan.
//!
//! A `.dawproject` file is a zip: `project.xml` plus `audio/` files. Times are seconds.
//! Volume is decibels. Pan is normalized (0 left, 1 right). Clip gain and fade curves ride
//! in attributes this module writes and reads; a DAW that ignores unknown attributes still
//! gets the clips, fades times, automation and media.

use crate::{Curve, ImportClip, ImportError, ImportTrack, Marker, Media, Plan, Point, Rational, TimecodeRate};
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

type Result<T> = std::result::Result<T, ImportError>;

fn fail(s: impl Into<String>) -> ImportError {
    ImportError(s.into())
}

const MAX_XML: u64 = 32 * 1024 * 1024;
const MAX_ENTRY: u64 = 32 * 1024 * 1024;
const MAX_TOTAL: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;

fn samples(t: Rational, rate: u32) -> Result<i64> {
    t.samples(rate).map_err(|e| fail(e.0))
}

fn secs(samples: i64, rate: u32) -> String {
    let value = samples as f64 / f64::from(rate.max(1));
    format!("{value:.9}")
}

fn samples_of(text: &str, rate: u32) -> Result<i64> {
    let value: f64 = text.parse().map_err(|_| fail(format!("invalid time `{text}`")))?;
    if !value.is_finite() || value < 0.0 {
        return Err(fail("invalid time"));
    }
    let n = (value * f64::from(rate)).round();
    if !n.is_finite() || n > (1i64 << 40) as f64 {
        return Err(fail("time exceeds SoundCraft limit"));
    }
    Ok(n as i64)
}

fn rat(samples: i64, rate: u32) -> Rational {
    Rational(samples, i64::from(rate.max(1)))
}

fn xml_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn curve_name(curve: Curve) -> &'static str {
    match curve {
        Curve::Linear => "linear",
        Curve::EqualPower => "equalPower",
        Curve::SCurve => "sCurve",
        Curve::Exponential => "exponential",
        Curve::Logarithmic => "logarithmic",
    }
}

fn curve_of(name: &str) -> Curve {
    match name {
        "linear" => Curve::Linear,
        "sCurve" | "s_curve" => Curve::SCurve,
        "exponential" => Curve::Exponential,
        "logarithmic" => Curve::Logarithmic,
        _ => Curve::EqualPower,
    }
}

/// Zip a plan into a DAWproject. Embedded WAV bytes are stored under `audio/`; a path with no
/// bytes is an external file reference.
pub fn write_dawproject(plan: &Plan) -> Result<Vec<u8>> {
    if plan.schema_version != 1 {
        return Err(fail("unsupported interchange schema"));
    }
    if plan.sources.is_empty() {
        return Err(fail("DAWproject export needs at least one source"));
    }
    let rate = plan.sample_rate;
    let mut files = HashMap::new();
    for (i, media) in plan.sources.iter().enumerate() {
        if !media.embedded_wav.is_empty() {
            files.insert(media.key.clone(), (format!("audio/m{i}.wav"), false, media.embedded_wav.clone()));
        } else if media.path.is_empty() {
            return Err(fail("source has neither a path nor embedded audio"));
        } else {
            files.insert(media.key.clone(), (media.path.clone(), true, Vec::new()));
        }
    }
    let xml = project_xml(plan, rate, &files)?;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("project.xml", opts).map_err(|e| fail(e.to_string()))?;
    zip.write_all(xml.as_bytes()).map_err(|e| fail(e.to_string()))?;
    for (path, external, bytes) in files.values() {
        if *external {
            continue;
        }
        zip.start_file(path, opts).map_err(|e| fail(e.to_string()))?;
        zip.write_all(bytes).map_err(|e| fail(e.to_string()))?;
    }
    let cursor = zip.finish().map_err(|e| fail(e.to_string()))?;
    Ok(cursor.into_inner())
}

fn project_xml(plan: &Plan, rate: u32, files: &HashMap<String, (String, bool, Vec<u8>)>) -> Result<String> {
    let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    xml.push_str(r#"<Project version="1.0"><Application name="SoundCraft" version="1"/><Transport>"#);
    xml.push_str(r#"<Tempo unit="bpm" value="120" min="20" max="666"/><TimeSignature numerator="4" denominator="4"/></Transport><Structure>"#);
    for (i, track) in plan.tracks.iter().enumerate() {
        let vol = track.volume.first().map(|p| p.value).unwrap_or(0.0);
        let pan = track.pan.first().map(|p| (p.value + 1.0) * 0.5).unwrap_or(0.5);
        xml.push_str(&format!(
            r#"<Track contentType="audio" id="t{i}" name="{}"><Channel id="c{i}" audioChannels="{}" role="regular"><Volume id="vol{i}" unit="decibel" value="{vol}" min="-144" max="24"/><Pan id="pan{i}" unit="normalized" value="{pan}" min="0" max="1"/></Channel></Track>"#,
            xml_escape(&track.name),
            track.channels
        ));
    }
    xml.push_str(r#"</Structure><Arrangement id="arr"><Lanes timeUnit="seconds">"#);
    for (i, track) in plan.tracks.iter().enumerate() {
        xml.push_str(&format!(r#"<Lanes track="t{i}" timeUnit="seconds"><Clips>"#));
        for clip in &track.clips {
            let file = files.get(&clip.source).ok_or_else(|| fail("clip names an unknown source"))?;
            let media = plan.sources.iter().find(|s| s.key == clip.source).ok_or_else(|| fail("clip names an unknown source"))?;
            let start = samples(clip.start, rate)?;
            let end = samples(clip.end, rate)?;
            let offset = samples(clip.offset, rate)?;
            let fade_in = samples(clip.fade_in, rate)?;
            let fade_out = samples(clip.fade_out, rate)?;
            xml.push_str(&format!(
                r#"<Clip time="{}" duration="{}" name="{}" playStart="{}" fadeTimeUnit="seconds" fadeInTime="{}" fadeOutTime="{}" fadeInCurve="{}" fadeOutCurve="{}" gain="{}"><Audio channels="{}" sampleRate="{rate}" duration="{}"><File path="{}" external="{}"/></Audio></Clip>"#,
                secs(start, rate),
                secs(end - start, rate),
                xml_escape(&clip.name),
                secs(offset, rate),
                secs(fade_in, rate),
                secs(fade_out, rate),
                curve_name(clip.fade_in_curve),
                curve_name(clip.fade_out_curve),
                clip.gain_db,
                media.channels,
                secs(i64::try_from(media.frames).unwrap_or(0), rate),
                xml_escape(&file.0),
                file.1
            ));
        }
        xml.push_str("</Clips>");
        if !track.volume.is_empty() {
            xml.push_str(&format!(r#"<Points unit="decibel" timeUnit="seconds"><Target parameter="vol{i}"/>"#));
            for point in &track.volume {
                xml.push_str(&format!(
                    r#"<RealPoint time="{}" value="{}" interpolation="linear"/>"#,
                    secs(samples(point.at, rate)?, rate),
                    point.value
                ));
            }
            xml.push_str("</Points>");
        }
        if !track.pan.is_empty() {
            xml.push_str(&format!(r#"<Points unit="normalized" timeUnit="seconds"><Target parameter="pan{i}"/>"#));
            for point in &track.pan {
                xml.push_str(&format!(
                    r#"<RealPoint time="{}" value="{}" interpolation="linear"/>"#,
                    secs(samples(point.at, rate)?, rate),
                    (point.value + 1.0) * 0.5
                ));
            }
            xml.push_str("</Points>");
        }
        xml.push_str("</Lanes>");
    }
    xml.push_str("</Lanes>");
    if !plan.markers.is_empty() {
        xml.push_str("<Markers>");
        for marker in &plan.markers {
            xml.push_str(&format!(
                r#"<Marker time="{}" name="{}" comment="{}"/>"#,
                secs(samples(marker.at, rate)?, rate),
                xml_escape(&marker.name),
                xml_escape(&marker.comment)
            ));
        }
        xml.push_str("</Markers>");
    }
    let _ = TimecodeRate { rate: Rational(25, 1), drop: false };
    xml.push_str("</Arrangement></Project>");
    Ok(xml)
}

/// Read a DAWproject archive into a plan. `name` is used when the project has none.
pub fn read_dawproject(bytes: &[u8], name: &str) -> Result<Plan> {
    if bytes.len() as u64 > MAX_TOTAL {
        return Err(fail("DAWproject exceeds 64 MiB"));
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| fail(format!("not a DAWproject zip: {e}")))?;
    if zip.len() > MAX_ENTRIES {
        return Err(fail("DAWproject has too many entries"));
    }
    let mut xml = None;
    let mut audio = HashMap::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| fail(e.to_string()))?;
        if file.is_dir() {
            continue;
        }
        let path = file.name().map_err(|e| fail(e.to_string()))?.replace('\\', "/");
        if path.split('/').any(|part| part == ".." || part.is_empty() && path.contains("//")) {
            return Err(fail("DAWproject entry path is not allowed"));
        }
        if file.size() > MAX_ENTRY {
            return Err(fail(format!("{path} exceeds 32 MiB")));
        }
        total = total.saturating_add(file.size());
        if total > MAX_TOTAL {
            return Err(fail("DAWproject exceeds 64 MiB"));
        }
        let mut buf = Vec::new();
        file.by_ref().take(MAX_ENTRY + 1).read_to_end(&mut buf).map_err(|e| fail(e.to_string()))?;
        if path == "project.xml" {
            if buf.len() as u64 > MAX_XML {
                return Err(fail("project.xml exceeds 32 MiB"));
            }
            xml = Some(String::from_utf8(buf).map_err(|_| fail("project.xml is not UTF-8"))?);
        } else if path.starts_with("audio/") {
            audio.insert(path, buf);
        }
    }
    let xml = xml.ok_or_else(|| fail("DAWproject has no project.xml"))?;
    let mut plan = parse_project(&xml, name, &audio)?;
    normalize_times(&mut plan)?;
    Ok(plan)
}

struct ClipBuild {
    track: usize,
    name: String,
    time: i64,
    duration: i64,
    offset: i64,
    gain_db: f32,
    fade_in: i64,
    fade_out: i64,
    fade_in_curve: Curve,
    fade_out_curve: Curve,
    channels: u16,
    sample_rate: u32,
    frames: u64,
    path: String,
    external: bool,
}

fn parse_project(xml: &str, name: &str, audio: &HashMap<String, Vec<u8>>) -> Result<Plan> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut tracks: Vec<ImportTrack> = Vec::new();
    let mut track_ids: HashMap<String, usize> = HashMap::new();
    let mut param_kind: HashMap<String, (usize, bool)> = HashMap::new();
    let mut clip: Option<ClipBuild> = None;
    let mut point_target: Option<(usize, bool)> = None;
    let mut current: Option<usize> = None;
    let mut markers = Vec::new();
    let mut sources: Vec<Media> = Vec::new();
    let mut source_by_path: HashMap<String, String> = HashMap::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                apply_start(
                    &tag,
                    &attrs(&e)?,
                    &mut tracks,
                    &mut track_ids,
                    &mut param_kind,
                    &mut clip,
                    &mut point_target,
                    &mut markers,
                    &mut current,
                )?;
                stack.push(tag);
            }
            Ok(quick_xml::events::Event::Empty(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                apply_start(
                    &tag,
                    &attrs(&e)?,
                    &mut tracks,
                    &mut track_ids,
                    &mut param_kind,
                    &mut clip,
                    &mut point_target,
                    &mut markers,
                    &mut current,
                )?;
                if tag == "File"
                    && let Some(c) = clip.as_mut()
                {
                    let map = attrs(&e)?;
                    c.path = attr_get(&map, "path");
                    c.external = attr_get(&map, "external") == "true";
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if tag == "Clip"
                    && let Some(c) = clip.take()
                {
                    finish_clip(c, &mut tracks, &mut sources, &mut source_by_path, audio)?;
                }
                if tag == "Points" {
                    point_target = None;
                }
                stack.pop();
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(e) => return Err(fail(format!("project.xml: {e}"))),
            _ => {}
        }
        buf.clear();
        let _ = &stack;
    }
    if sources.is_empty() {
        return Err(fail("DAWproject has no audio"));
    }
    let sample_rate = sources[0].sample_rate;
    if sources.iter().any(|s| s.sample_rate != sample_rate) {
        return Err(fail("mixed sample rates are not imported"));
    }
    let mut label = name.trim().to_string();
    if label.is_empty() {
        label = "DAWproject".into();
    }
    soundcraft_model::truncate_chars(&mut label, soundcraft_model::MAX_NAME_CHARS);
    Ok(Plan {
        schema_version: 1,
        name: label,
        sample_rate,
        timecode_start: Rational(0, 1),
        frame_rate: None,
        sources,
        tracks,
        markers,
        warnings: vec!["DAWproject: plugins, routing, MIDI and video are not transferred.".into()],
    })
}

fn attrs(e: &quick_xml::events::BytesStart<'_>) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|err| fail(err.to_string()))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let value = attr.unescape_value().map_err(|err| fail(err.to_string()))?.into_owned();
        map.insert(key, value);
    }
    Ok(map)
}

fn attr_get(map: &HashMap<String, String>, key: &str) -> String {
    map.get(key).cloned().unwrap_or_default()
}

fn num(map: &HashMap<String, String>, key: &str) -> Result<f64> {
    let text = attr_get(map, key);
    if text.is_empty() {
        return Ok(0.0);
    }
    text.parse::<f64>()
        .map_err(|_| fail(format!("invalid number `{text}`")))
        .and_then(|v| if v.is_finite() { Ok(v) } else { Err(fail("non-finite number")) })
}

#[allow(clippy::too_many_arguments)]
fn apply_start(
    tag: &str,
    map: &HashMap<String, String>,
    tracks: &mut Vec<ImportTrack>,
    track_ids: &mut HashMap<String, usize>,
    param_kind: &mut HashMap<String, (usize, bool)>,
    clip: &mut Option<ClipBuild>,
    point_target: &mut Option<(usize, bool)>,
    markers: &mut Vec<Marker>,
    current: &mut Option<usize>,
) -> Result<()> {
    match tag {
        "Track" => {
            let id = attr_get(map, "id");
            let mut name = attr_get(map, "name");
            if name.is_empty() {
                name = format!("Audio {}", tracks.len() + 1);
            }
            soundcraft_model::truncate_chars(&mut name, soundcraft_model::MAX_NAME_CHARS);
            let index = tracks.len();
            let channels = attr_get(map, "audioChannels").parse::<u16>().unwrap_or(0);
            tracks.push(ImportTrack { name, channels, clips: Vec::new(), volume: Vec::new(), pan: Vec::new() });
            if !id.is_empty() {
                track_ids.insert(id, index);
            }
        }
        "Channel" => {
            if let Some(track) = tracks.last_mut() {
                let channels = attr_get(map, "audioChannels").parse::<u16>().unwrap_or(track.channels);
                if channels > 0 {
                    track.channels = channels;
                }
            }
        }
        "Volume" | "Pan" => {
            let id = attr_get(map, "id");
            let index = tracks.len().saturating_sub(1);
            if !id.is_empty() {
                param_kind.insert(id, (index, tag == "Volume"));
            }
        }
        "Lanes" => {
            let id = attr_get(map, "track");
            if let Some(index) = track_ids.get(&id).copied() {
                *current = Some(index);
            }
        }
        "Clip" => {
            let track = current.ok_or_else(|| fail("clip is not on a track"))?;
            let rate = 48_000u32;
            *clip = Some(ClipBuild {
                track,
                name: {
                    let mut n = attr_get(map, "name");
                    if n.is_empty() {
                        n = "Clip".into();
                    }
                    soundcraft_model::truncate_chars(&mut n, soundcraft_model::MAX_NAME_CHARS);
                    n
                },
                time: (num(map, "time")? * 1_000_000_000.0).round() as i64,
                duration: (num(map, "duration")? * 1_000_000_000.0).round() as i64,
                offset: (num(map, "playStart")? * 1_000_000_000.0).round() as i64,
                gain_db: num(map, "gain")? as f32,
                fade_in: (num(map, "fadeInTime")? * 1_000_000_000.0).round() as i64,
                fade_out: (num(map, "fadeOutTime")? * 1_000_000_000.0).round() as i64,
                fade_in_curve: curve_of(&attr_get(map, "fadeInCurve")),
                fade_out_curve: curve_of(&attr_get(map, "fadeOutCurve")),
                channels: 1,
                sample_rate: rate,
                frames: 0,
                path: String::new(),
                external: true,
            });
            let _ = rate;
        }
        "Audio" => {
            if let Some(c) = clip.as_mut() {
                c.channels = attr_get(map, "channels").parse().unwrap_or(1);
                c.sample_rate = attr_get(map, "sampleRate").parse().unwrap_or(48_000);
                let duration = num(map, "duration")?;
                let frames = (duration * f64::from(c.sample_rate)).round();
                c.frames = if frames.is_finite() && frames > 0.0 { frames as u64 } else { 0 };
            }
        }
        "Target" => {
            let id = attr_get(map, "parameter");
            *point_target = param_kind.get(&id).copied();
        }
        "RealPoint" => {
            if let Some((index, volume)) = *point_target {
                let time_ns = (num(map, "time")? * 1_000_000_000.0).round() as i64;
                let value = num(map, "value")? as f32;
                let track = tracks.get_mut(index).ok_or_else(|| fail("automation for an unknown track"))?;
                let rate = 48_000u32;
                let point = Point { at: Rational(time_ns, 1_000_000_000), value: if volume { value } else { value * 2.0 - 1.0 } };
                if volume {
                    track.volume.push(point)
                } else {
                    track.pan.push(point)
                }
                let _ = rate;
            }
        }
        "Marker" => {
            let mut name = attr_get(map, "name");
            if name.is_empty() {
                name = "Marker".into();
            }
            soundcraft_model::truncate_chars(&mut name, soundcraft_model::MAX_NAME_CHARS);
            markers.push(Marker {
                name,
                at: Rational((num(map, "time")? * 1_000_000_000.0).round() as i64, 1_000_000_000),
                comment: attr_get(map, "comment"),
            });
        }
        _ => {}
    }
    Ok(())
}

fn finish_clip(
    c: ClipBuild,
    tracks: &mut [ImportTrack],
    sources: &mut Vec<Media>,
    by_path: &mut HashMap<String, String>,
    audio: &HashMap<String, Vec<u8>>,
) -> Result<()> {
    if c.sample_rate == 0 || c.duration <= 0 {
        return Err(fail("clip has no duration or sample rate"));
    }
    let key = if let Some(key) = by_path.get(&c.path) {
        key.clone()
    } else {
        let key = format!("m{}", sources.len());
        let embedded =
            if c.external { Vec::new() } else { audio.get(&c.path).cloned().ok_or_else(|| fail(format!("missing embedded audio {}", c.path)))? };
        if !embedded.is_empty() {
            let info = crate::aaf::wav_info(Cursor::new(embedded.as_slice())).map_err(fail)?;
            if info.sample_rate != c.sample_rate {
                return Err(fail("embedded WAV sample rate does not match the project"));
            }
        }
        let frames = if c.frames == 0 {
            u64::try_from(samples_of(&format!("{}", c.duration as f64 / 1_000_000_000.0), c.sample_rate)?).unwrap_or(1)
        } else {
            c.frames
        };
        sources.push(Media {
            key: key.clone(),
            name: c.name.clone(),
            path: if c.external { c.path.clone() } else { String::new() },
            channels: c.channels,
            frames,
            sample_rate: c.sample_rate,
            embedded_wav: embedded,
        });
        by_path.insert(c.path, key.clone());
        key
    };
    let track = tracks.get_mut(c.track).ok_or_else(|| fail("clip has no track"))?;
    if track.channels == 0 {
        track.channels = c.channels;
    }
    let rate = c.sample_rate;
    let to_samples = |ns: i64| -> Result<i64> {
        let secs = ns as f64 / 1_000_000_000.0;
        samples_of(&format!("{secs:.9}"), rate)
    };
    track.clips.push(ImportClip {
        name: c.name,
        source: key,
        start: rat(to_samples(c.time)?, rate),
        end: rat(to_samples(c.time)? + to_samples(c.duration)?, rate),
        offset: rat(to_samples(c.offset)?, rate),
        gain_db: c.gain_db,
        fade_in: rat(to_samples(c.fade_in)?, rate),
        fade_out: rat(to_samples(c.fade_out)?, rate),
        fade_in_curve: c.fade_in_curve,
        fade_out_curve: c.fade_out_curve,
    });
    Ok(())
}

/// Automation and markers were stored as nanosecond rationals. Once every clip has a sample rate,
/// convert those points to that rate. Called from [`read_dawproject`] after parsing... points are
/// converted here if the first source rate is known. `parse_project` leaves them as seconds
/// (`n/1_000_000_000`) which [`crate::build_session`] would treat as seconds only when the
/// denominator is the sample rate. Convert before returning.
pub fn normalize_times(plan: &mut Plan) -> Result<()> {
    let rate = plan.sample_rate;
    let fix = |t: &mut Rational| -> Result<()> {
        if t.1 == 1_000_000_000 {
            let samples = samples_of(&format!("{}", t.0 as f64 / 1_000_000_000.0), rate)?;
            *t = rat(samples, rate);
        }
        Ok(())
    };
    for track in &mut plan.tracks {
        for point in track.volume.iter_mut().chain(track.pan.iter_mut()) {
            fix(&mut point.at)?;
        }
    }
    for marker in &mut plan.markers {
        fix(&mut marker.at)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::aaf::wav_info;

    fn wav() -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend(80u32.to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(48000u32.to_le_bytes());
        v.extend((96000u32).to_le_bytes());
        v.extend(2u16.to_le_bytes());
        v.extend(16u16.to_le_bytes());
        v.extend(b"data");
        v.extend(9600u32.to_le_bytes());
        v.extend(vec![0; 9600]);
        v
    }

    #[test]
    fn dawproject_round_trips_the_edit() {
        let plan = Plan {
            schema_version: 1,
            name: "Hub".into(),
            sample_rate: 48000,
            timecode_start: Rational(0, 1),
            frame_rate: None,
            sources: vec![Media {
                key: "s".into(),
                name: "Src".into(),
                path: String::new(),
                channels: 1,
                frames: 4800,
                sample_rate: 48000,
                embedded_wav: wav(),
            }],
            tracks: vec![ImportTrack {
                name: "Vox".into(),
                channels: 1,
                clips: vec![ImportClip {
                    name: "Take".into(),
                    source: "s".into(),
                    start: Rational(4800, 48000),
                    end: Rational(9600, 48000),
                    offset: Rational(1000, 48000),
                    gain_db: -2.5,
                    fade_in: Rational(100, 48000),
                    fade_out: Rational(200, 48000),
                    fade_in_curve: Curve::Linear,
                    fade_out_curve: Curve::Logarithmic,
                }],
                volume: vec![Point { at: Rational(0, 48000), value: -4.0 }],
                pan: vec![Point { at: Rational(4800, 48000), value: -1.0 }],
            }],
            markers: vec![Marker { name: "Hook".into(), at: Rational(2400, 48000), comment: "yes".into() }],
            warnings: vec![],
        };
        let bytes = write_dawproject(&plan).unwrap();
        let mut back = read_dawproject(&bytes, "Hub").unwrap();
        normalize_times(&mut back).unwrap();
        assert_eq!(back.sample_rate, 48000);
        assert_eq!(back.tracks[0].name, "Vox");
        let clip = &back.tracks[0].clips[0];
        assert_eq!(clip.name, "Take");
        assert_eq!(clip.start.samples(48000).unwrap(), 4800);
        assert_eq!(clip.end.samples(48000).unwrap() - clip.start.samples(48000).unwrap(), 4800);
        assert_eq!(clip.offset.samples(48000).unwrap(), 1000);
        assert!((clip.gain_db - -2.5).abs() < 0.01);
        assert_eq!(clip.fade_in.samples(48000).unwrap(), 100);
        assert_eq!(clip.fade_out_curve, Curve::Logarithmic);
        assert!((back.tracks[0].volume[0].value - -4.0).abs() < 0.01);
        assert!((back.tracks[0].pan[0].value - -1.0).abs() < 0.05);
        assert_eq!(back.markers[0].name, "Hook");
        assert_eq!(back.markers[0].comment, "yes");
        assert!(wav_info(Cursor::new(back.sources[0].embedded_wav.as_slice())).is_ok());
    }

    #[test]
    fn path_traversal_is_rejected() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("../secret", opts).unwrap();
        zip.write_all(b"no").unwrap();
        zip.start_file("project.xml", opts).unwrap();
        zip.write_all(b"<Project version=\"1.0\"/>").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert!(read_dawproject(&bytes, "x").is_err());
    }
}
