//! Write the AAF subset [`crate::aaf::read_aaf`] reads back.
//!
//! Class and property identifiers are the public AAF baseline. Edit rate is the sample rate, so
//! component lengths are sample counts. Linked media is a `file://` locator; embedded media is an
//! essence stream holding a PCM WAV.

use crate::{Curve, ImportError, Plan, Rational};
use std::collections::HashMap;
use std::io::{Cursor, Write};

type Result<T> = std::result::Result<T, ImportError>;

fn fail(s: impl Into<String>) -> ImportError {
    ImportError(s.into())
}

const CLASS_HEADER: &str = "0d010101-0101-2f00-060e-2b3402060101";
const CLASS_CONTENT: &str = "0d010101-0101-1800-060e-2b3402060101";
const CLASS_DICTIONARY: &str = "0d010101-0101-2200-060e-2b3402060101";
const CLASS_DATA_DEF: &str = "0d010101-0101-1b00-060e-2b3402060101";
const CLASS_OP_DEF: &str = "0d010101-0101-1c00-060e-2b3402060101";
const CLASS_PARAM_DEF: &str = "0d010101-0101-1d00-060e-2b3402060101";
const CLASS_INTERP_DEF: &str = "0d010101-0101-2100-060e-2b3402060101";
const CLASS_COMPOSITION: &str = "0d010101-0101-3500-060e-2b3402060101";
const CLASS_MASTER: &str = "0d010101-0101-3600-060e-2b3402060101";
const CLASS_SOURCE: &str = "0d010101-0101-3700-060e-2b3402060101";
const CLASS_TIMELINE: &str = "0d010101-0101-3b00-060e-2b3402060101";
const CLASS_EVENT_SLOT: &str = "0d010101-0101-3900-060e-2b3402060101";
const CLASS_SEQUENCE: &str = "0d010101-0101-0f00-060e-2b3402060101";
const CLASS_SOURCE_CLIP: &str = "0d010101-0101-1100-060e-2b3402060101";
const CLASS_FILLER: &str = "0d010101-0101-0900-060e-2b3402060101";
const CLASS_TIMECODE: &str = "0d010101-0101-1400-060e-2b3402060101";
const CLASS_WAVE: &str = "0d010101-0101-2c00-060e-2b3402060101";
const CLASS_LOCATOR: &str = "0d010101-0101-3200-060e-2b3402060101";
const CLASS_OP_GROUP: &str = "0d010101-0101-0a00-060e-2b3402060101";
const CLASS_CONSTANT: &str = "0d010101-0101-3d00-060e-2b3402060101";
const CLASS_VARYING: &str = "0d010101-0101-3e00-060e-2b3402060101";
const CLASS_CONTROL: &str = "0d010101-0101-1900-060e-2b3402060101";
const CLASS_ESSENCE: &str = "0d010101-0101-2300-060e-2b3402060101";
const CLASS_MARKER: &str = "0d010101-0101-0800-060e-2b3402060101";

const SOUND: &str = "01030202-0200-0000-060e-2b3404010101";
const OP_GAIN_MONO: &str = "9d2ea894-0968-11d3-8a38-0050040ef7d2";
const OP_GAIN_STEREO: &str = "9d2ea895-0968-11d3-8a38-0050040ef7d2";
const OP_PAN: &str = "9d2ea893-0968-11d3-8a38-0050040ef7d2";
const PARAM_LEVEL: &str = "e4962320-2267-11d3-8a4c-0050040ef7d2";
const PARAM_PAN: &str = "e4962322-2267-11d3-8a4c-0050040ef7d2";
const INTERP_LINEAR: &str = "5b6c85a4-0ede-11d3-80a9-006008143e6f";
const RATIONAL_TYPE: &str = "03010100-0000-0000-060e-2b3401040101";

const PID_HEADER: u16 = 0x0002;
const PID_DEF_ID: u16 = 0x1B01;
const PID_DEF_NAME: u16 = 0x1B02;
const PID_DATA_DEFS: u16 = 0x2605;
const PID_OP_DEFS: u16 = 0x2603;
const PID_PARAM_DEFS: u16 = 0x2604;
const PID_INTERP_DEFS: u16 = 0x2609;
const PID_DICT: u16 = 0x3B04;
const PID_CONTENT: u16 = 0x3B03;
const PID_BYTE_ORDER: u16 = 0x3B01;
const WEAK_DATA: u16 = 0;
const WEAK_OP: u16 = 1;
const WEAK_PARAM: u16 = 2;
const WEAK_INTERP: u16 = 3;

struct Built {
    storages: Vec<(String, &'static str)>,
    streams: Vec<(String, Vec<u8>)>,
}

impl Built {
    fn obj(&mut self, path: &str, class: &'static str, props: Props) {
        self.storages.push((path.to_string(), class));
        self.streams.push((format!("{path}/properties"), props.finish()));
    }

    fn raw(&mut self, path: &str, bytes: Vec<u8>) {
        self.streams.push((path.to_string(), bytes));
    }
}

struct Props {
    items: Vec<(u16, u16, Vec<u8>)>,
}

impl Props {
    fn new() -> Self {
        Props { items: Vec::new() }
    }

    fn push(&mut self, pid: u16, form: u16, value: Vec<u8>) {
        self.items.push((pid, form, value));
    }

    fn data(&mut self, pid: u16, value: impl AsRef<[u8]>) {
        self.push(pid, 0x82, value.as_ref().to_vec());
    }

    fn u8(&mut self, pid: u16, v: u8) {
        self.data(pid, [v]);
    }

