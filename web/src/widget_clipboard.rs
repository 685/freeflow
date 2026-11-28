use crate::{
    dom::{self, Error, Result, closest, id, items, string},
    state, view,
};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{HtmlElement, KeyboardEvent, MouseEvent};

pub fn controls(node: &Value, menu: bool) -> String {
    let mut output = if menu {
        String::new()
    } else {
        "<div class=\"widget-clipboard\">".into()
    };
    let paste = !items(&node["slots"]).is_empty() || node["movable"] == true;
    for (action, label, shortcut, enabled) in [
        ("copy", "Copy", "C", true),
        ("cut", "Cut", "X", node["removable"] == true),
        ("paste", "Paste", "V", paste),
        ("duplicate", "Duplicate", "D", node["movable"] == true),
    ] {
        output.push_str(&format!(r#"<button type="button" class="button soft small" data-widget-clipboard="{action}" title="{label} widget (Ctrl/Cmd+{shortcut})"{}{}>{label}{}</button>"#,
            if menu { " role=\"menuitem\"" } else { "" },
            if enabled { "" } else { " disabled" },
            if menu { format!("<span>Ctrl/⌘+{shortcut}</span>") } else { String::new() }));
    }
    if menu {
        output.push_str(r#"<button type="button" class="button soft small" role="menuitem" data-widget-prompt-edit>Prompt Edit</button>"#);
        output.push_str(&crate::component_panel::controls(node, true));
        if node["libraryChildren"] == true {
            output.push_str(r#"<button type="button" class="button soft small" role="menuitem" data-widget-library="insert">Add widget</button>"#);
        }
        if node["libraryWrappable"] != false {
            output.push_str(r#"<button type="button" class="button soft small" role="menuitem" data-widget-library="wrap">Wrap widget</button>"#);
        }
    } else {
        output.push_str("</div>");
    }
    output
}

pub fn focus() {
    let selector = state::read(|state| {
        if state.showing_rendered_widgets() {
            format!("[data-rendered-widget=\"{}\"]", state.rendered.selected)
        } else {
            format!("[data-widget=\"{}\"]", state.widget)
        }
    });
    if let Some(row) = dom::find(&selector).and_then(|row| row.dyn_into::<HtmlElement>().ok()) {
        let _ = row.focus();
    }
}

async fn perform(action: String) -> Result {
    let (project, path, revision) = crate::visual_context()?;
    let node = state::read(|state| state.widget().cloned())
        .ok_or_else(|| Error::new("Select a Dart widget first."))?;
    let widget = node["id"].clone();
    let slot = dom::find("#insert-slot")
        .map(|select| dom::value(&select))
        .unwrap_or_default();
    let clipboard = dom::win().navigator().clipboard();
    let command = match action.as_str() {
        "copy" | "cut" => {
            let snippet = dom::project_post(
                &project,
                "widget-snippet",
                json!({"path":path,"revision":revision,"id":widget}),
            )
            .await?;
            JsFuture::from(clipboard.write_text(string(&snippet["code"])))
                .await
                .map_err(|_| {
                    Error::new("Allow clipboard access in your browser to copy widgets.")
                })?;
            if action == "copy" {
                dom::toast("Widget copied", false);
                return Ok(());
            }
            json!({"id":widget,"operation":"remove"})
        }
        "paste" => {
            let text = JsFuture::from(clipboard.read_text())
                .await
                .map_err(|_| {
                    Error::new("Allow clipboard access in your browser to paste widgets.")
                })?
                .as_string()
                .unwrap_or_default();
            json!({"id":widget,"operation":"paste","code":text,"slot":slot})
        }
        "duplicate" => json!({"id":widget,"operation":"duplicate"}),
        _ => return Ok(()),
    };
    crate::visual_edit_at(project, path, revision, command).await?;
    focus();
    dom::toast(
        if action == "cut" {
            "Widget cut"
        } else if action == "paste" {
            "Widget pasted"
        } else {
            "Widget duplicated"
        },
        false,
    );
    Ok(())
}

fn close() {
    id("widget-menu").set_hidden(true);
}

pub(crate) fn show(x: i32, y: i32) {
    let Some(node) = state::read(|state| state.widget().cloned()) else {
        return;
    };
    let menu = id("widget-menu");
    menu.set_inner_html(&controls(&node, true));
    menu.set_hidden(false);
    let width = dom::win()
        .inner_width()
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(1024.0);
    let height = dom::win()
        .inner_height()
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(768.0);
    let _ = menu.style().set_property(
        "left",
        &format!(
            "{}px",
            f64::from(x)
                .min(width - f64::from(menu.offset_width()) - 8.0)
                .max(8.0)
        ),
    );
    let _ = menu.style().set_property(
        "top",
        &format!(
            "{}px",
            f64::from(y)
                .min(height - f64::from(menu.offset_height()) - 8.0)
                .max(8.0)
        ),
    );
    if let Some(first) = menu.query_selector("button:enabled").ok().flatten() {
        let _ = first.unchecked_into::<HtmlElement>().focus();
    }
}

pub fn initialize() {
    dom::listen(&dom::doc(), "click", |event| {
        let action = closest(&event, "[data-widget-clipboard]")
            .and_then(|button| button.get_attribute("data-widget-clipboard"));
        let library = closest(&event, "#widget-menu [data-widget-library]")
            .and_then(|button| button.get_attribute("data-widget-library"));
        let prompt = closest(&event, "#widget-menu [data-widget-prompt-edit]").is_some();
        close();
        if let Some(action) = action {
            dom::spawn(perform(action));
        } else if let Some(library) = library {
            use crate::widget_library::{Operation, open};
            let operation = if library == "wrap" {
                Operation::Wrap
            } else {
                Operation::Insert
            };
            dom::spawn(open(operation));
        } else if prompt {
            dom::spawn(crate::agent_panel::prompt_edit());
        }
    });
    dom::listen(&id("explorer-tree"), "contextmenu", |event| {
        let Some(button) = closest(&event, "[data-widget],[data-rendered-widget]") else {
            return;
        };
        let Ok(event) = event.dyn_into::<MouseEvent>() else {
            return;
        };
        event.prevent_default();
        close();
        let (x, y) = (event.client_x(), event.client_y());
        if let Some(widget) = button.get_attribute("data-rendered-widget") {
            dom::spawn(async move {
                crate::preview_inspector::select_rendered(&widget).await?;
                if state::read(|state| state.rendered.selected == widget) {
                    show(x, y);
                }
                Ok(())
            });
        } else if let Some(widget) = button.get_attribute("data-widget") {
            state::change(|state| {
                state.widget = widget;
                view::explorer(state);
                view::inspector(state);
            });
            show(x, y);
        }
    });
    dom::listen(&dom::doc(), "keydown", |event| {
        let scoped = closest(
            &event,
            "#explorer-tree [data-widget],#explorer-tree [data-rendered-widget],#widget-menu",
        )
        .is_some();
        let Ok(key) = event.dyn_into::<KeyboardEvent>() else {
            return;
        };
        if key.key() == "Escape" && !id("widget-menu").hidden() {
            key.prevent_default();
            close();
            focus();
            return;
        }
        if !scoped {
            return;
        }
        if !id("widget-menu").hidden() && matches!(key.key().as_str(), "ArrowDown" | "ArrowUp") {
            key.prevent_default();
            let buttons = dom::all("#widget-menu button:enabled");
            let active = dom::doc().active_element();
            let index = buttons
                .iter()
                .position(|button| Some(button) == active.as_ref())
                .unwrap_or_default();
            if !buttons.is_empty() {
                let next = if key.key() == "ArrowDown" {
                    (index + 1) % buttons.len()
                } else {
                    (index + buttons.len() - 1) % buttons.len()
                };
                let _ = buttons[next].unchecked_ref::<HtmlElement>().focus();
            }
            return;
        }
        if !(key.ctrl_key() || key.meta_key()) || key.alt_key() || key.shift_key() || key.repeat() {
            return;
        }
        let action = match key.key().to_lowercase().as_str() {
            "c" => "copy",
            "x" => "cut",
            "v" => "paste",
            "d" => "duplicate",
            _ => return,
        };
        key.prevent_default();
        close();
        dom::spawn(perform(action.into()));
    });
    dom::listen(&dom::win(), "blur", |_| close());
}
