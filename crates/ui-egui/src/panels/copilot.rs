//! The Copilot pane: an offline assistant. It checks the presentation (`review.analyze`) and
//! offers each fix with one click, and runs the assistant actions (`assist.*`) and Design Ideas.
//! Nothing leaves the device and nothing costs money.

use egui::{Color32, RichText, Ui, vec2};
use serde_json::{Value, json};

use super::{SlideApp, Tokens};

/// The last check, kept until the presentation changes: (document uid, revision, result).
#[derive(Clone, Default)]
struct Checked {
    key: (u64, u64),
    result: Value,
}

fn action(app: &mut SlideApp, ui: &mut Ui, label: &str, tip: &str, id: &str, params: Value) {
    let w = ui.available_width();
    if ui.add(egui::Button::new(label).min_size(vec2(w, 30.0))).on_hover_text(tip).clicked() {
        match app.run(id, params) {
            Ok(_) => app.set_status(format!("Copilot: {}", label.trim_start_matches(|c: char| !c.is_alphanumeric()))),
            Err(e) => app.set_status(format!("Copilot: {e}")),
        }
    }
}

/// Run a finding's fix: its commands in order, one undo step each.
fn apply_fix(app: &mut SlideApp, fix: &Value) -> Result<(), String> {
    for step in fix.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let id = step.get("command").and_then(Value::as_str).ok_or("broken fix")?;
        app.run(id, step.get("params").cloned().unwrap_or(json!({})))?;
    }
    Ok(())
}

pub(super) fn copilot(app: &mut SlideApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new("Your presentation assistant. It runs on this device, free and offline.").color(t.text_dim));
    ui.add_space(8.0);
    let Some(st) = app.session.active() else { return };
    let key = (st.uid, st.revision);
    let slide = st.selection.slide;

    ui.label(RichText::new("Create").strong());
    action(app, ui, "Agenda slide", "Adds an agenda after the first slide, from the sections or the slide titles", "assist.agenda", json!({}));
    action(app, ui, "Speaker notes for this slide", "Drafts notes from the slide's own text where the notes are empty", "assist.notes", json!({}));
    action(app, ui, "Speaker notes for all slides", "Drafts notes for every slide whose notes are empty", "assist.notes", json!({"all": true}));
    action(app, ui, "Split this slide in two", "Moves the second half of a crowded text box to a new slide", "assist.split", json!({"index": slide}));
    let w = ui.available_width();
    if ui.add(egui::Button::new("Design ideas for this slide").min_size(vec2(w, 30.0))).clicked() {
        app.ui.pane = Some("designer".into());
    }
    ui.add_space(12.0);

    // The check, run again whenever the presentation changed.
    let cache_id = egui::Id::unique("copilot-check");
    let mut checked: Checked = ui.ctx().data_mut(|d| d.get_temp(cache_id)).unwrap_or_default();
    if checked.key != key || checked.result.is_null() {
        checked = Checked { key, result: app.run("review.analyze", json!({})).unwrap_or(Value::Null) };
        ui.ctx().data_mut(|d| d.insert_temp(cache_id, checked.clone()));
    }
    let findings = checked.result.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
    let stats = checked.result.get("stats").cloned().unwrap_or(Value::Null);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Check").strong());
        if let (Some(s), Some(w)) = (stats.get("slides").and_then(Value::as_u64), stats.get("words").and_then(Value::as_u64)) {
            ui.label(RichText::new(format!("{s} slides · {w} words")).color(t.text_dim));
        }
    });
    if findings.is_empty() {
        ui.label(RichText::new("✔ Nothing to improve. Well done!").color(Color32::from_rgb(40, 160, 80)));
        return;
    }
    let fixable: Vec<Value> = findings.iter().filter_map(|f| f.get("fix").cloned()).collect();
    if fixable.len() > 1
        && ui.add(egui::Button::new(format!("Fix all {} automatically", fixable.len())).min_size(vec2(ui.available_width(), 28.0))).clicked()
    {
        let mut ok = 0;
        // Deletions last, so earlier fixes still find their shapes.
        let (deletes, others): (Vec<&Value>, Vec<&Value>) = fixable.iter().partition(|f| f.to_string().contains("edit.delete"));
        for f in others.into_iter().chain(deletes) {
            if apply_fix(app, f).is_ok() {
                ok += 1;
            }
        }
        app.set_status(format!("Copilot: fixed {ok} of {}", fixable.len()));
    }
    ui.add_space(4.0);
    for (i, f) in findings.iter().enumerate() {
        let warning = f.get("severity").and_then(Value::as_str) == Some("warning");
        let msg = f.get("message").and_then(Value::as_str).unwrap_or("");
        let slide_no = f.get("slide").and_then(Value::as_u64);
        egui::Frame::NONE.fill(t.field_bg).corner_radius(6.0).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(if warning { "●" } else { "○" }).color(if warning { Color32::from_rgb(220, 120, 30) } else { t.text_dim }));
                ui.label(msg);
            });
            ui.horizontal(|ui| {
                if let Some(n) = slide_no
                    && ui.small_button(format!("Go to slide {n}")).clicked()
                {
                    let _ = app.run("slide.go", json!({"index": n.saturating_sub(1)}));
                }
                if let Some(fix) = f.get("fix")
                    && ui.small_button("Fix").clicked()
                {
                    match apply_fix(app, fix) {
                        Ok(()) => app.set_status("Copilot: fixed"),
                        Err(e) => app.set_status(format!("Copilot: {e}")),
                    }
                }
                if f.get("rule").and_then(Value::as_str) == Some("noNotes")
                    && let Some(n) = slide_no
                    && ui.small_button("Draft notes").clicked()
                {
                    let _ = app.run("slide.go", json!({"index": n.saturating_sub(1)}));
                    let _ = app.run("assist.notes", json!({}));
                }
                if matches!(f.get("rule").and_then(Value::as_str), Some("manyLines" | "wordy"))
                    && let Some(n) = slide_no
                    && ui.small_button("Split slide").clicked()
                {
                    match app.run("assist.split", json!({"index": n.saturating_sub(1)})) {
                        Ok(_) => app.set_status("Copilot: slide split in two"),
                        Err(e) => app.set_status(format!("Copilot: {e}")),
                    }
                }
            });
        });
        if i + 1 < findings.len() {
            ui.add_space(4.0);
        }
    }
}
