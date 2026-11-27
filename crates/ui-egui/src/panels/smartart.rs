use super::{SlideApp, Tokens, Ui, Value, json, run};

/// SmartArt Text Pane: one line per node, indented by level. Enter adds a line after, Tab and
/// Shift+Tab demote and promote, Backspace in an empty line removes it.
pub(super) fn smartart(app: &mut SlideApp, ui: &mut Ui) {
    let Some((gid, d)) = app.session.active().and_then(deckcraft_engine::cmd::smartart::selected) else {
        ui.label(tl!("Select a Diagram graphic to edit its text."));
        return;
    };
    let texts: Vec<String> = d.nodes.iter().map(|n| n.text.clone()).collect();
    let focused_any = crate::framework::widget_has_keyboard(ui.ctx()) || app.ui.smartart_focus.is_some();
    if app.ui.smartart_drafts.0 != gid.0 || app.ui.smartart_drafts.1.len() != texts.len() || !focused_any {
        app.ui.smartart_drafts = (gid.0, texts.clone());
    }
    let mut drafts = std::mem::take(&mut app.ui.smartart_drafts.1);
    let mut action: Option<(&str, Value)> = None;
    let mut commit: Option<usize> = None;
    let mut focus_row: Option<usize> = None;
    // The toolbar: add, remove, promote, demote, move up, move down the line with the caret.
    let row = app.ui.smartart_row.min(d.nodes.len().saturating_sub(1));
    ui.horizontal(|ui| {
        let tool = |ui: &mut Ui, glyph: &str, tip: &str| {
            ui.add(egui::Button::new(egui::RichText::new(glyph).size(16.0)).min_size(egui::vec2(28.0, 26.0))).on_hover_text(tip).clicked()
        };
        if tool(ui, "+", tl!("Add a line after this one")) {
            action = Some(("smartArt.addNode", json!({"index": row, "position": "after"})));
        }
        if tool(ui, "−", tl!("Remove this line")) && !d.nodes.is_empty() {
            action = Some(("smartArt.removeNode", json!({"index": row})));
            focus_row = Some(row.saturating_sub(1));
        }
        if tool(ui, "←", tl!("Promote")) {
            action = Some(("smartArt.promote", json!({"index": row})));
            focus_row = Some(row);
        }
        if tool(ui, "→", tl!("Demote")) {
            action = Some(("smartArt.demote", json!({"index": row})));
            focus_row = Some(row);
        }
        if tool(ui, "↑", tl!("Move Up")) {
            action = Some(("smartArt.move", json!({"index": row, "up": true})));
        }
        if tool(ui, "↓", tl!("Move Down")) {
            action = Some(("smartArt.move", json!({"index": row, "up": false})));
        }
    });
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Type your text here")).color(Tokens::get(ui.ctx()).text_dim));
    ui.add_space(4.0);
    for (i, node) in d.nodes.iter().enumerate() {
        let Some(draft) = drafts.get_mut(i) else { break };
        let id = egui::Id::unique(("smartart_line", gid.0, i));
        ui.horizontal(|ui| {
            ui.add_space(16.0 * node.level as f32);
            ui.label(tl!("•"));
            // The line keeps Tab for its own command; Escape leaves it.
            let te = egui::TextEdit::singleline(draft).id(id).hint_text(tl!("Diagram text")).desired_width(f32::INFINITY).lock_focus(true);
            let r = ui.add(te);
            if app.ui.smartart_focus == Some(i) {
                r.request_focus();
                app.ui.smartart_focus = None;
            }
            if r.gained_focus() || r.has_focus() {
                app.ui.smartart_row = i;
            }
            if r.has_focus() {
                let back = ui.input(|inp| inp.key_pressed(egui::Key::Backspace));
                if back && draft.is_empty() && texts.get(i).is_some_and(String::is_empty) && d.nodes.len() > 1 {
                    action = Some(("smartArt.removeNode", json!({"index": i})));
                    focus_row = Some(i.saturating_sub(1));
                }
                let (tab, shift) = ui.input(|inp| (inp.key_pressed(egui::Key::Tab), inp.modifiers.shift));
                if tab {
                    action = Some((if shift { "smartArt.promote" } else { "smartArt.demote" }, json!({"index": i})));
                    focus_row = Some(i);
                    commit = Some(i);
                }
            }
            if r.lost_focus() {
                if ui.input(|inp| inp.key_pressed(egui::Key::Enter)) {
                    action = Some(("smartArt.addNode", json!({"index": i, "position": "after"})));
                    focus_row = Some(i + 1);
                }
                commit = Some(i);
            }
        });
    }
    // Commit a changed line before any structural edit, so the edit sees the new text.
    if let Some(i) = commit
        && drafts.get(i) != texts.get(i)
    {
        run(app, "smartArt.setNodes", json!({"id": gid.0, "index": i, "text": drafts.get(i).cloned().unwrap_or_default()}));
    }
    if action.is_some() {
        for (i, t) in drafts.iter().enumerate() {
            if Some(i) != commit && texts.get(i) != Some(t) {
                run(app, "smartArt.setNodes", json!({"id": gid.0, "index": i, "text": t}));
            }
        }
    }
    app.ui.smartart_drafts.1 = drafts;
    if let Some((cmd, mut p)) = action {
        if let Some(o) = p.as_object_mut() {
            o.insert("id".into(), json!(gid.0));
        }
        let redraws = d.imported && cmd != "smartArt.setNodes";
        if let Ok(r) = app.run(cmd, p) {
            if redraws {
                // Saying so, rather than presenting it as the file's own layout (house rule 15).
                app.set_status(tl!("The graphic is now drawn with DeckCraft's own layout; Undo brings back the file's look"));
            }
            let row = r.get("index").and_then(Value::as_u64).map(|v| v as usize).or(focus_row).unwrap_or(0);
            app.ui.smartart_row = row;
            app.ui.smartart_drafts.0 = 0;
            app.ui.smartart_focus = Some(row);
        }
    }
}
