//! Reader for Pro Tools sessions and templates (`.ptx`, `.ptxt`, `.ptf`, `.pts`).
//!
//! The byte layout is [`docs/formats/ptx.md`](https://github.com/storytold/soundcraft/blob/main/docs/formats/ptx.md)
//! in this repository. This crate stops at that document: it does not open files and it
//! does not build a session. The mask is symmetric, so [`reveal`] and [`conceal`] undo
//! each other, and [`compose`] builds a synthetic file the tests round-trip.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod mask;
mod read;
mod tree;

/// Largest file the reader will mask. A session larger than this is rejected.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// How the mask is indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keying {
    /// Scheme byte `0x01`: index is the file offset modulo 256. Pro Tools 5–9.
    PerByte,
    /// Scheme byte `0x05`: index is the file offset divided by 4096, modulo 256. Pro Tools 10 and later.
    PerPage,
}

/// Endian of record headers and of the fields the specification marks as session-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOrder {
    Little,
    Big,
}

/// What a lane is, as far as the decoded records say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Audio,
    Midi,
    Folder,
}

/// One track row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    pub name: String,
    pub role: Role,
    pub channels: u16,
}

/// A referenced audio file. `frames` is missing when the file record had no length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    pub name: String,
    pub frames: Option<u64>,
}

/// A clip definition. `sync` is the sync-point offset; zero when the record has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub name: String,
    pub offset: u64,
    pub length: u64,
    pub anchor: u64,
    pub sync: u64,
    pub media_index: Option<u32>,
}

/// A clip placed on an audio lane. `lane_index` indexes [`Document::lanes`] in playlist order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub lane_index: usize,
    pub region_index: u32,
    pub start: u64,
    pub muted: bool,
}

/// A memory location. `end == start` is a point; a later `end` is a selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub index: u16,
    pub name: String,
    pub start: i64,
    pub end: i64,
    pub comment: String,
}

/// A live I/O path. `channels` is 0 when the record did not say 1 or 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalPath {
    pub name: String,
    pub channels: u16,
}

/// One MIDI note. `tick` and `length` are Pro Tools ticks (960,000 per quarter), relative to the chunk origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiNote {
    pub tick: u64,
    pub length: u64,
    pub pitch: u8,
    pub velocity: u8,
}

/// Notes from one `MdNLB` chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiClip {
    pub notes: Vec<MidiNote>,
}

/// Everything the field rules recognise. `notes` are skips and approximations, not a log of success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub order: ByteOrder,
    pub keying: Keying,
    pub version: Option<u32>,
    pub sample_rate: Option<u32>,
    pub frame_rate_code: Option<u32>,
    pub lanes: Vec<Lane>,
    pub media: Vec<Media>,
    pub regions: Vec<Region>,
    pub placements: Vec<Placement>,
    pub markers: Vec<Marker>,
    pub paths: Vec<SignalPath>,
    pub outputs: Vec<String>,
    pub plugins: Vec<String>,
    pub midi: Vec<MidiClip>,
    pub notes: Vec<String>,
}

/// A header this crate can write. `seed` 11 with [`Heading::per_page`] makes a non-zero page mask.
#[derive(Debug, Clone, Copy)]
pub struct Heading {
    pub little_endian: bool,
    pub per_page: bool,
    pub seed: u8,
}

/// One record. `head` is the payload before children, `tail` is the payload after them.
#[derive(Debug, Clone)]
pub struct Piece {
    pub class: u8,
    pub code: u16,
    pub head: Vec<u8>,
    pub nested: Vec<Piece>,
    pub tail: Vec<u8>,
}

impl Piece {
    fn leaf(class: u8, code: u16, head: Vec<u8>) -> Self {
        Piece { class, code, head, nested: Vec::new(), tail: Vec::new() }
    }
}

/// Why a file was not read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Error(message.into())
    }
}

/// True when the last path component ends in `.ptx`, `.ptxt`, `.ptf`, or `.pts`.
pub fn is_pro_tools_path(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "ptx" | "ptxt" | "ptf" | "pts")
}

/// Unmask a session. The returned bytes still start with the clear header.
pub fn reveal(file: &[u8]) -> Result<Vec<u8>, Error> {
    mask::reveal(file).map(|opened| opened.bytes)
}

/// Mask a revealed session. The header must already describe the scheme.
pub fn conceal(plain: &[u8]) -> Result<Vec<u8>, Error> {
    mask::conceal(plain)
}

