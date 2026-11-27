use super::galleries::{Fl, kurbo_flatten, shapes_gallery};
use super::home::layout_menu;
use super::view::zoom_menu;
use super::{
    Color32, CornerRadius, Icon, Rect, Sense, SlideApp, Stroke, Tokens, Ui, big, big_button, big_button_ex, enabled, group, json, menu_item, pos2,
    run, tip, vec2,
};

/// Photo Album is a split button: the main click starts a new album, the arrow edits one.
fn photo_album_button(app: &mut SlideApp, ui: &mut Ui) {
    let en = enabled(app, "insert.photoAlbum");
    let (main, arrow) = big_button_ex(ui, Icon::Picture, "Photo\nAlbum", en, true, false);
    let _ = main.clone().on_hover_text(tip("insert.photoAlbum", "Photo Album…"));
    main.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, en, tl!("Photo Album…")));
    if let Some(a) = &arrow {
        let _ = a.clone().on_hover_text(tl!("Photo Album options"));
        a.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, en, tl!("Photo Album options")));
    }
    if main.clicked() {
        run(app, "insert.photoAlbum", json!({}));
    }
    if let Some(a) = arrow {
        egui::Popup::menu(&a).show(|ui| {
            menu_item(app, ui, Icon::Picture, "New Photo Album…", "insert.photoAlbum", json!({}));
            menu_item(app, ui, Icon::Picture, "Edit Photo Album…", "insert.photoAlbumEdit", json!({}));
        });
    }
}

// ---------- Insert ----------