    fn u16(&mut self, pid: u16, v: u16) {
        self.data(pid, v.to_le_bytes());
    }

    fn u32(&mut self, pid: u16, v: u32) {
        self.data(pid, v.to_le_bytes());
    }

    fn i64(&mut self, pid: u16, v: i64) {
        self.data(pid, v.to_le_bytes());
    }

    fn rational(&mut self, pid: u16, n: i32, d: i32) {
        let mut v = Vec::with_capacity(8);
        v.extend(n.to_le_bytes());
        v.extend(d.to_le_bytes());
        self.data(pid, v);
    }

    fn text(&mut self, pid: u16, s: &str) {
        self.data(pid, utf16(s));
    }

    fn strong(&mut self, pid: u16, name: &str) {
        self.push(pid, 0x22, utf16(name));
    }

    fn vector(&mut self, pid: u16, name: &str) {
        self.push(pid, 0x32, utf16(name));
    }

    fn set(&mut self, pid: u16, name: &str) {
        self.push(pid, 0x3A, utf16(name));
    }

    fn weak(&mut self, pid: u16, index: u16, key: &[u8]) {
        let mut v = Vec::with_capacity(5 + key.len());
        v.extend(index.to_le_bytes());
        v.extend(PID_DEF_ID.to_le_bytes());
        v.push(u8::try_from(key.len()).unwrap_or(16));
        v.extend(key);
        self.push(pid, 0x02, v);
    }

    fn stream(&mut self, pid: u16, name: &str) {
        let mut v = vec![0x55];
        v.extend(utf16(name));
        self.push(pid, 0x42, v);
    }

    fn finish(self) -> Vec<u8> {
        let mut d = vec![0x4c, 0x20];
        d.extend(u16::try_from(self.items.len()).unwrap_or(u16::MAX).to_le_bytes());
        for (pid, form, value) in &self.items {
            d.extend(pid.to_le_bytes());
            d.extend(form.to_le_bytes());
            d.extend(u16::try_from(value.len()).unwrap_or(u16::MAX).to_le_bytes());
        }
        for (_, _, value) in &self.items {
            d.extend(value);
        }
        d
    }
}

fn utf16(s: &str) -> Vec<u8> {
    let mut v = Vec::new();
    for unit in s.encode_utf16() {
        v.extend(unit.to_le_bytes());
    }
    v.extend(0u16.to_le_bytes());
    v
}

fn auid(s: &str) -> Result<[u8; 16]> {
    let id = uuid::Uuid::parse_str(s).map_err(|_| fail("internal AAF identifier"))?;
    Ok(id.to_bytes_le())
}

fn class_uuid(s: &str) -> Result<uuid::Uuid> {
    uuid::Uuid::parse_str(s).map_err(|_| fail("internal AAF class"))
}

fn mob_bytes(n: u32) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[..12].copy_from_slice(&[0x06, 0x0a, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x0f, 0x00]);
    b[12] = 0x13;
    b[16..20].copy_from_slice(&n.to_le_bytes());
    b[24..30].copy_from_slice(&[0x06, 0x0e, 0x2b, 0x34, 0x7f, 0x7f]);
    b
}

fn samples(t: Rational, rate: u32) -> Result<i64> {
    t.samples(rate).map_err(|e| fail(e.0))
}

fn milli(v: f32) -> Result<(i32, i32)> {
    if !v.is_finite() {
        return Err(fail("non-finite parameter"));
    }
    let n = (f64::from(v) * 1000.0).round();
    if !n.is_finite() || n.abs() > f64::from(i32::MAX) {
        return Err(fail("parameter does not fit an AAF rational"));
    }
    Ok((n as i32, 1000))
}

fn indirect(n: i32, d: i32) -> Result<Vec<u8>> {
    let mut v = vec![0x4c];
    v.extend(auid(RATIONAL_TYPE)?);
    v.extend(n.to_le_bytes());
    v.extend(d.to_le_bytes());
    Ok(v)
}

fn i32_time(samples: i64) -> Result<i32> {
    i32::try_from(samples).map_err(|_| fail("automation time exceeds the AAF rational range"))
}

fn curve_byte(curve: Curve) -> u8 {
    match curve {
        Curve::Linear => 1,
        Curve::EqualPower => 2,
        Curve::SCurve => 3,
        Curve::Exponential => 4,
        Curve::Logarithmic => 5,
    }
}

fn locator_url(path: &str) -> String {
    let (prefix, raw) = if let Some(rest) = path.strip_prefix('/') {
        ("file:///", rest)
    } else if path.len() > 2 && path.as_bytes().get(1) == Some(&b':') {
        ("file:///", path)
    } else {
        ("", path)
    };
    let mut out = String::from(prefix);
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'~' | b'-') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn vec_index(n: usize) -> Vec<u8> {
    let mut d = Vec::new();
    let count = u32::try_from(n).unwrap_or(u32::MAX);
    d.extend(count.to_le_bytes());
    d.extend(count.to_le_bytes());
    d.extend(0xFFFFFFFFu32.to_le_bytes());
    for i in 0..n {
        d.extend(u32::try_from(i).unwrap_or(0).to_le_bytes());
    }
    d
}

fn set_index(keys: &[[u8; 16]]) -> Vec<u8> {
    let mut d = Vec::new();
    let count = u32::try_from(keys.len()).unwrap_or(0);
    d.extend(count.to_le_bytes());
    d.extend(count.to_le_bytes());
    d.extend(0xFFFFFFFFu32.to_le_bytes());
    d.extend(PID_DEF_ID.to_le_bytes());
    d.push(16);
    for (i, key) in keys.iter().enumerate() {
        d.extend(u32::try_from(i).unwrap_or(0).to_le_bytes());
        d.extend(1u32.to_le_bytes());
        d.extend(key);
    }
    d
}

