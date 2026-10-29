//! AAF and DAWproject import/export. The synthetic AAF fixture is the one from
//! storytold/soundcraft pull request 799 (Vic Herrera): two tracks, three edits, chained offsets.
use serde_json::{Value, json};
use soundcraft_audio_io::{AudioBuffer, BitDepth, EncodeOptions, FileFormat};
use soundcraft_engine::{Engine, file_access};
use soundcraft_model::{AutoParam, ChannelFormat, FadeShape, MarkerKind, Session, TrackKind};
use soundcraft_time::SampleRate;
use std::sync::atomic::{AtomicU64, Ordering};

struct Dir {
    path: std::path::PathBuf,
}
impl Dir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("soundcraft-ix-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&path).unwrap();
        Self { path }
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn wav(samples: usize, spike: usize) -> Vec<u8> {
    let mut ch = vec![0.0; samples];
    if let Some(s) = ch.get_mut(spike) {
        *s = 0.5;
    }
    soundcraft_audio_io::encode(
        &AudioBuffer { sample_rate: 48000, channels: vec![ch] },
        &EncodeOptions { format: FileFormat::Wav, bit_depth: BitDepth::Int16, dither: false, bwf: None },
    )
    .unwrap()
}

fn fixture_plan(dir: &Dir) -> Value {
    let path = dir.path.join("Source.wav");
    std::fs::write(&path, wav(480_000, 96_000)).unwrap();
    json!({"plan":{"schema_version":1,"name":"AAF test","sample_rate":48000,"timecode_start":[3600,1],"frame_rate":{"rate":[25,1],"drop":false},"warnings":["Test warning"],
        "sources":[{"key":"source","name":"Original source","path":path,"channels":1,"frames":480000,"sample_rate":48000}],
        "tracks":[{"name":"DX","channels":1,"clips":[{"name":"Cut","source":"source","start":[1,1],"end":[3,1],"offset":[2,1]}]},
                  {"name":"Empty","channels":1,"clips":[]}]},"selected":[0],"mapping":"absolute","offset_samples":0})
}

fn existing() -> Engine {
    let mut s = Session::new("Existing", SampleRate::HZ_48000);
    s.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Keep"));
    Engine::new(s)
}

#[test]
fn absolute_mapping_preserves_source_window_and_is_one_undo_step() {
    let dir = Dir::new();
    let params = fixture_plan(&dir);
    let mut e = existing();
    let before = e.session().to_json().unwrap();
    let report = e.execute("file.import_aaf", &params).unwrap();
    assert_eq!(report["tracks"].as_array().unwrap().len(), 1);
    assert_eq!(e.session().tracks.len(), 2);
    let c = &e.session().tracks[1].clips()[0];
    assert_eq!((c.start, c.length, c.source_offset()), (172848000, 96000, 96000));
    assert!(e.session().sources[0].unsaved);
    let source = c.source().unwrap();
    assert_eq!(e.session().pool.get(source).unwrap().channel(0).unwrap()[96000], 0.5);
    assert_eq!(e.undo_label(), Some("Session Data..."));
    e.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(e.session().to_json().unwrap(), before);
}

#[test]
fn relative_mapping_does_not_replace_existing_tracks() {
    let dir = Dir::new();
    let mut params = fixture_plan(&dir);
    params["mapping"] = json!("relative");
    params["offset_samples"] = json!(96000);
    let mut e = existing();
    e.execute("file.import_aaf", &params).unwrap();
    assert_eq!(e.session().tracks[0].name, "Keep");
    assert_eq!(e.session().tracks[1].clips()[0].start, 144000);
}

#[test]
fn invalid_inputs_fail_without_mutation() {
    let dir = Dir::new();
    let params = fixture_plan(&dir);
    let mut e = existing();
    let before = e.session().to_json().unwrap();
    for update in [
        json!({"selected": []}),
        json!({"selected": [99]}),
        json!({"selected": [0, 0]}),
        json!({"mapping": "unknown"}),
        json!({"offset_samples": -172900000}),
    ] {
        let mut p = params.clone();
        for (k, v) in update.as_object().unwrap() {
            p[k] = v.clone();
        }
        assert!(e.execute("file.import_aaf", &p).is_err(), "{update}");
        assert_eq!(e.session().to_json().unwrap(), before);
    }
}

#[test]
fn sample_rate_and_confined_media_are_rejected() {
    let dir = Dir::new();
    let params = fixture_plan(&dir);
    let mut s = Session::new("44.1", SampleRate::HZ_44100);
    s.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Existing"));
    let mut e = Engine::new(s);
    assert!(e.execute("file.import_aaf", &params).unwrap_err().to_string().contains("sample rates differ"));
    let restricted = dir.path.join("allowed");
    std::fs::create_dir(&restricted).unwrap();
    let roots = file_access::AutomationRoots::new(&[restricted.as_path()], &[]).unwrap();
    let mut e = existing();
    assert!(file_access::confine(roots.as_ref(), || e.execute("file.import_aaf", &params)).is_err());
    assert_eq!(e.session().tracks.len(), 1);
}

#[test]
fn playing_import_cannot_bypass_the_transport_guard() {
    let dir = Dir::new();
    let mut params = fixture_plan(&dir);
    params["tracks"] = json!([1]);
    let mut e = existing();
    e.transport.playing = true;
    assert!(e.execute("file.import_aaf", &params).is_err());
    assert_eq!(e.session().tracks.len(), 1);
}

struct AafFolder {
    dir: Dir,
}
impl AafFolder {
    fn new() -> Self {
        let dir = Dir::new();
        std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/synthetic-two-tracks.aaf"), dir.path.join("Montaje.aaf")).unwrap();
        for (name, channels) in [("Diálogo á #1.wav", 1), ("Ambiente estéreo.wav", 2)] {
            let audio = AudioBuffer { sample_rate: 48000, channels: vec![vec![0.25; 384000]; channels] };
            let options = EncodeOptions { format: FileFormat::Wav, bit_depth: BitDepth::Int16, dither: false, bwf: None };
            std::fs::write(dir.path.join(name), soundcraft_audio_io::encode(&audio, &options).unwrap()).unwrap();
        }
        Self { dir }
    }
}

#[test]
fn synthetic_aaf_becomes_editable_tracks() {
    let f = AafFolder::new();
    let aaf = f.dir.path.join("Montaje.aaf");
    let preview = Engine::default().execute("file.preview_aaf", &json!({"path": aaf, "media_dir": f.dir.path})).unwrap();
    let plan = &preview["plan"];
    assert_eq!(plan["name"], "Clase AAF — cortes y handles");
    assert_eq!(plan["timecode_start"], json!([3600, 1]));
    assert_eq!(plan["frame_rate"], json!({"rate": [25, 1], "drop": false}));
    let names: Vec<_> = plan["tracks"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["01 Diálogo", "02 Ambiente estéreo"]);
    assert_eq!(plan["tracks"][0]["clips"][1]["offset"], json!([7, 2]));
    let mut e = Engine::default();
    e.execute("file.import_aaf", &json!({"plan": plan, "selected": [0, 1]})).unwrap();
    let edits: Vec<_> = e.session().tracks.iter().flat_map(|t| t.clips()).map(|c| (c.start, c.length, c.source_offset())).collect();
    assert_eq!(edits, [(48000, 96000, 48000), (192000, 48000, 168000), (0, 240000, 72000)]);
    assert_eq!(e.session().tracks[1].channels(), 2);
}

#[test]
fn missing_media_is_reported() {
    let f = AafFolder::new();
    std::fs::remove_file(f.dir.path.join("Diálogo á #1.wav")).unwrap();
    let aaf = f.dir.path.join("Montaje.aaf");
    let err = Engine::default().execute("file.preview_aaf", &json!({"path": aaf, "media_dir": f.dir.path})).unwrap_err().to_string();
    assert!(err.contains("Missing media") || err.contains("missing"), "{err}");
}

#[test]
fn aaf_and_dawproject_round_trip_fades_gain_automation_and_markers() {
    let dir = Dir::new();
    let mut session = Session::new("Song", SampleRate::HZ_48000);
    let src = soundcraft_engine::io::add_source(
        &mut session,
        "Kick",
        AudioBuffer { sample_rate: 48000, channels: vec![vec![0.5; 48000]] },
        None,
        FileFormat::Wav,
    );
    let track = session.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Kick"));
    let mut clip = soundcraft_model::Clip::audio(session.new_clip_id(), "Hit", src, 1000, 4800, 12000);
    clip.gain_db = -3.0;
    clip.fade_in = soundcraft_model::Fade { len: 200, shape: FadeShape::Linear, curvature: 0.0 };
    clip.fade_out = soundcraft_model::Fade { len: 400, shape: FadeShape::EqualPower, curvature: 0.0 };
    session.track_mut(track).unwrap().playlist_mut().unwrap().clips.push(clip);
    session.track_mut(track).unwrap().lane_mut(&AutoParam::Volume).set_point(0, -6.0);
    session.track_mut(track).unwrap().lane_mut(&AutoParam::Pan(0)).set_point(4800, 0.5);
    session.add_marker("Verse", MarkerKind::Marker, 2400, 2400);
    let mut e = Engine::new(session);

    for (name, embed) in [("out.aaf", true), ("out.dawproject", true), ("link.aaf", false)] {
        let path = dir.path.join(name);
        e.execute("file.export_aaf", &json!({"path": path, "embed": embed, "tracks": ["Kick"]})).unwrap();
        let mut into = Engine::default();
        let media = if embed { json!(null) } else { json!(dir.path) };
        let mut params = json!({"path": path, "mapping": "relative"});
        if !embed {
            params["media_dir"] = media;
        }
        into.execute("file.import_aaf", &params).unwrap();
        let clip = &into.session().tracks[0].clips()[0];
        assert_eq!((clip.start, clip.length, clip.source_offset()), (4800, 12000, 1000), "{name}");
        assert!((clip.gain_db - -3.0).abs() < 0.02, "{name} gain {}", clip.gain_db);
        assert_eq!(clip.fade_in.len, 200, "{name}");
        assert_eq!(clip.fade_in.shape, FadeShape::Linear, "{name}");
        assert_eq!(clip.fade_out.len, 400, "{name}");
        let vol = into.session().tracks[0].lane(&AutoParam::Volume).unwrap().value_at(0, 99.0);
        assert!((vol - -6.0).abs() < 0.02, "{name} volume {vol}");
        let pan = into.session().tracks[0].lane(&AutoParam::Pan(0)).unwrap().value_at(4800, 99.0);
        assert!((pan - 0.5).abs() < 0.02, "{name} pan {pan}");
        assert_eq!(into.session().markers[0].name, "Verse", "{name}");
        let decoded = into.session().pool.get(clip.source().unwrap()).unwrap().channel(0).unwrap()[1000];
        assert!((decoded - 0.5).abs() < 0.01, "{name} sample {decoded}");
    }
}

#[test]
fn ptx_and_omf_are_refused_with_a_reason() {
    let dir = Dir::new();
    for name in ["song.ptx", "song.omf"] {
        let path = dir.path.join(name);
        std::fs::write(&path, b"nope").unwrap();
        let err = Engine::default().execute("file.import_aaf", &json!({"path": path})).unwrap_err().to_string();
        assert!(err.contains("not") || err.contains("LGPL") || err.contains("not implemented") || err.contains("not read"), "{name}: {err}");
    }
}