/// Read a session or template. The extension is not consulted; the header is.
pub fn parse(file: &[u8]) -> Result<Document, Error> {
    let opened = mask::reveal(file)?;
    let keying = opened.keying;
    read::document(&opened.bytes, opened.little_endian, keying)
}

/// Build a masked file from records. Used by the tests and by [`template_bytes`].
pub fn compose(heading: &Heading, pieces: &[Piece]) -> Result<Vec<u8>, Error> {
    let mut plain = header_bytes(heading).to_vec();
    plain.extend(pointer(heading.little_endian));
    for piece in pieces {
        encode(piece, heading.little_endian, &mut plain)?;
    }
    conceal(&plain)
}

/// A synthetic little-endian `.ptx`: two audio tracks, a folder, a MIDI track, one clip,
/// a marker, a bus, a plug-in name, and a MIDI chunk. Not a Pro Tools file.
pub fn template_bytes() -> Vec<u8> {
    compose(&Heading { little_endian: true, per_page: true, seed: 11 }, &template_pieces()).unwrap_or_default()
}

fn header_bytes(heading: &Heading) -> [u8; 20] {
    let mut bytes = [0u8; 20];
    bytes[0] = 0x03;
    bytes[1..17].copy_from_slice(b"0010111100101011");
    bytes[0x11] = if heading.little_endian { 0 } else { 1 };
    bytes[0x12] = if heading.per_page { 0x05 } else { 0x01 };
    bytes[0x13] = heading.seed;
    bytes
}

fn pointer(little_endian: bool) -> Vec<u8> {
    let mut out = vec![0x5a];
    push_u16(&mut out, 1, little_endian);
    push_u32(&mut out, 4, little_endian);
    push_u32(&mut out, 0, little_endian);
    out
}

fn encode(piece: &Piece, little_endian: bool, out: &mut Vec<u8>) -> Result<(), Error> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&piece.head);
    for child in &piece.nested {
        encode(child, little_endian, &mut payload)?;
    }
    payload.extend_from_slice(&piece.tail);
    let size = payload.len().checked_add(2).ok_or_else(|| Error::new("record too large"))?;
    let size = u32::try_from(size).map_err(|_| Error::new("record too large"))?;
    out.push(0x5a);
    push_u16(out, u16::from(piece.class), little_endian);
    push_u32(out, size, little_endian);
    push_u16(out, piece.code, little_endian);
    out.extend(payload);
    Ok(())
}

fn push_u16(out: &mut Vec<u8>, value: u16, little_endian: bool) {
    out.extend_from_slice(&if little_endian { value.to_le_bytes() } else { value.to_be_bytes() });
}

fn push_u32(out: &mut Vec<u8>, value: u32, little_endian: bool) {
    out.extend_from_slice(&if little_endian { value.to_le_bytes() } else { value.to_be_bytes() });
}

fn template_pieces() -> Vec<Piece> {
    let rate = {
        let mut body = vec![0u8; 6];
        body[2..6].copy_from_slice(&48_000u32.to_le_bytes());
        Piece::leaf(0x0a, 0x1028, body)
    };
    let version = {
        let mut body = vec![0u8; 22];
        body[18..22].copy_from_slice(&10u32.to_le_bytes());
        Piece::leaf(0x05, 0x2067, body)
    };
    let picture = {
        let mut body = vec![0u8; 44];
        body[40..44].copy_from_slice(&0x09u32.to_le_bytes());
        Piece::leaf(0x05, 0x204d, body)
    };
    let tracks = Piece {
        class: 0x03,
        code: 0x1015,
        head: 2u32.to_le_bytes().to_vec(),
        nested: vec![Piece::leaf(0x01, 0x1014, track_body("Kick", 1)), Piece::leaf(0x01, 0x1014, track_body("Room", 2))],
        tail: Vec::new(),
    };
    let kick = event(0, 48_000, 0x03);
    let fade = event(0, 0, 0x01);
    let map = Piece {
        class: 0x02,
        code: 0x1054,
        head: 2u32.to_le_bytes().to_vec(),
        nested: vec![playlist("Kick", vec![kick]), playlist("Room", vec![fade])],
        tail: Vec::new(),
    };
    let clips = Piece {
        class: 0x01,
        code: 0x262a,
        head: 1u32.to_le_bytes().to_vec(),
        nested: vec![Piece {
            class: 0x0b,
            code: 0x2629,
            head: Vec::new(),
            nested: vec![Piece::leaf(0x04, 0x2628, definition("Kick_01", 24_000))],
            tail: Vec::new(),
        }],
        tail: Vec::new(),
    };
    let files = Piece {
        class: 0x03,
        code: 0x1004,
        head: 1u32.to_le_bytes().to_vec(),
        nested: vec![
            Piece::leaf(0x01, 0x103a, media_body("Audio Files/Kick.wav")),
            Piece { class: 0x01, code: 0x1003, head: Vec::new(), nested: vec![Piece::leaf(0x01, 0x1001, length_body(24_000))], tail: Vec::new() },
        ],
        tail: Vec::new(),
    };
    let names = Piece {
        class: 0x01,
        code: 0x2519,
        head: Vec::new(),
        nested: vec![Piece::leaf(0x01, 0x251a, mirror(0x0b, "Drums")), Piece::leaf(0x01, 0x251a, mirror(0x02, "Keys"))],
        tail: Vec::new(),
    };
    let marks = Piece {
        class: 0x05,
        code: 0x2030,
        head: 1u32.to_le_bytes().to_vec(),
        nested: vec![Piece::leaf(0x12, 0x2077, marker_body(1, "Verse", 96_000))],
        tail: vec![0u8; 8],
    };
    let routes = Piece {
        class: 0x02,
        code: 0x2603,
        head: 1u32.to_le_bytes().to_vec(),
        nested: vec![Piece::leaf(0x0e, 0x2602, path_body("Reverb", 1))],
        tail: Vec::new(),
    };
    let output = Piece::leaf(0x09, 0x260e, output_body("Reverb"));
    let plugin = Piece::leaf(0x01, 0x1017, counted("Channel EQ"));
    let notes = Piece::leaf(0x01, 0x2000, midi_body());
    vec![rate, version, picture, tracks, map, clips, files, names, marks, routes, output, plugin, notes]
}