fn sound(props: &mut Props, key: &[u8]) {
    props.weak(0x0201, WEAK_DATA, key);
}

struct Src {
    mob: [u8; 32],
    frames: i64,
}

/// Serialize `plan` as an AAF. Linked `path`s are locators; non-empty `embedded_wav` is essence.
pub fn write_aaf(plan: &Plan) -> Result<Vec<u8>> {
    if plan.schema_version != 1 {
        return Err(fail("unsupported interchange schema"));
    }
    if plan.sources.is_empty() {
        return Err(fail("AAF export needs at least one source"));
    }
    let rate = plan.sample_rate;
    if rate == 0 || i32::try_from(rate).is_err() {
        return Err(fail("sample rate does not fit an AAF edit rate"));
    }
    let sound_key = auid(SOUND)?;
    let mut built = Built { storages: Vec::new(), streams: Vec::new() };
    let mut mob_n = 1u32;
    let mut sources = HashMap::new();
    for (i, media) in plan.sources.iter().enumerate() {
        let frames = i64::try_from(media.frames).map_err(|_| fail("source is too long"))?;
        let mob = mob_bytes(mob_n);
        mob_n = mob_n.saturating_add(1);
        let path = format!("/Header/Content/Mobs{{{i:x}}}");
        // Replaced below once the mob set order is known. Collected first.
        sources.insert(media.key.clone(), Src { mob, frames });
        let _ = path;
    }

    // Dictionary, then content mobs in a stable order: composition, sources, masters.
    write_dictionary(&mut built, &sound_key)?;
    let comp_mob = mob_bytes(mob_n);
    mob_n = mob_n.saturating_add(1);

    for (ordinal, media) in plan.sources.iter().enumerate() {
        let src = sources.get(&media.key).ok_or_else(|| fail("source missing"))?;
        let path = format!("/Header/Content/Mobs{{{ordinal:x}}}");
        write_source(&mut built, &path, src, media, &sound_key, rate)?;
    }
    let source_count = plan.sources.len();

    let mut master_mobs = Vec::new();
    let mut track_bodies = Vec::new();
    for track in &plan.tracks {
        let mut clips = Vec::new();
        let mut cursor_check = 0i64;
        for clip in &track.clips {
            let start = samples(clip.start, rate)?;
            let end = samples(clip.end, rate)?;
            let offset = samples(clip.offset, rate)?;
            if end <= start || start < cursor_check {
                return Err(fail("clips must be ordered and must not overlap"));
            }
            let src = sources.get(&clip.source).ok_or_else(|| fail("clip names an unknown source"))?;
            if offset.saturating_add(end - start) > src.frames {
                return Err(fail("clip extends beyond its source"));
            }
            let fade_in = samples(clip.fade_in, rate)?;
            let fade_out = samples(clip.fade_out, rate)?;
            if fade_in.saturating_add(fade_out) > end - start {
                return Err(fail("fades are longer than the clip"));
            }
            let master = mob_bytes(mob_n);
            mob_n = mob_n.saturating_add(1);
            master_mobs.push((master, clip.name.clone(), src.mob, src.frames));
            clips.push(Placed {
                name_mob: master,
                start,
                length: end - start,
                offset,
                gain_db: clip.gain_db,
                fade_in,
                fade_out,
                fade_in_curve: clip.fade_in_curve,
                fade_out_curve: clip.fade_out_curve,
                channels: track.channels,
            });
            cursor_check = end;
        }
        let volume = points(&track.volume, rate)?;
        let pan = points(&track.pan, rate)?;
        track_bodies.push((track.name.clone(), clips, volume, pan));
    }

    let comp_index = source_count + master_mobs.len();
    for (i, (mob, name, source_mob, frames)) in master_mobs.iter().enumerate() {
        let path = format!("/Header/Content/Mobs{{{:x}}}", source_count + i);
        write_master(&mut built, &path, *mob, name, *source_mob, *frames, &sound_key, rate)?;
    }
    let comp_path = format!("/Header/Content/Mobs{{{comp_index:x}}}");
    write_composition(&mut built, &comp_path, comp_mob, plan, rate, &track_bodies, &sound_key)?;

    let mob_total = comp_index + 1;
    let mut content = Props::new();
    content.set(0x1901, "Mobs");
    let mut essence_keys = Vec::new();
    for (ordinal, media) in plan.sources.iter().enumerate() {
        if media.embedded_wav.is_empty() {
            continue;
        }
        let src = sources.get(&media.key).ok_or_else(|| fail("source missing"))?;
        let epath = format!("/Header/Content/Ess{{{:x}}}", essence_keys.len());
        let mut props = Props::new();
        props.data(0x2701, src.mob);
        props.stream(0x2702, "Data");
        built.obj(&epath, CLASS_ESSENCE, props);
        built.raw(&format!("{epath}/Data"), media.embedded_wav.clone());
        essence_keys.push(src.mob);
        let _ = ordinal;
    }
    if !essence_keys.is_empty() {
        content.set(0x1902, "Ess");
        built.raw("/Header/Content/Ess index", set_index_bytes(&essence_keys));
    }
    built.obj("/Header/Content", CLASS_CONTENT, content);
    built.raw("/Header/Content/Mobs index", set_index_mobs(mob_total, &sources, plan, &master_mobs, comp_mob)?);

    let mut header = Props::new();
    header.u16(PID_BYTE_ORDER, 0x4949);
    header.strong(PID_CONTENT, "Content");
    header.strong(PID_DICT, "Dictionary");
    built.obj("/Header", CLASS_HEADER, header);
    let mut root = Props::new();
    root.strong(PID_HEADER, "Header");
    built.streams.push(("/properties".into(), root.finish()));
    built.raw("/referenced properties", referenced_table());
    let _ = mob_n;
    emit(built)
}

