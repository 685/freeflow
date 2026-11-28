use crate::{
    dom::{self, Result, escape as h, hidden, html, id, items, project_post, string},
    state,
};
use gloo_timers::callback::Timeout;
use serde_json::{Value, json};
use std::cell::RefCell;
use wasm_bindgen::JsCast;
use web_sys::{HtmlTextAreaElement, KeyboardEvent};

pub(crate) mod actions;

#[derive(Clone)]
struct Document {
    project: String,
    path: String,
    generation: u64,
    text: String,
    start: u32,
    end: u32,
}

impl Document {
    fn current() -> Option<Self> {
        state::read(|state| {
            if !state.mode.shows_code() || !state.file.ends_with(".dart") || state.source.is_null()
            {
                return None;
            }
            let field = input();
            Some(Self {
                project: state.project_id().into(),
                path: state.file.clone(),
                generation: state.generation,
                text: field.value(),
                start: field.selection_start().ok().flatten()?,
                end: field.selection_end().ok().flatten()?,
            })
        })
    }

    fn still_current(&self, cursor: bool) -> bool {
        Self::current().is_some_and(|next| {
            self.project == next.project
                && self.generation == next.generation
                && self.path == next.path
                && self.text == next.text
                && (!cursor || (self.start, self.end) == (next.start, next.end))
        })
    }

    async fn request(&self, action: &str) -> Result<Value> {
        project_post(
            &self.project,
            "editor",
            json!({
                "action":action,"path":self.path,"text":self.text,"offset":self.start,
            }),
        )
        .await
    }
}

struct Suggestions {
    document: Document,
    values: Vec<Value>,
    active: usize,
}

#[derive(Default)]
struct Editor {
    suggestions: Option<Suggestions>,
    popup_request: u64,
    problems: bool,
    problems_request: u64,
    diagnostics: Vec<Value>,
    timer: Option<Timeout>,
}

thread_local! { static EDITOR: RefCell<Editor> = RefCell::new(Editor::default()); }

fn input() -> HtmlTextAreaElement {
    dom::find("#editor textarea")
        .expect("code input")
        .unchecked_into()
}

fn hide_popup() {
    EDITOR.with_borrow_mut(|editor| {
        editor.popup_request += 1;
        editor.suggestions = None;
    });
    hidden("dart-popup", true);
}

pub fn changed() {
    hide_popup();
    if EDITOR.with_borrow(|editor| editor.problems) {
        render_problems(&[], "Checking Dart…");
        schedule_problems(500);
    }
}

pub fn reset() {
    hide_popup();
    if EDITOR.with_borrow(|editor| editor.problems) {
        set_problems(false);
    }
}

pub fn source_ready(dart: bool) {
    hide_popup();
    if EDITOR.with_borrow(|editor| editor.problems) {
        if dart {
            render_problems(&[], "Checking Dart…");
            schedule_problems(250);
        } else {
            set_problems(false);
        }
    }
}

fn popup(text: &str) {
    html("dart-popup", format!("<p>{}</p>", h(text)));
    hidden("dart-popup", false);
}

async fn assist(action: &str) -> Result {
    let Some(document) = Document::current() else {
        return Ok(());
    };
    hide_popup();
    let request = EDITOR.with_borrow(|editor| editor.popup_request);
    popup("Checking Dart…");
    let result = document.request(action).await;
    if !document.still_current(true)
        || !EDITOR.with_borrow(|editor| editor.popup_request == request)
    {
        return Ok(());
    }
    let result = match result {
        Ok(value) => value,
        Err(error) => {
            popup(&error.message);
            return Ok(());
        }
    };
    match action {
        "complete" => {
            let values = items(&result["items"]);
            if values.is_empty() {
                popup("No suggestions here.");
            } else {
                EDITOR.with_borrow_mut(|editor| {
                    editor.suggestions = Some(Suggestions {
                        document,
                        values: values.to_vec(),
                        active: 0,
                    })
                });
                render_suggestions();
            }
        }
        "definition" => {
            if let Some(found) = items(&result["locations"]).first() {
                hide_popup();
                let path = string(&found["path"]);
                if path != document.path {
                    crate::select(path.to_owned(), crate::code_mode()).await?;
                }
                if state::read(|state| state.project_id() == document.project && state.file == path)
                {
                    crate::editor::go_to(
                        found["line"].as_u64().unwrap_or(1),
                        found["offset"].as_u64().unwrap_or(0) as u32,
                    );
                }
            } else {
                popup("No definition in this project.");
            }
        }
        _ => {
            if let Some(text) = result["text"].as_str() {
                html("dart-popup", format!("<pre>{}</pre>", h(text)));
            } else {
                popup("No type information here.");
            }
        }
    }
    Ok(())
}