fn counted(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
    out
}

fn track_body(name: &str, channels: u32) -> Vec<u8> {
    let mut out = counted(name);
    out.push(0);
    out.extend_from_slice(&channels.to_le_bytes());
    out
}

fn playlist(name: &str, events: Vec<Piece>) -> Piece {
    let mut head = counted(name);
    head.extend_from_slice(&(events.len() as u32).to_le_bytes());
    Piece { class: 0x03, code: 0x1052, head, nested: events, tail: Vec::new() }
}

fn event(index: u32, start: u64, tag: u8) -> Piece {
    let mut body = vec![0u8; 16];
    body[2..6].copy_from_slice(&index.to_le_bytes());
    body[7..15].copy_from_slice(&start.to_le_bytes());
    body[15] = tag;
    let leaf = Piece::leaf(0x0a, 0x104f, body);
    Piece { class: 0x03, code: 0x1050, head: Vec::new(), nested: vec![leaf], tail: Vec::new() }
}

fn definition(name: &str, length: u32) -> Vec<u8> {
    let mut out = counted(name);
    out.extend_from_slice(&[0x01, 0x00, 0x40, 0x00, 0x00]);
    out.extend_from_slice(&length.to_le_bytes());
    out
}

fn media_body(name: &str) -> Vec<u8> {
    let mut out = vec![0u8; 9];
    out.extend(counted(name));
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(&[0u8; 5]);
    out
}

fn length_body(frames: u64) -> Vec<u8> {
    let mut out = vec![0u8; 14];
    out[6..14].copy_from_slice(&frames.to_le_bytes());
    out
}

fn mirror(kind: u8, name: &str) -> Vec<u8> {
    let mut out = vec![kind, 0];
    out.extend(counted(name));
    out
}

fn marker_body(index: u16, name: &str, at: i64) -> Vec<u8> {
    let mut out = vec![0u8; 6];
    out[0..2].copy_from_slice(&index.to_le_bytes());
    out[2..6].copy_from_slice(&[0x03, 0x09, 0x00, 0x00]);
    out.extend(counted(name));
    out.extend_from_slice(&at.to_le_bytes());
    out.extend_from_slice(&at.to_le_bytes());
    out
}

fn path_body(name: &str, channels: u32) -> Vec<u8> {
    let mut out = vec![0u8, 0u8];
    out.extend(counted(name));
    out.extend_from_slice(&channels.to_le_bytes());
    out
}

fn output_body(name: &str) -> Vec<u8> {
    let mut out = vec![0u8; 40];
    out[8] = 0x2a;
    out[36..40].copy_from_slice(&(name.len() as u32).to_le_bytes());
    out.extend_from_slice(name.as_bytes());
    out
}

fn midi_body() -> Vec<u8> {
    let mut out = vec![0u8; 11];
    out.extend_from_slice(b"MdNLB");
    out.extend_from_slice(&[0u8; 6]);
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend(note_record(960_000, 60, 480_000, 100));
    out.extend(note_record(1_920_000, 64, 960_000, 80));
    out
}

