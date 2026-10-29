//! File ▸ Import ▸ Session Data and File ▸ Export ▸ Selected Tracks as New AAF/OMF.
use super::{CommandSpec, always, bad, str_param};
use crate::cmd;
use crate::{Engine, Result};
use serde_json::{Value, json};
use soundcraft_interchange::Plan;
use soundcraft_model::TrackKind;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "file.preview_aaf",
            "Analyze AAF",
            [],
            None,
            "{path, media_dir, composition?} — read-only description of an AAF",
            always,
            preview
        ),
        cmd!(
            "file.import_aaf",
            "Session Data...",
            ["File", "Import"],
            None,
            "{path, media_dir?, plan?, selected?: [track indexes], mapping?: absolute|relative, offset_samples?: 0} — AAF or DAWproject into new tracks, one undo step",
            stopped,
            import
        ),
        cmd!(
            noundo "file.export_aaf",
            "Selected Tracks as New AAF/OMF...",
            ["File", "Export"],
            None,
            "{path, embed?: true, tracks?: [names]} — .aaf or .dawproject. OMF is not written. Selected audio tracks, or every audio track when nothing is selected.",
            has_audio,
            export
        ),
        cmd!("file.import_dawproject", "DAWproject...", [], None, "{path} — import a .dawproject archive", stopped, |e, p| import_path(e, p, true)),
        cmd!(
            noundo "file.export_dawproject",
            "DAWproject...",
            [],
            None,
            "{path, embed?: true, tracks?: [names]}",
            has_audio,
            |e, p| export(e, p)
        ),
    ]
}

fn stopped(e: &Engine) -> std::result::Result<(), String> {
    if e.transport.playing || e.transport.recording { Err("Stop playback and recording before importing session data".into()) } else { Ok(()) }
}

fn has_audio(e: &Engine) -> std::result::Result<(), String> {
    if e.session().tracks.iter().any(|t| t.kind == TrackKind::Audio) { Ok(()) } else { Err("the session has no audio tracks".into()) }
}

fn preview(e: &mut Engine, p: &Value) -> Result<Value> {
    let _ = e;
    let path = str_param(p, "path").ok_or_else(|| bad("file.preview_aaf", "`path` required"))?;
    let media = str_param(p, "media_dir").ok_or_else(|| bad("file.preview_aaf", "`media_dir` required"))?;
    let plan = crate::interchange::read_aaf_plan(path, media, str_param(p, "composition"))?;
    Ok(summary(&plan))
}

fn import(e: &mut Engine, p: &Value) -> Result<Value> {
    stopped(e).map_err(|why| bad("file.import_aaf", why))?;
    if let Some(path) = str_param(p, "path") {
        return import_path(
            e,
            &json!({"path": path, "media_dir": p.get("media_dir"), "mapping": p.get("mapping"), "offset_samples": p.get("offset_samples")}),
            false,
        );
    }
    let plan_value = p.get("plan").ok_or_else(|| bad("file.import_aaf", "`path` or `plan` required"))?;
    let plan: Plan = serde_json::from_value(plan_value.clone()).map_err(|err| bad("file.import_aaf", err.to_string()))?;
    let selected = match p.get("selected").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(|v| v.as_u64().and_then(|n| usize::try_from(n).ok())).collect::<Vec<_>>(),
        None => (0..plan.tracks.len()).collect(),
    };
    let mapping = str_param(p, "mapping").unwrap_or("absolute");
    let offset = p.get("offset_samples").and_then(Value::as_i64).unwrap_or(0);
    let (session, report) = crate::interchange::import_into(e.session(), plan, &selected, mapping, offset)?;
    *e.session_mut() = session;
    Ok(report)
}

/// What File ▸ Import ▸ Session Data runs for an AAF, DAWproject, OMF or Pro Tools path.
pub(crate) fn import_foreign(e: &mut Engine, path: &str) -> Result<Value> {
    import_path(e, &json!({"path": path}), path.to_ascii_lowercase().ends_with(".dawproject"))
}

fn import_path(e: &mut Engine, p: &Value, force_daw: bool) -> Result<Value> {
    stopped(e).map_err(|why| bad("file.import_aaf", why))?;
    let path = str_param(p, "path").ok_or_else(|| bad("file.import_aaf", "`path` required"))?;
    let path = if force_daw && !path.to_ascii_lowercase().ends_with(".dawproject") {
        return Err(bad("file.import_dawproject", "path must be a .dawproject file"));
    } else {
        path
    };
    let plan = crate::interchange::read_path(path, p.get("media_dir").and_then(Value::as_str))?;
    let selected: Vec<usize> = (0..plan.tracks.len()).collect();
    let mapping = str_param(p, "mapping").unwrap_or("absolute");
    let offset = p.get("offset_samples").and_then(Value::as_i64).unwrap_or(0);
    let (session, report) = crate::interchange::import_into(e.session(), plan, &selected, mapping, offset)?;
    *e.session_mut() = session;
    Ok(report)
}

fn export(e: &mut Engine, p: &Value) -> Result<Value> {
    let path = str_param(p, "path").ok_or_else(|| bad("file.export_aaf", "`path` required"))?;
    let embed = p.get("embed").and_then(Value::as_bool).unwrap_or(true);
    let tracks = track_ids(e, p)?;
    crate::interchange::export_to(e.session(), path, &tracks, embed)
}

fn track_ids(e: &Engine, p: &Value) -> Result<Vec<soundcraft_model::TrackId>> {
    if let Some(list) = p.get("tracks").and_then(Value::as_array) {
        let mut ids = Vec::new();
        for v in list {
            let name = v.as_str().ok_or_else(|| bad("file.export_aaf", "track names must be strings"))?;
            let track = e.session().track_by_name(name).ok_or_else(|| bad("file.export_aaf", format!("no track named {name}")))?;
            ids.push(track.id);
        }
        return Ok(ids);
    }
    let selected = &e.session().edit.selected_tracks;
    if selected.is_empty() { Ok(Vec::new()) } else { Ok(selected.clone()) }
}

fn summary(plan: &Plan) -> Value {
    let tracks: Vec<_> = plan
        .tracks
        .iter()
        .enumerate()
        .map(|(index, t)| json!({"index": index, "name": t.name, "channels": t.channels, "clips": t.clips.len()}))
        .collect();
    json!({
        "plan": plan,
        "name": plan.name,
        "sample_rate": plan.sample_rate,
        "tracks": tracks,
        "warnings": plan.warnings,
    })
}
