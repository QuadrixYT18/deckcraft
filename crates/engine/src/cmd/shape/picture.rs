use super::{NO_ID_SPACE, Result, Session, Value, bad, bool_or, bool_param, edit_geometry, edit_shapes, f64_param, json};

pub(super) fn crop(s: &mut Session, p: &Value) -> Result<Value> {
    let c = [f64_param(p, "left"), f64_param(p, "top"), f64_param(p, "right"), f64_param(p, "bottom")];
    edit_geometry(s, p, "picture.crop", |sh| {
        if let deckcraft_model::ShapeKind::Picture { fill } = &mut sh.kind {
            for (i, v) in c.iter().enumerate() {
                if let (Some(v), Some(slot)) = (v, fill.crop.get_mut(i)) {
                    *slot = v.clamp(-10.0, 0.95);
                }
            }
        }
        Ok(())
    })
}

pub(super) fn crop_to(s: &mut Session, p: &Value) -> Result<Value> {
    let r = p.get("rect").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).collect::<Vec<f64>>()).unwrap_or_default();
    let [x, y, w, h] = r[..] else { return Err(bad("picture.cropTo", "`rect` must be [x, y, w, h]")) };
    let rect = deckcraft_geom::Xfrm::new(x, y, w, h);
    edit_geometry(s, p, "picture.cropTo", |sh| {
        let deckcraft_model::ShapeKind::Picture { fill } = &mut sh.kind else { return Err(bad("picture.cropTo", "not a picture")) };
        let b = sh.xfrm.ok_or_else(|| bad("picture.cropTo", "the picture has no box of its own (a placeholder picture)"))?;
        if b.rot.abs() > 0.01 {
            // The handles and `rect` are in slide axes; a turned picture would crop askew.
            return Err(bad("picture.cropTo", "rotate the picture back to 0° to crop it by dragging"));
        }
        let (nb, crop) =
            crate::tools::picture_crop_to(b, fill.crop, rect).ok_or_else(|| bad("picture.cropTo", "nothing of the picture would be left"))?;
        fill.crop = crop;
        sh.xfrm = Some(nb);
        Ok(())
    })
}

pub(super) fn picture_adjust(s: &mut Session, p: &Value) -> Result<Value> {
    edit_shapes(s, p, "picture.adjust", |sh| {
        if let deckcraft_model::ShapeKind::Picture { fill } = &mut sh.kind {
            let a = &mut fill.adjust;
            if bool_or(p, "reset", false) {
                *a = Default::default();
            }
            if let Some(v) = f64_param(p, "brightness") {
                a.brightness = v.clamp(-1.0, 1.0);
            }
            if let Some(v) = f64_param(p, "contrast") {
                a.contrast = v.clamp(-1.0, 1.0);
            }
            if let Some(v) = f64_param(p, "saturation") {
                a.saturation = Some(v.clamp(0.0, 4.0));
            }
            if let Some(v) = bool_param(p, "grayscale") {
                a.grayscale = v;
            }
            if let Some(v) = f64_param(p, "transparency") {
                fill.alpha = Some((1.0 - v).clamp(0.0, 1.0));
            }
        }
        Ok(())
    })
}

pub(super) fn picture_reset(s: &mut Session, p: &Value) -> Result<Value> {
    edit_shapes(s, p, "picture.reset", |sh| {
        if let deckcraft_model::ShapeKind::Picture { fill } = &mut sh.kind {
            fill.adjust = Default::default();
            fill.crop = [0.0; 4];
            fill.alpha = None;
            if let Some(a) = fill.artistic.take() {
                fill.media = a.original;
            }
        }
        sh.effects = None;
        sh.line = None;
        Ok(())
    })
}

pub(super) fn picture_change(s: &mut Session, p: &Value) -> Result<Value> {
    let (name, bytes) = crate::cmd::insert::media_bytes(p, "picture.change")?;
    let ct = crate::cmd::insert::content_type(&name, &bytes);
    let media = s.edit(|doc, _| doc.add_media(&name, ct, bytes).ok_or_else(|| bad("picture.change", NO_ID_SPACE)))?;
    edit_shapes(s, p, "picture.change", |sh| {
        if let deckcraft_model::ShapeKind::Picture { fill } = &mut sh.kind {
            fill.media = media;
            fill.crop = [0.0; 4];
            fill.artistic = None;
        }
        Ok(())
    })
}