fn render_suggestions() {
    let markup = EDITOR.with_borrow(|editor| {
        let Some(list) = &editor.suggestions else { return String::new(); };
        list.values.iter().enumerate().map(|(index,item)| format!(
            r#"<button type="button" tabindex="-1" class="dart-suggestion{}" data-suggestion="{index}"><span>{}</span><small class="ellipsis">{}</small></button>"#,
            if index == list.active { " active" } else { "" },h(string(&item["label"])),h(string(&item["detail"])),
        )).collect::<String>()
    });
    html("dart-popup", markup);
    if let Some(item) = dom::find(".dart-suggestion.active") {
        item.scroll_into_view_with_bool(false);
    }
}

fn byte_index(text: &str, offset: u32) -> Option<usize> {
    let mut remaining = offset as usize;
    for (byte, ch) in text.char_indices() {
        if remaining == 0 {
            return Some(byte);
        }
        remaining = remaining.checked_sub(ch.len_utf16())?;
    }
    (remaining == 0).then_some(text.len())
}

fn position_offset(text: &str, value: &Value) -> Option<u32> {
    let line = usize::try_from(value["line"].as_u64()?).ok()?;
    let column = usize::try_from(value["character"].as_u64()?).ok()?;
    let mut lines = text.split('\n');
    let prefix: usize = lines
        .by_ref()
        .take(line)
        .map(|line| line.encode_utf16().count() + 1)
        .sum();
    let len = lines.next()?.trim_end_matches('\r').encode_utf16().count();
    (column <= len).then(|| (prefix + column) as u32)
}

fn range(text: &str, value: &Value) -> Option<(u32, u32)> {
    let start = position_offset(text, &value["start"])?;
    let end = position_offset(text, &value["end"])?;
    (start <= end).then_some((start, end))
}

fn accept(index: usize) {
    let selected = EDITOR.with_borrow(|editor| {
        let list = editor.suggestions.as_ref()?;
        Some((list.document.clone(), list.values.get(index)?.clone()))
    });
    hide_popup();
    let Some((document, item)) = selected else {
        return;
    };
    if !document.still_current(true) {
        return;
    }
    let source = &document.text;
    let text = string(&item["text"]);
    let fallback = || {
        if document.start != document.end {
            return (document.start, document.end);
        }
        let cursor = byte_index(source, document.start).unwrap_or_default();
        let prefix = source[..cursor]
            .rsplit(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
            .next()
            .unwrap_or_default();
        (
            document
                .start
                .saturating_sub(prefix.encode_utf16().count() as u32),
            document.end,
        )
    };
    let (start, end) = range(source, &item["range"]).unwrap_or_else(fallback);
    let mut edits = vec![(start, end, text, true)];
    for additional in items(&item["additional"]) {
        let Some((start, end)) = range(source, &additional["range"]) else {
            return;
        };
        edits.push((start, end, string(&additional["text"]), false));
    }
    if let Some((updated, caret)) = apply_ranges(source, edits) {
        let _ = input().focus();
        crate::editor::apply_edit(&updated, caret);
    }
}

fn apply_ranges(source: &str, mut edits: Vec<(u32, u32, &str, bool)>) -> Option<(String, u32)> {
    edits.sort_by_key(|(start, end, _, _)| (*start, *end));
    if edits.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return None;
    }
    let mut result = String::with_capacity(source.len());
    let mut previous = 0;
    let mut position = 0;
    for (start, end, text, main) in edits {
        let (first, last) = byte_index(source, start).zip(byte_index(source, end))?;
        result.push_str(&source[previous..first]);
        result.push_str(text);
        if main {
            position = result.encode_utf16().count() as u32;
        }
        previous = last;
    }
    result.push_str(&source[previous..]);
    (result.len() <= 2 * 1024 * 1024).then_some((result, position))
}

