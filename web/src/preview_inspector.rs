use crate::{
    dom::{self, Result, id, string},
    state::{self, Mode, State},
    view,
};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{HtmlIFrameElement, MessageEvent, Url, Window};

fn frame() -> HtmlIFrameElement {
    id("preview-frame").unchecked_into()
}

fn connection(state: &State) -> Option<(Window, String)> {
    if state.preview["result"]["inspector"] != true || state.preview["status"] != "ready" {
        return None;
    }
    let url = Url::new(state.preview["result"]["url"].as_str()?).ok()?;
    Some((frame().content_window()?, url.origin()))
}

fn send(state: &State, message: &str) {
    if let Some((window, origin)) = connection(state) {
        let _ = window.post_message(&JsValue::from_str(message), &origin);
    }
}

pub fn reset(state: &mut State, keep_enabled: bool) {
    state.inspect_ready = false;
    state.inspecting &= keep_enabled;
    state.pick_request += 1;
}

pub fn stop(state: &mut State) {
    if state.inspecting {
        send(state, "freeflow:inspect-off");
        state.inspecting = false;
        state.pick_request += 1;
    }
}

pub fn controls(state: &State) {
    let button = id("inspect-preview");
    let title = if state.inspecting {
        "Stop picking widgets"
    } else if !state.inspect_ready && state.preview["status"] == "ready" {
        "Widget picking requires a compatible Flutter debug preview"
    } else {
        "Pick widget in preview"
    };
    dom::disabled("inspect-preview", !state.inspect_ready);
    dom::attr(&button, "title", title);
    dom::attr(
        &button,
        "aria-pressed",
        if state.inspecting { "true" } else { "false" },
    );
}

pub fn initialize() {
    dom::click("inspect-preview", || {
        state::change(|state| {
            if !state.inspect_ready {
                return;
            }
            if state.inspecting {
                stop(state);
            } else {
                state.inspecting = true;
                send(state, "freeflow:inspect-on");
            }
            controls(state);
        });
    });
    dom::listen(&frame(), "load", |_| {
        state::read(|state| send(state, "freeflow:hello"));
    });
    dom::listen(&dom::win(), "message", |event| {
        let Ok(event) = event.dyn_into::<MessageEvent>() else {
            return;
        };
        let trusted = state::read(|state| {
            connection(state).is_some_and(|(window, origin)| {
                event.origin() == origin
                    && event
                        .source()
                        .is_some_and(|source| js_sys::Object::is(source.as_ref(), window.as_ref()))
            })
        });
        if !trusted {
            return;
        }
        let Some(data) = event.data().as_string().filter(|data| data.len() <= 64_000) else {
            return;
        };
        let Ok(message) = serde_json::from_str::<Value>(&data) else {
            return;
        };
        match string(&message["type"]) {
            "freeflow:inspector-ready" => {
                state::change(|state| {
                    if !state.inspect_ready {
                        state.inspect_ready = true;
                        if state.inspecting {
                            send(state, "freeflow:inspect-on");
                        }
                        controls(state);
                    }
                });
            }
            "freeflow:selection" => {
                let context = state::change(|state| {
                    if !state.inspecting || !state.mode.shows_preview() {
                        return None;
                    }
                    state.pick_request += 1;
                    Some((
                        state.project_id().to_owned(),
                        state.generation,
                        state.pick_request,
                    ))
                });
                if let Some((project, generation, request)) = context {
                    dom::spawn(pick(
                        project,
                        generation,
                        request,
                        message["locations"].clone(),
                    ));
                }
            }
            _ => {}
        }
    });
}

async fn pick(project: String, generation: u64, request: u64, locations: Value) -> Result {
    let hit = dom::project_post(&project, "inspect", json!({"locations":locations})).await?;
    let current = |state: &State| state.generation == generation && state.pick_request == request;
    if !state::read(current) {
        return Ok(());
    }
    let Some(path) = hit["path"].as_str() else {
        dom::toast("This widget has no source location in your project.", false);
        return Ok(());
    };
    let widget = string(&hit["widget"]);
    let (file, mode) = state::read(|state| (state.file.clone(), state.mode));
    let next_mode = if widget.is_empty() { Mode::Split } else { mode };
    if file != path || mode != next_mode {
        crate::select(path.to_owned(), next_mode).await?;
    }
    let row = state::change(|state| {
        if !current(state) || state.file != path {
            return None;
        }
        state.widgets = true;
        state.widget.clear();
        let found = state.visual["revision"] == hit["revision"] && state.node(widget).is_some();
        if found {
            state.widget = widget.to_owned();
            let mut parent = state
                .node(widget)
                .and_then(|node| node["parent"].as_str())
                .map(str::to_owned);
            while let Some(id) = parent {
                state.collapsed.remove(&id);
                parent = state
                    .node(&id)
                    .and_then(|node| node["parent"].as_str())
                    .map(str::to_owned);
            }
        }
        view::explorer(state);
        view::inspector(state);
        if state.mode.shows_code()
            && state.source["revision"] == hit["revision"]
            && !state.drafts.contains_key(&state.draft_key(path))
        {
            crate::editor::go_to(
                hit["line"].as_u64().unwrap_or(1),
                hit["position"].as_u64().unwrap_or_default() as u32,
            );
        }
        found.then(|| format!(r#"[data-widget="{widget}"]"#))
    });
    if let Some(row) = row.and_then(|selector| dom::find(&selector)) {
        row.scroll_into_view_with_bool(false);
    }
    Ok(())
}
