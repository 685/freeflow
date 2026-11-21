mod agent_panel;
mod asset_panel;
mod component_panel;
mod design_values;
mod device_panel;
mod dialogs;
mod dom;
mod editor;
mod editor_assist;
mod phone_preview;
mod planner;
mod preview_canvas;
mod preview_inspector;
mod preview_logs;
mod property_panel;
mod saved_edits;
mod source_control;
mod state;
mod store_upload;
mod theme;
mod view;
mod web_publish;
mod widget_clipboard;

use crate::{
    dom::{
        self as ui, Error, Result, all, api, basename, closest, id, items, path_query, project_get,
        project_post, project_route, string, target, toast,
    },
    state::{Draft, MAIN, Mode},
};
use gloo_timers::future::TimeoutFuture;
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{Blob, DragEvent, Element, KeyboardEvent, ResizeObserver, Url};

fn project_id() -> Result<String> {
    state::read(|state| {
        let id = state.project_id();
        if id.is_empty() {
            Err(Error::new("Open a project first."))
        } else {
            Ok(id.to_owned())
        }
    })
}

fn current(project: &str) -> bool {
    state::read(|state| state.project_id() == project)
}

pub async fn refresh_state() -> Result {
    let next = api("/state").await?;
    state::change(|state| {
        state.projects = items(&next["projects"]).to_vec();
        state.templates = items(&next["templates"]).to_vec();
        state.flutter = next["flutter"].clone();
    });
    view::render();
    Ok(())
}

pub async fn refresh_tree(project: &str) -> Result {
    let result = project_get(project, "tree").await?;
    state::change(|state| {
        if state.project_id() == project && state.tree != items(&result["entries"]) {
            state.tree = items(&result["entries"]).to_vec();
            view::explorer(state);
        }
    });
    Ok(())
}

pub async fn activate(project: Value) -> Result {
    let project_id = string(&project["id"]).to_owned();
    if project_id.is_empty() {
        return Ok(());
    }
    if let Ok(Some(storage)) = ui::win().local_storage() {
        let _ = storage.set_item("freeflow-project", &project_id);
    }
    editor_assist::reset();
    let generation = state::change(|state| {
        state.generation += 1;
        if let Some(url) = state.asset_url.take() {
            let _ = Url::revoke_object_url(&url);
        }
        let next = state::State {
            projects: std::mem::take(&mut state.projects),
            templates: std::mem::take(&mut state.templates),
            flutter: std::mem::take(&mut state.flutter),
            drafts: std::mem::take(&mut state.drafts),
            device: state.device.clone(),
            canvas: state.canvas.clone(),
            generation: state.generation,
            project,
            ..Default::default()
        };
        *state = next;
        view::console_controls(state);
        state.generation
    });
    agent_panel::reset_inputs();
    view::render();
    refresh_tree(&project_id).await?;
    saved_edits::refresh(&project_id).await?;
    let path = state::change(|state| {
        if state.generation != generation {
            return None;
        }
        if !state.tree.iter().any(|entry| entry["path"] == MAIN) {
            if let Some(path) = state
                .tree
                .iter()
                .map(|entry| string(&entry["path"]))
                .find(|path| path.starts_with("lib/") && path.ends_with(".dart"))
            {
                state.file = path.into();
                state.last_dart = path.into();
            }
        }
        Some(state.current_file().to_owned())
    });
    if let Some(path) = path {
        load_file(&path, false).await?;
        update_preview(&project_id).await?;
        phone_preview::refresh(&project_id).await?;
        device_panel::refresh(&project_id).await?;
    }
    Ok(())
}