struct Placed {
    name_mob: [u8; 32],
    start: i64,
    length: i64,
    offset: i64,
    gain_db: f32,
    fade_in: i64,
    fade_out: i64,
    fade_in_curve: Curve,
    fade_out_curve: Curve,
    channels: u16,
}

fn points(points: &[crate::Point], rate: u32) -> Result<Vec<(i64, f32)>> {
    points.iter().map(|p| Ok((samples(p.at, rate)?, p.value))).collect()
}

fn set_index_bytes(keys: &[[u8; 32]]) -> Vec<u8> {
    let mut d = Vec::new();
    let count = u32::try_from(keys.len()).unwrap_or(0);
    d.extend(count.to_le_bytes());
    d.extend(count.to_le_bytes());
    d.extend(0xFFFFFFFFu32.to_le_bytes());
    d.extend(0x4401u16.to_le_bytes());
    d.push(32);
    for (i, key) in keys.iter().enumerate() {
        d.extend(u32::try_from(i).unwrap_or(0).to_le_bytes());
        d.extend(1u32.to_le_bytes());
        d.extend(key);
    }
    d
}

fn set_index_mobs(
    total: usize,
    sources: &HashMap<String, Src>,
    plan: &Plan,
    masters: &[([u8; 32], String, [u8; 32], i64)],
    comp: [u8; 32],
) -> Result<Vec<u8>> {
    let mut keys = Vec::new();
    for media in &plan.sources {
        keys.push(sources.get(&media.key).ok_or_else(|| fail("source missing"))?.mob);
    }
    for (mob, _, _, _) in masters {
        keys.push(*mob);
    }
    keys.push(comp);
    if keys.len() != total {
        return Err(fail("internal mob count"));
    }
    Ok(set_index_bytes(&keys))
}

fn referenced_table() -> Vec<u8> {
    let paths: [&[u16]; 4] = [
        &[PID_HEADER, PID_DICT, PID_DATA_DEFS],
        &[PID_HEADER, PID_DICT, PID_OP_DEFS],
        &[PID_HEADER, PID_DICT, PID_PARAM_DEFS],
        &[PID_HEADER, PID_DICT, PID_INTERP_DEFS],
    ];
    let mut pids = Vec::new();
    for path in paths {
        pids.extend_from_slice(path);
        pids.push(0);
    }
    let mut d = vec![0x4c];
    d.extend(u16::try_from(paths.len()).unwrap_or(0).to_le_bytes());
    d.extend(u32::try_from(pids.len()).unwrap_or(0).to_le_bytes());
    for pid in pids {
        d.extend(pid.to_le_bytes());
    }
    d
}

fn def_obj(built: &mut Built, path: &str, class: &'static str, key: [u8; 16], name: &str) {
    let mut props = Props::new();
    props.data(PID_DEF_ID, key);
    props.text(PID_DEF_NAME, name);
    built.obj(path, class, props);
}

