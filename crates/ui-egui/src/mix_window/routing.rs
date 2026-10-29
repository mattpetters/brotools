use super::plugin_menu::{MenuSearch, menu_row};
use super::{Route, SoundApp, Track, TrackId, Ui, json};
use soundcraft_model::MidiEndpoint;

pub(crate) fn route_name(app: &SoundApp, r: &Route) -> String {
    match r {
        Route::None => "no output".into(),
        Route::Main => app.engine.session().outputs.first().map_or_else(|| "Out 1-2".into(), |o| o.name.clone()),
        Route::Bus(b) => app.engine.session().bus(*b).map_or_else(|| "bus?".into(), |b| b.name.clone()),
        Route::Hardware(h) => h.clone(),
    }
}

pub(crate) fn input_name(app: &SoundApp, track: &Track) -> String {
    if track.kind.is_midi() {
        MidiEndpoint::of(&track.mixer.input).label()
    } else if track.kind.has_playlist() && track.mixer.input == Route::None {
        "In 1".to_string()
    } else {
        route_name(app, &track.mixer.input)
    }
}

pub(crate) fn midi_output_name(track: &Track) -> String {
    MidiEndpoint::of(&track.mixer.midi_output).output_label()
}

/// The input or output menu under `resp`. It closes only on a click outside, so its bus search
/// field can be typed in.
pub(crate) fn route_popup(app: &mut SoundApp, resp: &egui::Response, id: TrackId, input: bool) {
    egui::Popup::menu(resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| route_menu(app, ui, id, input));
}

/// Input or output menu: no route and buses on both, plus the main output (outputs) or inputs 1-8 (inputs).
fn route_menu(app: &mut SoundApp, ui: &mut Ui, id: TrackId, input: bool) {
    let midi = app.engine.session().track(id).is_some_and(|t| t.kind.is_midi());
    if midi && input {
        midi_input_menu(app, ui, id);
        return;
    }
    if midi && !input {
        midi_output_menu(app, ui, id);
        ui.separator();
    }
    let key = if input { "input" } else { "output" };
    let cmd = if input { "track.input" } else { "track.output" };
    if ui.button(if input { "no input" } else { "no output" }).clicked() {
        let _ = app.run(cmd, json!({"track": id.0, key: "none"}));
        ui.close();
    }
    let main_name = app.engine.session().outputs.first().map_or_else(|| "Out 1-2".to_string(), |o| o.name.clone());
    if !input && ui.button(main_name).clicked() {
        let _ = app.run(cmd, json!({"track": id.0, key: "main"}));
        ui.close();
    }
    if input {
        for i in 1..=8 {
            if ui.button(format!("In {i}")).clicked() {
                let _ = app.run(cmd, json!({"track": id.0, key: format!("In {i}")}));
                ui.close();
            }
        }
    }
    ui.separator();
    let busses: Vec<String> = app.engine.session().busses.iter().map(|b| b.name.clone()).collect();
    ui.strong("Bus");
    let search = MenuSearch::show(ui, egui::Id::unique(("route-bus-search", id.0, input)), "Search buses…");
    let visible: Vec<&String> = busses.iter().filter(|b| search.matches(b)).collect();
    if let Some(i) = search.rows(ui, visible.len(), 420.0, |ui, i, highlighted| menu_row(ui, highlighted, visible[i])) {
        let _ = app.run(cmd, json!({"track": id.0, key: visible[i]}));
        ui.close();
    }
    if ui.button("new bus...").clicked() {
        let mut n = busses.len() + 1;
        while busses.contains(&format!("Bus {n}")) || app.engine.session().track_by_name(&format!("Bus {n}")).is_some() {
            n += 1;
        }
        let _ = app.run(cmd, json!({"track": id.0, key: format!("Bus {n}")}));
        ui.close();
    }
}

fn midi_input_menu(app: &mut SoundApp, ui: &mut Ui, id: TrackId) {
    if ui.button("All").clicked() {
        let _ = app.run("track.input", json!({"track": id.0, "input": "all"}));
        ui.close();
    }
    let names: Vec<String> = app.midi_hw.inputs().iter().filter(|p| p.enabled).map(|p| p.name.clone()).collect();
    if names.is_empty() {
        ui.weak("No MIDI inputs — Setup › MIDI › MIDI Input Devices");
        return;
    }
    for name in names {
        let name2 = name.clone();
        ui.menu_button(&name, |ui| {
            if ui.button("All channels").clicked() {
                let _ = app.run("track.input", json!({"track": id.0, "input": format!("MIDI:*:{name2}")}));
                ui.close();
            }
            for ch in 1..=16 {
                if ui.button(format!("Channel {ch}")).clicked() {
                    let _ = app.run("track.input", json!({"track": id.0, "input": format!("MIDI:{ch}:{name2}")}));
                    ui.close();
                }
            }
        });
    }
}

fn midi_output_menu(app: &mut SoundApp, ui: &mut Ui, id: TrackId) {
    ui.strong("MIDI output");
    if ui.button("no MIDI output").clicked() {
        let _ = app.run("track.midi_output", json!({"track": id.0, "output": "none"}));
        ui.close();
    }
    let names: Vec<String> = app.midi_hw.outputs().iter().filter(|p| p.enabled).map(|p| p.name.clone()).collect();
    for name in names {
        let name2 = name.clone();
        ui.menu_button(&name, |ui| {
            if ui.button("All channels").clicked() {
                let _ = app.run("track.midi_output", json!({"track": id.0, "output": format!("MIDI:*:{name2}")}));
                ui.close();
            }
            for ch in 1..=16 {
                if ui.button(format!("Channel {ch}")).clicked() {
                    let _ = app.run("track.midi_output", json!({"track": id.0, "output": format!("MIDI:{ch}:{name2}")}));
                    ui.close();
                }
            }
        });
    }
}
