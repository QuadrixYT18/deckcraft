use super::{Icon, SlideApp, Ui, big_button, big_button_ex, group, json, run};

// ---------- Draw ----------

pub(super) fn draw(app: &mut SlideApp, ui: &mut Ui) {
    use deckcraft_engine::ToolKind;
    let cur = app.session.tool.kind.clone();
    group(ui, |ui| {
        let sel = matches!(cur, ToolKind::Select);
        if big_button_ex(ui, Icon::LassoSelect, "Select", true, false, sel).0.clicked() {
            app.session.set_tool(ToolKind::Select);
        }
        let er = matches!(&cur, ToolKind::Ink { mode, .. } if mode == "eraser");
        let eraser = |w: f64| ToolKind::Ink { mode: "eraser".into(), color: deckcraft_color::Rgba::BLACK, width: w };
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let (r, _) = big_button_ex(ui, Icon::Eraser, "Eraser", true, false, er);
            if r.clicked() {
                app.session.set_tool(eraser(app.ui.eraser_width));
            }
            let arrow = crate::widgets::drop_button(ui, "", egui::vec2(r.rect.width(), 12.0), true);
            egui::Popup::menu(&arrow).show(|ui| {
                ui.label(tl!("Eraser size"));
                for (label, w) in [("Small", 4.0), ("Medium", 12.0), ("Large", 30.0)] {
                    if ui.selectable_label((app.ui.eraser_width - w).abs() < 1e-6, tl!(label)).clicked() {
                        app.ui.eraser_width = w;
                        app.session.set_tool(eraser(w));
                        ui.close();
                    }
                }
            });
        });
    });
    group(ui, |ui| {
        if app.ui.pens.is_empty() {
            app.ui.pens = crate::state::Pen::defaults();
        }
        let sc = super::scheme(app);
        let mut remove: Option<usize> = None;
        for k in 0..app.ui.pens.len() {
            let pen = app.ui.pens[k].clone();
            let color = deckcraft_color::Rgba::rgb(pen.rgb[0], pen.rgb[1], pen.rgb[2]);
            let mode = if pen.kind == "highlighter" { "highlighter" } else { "pen" };
            // Named by colour where it has a name, so each pen is told apart (also by screen readers).
            let (icon, name) = match (pen.kind.as_str(), pen.rgb) {
                ("highlighter", _) => (Icon::HighlighterPen, "Highlighter"),
                ("pencil", _) => (Icon::Pencil, "Pencil"),
                (_, [20, 20, 20]) => (Icon::Pen, "Black Pen"),
                (_, [0xD8, 0x3F, 0x3F]) => (Icon::Pen, "Red Pen"),
                (_, [0x2E, 0x6F, 0xD8]) => (Icon::Pen, "Blue Pen"),
                _ => (Icon::Pen, "Pen"),
            };
            let on = matches!(&cur, ToolKind::Ink { mode: m, color: c, width: w } if m == mode && *c == color && (*w - pen.width).abs() < 1e-6);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let (r, _) = big_button_ex(ui, icon, name, true, false, on);
                // The pen's colour as a bar under its icon.
                let bar = egui::Rect::from_min_size(egui::pos2(r.rect.center().x - 10.0, r.rect.min.y + 30.0), egui::vec2(20.0, 4.0));
                ui.painter().rect_filled(bar, 1.0, egui::Color32::from_rgb(pen.rgb[0], pen.rgb[1], pen.rgb[2]));
                if r.clicked() {
                    app.session.set_tool(ToolKind::Ink { mode: mode.into(), color, width: pen.width });
                }
                let arrow = crate::widgets::drop_button(ui, "", egui::vec2(r.rect.width(), 12.0), true);
                egui::Popup::menu(&arrow).show(|ui| {
                    ui.label(tl!("Colour"));
                    if let Some(Some(c)) = crate::widgets::color_grid(ui, &sc, None) {
                        let rgb = c.resolve(&sc, None);
                        app.ui.pens[k].rgb = [rgb.r, rgb.g, rgb.b];
                    }
                    ui.separator();
                    ui.label(tl!("Thickness"));
                    let widths: &[f64] =
                        if pen.kind == "highlighter" { &[4.0, 6.0, 10.0, 14.0, 20.0] } else { &[0.5, 1.0, 1.5, 2.25, 3.5, 5.0, 8.0] };
                    ui.horizontal_wrapped(|ui| {
                        for w in widths {
                            if ui.selectable_label((pen.width - w).abs() < 1e-6, format!("{w} pt")).clicked() {
                                app.ui.pens[k].width = *w;
                            }
                        }
                    });
                    ui.separator();
                    if ui.button(tl!("Remove Pen")).clicked() {
                        remove = Some(k);
                        ui.close();
                    }
                });
                // A changed pen that is in use draws with its new settings at once.
                let now = &app.ui.pens.get(k).cloned().unwrap_or(pen.clone());
                if on && now != &pen {
                    let c = deckcraft_color::Rgba::rgb(now.rgb[0], now.rgb[1], now.rgb[2]);
                    app.session.set_tool(ToolKind::Ink { mode: mode.into(), color: c, width: now.width });
                }
            });
        }
        if let Some(k) = remove
            && app.ui.pens.len() > 1
        {
            app.ui.pens.remove(k);
        }
        let add = big_button(ui, Icon::Pen, "Add Pen", true);
        egui::Popup::menu(&add).show(|ui| {
            for (kind, label, rgb, width) in
                [("pen", "Pen", [20, 20, 20], 1.5), ("pencil", "Pencil", [90, 90, 90], 1.0), ("highlighter", "Highlighter", [255, 230, 0], 10.0)]
            {
                if ui.button(tl!(label)).clicked() {
                    app.ui.pens.push(crate::state::Pen { kind: kind.into(), rgb, width });
                    ui.close();
                }
            }
        });
    });
    group(ui, |ui| {
        let has_ink = app.session.doc().is_ok_and(|d| d.shapes().iter().any(|s| matches!(s.kind, deckcraft_model::ShapeKind::Ink { .. })));
        let r = big_button(ui, Icon::InkToShape, "Ink to\nShape", has_ink);
        if r.on_hover_text(tl!("Ink to Shape: turn drawn lines, rectangles, ovals and triangles into shapes")).clicked() {
            app.session.set_tool(ToolKind::Select);
            run(app, "draw.inkToShape", json!({}));
        }
        let r = big_button(ui, Icon::InkToText, "Ink to\nText", false);
        let _ = r.on_hover_text(tl!("Ink to Text (coming soon)"));
        let r = big_button(ui, Icon::InkToMath, "Ink to\nMath", false);
        let _ = r.on_hover_text(tl!("Ink to Math (coming soon)"));
    });
}
