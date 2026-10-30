//! Field extraction. Offsets are the ones in `docs/formats/ptx.md`.

use crate::tree::{self, Node};
use crate::{ByteOrder, Document, Keying, Lane, Marker, Media, MidiClip, MidiNote, Placement, Region, Role, SignalPath};

pub(crate) fn document(data: &[u8], little_endian: bool, keying: Keying) -> Result<Document, crate::Error> {
    let nodes = tree::walk(data, little_endian)?;
    let order = if little_endian { ByteOrder::Little } else { ByteOrder::Big };
    let mut doc = Document {
        order,
        keying,
        version: version(data, &nodes, little_endian),
        sample_rate: sample_rate(data, &nodes, little_endian),
        frame_rate_code: frame_code(data, &nodes),
        lanes: Vec::new(),
        media: Vec::new(),
        regions: Vec::new(),
        placements: Vec::new(),
        markers: Vec::new(),
        paths: Vec::new(),
        outputs: Vec::new(),
        plugins: Vec::new(),
        midi: Vec::new(),
        notes: Vec::new(),
    };
    doc.media = media(data, &nodes, little_endian, doc.version);
    doc.regions = regions(data, &nodes, little_endian);
    let described = descriptors(data, &nodes, little_endian, &mut doc.notes);
    let mirrors = mirrors(data, &nodes, little_endian);
    playlists(data, &nodes, little_endian, &described, &mirrors, &mut doc);
    if doc.lanes.iter().all(|l| l.role != Role::Audio) {
        doc.lanes.extend(described.into_iter().map(|(name, channels)| Lane { name, role: Role::Audio, channels }));
    }
    extra_lanes(&mirrors, &mut doc);
    doc.markers = markers(data, &nodes, little_endian);
    doc.paths = paths(data, &nodes, little_endian, &mut doc.notes);
    doc.outputs = outputs(data, &nodes);
    doc.plugins = plugins(data, &nodes, little_endian);
    doc.midi = midi_chunks(data, &nodes, little_endian);
    let fades = count_fades(data, &nodes);
    if fades > 0 {
        doc.notes.push(format!("{fades} fade event(s) skipped"));
    }
    let known = doc.sample_rate.is_some()
        || !doc.lanes.is_empty()
        || !doc.markers.is_empty()
        || !doc.media.is_empty()
        || !doc.paths.is_empty()
        || !doc.regions.is_empty();
    if !known {
        return Err(crate::Error::new("no sample rate, track, marker, media, or I/O path was recognised"));
    }
    Ok(doc)
}

fn sample_rate(data: &[u8], nodes: &[Node], little_endian: bool) -> Option<u32> {
    find(nodes, 0x1028).into_iter().find_map(|n| {
        let rate = tree::u32_at(n.body(data), 2, little_endian)?;
        (8_000..=768_000).contains(&rate).then_some(rate)
    })
}

fn version(data: &[u8], nodes: &[Node], little_endian: bool) -> Option<u32> {
    find(nodes, 0x2067).into_iter().find_map(|n| {
        let word = tree::u32_at(n.body(data), 18, little_endian)?.checked_add(2)?;
        (5..=40).contains(&word).then_some(word)
    })
}

fn frame_code(data: &[u8], nodes: &[Node]) -> Option<u32> {
    find(nodes, 0x204d).into_iter().find_map(|n| {
        let body = n.body(data);
        let bytes = body.get(40..44)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    })
}

fn media(data: &[u8], nodes: &[Node], little_endian: bool, version: Option<u32>) -> Vec<Media> {
    let mut names = Vec::new();
    for node in find(nodes, 0x103a) {
        push_names(node.body(data), little_endian, version, &mut names);
    }
    let lengths: Vec<u64> = find(nodes, 0x1001).into_iter().filter_map(|n| tree::u64_at(n.body(data), 6, little_endian)).collect();
    names.into_iter().enumerate().map(|(i, name)| Media { frames: lengths.get(i).copied(), name }).collect()
}

fn push_names(body: &[u8], little_endian: bool, version: Option<u32>, names: &mut Vec<String>) {
    let mut i = 9usize;
    while i + 4 <= body.len() && names.len() < 100_000 {
        let Some((name, after)) = text(body, i, little_endian, true) else { break };
        let tag = body.get(after..after + 4).unwrap_or(&[]);
        i = after.saturating_add(9);
        if keep_media(&name, tag, version) {
            names.push(name);
        }
    }
}