pub(super) fn insert(app: &mut SlideApp, ui: &mut Ui) {
    group(ui, |ui| {
        let (main, arrow) = big_button_ex(ui, Icon::NewSlide, "New\nSlide", enabled(app, "slide.new"), true, false);
        if main.clicked() {
            run(app, "slide.new", json!({}));
        }
        if let Some(a) = arrow {
            egui::Popup::menu(&a).width(420.0).show(|ui| layout_menu(app, ui, "slide.new"));
        }
    });
    group(ui, |ui| {
        let r = big_button(ui, Icon::Table, "Table", enabled(app, "insert.table"));
        egui::Popup::menu(&r).show(|ui| {
            if let Some((rr, cc)) = table_grid_picker(ui) {
                run(app, "insert.table", json!({"rows": rr, "cols": cc}));
                ui.close();
            }
            ui.separator();
            if ui.button(tl!("Insert Table…")).clicked() {
                app.dialog = Some(crate::dialogs::Dialog::new("table"));
                ui.close();
            }
            if ui.button(tl!("Draw Table")).clicked() {
                app.session.set_tool(deckcraft_engine::ToolKind::Shape { preset: deckcraft_engine::tools::DRAW_TABLE.into() });
                ui.close();
            }
        });
    });
    group(ui, |ui| {
        big(app, ui, Icon::Picture, "Pictures", "app.insertPictureDialog", json!({}));
        photo_album_button(app, ui);
        let r = big_button(ui, Icon::Screenshot, "Screenshot", false);
        let _ = r.on_hover_text(tl!("Screenshot (not available yet)"));
    });
    group(ui, |ui| {
        let r = big_button(ui, Icon::Shapes, "Shapes", enabled(app, "shape.insert"));
        egui::Popup::menu(&r).show(|ui| {
            if let Some(p) = shapes_gallery(ui) {
                app.session.set_tool(deckcraft_engine::ToolKind::Shape { preset: p.to_string() });
            }
        });
        let r = big_button(ui, Icon::Icons, "Icons", enabled(app, "insert.icon"));
        egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(340.0).show(|ui| icon_library(app, ui));
        let r = big_button(ui, Icon::SmartArt, "Diagram", enabled(app, "insert.smartArt"));
        egui::Popup::menu(&r).show(|ui| diagram_menu(app, ui));
        let r = big_button(ui, Icon::Chart, "Chart", enabled(app, "insert.chart"));
        egui::Popup::menu(&r).show(|ui| chart_menu(app, ui));
    });
    group(ui, |ui| {
        let r = big_button(ui, Icon::Zoom, "Zoom", enabled(app, "insert.zoom"));
        egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(300.0).show(|ui| zoom_menu(app, ui));
        big(app, ui, Icon::Link, "Link", "app.dialog", json!({"id": "hyperlink"}));
        let r = big_button(ui, Icon::Action, "Action", enabled(app, "insert.actionButton"));
        egui::Popup::menu(&r).show(|ui| {
            for k in ["back", "forward", "beginning", "end", "home", "information", "return", "movie", "document", "sound", "help", "custom"] {
                if ui.button(format!("Action Button: {k}")).clicked() {
                    run(app, "insert.actionButton", json!({"kind": k}));
                }
            }
            ui.separator();
            let has_sel = app.session.active().is_some_and(|s| !s.selection.shapes.is_empty());
            if ui.add_enabled(has_sel, egui::Button::new(tl!("Action Settings…"))).clicked() {
                app.dialog = Some(crate::dialogs::Dialog::new("action"));
            }
        });
    });
    group(ui, |ui| {
        big(app, ui, Icon::Comment, "Comment", "app.dialog", json!({"id": "comment"}));
    });
    group(ui, |ui| {
        big(app, ui, Icon::TextBox, "Text\nBox", "insert.textBox", json!({}));
        big(app, ui, Icon::HeaderFooter, "Header &\nFooter", "app.dialog", json!({"id": "headerFooter"}));
        let r = big_button(ui, Icon::WordArt, "Decorative Text", enabled(app, "insert.wordArt"));
        egui::Popup::menu(&r).show(|ui| {
            for i in 0..6 {
                if ui.button(format!("Decorative Text style {}", i + 1)).clicked() {
                    run(app, "insert.wordArt", json!({"style": i}));
                }
            }
        });
        big(app, ui, Icon::DateTime, "Date &\nTime", "insert.dateTime", json!({}));
        big(app, ui, Icon::SlideNumber, "Slide\nNumber", "insert.slideNumber", json!({}));
    });
    group(ui, |ui| {
        big(app, ui, Icon::Equation, "Equation", "app.dialog", json!({"id": "equation"}));
        big(app, ui, Icon::Symbol, "Symbol", "app.dialog", json!({"id": "symbol"}));
    });
    group(ui, |ui| {
        big(app, ui, Icon::Video, "Video", "app.insertVideoDialog", json!({}));
        big(app, ui, Icon::Audio, "Audio", "app.insertAudioDialog", json!({}));
    });
}

/// Hover-to-size table grid. Returns (rows, cols) when clicked.
pub fn table_grid_picker(ui: &mut Ui) -> Option<(usize, usize)> {
    let t = Tokens::get(ui.ctx());
    let id = ui.scope_id().with("tgrid");
    let mut hover: (usize, usize) = ui.data_mut(|d| d.get_temp(id).unwrap_or((0, 0)));
    let cell = 16.0;
    let label = if hover.0 > 0 { format!("{}x{} Table", hover.1, hover.0) } else { "Insert Table".into() };
    ui.label(label);
    let (rect, resp) = ui.allocate_exact_size(vec2(10.0 * (cell + 2.0), 8.0 * (cell + 2.0)), Sense::click());
    if let Some(p) = resp.hover_pos() {
        let c = ((p.x - rect.min.x) / (cell + 2.0)).floor() as usize + 1;
        let r = ((p.y - rect.min.y) / (cell + 2.0)).floor() as usize + 1;
        hover = (r.min(8), c.min(10));
    }
    for r in 0..8 {
        for c in 0..10 {
            let cr = Rect::from_min_size(rect.min + vec2(c as f32 * (cell + 2.0), r as f32 * (cell + 2.0)), vec2(cell, cell));
            let on = r < hover.0 && c < hover.1;
            ui.painter().rect(
                cr,
                CornerRadius::ZERO,
                if on { t.accent.gamma_multiply(0.3) } else { t.card },
                Stroke::new(1.0, if on { t.accent } else { t.border }),
                egui::StrokeKind::Inside,
            );
        }
    }
    ui.data_mut(|d| d.insert_temp(id, hover));
    if resp.clicked() && hover.0 > 0 { Some(hover) } else { None }
}