fn render_problems(items: &[Value], caption: &str) {
    let count = items.len();
    dom::text(
        "dart-problems",
        &if count == 0 {
            "Problems".into()
        } else {
            format!("Problems · {count}")
        },
    );
    let content = if count == 0 {
        format!("<p>{}</p>", h(caption))
    } else {
        items.iter().enumerate().map(|(index,item)| {
            let (class,label) = match item["severity"].as_u64() {
                Some(1) => ("error","Error"),
                Some(2) => ("warning","Warning"),
                _ => ("info","Info"),
            };
            let line = item["range"]["start"]["line"].as_u64().unwrap_or(0)+1;
            format!(r#"<div class="dart-problem-row"><button type="button" class="dart-problem {class}" data-problem="{index}"><small>{label} · Line {line}</small><span>{}</span></button><button class="button soft small" type="button" data-dart-fix="{index}" aria-label="Quick fixes for problem on line {line}">Fix</button></div>"#,h(string(&item["message"])))
        }).collect()
    };
    html("dart-problem-list", content);
}

fn schedule_problems(delay: u32) {
    EDITOR.with_borrow_mut(|editor| {
        editor.problems_request += 1;
        editor.diagnostics.clear();
        let request = editor.problems_request;
        editor.timer = Some(Timeout::new(delay, move || {
            dom::spawn(check_problems(request))
        }));
    });
}

async fn check_problems(request: u64) -> Result {
    let Some(document) = Document::current() else {
        return Ok(());
    };
    let result = document.request("diagnose").await;
    if !document.still_current(false) {
        return Ok(());
    }
    EDITOR.with_borrow_mut(|editor| {
        if !editor.problems || editor.problems_request != request {
            return;
        }
        match result {
            Ok(result) => {
                editor.diagnostics = items(&result["diagnostics"]).to_vec();
                render_problems(&editor.diagnostics, "Dart found no problems in this file.");
            }
            Err(error) => render_problems(&[], &error.message),
        }
    });
    Ok(())
}

fn set_problems(open: bool) {
    EDITOR.with_borrow_mut(|editor| {
        editor.problems = open;
        if !open {
            editor.timer = None;
            editor.diagnostics.clear();
        }
    });
    hidden("dart-problem-list", !open);
    dom::attr(
        &id("dart-problems"),
        "aria-expanded",
        if open { "true" } else { "false" },
    );
    if open {
        render_problems(&[], "Checking Dart…");
        schedule_problems(0);
    } else {
        dom::text("dart-problems", "Problems");
    }
}

pub fn key(key: &KeyboardEvent) -> bool {
    if key.is_composing() {
        return false;
    }
    let symbol = key.key();
    let pending = EDITOR.with_borrow(|editor| editor.suggestions.is_some());
    if (key.ctrl_key() || key.meta_key()) && symbol == " " {
        key.prevent_default();
        dom::spawn(assist("complete"));
    } else if (key.ctrl_key() || key.meta_key()) && symbol == "." && Document::current().is_some() {
        key.prevent_default();
        dom::spawn(actions::open(None));
    } else if symbol == "F12"
        && state::read(|state| state.mode.shows_code() && state.file.ends_with(".dart"))
    {
        key.prevent_default();
        dom::spawn(assist("definition"));
    } else if pending && matches!(symbol.as_str(), "ArrowDown" | "ArrowUp") {
        key.prevent_default();
        EDITOR.with_borrow_mut(|editor| {
            if let Some(list) = &mut editor.suggestions {
                let count = list.values.len();
                list.active =
                    (list.active + if symbol == "ArrowDown" { 1 } else { count - 1 }) % count;
            }
        });
        render_suggestions();
    } else if pending && !key.shift_key() && matches!(symbol.as_str(), "Enter" | "Tab") {
        key.prevent_default();
        let index =
            EDITOR.with_borrow(|editor| editor.suggestions.as_ref().map_or(0, |list| list.active));
        accept(index);
    } else if symbol == "Escape" && !id("dart-popup").hidden() {
        key.prevent_default();
        hide_popup();
    } else {
        return false;
    }
    true
}

pub fn initialize() {
    dom::click("dart-actions", || dom::spawn(actions::open(None)));
    for (button, action) in [
        ("dart-complete", "complete"),
        ("dart-definition", "definition"),
        ("dart-type", "hover"),
    ] {
        dom::click(button, move || {
            let _ = input().focus();
            dom::spawn(assist(action));
        });
    }
    dom::click("dart-problems", || {
        set_problems(!EDITOR.with_borrow(|editor| editor.problems))
    });
    dom::listen(&id("dart-popup"), "mousedown", |event| {
        event.prevent_default()
    });
    dom::listen(&id("dart-popup"), "click", |event| {
        if let Some(index) = dom::closest(&event, "[data-suggestion]")
            .and_then(|button| button.get_attribute("data-suggestion"))
            .and_then(|index| index.parse::<usize>().ok())
        {
            accept(index);
        }
    });
    dom::listen(&input(), "click", |_| hide_popup());
    dom::listen(&id("dart-problem-list"), "click", |event| {
        if let Some(index) = dom::closest(&event, "[data-dart-fix]")
            .and_then(|button| button.get_attribute("data-dart-fix"))
            .and_then(|index| index.parse::<usize>().ok())
            && let Some(problem) =
                EDITOR.with_borrow(|editor| editor.diagnostics.get(index).cloned())
        {
            dom::spawn(actions::open(Some(problem["range"].clone())));
            return;
        }
        let index = dom::closest(&event, "[data-problem]")
            .and_then(|button| button.get_attribute("data-problem"))
            .and_then(|index| index.parse::<usize>().ok());
        let problem = EDITOR
            .with_borrow(|editor| index.and_then(|index| editor.diagnostics.get(index).cloned()));
        if let Some((problem, document)) = problem.zip(Document::current()) {
            let start = &problem["range"]["start"];
            if let Some(offset) = position_offset(&document.text, start) {
                crate::editor::go_to(start["line"].as_u64().unwrap_or(0) + 1, offset);
            }
        }
    });
}