fn write_dictionary(built: &mut Built, sound_key: &[u8; 16]) -> Result<()> {
    let defs: [(&str, &'static str, &str, &str); 7] = [
        ("DDefs{0}", CLASS_DATA_DEF, SOUND, "DataDef_Sound"),
        ("Ops{0}", CLASS_OP_DEF, OP_GAIN_MONO, "OperationDef_MonoAudioGain"),
        ("Ops{1}", CLASS_OP_DEF, OP_GAIN_STEREO, "OperationDef_StereoAudioGain"),
        ("Ops{2}", CLASS_OP_DEF, OP_PAN, "OperationDef_MonoAudioPan"),
        ("PDefs{0}", CLASS_PARAM_DEF, PARAM_LEVEL, "ParameterDef_Level"),
        ("PDefs{1}", CLASS_PARAM_DEF, PARAM_PAN, "ParameterDef_Pan"),
        ("IDefs{0}", CLASS_INTERP_DEF, INTERP_LINEAR, "InterpolationDef_Linear"),
    ];
    let mut keys: HashMap<&str, Vec<[u8; 16]>> = HashMap::new();
    for (leaf, class, id, name) in defs {
        let key = auid(id)?;
        let set = leaf.split('{').next().unwrap_or(leaf);
        keys.entry(set).or_default().push(key);
        def_obj(built, &format!("/Header/Dictionary/{leaf}"), class, key, name);
    }
    for (set, key_list) in &keys {
        built.raw(&format!("/Header/Dictionary/{set} index"), set_index(key_list));
    }
    let mut props = Props::new();
    props.set(PID_DATA_DEFS, "DDefs");
    props.set(PID_OP_DEFS, "Ops");
    props.set(PID_PARAM_DEFS, "PDefs");
    props.set(PID_INTERP_DEFS, "IDefs");
    built.obj("/Header/Dictionary", CLASS_DICTIONARY, props);
    let _ = sound_key;
    Ok(())
}

fn write_source(built: &mut Built, path: &str, src: &Src, media: &crate::Media, sound_key: &[u8], rate: u32) -> Result<()> {
    if !media.embedded_wav.is_empty() {
        let info = crate::aaf::wav_info(Cursor::new(media.embedded_wav.as_slice())).map_err(fail)?;
        if info.sample_rate != rate || info.channels != media.channels || info.frames != media.frames {
            return Err(fail("embedded WAV does not match the source metadata"));
        }
    } else if media.path.is_empty() {
        return Err(fail("source has neither a path nor embedded audio"));
    }
    let desc = format!("{path}/Desc");
    let mut desc_props = Props::new();
    desc_props.rational(0x3001, i32::try_from(rate).unwrap_or(1), 1);
    desc_props.i64(0x3002, src.frames);
    if media.embedded_wav.is_empty() {
        let mut locator = Props::new();
        locator.text(0x4001, &locator_url(&media.path));
        built.obj(&format!("{desc}/Locs{{0}}"), CLASS_LOCATOR, locator);
        desc_props.vector(0x2F01, "Locs");
        built.raw(&format!("{desc}/Locs index"), vec_index(1));
    }
    built.obj(&desc, CLASS_WAVE, desc_props);

    // Masters carry the source clip. This slot only publishes the file's length so a walker that
    // does inspect it sees one edit rate and a zero origin.
    let slot = format!("{path}/Slots{{0}}");
    let mut segment = Props::new();
    sound(&mut segment, sound_key);
    segment.i64(0x0202, src.frames);
    built.obj(&format!("{slot}/Seg"), CLASS_FILLER, segment);
    let mut slot_props = Props::new();
    slot_props.u32(0x4801, 1);
    slot_props.strong(0x4803, "Seg");
    slot_props.rational(0x4B01, i32::try_from(rate).unwrap_or(1), 1);
    slot_props.i64(0x4B02, 0);
    built.obj(&slot, CLASS_TIMELINE, slot_props);
    built.raw(&format!("{path}/Slots index"), vec_index(1));

    let mut mob = Props::new();
    mob.data(0x4401, src.mob);
    mob.text(0x4402, &media.name);
    mob.vector(0x4403, "Slots");
    mob.strong(0x4701, "Desc");
    built.obj(path, CLASS_SOURCE, mob);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_master(built: &mut Built, path: &str, mob: [u8; 32], name: &str, source: [u8; 32], frames: i64, sound_key: &[u8], rate: u32) -> Result<()> {
    let slot = format!("{path}/Slots{{0}}");
    let mut clip = Props::new();
    sound(&mut clip, sound_key);
    clip.i64(0x0202, frames);
    clip.data(0x1101, source);
    clip.u32(0x1102, 1);
    clip.i64(0x1201, 0);
    built.obj(&format!("{slot}/Seg"), CLASS_SOURCE_CLIP, clip);
    let mut slot_props = Props::new();
    slot_props.u32(0x4801, 1);
    slot_props.strong(0x4803, "Seg");
    slot_props.rational(0x4B01, i32::try_from(rate).unwrap_or(1), 1);
    slot_props.i64(0x4B02, 0);
    built.obj(&slot, CLASS_TIMELINE, slot_props);
    built.raw(&format!("{path}/Slots index"), vec_index(1));
    let mut props = Props::new();
    props.data(0x4401, mob);
    props.text(0x4402, name);
    props.vector(0x4403, "Slots");
    built.obj(path, CLASS_MASTER, props);
    Ok(())
}

fn rounded_rate(n: i32, d: i32) -> Result<i64> {
    if d <= 0 {
        return Err(fail("invalid frame rate"));
    }
    let n = i128::from(n);
    let d = i128::from(d);
    let floor = n.div_euclid(d);
    let twice = (n - floor * d) * 2;
    let chosen = if twice < d || (twice == d && floor % 2 == 0) { floor } else { floor + 1 };
    i64::try_from(chosen).map_err(|_| fail("frame rate does not fit"))
}

fn write_composition(
    built: &mut Built,
    path: &str,
    mob: [u8; 32],
    plan: &Plan,
    rate: u32,
    tracks: &[(String, Vec<Placed>, Vec<(i64, f32)>, Vec<(i64, f32)>)],
    sound_key: &[u8],
) -> Result<()> {
    let mut slot_count = 0usize;
    if let Some(tc) = &plan.frame_rate {
        let fps_i = i32::try_from(tc.rate.0).map_err(|_| fail("frame rate numerator does not fit"))?;
        let fps_d = i32::try_from(tc.rate.1).map_err(|_| fail("frame rate denominator does not fit"))?;
        let rounded = rounded_rate(fps_i, fps_d)?;
        let frames = mul_rate(plan.timecode_start, tc.rate)?;
        let mut body = Props::new();
        body.i64(0x1501, frames);
        body.u16(0x1502, u16::try_from(rounded).unwrap_or(30));
        body.u8(0x1503, u8::from(tc.drop));
        built.obj(&format!("{path}/Slots{{{slot_count:x}}}/Seg"), CLASS_TIMECODE, body);
        let mut slot = Props::new();
        slot.u32(0x4801, 1);
        slot.strong(0x4803, "Seg");
        slot.rational(0x4B01, fps_i, fps_d);
        slot.i64(0x4B02, 0);
        built.obj(&format!("{path}/Slots{{{slot_count:x}}}"), CLASS_TIMELINE, slot);
        slot_count += 1;
    }
    for (name, clips, volume, pan) in tracks {
        write_track(built, path, slot_count, name, clips, volume, pan, sound_key, rate)?;
        slot_count += 1;
    }
    if !plan.markers.is_empty() {
        write_markers(built, path, slot_count, plan, rate, sound_key)?;
        slot_count += 1;
    }
    built.raw(&format!("{path}/Slots index"), vec_index(slot_count));
    let mut props = Props::new();
    props.data(0x4401, mob);
    props.text(0x4402, &plan.name);
    props.vector(0x4403, "Slots");
    built.obj(path, CLASS_COMPOSITION, props);
    Ok(())
}

fn mul_rate(seconds: Rational, rate: Rational) -> Result<i64> {
    let n = i128::from(seconds.0) * i128::from(rate.0);
    let d = i128::from(seconds.1) * i128::from(rate.1);
    if d <= 0 {
        return Err(fail("invalid timecode"));
    }
    let v = (n + d / 2) / d;
    i64::try_from(v).map_err(|_| fail("timecode overflow"))
}

#[allow(clippy::too_many_arguments)]
fn write_track(
    built: &mut Built,
    comp: &str,
    slot_index: usize,
    name: &str,
    clips: &[Placed],
    volume: &[(i64, f32)],
    pan: &[(i64, f32)],
    sound_key: &[u8],
    rate: u32,
) -> Result<()> {
    let slot = format!("{comp}/Slots{{{slot_index:x}}}");
    let automated = !volume.is_empty() || !pan.is_empty();
    let seq = if automated { format!("{slot}/Seq") } else { format!("{slot}/Seg") };
    let mut cursor = 0i64;
    let mut count = 0usize;
    let mut end = 0i64;
    for clip in clips {
        if clip.start > cursor {
            let gap = clip.start - cursor;
            let mut filler = Props::new();
            sound(&mut filler, sound_key);
            filler.i64(0x0202, gap);
            built.obj(&format!("{seq}/Comps{{{count:x}}}"), CLASS_FILLER, filler);
            count += 1;
        }
        write_clip(built, &seq, count, clip, sound_key)?;
        count += 1;
        cursor = clip.start + clip.length;
        end = cursor;
    }
    built.raw(&format!("{seq}/Comps index"), vec_index(count));
    let mut sequence = Props::new();
    sound(&mut sequence, sound_key);
    sequence.i64(0x0202, end);
    sequence.vector(0x1001, "Comps");
    built.obj(&seq, CLASS_SEQUENCE, sequence);
    if automated {
        write_automation(built, &slot, end, volume, pan, sound_key)?;
    }
    let mut slot_props = Props::new();
    slot_props.u32(0x4801, u32::try_from(slot_index).unwrap_or(1).saturating_add(1));
    slot_props.text(0x4802, name);
    slot_props.strong(0x4803, "Seg");
    slot_props.rational(0x4B01, i32::try_from(rate).unwrap_or(1), 1);
    slot_props.i64(0x4B02, 0);
    built.obj(&slot, CLASS_TIMELINE, slot_props);
    Ok(())
}

fn write_clip(built: &mut Built, slot: &str, index: usize, clip: &Placed, sound_key: &[u8]) -> Result<()> {
    let mut props = Props::new();
    sound(&mut props, sound_key);
    props.i64(0x0202, clip.length);
    props.data(0x1101, clip.name_mob);
    props.u32(0x1102, 1);
    props.i64(0x1201, clip.offset);
    if clip.fade_in > 0 {
        props.i64(0x1202, clip.fade_in);
        props.u8(0x1203, curve_byte(clip.fade_in_curve));
    }
    if clip.fade_out > 0 {
        props.i64(0x1204, clip.fade_out);
        props.u8(0x1205, curve_byte(clip.fade_out_curve));
    }
    let leaf = format!("{slot}/Comps{{{index:x}}}");
    if clip.gain_db.abs() < 0.0005 {
        built.obj(&leaf, CLASS_SOURCE_CLIP, props);
        return Ok(());
    }
    let op = if clip.channels > 1 { auid(OP_GAIN_STEREO)? } else { auid(OP_GAIN_MONO)? };
    let (n, d) = milli(clip.gain_db)?;
    built.obj(&format!("{leaf}/In{{0}}"), CLASS_SOURCE_CLIP, props);
    let mut constant = Props::new();
    constant.weak(0x4C01, WEAK_PARAM, &auid(PARAM_LEVEL)?);
    constant.data(0x4D01, indirect(n, d)?);
    built.obj(&format!("{leaf}/Par{{0}}"), CLASS_CONSTANT, constant);
    built.raw(&format!("{leaf}/In index"), vec_index(1));
    built.raw(&format!("{leaf}/Par index"), vec_index(1));
    let mut group = Props::new();
    sound(&mut group, sound_key);
    group.i64(0x0202, clip.length);
    group.weak(0x0B01, WEAK_OP, &op);
    group.vector(0x0B02, "In");
    group.vector(0x0B03, "Par");
    built.obj(&leaf, CLASS_OP_GROUP, group);
    Ok(())
}

fn write_automation(built: &mut Built, slot: &str, length: i64, volume: &[(i64, f32)], pan: &[(i64, f32)], sound_key: &[u8]) -> Result<()> {
    let mut which = Vec::new();
    if !volume.is_empty() {
        which.push((PARAM_LEVEL, volume));
    }
    if !pan.is_empty() {
        which.push((PARAM_PAN, pan));
    }
    for (i, (param, points)) in which.iter().enumerate() {
        let base = format!("{slot}/Seg/Par{{{i:x}}}");
        for (p, (at, value)) in points.iter().enumerate() {
            let (n, d) = milli(*value)?;
            let mut point = Props::new();
            point.rational(0x1A03, i32_time(*at)?, 1);
            point.data(0x1A02, indirect(n, d)?);
            built.obj(&format!("{base}/Pts{{{p:x}}}"), CLASS_CONTROL, point);
        }
        built.raw(&format!("{base}/Pts index"), vec_index(points.len()));
        let mut varying = Props::new();
        varying.weak(0x4C01, WEAK_PARAM, &auid(param)?);
        varying.weak(0x4E01, WEAK_INTERP, &auid(INTERP_LINEAR)?);
        varying.vector(0x4E02, "Pts");
        built.obj(&base, CLASS_VARYING, varying);
    }
    built.raw(&format!("{slot}/Seg/Par index"), vec_index(which.len()));
    built.raw(&format!("{slot}/Seg/In index"), vec_index(1));
    // The sequence was stored at `{slot}/Seq`. The group input must be `{slot}/Seg/In{0}`.
    relocate_sequence(built, &format!("{slot}/Seq"), &format!("{slot}/Seg/In{{0}}"));
    let op = if !volume.is_empty() { auid(OP_GAIN_MONO)? } else { auid(OP_PAN)? };
    let mut group = Props::new();
    sound(&mut group, sound_key);
    group.i64(0x0202, length);
    group.weak(0x0B01, WEAK_OP, &op);
    group.vector(0x0B02, "In");
    group.vector(0x0B03, "Par");
    built.obj(&format!("{slot}/Seg"), CLASS_OP_GROUP, group);
    Ok(())
}

/// Rename a storage and the streams under it. Vector children keep their relative names.
fn relocate_sequence(built: &mut Built, from: &str, to: &str) {
    let rewrite = |path: &str| -> String {
        if path == from {
            to.to_string()
        } else if let Some(rest) = path.strip_prefix(from) {
            format!("{to}{rest}")
        } else {
            path.to_string()
        }
    };
    for (path, _) in &mut built.storages {
        *path = rewrite(path);
    }
    for (path, _) in &mut built.streams {
        *path = rewrite(path);
    }
}

fn write_markers(built: &mut Built, comp: &str, slot_index: usize, plan: &Plan, rate: u32, sound_key: &[u8]) -> Result<()> {
    let slot = format!("{comp}/Slots{{{slot_index:x}}}");
    for (i, marker) in plan.markers.iter().enumerate() {
        let at = samples(marker.at, rate)?;
        let mut text = marker.name.clone();
        if !marker.comment.is_empty() {
            text.push('\n');
            text.push_str(&marker.comment);
        }
        let mut props = Props::new();
        sound(&mut props, sound_key);
        props.i64(0x0202, 1);
        props.i64(0x0601, at);
        props.text(0x0602, &text);
        built.obj(&format!("{slot}/Seg/Comps{{{i:x}}}"), CLASS_MARKER, props);
    }
    built.raw(&format!("{slot}/Seg/Comps index"), vec_index(plan.markers.len()));
    let mut sequence = Props::new();
    sound(&mut sequence, sound_key);
    sequence.i64(0x0202, 1);
    sequence.vector(0x1001, "Comps");
    built.obj(&format!("{slot}/Seg"), CLASS_SEQUENCE, sequence);
    let mut slot_props = Props::new();
    slot_props.u32(0x4801, u32::try_from(slot_index).unwrap_or(1).saturating_add(1));
    slot_props.text(0x4802, "Markers");
    slot_props.strong(0x4803, "Seg");
    slot_props.rational(0x4901, i32::try_from(rate).unwrap_or(1), 1);
    built.obj(&slot, CLASS_EVENT_SLOT, slot_props);
    Ok(())
}

fn emit(mut built: Built) -> Result<Vec<u8>> {
    built.storages.sort_by_key(|(path, _)| path.split('/').filter(|part| !part.is_empty()).count());
    let mut file = cfb::CompoundFile::create(Cursor::new(Vec::new())).map_err(|e| fail(e.to_string()))?;
    for (path, class) in &built.storages {
        file.create_storage(path).map_err(|e| fail(format!("{path}: {e}")))?;
        file.set_storage_clsid(path, class_uuid(class)?).map_err(|e| fail(e.to_string()))?;
    }
    for (path, bytes) in &built.streams {
        file.create_stream(path).map_err(|e| fail(format!("{path}: {e}")))?;
        file.open_stream(path).map_err(|e| fail(e.to_string()))?.write_all(bytes).map_err(|e| fail(e.to_string()))?;
    }
    file.flush().map_err(|e| fail(e.to_string()))?;
    Ok(file.into_inner().into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::aaf::{AafOptions, MediaError, MediaInfo, MediaResolver, read_aaf};
    use crate::{Curve, ImportClip, ImportTrack, Marker, Media, Plan, Point, Rational, TimecodeRate};
    use std::path::{Path, PathBuf};

    fn wav(frames: usize) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend(b"RIFF");
        let data = frames * 2;
        v.extend(((36 + data) as u32).to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(48000u32.to_le_bytes());
        v.extend((48000u32 * 2).to_le_bytes());
        v.extend(2u16.to_le_bytes());
        v.extend(16u16.to_le_bytes());
        v.extend(b"data");
        v.extend((data as u32).to_le_bytes());
        v.extend(vec![0; data]);
        v
    }

    fn rich() -> Plan {
        let audio = wav(48_000);
        Plan {
            schema_version: 1,
            name: "Round Trip".into(),
            sample_rate: 48000,
            timecode_start: Rational(3600, 1),
            frame_rate: Some(TimecodeRate { rate: Rational(25, 1), drop: false }),
            sources: vec![Media {
                key: "s".into(),
                name: "Source".into(),
                path: String::new(),
                channels: 1,
                frames: 48_000,
                sample_rate: 48000,
                embedded_wav: audio,
            }],
            tracks: vec![ImportTrack {
                name: "DX".into(),
                channels: 1,
                clips: vec![
                    ImportClip {
                        name: "A".into(),
                        source: "s".into(),
                        start: Rational(0, 1),
                        end: Rational(1, 4),
                        offset: Rational(1, 10),
                        gain_db: -3.5,
                        fade_in: Rational(1, 100),
                        fade_out: Rational(1, 50),
                        fade_in_curve: Curve::Linear,
                        fade_out_curve: Curve::SCurve,
                    },
                    ImportClip {
                        name: "B".into(),
                        source: "s".into(),
                        start: Rational(1, 2),
                        end: Rational(3, 4),
                        offset: Rational(1, 5),
                        gain_db: 0.0,
                        fade_in: Rational::default(),
                        fade_out: Rational::default(),
                        fade_in_curve: Curve::EqualPower,
                        fade_out_curve: Curve::EqualPower,
                    },
                ],
                volume: vec![Point { at: Rational(0, 1), value: -6.0 }, Point { at: Rational(1, 2), value: -1.5 }],
                pan: vec![Point { at: Rational(0, 1), value: 0.25 }],
            }],
            markers: vec![Marker { name: "Verse".into(), at: Rational(1, 5), comment: "go".into() }],
            warnings: vec![],
        }
    }

    struct Refuse;
    impl MediaResolver for Refuse {
        fn resolve(&mut self, locator: &Path) -> std::result::Result<MediaInfo, MediaError> {
            Err(MediaError::Missing(locator.display().to_string()))
        }
    }

    #[test]
    fn embedded_aaf_round_trips_edits_fades_gain_automation_and_markers() {
        let plan = rich();
        let bytes = write_aaf(&plan).unwrap();
        let options = AafOptions { composition: None, base_dir: Path::new("/tmp"), default_name: "Round Trip" };
        let back = read_aaf(Cursor::new(bytes), &options, &mut Refuse).unwrap();
        assert_eq!(back.name, "Round Trip");
        assert_eq!(back.sample_rate, 48000);
        assert_eq!(back.timecode_start, Rational(3600, 1));
        assert_eq!(back.tracks.len(), 1);
        assert_eq!(back.tracks[0].name, "DX");
        let clips = &back.tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].name, "A");
        assert_eq!((clips[0].start, clips[0].end, clips[0].offset), (Rational(0, 1), Rational(1, 4), Rational(1, 10)));
        assert!((clips[0].gain_db - -3.5).abs() < 0.01);
        assert_eq!(clips[0].fade_in, Rational(1, 100));
        assert_eq!(clips[0].fade_out, Rational(1, 50));
        assert_eq!(clips[0].fade_in_curve, Curve::Linear);
        assert_eq!(clips[0].fade_out_curve, Curve::SCurve);
        assert_eq!(clips[1].start, Rational(1, 2));
        assert_eq!(back.tracks[0].volume.len(), 2);
        assert!((back.tracks[0].volume[1].value - -1.5).abs() < 0.01);
        assert!((back.tracks[0].pan[0].value - 0.25).abs() < 0.01);
        assert_eq!(back.markers.len(), 1);
        assert_eq!(back.markers[0].name, "Verse");
        assert_eq!(back.markers[0].comment, "go");
        assert!(!back.sources[0].embedded_wav.is_empty());
        assert_eq!(back.sources[0].frames, 48_000);
    }

    struct Link;
    impl MediaResolver for Link {
        fn resolve(&mut self, locator: &Path) -> std::result::Result<MediaInfo, MediaError> {
            if locator.ends_with("room.wav") {
                Ok(MediaInfo { path: locator.display().to_string(), relinked_from: None, channels: 1, frames: 4800, sample_rate: 48000 })
            } else {
                Err(MediaError::Missing(locator.display().to_string()))
            }
        }
    }

    #[test]
    fn linked_locator_is_resolved_without_copying_samples() {
        let mut plan = rich();
        plan.sources[0].embedded_wav.clear();
        plan.sources[0].path = "room.wav".into();
        plan.sources[0].frames = 4800;
        plan.tracks[0].clips.truncate(1);
        plan.tracks[0].clips[0].end = Rational(1, 48);
        plan.tracks[0].clips[0].offset = Rational(0, 1);
        plan.tracks[0].clips[0].fade_in = Rational::default();
        plan.tracks[0].clips[0].fade_out = Rational::default();
        plan.tracks[0].volume.clear();
        plan.tracks[0].pan.clear();
        plan.markers.clear();
        let bytes = write_aaf(&plan).unwrap();
        let back =
            read_aaf(Cursor::new(bytes), &AafOptions { composition: None, base_dir: Path::new("/media"), default_name: "x" }, &mut Link).unwrap();
        assert!(back.sources[0].path.ends_with("room.wav"), "{}", back.sources[0].path);
        assert!(back.sources[0].embedded_wav.is_empty());
        assert_eq!(back.tracks[0].clips[0].length_samples_hint(), 1000);
    }

    trait Hint {
        fn length_samples_hint(&self) -> i64;
    }
    impl Hint for ImportClip {
        fn length_samples_hint(&self) -> i64 {
            self.end.samples(48000).unwrap() - self.start.samples(48000).unwrap()
        }
    }

    #[test]
    fn garbage_is_rejected() {
        let err = read_aaf(Cursor::new(b"not an aaf"), &AafOptions { composition: None, base_dir: Path::new("/"), default_name: "x" }, &mut Refuse);
        assert!(err.is_err());
        let _ = PathBuf::new();
    }
}