fn render_icon_categories(app: &mut SlideApp, ui: &mut Ui, icons: &[deckcraft_engine::icons::Icon], color: Color32, hover: Color32) {
    let mut cats: Vec<&str> = Vec::new();
    for icon in icons {
        if !cats.contains(&icon.category) {
            cats.push(icon.category);
        }
    }
    for cat in cats {
        ui.label(egui::RichText::new(cat).small().strong());
        ui.horizontal_wrapped(|ui| {
            for icon in icons.iter().filter(|i| i.category == cat) {
                icon_cell(app, ui, icon, color, hover);
            }
        });
    }
}

fn render_filtered_icons(app: &mut SlideApp, ui: &mut Ui, icons: &[deckcraft_engine::icons::Icon], q: &str, color: Color32, hover: Color32) {
    let mut n = 0;
    ui.horizontal_wrapped(|ui| {
        for icon in icons {
            if icon.name.contains(q) || icon.label.to_lowercase().contains(q) {
                icon_cell(app, ui, icon, color, hover);
                n += 1;
            }
        }
    });
    if n == 0 {
        ui.label(tl!("No icons match."));
    }
}

/// Lucide stroke icons (Insert › Icons). Search filters the gallery; a click inserts the outline.
fn icon_library(app: &mut SlideApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let (color, hover) = (t.text, t.hover);
    let id = ui.scope_id().with("icon-query");
    let mut query = ui.data_mut(|d| d.get_temp::<String>(id).unwrap_or_default());
    ui.add(egui::TextEdit::singleline(&mut query).hint_text(tl!("Search icons")).desired_width(ui.available_width()));
    let q = query.trim().to_lowercase();
    ui.data_mut(|d| d.insert_temp(id, query));
    let icons = deckcraft_engine::icons::icons();
    egui::ScrollArea::vertical().id_salt("icon-scroll").max_height(420.0).show(ui, |ui| {
        if q.is_empty() {
            render_icon_categories(app, ui, icons, color, hover);
        } else {
            render_filtered_icons(app, ui, icons, &q, color, hover);
        }
    });
}

fn icon_cell(app: &mut SlideApp, ui: &mut Ui, icon: &deckcraft_engine::icons::Icon, color: Color32, hover: Color32) {
    let (r, resp) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(4), hover);
    }
    paint_icon(ui.painter(), r.shrink(6.0), icon, color);
    if resp.on_hover_text(icon.label).clicked() {
        let _ = app.run("insert.icon", json!({"name": icon.name}));
        ui.close();
    }
}

/// Stroke a Lucide outline into a gallery cell. The line is the design stroke scaled to the cell.
fn paint_icon(p: &egui::Painter, r: Rect, icon: &deckcraft_engine::icons::Icon, color: Color32) {
    let Some(src) = deckcraft_engine::icons::outline(icon.name) else { return };
    let mut bp = src.clone();
    let (w, h) = (r.width().max(1.0) as f64, r.height().max(1.0) as f64);
    bp.apply_affine(deckcraft_geom::Affine::scale_non_uniform(w / deckcraft_engine::icons::VIEW, h / deckcraft_engine::icons::VIEW));
    let sw = ((r.width() * icon.stroke as f32) / deckcraft_engine::icons::VIEW as f32).clamp(1.0, 3.5);
    let mut pts: Vec<egui::Pos2> = vec![];
    let mut polys: Vec<(Vec<egui::Pos2>, bool)> = vec![];
    kurbo_flatten(&bp, |el| match el {
        Fl::Move(x, y) => {
            if pts.len() > 1 {
                polys.push((std::mem::take(&mut pts), false));
            }
            pts.clear();
            pts.push(pos2(r.min.x + x as f32, r.min.y + y as f32));
        }
        Fl::Line(x, y) => pts.push(pos2(r.min.x + x as f32, r.min.y + y as f32)),
        Fl::Close => {
            if pts.len() > 1 {
                polys.push((std::mem::take(&mut pts), true));
            }
        }
    });
    if pts.len() > 1 {
        polys.push((pts, false));
    }
    for (poly, closed) in polys {
        p.add(egui::Shape::Path(egui::epaint::PathShape {
            points: poly,
            closed,
            fill: Color32::TRANSPARENT,
            stroke: egui::epaint::PathStroke::new(sw, color),
        }));
    }
}