fn keep_media(name: &str, tag: &[u8], version: Option<u32>) -> bool {
    if name.is_empty() || name == "Audio Files" || name == "Fade Files" || name.to_ascii_lowercase().ends_with(".grp") {
        return false;
    }
    let known = matches!(tag, b"WAVE" | b"EVAW" | b"AIFF" | b"FFIA");
    if known {
        return true;
    }
    let recent = version.is_none_or(|v| v >= 10);
    recent && tag.first() == Some(&0) && has_audio_suffix(name)
}

fn has_audio_suffix(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".wav") || lower.ends_with(".aif") || lower.ends_with(".aiff")
}

fn descriptors(data: &[u8], nodes: &[Node], little_endian: bool, notes: &mut Vec<String>) -> Vec<(String, u16)> {
    let mut out = Vec::new();
    for node in find(nodes, 0x1014) {
        let body = node.body(data);
        let Some((name, end)) = text(body, 0, little_endian, false) else { continue };
        if name.is_empty() {
            continue;
        }
        let raw = tree::u32_at(body, end.saturating_add(1), little_endian).unwrap_or(1);
        let channels = u16::try_from(raw).unwrap_or(0);
        if !(1..=16).contains(&channels) {
            notes.push(format!("channel count {raw} on {name} is not 1..=16; stored as 1"));
            out.push((name, 1));
        } else {
            out.push((name, channels));
        }
    }
    out
}

fn mirrors(data: &[u8], nodes: &[Node], little_endian: bool) -> Vec<(u8, String)> {
    find(nodes, 0x251a)
        .into_iter()
        .filter_map(|n| {
            let body = n.body(data);
            let kind = *body.first()?;
            let (name, _) = text(body, 2, little_endian, false)?;
            (!name.is_empty()).then_some((kind, name))
        })
        .collect()
}

fn playlists(data: &[u8], nodes: &[Node], little_endian: bool, described: &[(String, u16)], mirrors: &[(u8, String)], doc: &mut Document) {
    let maps = find(nodes, 0x1054);
    let Some(map) = maps.first() else { return };
    let mut audio_mirrors = mirrors.iter().filter(|(k, _)| *k == 0).map(|(_, n)| n.as_str());
    let mut anonymous = 0u32;
    for playlist in map.children.iter().filter(|n| n.code == 0x1052) {
        let body = playlist.body(data);
        let Some((mut name, _)) = text(body, 0, little_endian, true) else { continue };
        if name.is_empty() {
            name = audio_mirrors.next().unwrap_or("").to_string();
        }
        if name.is_empty() {
            anonymous += 1;
            name = format!("Audio {anonymous}");
            doc.notes.push(format!("{name} had no stored playlist name"));
        }
        let channels = described.iter().find(|(n, _)| n == &name).map(|(_, c)| *c).unwrap_or(1);
        let lane_index = doc.lanes.len();
        for event in playlist.children.iter().filter(|n| n.code == 0x1050) {
            if let Some(placement) = placement(data, event, little_endian)
                && !placement.fade
            {
                doc.placements.push(Placement { lane_index, region_index: placement.region, start: placement.start, muted: placement.muted });
            }
        }
        doc.lanes.push(Lane { name, role: Role::Audio, channels });
    }
}

struct RawPlacement {
    region: u32,
    start: u64,
    muted: bool,
    fade: bool,
}

fn placement(data: &[u8], event: &Node, little_endian: bool) -> Option<RawPlacement> {
    let child = event.children.iter().find(|n| n.code == 0x104f)?;
    let body = child.body(data);
    if body.len() < 16 {
        return None;
    }
    let tag = body[15];
    let modern = tag == 0x01 || tag == 0x03;
    let (region, start) = if modern || little_endian {
        let region = u32::from_le_bytes([body[2], body[3], body[4], body[5]]);
        let start = u64::from_le_bytes([body[7], body[8], body[9], body[10], body[11], body[12], body[13], body[14]]);
        (region, start)
    } else {
        let region = tree::u32_at(body, 2, false)?;
        let start = u64::from(tree::u32_at(body, 7, false)?);
        (region, start)
    };
    Some(RawPlacement { region, start, muted: body[0] == 0x01, fade: tag == 0x01 })
}

