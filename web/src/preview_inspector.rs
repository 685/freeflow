use crate::{
    dom::{self, Result, id, items, string},
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

pub fn brightness(state: &State) {
    let value = match state.canvas.brightness.as_str() {
        "light" => "light",
        "dark" => "dark",
        _ => "system",
    };
    send(state, &format!("freeflow:brightness:{value}"));
}

fn highlight(bounds: &Value) {
    let overlay = id("widget-highlight");
    let values = (0..4)
        .map(|index| bounds[index].as_f64().filter(|value| value.is_finite()))
        .collect::<Option<Vec<_>>>();
    overlay.set_hidden(values.is_none());
    if let Some(values) = values {
        for (property, value) in ["left", "top", "width", "height"].into_iter().zip(values) {
            let _ = overlay
                .style()
                .set_property(property, &format!("{value}px"));
        }
    }
}

pub fn request_tree(state: &State) {
    if state.showing_rendered_widgets() && state.inspect_ready {
        let selected = if state.inspecting {
            ""
        } else {
            &state.rendered.selected
        };
        send(state, &format!("freeflow:tree:{selected}"));
    } else {
        highlight(&Value::Null);
    }
}

pub fn show_tree(show: bool) {
    state::change(|state| {
        state.pick_request += 1;
        state.rendered.show = show;
        state.rendered.selected.clear();
        highlight(&Value::Null);
        view::explorer(state);
        view::inspector(state);
        request_tree(state);
    });
}

pub async fn select_rendered(id: &str) -> Result {
    let context = state::change(|state| {
        let node = state
            .rendered
            .nodes
            .iter()
            .find(|node| node["id"] == id)?
            .clone();
        stop(state);
        controls(state);
        state.rendered.selected = id.into();
        state.widget.clear();
        state.pick_request += 1;
        request_tree(state);
        view::explorer(state);
        view::inspector(state);
        Some((
            state.project_id().to_owned(),
            state.generation,
            state.pick_request,
            node,
        ))
    });
    if let Some((project, generation, request, node)) = context {
        pick(project, generation, request, json!([node]), id.to_owned()).await?;
    }
    Ok(())
}

fn update_tree(message: &Value) {
    state::change(|state| {
        if !state.showing_rendered_widgets() {
            return;
        }
        let nodes = items(&message["nodes"]);
        if nodes.len() > 350 {
            return;
        }
        let truncated = message["truncated"] == true;
        if nodes != state.rendered.nodes || state.rendered.truncated != truncated {
            state.rendered.nodes = nodes.to_vec();
            state.rendered.truncated = truncated;
            if !state.rendered.selected.is_empty()
                && !nodes
                    .iter()
                    .any(|node| node["id"] == state.rendered.selected)
            {
                state.rendered.selected.clear();
                state.widget.clear();
                state.pick_request += 1;
                view::inspector(state);
            }
            view::explorer(state);
        }
        if !state.inspecting && message["selected"] == state.rendered.selected {
            highlight(&message["bounds"]);
        }
    });
}

pub fn reset(state: &mut State, keep_enabled: bool) {
    crate::preview_logs::reset();
    state.inspect_ready = false;
    state.inspecting &= keep_enabled;
    state.pick_request += 1;
    if !state.rendered.nodes.is_empty() {
        state.rendered.nodes.clear();
        state.rendered.selected.clear();
        state.rendered.collapsed.clear();
        state.rendered.truncated = false;
        if state.showing_rendered_widgets() {
            view::explorer(state);
            view::inspector(state);
        }
    }
    highlight(&Value::Null);
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
    crate::preview_canvas::render(state);
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
                crate::preview_canvas::stop_pan(state);
                state.inspecting = true;
                highlight(&Value::Null);
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
        let Some(data) = event
            .data()
            .as_string()
            .filter(|data| data.len() <= 1_000_000)
        else {
            return;
        };
        let Ok(message) = serde_json::from_str::<Value>(&data) else {
            return;
        };
        let fresh = state::read(|state| {
            message["frame"]
                == format!(
                    "{}-{}",
                    string(&state.preview["id"]),
                    state.preview["result"]["epoch"]
                        .as_u64()
                        .unwrap_or_default()
                )
        });
        if !fresh {
            return;
        }
        match string(&message["type"]) {
            "freeflow:inspector-ready" => {
                state::change(|state| {
                    if !state.inspect_ready {
                        state.inspect_ready = true;
                        brightness(state);
                        if state.inspecting {
                            send(state, "freeflow:inspect-on");
                        }
                        controls(state);
                        if state.showing_rendered_widgets() {
                            view::explorer(state);
                            request_tree(state);
                        }
                    }
                });
            }
            "freeflow:tree" => update_tree(&message),
            "freeflow:app-log" => crate::preview_logs::enqueue(&message),
            "freeflow:zoom" => crate::preview_canvas::wheel_from_preview(&message),
            "freeflow:brightness-error" => {
                dom::toast("Flutter could not change the preview brightness.", true)
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
                    dom::spawn(async move {
                        let selected = pick(
                            project,
                            generation,
                            request,
                            message["locations"].clone(),
                            string(&message["selected"]).to_owned(),
                        )
                        .await?;
                        if selected
                            && let (Some(x), Some(y)) =
                                (message["menu"][0].as_f64(), message["menu"][1].as_f64())
                        {
                            let frame = frame();
                            let rect = frame.get_bounding_client_rect();
                            let x = rect.left()
                                + x * rect.width() / f64::from(frame.client_width().max(1));
                            let y = rect.top()
                                + y * rect.height() / f64::from(frame.client_height().max(1));
                            crate::widget_clipboard::show(x as i32, y as i32);
                        }
                        Ok(())
                    });
                }
            }
            _ => {}
        }
    });
}

