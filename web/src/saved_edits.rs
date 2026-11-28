use crate::{
    dom::{self, Error, Result, basename, id, items, string},
    state::{self, Mode, State},
};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use web_sys::KeyboardEvent;

fn drafted(state: &State, entry: &Value) -> bool {
    items(&entry["files"])
        .iter()
        .any(|file| state.drafts.contains_key(&state.draft_key(string(file))))
}

pub fn controls(state: &State) {
    for (action, key) in [("undo", "Z"), ("redo", "Shift+Z")] {
        let entry = &state.edit_history[action];
        let blocked = drafted(state, entry);
        let enabled = !state.edit_history_busy
            && state.edit_history_writes == 0
            && !entry.is_null()
            && !blocked;
        let title = if blocked {
            "Save or discard unsaved code in the affected files first.".into()
        } else if !entry.is_null() {
            let names = items(&entry["files"])
                .iter()
                .map(|file| basename(string(file)))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{} {names} (Ctrl/Cmd+{key})",
                if action == "undo" { "Undo" } else { "Redo" }
            )
        } else {
            format!("No saved edit to {action}")
        };
        let button = id(&format!("{action}-edit"));
        dom::disabled(&format!("{action}-edit"), !enabled);
        dom::attr(&button, "title", &title);
    }
}

pub fn writing(route: &str, method: &str) -> Option<(String, u64)> {
    let (project, part) = route.strip_prefix("/projects/")?.split_once('/')?;
    if !matches!(
        (method, part),
        ("PUT", "file")
            | (
                "POST",
                "widgets"
                    | "theme"
                    | "asset"
                    | "theme-values"
                    | "theme-binding"
                    | "widget-library-insert"
                    | "widget-library-wrap"
                    | "widget-constructor-switch"
                    | "widget-property-save"
                    | "launcher-icons"
            )
    ) {
        return None;
    }
    state::change(|state| {
        if state.project_id() != project {
            return None;
        }
        state.edit_history_writes += 1;
        state.edit_history_request += 1;
        state.edit_history = Value::Null;
        controls(state);
        Some((project.to_owned(), state.generation))
    })
}

pub async fn written(context: Option<(String, u64)>) {
    if let Some((project, generation)) = context {
        let current = state::change(|state| {
            if state.generation != generation {
                return false;
            }
            state.edit_history_writes = state.edit_history_writes.saturating_sub(1);
            true
        });
        if current {
            let _ = refresh(&project).await;
        }
    }
}

pub async fn refresh(project: &str) -> Result {
    let context = state::change(|state| {
        if state.project_id() != project || state.edit_history_busy || state.edit_history_writes > 0
        {
            return None;
        }
        state.edit_history_request += 1;
        Some((state.generation, state.edit_history_request))
    });
    let Some((generation, request)) = context else {
        return Ok(());
    };
    let next = dom::project_get(project, "edits").await?;
    state::change(|state| {
        if state.generation == generation
            && state.edit_history_request == request
            && !state.edit_history_busy
            && state.edit_history_writes == 0
        {
            state.edit_history = next;
            controls(state);
        }
    });
    Ok(())
}

async fn replay(action: &str) -> Result {
    let (project, generation, revision) = state::change(|state| {
        if state.edit_history_busy
            || state.edit_history_writes > 0
            || state.edit_history[action].is_null()
        {
            return Err(Error::new("There are no saved edits to undo or redo."));
        }
        if drafted(state, &state.edit_history[action]) {
            return Err(Error::new(
                "Save or discard unsaved code in the affected files first.",
            ));
        }
        state.edit_history_busy = true;
        state.edit_history_request += 1;
        controls(state);
        Ok((
            state.project_id().to_owned(),
            state.generation,
            state.edit_history["revision"].clone(),
        ))
    })?;
    let result = dom::project_post(
        &project,
        "edits",
        json!({"action":action,"revision":revision}),
    )
    .await;
    let outcome = match result {
        Ok(result) => {
            state::change(|state| {
                if state.generation == generation {
                    state.edit_history = result["history"].clone();
                }
            });
            sync_files(&project, generation, &result).await
        }
        Err(error) => Err(error),
    };
    state::change(|state| {
        if state.generation == generation {
            state.edit_history_busy = false;
            state.edit_history_request += 1;
            controls(state);
        }
    });
    if outcome.is_err() {
        let _ = refresh(&project).await;
    }
    outcome?;
    if state::read(|state| state.generation == generation) {
        dom::toast(
            if action == "undo" {
                "Saved edit undone"
            } else {
                "Saved edit redone"
            },
            false,
        );
    }
    Ok(())
}

async fn sync_files(project: &str, generation: u64, result: &Value) -> Result {
    if !state::read(|state| state.generation == generation) {
        return Ok(());
    }
    crate::refresh_tree(project).await?;
    if state::read(|state| !state.asset_catalog.is_null() || state.assets_open) {
        crate::asset_panel::refresh(project).await?;
    }
    let active = state::change(|state| {
        if state.generation != generation {
            return None;
        }
        let changes = items(&result["files"]);
        state.tabs.retain(|path| {
            !changes
                .iter()
                .any(|file| file["path"] == path.as_str() && file["exists"] == false)
                || state
                    .drafts
                    .contains_key(&(project.to_owned(), path.clone()))
        });
        let path = state.current_file().to_owned();
        let changed = changes.iter().find(|file| file["path"] == path)?;
        let keep = changed["exists"] == true
            || state.mode == Mode::Plan
            || state.drafts.contains_key(&state.draft_key(&path));
        Some((path, keep))
    });
    if let Some((path, keep)) = active {
        if keep {
            crate::load_file(&path, true).await?;
        } else {
            let fallback = state::read(|state| {
                state
                    .tree
                    .iter()
                    .find(|entry| entry["path"] == state::MAIN)
                    .or_else(|| {
                        state.tree.iter().find(|entry| {
                            entry["kind"] == "file" && string(&entry["path"]).ends_with(".dart")
                        })
                    })
                    .map(|entry| string(&entry["path"]).to_owned())
                    .unwrap_or_else(|| "pubspec.yaml".into())
            });
            let mode = state::read(|state| state.mode);
            crate::select(fallback, mode).await?;
        }
    }
    Ok(())
}

pub fn initialize() {
    dom::click("undo-edit", || dom::spawn(replay("undo")));
    dom::click("redo-edit", || dom::spawn(replay("redo")));
    dom::listen(&dom::doc(), "keydown", |event| {
        if dom::closest(
            &event,
            "input,textarea,select,[contenteditable=\"true\"],dialog",
        )
        .is_some()
        {
            return;
        }
        let Ok(key) = event.dyn_into::<KeyboardEvent>() else {
            return;
        };
        if !(key.ctrl_key() || key.meta_key()) || key.alt_key() || key.repeat() {
            return;
        }
        let action = match key.key().to_lowercase().as_str() {
            "z" if key.shift_key() => "redo",
            "z" => "undo",
            "y" if !key.shift_key() => "redo",
            _ => return,
        };
        key.prevent_default();
        if state::read(|state| !state.edit_history_busy && !state.edit_history[action].is_null()) {
            dom::spawn(replay(action));
        }
    });
}
