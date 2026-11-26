//! Presentation check: offline, rule-based findings about text and design, each with the slide
//! (and shape) it concerns and, where one exists, the command that fixes it. The Copilot pane
//! shows them; a language model is not involved.

use deckcraft_model::{PhType, Presentation, Shape, ShapeKind, Slide};
use serde_json::{Value, json};

use super::{CommandSpec, cmd, has_doc};
use crate::{Result, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        query "review.analyze",
        "Check Presentation",
        [],
        None,
        "{} → {findings: [{slide, shape?, severity: info|warning, rule, message, args (the values in the message), fix?: [{command, params}] (run in order)}], stats: {slides, words, fonts}}",
        has_doc,
        analyze_cmd
    )]
}

/// More words than this on one slide is hard to read while someone is talking.
const MAX_WORDS: usize = 70;
/// More bullet lines than this in one text box.
const MAX_LINES: usize = 7;
/// Text set smaller than this (points) is hard to read from the back of a room.
const MIN_SIZE: f64 = 14.0;
/// More font families than this across the deck looks unplanned.
const MAX_FONTS: usize = 3;
/// A title longer than this many words.
const MAX_TITLE_WORDS: usize = 12;

fn words(s: &str) -> usize {
    s.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric)).count()
}

fn is_title(sh: &Shape) -> bool {
    matches!(sh.ph_type(), Some(PhType::Title | PhType::CtrTitle))
}

/// Every shape on the slide, groups opened.
fn all_shapes(slide: &Slide) -> Vec<&Shape> {
    let mut out = vec![];
    deckcraft_model::walk(&slide.shapes, &mut |sh, _| out.push(sh));
    out
}

fn text_of(sh: &Shape) -> String {
    match &sh.kind {
        ShapeKind::Table(t) => t.rows.iter().flat_map(|r| r.cells.iter().map(|c| c.text.text())).collect::<Vec<_>>().join(" "),
        _ => sh.text.as_ref().map(|t| t.text()).unwrap_or_default(),
    }
}