/// The chart types in the Insert Chart and Change Chart Type menus: the main ones, a separator
/// (`None`), then the Office 2016 types not already listed.
pub(super) fn chart_menu_types() -> Vec<Option<deckcraft_model::chart::ChartType>> {
    use deckcraft_model::chart::ChartType;
    let mut v: Vec<Option<ChartType>> = ChartType::MAIN.iter().copied().map(Some).collect();
    v.push(None);
    v.extend(ChartType::OFFICE_2016.iter().copied().filter(|k| !ChartType::MAIN.contains(k)).map(Some));
    v
}

/// Insert › Diagram: the layouts by kind, each a submenu; a click inserts that layout with
/// three sample items. More Diagrams… opens the dialog (layout picker and your own items).
fn diagram_menu(app: &mut SlideApp, ui: &mut Ui) {
    const KINDS: [(&str, &[&str]); 9] = [
        ("List layouts", &["list", "blockList", "hBulletList", "verticalBoxList", "trapezoidList", "tableList"]),
        ("Process", &["process", "chevron", "verticalProcess", "stepUp", "accentProcess", "continuousBlockProcess", "bendingProcess"]),
        ("Cycle", &["cycle", "textCycle", "blockCycle", "continuousCycle", "segmentedCycle", "multidirectionalCycle", "radial"]),
        ("Hierarchy", &["orgChart", "nameTitleOrgChart", "halfCircleOrgChart", "hierarchy", "horizontalHierarchy"]),
        ("Relationship", &["venn", "linearVenn", "stackedVenn", "target", "nestedTarget", "pie", "gear", "funnel"]),
        ("Matrix", &["matrix", "titledMatrix", "gridMatrix", "cycleMatrix"]),
        ("Pyramid", &["pyramid", "invertedPyramid", "segmentedPyramid"]),
        ("Picture", &["pictureList", "pictureCaption", "pictureProcess", "pictureHList"]),
        ("Timeline layouts", &["timeline"]),
    ];
    fn name(id: &'static str) -> &'static str {
        deckcraft_engine::cmd::smartart::LAYOUTS.iter().find(|(k, _)| *k == id).map_or(id, |(_, n)| *n)
    }
    for (kind, ids) in KINDS {
        ui.menu_button(tl!(kind), |ui| {
            for id in ids {
                let item = tl!("Text");
                let params = json!({"kind": id, "items": [item, item, item]});
                let tex = insert_preview(ui.ctx(), &format!("diagram-{id}"), "insert.smartArt", params.clone());
                if preview_row(ui, tex.as_ref(), tl!(name(id))) {
                    run(app, "insert.smartArt", params);
                    ui.close();
                }
            }
        });
    }
    ui.separator();
    if ui.button(tl!("More Diagrams…")).clicked() {
        run(app, "app.dialog", json!({"id": "smartart"}));
        ui.close();
    }
}

/// Insert › Chart: the chart types by kind, each a submenu.
fn chart_menu(app: &mut SlideApp, ui: &mut Ui) {
    use deckcraft_model::chart::ChartType as T;
    const KINDS: [(&str, &[T]); 11] = [
        ("Column charts", &[T::Column, T::StackedColumn, T::PercentColumn]),
        ("Bar", &[T::Bar, T::StackedBar, T::PercentBar]),
        ("Line", &[T::Line, T::LineMarkers, T::StackedLine]),
        ("Area", &[T::Area, T::StackedArea]),
        ("Pie", &[T::Pie, T::Doughnut]),
        ("Scatter", &[T::Scatter, T::Bubble]),
        ("Hierarchy", &[T::Treemap, T::Sunburst]),
        ("Statistic charts", &[T::Histogram, T::Pareto, T::BoxWhisker]),
        ("Waterfall", &[T::Waterfall, T::Funnel, T::Stock]),
        ("Radar", &[T::Radar, T::FilledRadar, T::Surface]),
        ("Combo chart", &[T::Combo, T::RegionMap]),
    ];
    for (kind, types) in KINDS {
        ui.menu_button(tl!(kind), |ui| {
            for k in types {
                // "Combo" alone reads as a combo box in German.
                let label = if *k == T::Combo { tl!("Combo chart") } else { tl!(k.label()) };
                let tex = insert_preview(ui.ctx(), &format!("chart-{k:?}"), "insert.chart", json!({"type": format!("{k:?}")}));
                if preview_row(ui, tex.as_ref(), label) {
                    run(app, "insert.chart", json!({"type": format!("{k:?}")}));
                    ui.close();
                }
            }
        });
    }
}

/// A small picture of what `command` with `params` inserts on an empty slide, cut to the inserted
/// shape: rendered once and kept for the menus.
fn insert_preview(ctx: &egui::Context, key: &str, command: &str, params: serde_json::Value) -> Option<egui::TextureHandle> {
    type Cache = std::collections::HashMap<String, Option<egui::TextureHandle>>;
    let cache_id = egui::Id::unique("insert-previews");
    if let Some(hit) =
        ctx.data(|d| d.get_temp::<std::sync::Arc<std::sync::Mutex<Cache>>>(cache_id)).and_then(|c| c.lock().ok().and_then(|m| m.get(key).cloned()))
    {
        return hit;
    }
    let made = (|| {
        let mut s = deckcraft_engine::Session::with_new();
        let id = s.execute(command, &params).ok()?.get("id")?.as_u64()?;
        let st = s.doc().ok()?;
        let x = st.shape(deckcraft_model::ShapeId(u32::try_from(id).ok()?))?.xfrm?;
        let doc = &st.doc;
        let scale = 240.0 / doc.slide_size.width.max(1.0);
        let img = deckcraft_render::render_slide(doc, 0, &deckcraft_render::RenderOpts { scale, ..Default::default() });
        let full = crate::textures::to_color_image(&img, false);
        let [w, h] = full.size;
        let px = |v: f64, max: usize| ((v * scale).round().max(0.0) as usize).min(max);
        let (x0, y0) = (px(x.x - 4.0, w), px(x.y - 4.0, h));
        let (x1, y1) = (px(x.x + x.w + 4.0, w), px(x.y + x.h + 4.0, h));
        (x1 > x0 + 2 && y1 > y0 + 2).then(|| full.region_by_pixels([x0, y0], [x1 - x0, y1 - y0]))
    })()
    .map(|img| crate::textures::upload_editor(ctx, format!("insert-preview-{key}"), img));
    let cache = ctx.data_mut(|d| d.get_temp_mut_or_default::<std::sync::Arc<std::sync::Mutex<Cache>>>(cache_id).clone());
    if let Ok(mut m) = cache.lock() {
        m.insert(key.to_string(), made.clone());
    }
    made
}

/// A menu row with a preview picture beside its name.
fn preview_row(ui: &mut Ui, tex: Option<&egui::TextureHandle>, label: &str) -> bool {
    let size = vec2(64.0, 40.0);
    let button = match tex {
        Some(t) => egui::Button::image_and_text(egui::Image::new(t).fit_to_exact_size(size).maintain_aspect_ratio(true), label),
        None => egui::Button::new(label),
    };
    ui.add(button.min_size(vec2(0.0, size.y + 6.0))).clicked()
}
