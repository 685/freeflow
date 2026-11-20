use crate::{
    dialogs::{self, View},
    dom::{self, Error, Result, escape as h, icon, items, project_get, project_post, string},
    state::{self, Mode, State},
};
use serde_json::{Value, json};
use std::cell::RefCell;

#[derive(Clone)]
struct References {
    project: String,
    generation: u64,
    result: Value,
    position: Option<usize>,
}

thread_local! { static REFERENCES: RefCell<Option<References>> = const { RefCell::new(None) }; }

pub fn controls(node: &Value, menu: bool) -> String {
    let (caption, target) = if node["component"] == true {
        (
            string(&node["type"]).split('.').next().unwrap_or_default(),
            "instance",
        )
    } else if let Some(name) = node["ownerComponent"]["name"].as_str() {
        (name, "owner")
    } else {
        return String::new();
    };
    let mut output = if menu {
        String::new()
    } else {
        format!(
            "<div class=\"component-controls\"><span>{} {}</span>",
            icon("component", 14),
            h(caption)
        )
    };
    for (action, label) in [
        ("definition", "See definition"),
        ("references", "See references"),
    ] {
        if target == "owner" && action == "definition" {
            continue;
        }
        output.push_str(&format!(r#"<button type="button" class="button soft small" data-component-action="{action}" data-component-target="{target}" {}>{label}</button>"#, if menu { "role=\"menuitem\"" } else { "" }));
    }
    if !menu {
        output.push_str("</div>");
    }
    output
}

pub fn navigation(state: &State) -> String {
    REFERENCES.with_borrow(|references| {
        let Some(references) = references.as_ref().filter(|references| references.project == state.project_id() && references.generation == state.generation) else { return String::new(); };
        let total = items(&references.result["locations"]).len();
        let position = references.position.map_or(0, |position| position+1);
        format!(r#"<div class="component-navigation"><span>{} · {position}/{total}</span><div><button class="icon-button" type="button" data-component-step="previous" aria-label="Previous component reference" {}>{}</button><button class="icon-button" type="button" data-component-step="next" aria-label="Next component reference" {}>{}</button><button class="button soft small" type="button" data-component-step="all">All references</button><button class="icon-button" type="button" data-component-step="close" aria-label="Close component references">{}</button></div></div>"#,
            h(string(&references.result["name"])), if position <= 1 { "disabled" } else { "" }, icon("up", 14),
            if total == 0 || position >= total { "disabled" } else { "" }, icon("down", 14), icon("x", 14))
    })
}

fn current(project: &str, generation: u64) -> bool {
    state::read(|state| state.project_id() == project && state.generation == generation)
}

async fn jump(project: &str, generation: u64, location: &Value) -> Result {
    if !current(project, generation) {
        return Err(Error::new(
            "The project changed. Find its component references again.",
        ));
    }
    let path = string(&location["path"]);
    let source = project_get(project, &format!("file{}", dom::path_query(path))).await?;
    if source["revision"] != location["revision"] {
        return Err(Error::new(
            "This source changed. Refresh the component search before opening this location.",
        ));
    }
    if !current(project, generation) {
        return Ok(());
    }
    let widget = string(&location["widget"]).to_owned();
    let mode = if widget.is_empty() {
        Mode::Code
    } else {
        state::read(|state| {
            if state.mode == Mode::Split {
                Mode::Split
            } else {
                Mode::Design
            }
        })
    };
    crate::select(path.into(), mode).await?;
    if !current(project, generation) {
        return Ok(());
    }
    crate::preview_inspector::show_tree(false);
    state::change(|state| {
        state.widgets = true;
        if !widget.is_empty() {
            state.widget = widget;
        }
        crate::view::explorer(state);
        crate::view::inspector(state);
    });
    crate::editor::go_to(
        location["line"].as_u64().unwrap_or(1),
        location["symbol"]
            .as_u64()
            .or(location["offset"].as_u64())
            .unwrap_or_default()
            .min(u32::MAX as u64) as u32,
    );
    crate::widget_clipboard::focus();
    if let Some(row) = dom::find("#explorer-tree .selected") {
        row.scroll_into_view();
    }
    Ok(())
}

fn show_results(project: String, generation: u64, result: Value) {
    let locations = items(&result["locations"]);
    let mut body = String::new();
    if locations.is_empty() {
        body.push_str("<p class=\"dialog-help\">No uses of this component were found in the saved project.</p>");
    }
    for (index, location) in locations.iter().enumerate() {
        body.push_str(&format!(r#"<button type="button" class="component-location" data-dialog-action="component-use-{index}"><strong>{}:{}</strong><code>{}</code></button>"#,
            h(string(&location["path"])), location["line"].as_u64().unwrap_or_default(), h(string(&location["snippet"]))));
    }
    if result["truncated"] == true {
        body.push_str("<p class=\"dialog-help\">Showing part of the available references.</p>");
    }
    if result["definition"].is_object() {
        body.push_str("<div class=\"form-footer\"><button type=\"button\" class=\"button outline\" data-dialog-action=\"component-definition\">Open definition</button></div>");
    }
    dialogs::open(
        View::ComponentReferences {
            project,
            generation,
            result: result.clone(),
        },
        &format!("{} references", string(&result["name"])),
        "Open a use in its widget tree or Dart source.",
        body,
    );
}

async fn open(action: String, target: String) -> Result {
    let (project, path, revision) = crate::visual_context()?;
    let (generation, widget) = state::read(|state| {
        (
            state.generation,
            state
                .widget()
                .map(|node| node["id"].clone())
                .unwrap_or_default(),
        )
    });
    if widget.is_null() {
        return Err(Error::new("Select a component first."));
    }
    let version = if action == "references" {
        dialogs::open(
            View::ComponentReferences {
                project: project.clone(),
                generation,
                result: Value::Null,
            },
            "Component references",
            "",
            "<p class=\"dialog-help\">Finding uses with Dart…</p>".into(),
        );
        Some(dialogs::version())
    } else {
        None
    };
    let result = project_post(
        &project,
        "component",
        json!({"path":path,"revision":revision,"id":widget,"action":action,"target":target}),
    )
    .await;
    if !current(&project, generation)
        || version.is_some_and(|version| !dialogs::current_view(version))
    {
        return Ok(());
    }
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            if version.is_some_and(dialogs::current_view) {
                dom::html(
                    "modal-body",
                    format!("<p class=\"dialog-help\">{}</p>", h(&error.message)),
                );
            }
            return Err(error);
        }
    };
    if action == "definition" {
        if !state::read(|state| state.widget().is_some_and(|node| node["id"] == widget)) {
            return Ok(());
        }
        if !result["definition"].is_object() {
            return Err(Error::new(
                "Dart did not find a component class inside this project.",
            ));
        }
        jump(&project, generation, &result["definition"]).await?;
    } else {
        REFERENCES.with_borrow_mut(|references| {
            *references = Some(References {
                project: project.clone(),
                generation,
                result: result.clone(),
                position: None,
            })
        });
        show_results(project, generation, result);
        state::read(crate::view::inspector);
    }
    Ok(())
}