pub fn analyze(p: &Presentation) -> Value {
    let mut findings: Vec<Value> = vec![];
    let mut fonts = std::collections::BTreeSet::new();
    let mut total_words = 0usize;
    for (i, slide) in p.slides.iter().enumerate() {
        let n = i + 1;
        let shapes = all_shapes(slide);
        let mut slide_words = 0usize;
        let mut has_title_text = false;
        for sh in &shapes {
            let text = text_of(sh);
            let w = words(&text);
            slide_words += w;
            if is_title(sh) {
                if w > 0 {
                    has_title_text = true;
                }
                if w > MAX_TITLE_WORDS {
                    findings.push(
                        json!({"slide": n, "shape": sh.id.0, "severity": "info", "rule": "longTitle", "args": {"words": w, "max": MAX_TITLE_WORDS},
                        "message": format!("The title has {w} words. Titles of up to {MAX_TITLE_WORDS} words are read at a glance.")}),
                    );
                }
            }
            if let Some(body) = &sh.text {
                let lines = body.paragraphs.iter().filter(|p| !p.text().trim().is_empty()).count();
                if lines > MAX_LINES && !is_title(sh) {
                    findings.push(json!({"slide": n, "shape": sh.id.0, "severity": "warning", "rule": "manyLines", "args": {"name": sh.name, "lines": lines, "max": MAX_LINES},
                        "message": format!("\"{}\" has {lines} lines. Up to {MAX_LINES} bullets per box keep it readable; split it over two slides.", sh.name)}));
                }
                let mut smallest: Option<f64> = None;
                for para in &body.paragraphs {
                    for run in &para.runs {
                        if let Some(f) = &run.props.font {
                            fonts.insert(f.clone());
                        }
                        if let Some(sz) = run.props.size.filter(|s| s.is_finite() && *s > 0.0)
                            && !run.text.trim().is_empty()
                        {
                            smallest = Some(smallest.map_or(sz, |m: f64| m.min(sz)));
                        }
                    }
                }
                if let Some(sz) = smallest.filter(|s| *s < MIN_SIZE) {
                    findings.push(json!({"slide": n, "shape": sh.id.0, "severity": "warning", "rule": "smallText", "args": {"name": sh.name, "size": sz.round() as i64, "min": MIN_SIZE as i64},
                    "message": format!("\"{}\" uses {sz:.0} pt text. From {MIN_SIZE:.0} pt it can be read from the back of a room.", sh.name),
                    "fix": [
                        {"command": "slide.go", "params": {"index": i}},
                        {"command": "edit.select", "params": {"ids": [sh.id.0]}},
                        {"command": "format.size", "params": {"size": MIN_SIZE}},
                    ]}));
                }
            }
            // An empty placeholder shows its prompt while editing and nothing in the show.
            if sh.is_placeholder() && w == 0 && matches!(sh.ph_type(), Some(PhType::Body | PhType::SubTitle | PhType::Obj)) && sh.text.is_some() {
                findings.push(json!({"slide": n, "shape": sh.id.0, "severity": "info", "rule": "emptyPlaceholder", "args": {"name": sh.name},
                "message": format!("\"{}\" is empty. Fill it or delete it.", sh.name),
                "fix": [
                    {"command": "slide.go", "params": {"index": i}},
                    {"command": "edit.delete", "params": {"ids": [sh.id.0]}},
                ]}));
            }
        }
        total_words += slide_words;
        if slide_words > MAX_WORDS {
            findings.push(json!({"slide": n, "severity": "warning", "rule": "wordy", "args": {"slide": n, "words": slide_words, "max": MAX_WORDS},
                "message": format!("Slide {n} has {slide_words} words. Around {MAX_WORDS} or fewer keeps the audience listening rather than reading.")}));
        }
        if !has_title_text && !slide.hidden {
            findings.push(json!({"slide": n, "severity": "info", "rule": "noTitle", "args": {"slide": n},
                "message": format!("Slide {n} has no title. A title helps the audience and screen readers follow along.")}));
        }
        if slide.notes.text().trim().is_empty() && slide_words > 0 {
            findings.push(json!({"slide": n, "severity": "info", "rule": "noNotes", "args": {"slide": n},
                "message": format!("Slide {n} has no speaker notes.")}));
        }
    }
    if fonts.len() > MAX_FONTS {
        findings.push(json!({"slide": null, "severity": "warning", "rule": "manyFonts", "args": {"count": fonts.len(), "fonts": fonts.iter().cloned().collect::<Vec<_>>().join(", ")},
            "message": format!("The presentation uses {} fonts ({}). Two or three look calmer.", fonts.len(), fonts.iter().cloned().collect::<Vec<_>>().join(", "))}));
    }
    // Long decks without sections are hard to navigate.
    if p.slides.len() > 15 && p.sections.is_empty() {
        findings.push(json!({"slide": null, "severity": "info", "rule": "noSections", "args": {"slides": p.slides.len()},
            "message": format!("{} slides and no sections. Sections (Home › Section) group the slide list.", p.slides.len())}));
    }
    json!({
        "findings": findings,
        "stats": {"slides": p.slides.len(), "words": total_words, "fonts": fonts.into_iter().collect::<Vec<_>>()},
    })
}

fn analyze_cmd(s: &mut Session, _p: &Value) -> Result<Value> {
    Ok(analyze(&s.doc()?.doc))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;

    #[test]
    fn finds_wordy_slides_small_text_and_fixes_them() {
        let mut s = Session::with_new();
        let long = "word ".repeat(90);
        let r = s.execute("shape.insert", &json!({"preset": "rect", "rect": [10, 10, 400, 300], "text": long})).unwrap();
        let id = r["id"].as_u64().unwrap();
        s.execute("edit.select", &json!({"ids": [id]})).unwrap();
        s.execute("format.size", &json!({"size": 9})).unwrap();
        let a = s.execute("review.analyze", &json!({})).unwrap();
        let rules: Vec<&str> = a["findings"].as_array().unwrap().iter().filter_map(|f| f["rule"].as_str()).collect();
        assert!(rules.contains(&"wordy"), "{rules:?}");
        assert!(rules.contains(&"smallText"), "{rules:?}");
        // The suggested fix runs and removes the finding.
        let fix = a["findings"].as_array().unwrap().iter().find(|f| f["rule"] == "smallText").unwrap()["fix"].clone();
        for step in fix.as_array().unwrap() {
            s.execute(step["command"].as_str().unwrap(), &step["params"]).unwrap();
        }
        let a = s.execute("review.analyze", &json!({})).unwrap();
        assert!(!a["findings"].as_array().unwrap().iter().any(|f| f["rule"] == "smallText"));
    }
}