fn note_record(tick: u64, pitch: u8, length: u64, velocity: u8) -> Vec<u8> {
    let mut out = vec![0u8; 35];
    write_u40(&mut out, 0, tick);
    out[8] = pitch;
    write_u40(&mut out, 9, length);
    out[17] = velocity;
    out
}

fn write_u40(out: &mut [u8], at: usize, value: u64) {
    for (i, byte) in out.iter_mut().enumerate().take(at + 5).skip(at) {
        *byte = ((value >> ((i - at) * 8)) & 0xff) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert!(is_pro_tools_path("Song.ptx"));
        assert!(is_pro_tools_path(r"C:\T\Mix.PTXT"));
        assert!(is_pro_tools_path("/s/old.ptf"));
        assert!(is_pro_tools_path("five.pts"));
        assert!(!is_pro_tools_path("song.scraft"));
        assert!(!is_pro_tools_path("song.ptx.bak"));
    }

    #[test]
    fn synthetic_template_round_trips_the_fields() {
        let bytes = template_bytes();
        assert!(bytes.len() > 20);
        assert_eq!(reveal(&bytes).unwrap(), reveal(&conceal(&reveal(&bytes).unwrap()).unwrap()).unwrap());
        let doc = parse(&bytes).unwrap();
        assert_template_header(&doc);
        assert_template_timeline(&doc);
        assert_template_mix(&doc);
    }

    fn assert_template_header(doc: &Document) {
        assert_eq!(doc.sample_rate, Some(48_000));
        assert_eq!(doc.version, Some(12));
        assert_eq!(doc.frame_rate_code, Some(0x09));
        assert_eq!(doc.keying, Keying::PerPage);
        assert_eq!(
            doc.lanes.iter().map(|l| (l.name.as_str(), l.role, l.channels)).collect::<Vec<_>>(),
            vec![("Kick", Role::Audio, 1), ("Room", Role::Audio, 2), ("Drums", Role::Folder, 1), ("Keys", Role::Midi, 1),]
        );
    }

    fn assert_template_timeline(doc: &Document) {
        assert_eq!(doc.media.len(), 1);
        assert_eq!(doc.media[0].name, "Audio Files/Kick.wav");
        assert_eq!(doc.media[0].frames, Some(24_000));
        assert_eq!(doc.regions.len(), 1);
        assert_eq!(doc.regions[0].name, "Kick_01");
        assert_eq!((doc.regions[0].offset, doc.regions[0].length), (0, 24_000));
        assert_eq!(doc.placements.len(), 1);
        assert_eq!(doc.placements[0].lane_index, 0);
        assert_eq!(doc.placements[0].start, 48_000);
        assert!(!doc.placements[0].muted);
        assert!(doc.notes.iter().any(|n| n.contains("fade")));
    }

    fn assert_template_mix(doc: &Document) {
        assert_eq!(doc.markers.len(), 1);
        assert_eq!(doc.markers[0].name, "Verse");
        assert_eq!(doc.markers[0].start, 96_000);
        assert_eq!(doc.paths.len(), 1);
        assert_eq!(doc.paths[0].name, "Reverb");
        assert_eq!(doc.paths[0].channels, 1);
        assert_eq!(doc.outputs, vec!["Reverb".to_string()]);
        assert_eq!(doc.plugins, vec!["Channel EQ".to_string()]);
        assert_eq!(doc.midi.len(), 1);
        assert_eq!(doc.midi[0].notes.len(), 2);
        assert_eq!(doc.midi[0].notes[0].pitch, 60);
        assert_eq!(doc.midi[0].notes[0].tick, 0);
        assert_eq!(doc.midi[0].notes[0].length, 480_000);
        assert_eq!(doc.midi[0].notes[1].tick, 960_000);
        assert!(doc.notes.iter().any(|n| n.contains("Keys")));
    }

    #[test]
    fn three_width_region_and_big_endian_rate() {
        let mut widths = vec![0u8; 5];
        widths[1] = 0x20;
        widths[2] = 0x30;
        widths[3] = 0x40;
        let mut payload = vec![0u8; 9];
        payload.extend(counted("Hat"));
        payload.extend(widths);
        payload.extend_from_slice(&1000u16.to_le_bytes());
        payload.extend_from_slice(&50_000u32.to_le_bytes()[..3]);
        payload.extend_from_slice(&48_000u32.to_le_bytes());
        let region =
            Piece { class: 1, code: 0x100b, head: 1u32.to_le_bytes().to_vec(), nested: vec![Piece::leaf(1, 0x1008, payload)], tail: Vec::new() };
        let bytes = compose(&Heading { little_endian: true, per_page: true, seed: 11 }, &[region]).unwrap();
        let doc = parse(&bytes).unwrap();
        assert_eq!(doc.regions.len(), 1);
        assert_eq!(doc.regions[0].name, "Hat");
        assert_eq!(doc.regions[0].offset, 1000);
        assert_eq!(doc.regions[0].length, 50_000);
        assert_eq!(doc.regions[0].anchor, 48_000);

        let mut rate = vec![0u8; 6];
        rate[2..6].copy_from_slice(&44_100u32.to_be_bytes());
        let bytes = compose(&Heading { little_endian: false, per_page: false, seed: 0x2a }, &[Piece::leaf(0x0a, 0x1028, rate)]).unwrap();
        let doc = parse(&bytes).unwrap();
        assert_eq!(doc.order, ByteOrder::Big);
        assert_eq!(doc.keying, Keying::PerByte);
        assert_eq!(doc.sample_rate, Some(44_100));
    }

    #[test]
    fn sync_point_geometry() {
        let mut body = counted("Vox");
        body.extend_from_slice(&[0x00, 0x30, 0x22, 0x44, 0x08]);
        body.extend_from_slice(&256u32.to_le_bytes()[..3]);
        body.extend_from_slice(&10u16.to_le_bytes());
        body.extend_from_slice(&1000u16.to_le_bytes());
        let list = Piece {
            class: 1,
            code: 0x262a,
            head: 1u32.to_le_bytes().to_vec(),
            nested: vec![Piece { class: 1, code: 0x2629, head: Vec::new(), nested: vec![Piece::leaf(1, 0x2628, body)], tail: Vec::new() }],
            tail: Vec::new(),
        };
        let doc = parse(&compose(&Heading { little_endian: true, per_page: true, seed: 11 }, &[list]).unwrap()).unwrap();
        assert_eq!(doc.regions[0].offset, 0x100);
        assert_eq!(doc.regions[0].sync, 10);
        assert_eq!(doc.regions[0].length, 1000);
    }

    #[test]
    fn malformed_input_does_not_panic() {
        assert!(parse(&[]).is_err());
        assert!(parse(&[0u8; 19]).is_err());
        let mut header = header_bytes(&Heading { little_endian: true, per_page: true, seed: 11 }).to_vec();
        header.extend(std::iter::repeat_n(0x5a, 200));
        assert!(parse(&header).is_err());
        let mut state = 0x1234_5678u32;
        for _ in 0..40 {
            let mut junk = header_bytes(&Heading { little_endian: true, per_page: state & 1 == 0, seed: (state >> 8) as u8 }).to_vec();
            for _ in 0..128 {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                junk.push((state >> 16) as u8);
            }
            let _ = parse(&junk);
        }
        let mut deep = Piece::leaf(1, 0x1028, {
            let mut body = vec![0u8; 6];
            body[2..6].copy_from_slice(&48_000u32.to_le_bytes());
            body
        });
        for _ in 0..40 {
            deep = Piece { class: 1, code: 0x1050, head: Vec::new(), nested: vec![deep], tail: Vec::new() };
        }
        let bytes = compose(&Heading { little_endian: true, per_page: true, seed: 11 }, &[deep]).unwrap();
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn too_many_records_is_an_error() {
        let leaf = Piece::leaf(1, 0x1017, counted("No"));
        let pieces = vec![leaf; 100_001];
        let bytes = compose(&Heading { little_endian: true, per_page: true, seed: 11 }, &pieces).unwrap();
        assert!(parse(&bytes).unwrap_err().to_string().contains("100000"));
    }

    #[test]
    fn directory_label_is_not_media_but_a_path_through_it_is() {
        let files = Piece {
            class: 3,
            code: 0x1004,
            head: 2u32.to_le_bytes().to_vec(),
            nested: vec![Piece::leaf(1, 0x103a, {
                let mut body = media_body("Audio Files");
                let mut second = counted("Audio Files/Kick.wav");
                // media_body already ends at the next-name gap; append another name the same way.
                body.extend(counted_gap(&mut second));
                body
            })],
            tail: Vec::new(),
        };
        let doc = parse(&compose(&Heading { little_endian: true, per_page: true, seed: 11 }, &[files]).unwrap()).unwrap();
        assert_eq!(doc.media.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["Audio Files/Kick.wav"]);
    }

    fn counted_gap(name: &mut Vec<u8>) -> Vec<u8> {
        let mut out = std::mem::take(name);
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(&[0u8; 5]);
        out
    }
}