async fn pick(
    project: String,
    generation: u64,
    request: u64,
    locations: Value,
    rendered_id: String,
) -> Result<bool> {
    let hit = dom::project_post(&project, "inspect", json!({"locations":locations})).await?;
    let current = |state: &State| state.generation == generation && state.pick_request == request;
    if !state::read(current) {
        return Ok(false);
    }
    let Some(path) = hit["path"].as_str() else {
        dom::toast("This widget has no source location in your project.", false);
        return Ok(false);
    };
    let widget = string(&hit["widget"]);
    let (file, mode, stale) = state::read(|state| {
        (
            state.file.clone(),
            state.mode,
            state.visual["revision"] != hit["revision"],
        )
    });
    let next_mode = if widget.is_empty() { Mode::Split } else { mode };
    if file != path || mode != next_mode || stale {
        crate::select(path.to_owned(), next_mode).await?;
    }
    let (selected, row) = state::change(|state| {
        if !current(state) || state.file != path {
            return (false, None);
        }
        state.widgets = true;
        state.widget.clear();
        let found = state.visual["revision"] == hit["revision"] && state.node(widget).is_some();
        if found {
            state.widget = widget.to_owned();
            state.reveal_source_widget();
        }
        let selector = if state.showing_rendered_widgets()
            && state
                .rendered
                .nodes
                .iter()
                .any(|node| node["id"] == rendered_id)
        {
            state.rendered.selected = rendered_id.to_owned();
            let mut parent = state
                .rendered
                .nodes
                .iter()
                .find(|node| node["id"] == rendered_id)
                .and_then(|node| node["parent"].as_str());
            for _ in 0..120 {
                let Some(id) = parent else { break };
                state.rendered.collapsed.remove(id);
                parent = state
                    .rendered
                    .nodes
                    .iter()
                    .find(|node| node["id"] == id)
                    .and_then(|node| node["parent"].as_str());
            }
            Some(format!(r#"[data-rendered-widget="{rendered_id}"]"#))
        } else {
            if state.showing_rendered_widgets() {
                state.rendered.show = false;
            }
            found.then(|| format!(r#"[data-widget="{widget}"]"#))
        };
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
        request_tree(state);
        (found, selector)
    });
    if let Some(row) = row.and_then(|selector| dom::find(&selector)) {
        row.scroll_into_view_with_bool(false);
    }
    Ok(selected)
}
