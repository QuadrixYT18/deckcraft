//! Assistant actions, offline and rule-based (the Copilot pane runs them): an agenda slide from
//! the deck's titles or sections, speaker notes drafted from a slide's own text, and a crowded
//! slide split over two. Each is one undo step.

use std::sync::Arc;

use deckcraft_model::text::TextBody;
use deckcraft_model::{PhType, Presentation, Shape, Slide};
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_slide, opt_usize_param};
use crate::{Result, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "assist.agenda",
            "Add Agenda Slide",
            [],
            None,
            "{} → {index}: an agenda after the first slide, from the sections (or the slide titles)",
            has_slide,
            agenda
        ),
        cmd!(
            "assist.notes",
            "Draft Speaker Notes",
            [],
            None,
            "{all?: bool} → {filled}: notes from each slide's own text where the notes are empty (the current slide, or every slide)",
            has_slide,
            notes
        ),
        cmd!(
            "assist.split",
            "Split Crowded Slide",
            [],
            None,
            "{index?} → {index}: a slide whose text box has too many lines becomes two slides",
            has_slide,
            split
        ),
    ]
}

/// Lines a text box keeps before `assist.split` moves the rest to a new slide.
const SPLIT_ABOVE: usize = 7;

fn is_title(sh: &Shape) -> bool {
    matches!(sh.ph_type(), Some(PhType::Title | PhType::CtrTitle))
}

/// The slide's title text, first line only.
pub fn title_of(slide: &Slide) -> Option<String> {
    slide
        .shapes
        .iter()
        .filter(|s| is_title(s))
        .find_map(|s| s.text.as_ref())
        .map(|t| t.text().lines().next().unwrap_or("").trim().to_string())
        .filter(|t| !t.is_empty())
}

fn agenda_items(p: &Presentation) -> Vec<String> {
    if !p.sections.is_empty() {
        return p.sections.iter().map(|s| s.name.trim().to_string()).filter(|n| !n.is_empty()).collect();
    }
    let mut items: Vec<String> = vec![];
    for s in p.slides.iter().skip(1) {
        if s.hidden {
            continue;
        }
        if let Some(t) = title_of(s)
            && !items.contains(&t)
            && !t.eq_ignore_ascii_case("agenda")
        {
            items.push(t);
        }
    }
    items
}

fn agenda(s: &mut Session, _p: &Value) -> Result<Value> {
    let items = agenda_items(&s.doc()?.doc);
    if items.is_empty() {
        return Err(bad("assist.agenda", "no slide titles or sections to list yet"));
    }
    let at = 1.min(s.doc()?.doc.slides.len());
    s.execute("slide.new", &json!({"layout": "titleAndContent", "at": at, "title": "Agenda", "body": items.join("\n")}))?;
    Ok(json!({"index": at, "items": items}))
}

/// Notes drafted from the slide: its title as the opening line, then each line of text.
fn draft_notes(slide: &Slide) -> Option<String> {
    let mut lines = vec![];
    if let Some(t) = title_of(slide) {
        lines.push(format!("{t}."));
    }
    let mut seen_text = false;
    deckcraft_model::walk(&slide.shapes, &mut |sh, _| {
        if is_title(sh) {
            return;
        }
        if let Some(body) = &sh.text {
            for para in &body.paragraphs {
                let t = para.text();
                let t = t.trim();
                if !t.is_empty() {
                    seen_text = true;
                    let end = if t.ends_with(['.', '!', '?', ':']) { "" } else { "." };
                    lines.push(format!("- {t}{end}"));
                }
            }
        }
    });
    (seen_text || !lines.is_empty()).then(|| lines.join("\n"))
}

fn notes(s: &mut Session, p: &Value) -> Result<Value> {
    let all = p.get("all").and_then(Value::as_bool).unwrap_or(false);
    let current = s.doc()?.selection.slide;
    let filled = s.edit(|doc, _| {
        let mut n = 0;
        for (i, slide) in doc.slides.iter_mut().enumerate() {
            if (!all && i != current) || !slide.notes.text().trim().is_empty() {
                continue;
            }
            if let Some(text) = draft_notes(slide) {
                Arc::make_mut(slide).notes = TextBody::from_text(&text);
                n += 1;
            }
        }
        Ok(n)
    })?;
    if filled == 0 {
        return Err(bad("assist.notes", "every slide already has notes, or there is no text to draft from"));
    }
    Ok(json!({"filled": filled}))
}

