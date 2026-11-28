use crate::{
    dialogs::{self, View, close_current},
    dom::{
        self, Error, Result, api, disabled, escape as h, hidden, html, id, items, post,
        project_get, project_post, string, toast,
    },
    state::{self, State},
};
use base64::Engine;
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{File, FileList, HtmlInputElement};

fn image_input() -> HtmlInputElement {
    id("agent-image").unchecked_into()
}

fn selected_image() -> Option<File> {
    image_input().files()?.get(0)
}

fn refresh_image() {
    let name = selected_image().map(|file| file.name());
    hidden("agent-attachment", name.is_none());
    dom::text("agent-attachment-name", name.as_deref().unwrap_or_default());
}

fn busy(state: &State) -> bool {
    state.agent_posting || !state.agent_job.is_null()
}

pub fn layout(state: &State) {
    hidden("agent-panel", !state.agent_open);
    hidden("inspector", state.agent_open);
    hidden("properties-heading", state.agent_open);
    dom::attr(
        &id("agent"),
        "aria-pressed",
        if state.agent_open { "true" } else { "false" },
    );
}

pub fn context(state: &State) {
    if !state.agent_open {
        return;
    }
    let widget = selected_widget(state);
    let text = if let Some(widget) = widget {
        format!(
            "{} · {}:{}",
            string(&widget["type"]),
            dom::basename(state.current_file()),
            widget["line"]
        )
    } else {
        state.current_file().to_owned()
    };
    dom::text("agent-context", &text);
    dom::attr(&id("agent-context"), "title", &text);
}

fn selected_widget(state: &State) -> Option<&Value> {
    if state.mode.shows_preview() && state.widgets {
        state.widget()
    } else {
        None
    }
}