pub async fn act(
    project: &str,
    generation: u64,
    result: &Value,
    version: u64,
    action: &str,
) -> Result {
    let location = if action == "component-definition" {
        result["definition"].clone()
    } else if let Some(index) = action
        .strip_prefix("component-use-")
        .and_then(|index| index.parse::<usize>().ok())
    {
        let Some(location) = items(&result["locations"]).get(index) else {
            return Ok(());
        };
        location.clone()
    } else {
        return Ok(());
    };
    jump(project, generation, &location).await?;
    if let Some(index) = action
        .strip_prefix("component-use-")
        .and_then(|index| index.parse::<usize>().ok())
    {
        REFERENCES.with_borrow_mut(|references| {
            if let Some(references) = references.as_mut() {
                references.position = Some(index);
            }
        });
        state::read(crate::view::inspector);
    }
    dialogs::close_current(version);
    Ok(())
}

async fn step(action: String) -> Result {
    if action == "close" {
        REFERENCES.with_borrow_mut(|references| *references = None);
        state::read(crate::view::inspector);
        return Ok(());
    }
    let references = REFERENCES.with_borrow(Clone::clone);
    let Some(mut references) =
        references.filter(|references| current(&references.project, references.generation))
    else {
        return Ok(());
    };
    if action == "all" {
        show_results(references.project, references.generation, references.result);
        return Ok(());
    }
    let locations = items(&references.result["locations"]);
    let position = if action == "previous" {
        references
            .position
            .and_then(|position| position.checked_sub(1))
    } else {
        Some(references.position.map_or(0, |position| position + 1))
    };
    let Some((position, location)) = position.and_then(|position| {
        locations
            .get(position)
            .map(|location| (position, location.clone()))
    }) else {
        return Ok(());
    };
    references.position = Some(position);
    jump(&references.project, references.generation, &location).await?;
    REFERENCES.with_borrow_mut(|current| *current = Some(references));
    state::read(crate::view::inspector);
    Ok(())
}

pub fn initialize() {
    dom::listen(&dom::doc(), "click", |event| {
        if let Some(button) = dom::closest(&event, "[data-component-action]") {
            let action = button
                .get_attribute("data-component-action")
                .unwrap_or_default();
            let target = button
                .get_attribute("data-component-target")
                .unwrap_or_default();
            dom::attr(&button, "disabled", "");
            dom::spawn(async move {
                let result = open(action, target).await;
                let _ = button.remove_attribute("disabled");
                result
            });
        } else if let Some(button) = dom::closest(&event, "[data-component-step]") {
            dom::spawn(step(
                button
                    .get_attribute("data-component-step")
                    .unwrap_or_default(),
            ));
        }
    });
}