async fn update_visual(project: &str, file: &str, prefer: &str) -> Result {
    let visual = match project_get(project, &format!("widgets{}", path_query(file))).await {
        Ok(visual) => visual,
        Err(error) if error.status == 404 => Value::Null,
        Err(error) => return Err(error),
    };
    state::change(|state| {
        if state.project_id() != project || state.file != file {
            return;
        }
        let before = state.node(prefer).cloned();
        let nodes = items(&visual["nodes"]);
        let first = nodes
            .iter()
            .find(|node| {
                node["id"] == prefer
                    && before
                        .as_ref()
                        .is_none_or(|old| old["type"] == node["type"])
            })
            .or_else(|| {
                nodes.iter().find(|node| {
                    before.as_ref().is_some_and(|old| {
                        old["type"] == node["type"] && old["line"] == node["line"]
                    })
                })
            })
            .or_else(|| {
                nodes
                    .iter()
                    .find(|node| string(&node["label"]).starts_with("Make today"))
            })
            .or_else(|| nodes.first());
        state.widget = first.map_or("", |node| string(&node["id"])).into();
        state.visual = visual;
        view::explorer(state);
        view::inspector(state);
    });
    Ok(())
}

pub async fn load_file(file: &str, keep: bool) -> Result {
    let project = project_id()?;
    let (generation, request, plan) = state::change(|state| {
        state.file_load += 1;
        if !keep {
            state.source = Value::Null;
            state.file_error.clear();
            if let Some(url) = state.asset_url.take() {
                let _ = Url::revoke_object_url(&url);
            }
            ui::html("asset-view", "");
            view::editor(state);
        }
        (state.generation, state.file_load, state.mode == Mode::Plan)
    });
    let matches = || {
        state::read(|state| {
            state.generation == generation
                && state.file_load == request
                && state.current_file() == file
        })
    };
    let result = async {
        if asset_panel::previewable(file)
            && (!file.to_ascii_lowercase().ends_with(".svg")
                || state::read(|state| state.assets_open && !state.widgets))
        {
            let path = project_route(&project, &format!("asset{}", path_query(file)));
            let response = ui::request(&path, "GET", None).await?;
            let blob: Blob = JsFuture::from(response.blob()?).await?.dyn_into()?;
            if matches() {
                let url = Url::create_object_url_with_blob(&blob)?;
                ui::html("asset-view", asset_panel::preview(file, &url));
                state::change(|state| {
                    if let Some(old) = state.asset_url.replace(url) {
                        let _ = Url::revoke_object_url(&old);
                    }
                });
            }
            return Ok(());
        }
        let route = if plan {
            "plan".into()
        } else {
            format!("file{}", path_query(file))
        };
        let source = project_get(&project, &route).await?;
        if !matches() {
            return Ok(());
        }
        let dart = file.ends_with(".dart");
        let selected = state::change(|state| {
            state.source = source;
            let draft = state.drafts.get(&state.draft_key(file));
            let content = draft.map_or(string(&state.source["content"]), |draft| &draft.content);
            editor::set(content, dart, keep);
            view::editor(state);
            state.widget.clone()
        });
        if dart {
            update_visual(&project, file, &selected).await?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let error: Error = error;
        if matches() {
            state::change(|state| {
                state.file_error = error.message;
                if asset_panel::previewable(file) {
                    ui::html(
                        "asset-view",
                        format!("<p>{}</p>", ui::escape(&state.file_error)),
                    );
                }
                view::editor(state);
            });
        }
    }
    Ok(())
}

pub async fn select(file: String, mode: Mode) -> Result {
    project_id()?;
    if !mode.shows_code() || !file.ends_with(".dart") {
        editor_assist::reset();
    }
    let path = state::change(|state| {
        state.mode = mode;
        if mode != Mode::Plan {
            if file != state.file {
                state.visual = Value::Null;
                state.widget.clear();
                state.collapsed.clear();
                state.rendered.selected.clear();
            }
            state.file = file.clone();
            if file.ends_with(".dart") {
                state.last_dart = file.clone();
            }
        }
        match mode {
            Mode::Design => state.widgets = true,
            Mode::Code | Mode::Split => {
                if mode == Mode::Code {
                    state.widgets = false;
                }
                if !state.tabs.contains(&file) {
                    if state.tabs.len() >= 9 {
                        state.tabs.remove(0);
                    }
                    state.tabs.push(file.clone());
                }
                for (index, _) in file.match_indices('/') {
                    state.folders.insert(file[..index].into());
                }
            }
            Mode::Plan => {}
        }
        state.current_file().to_owned()
    });
    view::render();
    load_file(&path, false).await
}

fn input_changed(value: String) {
    state::change(|state| {
        set_draft(state, value);
        view::editor(state);
    });
}

fn set_draft(state: &mut state::State, value: String) {
    if state.source.is_null() {
        return;
    }
    let key = state.draft_key(state.current_file());
    if state.source["content"] == value {
        state.drafts.remove(&key);
    } else {
        let revision = state.drafts.get(&key).map_or_else(
            || state.source["revision"].clone(),
            |draft| draft.revision.clone(),
        );
        state.drafts.insert(
            key,
            Draft {
                content: value,
                revision,
            },
        );
    }
}

pub async fn save_draft() -> Result {
    let project = project_id()?;
    let (file, key, draft) = state::read(|state| {
        let file = state.current_file().to_owned();
        let key = state.draft_key(&file);
        let draft = state.drafts.get(&key).cloned();
        (file, key, draft)
    });
    let Some(draft) = draft else {
        return Ok(());
    };
    let saved = ui::send(
        &project_route(&project, "file"),
        "PUT",
        json!({"path":file,"content":draft.content,"revision":draft.revision}),
    )
    .await?;
    let selected = state::change(|state| {
        if state.drafts.get(&key) == Some(&draft) {
            state.drafts.remove(&key);
        } else if let Some(newer) = state.drafts.get_mut(&key) {
            newer.revision = saved["revision"].clone();
        }
        if state.project_id() == project && state.current_file() == file {
            state.source = saved;
            view::editor(state);
            Some(state.widget.clone())
        } else {
            None
        }
    });
    if let Some(selected) = selected.filter(|_| file.ends_with(".dart")) {
        update_visual(&project, &file, &selected).await?;
    }
    refresh_tree(&project).await?;
    toast(&format!("Saved {}", basename(&file)), false);
    Ok(())
}

pub async fn visual_edit(command: Value) -> Result {
    let (project, path, revision) = visual_context()?;
    visual_edit_at(project, path, revision, command).await
}

pub fn visual_context() -> Result<(String, String, Value)> {
    let project = project_id()?;
    let (path, revision, drafted) = state::read(|state| {
        let path = string(&state.visual["path"]).to_owned();
        let drafted = state.drafts.contains_key(&state.draft_key(&path));
        (path, state.visual["revision"].clone(), drafted)
    });
    if path.is_empty() {
        return Err(Error::new("Select a Dart widget first."));
    }
    if drafted {
        return Err(Error::new(
            "Save the code draft before editing its widgets.",
        ));
    }
    Ok((project, path, revision))
}

pub async fn visual_edit_at(
    project: String,
    path: String,
    revision: Value,
    command: Value,
) -> Result {
    if state::read(|state| state.drafts.contains_key(&(project.clone(), path.clone()))) {
        return Err(Error::new(
            "Save the code draft before editing its widgets.",
        ));
    }
    let saved = project_post(
        &project,
        "widgets",
        json!({"path":path,"revision":revision,"command":command}),
    )
    .await?;
    if state::read(|state| state.project_id() == project && state.file == path) {
        state::change(|state| {
            if state.mode.shows_code() && !state.drafts.contains_key(&state.draft_key(&path)) {
                editor::set(string(&saved["content"]), true, true);
            }
            state.source = saved;
        });
        update_visual(&project, &path, string(&command["id"])).await?;
    }
    Ok(())
}

pub fn finished(job: &Value) -> bool {
    matches!(string(&job["status"]), "succeeded" | "failed" | "stopped")
}

pub fn track(job: &Value, open: bool) {
    let job_id = string(&job["id"]);
    if job_id.is_empty() {
        return;
    }
    state::change(|state| {
        state.logs.retain(|entry| entry["id"] != job_id);
        state.logs.insert(0, job.clone());
        state.logs.truncate(22);
        state.console_job = job_id.into();
        if open {
            state.console_open = true;
        }
        ui::hidden("console-indicator", state.console_open);
        view::console_controls(state);
    });
}

pub async fn wait_job(mut job: Value) -> Result<Value> {
    let job_id = string(&job["id"]).to_owned();
    if job_id.is_empty() {
        return Err(Error::new("Freeflow did not start the command."));
    }
    while !finished(&job) {
        TimeoutFuture::new(550).await;
        job = api(&format!("/jobs/{job_id}")).await?;
    }
    if job["status"] == "succeeded" {
        Ok(job["result"].clone())
    } else {
        Err(Error::new(job["error"].as_str().unwrap_or(
            if job["status"] == "stopped" {
                "The command was canceled."
            } else {
                "See the console for Flutter output."
            },
        )))
    }
}

fn console(open: bool) {
    state::change(|state| {
        state.console_open = open;
        view::console_controls(state);
    });
    if open {
        ui::spawn(refresh_console());
    }
}

async fn refresh_console() -> Result {
    let choice = state::change(|state| {
        if !state.console_open {
            return None;
        }
        if !state.preview.is_null()
            && !state
                .logs
                .iter()
                .any(|job| job["id"] == state.preview["id"])
        {
            state.logs.insert(0, state.preview.clone());
            state.logs.truncate(22);
            if state.console_job.is_empty() {
                state.console_job = string(&state.preview["id"]).into();
            }
        }
        Some(state.console_job.clone())
    });
    let Some(choice) = choice else {
        return Ok(());
    };
    let job = if choice.is_empty() {
        Value::Null
    } else {
        api(&format!("/jobs/{choice}")).await.unwrap_or(Value::Null)
    };
    state::read(|state| {
        if state.console_open && state.console_job == choice {
            view::console_logs(state, &job);
        }
    });
    Ok(())
}

async fn update_preview(project: &str) -> Result {
    let job = project_get(project, "preview").await?;
    let failed = state::change(|state| {
        if state.project_id() != project {
            return false;
        }
        let failed = job["status"] == "failed"
            && (state.preview["status"] != "failed" || state.preview["id"] != job["id"]);
        state.preview = job.clone();
        view::preview(state, false);
        failed
    });
    if failed {
        track(&job, true);
        toast("Flutter preview could not start.", true);
    }
    Ok(())
}

async fn preview_action(action: &str) -> Result {
    let project = project_id()?;
    if action == "start" && !state::read(|state| state.flutter["found"] == true) {
        return dialogs::settings().await;
    }
    let job = project_post(&project, "preview", json!({"action":action})).await?;
    if current(&project) {
        if action != "stop" {
            track(&job, false);
        }
        state::change(|state| {
            state.preview = job;
            view::preview(state, false);
        });
    }
    Ok(())
}

pub async fn new_project(data: Value, preview: bool) -> Result {
    if !state::read(|state| state.flutter["found"] == true) {
        return dialogs::settings().await;
    }
    let job = ui::post("/projects/new", data).await?;
    track(&job, true);
    let result = wait_job(job).await?;
    refresh_state().await?;
    activate(result["project"].clone()).await?;
    if preview {
        preview_action("start").await?;
    }
    Ok(())
}

pub async fn import_project(folder: &str) -> Result {
    let project = ui::post("/projects/import", json!({"folder":folder})).await?;
    refresh_state().await?;
    activate(project).await
}

async fn run_command(action: &str) -> Result {
    let project = project_id()?;
    if !state::read(|state| state.flutter["found"] == true) {
        return dialogs::settings().await;
    }
    if action == "format" {
        save_draft().await?;
    }
    let (path, revision) = state::read(|state| {
        (
            state.current_file().to_owned(),
            state.source["revision"].clone(),
        )
    });
    let job = project_post(
        &project,
        "command",
        json!({"action":action,"path":path,"revision":revision}),
    )
    .await?;
    track(&job, true);
    wait_job(job).await?;
    if action == "format" {
        saved_edits::refresh(&project).await?;
    }
    if action == "format" && current(&project) && state::read(|state| state.current_file() == path)
    {
        load_file(&path, true).await?;
    }
    let message = match action {
        "format" => format!("Formatted {}", basename(&path)),
        "test" => "Flutter tests passed".into(),
        "dependencies" => "Flutter dependencies refreshed".into(),
        _ => "Flutter analysis passed".into(),
    };
    toast(&message, false);
    Ok(())
}

async fn poll(tick: u32) -> Result {
    if ui::doc().hidden() {
        return Ok(());
    }
    if let Ok(project) = project_id() {
        saved_edits::refresh(&project).await?;
        agent_panel::poll(&project).await?;
        device_panel::poll(&project).await?;
        phone_preview::poll(&project).await?;
        update_preview(&project).await?;
        state::read(preview_inspector::request_tree);
        if tick.is_multiple_of(4) && current(&project) {
            refresh_tree(&project).await?;
            let (file, revision, plan, loaded) = state::read(|state| {
                (
                    state.current_file().to_owned(),
                    state.source["revision"].clone(),
                    state.mode == Mode::Plan,
                    !state.source.is_null(),
                )
            });
            if loaded {
                let path = if plan {
                    "plan".into()
                } else {
                    format!("file{}", path_query(&file))
                };
                if let Ok(next) = project_get(&project, &path).await {
                    if next["revision"] != revision
                        && state::read(|state| {
                            state.project_id() == project
                                && state.current_file() == file
                                && state.source["revision"] == revision
                        })
                    {
                        load_file(&file, true).await?;
                    }
                }
            }
        }
    }
    refresh_console().await
}

fn register_events() {
    ui::click("project-switch", dialogs::projects);
    ui::click("new-project", dialogs::create);
    ui::click("import-project", dialogs::import);
    ui::click("try-sample", || {
        ui::spawn(async {
            ui::disabled("try-sample", true);
            let result = new_project(
                json!({"name":"Daymark","template":"daymark","organization":"dev.freeflow"}),
                true,
            )
            .await;
            ui::disabled("try-sample", false);
            result
        })
    });
    ui::click("ai", || ui::spawn(dialogs::ai()));
    ui::click("backends", || ui::spawn(dialogs::backends("firebase")));
    ui::click("export", || ui::spawn(dialogs::exports("web")));
    ui::click("settings", || ui::spawn(dialogs::settings()));
    ui::click("history", || ui::spawn(dialogs::history()));
    ui::click("new-file", dialogs::new_file);
    ui::click("run", || {
        let action = state::read(|state| {
            if matches!(
                string(&state.preview["status"]),
                "ready" | "running" | "queued"
            ) {
                "stop"
            } else {
                "start"
            }
        });
        ui::spawn(preview_action(action));
    });
    ui::click("reload", || ui::spawn(preview_action("reload")));
    ui::click("restart-preview", || ui::spawn(preview_action("restart")));
    ui::click("phone-preview", || ui::spawn(phone_preview::open()));
    ui::click("open-preview", || {
        let url = state::read(|state| string(&state.preview["result"]["url"]).to_owned());
        if !url.is_empty() {
            let _ = ui::win().open_with_url_and_target_and_features(
                &url,
                "_blank",
                "noopener,noreferrer",
            );
        }
    });
    ui::listen(&id("preview-empty"), "click", |event| {
        match closest(&event, "[data-preview-action]")
            .and_then(|node| node.get_attribute("data-preview-action"))
            .as_deref()
        {
            Some("start") => ui::spawn(preview_action("start")),
            Some("console") => console(true),
            _ => {}
        }
    });
    ui::click("save", || ui::spawn(save_draft()));
    for command in ["format", "analyze", "test", "dependencies"] {
        ui::click(command, move || ui::spawn(run_command(command)));
    }
    ui::listen(&id("editor-tabs"), "click", |event| {
        if let Some(file) =
            closest(&event, "[data-file]").and_then(|element| element.get_attribute("data-file"))
        {
            ui::spawn(select(file, code_mode()));
        }
    });
    ui::listen(&id("editor-notice"), "click", |event| {
        if closest(&event, "#reload-source").is_none() {
            return;
        }
        let path = state::change(|state| {
            let path = state.current_file().to_owned();
            state.drafts.remove(&state.draft_key(&path));
            path
        });
        ui::spawn(async move { load_file(&path, true).await });
    });
    ui::click("console-toggle", || {
        console(!state::read(|state| state.console_open))
    });
    ui::click("console-close", || console(false));
    ui::listen(&id("console-job"), "change", |event| {
        if let Some(select) = target(&event) {
            state::change(|state| state.console_job = ui::value(&select));
            ui::spawn(refresh_console());
        }
    });
    ui::click("cancel-job", || {
        ui::spawn(async {
            let job = state::read(|state| state.console_job.clone());
            if !job.is_empty() {
                ui::post(&format!("/jobs/{job}/stop"), json!({})).await?;
            }
            Ok(())
        })
    });
    for (button, class) in [
        ("mobile-explorer", "show-explorer"),
        ("mobile-inspector", "show-inspector"),
    ] {
        ui::click(button, move || {
            if let Some(workbench) = ui::find(".workbench") {
                if let Ok(open) = workbench.class_list().toggle(class) {
                    ui::attr(
                        &id(button),
                        "aria-pressed",
                        if open { "true" } else { "false" },
                    );
                }
            }
        });
    }
    for button in all("[data-mode]") {
        let mode = Mode::parse(&button.get_attribute("data-mode").unwrap_or_default());
        ui::listen(&button, "click", move |_| {
            let file = state::read(|state| {
                (!state.project.is_null() && state.mode != mode).then(|| {
                    if mode == Mode::Design {
                        state.last_dart.clone()
                    } else {
                        state.file.clone()
                    }
                })
            });
            if let Some(file) = file {
                ui::spawn(select(file, mode));
            }
        });
    }
    for button in all("[data-device]") {
        let device = button.get_attribute("data-device").unwrap_or_default();
        ui::listen(&button, "click", move |_| {
            preview_canvas::device(&device);
        });
    }
    for button in all("[data-explorer]") {
        let widgets = button.get_attribute("data-explorer").as_deref() == Some("widgets");
        let assets = button.get_attribute("data-explorer").as_deref() == Some("assets");
        ui::listen(&button, "click", move |_| {
            state::change(|state| {
                state.widgets = widgets;
                state.assets_open = assets;
                view::explorer(state);
                view::inspector(state);
                preview_inspector::request_tree(state);
            });
            if assets {
                ui::spawn(asset_panel::refresh_current());
            }
        });
    }
    ui::click("source-widgets", || preview_inspector::show_tree(false));
    ui::click("rendered-widgets", || preview_inspector::show_tree(true));
    ui::listen(&id("source-select"), "change", |event| {
        if let Some(element) = target(&event) {
            let mode = state::read(|state| {
                if state.mode == Mode::Split {
                    Mode::Split
                } else {
                    Mode::Design
                }
            });
            ui::spawn(select(ui::value(&element), mode));
        }
    });
    ui::listen(&ui::doc(), "keydown", |event| {
        let Ok(key) = event.dyn_into::<KeyboardEvent>() else {
            return;
        };
        if !(key.ctrl_key() || key.meta_key()) || key.alt_key() {
            return;
        }
        match key.key().to_lowercase().as_str() {
            "s" if state::read(|state| state.mode != Mode::Design) => {
                key.prevent_default();
                ui::spawn(save_draft());
            }
            "p" if project_id().is_ok() => {
                key.prevent_default();
                dialogs::quick_open();
            }
            _ => {}
        }
    });
    tree_events();
    inspector_events();
}

fn tree_events() {
    let tree = id("explorer-tree");
    ui::listen(&tree, "click", |event| {
        if let Some(node) =
            closest(&event, "[data-collapse]").and_then(|node| node.get_attribute("data-collapse"))
        {
            state::change(|state| {
                let collapsed = if state.showing_rendered_widgets() {
                    &mut state.rendered.collapsed
                } else {
                    &mut state.collapsed
                };
                if !collapsed.remove(&node) {
                    collapsed.insert(node);
                }
                view::explorer(state);
            });
            return;
        }
        let Some(button) = closest(&event, "button") else {
            return;
        };
        if let Some(folder) = button.get_attribute("data-folder") {
            state::change(|state| {
                if !state.folders.remove(&folder) {
                    state.folders.insert(folder);
                }
                view::explorer(state);
            });
        } else if let Some(file) = button.get_attribute("data-file") {
            ui::spawn(select(file, code_mode()));
        } else if let Some(widget) = button.get_attribute("data-rendered-widget") {
            ui::spawn(async move {
                preview_inspector::select_rendered(&widget).await?;
                widget_clipboard::focus();
                Ok(())
            });
        } else if let Some(widget) = button.get_attribute("data-widget") {
            state::change(|state| {
                state.widget = widget;
                view::explorer(state);
                view::inspector(state);
            });
            widget_clipboard::focus();
        }
    });
    ui::listen(&tree, "dragstart", |event| {
        let Some(widget) =
            closest(&event, "[data-widget]").and_then(|node| node.get_attribute("data-widget"))
        else {
            return;
        };
        let Ok(event) = event.dyn_into::<DragEvent>() else {
            return;
        };
        state::change(|state| {
            let node = state
                .node(&widget)
                .filter(|node| node["removable"] == true)
                .cloned();
            if node.is_some() {
                if let Some(data) = event.data_transfer() {
                    data.set_effect_allowed("move");
                    let _ = data.set_data("text/plain", &widget);
                }
            }
            state.dragged = node;
        });
    });
    ui::listen(&tree, "dragover", |event| {
        clear_drop();
        let Ok(event) = event.dyn_into::<DragEvent>() else {
            return;
        };
        if let Some((button, _, side)) = drop_target(&event) {
            event.prevent_default();
            if let Some(data) = event.data_transfer() {
                data.set_drop_effect("move");
            }
            ui::toggle(&button, &format!("drop-{side}"), true);
        }
    });
    ui::listen(&tree, "dragend", |_| {
        state::change(|state| state.dragged = None);
        clear_drop();
    });
    ui::listen(&tree, "drop", |event| {
        clear_drop();
        let Ok(event) = event.dyn_into::<DragEvent>() else {
            return;
        };
        if let Some((_, target, side)) = drop_target(&event) {
            event.prevent_default();
            let dragged = state::change(|state| state.dragged.take());
            if let Some(dragged) = dragged {
                ui::spawn(visual_edit(
                    json!({"id":dragged["id"],"operation":"move","target":target,"side":side}),
                ));
            }
        }
    });
}

fn clear_drop() {
    for row in all(".drop-before,.drop-after,.drop-inside") {
        ui::toggle(&row, "drop-before", false);
        ui::toggle(&row, "drop-after", false);
        ui::toggle(&row, "drop-inside", false);
    }
}

fn drop_target(event: &DragEvent) -> Option<(Element, String, &'static str)> {
    let button = closest(event, "[data-widget]")?;
    let widget = button.get_attribute("data-widget")?;
    let side = state::read(|state| {
        let Some((from, to)) = state.dragged.as_ref().zip(state.node(&widget)) else {
            return None;
        };
        let mut ancestor = Some(to);
        while let Some(node) = ancestor {
            if node["id"] == from["id"] {
                return None;
            }
            ancestor = node["parent"].as_str().and_then(|id| state.node(id));
        }
        let rect = button.get_bounding_client_rect();
        let position = (f64::from(event.client_y()) - rect.top()) / rect.height();
        if !items(&to["slots"]).is_empty()
            && (to["movable"] != true || (0.25..=0.75).contains(&position))
        {
            Some("inside")
        } else if to["movable"] == true {
            Some(if position < 0.5 { "before" } else { "after" })
        } else {
            None
        }
    });
    Some((button, widget, side?))
}

fn inspector_events() {
    let panel = id("inspector");
    ui::listen(&panel, "change", |event| {
        let Some(input) = target(&event) else {
            return;
        };
        let raw = ui::value(&input);
        if input.id() == "insert-slot" {
            state::read(|state| {
                if let Some(node) = state.widget() {
                    ui::html("insert-widget", view::widget_options(node, &raw));
                }
            });
            return;
        }
        let Some(property) = input.get_attribute("data-property") else {
            return;
        };
        let kind = input.get_attribute("data-type").unwrap_or_default();
        if raw.is_empty() && kind != "string" {
            if input
                .get_attribute("data-branch")
                .unwrap_or_default()
                .is_empty()
            {
                ui::spawn(property_panel::reset(property));
            }
            return;
        }
        let value = match kind.as_str() {
            "boolean" => json!(raw == "true"),
            "number" | "integer" | "spacing" => {
                raw.parse::<f64>().ok().map_or(Value::Null, |n| json!(n))
            }
            _ => json!(raw),
        };
        let widget = state::read(|state| state.widget.clone());
        let mut command = json!({"id":widget,"operation":"property","name":property,"value":value});
        if let Some(branch) = input
            .get_attribute("data-branch")
            .filter(|branch| !branch.is_empty())
        {
            command["branch"] = json!(branch);
        }
        ui::spawn(visual_edit(command));
    });
    ui::listen(&panel, "click", |event| {
        let Some(button) = closest(&event, "button") else {
            return;
        };
        let Some(node) = state::read(|state| state.widget().cloned()) else {
            return;
        };
        if let Some(direction) = button.get_attribute("data-move") {
            let up = direction == "up";
            let sibling = state::read(|state| {
                let siblings = state.siblings(&node);
                let place = siblings
                    .iter()
                    .position(|entry| entry["id"] == node["id"])?;
                let index = if up { place.checked_sub(1)? } else { place + 1 };
                siblings.get(index).map(|entry| entry["id"].clone())
            });
            if let Some(sibling) = sibling {
                ui::spawn(visual_edit(
                    json!({"id":node["id"],"operation":"move","target":sibling,"side":if up {"before"} else {"after"}}),
                ));
            }
            return;
        }
        match button.get_attribute("data-widget-action").as_deref() {
            Some("code") => {
                let file = state::read(|state| string(&state.visual["path"]).to_owned());
                ui::spawn(async move {
                    select(file, code_mode()).await?;
                    editor::go_to(
                        node["line"].as_u64().unwrap_or_default(),
                        string(&node["id"]).parse().unwrap_or_default(),
                    );
                    Ok(())
                });
            }
            Some("wrap") => dialogs::wrap(node),
            Some("remove") => ui::spawn(visual_edit(json!({"id":node["id"],"operation":"remove"}))),
            Some("add") => ui::spawn(visual_edit(json!({"id":node["id"],"operation":"add",
                "slot":ui::value(&id("insert-slot")),"widget":ui::value(&id("insert-widget"))}))),
            _ => {}
        }
    });
}

fn code_mode() -> Mode {
    state::read(|state| {
        if state.mode == Mode::Split {
            Mode::Split
        } else {
            Mode::Code
        }
    })
}

#[wasm_bindgen(start)]
fn start() -> std::result::Result<(), JsValue> {
    ui::icons();
    editor::initialize();
    editor_assist::initialize();
    dialogs::initialize();
    register_events();
    preview_inspector::initialize();
    widget_clipboard::initialize();
    saved_edits::initialize();
    preview_canvas::initialize();
    agent_panel::initialize();
    property_panel::initialize();
    component_panel::initialize();
    asset_panel::initialize();
    design_values::initialize();
    ui::click("run-device", || ui::spawn(device_panel::open()));
    planner::initialize();
    ui::click("app-theme", || ui::spawn(theme::open(None)));
    ui::click("git", || ui::spawn(source_control::open()));
    let resized = Closure::<dyn FnMut()>::new(|| state::read(view::fit));
    let observer = ResizeObserver::new(resized.as_ref().unchecked_ref())?;
    observer.observe(&id("canvas"));
    resized.forget();
    std::mem::forget(observer);
    ui::spawn(async {
        refresh_state().await?;
        let previous = ui::win()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|storage| storage.get_item("freeflow-project").ok().flatten());
        let project = state::read(|state| {
            state
                .projects
                .iter()
                .find(|project| previous.as_deref() == project["id"].as_str())
                .or_else(|| state.projects.first())
                .cloned()
        });
        if let Some(project) = project {
            activate(project).await?;
        }
        Ok(())
    });
    spawn_local(async {
        let mut tick: u32 = 0;
        loop {
            TimeoutFuture::new(850).await;
            tick = tick.wrapping_add(1);
            if let Err(error) = poll(tick).await {
                if error.status == 401 {
                    ui::text("preview-status", "Reload to reconnect");
                }
            }
        }
    });
    Ok(())
}