fn count_fades(data: &[u8], nodes: &[Node]) -> usize {
    find(nodes, 0x104f).into_iter().filter(|n| n.body(data).get(15) == Some(&0x01)).count()
}

fn regions(data: &[u8], nodes: &[Node], little_endian: bool) -> Vec<Region> {
    let mut out = Vec::new();
    for code in [0x262a_u16, 0x100b] {
        for list in find(nodes, code) {
            let child_code = if code == 0x262a { 0x2629 } else { 0x1008 };
            for child in list.children.iter().filter(|n| n.code == child_code) {
                if let Some(region) = region(data, child, little_endian) {
                    out.push(region);
                }
            }
        }
    }
    out
}

fn region(data: &[u8], node: &Node, little_endian: bool) -> Option<Region> {
    if let Some(def) = node.children.iter().find(|n| n.code == 0x2628)
        && let Some(mut region) = geometry(def.body(data))
    {
        region.media_index = media_index(data, def, little_endian);
        return Some(region);
    }
    let body = node.body(data);
    let (name, after) = text(body, 9, little_endian, false)?;
    let (offset, length, anchor) = three_widths(body, after, little_endian)?;
    let media_index = node.children.first().and_then(|child| media_index(data, child, little_endian));
    Some(Region { name, offset, length, anchor, sync: 0, media_index })
}

fn media_index(data: &[u8], child: &Node, little_endian: bool) -> Option<u32> {
    let at = child.body_at + child.body_len;
    // The code word sits two bytes before the body, and `size` covers the code and the body.
    // The byte just after the child record is `body_at + body_len`.
    tree::u32_at(data, at, little_endian)
}

fn geometry(body: &[u8]) -> Option<Region> {
    let (name, a) = text(body, 0, true, false)?;
    if body.len() < a + 5 {
        return None;
    }
    if body.get(a..a + 5) == Some(&[0x00, 0x30, 0x22, 0x44, 0x08]) {
        let offset = u64::from(u24_le(body, a + 5)?);
        let sync = u64::from(u16_le(body, a + 8)?);
        let length = u64::from(u16_le(body, a + 10)?);
        return Some(Region { name, offset, length, anchor: 0, sync, media_index: None });
    }
    let flags = u16_le(body, a)?;
    let width = match body.get(a + 2).copied()? {
        0x10 => 1,
        0x20 => 2,
        0x30 => 3,
        0x40 => 4,
        _ => return None,
    };
    let (offset, at) = match flags {
        0x0000 | 0x0001 => (0, a + 5),
        0x1001 => (u64::from(*body.get(a + 5)?), a + 6),
        0x2000 | 0x2001 => (u64::from(u16_le(body, a + 5)?), a + 7),
        0x3000 | 0x3001 => (u64::from(u24_le(body, a + 5)?), a + 8),
        0x4001 => (u64::from(u32_le(body, a + 5)?), a + 9),
        _ => return None,
    };
    let length = width_le(body, at, width)?;
    Some(Region { name, offset, length, anchor: 0, sync: 0, media_index: None })
}

fn three_widths(body: &[u8], j: usize, little_endian: bool) -> Option<(u64, u64, u64)> {
    let b = body.get(j..j + 5)?;
    let (ow, lw, sw) = if little_endian { (b[1] >> 4, b[2] >> 4, b[3] >> 4) } else { (b[4] >> 4, b[3] >> 4, b[2] >> 4) };
    let mut p = j + 5;
    let offset = width_le(body, p, ow)?;
    p += ow as usize;
    let length = width_le(body, p, lw)?;
    p += lw as usize;
    let anchor = width_le(body, p, sw)?;
    Some((offset, length, anchor))
}

fn markers(data: &[u8], nodes: &[Node], little_endian: bool) -> Vec<Marker> {
    let mut out = Vec::new();
    for ruler in find(nodes, 0x2030) {
        for mark in ruler.children.iter().filter(|n| n.code == 0x2077) {
            if let Some(marker) = marker(mark.body(data), little_endian) {
                out.push(marker);
            }
        }
    }
    out
}