fn split(s: &mut Session, p: &Value) -> Result<Value> {
    let index = opt_usize_param(p, "index", "assist.split")?.unwrap_or(s.doc()?.selection.slide);
    let doc = s.doc()?.doc.clone();
    let slide = doc.slides.get(index).ok_or_else(|| bad("assist.split", "no such slide"))?;
    // The box with the most lines, if it has too many.
    let (pos, lines) = slide
        .shapes
        .iter()
        .enumerate()
        .filter(|(_, sh)| !is_title(sh))
        .filter_map(|(i, sh)| sh.text.as_ref().map(|t| (i, t.paragraphs.iter().filter(|p| !p.text().trim().is_empty()).count())))
        .max_by_key(|(_, n)| *n)
        .ok_or_else(|| bad("assist.split", "this slide has no text box"))?;
    if lines <= SPLIT_ABOVE {
        return Err(bad("assist.split", format!("the slide's longest text box has {lines} lines; nothing to split")));
    }
    let keep = lines.div_ceil(2);
    s.execute("slide.duplicate", &json!({"index": index}))?;
    s.edit(|doc, _| {
        let cut = |slide: &mut Arc<Slide>, first_half: bool| {
            let slide = Arc::make_mut(slide);
            if let Some(body) = slide.shapes.get_mut(pos).and_then(|sh| sh.text.as_mut()) {
                let mut k = 0usize;
                body.paragraphs.retain(|p| {
                    if p.text().trim().is_empty() {
                        return false;
                    }
                    k += 1;
                    if first_half { k <= keep } else { k > keep }
                });
            }
            if !first_half
                && let Some(title) = slide.shapes.iter_mut().find(|s| is_title(s)).and_then(|s| s.text.as_mut())
                && let Some(run) = title.paragraphs.last_mut().and_then(|p| p.runs.last_mut())
            {
                run.text.push_str(" (cont.)");
            }
        };
        if let Some(a) = doc.slides.get_mut(index) {
            cut(a, true);
        }
        if let Some(b) = doc.slides.get_mut(index + 1) {
            cut(b, false);
        }
        Ok(())
    })?;
    Ok(json!({"index": index + 1}))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;

    fn deck() -> Session {
        let mut s = Session::with_new();
        for t in ["Motivation", "Technik", "Ergebnisse"] {
            s.execute("slide.new", &json!({"title": t, "body": "Punkt eins\nPunkt zwei"})).unwrap();
        }
        s
    }

    #[test]
    fn agenda_lists_titles_after_the_first_slide() {
        let mut s = deck();
        let r = s.execute("assist.agenda", &json!({})).unwrap();
        assert_eq!(r["items"], json!(["Motivation", "Technik", "Ergebnisse"]));
        let d = s.doc().unwrap();
        assert_eq!(crate::cmd::assist::title_of(&d.doc.slides[1]).as_deref(), Some("Agenda"));
        // One undo step takes it back.
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.doc().unwrap().doc.slides.len(), 4);
    }

    #[test]
    fn notes_are_drafted_only_where_empty() {
        let mut s = deck();
        let r = s.execute("assist.notes", &json!({"all": true})).unwrap();
        assert!(r["filled"].as_u64().unwrap() >= 3);
        let notes = s.doc().unwrap().doc.slides[1].notes.text();
        assert!(notes.starts_with("Motivation.") && notes.contains("- Punkt eins."), "{notes}");
        assert!(s.execute("assist.notes", &json!({"all": true})).is_err(), "nothing left to fill");
    }

    #[test]
    fn a_crowded_slide_becomes_two() {
        let mut s = Session::with_new();
        let body = (1..=10).map(|i| format!("Punkt {i}")).collect::<Vec<_>>().join("\n");
        s.execute("slide.new", &json!({"title": "Viel", "body": body})).unwrap();
        let i = s.doc().unwrap().selection.slide;
        s.execute("assist.split", &json!({"index": i})).unwrap();
        let d = &s.doc().unwrap().doc;
        let text = |k: usize| d.slides[k].shapes.iter().filter_map(|x| x.text.as_ref()).map(|t| t.text()).collect::<Vec<_>>().join("|");
        assert!(text(i).contains("Punkt 5") && !text(i).contains("Punkt 6"), "{}", text(i));
        assert!(text(i + 1).contains("Punkt 6") && text(i + 1).contains("Viel (cont.)"), "{}", text(i + 1));
    }
}
