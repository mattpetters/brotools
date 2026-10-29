# Pro Tools session container (`.ptx`, `.ptxt`, `.ptf`, `.pts`)

This note is the specification the SoundCraft reader implements. It was written after
studying other people's work, then closed. The Rust crate `soundcraft-ptx` was written
from this note alone: none of that code, and none of its type names, function names, or
comments, was copied. Numeric record codes and field offsets are facts about the bytes,
so they appear here as numbers.

## Provenance

Studied, not linked, not translated, not committed:

| Source | Licence | What it was used for |
|---|---|---|
| [zamaudio/ptformat](https://github.com/zamaudio/ptformat) by Damien Zammit and Robin Gareus (2015–2019) | LGPL-2.1-or-later | The mask, the record envelope, and which record codes hold media names, regions, track names, and MIDI notes for Pro Tools 5–12. Their own table stops at version 12. |
| The same library as bundled by Ardour (`libs/ptformat`, still LGPL) | LGPL-2.1-or-later | A second copy of that reader. It does not describe a newer mask. |
| [sebedard-creator/pt_api](https://github.com/sebedard-creator/pt_api) `pt_format_specs.md` (the text current in October 2026; the project states its sources are LGPL-2.1-or-later) | LGPL-2.1-or-later | Later little-endian layouts: playlist map, clip geometry, memory locations, live I/O path names, the picture-rate code, and which payloads must stay flat. The project's code was not imported. |
| Tyler Eveland, "logic2ableton" (public write-up) | the page does not grant a code licence; only the published byte-level description was read | Confirms the mask and that MIDI notes are 35-byte records at 960,000 ticks per quarter note. |

No redistributable Pro Tools session was found under a licence that allows committing it.
Ardour and ptformat ship no sample sessions we could relicense. Real `.ptx` files are
somebody's session. Fixtures in this repository are synthesised by
`soundcraft_ptx::template_bytes` and by the unit tests. They are not Pro Tools output.

## What a file is

| Extension | Era | Container |
|---|---|---|
| `.pts` | Pro Tools 5 | Same envelope as `.ptf`. Accepted when the header matches. |
| `.ptf` | Pro Tools 7–9 (the 2019 reader also groups 5 and 6 here) | Mask scheme byte `0x01`. Often big-endian. |
| `.ptx` | Pro Tools 10 and later, including the 2024-era files the later write-up was checked against | Mask scheme byte `0x05`. Payloads below are little-endian. |
| `.ptxt` | Session template | The same bytes as `.ptx`. The extension is only a hint that the timeline may be empty. |

A file whose scheme byte is neither `0x01` nor `0x05` is rejected. Nothing in the studied
sources describes a third scheme, including for the versions those sources actually open.
A future Pro Tools that changes the scheme will not be misread as an empty session.

The 2019 reader refuses a version word above 12. This reader does not. The scheme byte
decides whether the mask is understood; the version word is reported and does not by
itself reject the file. Records this note does not describe stay uninterpreted.

## Clear header

The first 20 bytes are never masked.

| Offset | Size | Meaning |
|---|---|---|
| 0 | 1 | `0x03`, or the file is still recognised when bytes 1..17 are the 16 ASCII characters `0010111100101011`. |
| 1 | 16 | Those 16 characters on every file this note accepts via the second rule. When byte 0 is `0x03` they are still expected on real files but are not re-checked. |
| 17 | 1 | `0x00` little-endian, `0x01` big-endian. Any other value is rejected. This flag applies to record headers and to the fields marked "session endian" below. |
| 18 | 1 | Mask scheme: `0x01` or `0x05`. |
| 19 | 1 | Seed byte used to recover the mask stride. |

## Mask

The mask is 256 bytes. It is applied to every byte from offset 20 onward. Applying it
twice returns the original file, so reading and writing the container is the same step.

Recover the stride `s` by scanning `i` from 0 through 255:

- Scheme `0x01`: `(i × 53) mod 256 = seed`. The stride is `i`. The mask index for file offset `n` is `n mod 256`. This is the Pro Tools 5–9 scheme.
- Scheme `0x05`: `(i × 11) mod 256 = seed`. The stride is the 8-bit negation of `i`. The mask index is `(n / 4096) mod 256`. This is the Pro Tools 10+ scheme.

Both multipliers are coprime to 256, so every seed has one stride. Byte `k` of the mask
is `(k × s) mod 256`. Byte 0 of the mask is therefore always 0. Under scheme `0x05` the
whole first 4096 bytes of the file are unchanged by the mask, header included.

A file shorter than 20 bytes, or longer than 64 MiB, is rejected before the mask runs.

## Records

After the header the body is a sequence of records. A record begins with the byte `0x5A`.

```
5A | class u16 | size u32 | code u16 | payload
```

`class`, `size`, and `code` use the session endian. `size` counts the code and the
payload, not the seven bytes in front of the code. The total on disk is `size + 7`.
`size` of 0 is an empty record (no code). `size` of 1 is impossible and is not a record.
`class` must be 0..=255; a high byte other than zero means this `0x5A` is payload, not a
header.

The first record of a real file sits at offset 20 and is a pointer, not a coded record:
class 1, size 4, and four bytes of pointer, with no code. This reader treats that shape
as a pointer only when it is at offset 20. The pointer is not followed; the records are
already in the body.

A record's children are further `0x5A` records inside its payload. Scanning starts at the
first byte of the payload, except:

- Codes `0x1004`, `0x100b`, `0x1015`, `0x1018`, `0x1054`, `0x2030`, `0x2603`, and `0x262a` begin with a raw 32-bit count. Those four bytes are not scanned, so a count whose first byte is `0x5A` cannot be mistaken for a child.
- Code `0x1052` begins with a counted name (see below) and then a 32-bit event count. Scanning starts after that count.

Some payloads are fixed numbers that can accidentally contain `0x5A`. Those codes are
never scanned for children; the payload is kept whole:

`0x0002`, `0x1001`, `0x1014`, `0x1017`, `0x1028`, `0x103a`, `0x104f`, `0x2000`, `0x204d`,
`0x2077`, `0x251a`, `0x2602`, `0x260a`, `0x260e`, `0x2628`, `0x262f`, `0x2637`.

Nesting deeper than 32, or more than 100,000 records, is an error. A header whose `size`
runs past its parent is skipped and the scan resumes at the next byte. A truncated or
random body therefore cannot force an unbounded allocation: the size has to fit in the
bytes already in hand.

### Counted text

A counted string is a session-endian `u32` byte length followed by that many bytes. The
length must fit in the payload and must be at most 16,384. The bytes must be strict
UTF-8, must not contain a NUL, and must not contain a control character other than tab.
Callers that need a name also reject the empty string. Modern little-endian payloads
described by the 2026 write-up store the length little-endian even when this note says
"session endian" for the older fields; where the two disagree, the field says which.

## Record catalogue

Codes are dispatch keys. The class byte is historical and is not used to decide the
meaning. "Read" is what this implementation takes. Anything else in the payload is kept
only long enough to find the fields below, then dropped.

| Code | Role | What is read |
|---|---|---|
| `0x0001` shape | Opening pointer | Ignored. |
| `0x0002` | Trailing pointer table | Not interpreted. |
| `0x1001` | One media file's length | 64-bit length at payload offset 6, session endian. |
| `0x1003` | Wrapper around `0x1001` | Children only. |
| `0x1004` | Media list | Count at payload offset 0. Children `0x103a` and `0x1003`. |
| `0x1008` | Audio region, older sessions | Name and three widths, below. |
| `0x100b` | Region list, older sessions | Children `0x1008`. |
| `0x1014` | Audio track descriptor | Name and channel count. |
| `0x1015` | Audio track list | Children `0x1014`. |
| `0x1017` | Plug-in entry | Counted strings in the payload, as names only. |
| `0x1018` | Plug-in list | Children `0x1017`, plus the same string scan. |
| `0x1028` | Session sample rate | `u32` at payload offset 2, session endian. |
| `0x103a` | Media name | Names beginning at payload offset 9. |
| `0x104f` | One timeline event | Mute, region index, start sample, fade-or-audio tag. |
| `0x1050` | Event wrapper | Child `0x104f`. |
| `0x1052` | One playlist | Name, then child events. |
| `0x1054` | Playlist map | Child playlists, in order. This is the track order. |
| `0x2000` | MIDI notes | `MdNLB` chunks, below. |
| `0x2030` | Memory-location ruler, when it holds `0x2077` children | Those children. |
| `0x204d` | Picture rate | `u32` little-endian at payload offset 40. |
| `0x2067` | Session path / version | Version word at payload offset 18, session endian. |
| `0x2077` | One memory location | Index, name, time. |
| `0x2519` | All-track name list | Children `0x251a`. |
| `0x251a` | One track-name mirror | Kind byte and name. |
| `0x2602` | One live I/O path | Name and a channel count when it is 1 or 2. |
| `0x2603` | I/O path catalogue | Children `0x2602`. |
| `0x260e` | A track's main output | Output name, when the fixed marker bytes match. |
| `0x2628` | Clip name and geometry | The flag table below. |
| `0x2629` | Audio clip definition | Child `0x2628`, else the older three-width layout. |
| `0x262a` | Clip-definition list | Children `0x2629`, in index order. |

The 2019 reader also names codes for compound regions, MIDI region maps, and an I/O
channel table (`0x1021`, `0x1022`, `0x1056`–`0x1058`, `0x2523`, `0x262c`, `0x2633`,
`0x2634`). Those payloads are not given field offsets in either source that are stable
enough to restore sends, clip groups, or MIDI region placement. They are counted in the
fidelity report and otherwise skipped.

## Fields

### Sample rate, version, picture rate

- `0x1028` payload offset 2: sample rate, session-endian `u32`. Accepted when it is 8,000..=768,000. The first such record wins.
- `0x2067` payload offset 18: session-endian `u32`. The reported version is that value plus 2, and only when the result is 5..=40. That is the scale the 2019 reader used for Pro Tools 5–12. A word outside that range is ignored rather than treated as a major version.
- `0x204d` payload offset 40: little-endian `u32` picture-rate code, when the payload is at least 44 bytes. The mapping used by the session importer:

| Code | Rate |
|---|---|
| `0x01` | 24 |
| `0x02` | 25 |
| `0x03` | 29.97 |
| `0x05` | 29.97 drop |
| `0x07` | 30 |
| `0x08` | 30 drop (imported as 30; this session model has no separate 30-drop rate) |
| `0x09` | 23.976 |
| `0x0A` | 50 |
| `0x0B` | 59.94 |
| `0x0C` | 59.94 drop |
| `0x0D` | 60 |
| `0x0E` | 60 drop (imported as 60) |
| `0x0F` | 47.952 |
| `0x10` | 48 |
| `0x11` | 100 |
| `0x12` | 119.88 |
| `0x13` | 119.88 drop (imported as 119.88) |
| `0x14` | 120 |
| `0x15` | 120 drop (imported as 120) |

### Media names (`0x103a`)

Names start at payload offset 9. Each name is a counted string in the session endian.
The next name starts 9 bytes after that string: a 4-byte type tag and 5 further bytes.
The tag is the ASCII `WAVE`, `EVAW`, `AIFF`, or `FFIA`. On a version word of 10 or
higher, or when no version word was found, a tag whose first byte is 0 is also accepted
when the name ends in `.wav`, `.aif`, or `.aiff`.

An entry is a label, not media, when its whole text is `Audio Files` or `Fade Files`, or
when the name ends in `.grp`. A path that merely contains an `Audio Files` directory is
kept. The 2019 reader dropped any name that contained those words; that drops real paths,
so this reader does not follow it.

Lengths come from `0x1001` records under the same `0x1004`, in order. Payload offset 6 is
a session-endian `u64` frame count. If there are more names than lengths, the extra names
have an unknown length.

### Audio tracks

`0x1014` payload: a counted name at offset 0, one opaque byte, then a session-endian
`u32` channel count. A count outside 1..=16 is stored as 1 and reported. One descriptor
is one track, not one track per channel.

`0x1052` payload: a counted name at offset 0 (empty length means the playlist has no
name stored here), then a session-endian `u32` event count, then `0x1050` children.
When any `0x1054` playlist map exists, that order is the audio-track order. Channel
counts are taken from the `0x1014` descriptor with the same name. An empty playlist name
is filled from the next `0x251a` mirror whose kind byte is `0x00`, and if none remains
it becomes `Audio N`.

When no playlist map exists, the `0x1014` list is the audio-track order.

### Track-name mirrors (`0x251a`)

Payload byte 0 is a kind tag. The counted name starts at payload offset 2.

| Byte | Meaning used here |
|---|---|
| `0x00` | Audio. Used to name an otherwise empty playlist. Not a second copy of the track. |
| `0x0b` | A basic folder, from the layout observed on a 2024-era session. Imported as a folder track. Not a routing folder, and not nested. |
| anything else | Not specified by the sources. If the name is not already an audio track or a folder, it is imported as a MIDI track and the fidelity report says the kind byte was not a documented audio or folder tag. Aux, master, and instrument tracks are not distinguished. |

### Timeline events (`0x104f`)

Payload, little-endian:

| Offset | Size | Meaning |
|---|---|---|
| 0 | 1 | `0x01` muted, otherwise audible. |
| 2 | 4 | Index into the clip-definition list (`0x262a` / `0x2629` order). |
| 7 | 8 | Start, in samples. |
| 15 | 1 | `0x03` audio, `0x01` fade. Fades are counted and dropped. |

On a big-endian session, if byte 15 is not `0x01` or `0x03`, the index and the start are
read again as a session-endian `u32` at offset 2 and a session-endian `u32` at offset 7.
That is the width the 2019 reader used for the older placement record.

The event's playlist index is the index of its `0x1052` among the playlist map's children.

### Clip geometry (`0x2628`)

Payload starts with a little-endian counted name. Let `A` be the offset just past that
name. A little-endian `u16` at `A` is the flag. The byte at `A+2` selects the width of
the length: `0x10` one byte, `0x20` two, `0x30` three, `0x40` four.

| Flag | Source offset | Length starts |
|---|---|---|
| `0x0000`, `0x0001` | none (offset 0) | `A+5` |
| `0x1001` | one byte at `A+5` | `A+6` |
| `0x2000`, `0x2001` | `u16` at `A+5` | `A+7` |
| `0x3000`, `0x3001` | `u24` at `A+5` | `A+8` |
| `0x4001` | `u32` at `A+5` | `A+9` |

All of those integers are little-endian. A definition whose first five bytes at `A` are
`00 30 22 44 08` is the sync-point form: flag `0x3000`, source offset `u24` at `A+5`,
sync offset `u16` at `A+8`, audible length `u16` at `A+10`. The timeline timestamp is the
sync anchor; the clip start is the anchor minus the sync offset.

If a `0x2629` or `0x1008` has no `0x2628` child that matches this table, the older
three-width record is tried. The counted name starts at payload offset 9. Immediately
after the string, five bytes give the widths, then the integers are packed little-endian
regardless of the session endian:

- Big-endian session: high nibbles of bytes +4, +3, +2 are the widths of the source offset, the length, and the anchor.
- Little-endian session: high nibbles of bytes +1, +2, +3 are those three widths.

Each width is 0..=5 bytes (0 means the value is 0 and consumes nothing). The three
integers follow in that order: source offset, length, anchor. A 32-bit session-endian
media index may sit immediately after the first child record; when it is in range it
selects the media name.

### Memory locations (`0x2077`)

Little-endian, falling back to session endian when the little-endian name length does
not fit:

| Offset | Size | Meaning |
|---|---|---|
| 0 | 2 | Index. |
| 2 | 4 | `03 09 00 00` on records written by the later tool. Not required. |
| 6 | 4 | Name length. |
| 10 | N | Name. |
| 10+N | 8 | Signed start, in samples. |
| 18+N | 8 | Signed end. Equal to the start for a point. A greater end is a selection. |

A negative start is dropped. A comment is taken only when the payload length is exactly
`136 + N + C`, the `u32` at offset `86 + N` equals `C`, and the `C` bytes at `90 + N`
are text. Any other shape has no comment. The surrounding `0x2506` / `0x4826` / `0x4827`
records are not interpreted.

### Live I/O paths (`0x2602`)

A counted name at payload offset 2. The session-endian `u32` immediately after the name
is a channel count when it is 1 or 2; otherwise the width is unknown and the importer
uses stereo only as a stand-in, and says so. These names become mix busses and output
paths. The sources do not describe a general hardware-input record, so inputs are not
restored.

### Per-track output (`0x260e`)

Accepted only when payload bytes 8..12 are `2A 00 00 00`. The little-endian `u32` at
offset 36 is the name length; the name follows at offset 40. Descriptors are assigned to
audio tracks in playlist order. A count that does not match the track count is reported;
the common prefix is still assigned.

### Plug-in names (`0x1017`)

Neither source defines this payload. The code is only named "plug-in entry" by the 2019
catalogue. This reader walks the payload for counted strings (session endian, length
2..=80, containing a letter) and keeps up to 32 of them. They are names found in that
record, not manufacturer ids, and they are not tied to a slot. Parameters and bypass are
not present in any layout this note trusts, so they are not invented. The importer places
the names on insert slots of the audio tracks in list order and says that the placement
is approximate.

### MIDI notes (`0x2000`)

Search the payload for the five ASCII bytes `MdNLB`. Eleven bytes after the `M` is a
session-endian `u32` note count. The following bytes are 35-byte notes, and the first
note's position word is also the origin subtracted from every note in the chunk (the
2019 reader reads the origin and the first note from the same address). Each note:

| Offset | Size | Meaning |
|---|---|---|
| 0 | 5 | Absolute tick, session endian. |
| 8 | 1 | Pitch. |
| 9 | 5 | Duration in ticks. |
| 17 | 1 | Velocity. |

Ticks are Pro Tools' 960,000 per quarter note. The importer divides by 1,000 to reach
the session's 960 ticks per quarter, then converts a clip length with the restored tempo.
Tempo is not in this container's decoded records, so that conversion uses 120 BPM and
the report says so. Notes are placed on the MIDI tracks in chunk order, one clip per
chunk, starting at sample 0. The older MIDI region-to-track map is not restored; the
report says the clip was not given a timeline position from the session.

## What is not restored

These are absent from the layouts above, not silently defaulted without a report line:

- Tempo map and meter. The session stays at 120 BPM and 4/4.
- Session bit depth. Recording depth stays 24-bit. Per-file bit depth is not a field of `0x1001` in the sources.
- Track colors. Tracks take the default palette in order.
- Aux, master, instrument, and VCA tracks as distinct kinds.
- Routing folders and nested folders.
- Sends, hardware inputs, and plug-in parameters, bypass, and sidechains.
- Fade curves, crossfades, clip gain, and volume or pan automation.
- Elastic audio, clip groups, and video tracks.
- The on-disk location of a plug-in beyond the text found in `0x1017`.

Opening a file does not write it back. Saving from SoundCraft writes a SoundCraft
session, and only after a new path is chosen. The Pro Tools file is not replaced.

## Malformed input

The reader returns an error for a short file, a bad endian flag, an unknown scheme, a
file over 64 MiB, nesting past 32, or more than 100,000 records. It also returns an
error when the mask succeeds but no sample rate, track, marker, media name, or I/O path
was recognised, so a random body with a forged header does not become an empty session.

Inside a recognised file, a record that does not match the field rules is skipped and
listed. Strings that are not UTF-8 are skipped. Sample positions that do not fit in the
session's position range are skipped. No path in the file is opened by the parser; the
importer resolves media only through the session path check, and a missing or refused
file is a missing-media line, not a failed import.

## Session mapping

`soundcraft-ptx` stops at the document in this note. It does not depend on the session
model. The engine maps that document onto a session:

- Playlist order, then folders, then MIDI tracks.
- Clip definitions indexed by `0x104f`, with source offset, length, mute, and sync.
- Media names stored as source paths. Bytes are read only when the path check allows it.
- Memory locations as markers or selections.
- I/O path names as busses and output paths; `0x260e` names as the track output.
- Plug-in name strings as inserts whose id is `ptx:` plus the text.
- A fidelity list on `session.open` and `file.import_pro_tools`.

The audio subset (mono and stereo clips, markers, sample rate, media paths) is the same
information the interchange plan on `cursor/session-interchange-2205` carries in schema 1.
That plan has no columns for track kind, folder, I/O, plug-in names, or picture rate, so
those are applied on the session after the audio clips. When that crate is on this
branch, the audio subset can go through its session builder without changing this
document.