fn render(state: &State) {
    if !state.agent_open {
        return;
    }
    let messages = items(&state.agent["messages"]);
    let checkpoints = items(&state.agent["checkpoints"]);
    let waiting = busy(state);
    let mut output = String::new();
    if messages.is_empty() && !waiting {
        let provider = &state.agent["provider"];
        if provider["configured"] == true {
            output.push_str(
                "<p class=\"muted\">Ask about this Flutter project or describe a change.</p>",
            );
        } else {
            output.push_str(r#"<p>Connect an AI provider to start a conversation.</p><button type="button" class="button outline small" data-agent="configure">Configure provider</button>"#);
        }
    }
    for entry in messages {
        let user = entry["role"] == "user";
        output.push_str(&message(
            user,
            string(&entry["text"]),
            entry["image"].as_str(),
        ));
        if let Some(checkpoint) = checkpoints
            .iter()
            .find(|checkpoint| checkpoint["id"] == entry["checkpoint"])
        {
            output.push_str(&checkpoint_action(checkpoint));
        }
        output.push_str("</article>");
    }
    for checkpoint in checkpoints.iter().filter(|checkpoint| {
        !messages
            .iter()
            .any(|message| message["checkpoint"] == checkpoint["id"])
    }) {
        output.push_str(&checkpoint_action(checkpoint));
    }
    if waiting {
        let progress = &state.agent_job["result"];
        if let Some(prompt) = progress["prompt"].as_str() {
            output.push_str(&message(true, prompt, progress["image"].as_str()));
            output.push_str("</article>");
        }
        let phase = progress["phase"].as_str().unwrap_or("Waiting for Agent");
        let partial = string(&progress["partial"]);
        output.push_str(&message(
            false,
            if partial.is_empty() { phase } else { partial },
            None,
        ));
        output.push_str("</article>");
    }
    html("agent-messages", output);
    html(
        "agent-thread",
        items(&state.agent["threads"])
            .iter()
            .map(|thread| {
                let selected = if thread["id"] == state.agent["thread"] {
                    " selected"
                } else {
                    ""
                };
                format!(
                    r#"<option value="{}"{selected}>{}</option>"#,
                    h(string(&thread["id"])),
                    h(string(&thread["title"]))
                )
            })
            .collect::<String>(),
    );
    let log = id("agent-messages");
    log.set_scroll_top(log.scroll_height());
    for control in [
        "agent-send",
        "agent-prompt",
        "agent-mode",
        "agent-attach",
        "agent-remove-image",
        "agent-settings",
        "agent-thread",
    ] {
        disabled(control, waiting);
    }
    disabled("agent-new", waiting || messages.is_empty());
    hidden("agent-stop", state.agent_job.is_null());
    hidden("agent-send", !state.agent_job.is_null());
    context(state);
}

fn message(user: bool, text: &str, attachment: Option<&str>) -> String {
    let image = attachment
        .map(|name| format!("<small>Image · {}</small>", h(name)))
        .unwrap_or_default();
    format!(
        r#"<article class="agent-message{}"><strong>{}</strong><p>{}</p>{image}"#,
        if user { " user" } else { "" },
        if user { "You" } else { "Agent" },
        h(text)
    )
}

fn checkpoint_action(checkpoint: &Value) -> String {
    let paths = items(&checkpoint["paths"])
        .iter()
        .map(|path| h(string(path)))
        .collect::<Vec<_>>()
        .join(", ");
    let button = if checkpoint["reverted"] == true {
        "<small>Checkpoint restored</small>".into()
    } else {
        format!(
            r#"<button type="button" class="button soft small" data-agent="undo" data-checkpoint="{}">Restore checkpoint</button>"#,
            h(string(&checkpoint["id"]))
        )
    };
    format!(r#"<div class="agent-checkpoint"><small>{paths}</small>{button}</div>"#)
}

async fn refresh(project: &str) -> Result {
    let result = project_get(project, "agent").await?;
    state::change(|state| {
        if state.project_id() == project {
            if !result["job"].is_null() {
                state.agent_job = result["job"].clone();
            }
            state.agent = result;
            render(state);
        }
    });
    Ok(())
}

async fn open() -> Result {
    let project = crate::project_id()?;
    state::change(|state| {
        state.agent_open = true;
        layout(state);
        render(state);
    });
    if let Some(workbench) = dom::find(".workbench") {
        dom::toggle(&workbench, "show-inspector", true);
    }
    refresh(&project).await
}

pub async fn prepare(prompt: &str) -> Result {
    let project = crate::project_id()?;
    open().await?;
    if crate::current(&project) && !state::read(busy) {
        let field = id("agent-prompt");
        dom::set_value(&field, prompt);
        dom::set_value(&id("agent-mode"), "edit");
        let _ = field.focus();
    }
    Ok(())
}

pub(crate) async fn generate_project(
    project: &str,
    prompt: &str,
    image: Option<FileList>,
) -> Result {
    if !crate::current(project) {
        return Ok(());
    }
    prepare(prompt).await?;
    if !crate::current(project) || state::read(busy) {
        return Ok(());
    }
    image_input().set_files(image.as_ref());
    refresh_image();
    if state::read(|state| state.agent["provider"]["configured"] == true) {
        send().await?;
    }
    Ok(())
}

fn close() {
    state::change(|state| {
        state.agent_open = false;
        layout(state);
    });
    if let Some(workbench) = dom::find(".workbench") {
        dom::toggle(&workbench, "show-inspector", false);
    }
}

async fn settings(project: String) -> Result {
    let (provider, rules, path, generation) = state::read(|state| {
        (
            state.agent["provider"].clone(),
            string(&state.agent["rules"]).to_owned(),
            state.current_file().to_owned(),
            state.generation,
        )
    });
    let sources = project_get(
        &project,
        &format!("project-rules{}", dom::path_query(&path)),
    )
    .await?;
    if !state::read(|state| {
        state.project_id() == project
            && state.generation == generation
            && state.current_file() == path
    }) {
        return Ok(());
    }
    let mut loaded = String::new();
    for file in items(&sources["files"]) {
        loaded.push_str(&format!(
            "<li><code>{}</code> · {}</li>",
            h(string(&file["path"])),
            if file["truncated"] == true {
                "Excerpt loaded"
            } else {
                "Loaded"
            }
        ));
    }
    for file in items(&sources["skipped"]) {
        loaded.push_str(&format!(
            "<li><code>{}</code> · Skipped: {}</li>",
            h(string(&file["path"])),
            h(string(&file["reason"]))
        ));
    }
    if loaded.is_empty() {
        loaded.push_str("<li>No rule files found for the selected file.</li>");
    }
    if let Some(count) = sources["omittedFiles"].as_u64().filter(|count| *count > 0) {
        loaded.push_str(&format!(
            "<li>{count} additional rule files omitted from this excerpt.</li>"
        ));
    }
    dialogs::open(
        View::AgentSettings {
            project: project.into(),
        },
        "Agent settings",
        "Use an OpenAI-compatible provider, such as a local Ollama server.",
        format!(
            r#"<form class="form">
              <label class="form-field"><span>Base URL</span><input name="baseUrl" required autofocus value="{}" placeholder="http://127.0.0.1:11434/v1"></label>
              <label class="form-field"><span>Model</span><input name="model" required value="{}" placeholder="Model name"></label>
              <label class="form-field"><span>API key</span><input name="apiKey" type="password" autocomplete="off" placeholder="{}"></label>
              {}
              <div class="form-field"><span>Project rule files</span><p class="muted">Root rules and rules in the selected file’s folders load automatically from AGENTS.md, CLAUDE.md, ARCHITECTURE.md and .cursorrules.</p><ul>{}</ul></div>
              <label class="form-field"><span>Additional project instructions</span><textarea name="rules" rows="4" maxlength="8000">{}</textarea></label>
              <div class="form-error" role="alert"></div>
              <div class="form-footer"><button type="submit" class="button primary">Save settings</button></div>
            </form>"#,
            h(provider["baseUrl"]
                .as_str()
                .unwrap_or("http://127.0.0.1:11434/v1")),
            h(string(&provider["model"])),
            if provider["hasKey"] == true {
                "Key is saved"
            } else {
                "Optional for local providers"
            },
            if provider["hasKey"] == true {
                r#"<label class="check-row"><input type="checkbox" name="clearKey">Remove saved key</label>"#
            } else {
                ""
            },
            loaded,
            h(&rules)
        ),
    );
    Ok(())
}

pub async fn save_settings(project: &str, version: u64, input: Value) -> Result {
    post("/agent-config", input.clone()).await?;
    let updated = state::read(|state| state.agent["rules"] != input["rules"]);
    if updated {
        let job = project_post(
            project,
            "agent",
            json!({"action":"rules","rules":input["rules"]}),
        )
        .await?;
        crate::wait_job(job).await?;
    }
    refresh(project).await?;
    close_current(version);
    toast("Agent settings saved", false);
    Ok(())
}

pub(crate) fn check_image(file: &File) -> Result {
    if !matches!(
        file.type_().as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    ) {
        return Err(Error::new("Choose a PNG, JPEG, WebP or GIF image."));
    }
    if file.size() == 0.0 || file.size() > f64::from(3 * 1024 * 1024) {
        return Err(Error::new("Choose a valid image up to 3 MiB."));
    }
    Ok(())
}

async fn attachment(file: Option<File>) -> Result<Value> {
    let Some(file) = file else {
        return Ok(Value::Null);
    };
    check_image(&file)?;
    let buffer = JsFuture::from(file.array_buffer()).await?;
    let data =
        base64::engine::general_purpose::STANDARD.encode(js_sys::Uint8Array::new(&buffer).to_vec());
    Ok(json!({"name":file.name(),"mime":file.type_(),"data":data}))
}

pub(crate) async fn prompt_edit() -> Result {
    let context = crate::property_panel::Context::capture().await?;
    let kind = state::read(|state| {
        state
            .widget()
            .map(|widget| string(&widget["type"]).to_owned())
    })
    .unwrap_or_default();
    let description = format!("{kind} · {}", context.path);
    dialogs::open(View::WidgetPromptEdit { context }, "Prompt Edit", &description,
        r#"<form class="form">
          <label class="form-field"><span>What should change?</span><textarea name="prompt" rows="5" maxlength="12000" placeholder="Describe the widget edit…" required autofocus></textarea></label>
          <p id="widget-edit-progress" class="dialog-help" role="status"></p>
          <div class="form-error" role="alert"></div>
          <div class="form-footer"><button id="widget-edit-stop" type="button" class="button outline" data-dialog-action="stop-widget-edit" hidden>Stop</button><button type="submit" class="button primary">Apply edit</button></div>
        </form>"#.into());
    Ok(())
}

pub(crate) async fn submit_prompt_edit(
    context: &crate::property_panel::Context,
    version: u64,
    input: Value,
) -> Result {
    if !context.current() || state::read(busy) {
        return Err(Error::new(
            "Wait for the current Agent task or select the widget again.",
        ));
    }
    if drafted(&context.project) {
        return Err(Error::new(
            "Save the project’s code drafts before editing a widget.",
        ));
    }
    let prompt = string(&input["prompt"]).trim();
    if prompt.is_empty() {
        return Err(Error::new("Describe the change to this widget."));
    }
    state::change(|state| {
        state.agent_posting = true;
        render(state);
    });
    dom::text("widget-edit-progress", "Preparing the widget edit…");
    let project = &context.project;
    let result =
        async {
            let job = project_post(project,"agent",json!({
            "action":"send","text":prompt,"edit":true,"widgetEdit":true,
            "context":{"path":context.path,"widget":context.widget,"revision":context.revision}
        })).await?;
            if dialogs::current_view(version) {
                let stop = id("widget-edit-stop");
                dom::attr(&stop, "data-job", string(&job["id"]));
                stop.set_hidden(false);
                dom::text("widget-edit-progress", "Editing and checking Dart…");
            }
            crate::wait_job(job).await
        }
        .await;
    if dialogs::current_view(version) {
        hidden("widget-edit-stop", true);
        dom::text("widget-edit-progress", "");
    }
    state::change(|state| {
        if state.project_id() == project {
            state.agent_posting = false;
            render(state);
        }
    });
    let result = result?;
    if crate::current(project) {
        if result["checkpoint"].is_object() {
            crate::saved_edits::refresh(project).await?;
            update_source(project).await?;
            toast("Widget edit saved", false);
        } else {
            toast(string(&result["reply"]), false);
        }
        if state::read(|state| state.agent_open) {
            refresh(project).await?;
        }
    }
    dialogs::close_current(version);
    Ok(())
}

async fn send() -> Result {
    let project = crate::project_id()?;
    if state::read(busy) {
        return Ok(());
    }
    if !state::read(|state| state.agent["provider"]["configured"] == true) {
        return settings(project).await;
    }
    let prompt = dom::value(&id("agent-prompt")).trim().to_owned();
    if prompt.is_empty() {
        return Ok(());
    }
    let edit = dom::value(&id("agent-mode")) == "edit";
    if edit && drafted(&project) {
        return Err(Error::new(
            "Save the project’s code drafts or switch the Agent to Ask.",
        ));
    }
    let hint = state::read(|state| {
        let widget = selected_widget(state).cloned().unwrap_or_default();
        json!({"path":state.current_file(),"widget":widget["id"],"type":widget["type"],"line":widget["line"]})
    });
    let thread = state::read(|state| state.agent["thread"].clone());
    state::change(|state| {
        state.agent_posting = true;
        render(state);
    });
    let outcome = async {
        let image = attachment(selected_image()).await?;
        let mut job = project_post(
            &project,
            "agent",
            json!({
                "action":"send","text":prompt,"edit":edit,"context":hint,"image":image,"thread":thread
            }),
        )
        .await?;
        job["result"] = json!({"prompt":prompt,"image":image["name"],"phase":"Thinking"});
        Ok(job)
    }
    .await;
    state::change(|state| {
        if state.project_id() != project {
            return;
        }
        state.agent_posting = false;
        if let Ok(job) = &outcome {
            state.agent_job = job.clone();
            dom::set_value(&id("agent-prompt"), "");
        }
        render(state);
    });
    outcome.map(|_: Value| ())
}

fn drafted(project: &str) -> bool {
    state::read(|state| state.drafts.keys().any(|(id, _)| id == project))
}

async fn update_source(project: &str) -> Result {
    if crate::current(project) {
        crate::refresh_tree(project).await?;
        let file = state::read(|state| state.current_file().to_owned());
        crate::load_file(&file, true).await?;
    }
    Ok(())
}

pub async fn poll(project: &str) -> Result {
    let previous = state::read(|state| state.agent_job.clone());
    let job_id = string(&previous["id"]);
    if job_id.is_empty() {
        return Ok(());
    }
    let job = api(&format!("/jobs/{job_id}")).await?;
    if !crate::finished(&job) {
        if job != previous {
            state::change(|state| {
                if state.project_id() == project && state.agent_job["id"] == job_id {
                    state.agent_job = job;
                    render(state);
                }
            });
        }
        return Ok(());
    }
    if !crate::current(project) {
        return Ok(());
    }
    let succeeded = job["status"] == "succeeded";
    state::change(|state| {
        if state.agent_job["id"] == job_id {
            state.agent_job = Value::Null;
        }
    });
    if succeeded {
        image_input().set_value("");
        refresh_image();
    } else {
        let field = id("agent-prompt");
        if dom::value(&field).is_empty() {
            dom::set_value(&field, string(&job["result"]["prompt"]));
        }
        toast(
            job["error"].as_str().unwrap_or("Agent reply canceled."),
            job["status"] == "failed",
        );
    }
    crate::saved_edits::refresh(project).await?;
    refresh(project).await?;
    if succeeded && job["result"]["checkpoint"].is_object() {
        update_source(project).await?;
    }
    Ok(())
}

async fn small_action(action: &str, mut input: Value) -> Result {
    let project = crate::project_id()?;
    if state::read(busy) {
        return Ok(());
    }
    if action == "undo" && drafted(&project) {
        return Err(Error::new(
            "Save the project’s code drafts before restoring a checkpoint.",
        ));
    }
    state::change(|state| {
        state.agent_posting = true;
        render(state);
    });
    let result = async {
        input["action"] = json!(action);
        let job = project_post(&project, "agent", input).await?;
        crate::wait_job(job).await?;
        if action == "undo" {
            crate::saved_edits::refresh(&project).await?;
            update_source(&project).await?;
        }
        refresh(&project).await
    }
    .await;
    state::change(|state| {
        if state.project_id() == project {
            state.agent_posting = false;
            render(state);
        }
    });
    result
}

pub fn reset_inputs() {
    dom::set_value(&id("agent-prompt"), "");
    image_input().set_value("");
    refresh_image();
}

pub fn initialize() {
    dom::click("agent", || {
        if state::read(|state| state.agent_open) {
            close();
        } else {
            dom::spawn(open());
        }
    });
    dom::click("agent-close", close);
    dom::click("agent-settings", || {
        if let Ok(project) = crate::project_id() {
            dom::spawn(settings(project));
        }
    });
    dom::click("agent-new", || {
        dom::spawn(small_action("new", json!({})));
    });
    dom::listen(&id("agent-thread"), "change", |event| {
        if let Some(field) = dom::target(&event) {
            dom::spawn(small_action(
                "select-thread",
                json!({"thread":dom::value(&field)}),
            ));
        }
    });
    dom::click("agent-attach", || image_input().click());
    dom::click("agent-remove-image", || {
        image_input().set_value("");
        refresh_image();
    });
    dom::listen(&image_input(), "change", |_| refresh_image());
    dom::click("agent-stop", || {
        dom::spawn(async {
            let job = state::read(|state| string(&state.agent_job["id"]).to_owned());
            if !job.is_empty() {
                post(&format!("/jobs/{job}/stop"), json!({})).await?;
            }
            Ok(())
        });
    });
    dom::listen(&id("agent-form"), "submit", |event| {
        event.prevent_default();
        dom::spawn(send());
    });
    dom::listen(&id("agent-messages"), "click", |event| {
        let Some(button) = dom::closest(&event, "[data-agent]") else {
            return;
        };
        match button.get_attribute("data-agent").as_deref() {
            Some("configure") => {
                if let Ok(project) = crate::project_id() {
                    dom::spawn(settings(project));
                }
            }
            Some("undo") => {
                if let Some(checkpoint) = button.get_attribute("data-checkpoint") {
                    dom::spawn(small_action("undo", json!({"checkpoint":checkpoint})));
                }
            }
            _ => {}
        }
    });
}