fn marker(body: &[u8], little_endian: bool) -> Option<Marker> {
    let le_fits = text(body, 6, true, true).is_some_and(|(_, end)| end <= body.len());
    let little = le_fits || little_endian;
    let (name, end) = text(body, 6, little, false)?;
    // `text` starts at the length word, which for a marker is at offset 6, so the name
    // bytes start at 10. `end` is the first byte after the name.
    let start = i64_at(body, end, little)?;
    if start < 0 {
        return None;
    }
    let end_at = i64_at(body, end + 8, little).unwrap_or(start);
    let index = if little { u16_le(body, 0)? } else { tree::u16_at(body, 0, false)? };
    let comment = marker_comment(body, end.saturating_sub(10));
    Some(Marker { index, name, start, end: end_at.max(start), comment })
}

fn marker_comment(body: &[u8], name_len: usize) -> String {
    let at = 86usize.saturating_add(name_len);
    let Some(len) = u32_le(body, at) else { return String::new() };
    let len = len as usize;
    if len > 4096 || body.len() != 136 + name_len + len {
        return String::new();
    }
    let start = 90 + name_len;
    match body.get(start..start + len).and_then(|b| std::str::from_utf8(b).ok()) {
        Some(text) if !text.contains('\0') => text.to_string(),
        _ => String::new(),
    }
}

fn paths(data: &[u8], nodes: &[Node], little_endian: bool, notes: &mut Vec<String>) -> Vec<SignalPath> {
    let mut out = Vec::new();
    for node in find(nodes, 0x2602) {
        let body = node.body(data);
        let Some((name, end)) = text(body, 2, little_endian, false) else { continue };
        if name.is_empty() {
            continue;
        }
        let width = tree::u32_at(body, end, little_endian).unwrap_or(0);
        let channels = if width == 1 || width == 2 { width as u16 } else { 0 };
        if channels == 0 && width != 0 {
            notes.push(format!("I/O path {name} has width {width}; treated as an unspecified width"));
        }
        out.push(SignalPath { name, channels });
    }
    out
}