pub(super) fn media_options(s: &mut Session, p: &Value) -> Result<Value> {
    edit_shapes(s, p, "media.options", |sh| {
        if let deckcraft_model::ShapeKind::Media(m) = &mut sh.kind {
            if let Some(v) = bool_param(p, "autoplay") {
                m.autoplay = v;
            }
            if let Some(v) = bool_param(p, "loop") {
                m.loop_play = v;
            }
            if let Some(v) = bool_param(p, "rewind") {
                m.rewind = v;
            }
            if let Some(v) = bool_param(p, "acrossSlides") {
                m.play_across_slides = v;
            }
            if let Some(v) = bool_param(p, "hide") {
                m.hide_while_not_playing = v;
            }
            if let Some(v) = bool_param(p, "fullScreen") {
                m.full_screen = v;
            }
            if let Some(v) = f64_param(p, "volume") {
                m.volume = v.clamp(0.0, 1.0);
            }
            let ms = |k: &str| p.get(k).and_then(Value::as_u64).map(|v| v.min(u32::MAX as u64) as u32);
            if let Some(v) = ms("trimStart") {
                m.trim_start_ms = v;
            }
            if let Some(v) = ms("trimEnd") {
                m.trim_end_ms = v;
            }
            if let Some(v) = ms("fadeIn") {
                m.fade_in_ms = v;
            }
            if let Some(v) = ms("fadeOut") {
                m.fade_out_ms = v;
            }
        }
        Ok(())
    })?;
    Ok(json!({}))
}

#[cfg(test)]
mod crop_to_tests {
    use serde_json::json;

    use crate::Session;

    fn picture(s: &mut Session) -> deckcraft_model::ShapeId {
        // A 2x1 PNG.
        let png = image_png();
        let r = s.execute("insert.picture", &json!({"data": png, "name": "p.png", "rect": [100, 100, 400, 200]})).unwrap();
        deckcraft_model::ShapeId(r["id"].as_u64().unwrap() as u32)
    }

    fn image_png() -> String {
        let img = image::RgbaImage::from_pixel(2, 1, image::Rgba([200, 30, 30, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        crate::cmd::base64_encode(out.get_ref())
    }

    fn state(s: &Session, id: deckcraft_model::ShapeId) -> (deckcraft_geom::Xfrm, [f64; 4]) {
        let sh = s.doc().unwrap().shape(id).cloned().unwrap();
        let deckcraft_model::ShapeKind::Picture { fill } = sh.kind else { panic!() };
        (sh.xfrm.unwrap(), fill.crop)
    }

    #[test]
    fn cropping_keeps_the_image_in_place_and_uncropping_brings_it_back() {
        let mut s = Session::with_new();
        let id = picture(&mut s);
        let (before, _) = state(&s, id);
        let full = crate::tools::picture_full_rect(before, [0.0; 4]);
        // Keep the left half.
        s.execute("picture.cropTo", &json!({"ids": [id.0], "rect": [100, 100, 200, 200]})).unwrap();
        let (b, c) = state(&s, id);
        assert!(b.w < before.w && c[2] > 0.4, "box {b:?} crop {c:?}");
        // The image itself has not moved or stretched.
        let still = crate::tools::picture_full_rect(b, c);
        assert!((still.x - full.x).abs() < 1e-6 && (still.w - full.w).abs() < 1e-6, "{still:?} vs {full:?}");
        // Dragging the edge back out (even past the image) restores it, never more.
        s.execute("picture.cropTo", &json!({"ids": [id.0], "rect": [0, 0, 960, 540]})).unwrap();
        let (b, c) = state(&s, id);
        assert!((b.w - before.w).abs() < 1e-6 && c.iter().all(|v| v.abs() < 1e-9), "{b:?} {c:?}");
        // One undo step per crop.
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(state(&s, id).0.w < before.w);
    }

    #[test]
    fn a_flipped_picture_crops_the_side_that_is_shown() {
        let mut s = Session::with_new();
        let id = picture(&mut s);
        s.execute("arrange.flipHorizontal", &json!({"ids": [id.0]})).unwrap();
        assert!(state(&s, id).0.flip_h);
        s.execute("picture.cropTo", &json!({"ids": [id.0], "rect": [100, 100, 200, 200]})).unwrap();
        let (_, c) = state(&s, id);
        // The left of the box shows the image's right side.
        assert!(c[0] > 0.4 && c[2].abs() < 1e-9, "{c:?}");
    }
}