fn outputs(data: &[u8], nodes: &[Node]) -> Vec<String> {
    find(nodes, 0x260e)
        .into_iter()
        .filter_map(|n| {
            let body = n.body(data);
            if body.get(8..12) != Some(&[0x2a, 0x00, 0x00, 0x00]) {
                return None;
            }
            let (name, _) = text(body, 36, true, false)?;
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

fn plugins(data: &[u8], nodes: &[Node], little_endian: bool) -> Vec<String> {
    let mut out = Vec::new();
    for node in find(nodes, 0x1017) {
        scan_plugin(node.body(data), little_endian, &mut out);
    }
    for node in find(nodes, 0x1018) {
        if node.children.iter().all(|c| c.code != 0x1017) {
            scan_plugin(node.body(data), little_endian, &mut out);
        }
    }
    out
}

fn scan_plugin(body: &[u8], little_endian: bool, out: &mut Vec<String>) {
    let mut i = 0usize;
    while i + 4 < body.len() && out.len() < 32 {
        if let Some((name, after)) = text(body, i, little_endian, false)
            && (2..=80).contains(&name.len())
            && name.chars().any(|c| c.is_alphabetic())
            && !out.iter().any(|s| s == &name)
        {
            out.push(name);
            i = after;
        } else {
            i += 1;
        }
    }
}

fn midi_chunks(data: &[u8], nodes: &[Node], little_endian: bool) -> Vec<MidiClip> {
    let mut out = Vec::new();
    for node in find(nodes, 0x2000) {
        let body = node.body(data);
        let mut i = 0usize;
        while i + 16 < body.len() {
            if body.get(i..i + 5) != Some(b"MdNLB") {
                i += 1;
                continue;
            }
            let count_at = i + 11;
            let Some(count) = tree::u32_at(body, count_at, little_endian) else { break };
            let mut at = count_at + 4;
            let origin = width_of(body, at, 5, little_endian).unwrap_or(0);
            let mut notes = Vec::new();
            let limit = count.min(100_000);
            for _ in 0..limit {
                if at + 35 > body.len() {
                    break;
                }
                if let Some(note) = midi_note(body, at, origin, little_endian) {
                    notes.push(note);
                }
                at += 35;
            }
            if !notes.is_empty() {
                out.push(MidiClip { notes });
            }
            i = at;
        }
    }
    out
}

fn midi_note(body: &[u8], at: usize, origin: u64, little_endian: bool) -> Option<MidiNote> {
    let pos = width_of(body, at, 5, little_endian)?;
    let length = width_of(body, at + 9, 5, little_endian)?;
    let pitch = *body.get(at + 8)?;
    let velocity = *body.get(at + 17)?;
    if pitch > 127 {
        return None;
    }
    Some(MidiNote { tick: pos.saturating_sub(origin), length, pitch, velocity })
}

fn extra_lanes(mirrors: &[(u8, String)], doc: &mut Document) {
    for (kind, name) in mirrors {
        if doc.lanes.iter().any(|l| l.name == *name) {
            continue;
        }
        if *kind == 0x0b {
            doc.lanes.push(Lane { name: name.clone(), role: Role::Folder, channels: 1 });
        } else if *kind != 0x00 {
            doc.notes.push(format!("track kind byte {kind:#04x} on {name} is not a documented audio or folder tag; imported as MIDI"));
            doc.lanes.push(Lane { name: name.clone(), role: Role::Midi, channels: 1 });
        }
    }
}

fn find(nodes: &[Node], code: u16) -> Vec<&Node> {
    let mut out = Vec::new();
    walk_code(nodes, code, &mut out);
    out
}

fn walk_code<'a>(nodes: &'a [Node], code: u16, out: &mut Vec<&'a Node>) {
    for node in nodes {
        if node.code == code {
            out.push(node);
        }
        walk_code(&node.children, code, out);
    }
}

fn text(body: &[u8], at: usize, little_endian: bool, allow_empty: bool) -> Option<(String, usize)> {
    let len = tree::u32_at(body, at, little_endian)? as usize;
    if len > 16 * 1024 {
        return None;
    }
    let start = at.checked_add(4)?;
    let end = start.checked_add(len)?;
    let bytes = body.get(start..end)?;
    if bytes.contains(&0) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if text.chars().any(|c| c.is_control() && c != '\t') {
        return None;
    }
    if text.is_empty() && !allow_empty {
        return None;
    }
    Some((text.to_string(), end))
}

fn width_le(body: &[u8], at: usize, width: u8) -> Option<u64> {
    width_of(body, at, width, true)
}

fn width_of(body: &[u8], at: usize, width: u8, little_endian: bool) -> Option<u64> {
    match width {
        0 => Some(0),
        1 => body.get(at).copied().map(u64::from),
        2 => (if little_endian { u16_le(body, at) } else { tree::u16_at(body, at, false) }).map(u64::from),
        3 => u24(body, at, little_endian).map(u64::from),
        4 => tree::u32_at(body, at, little_endian).map(u64::from),
        5 => u40(body, at, little_endian),
        _ => None,
    }
}

fn u16_le(body: &[u8], at: usize) -> Option<u16> {
    tree::u16_at(body, at, true)
}

fn u24_le(body: &[u8], at: usize) -> Option<u32> {
    u24(body, at, true)
}

fn u24(body: &[u8], at: usize, little_endian: bool) -> Option<u32> {
    let b = body.get(at..at + 3)?;
    Some(if little_endian {
        u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16
    } else {
        u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2])
    })
}

fn u32_le(body: &[u8], at: usize) -> Option<u32> {
    tree::u32_at(body, at, true)
}

fn u40(body: &[u8], at: usize, little_endian: bool) -> Option<u64> {
    let b = body.get(at..at + 5)?;
    Some(if little_endian {
        u64::from(b[0]) | u64::from(b[1]) << 8 | u64::from(b[2]) << 16 | u64::from(b[3]) << 24 | u64::from(b[4]) << 32
    } else {
        u64::from(b[0]) << 32 | u64::from(b[1]) << 24 | u64::from(b[2]) << 16 | u64::from(b[3]) << 8 | u64::from(b[4])
    })
}

fn i64_at(body: &[u8], at: usize, little_endian: bool) -> Option<i64> {
    let bytes = body.get(at..at + 8)?;
    let raw = [bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]];
    Some(if little_endian { i64::from_le_bytes(raw) } else { i64::from_be_bytes(raw) })
}
